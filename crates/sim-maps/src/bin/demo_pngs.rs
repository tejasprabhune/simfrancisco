/// Generates synthetic demo PNGs for visual inspection of Phases 3-6.
///
/// Output directory: /tmp/map/

use anyhow::Result;
use std::path::Path;

use sim_maps::{
    autotile::autotile_grid,
    config::ElevationConfig,
    debug::{dump_collision_png, dump_semantic_png},
    dem::compute_max_rise,
    lod::{downsample_collision, downsample_semantic},
    osm::{BuildingRecord, BuildingTier, FeatureIndex, RoadFeature},
    pipeline::semantic_to_collision,
    raster::{rasterize_semantic_grid, rect_poly, road_line},
    topo::apply_topography,
    types::{Grid, SemanticClass},
};

fn main() -> Result<()> {
    let out = Path::new("/tmp/map");
    std::fs::create_dir_all(out)?;

    phase3_city_block(out)?;
    phase3_roads(out)?;
    phase3_coastal(out)?;
    phase4_terrain(out)?;
    phase4_buildings_flat(out)?;
    phase5_autotile_roads(out)?;
    phase5_autotile_water(out)?;
    phase6_lod_comparison(out)?;
    phase7_slope_collision(out)?;

    println!("Demo PNGs written to /tmp/map/");
    Ok(())
}

// ── Phase 3 demos ─────────────────────────────────────────────────────────────

fn phase3_city_block(out: &Path) -> Result<()> {
    let cells = 64u32;
    let mpc = 2.0_f64;
    // 128 × 128 m chunk.

    let mut features = FeatureIndex::empty();

    // Central park (50×50 m).
    features.park_polys.push(rect_poly(39.0, 39.0, 89.0, 89.0));

    // Building block in NW corner.
    features.buildings.push(BuildingRecord { poly: rect_poly(5.0, 79.0, 35.0, 123.0), tier: BuildingTier::Low });
    // Building block in NE corner.
    features.buildings.push(BuildingRecord { poly: rect_poly(93.0, 79.0, 123.0, 123.0), tier: BuildingTier::Low });

    // Two crossing roads.
    features.roads.push(RoadFeature {
        line: road_line(&[(0.0, 64.0), (128.0, 64.0)]),
        semantic: SemanticClass::Road,
        buffer_m: 6.0,
    });
    features.roads.push(RoadFeature {
        line: road_line(&[(64.0, 0.0), (64.0, 128.0)]),
        semantic: SemanticClass::Road,
        buffer_m: 6.0,
    });

    let grid = rasterize_semantic_grid(&features, 0.0, 0.0, cells, cells, mpc);
    dump_semantic_png(&grid, &out.join("p3_city_block.png"), 4)?;
    println!("  p3_city_block.png");
    Ok(())
}

fn phase3_roads(out: &Path) -> Result<()> {
    let cells = 64u32;
    let mpc = 2.0_f64;

    let mut features = FeatureIndex::empty();

    // Primary road E–W.
    features.roads.push(RoadFeature {
        line: road_line(&[(0.0, 96.0), (128.0, 96.0)]),
        semantic: SemanticClass::Road,
        buffer_m: 8.0,
    });
    // Secondary road N–S.
    features.roads.push(RoadFeature {
        line: road_line(&[(64.0, 0.0), (64.0, 128.0)]),
        semantic: SemanticClass::Road,
        buffer_m: 5.0,
    });
    // Sidewalks flanking the primary.
    features.roads.push(RoadFeature {
        line: road_line(&[(0.0, 108.0), (128.0, 108.0)]),
        semantic: SemanticClass::Sidewalk,
        buffer_m: 2.0,
    });
    features.roads.push(RoadFeature {
        line: road_line(&[(0.0, 84.0), (128.0, 84.0)]),
        semantic: SemanticClass::Sidewalk,
        buffer_m: 2.0,
    });
    // Footpath diagonal.
    features.roads.push(RoadFeature {
        line: road_line(&[(0.0, 0.0), (60.0, 80.0)]),
        semantic: SemanticClass::Path,
        buffer_m: 2.0,
    });

    let grid = rasterize_semantic_grid(&features, 0.0, 0.0, cells, cells, mpc);
    dump_semantic_png(&grid, &out.join("p3_roads.png"), 4)?;
    println!("  p3_roads.png");
    Ok(())
}

fn phase3_coastal(out: &Path) -> Result<()> {
    let cells = 64u32;
    let mpc = 2.0_f64;

    let mut features = FeatureIndex::empty();

    // Ocean covers the southern third.
    features.water_polys.push(rect_poly(0.0, 0.0, 128.0, 42.0));
    // Sand strip just above the waterline.
    features.park_polys.push(rect_poly(0.0, 42.0, 128.0, 56.0));

    let grid = rasterize_semantic_grid(&features, 0.0, 0.0, cells, cells, mpc);
    dump_semantic_png(&grid, &out.join("p3_coastal.png"), 4)?;
    println!("  p3_coastal.png");
    Ok(())
}

// ── Phase 4 demos ─────────────────────────────────────────────────────────────

fn phase4_terrain(out: &Path) -> Result<()> {
    let cells = 64u32;
    let mpc = 2.0_f64;
    let origin = (0.0_f64, 0.0_f64);

    let features = FeatureIndex::empty();
    let mut semantic = rasterize_semantic_grid(&features, origin.0, origin.1, cells, cells, mpc);

    // Synthetic hill: elevation = distance from centre, capped, creating a
    // plateau in the middle surrounded by steep cliffs.
    let cx = cells as f64 / 2.0;
    let cy = cells as f64 / 2.0;
    let mut elevation: Grid<f32> = Grid::from_fn(cells, cells, |col, row| {
        let dx = col as f64 - cx;
        let dy = row as f64 - cy;
        let dist = (dx * dx + dy * dy).sqrt();
        // 0 at centre, rises sharply at distance 20 cells.
        if dist < 18.0 {
            50.0_f32
        } else if dist < 22.0 {
            50.0 - (dist - 18.0) as f32 * 12.0
        } else {
            2.0
        }
    });

    let cfg = ElevationConfig { walkable_threshold_m: 1.5, flatten_buildings: false };
    apply_topography(&mut semantic, &mut elevation, &[],
                     origin.0, origin.1, mpc, &cfg);

    dump_semantic_png(&semantic, &out.join("p4_terrain.png"), 4)?;
    println!("  p4_terrain.png");
    Ok(())
}

fn phase4_buildings_flat(out: &Path) -> Result<()> {
    let cells = 64u32;
    let mpc = 2.0_f64;
    let origin = (0.0_f64, 0.0_f64);

    let building = rect_poly(30.0, 30.0, 98.0, 98.0);

    let mut features = FeatureIndex::empty();
    features.buildings.push(BuildingRecord { poly: building.clone(), tier: BuildingTier::Low });

    let mut semantic = rasterize_semantic_grid(&features, origin.0, origin.1, cells, cells, mpc);

    // Sloped elevation: increases north to south.
    let mut elevation: Grid<f32> = Grid::from_fn(cells, cells, |_col, row| row as f32 * 1.0);
    let building_polys = vec![building];

    let cfg = ElevationConfig { walkable_threshold_m: 1.5, flatten_buildings: true };
    apply_topography(&mut semantic, &mut elevation, &building_polys,
                     origin.0, origin.1, mpc, &cfg);

    dump_semantic_png(&semantic, &out.join("p4_buildings_flat_semantic.png"), 4)?;

    // Also dump a collision layer to show buildings as blocked.
    let coll: Grid<u8> = Grid::from_fn(cells, cells, |col, row| {
        match semantic.get(col, row) {
            SemanticClass::BuildingFloor | SemanticClass::BuildingWall => 255,
            SemanticClass::CliffFace => 200,
            SemanticClass::Road => 10,
            SemanticClass::Grass => 20,
            _ => 50,
        }
    });
    dump_collision_png(&coll, &out.join("p4_buildings_flat_collision.png"), 4)?;

    println!("  p4_buildings_flat_semantic.png");
    println!("  p4_buildings_flat_collision.png");
    Ok(())
}

// ── Phase 5 demos ─────────────────────────────────────────────────────────────

/// Render autotile variant as a greyscale heatmap (0=black, 46=white).
fn dump_variant_heatmap(variants: &Grid<u8>, path: &Path, scale: u32) -> Result<()> {
    use image::{ImageBuffer, Rgb};
    let w = variants.width * scale;
    let h = variants.height * scale;
    let mut img = ImageBuffer::<Rgb<u8>, _>::new(w, h);
    for row in 0..variants.height {
        for col in 0..variants.width {
            let v = *variants.get(col, row);
            let brightness = (v as u32 * 255 / 46) as u8;
            let rgb = Rgb([brightness, brightness, brightness]);
            for dr in 0..scale {
                for dc in 0..scale {
                    img.put_pixel(col * scale + dc, row * scale + dr, rgb);
                }
            }
        }
    }
    img.save(path)?;
    Ok(())
}

fn phase5_autotile_roads(out: &Path) -> Result<()> {
    // Build the same road grid as p3_roads but visualise the autotile variants.
    let cells = 64u32;
    let mpc = 2.0_f64;

    let mut features = FeatureIndex::empty();
    features.roads.push(RoadFeature {
        line: road_line(&[(0.0, 96.0), (128.0, 96.0)]),
        semantic: SemanticClass::Road,
        buffer_m: 8.0,
    });
    features.roads.push(RoadFeature {
        line: road_line(&[(64.0, 0.0), (64.0, 128.0)]),
        semantic: SemanticClass::Road,
        buffer_m: 5.0,
    });

    let semantic = rasterize_semantic_grid(&features, 0.0, 0.0, cells, cells, mpc);
    let variants = autotile_grid(&semantic);

    dump_semantic_png(&semantic, &out.join("p5_autotile_roads_semantic.png"), 4)?;
    dump_variant_heatmap(&variants, &out.join("p5_autotile_roads_variants.png"), 4)?;

    println!("  p5_autotile_roads_semantic.png");
    println!("  p5_autotile_roads_variants.png");
    Ok(())
}

fn phase5_autotile_water(out: &Path) -> Result<()> {
    // Irregular water blob: large rect with corners cut out.
    let cells = 48u32;
    let mpc = 2.0_f64;

    let mut features = FeatureIndex::empty();
    // Central water body.
    features.water_polys.push(rect_poly(12.0, 12.0, 84.0, 84.0));
    // Two small islands (Grass patches that overwrite water — lower precedence,
    // so we represent them as buildings to get higher precedence).
    features.buildings.push(BuildingRecord { poly: rect_poly(28.0, 28.0, 44.0, 44.0), tier: BuildingTier::Low });
    features.buildings.push(BuildingRecord { poly: rect_poly(56.0, 52.0, 72.0, 68.0), tier: BuildingTier::Low });

    let semantic = rasterize_semantic_grid(&features, 0.0, 0.0, cells, cells, mpc);
    let variants = autotile_grid(&semantic);

    dump_semantic_png(&semantic, &out.join("p5_autotile_water_semantic.png"), 4)?;
    dump_variant_heatmap(&variants, &out.join("p5_autotile_water_variants.png"), 4)?;

    println!("  p5_autotile_water_semantic.png");
    println!("  p5_autotile_water_variants.png");
    Ok(())
}

// ── Phase 7 demo ──────────────────────────────────────────────────────────────

fn phase7_slope_collision(out: &Path) -> Result<()> {
    let cells = 64u32;
    let mpc = 2.0_f64;
    let origin = (0.0_f64, 0.0_f64);

    let mut features = FeatureIndex::empty();
    // Two crossing roads on hilly terrain.
    features.roads.push(RoadFeature {
        line: road_line(&[(0.0, 64.0), (128.0, 64.0)]),
        semantic: SemanticClass::Road, buffer_m: 6.0,
    });
    features.roads.push(RoadFeature {
        line: road_line(&[(64.0, 0.0), (64.0, 128.0)]),
        semantic: SemanticClass::Road, buffer_m: 6.0,
    });

    let mut semantic = rasterize_semantic_grid(&features, origin.0, origin.1, cells, cells, mpc);

    // Hill: gentle on one side (slope ~0.8m/cell), cliff on the other (slope ~3m/cell).
    let cx = cells as f64 / 2.0;
    let cy = cells as f64 / 2.0;
    let mut elevation: Grid<f32> = Grid::from_fn(cells, cells, |col, row| {
        let dx = col as f64 - cx;
        let dy = row as f64 - cy;
        let dist = (dx * dx + dy * dy).sqrt();
        if dist < 15.0 { 20.0_f32 }
        else if dist < 22.0 { 20.0 - (dist - 15.0) as f32 * 2.5 }
        else { 2.5 }
    });

    let cfg = ElevationConfig { walkable_threshold_m: 1.5, flatten_buildings: false };
    apply_topography(&mut semantic, &mut elevation, &[], origin.0, origin.1, mpc, &cfg);

    let rise = compute_max_rise(&elevation);
    let coll_slope = semantic_to_collision(&semantic, &rise, false, 1.5);
    let coll_flat = {
        let zero_rise = Grid::filled(cells, cells, 0.0_f32);
        semantic_to_collision(&semantic, &zero_rise, false, 1.5)
    };

    dump_semantic_png(&semantic, &out.join("p7_slope_semantic.png"), 4)?;
    dump_collision_png(&coll_flat, &out.join("p7_slope_collision_flat.png"), 4)?;
    dump_collision_png(&coll_slope, &out.join("p7_slope_collision_slope.png"), 4)?;

    // Max-rise heatmap (0=black, ≥threshold=white).
    {
        use image::{ImageBuffer, Rgb};
        let scale = 4u32;
        let w = cells * scale;
        let h = cells * scale;
        let mut img = ImageBuffer::<Rgb<u8>, _>::new(w, h);
        let threshold = 1.5_f32;
        for row in 0..cells {
            for col in 0..cells {
                let r = (*rise.get(col, row)).min(threshold) / threshold;
                let v = (r * 255.0) as u8;
                let px = Rgb([v, v, v]);
                for dr in 0..scale { for dc in 0..scale {
                    img.put_pixel(col * scale + dc, row * scale + dr, px);
                }}
            }
        }
        img.save(out.join("p7_slope_rise.png"))?;
    }

    println!("  p7_slope_semantic.png");
    println!("  p7_slope_collision_flat.png  p7_slope_collision_slope.png");
    println!("  p7_slope_rise.png");
    Ok(())
}

// ── Phase 6 demo ──────────────────────────────────────────────────────────────

fn phase6_lod_comparison(out: &Path) -> Result<()> {
    // Build a rich 64×64 cell scene and export LOD 0, 1, 2 side-by-side.
    let cells = 64u32;
    let mpc = 2.0_f64;

    let mut features = FeatureIndex::empty();
    // City block: park in centre, buildings on two sides, roads crossing.
    features.park_polys.push(rect_poly(40.0, 40.0, 88.0, 88.0));
    features.buildings.push(BuildingRecord { poly: rect_poly(4.0, 80.0, 36.0, 124.0), tier: BuildingTier::Low });
    features.buildings.push(BuildingRecord { poly: rect_poly(92.0, 4.0, 124.0, 36.0), tier: BuildingTier::Low });
    features.water_polys.push(rect_poly(0.0, 0.0, 128.0, 20.0));
    features.roads.push(RoadFeature {
        line: road_line(&[(0.0, 64.0), (128.0, 64.0)]),
        semantic: SemanticClass::Road, buffer_m: 6.0,
    });
    features.roads.push(RoadFeature {
        line: road_line(&[(64.0, 0.0), (64.0, 128.0)]),
        semantic: SemanticClass::Road, buffer_m: 6.0,
    });

    let semantic0 = rasterize_semantic_grid(&features, 0.0, 0.0, cells, cells, mpc);

    // LOD 0 (64×64, 4× scale = 256×256 px)
    dump_semantic_png(&semantic0, &out.join("p6_lod0.png"), 4)?;

    // LOD 1 (32×32, 8× scale = 256×256 px — same display size for comparison)
    let semantic1 = downsample_semantic(&semantic0, 2);
    dump_semantic_png(&semantic1, &out.join("p6_lod1.png"), 8)?;

    // LOD 2 (16×16, 16× scale = 256×256 px)
    let semantic2 = downsample_semantic(&semantic0, 4);
    dump_semantic_png(&semantic2, &out.join("p6_lod2.png"), 16)?;

    // Collision LOD comparison (max-pool).
    let coll0 = {
        use sim_maps::types::{collision, make_tile_id};
        let _ = make_tile_id; // suppress warning
        Grid::from_fn(cells, cells, |col, row| {
            match semantic0.get(col, row) {
                SemanticClass::Water | SemanticClass::BuildingWall | SemanticClass::BuildingFloor
                    => collision::BLOCKED,
                SemanticClass::Road     => collision::ROAD_COST,
                SemanticClass::ParkGrass => collision::PARK_COST,
                _   => collision::GRASS_COST,
            }
        })
    };
    dump_collision_png(&coll0, &out.join("p6_coll_lod0.png"), 4)?;
    let coll1 = downsample_collision(&coll0, 2);
    dump_collision_png(&coll1, &out.join("p6_coll_lod1.png"), 8)?;
    let coll2 = downsample_collision(&coll0, 4);
    dump_collision_png(&coll2, &out.join("p6_coll_lod2.png"), 16)?;

    println!("  p6_lod0.png  p6_lod1.png  p6_lod2.png");
    println!("  p6_coll_lod0.png  p6_coll_lod1.png  p6_coll_lod2.png");
    Ok(())
}
