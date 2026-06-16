/// Stitch all chunks from tiles.db into full-city overview PNGs.
///
/// Semantic-color exports (1 px per cell):
///   /tmp/map/sf_lod0.png  — 8375×7500 px (2 m/cell)
///   /tmp/map/sf_lod2.png  — 2144×1920 px (8 m/cell)
///   /tmp/map/sf_lod4.png  —  536×480  px (32 m/cell)
///   /tmp/map/fidi.png, soma.png, ggp.png — neighborhood crops from LOD 0
///
/// Atlas-tile exports (actual pixel art):
///   /tmp/map/fidi_tiles.png — FiDi at 4 px/cell using real atlas tiles (~2400×1800)
///   /tmp/map/soma_tiles.png — SoMa at 4 px/cell
///   /tmp/map/ggp_tiles.png  — Golden Gate Park at 4 px/cell
///   /tmp/map/sf_tiles.png   — Whole city at LOD 4, 4 px/cell (~2144×1920)
///
/// Usage: cargo run --bin export_map [-- --db tiles.db]

use anyhow::{Context, Result};
use clap::Parser;
use sim_maps::{
    config::Config,
    crs::reproject_bbox,
    db::decompress_u32,
    debug::semantic_color,
    render,
    types::{SemanticClass, tile_id_class, tile_id_variant},
};
use image::{DynamicImage, GenericImageView, RgbImage};
use rusqlite::{params, Connection};
use std::path::PathBuf;

const ATLAS_TILE: u32 = 32;
const ATLAS_COLS: u32 = 47;

#[derive(Parser, Debug)]
#[command(name = "export_map", about = "Stitch tiles.db chunks into full-city PNGs")]
struct Cli {
    #[arg(short, long, default_value = "config/pipeline.toml")]
    config: PathBuf,

    #[arg(long, default_value = "tiles.db")]
    db: PathBuf,

    #[arg(short, long, default_value = "/tmp/map")]
    out: PathBuf,

    /// Output filename prefix, e.g. "neu_york" -> neu_york_tiles.png.
    #[arg(long, default_value = "sf")]
    name: String,

    /// Render only the whole-city image (stitched per-chunk detail renders with
    /// MX buildings) to sf_city.png, then exit. Skips the other exports.
    #[arg(long)]
    full: bool,

    /// Pixels per cell for the --full render (default 2 → ~16750×15000 px).
    #[arg(long, default_value_t = 2)]
    full_scale: u32,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let cfg = Config::from_file(&cli.config)?;
    let bbox_utm = reproject_bbox(&cfg.bbox_wgs84, &cfg.crs.utm_epsg)?;
    let (nx, ny) = bbox_utm.chunk_grid_dims(cfg.pipeline.chunk_meters);
    let cells_full = cfg.cells_per_chunk();

    std::fs::create_dir_all(&cli.out)?;

    let conn = Connection::open(&cli.db)
        .with_context(|| format!("open {}", cli.db.display()))?;

    let atlas = image::open(&cfg.atlas.path)
        .with_context(|| format!("open atlas {}", cfg.atlas.path))?;

    let facade_atlas = image::open("assets/facade_atlas.png")
        .context("open assets/facade_atlas.png")?;

    if cli.full {
        render_full_city(&conn, &atlas, &facade_atlas, nx, ny, cells_full, cli.full_scale, &cli.out)?;
        return Ok(());
    }

    let mut lod0_img: Option<RgbImage> = None;

    // Semantic-color exports.
    for lod in [0u32, 2u32, 4u32] {
        let factor = 1u32 << lod;
        let chunk_cells = (cells_full + factor - 1) / factor;
        let map_w = nx as u32 * chunk_cells;
        let map_h = ny as u32 * chunk_cells;
        let mut img = RgbImage::new(map_w, map_h);

        let bg = semantic_color(SemanticClass::Grass);
        for px in img.pixels_mut() {
            *px = image::Rgb(bg);
        }

        let mut stmt = conn.prepare("SELECT cx, cy, render, w, h FROM chunks WHERE lod=?1")?;
        let rows = stmt.query_map(params![lod], |row| Ok((
            row.get::<_, i32>(0)?,
            row.get::<_, i32>(1)?,
            row.get::<_, Vec<u8>>(2)?,
            row.get::<_, u32>(3)?,
            row.get::<_, u32>(4)?,
        )))?;

        let mut chunk_count = 0u32;
        for row in rows {
            let (cx, cy, render_blob, w, h) = row?;
            if cx < 0 || cy < 0 { continue; }
            let (cx, cy) = (cx as u32, cy as u32);
            let tiles = decompress_u32(&render_blob, (w * h) as usize)?;
            let img_col_origin = cx * chunk_cells;
            let img_row_origin = (ny as u32 - 1 - cy) * chunk_cells;

            for cell_row in 0..h {
                for cell_col in 0..w {
                    let tile_id = tiles[(cell_row * w + cell_col) as usize];
                    let class = SemanticClass::from_u8(tile_id_class(tile_id));
                    let px_col = img_col_origin + cell_col;
                    let px_row = img_row_origin + cell_row;
                    if px_col < map_w && px_row < map_h {
                        img.put_pixel(px_col, px_row, image::Rgb(semantic_color(class)));
                    }
                }
            }
            chunk_count += 1;
        }

        let out_path = cli.out.join(format!("{}_lod{lod}.png", cli.name));
        img.save(&out_path).with_context(|| format!("save {}", out_path.display()))?;
        println!("{}_lod{lod}.png  ({map_w}×{map_h} px, {chunk_count} chunks, {chunk_cells} cells/chunk)", cli.name);

        if lod == 0 { lod0_img = Some(img); }
    }

    // Neighborhood semantic crops from LOD 0.
    if let Some(ref img) = lod0_img {
        let map_h = img.height();
        let mpc = cfg.pipeline.meters_per_cell;
        let utm_to_px = |utm_x: f64, utm_y: f64| -> (u32, u32) {
            let px = ((utm_x - bbox_utm.min_x) / mpc).round() as i64;
            let py = (map_h as i64) - 1 - ((utm_y - bbox_utm.min_y) / mpc).round() as i64;
            (px.max(0) as u32, py.max(0) as u32)
        };
        let neighborhoods: &[(&str, f64, f64, f64, f64)] = &[
            ("fidi", 553_200.0, 4_183_300.0, 1_200.0, 900.0),
            ("soma", 552_800.0, 4_181_400.0, 1_200.0, 900.0),
            ("ggp",  546_200.0, 4_179_800.0, 1_800.0, 900.0),
        ];
        for &(name, cx, cy, hw, hh) in neighborhoods {
            let (px_cx, px_cy) = utm_to_px(cx, cy);
            let half_w = (hw / mpc).round() as u32;
            let half_h = (hh / mpc).round() as u32;
            let x0 = px_cx.saturating_sub(half_w).min(img.width());
            let y0 = px_cy.saturating_sub(half_h).min(img.height());
            let x1 = (px_cx + half_w).min(img.width());
            let y1 = (px_cy + half_h).min(img.height());
            let (w, h) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
            // SF-specific crops fall outside other cities' images; skip them.
            if w == 0 || h == 0 { continue; }
            img.view(x0, y0, w, h).to_image()
                .save(cli.out.join(format!("{name}.png")))?;
            println!("{name}.png  ({w}×{h} px, center UTM ({cx:.0},{cy:.0}))");
        }
    }

    // Atlas-tile exports: render actual pixel art at 4 px per cell.
    // Neighborhoods from LOD 0; whole city at LOD 4.
    let px_per_cell: u32 = 4;

    // Whole city at LOD 4 with atlas tiles.
    {
        let lod = 4u32;
        let factor = 1u32 << lod;
        let chunk_cells = (cells_full + factor - 1) / factor;
        let map_w = nx as u32 * chunk_cells * px_per_cell;
        let map_h = ny as u32 * chunk_cells * px_per_cell;
        let mut img = RgbImage::new(map_w, map_h);
        fill_atlas_bg(&mut img, &atlas, SemanticClass::Grass, px_per_cell);

        let mut stmt = conn.prepare("SELECT cx, cy, render, w, h FROM chunks WHERE lod=?1")?;
        let rows = stmt.query_map(params![lod], |row| Ok((
            row.get::<_, i32>(0)?,
            row.get::<_, i32>(1)?,
            row.get::<_, Vec<u8>>(2)?,
            row.get::<_, u32>(3)?,
            row.get::<_, u32>(4)?,
        )))?;

        for row in rows {
            let (cx, cy, render_blob, w, h) = row?;
            if cx < 0 || cy < 0 { continue; }
            let (cx, cy) = (cx as u32, cy as u32);
            // Skip chunks outside this export's recomputed grid (guards the row-origin
            // underflow when a grid is taller/wider than SF's).
            if cx >= nx as u32 || cy >= ny as u32 { continue; }
            let tiles = decompress_u32(&render_blob, (w * h) as usize)?;
            let img_col_origin = cx * chunk_cells * px_per_cell;
            let img_row_origin = (ny as u32 - 1 - cy) * chunk_cells * px_per_cell;

            for cell_row in 0..h {
                for cell_col in 0..w {
                    let tile_id = tiles[(cell_row * w + cell_col) as usize];
                    let dst_x = img_col_origin + cell_col * px_per_cell;
                    let dst_y = img_row_origin + cell_row * px_per_cell;
                    if dst_x + px_per_cell > map_w || dst_y + px_per_cell > map_h {
                        continue;
                    }
                    blit_atlas_tile(&mut img, &atlas,
                        tile_id_class(tile_id) as u32,
                        tile_id_variant(tile_id) as u32,
                        dst_x, dst_y, px_per_cell);
                }
            }
        }

        let out_path = cli.out.join(format!("{}_tiles.png", cli.name));
        img.save(&out_path)?;
        println!("{}_tiles.png  ({map_w}×{map_h} px, LOD {lod}, {px_per_cell}px/cell atlas render)", cli.name);
    }

    // Neighborhood atlas-tile crops from LOD 0.
    let neighborhoods_tile: &[(&str, f64, f64, f64, f64)] = &[
        ("fidi", 553_200.0, 4_183_300.0, 1_200.0, 900.0),
        ("soma", 552_800.0, 4_181_400.0, 1_200.0, 900.0),
        ("ggp",  546_200.0, 4_179_800.0, 1_800.0, 900.0),
    ];

    let mpc = cfg.pipeline.meters_per_cell;

    for &(name, cx_utm, cy_utm, hw_m, hh_m) in neighborhoods_tile {
        // UTM crop bounds → chunk/cell coords.
        let x_min = cx_utm - hw_m;
        let x_max = cx_utm + hw_m;
        let y_min = cy_utm - hh_m;
        let y_max = cy_utm + hh_m;

        let chunk_m = cfg.pipeline.chunk_meters;
        let cx_lo = ((x_min - bbox_utm.min_x) / chunk_m).floor() as i32;
        let cx_hi = ((x_max - bbox_utm.min_x) / chunk_m).ceil()  as i32;
        let cy_lo = ((y_min - bbox_utm.min_y) / chunk_m).floor() as i32;
        let cy_hi = ((y_max - bbox_utm.min_y) / chunk_m).ceil()  as i32;

        let cx_lo = (cx_lo.max(0) as u32).min((nx - 1) as u32);
        let cx_hi = (cx_hi.max(0) as u32).min((nx - 1) as u32);
        let cy_lo = (cy_lo.max(0) as u32).min((ny - 1) as u32);
        let cy_hi = (cy_hi.max(0) as u32).min((ny - 1) as u32);
        // SF-specific named crops fall outside other cities' grids; skip them.
        if cx_hi < cx_lo || cy_hi < cy_lo {
            continue;
        }

        let crop_cells_w = (cx_hi - cx_lo + 1) * cells_full;
        let crop_cells_h = (cy_hi - cy_lo + 1) * cells_full;
        let img_w = crop_cells_w * px_per_cell;
        let img_h = crop_cells_h * px_per_cell;

        let mut img = RgbImage::new(img_w, img_h);
        fill_atlas_bg(&mut img, &atlas, SemanticClass::Grass, px_per_cell);

        let chunk_h = cy_hi - cy_lo + 1;

        let mut stmt = conn.prepare(
            "SELECT cx, cy, render, w, h FROM chunks WHERE lod=0 AND cx>=?1 AND cx<=?2 AND cy>=?3 AND cy<=?4"
        )?;
        let rows = stmt.query_map(params![cx_lo, cx_hi, cy_lo, cy_hi], |row| Ok((
            row.get::<_, i32>(0)?,
            row.get::<_, i32>(1)?,
            row.get::<_, Vec<u8>>(2)?,
            row.get::<_, u32>(3)?,
            row.get::<_, u32>(4)?,
        )))?;

        for row in rows {
            let (cx, cy, render_blob, w, h) = row?;
            if cx < 0 || cy < 0 { continue; }
            let (cx, cy) = (cx as u32, cy as u32);
            let tiles = decompress_u32(&render_blob, (w * h) as usize)?;
            let chunk_col = cx - cx_lo;
            let chunk_row = chunk_h - 1 - (cy - cy_lo); // flip y
            let img_col_origin = chunk_col * cells_full * px_per_cell;
            let img_row_origin = chunk_row * cells_full * px_per_cell;

            for cell_row in 0..h {
                for cell_col in 0..w {
                    let tile_id = tiles[(cell_row * w + cell_col) as usize];
                    blit_atlas_tile(&mut img, &atlas,
                        tile_id_class(tile_id) as u32,
                        tile_id_variant(tile_id) as u32,
                        img_col_origin + cell_col * px_per_cell,
                        img_row_origin + cell_row * px_per_cell,
                        px_per_cell);
                }
            }
        }

        // Crop to exact UTM bounds (trim partial-chunk edges).
        let cell_off_x = ((x_min - bbox_utm.min_x - cx_lo as f64 * chunk_m) / mpc).round() as u32;
        let cell_off_y_from_top = (((cy_hi as f64 + 1.0) * chunk_m + bbox_utm.min_y - y_max) / mpc)
            .round() as u32;
        let cells_wide = ((x_max - x_min) / mpc).round() as u32;
        let cells_tall = ((y_max - y_min) / mpc).round() as u32;

        let px0 = (cell_off_x * px_per_cell).min(img_w);
        let py0 = (cell_off_y_from_top * px_per_cell).min(img_h);
        let pw  = (cells_wide * px_per_cell).min(img_w - px0);
        let ph  = (cells_tall * px_per_cell).min(img_h - py0);

        let out_img = if pw > 0 && ph > 0 {
            img.view(px0, py0, pw, ph).to_image()
        } else {
            img
        };

        let out_path = cli.out.join(format!("{name}_tiles.png"));
        out_img.save(&out_path)?;
        println!("{name}_tiles.png  ({}×{} px, LOD 0, {px_per_cell}px/cell atlas render)",
            out_img.width(), out_img.height());
    }

    // Full 32px/cell detail views with MX modular buildings + oblique facades,
    // via the shared renderer (same code the verify harness checks).
    //   Alamo Square (32,34): dense Low Victorians.
    //   FiDi/Transamerica (43,43): Tall towers. Ferry (46,43): waterfront + mid-rise.
    //   Inner Richmond (22,29): dense Victorian rowhouses.
    for &(detail_cx, detail_cy, label) in &[
        (32i32, 34i32, "alamo_detail"),
        (43i32, 43i32, "fidi_detail"),
        (46i32, 43i32, "ferry_detail"),
        (22i32, 29i32, "richmond_detail"),
    ] {
        match render::render_detail_chunk(&conn, &atlas, &facade_atlas, detail_cx, detail_cy)? {
            Some(dr) => {
                let out_path = cli.out.join(format!("{label}.png"));
                dr.img.save(&out_path)?;
                println!(
                    "{label}.png  ({}×{} px, chunk ({detail_cx},{detail_cy}), 32px/cell + MX buildings)",
                    dr.img.width(),
                    dr.img.height()
                );
            }
            None => println!("{label}: chunk ({detail_cx},{detail_cy}) not found at LOD 0"),
        }
    }

    Ok(())
}

/// Blit an atlas tile into the destination image using nearest-neighbour scaling.
///
/// Each output pixel independently samples a position in the 32×32 atlas tile,
/// so even at 4px/cell the four output pixels come from four different tile locations.
fn blit_atlas_tile(
    dst: &mut RgbImage,
    atlas: &DynamicImage,
    class: u32,
    variant: u32,
    dst_x: u32,
    dst_y: u32,
    px_size: u32,
) {
    let variant = variant.min(ATLAS_COLS - 1);
    let atlas_x = variant * ATLAS_TILE;
    let atlas_y = class * ATLAS_TILE;
    let dst_w = dst.width();
    let dst_h = dst.height();

    for dy in 0..px_size {
        for dx in 0..px_size {
            let src_dx = dx * ATLAS_TILE / px_size;
            let src_dy = dy * ATLAS_TILE / px_size;
            let px_x = dst_x + dx;
            let px_y = dst_y + dy;
            if px_x < dst_w && px_y < dst_h
                && atlas_x + src_dx < atlas.width()
                && atlas_y + src_dy < atlas.height()
            {
                let p = atlas.get_pixel(atlas_x + src_dx, atlas_y + src_dy);
                dst.put_pixel(px_x, px_y, image::Rgb([p[0], p[1], p[2]]));
            }
        }
    }
}

/// Render the entire city: render each chunk via the shared renderer (32px/cell
/// with MX buildings + facades + trees), downscale it to `scale` px/cell, and
/// stitch into one big PNG. cy=0 is south, so chunk rows are flipped vertically.
fn render_full_city(
    conn: &Connection,
    atlas: &DynamicImage,
    facade_atlas: &DynamicImage,
    nx: i32,
    ny: i32,
    cells_full: u32,
    scale: u32,
    out: &std::path::Path,
) -> Result<()> {
    use image::imageops::{resize, FilterType};

    let chunk_px = cells_full * scale;
    let full_w = nx as u32 * chunk_px;
    let full_h = ny as u32 * chunk_px;
    let mut full = RgbImage::new(full_w, full_h);
    fill_atlas_bg(&mut full, atlas, SemanticClass::Grass, scale);

    let mut done = 0u32;
    for cy in 0..ny {
        for cx in 0..nx {
            let dr = match render::render_detail_chunk(conn, atlas, facade_atlas, cx, cy)? {
                Some(dr) => dr,
                None => continue,
            };
            let small = resize(&dr.img, chunk_px, chunk_px, FilterType::Triangle);
            let ox = cx as u32 * chunk_px;
            let oy = (ny - 1 - cy) as u32 * chunk_px; // cy=0 is south → bottom row
            for (px, py, p) in small.enumerate_pixels() {
                let (dx, dy) = (ox + px, oy + py);
                if dx < full_w && dy < full_h {
                    full.put_pixel(dx, dy, *p);
                }
            }
            done += 1;
            if done % 250 == 0 {
                println!("  rendered {done} chunks...");
            }
        }
    }

    let path = out.join("sf_city.png");
    full.save(&path)?;
    println!(
        "sf_city.png  ({full_w}×{full_h} px, {scale}px/cell, {done} chunks, MX buildings)"
    );
    Ok(())
}

fn fill_atlas_bg(dst: &mut RgbImage, atlas: &DynamicImage, class: SemanticClass, px_size: u32) {
    // Fill entire image with the solid-fill variant (variant 46 = fully surrounded).
    let atlas_x = 46 * ATLAS_TILE;
    let atlas_y = class as u32 * ATLAS_TILE;
    let p = atlas.get_pixel(atlas_x + 16, atlas_y + 16);
    let bg = image::Rgb([p[0], p[1], p[2]]);
    let _ = px_size;
    for px in dst.pixels_mut() { *px = bg; }
}
