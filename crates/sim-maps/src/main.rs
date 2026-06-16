use sim_maps::{config, crs, db, debug, dem, lod, osm, pipeline, raster, topo, types};

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use log::info;
use rayon::prelude::*;

use config::Config;
use crs::reproject_bbox;
use db::{ChunkWrite, DbWriter};
use dem::DemReader;
use osm::FeatureIndex;
use pipeline::{semantic_to_collision, semantic_to_render};
use raster::rasterize_semantic_grid;
use topo::apply_topography;
use types::{ChunkCoord, Grid, SemanticClass};

#[derive(Parser, Debug)]
#[command(name = "pipeline", about = "SF RPG tilemap pipeline")]
struct Cli {
    /// Path to pipeline.toml config file
    #[arg(short, long, default_value = "config/pipeline.toml")]
    config: PathBuf,

    /// Dump a chunk PNG for visual inspection (format: cx,cy,lod)
    #[arg(long, value_name = "cx,cy,lod")]
    dump_chunk: Option<String>,
}

fn main() -> Result<()> {
    env_logger::init();
    let cli = Cli::parse();

    let cfg = Config::from_file(&cli.config)?;
    info!(
        "config loaded — {} m/cell, {} m chunks",
        cfg.pipeline.meters_per_cell, cfg.pipeline.chunk_meters
    );

    let bbox_utm = reproject_bbox(&cfg.bbox_wgs84, &cfg.crs.utm_epsg)?;
    info!(
        "{} bbox: ({:.0}, {:.0}) – ({:.0}, {:.0})",
        cfg.crs.utm_epsg, bbox_utm.min_x, bbox_utm.min_y, bbox_utm.max_x, bbox_utm.max_y
    );

    let (nx, ny) = bbox_utm.chunk_grid_dims(cfg.pipeline.chunk_meters);
    info!("chunk grid: {nx} × {ny} = {} chunks", nx * ny);

    let maybe_dem = load_dem(&cfg);
    let features = load_osm_if_present(&cfg, &bbox_utm);

    let db_path = cfg.paths.output_db.clone();
    let writer = DbWriter::open(&db_path)?;

    let manifest = serde_json::json!({
        "crs": cfg.crs.utm_epsg,
        "bbox_wgs84": cfg.bbox_wgs84,
        "bbox_utm": {
            "min_x": bbox_utm.min_x,
            "min_y": bbox_utm.min_y,
            "max_x": bbox_utm.max_x,
            "max_y": bbox_utm.max_y,
        },
        "meters_per_cell": cfg.pipeline.meters_per_cell,
        "chunk_meters": cfg.pipeline.chunk_meters,
        "cells_per_chunk": cfg.cells_per_chunk(),
        "lod_levels": cfg.pipeline.lod_levels,
        "atlas": cfg.atlas,
    });
    writer.send_meta("manifest", &serde_json::to_string(&manifest)?)?;

    let cells = cfg.cells_per_chunk();
    let mpc = cfg.pipeline.meters_per_cell;
    let lod_levels = cfg.pipeline.lod_levels;
    let threshold_m = cfg.elevation.walkable_threshold_m as f32;
    let building_blocked = cfg.collision.building_floor_blocked;
    let elev_cfg = cfg.elevation.clone();

    let chunk_coords: Vec<(i32, i32)> = (0..ny)
        .flat_map(|cy| (0..nx).map(move |cx| (cx, cy)))
        .collect();

    // Pre-sample elevation grids serially: DemReader contains raw Proj pointers
    // (*mut PJconsts) which are !Send, so it cannot be shared across rayon threads.
    let elevations: Vec<Grid<f32>> = match &maybe_dem {
        Some(dem) => chunk_coords.iter().map(|&(cx, cy)| {
            let (ox, oy) = bbox_utm.chunk_origin(cx, cy, cfg.pipeline.chunk_meters);
            dem.sample_grid_utm(ox, oy, cells, cells, mpc)
        }).collect(),
        None => chunk_coords.iter()
            .map(|_| Grid::filled(cells, cells, 0.0_f32))
            .collect(),
    };
    drop(maybe_dem);

    // Extract building polys once for apply_topography (shared read-only across threads).
    let building_polys: Vec<geo::Polygon<f64>> = features.buildings.iter()
        .map(|b| b.poly.clone())
        .collect();

    // Phase 8a: rasterize every chunk's semantic grid in parallel.
    let mut semantics: Vec<Grid<SemanticClass>> = chunk_coords
        .par_iter()
        .map(|&(cx, cy)| {
            let (ox, oy) = bbox_utm.chunk_origin(cx, cy, cfg.pipeline.chunk_meters);
            rasterize_semantic_grid(&features, ox, oy, cells, cells, mpc)
        })
        .collect();

    // Phase 8b: flood-fill ocean / lake / bay water inward from the map edges through
    // undeveloped cells. This robustly captures large water bodies (the Great Lakes,
    // harbors, the ocean) that OSM tags inconsistently as coastline or as huge water
    // multipolygons that get clipped open at the bbox and never rasterize.
    flood_fill_water(
        &mut semantics, &elevations, cfg.pipeline.water_max_elev_m,
        &chunk_coords, nx, ny, cells,
    );

    // Phase 8b': reclaim "green islands" — patches of natural ground (parks,
    // reserves) drawn over open water that the flood cannot cross because
    // ParkGrass is a barrier. A green component with no link to any developed
    // cell is an OSM artifact sitting in the bay/ocean; real parks border
    // streets and buildings and are left untouched.
    reclaim_green_water_islands(&mut semantics, &chunk_coords, nx, ny, cells);

    // Phase 8c: per-chunk topography + render + collision + LODs, written in parallel.
    let (result_tx, result_rx) = std::sync::mpsc::channel::<ChunkWrite>();
    let total_writes = chunk_coords.len() as u64 * lod_levels as u64;
    let write_count: u32 = std::thread::scope(|s| {
        let writer_thread = s.spawn(|| -> u32 {
            use std::io::{IsTerminal, Write as _};
            let tty = std::io::stderr().is_terminal();
            let mut count = 0u32;
            let mut last_pct = u32::MAX;
            for cw in result_rx {
                writer.send_chunk(cw).expect("DB write");
                count += 1;
                if total_writes > 0 {
                    let pct = (count as u64 * 100 / total_writes) as u32;
                    if pct != last_pct {
                        last_pct = pct;
                        let filled = (pct / 5).min(20) as usize;
                        let bar = "#".repeat(filled) + &"-".repeat(20 - filled);
                        if tty {
                            eprint!("\r  building map [{bar}] {pct:3}%  ({count}/{total_writes} tiles)");
                            let _ = std::io::stderr().flush();
                        } else if pct % 10 == 0 {
                            eprintln!("  building map {pct}%  ({count}/{total_writes} tiles)");
                        }
                    }
                }
            }
            if tty {
                eprintln!();
            }
            count
        });

        semantics.par_iter_mut().enumerate().for_each_with(
            result_tx,
            |tx, (idx, semantic)| {
                let (cx, cy) = chunk_coords[idx];
                let (ox, oy) = bbox_utm.chunk_origin(cx, cy, cfg.pipeline.chunk_meters);
                let mut elevation = elevations[idx].clone();
                apply_topography(
                    semantic, &mut elevation, &building_polys,
                    ox, oy, mpc, &elev_cfg,
                );

                // Phase 7: slope-aware collision.
                let rise = dem::compute_max_rise(&elevation);
                let render0 = semantic_to_render(semantic);
                let coll0 = semantic_to_collision(
                    semantic, &rise, building_blocked, threshold_m,
                );

                tx.send(ChunkWrite {
                    coord: ChunkCoord { cx, cy }, lod: 0,
                    render: render0, collision: coll0.clone(),
                }).expect("send lod0");

                for lod_level in 1..lod_levels {
                    let factor = 1u32 << lod_level;
                    let sem_ds = lod::downsample_semantic(semantic, factor);
                    let render = semantic_to_render(&sem_ds);
                    let coll = lod::downsample_collision(&coll0, factor);
                    tx.send(ChunkWrite {
                        coord: ChunkCoord { cx, cy }, lod: lod_level,
                        render, collision: coll,
                    }).expect("send lod");
                }
            },
        );

        writer_thread.join().expect("writer thread panicked")
    });

    writer.shutdown()?;
    info!(
        "rasterized {} chunks × {lod_levels} LODs = {write_count} writes → {}",
        chunk_coords.len(),
        db_path.display()
    );

    // Phase 9C: insert building records into DB.
    insert_building_records(&db_path, &features, &chunk_coords, &bbox_utm, cfg.pipeline.chunk_meters, mpc)?;

    write_mapping_json(&cfg)?;

    if let Some(spec) = &cli.dump_chunk {
        dump_chunk_png(&db_path, spec, cells)?;
    }

    Ok(())
}

fn load_dem(cfg: &Config) -> Option<DemReader> {
    let dem_path = &cfg.paths.dem_tif;
    if !dem_path.exists() {
        info!(
            "DEM not found at {} — elevation will be flat zero",
            dem_path.display()
        );
        return None;
    }
    match DemReader::from_file(dem_path, &cfg.crs.utm_epsg) {
        Err(e) => {
            log::warn!("DEM load failed: {e:#} — continuing with flat elevation");
            None
        }
        Ok(dem) => {
            // Optional sanity-check landmarks (WGS-84 → UTM); logged only.
            if !cfg.landmarks.is_empty() {
                if let Ok(p) = crs::new_proj(&cfg.crs.utm_epsg) {
                    for lm in &cfg.landmarks {
                        if let Ok((x, y)) = crs::wgs84_to_utm(&p, lm.lon, lm.lat) {
                            match dem.sample_utm(x, y) {
                                Some(elev) => info!(
                                    "elevation {}: {elev:.1} m (expected ~{:.0} m)",
                                    lm.name, lm.expected_m
                                ),
                                None => log::warn!("elevation {}: outside DEM extent", lm.name),
                            }
                        }
                    }
                }
            }
            Some(dem)
        }
    }
}

/// Points known to be on land (one per landmass), in projected UTM meters: from
/// config `land_refs` + `land_ref` (WGS-84) if given, else the bbox centre.
fn land_refs_utm(cfg: &Config, bbox: &config::BboxUtm) -> Vec<(f64, f64)> {
    let center = ((bbox.min_x + bbox.max_x) / 2.0, (bbox.min_y + bbox.max_y) / 2.0);
    let mut refs: Vec<config::LandRefWgs84> = cfg.land_refs.clone();
    if let Some(lr) = &cfg.land_ref {
        refs.push(*lr);
    }
    if refs.is_empty() {
        return vec![center];
    }
    let proj = match crs::new_proj(&cfg.crs.utm_epsg) {
        Ok(p) => p,
        Err(e) => {
            log::warn!("land_ref reproject failed: {e:#}; using bbox centre");
            return vec![center];
        }
    };
    let out: Vec<(f64, f64)> = refs
        .iter()
        .filter_map(|lr| crs::wgs84_to_utm(&proj, lr.lon, lr.lat).ok())
        .collect();
    if out.is_empty() { vec![center] } else { out }
}

fn load_osm_if_present(cfg: &Config, bbox: &config::BboxUtm) -> FeatureIndex {
    let path = &cfg.paths.osm_pbf;
    if !path.exists() {
        info!(
            "OSM PBF not found at {} — rasterizing empty (Grass) chunks",
            path.display()
        );
        return FeatureIndex::empty();
    }
    let land_refs = land_refs_utm(cfg, bbox);
    match FeatureIndex::from_pbf(path, bbox, &cfg.crs.utm_epsg, &land_refs) {
        Ok(idx) => idx,
        Err(e) => {
            log::warn!("OSM load failed: {e:#} — falling back to empty features");
            FeatureIndex::empty()
        }
    }
}

fn dump_chunk_png(db_path: &std::path::Path, spec: &str, cells: u32) -> Result<()> {
    use db::decompress_u32;
    use rusqlite::{params, Connection};
    use types::{tile_id_class, SemanticClass};

    let parts: Vec<&str> = spec.split(',').collect();
    if parts.len() != 3 {
        anyhow::bail!("--dump-chunk expects cx,cy,lod e.g. 10,5,0");
    }
    let cx: i32 = parts[0].parse()?;
    let cy: i32 = parts[1].parse()?;
    let lod: u32 = parts[2].parse()?;

    let conn = Connection::open(db_path)?;
    let (render_blob, w, h): (Vec<u8>, u32, u32) = conn.query_row(
        "SELECT render, w, h FROM chunks WHERE cx=?1 AND cy=?2 AND lod=?3",
        params![cx, cy, lod],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;

    let render = decompress_u32(&render_blob, (w * h) as usize)?;
    let mut semantic_grid: Grid<SemanticClass> = Grid::filled(w, h, SemanticClass::Grass);
    for row in 0..h {
        for col in 0..w {
            let class_u8 = tile_id_class(render[(row * w + col) as usize]);
            semantic_grid.set(col, row, SemanticClass::from_u8(class_u8));
        }
    }

    let scale = if w <= 64 { 4u32 } else { 2 };
    let png_path = format!("chunk_{cx}_{cy}_lod{lod}.png");
    debug::dump_semantic_png(&semantic_grid, std::path::Path::new(&png_path), scale)?;
    info!("debug PNG written to {png_path} (scale {scale}×)");
    let _ = cells;
    Ok(())
}

/// Flood-fill ocean/lake/bay water inward from the bbox edges through undeveloped
/// (Grass) and existing Water cells. Buildings, roads, parks, sand and other land act
/// as barriers, so interior land is untouched while large edge-connected water bodies
/// (the Great Lakes, harbors, the ocean) become Water regardless of OSM tagging.
fn flood_fill_water(
    semantics: &mut [Grid<SemanticClass>],
    elevations: &[Grid<f32>],
    max_water_elev: Option<f32>,
    chunk_coords: &[(i32, i32)],
    nx: i32,
    ny: i32,
    cells_per_chunk: u32,
) {
    use std::collections::{HashMap, VecDeque};
    let cpc = cells_per_chunk as i64;
    let gw = nx as i64 * cpc;
    let gh = ny as i64 * cpc;
    if gw <= 0 || gh <= 0 {
        return;
    }
    let mut chunk_idx: HashMap<(i32, i32), usize> = HashMap::new();
    for (i, &(cx, cy)) in chunk_coords.iter().enumerate() {
        chunk_idx.insert((cx, cy), i);
    }
    let cell_at = |gx: i64, gy: i64| -> Option<(usize, u32, u32)> {
        if gx < 0 || gy < 0 || gx >= gw || gy >= gh {
            return None;
        }
        let i = *chunk_idx.get(&((gx / cpc) as i32, (gy / cpc) as i32))?;
        Some((i, (gx % cpc) as u32, (gy % cpc) as u32))
    };
    // Elevation of a cell (metres). Cells outside the grid read as +inf so the
    // flood never spreads off the edge.
    let elev_at = |gx: i64, gy: i64| -> f32 {
        cell_at(gx, gy)
            .map(|(i, lx, ly)| *elevations[i].get(lx, ly))
            .unwrap_or(f32::INFINITY)
    };
    // A cell is floodable if it is undeveloped ground (Grass/Water) and, when an
    // elevation ceiling is configured, sits at or below it. The ceiling stops the
    // flood from climbing undeveloped high ground (foothills, inland basins) and
    // mistaking it for ocean.
    let is_open = |sem: &[Grid<SemanticClass>], gx: i64, gy: i64| -> bool {
        let undeveloped = matches!(
            cell_at(gx, gy).map(|(i, lx, ly)| *sem[i].get(lx, ly)),
            Some(SemanticClass::Grass) | Some(SemanticClass::Water)
        );
        undeveloped && max_water_elev.map_or(true, |ceil| elev_at(gx, gy) <= ceil)
    };

    let mut visited = vec![false; (gw * gh) as usize];
    let mut q: VecDeque<(i64, i64)> = VecDeque::new();

    // Seed from every open cell on the four bbox edges.
    let mut edges: Vec<(i64, i64)> = Vec::new();
    for gx in 0..gw {
        edges.push((gx, 0));
        edges.push((gx, gh - 1));
    }
    for gy in 0..gh {
        edges.push((0, gy));
        edges.push((gw - 1, gy));
    }
    for (gx, gy) in edges {
        let id = (gy * gw + gx) as usize;
        if !visited[id] && is_open(semantics, gx, gy) {
            visited[id] = true;
            q.push_back((gx, gy));
        }
    }

    while let Some((gx, gy)) = q.pop_front() {
        if let Some((i, lx, ly)) = cell_at(gx, gy) {
            semantics[i].set(lx, ly, SemanticClass::Water);
        }
        for (ngx, ngy) in [(gx - 1, gy), (gx + 1, gy), (gx, gy - 1), (gx, gy + 1)] {
            if ngx < 0 || ngy < 0 || ngx >= gw || ngy >= gh {
                continue;
            }
            let id = (ngy * gw + ngx) as usize;
            if !visited[id] && is_open(semantics, ngx, ngy) {
                visited[id] = true;
                q.push_back((ngx, ngy));
            }
        }
    }
}

/// Remove "green islands": connected components of natural ground
/// (Grass/ParkGrass/Sand/Shoreline) whose border is overwhelmingly water rather
/// than developed land. These are OSM polygons (marine reserves, protected
/// areas, unmapped bay floor) sitting in the bay or ocean that the edge flood
/// cannot enter because ParkGrass and the shoreline band act as barriers. A
/// blob can pick up a few stray developed neighbours (Biscayne Bay's Stiltsville
/// houses, channel markers) yet still be 99% water-bordered, so we reclaim on
/// the *fraction* of developed border, not on touching any developed cell at
/// all. Real land is 70-100% developed-bordered (dense streets) and is kept.
fn reclaim_green_water_islands(
    semantics: &mut [Grid<SemanticClass>],
    chunk_coords: &[(i32, i32)],
    nx: i32,
    ny: i32,
    cells_per_chunk: u32,
) {
    // Reclaim a component only if its water border dominates its developed border
    // by this factor (i.e. developed fraction < ~14%). Real landmasses sit far
    // above this; bay/ocean green blobs sit far below.
    const WATER_DOMINANCE: u32 = 6;
    use std::collections::{HashMap, VecDeque};
    let cpc = cells_per_chunk as i64;
    let gw = nx as i64 * cpc;
    let gh = ny as i64 * cpc;
    if gw <= 0 || gh <= 0 {
        return;
    }
    let mut chunk_idx: HashMap<(i32, i32), usize> = HashMap::new();
    for (i, &(cx, cy)) in chunk_coords.iter().enumerate() {
        chunk_idx.insert((cx, cy), i);
    }
    let cell_at = |gx: i64, gy: i64| -> Option<(usize, u32, u32)> {
        if gx < 0 || gy < 0 || gx >= gw || gy >= gh {
            return None;
        }
        let i = *chunk_idx.get(&((gx / cpc) as i32, (gy / cpc) as i32))?;
        Some((i, (gx % cpc) as u32, (gy % cpc) as u32))
    };
    let class_at = |sem: &[Grid<SemanticClass>], gx: i64, gy: i64| -> Option<SemanticClass> {
        cell_at(gx, gy).map(|(i, lx, ly)| *sem[i].get(lx, ly))
    };
    let is_green = |c: SemanticClass| {
        matches!(
            c,
            SemanticClass::Grass
                | SemanticClass::ParkGrass
                | SemanticClass::Sand
                | SemanticClass::Shoreline
        )
    };

    let mut visited = vec![false; (gw * gh) as usize];
    for sy in 0..gh {
        for sx in 0..gw {
            let sid = (sy * gw + sx) as usize;
            if visited[sid] {
                continue;
            }
            match class_at(semantics, sx, sy) {
                Some(c) if is_green(c) => {}
                _ => continue,
            }
            // BFS the green component; tally its water-bordered vs developed-bordered edges.
            let mut comp: Vec<(i64, i64)> = Vec::new();
            let mut water_border: u32 = 0;
            let mut dev_border: u32 = 0;
            let mut q: VecDeque<(i64, i64)> = VecDeque::new();
            visited[sid] = true;
            q.push_back((sx, sy));
            while let Some((gx, gy)) = q.pop_front() {
                comp.push((gx, gy));
                for (ngx, ngy) in [(gx - 1, gy), (gx + 1, gy), (gx, gy - 1), (gx, gy + 1)] {
                    match class_at(semantics, ngx, ngy) {
                        Some(nc) if is_green(nc) => {
                            let nid = (ngy * gw + ngx) as usize;
                            if !visited[nid] {
                                visited[nid] = true;
                                q.push_back((ngx, ngy));
                            }
                        }
                        Some(SemanticClass::Water) => water_border += 1,
                        None => {} // bbox edge — neutral
                        Some(_) => dev_border += 1, // road / building / path / plaza
                    }
                }
            }
            // A blob floating in water: touches water and is overwhelmingly
            // water-bordered. Stray bay structures (Stiltsville, markers) leave a
            // tiny developed border that no longer rescues it.
            if water_border > 0 && dev_border * WATER_DOMINANCE < water_border {
                for (gx, gy) in comp {
                    if let Some((i, lx, ly)) = cell_at(gx, gy) {
                        semantics[i].set(lx, ly, SemanticClass::Water);
                    }
                }
            }
        }
    }
}

fn insert_building_records(
    db_path: &std::path::Path,
    features: &osm::FeatureIndex,
    chunk_coords: &[(i32, i32)],
    bbox_utm: &config::BboxUtm,
    chunk_meters: f64,
    mpc: f64,
) -> Result<()> {
    use geo::algorithm::bounding_rect::BoundingRect;
    use osm::BuildingTier;
    use rusqlite::{params, Connection};

    if features.buildings.is_empty() {
        return Ok(());
    }

    let _ = chunk_coords; // chunk range is derived per-building below
    let (nx, ny) = bbox_utm.chunk_grid_dims(chunk_meters);
    let conn = Connection::open(db_path)?;
    let tx = conn.unchecked_transaction()?;
    let mut count = 0u32;
    {
        let mut stmt = tx.prepare(
            "INSERT OR IGNORE INTO buildings(cx, cy, cell_x, cell_y, cell_w, cell_h, tier)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        // Iterate each building once and write only the chunks it overlaps
        // (O(buildings), not O(chunks × buildings) — the latter is ~48e9 ops for LA),
        // batched in one transaction.
        for bldg in &features.buildings {
            let bbox = match bldg.poly.bounding_rect() {
                Some(r) => r,
                None => continue,
            };
            let cx_lo = (((bbox.min().x - bbox_utm.min_x) / chunk_meters).floor() as i32).max(0);
            let cx_hi = (((bbox.max().x - bbox_utm.min_x) / chunk_meters).floor() as i32).min(nx - 1);
            let cy_lo = (((bbox.min().y - bbox_utm.min_y) / chunk_meters).floor() as i32).max(0);
            let cy_hi = (((bbox.max().y - bbox_utm.min_y) / chunk_meters).floor() as i32).min(ny - 1);
            if cx_hi < cx_lo || cy_hi < cy_lo {
                continue;
            }
            let tier: u8 = match bldg.tier {
                BuildingTier::Low => 0,
                BuildingTier::Mid => 1,
                BuildingTier::Tall => 2,
            };
            for cy in cy_lo..=cy_hi {
                for cx in cx_lo..=cx_hi {
                    let (ox, oy) = bbox_utm.chunk_origin(cx, cy, chunk_meters);
                    let chunk_max_x = ox + chunk_meters;
                    let chunk_max_y = oy + chunk_meters;
                    let overlap_min_x = bbox.min().x.max(ox);
                    let overlap_max_x = bbox.max().x.min(chunk_max_x);
                    let overlap_min_y = bbox.min().y.max(oy);
                    let overlap_max_y = bbox.max().y.min(chunk_max_y);
                    if overlap_max_x <= overlap_min_x || overlap_max_y <= overlap_min_y {
                        continue;
                    }
                    let cell_x = ((overlap_min_x - ox) / mpc) as f32;
                    let cell_y = ((overlap_min_y - oy) / mpc) as f32;
                    let cell_w = ((overlap_max_x - overlap_min_x) / mpc) as f32;
                    let cell_h = ((overlap_max_y - overlap_min_y) / mpc) as f32;
                    if cell_w < 1.0 || cell_h < 1.0 {
                        continue;
                    }
                    stmt.execute(params![cx, cy, cell_x, cell_y, cell_w, cell_h, tier])?;
                    count += 1;
                }
            }
        }
    }
    tx.commit()?;
    info!("building records inserted: {count}");
    Ok(())
}

fn write_mapping_json(cfg: &Config) -> Result<()> {
    use sim_maps::autotile::VARIANT_COUNT;

    let classes: &[(&str, u8)] = &[
        ("Grass",         0),
        ("ParkGrass",     1),
        ("Sand",          2),
        ("Path",          3),
        ("Sidewalk",      4),
        ("Road",          5),
        ("Stairs",        6),
        ("CliffFace",     7),
        ("BuildingFloor", 8),
        ("BuildingWall",  9),
        ("Water",        10),
        ("Plaza",        11),
        ("Shoreline",    12),
        ("BuildingMid",  13),
        ("BuildingTall", 14),
    ];

    let mut tiles = Vec::new();
    for (name, ordinal) in classes {
        for variant in 0..VARIANT_COUNT {
            let tile_index = (*ordinal as u32) * (VARIANT_COUNT as u32) + (variant as u32);
            let atlas_col = tile_index % cfg.atlas.columns;
            let atlas_row = tile_index / cfg.atlas.columns;
            tiles.push(serde_json::json!({
                "class":      ordinal,
                "class_name": name,
                "variant":    variant,
                "tile_index": tile_index,
                "atlas_col":  atlas_col,
                "atlas_row":  atlas_row,
            }));
        }
    }

    let doc = serde_json::json!({
        "atlas":         cfg.atlas,
        "variant_count": VARIANT_COUNT,
        "tiles":         tiles,
    });

    let json_path = &cfg.paths.mapping_json;
    std::fs::write(json_path, serde_json::to_string_pretty(&doc)?)?;
    info!("mapping.json written ({} tile entries) → {}", tiles.len(), json_path.display());
    Ok(())
}
