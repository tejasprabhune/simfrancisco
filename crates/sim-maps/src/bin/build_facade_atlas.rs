/// Build the facade atlas used for oblique building rendering, from LimeZu
/// "Modern Exteriors" 32x32 modular building pieces.
///
/// Layout: 6 columns × 4 rows × 32×32 px = 192×96... actually 192×128.
///   Rows 0..2 = three wall colour schemes (brick-red, blue-gray, taupe), each
///   sliced from a pre-composed Middle_Floor bay (identical layout across colours):
///     col 0 CORNICE   — top trim band where the wall meets the roof
///     col 1 WIN_TOP   — upper half of a window-in-wall
///     col 2 WIN_BOT   — lower half of a window-in-wall
///     col 3 BASE      — ground-floor base / foundation band
///     col 4 WALL      — plain wall (between windows / edges)
///     col 5 (spare, copy of WALL)
///   Row 3 = roof fill textures (native MX colours, no tint):
///     col 0 terracotta · col 1 slate-blue · col 2 olive · col 3 gray-blue flat
///
/// The renderer composes a per-building facade by stacking CORNICE / window
/// floors / BASE to the tier's floor count, tiled to the footprint width, and
/// fills the rooftop footprint with one of the roof colours chosen by a hash of
/// the building's coordinates so neighbours read as separate structures.
/// See CREDITS.md for attribution.
///
/// Usage: cargo run --release --bin build_facade_atlas [-- [MX_32x32_DIR]]

use anyhow::{Context, Result};
use image::{DynamicImage, GenericImage, GenericImageView, RgbaImage};
use std::path::PathBuf;

const TS: u32 = 32;
const COLS: u32 = 6;
const ROWS: u32 = 4;

const MB: &str =
    "ME_Theme_Sorter_32x32/5_Floor_Modular_Building_Singles_32x32/ME_Singles_Floor_Modular_Building_32x32";

/// A 32×32 slice (file relative to MX dir, pixel offset).
struct Slice {
    rel: String,
    x: u32,
    y: u32,
}
fn s(rel: String, x: u32, y: u32) -> Slice {
    Slice { rel, x, y }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let home = PathBuf::from(std::env::var("HOME").context("HOME not set")?);
    let mx_dir: PathBuf = if args.len() > 1 {
        PathBuf::from(&args[1])
    } else {
        home.join("Downloads/modernexteriors-win/Modern_Exteriors_32x32")
    };
    anyhow::ensure!(mx_dir.exists(), "MX 32x32 dir not found: {}", mx_dir.display());

    // Pre-composed Middle_Floor bays (224×128), identical layout per colour.
    // Window centred at x=24; cornice at y=0, window y=40/68, base y=100.
    let bay = |n: u32| format!("{MB}_Middle_Floor_{n}.png");
    let wall_schemes: [(&str, u32); 3] = [("brick", 1), ("blue", 7), ("taupe", 13)];

    // Roof fill textures (native MX colours), clean shingle/flat surfaces.
    let roofs: [(&str, &str, u32, u32); 4] = [
        ("terracotta", "Roof_2", 96, 128),
        ("slate", "Roof_4", 96, 128),
        ("olive", "Roof_6", 96, 128),
        ("grayblue", "Roof_1", 96, 128),
    ];

    let mut atlas = RgbaImage::new(COLS * TS, ROWS * TS);

    for (row, (name, bay_n)) in wall_schemes.iter().enumerate() {
        let b = bay(*bay_n);
        let slots = [
            s(b.clone(), 24, 0),   // 0 CORNICE
            s(b.clone(), 24, 40),  // 1 WIN_TOP
            s(b.clone(), 24, 68),  // 2 WIN_BOT
            s(b.clone(), 24, 100), // 3 BASE
            s(b.clone(), 56, 52),  // 4 WALL (plain area between windows)
            s(b.clone(), 56, 52),  // 5 spare = WALL
        ];
        for (col, sl) in slots.iter().enumerate() {
            let mut tile = load_slice(&mx_dir, sl)?;
            soften_tile(&mut tile);
            atlas.copy_from(&tile, col as u32 * TS, row as u32 * TS)?;
        }
        println!("  wall row {row} — {name} (Middle_Floor_{bay_n})");
    }

    for (col, (name, file, x, y)) in roofs.iter().enumerate() {
        let sl = s(format!("{MB}_{file}.png"), *x, *y);
        let mut tile = load_slice(&mx_dir, &sl)?;
        soften_tile(&mut tile);
        atlas.copy_from(&tile, col as u32 * TS, 3 * TS)?;
        println!("  roof  col {col} — {name} ({file}@{x},{y})");
    }

    std::fs::create_dir_all("assets").context("create assets/")?;
    atlas.save("assets/facade_atlas.png").context("save facade_atlas.png")?;
    println!(
        "facade_atlas.png written: {}×{} px (3 wall colours × 6 slots + 4 roof colours, MX-sourced)",
        COLS * TS,
        ROWS * TS
    );

    build_props_atlas(&mx_dir)?;
    Ok(())
}

/// Props atlas: a few MX trees for the ground overlay. Each tree variant is placed
/// bottom-aligned and horizontally centred in a 96×128 cell, so the renderer can
/// blit one cell with its trunk anchored to a ground cell. Trees ship with a baked
/// soft shadow, so no extra shadow is needed.
fn build_props_atlas(mx_dir: &std::path::Path) -> Result<()> {
    const CW: u32 = 96;
    const CH: u32 = 128;
    let cp = "ME_Theme_Sorter_32x32/3_City_Props_Singles_32x32/ME_Singles_City_Props_32x32";
    let trees: [(&str, u32, u32); 3] =
        [("Tree_1", 64, 96), ("Tree_3", 64, 96), ("Tree_5", 64, 128)];

    let mut props = RgbaImage::new(CW * trees.len() as u32, CH);
    for (i, (name, tw, th)) in trees.iter().enumerate() {
        let path = mx_dir.join(format!("{cp}_{name}.png"));
        let img = image::open(&path).with_context(|| format!("open {}", path.display()))?;
        let ox = i as u32 * CW + (CW - tw) / 2;
        let oy = CH - th;
        for py in 0..*th {
            for px in 0..*tw {
                if px < img.width() && py < img.height() {
                    props.put_pixel(ox + px, oy + py, img.get_pixel(px, py));
                }
            }
        }
        println!("  prop col {i} — {name} ({tw}×{th})");
    }
    props.save("assets/props_atlas.png").context("save props_atlas.png")?;
    println!("props_atlas.png written: {}×{} px (3 MX trees)", CW * trees.len() as u32, CH);
    Ok(())
}

/// Gently soften a tile: pull saturation down ~22% and lighten ~6%, so bold
/// MX colours (e.g. brick-red) read less harsh while muted tones barely change.
fn soften_tile(tile: &mut RgbaImage) {
    const SAT_KEEP: f32 = 0.78;
    const LIGHTEN: f32 = 0.06;
    for p in tile.pixels_mut() {
        if p[3] == 0 {
            continue;
        }
        let (r, g, b) = (p[0] as f32, p[1] as f32, p[2] as f32);
        let lum = 0.299 * r + 0.587 * g + 0.114 * b;
        let soft = |c: f32| -> u8 {
            let d = lum + (c - lum) * SAT_KEEP;
            (d + (255.0 - d) * LIGHTEN).round().clamp(0.0, 255.0) as u8
        };
        *p = image::Rgba([soft(r), soft(g), soft(b), p[3]]);
    }
}

fn load_slice(mx_dir: &std::path::Path, sl: &Slice) -> Result<RgbaImage> {
    let path = mx_dir.join(&sl.rel);
    let img: DynamicImage =
        image::open(&path).with_context(|| format!("open {}", path.display()))?;
    let mut out = RgbaImage::new(TS, TS);
    for py in 0..TS {
        for px in 0..TS {
            let (sx, sy) = (sl.x + px, sl.y + py);
            if sx < img.width() && sy < img.height() {
                out.put_pixel(px, py, img.get_pixel(sx, sy));
            }
        }
    }
    Ok(out)
}
