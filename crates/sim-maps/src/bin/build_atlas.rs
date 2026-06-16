/// Build the terrain atlas PNG from the LimeZu "Modern Exteriors" 32x32 set.
///
/// Atlas layout (unchanged): 47 columns × 15 rows × 32×32 px = 1504×480 px.
///   Row    = SemanticClass ordinal (0=Grass … 14=BuildingTall).
///   Column = blob-47 autotile variant (0..46, sorted-mask order).
///
/// Each class is sourced from one MX 32×32 base tile. For every one of the 47
/// blob variants we re-derive which of the cell's N/E/S/W edges face a DIFFERENT
/// class (an "open" edge) from the variant's canonical neighbour mask, then bake an
/// organic rim into the base texture along those edges (a subtle darker lip for
/// land, a lighter foam lip for water) plus concave notches at inner corners. Rim
/// colours are snapped to the MX master palette so the output stays palette-cohesive.
///
/// Disjoint 32px cells can't blend into their neighbours (a transparent edge would
/// reveal the background, not the adjacent class), so the rim is a self-contained
/// contour: it turns each terrain patch into a defined, rounded-cornered region
/// instead of a flat checkerboard square. See CREDITS.md for attribution.
///
/// Usage: cargo run --release --bin build_atlas [-- [MX_32x32_DIR]]

use anyhow::{Context, Result};
use sim_maps::autotile::{suppress_corners, BLOB_VARIANTS};
use image::{DynamicImage, GenericImageView, Rgba, RgbaImage};
use std::path::{Path, PathBuf};

const TS: u32 = 32;
const VARIANTS: u32 = 47;
const CLASSES: u32 = 15;

#[derive(Clone, Copy, PartialEq)]
enum Rim {
    DarkLand,
    LightWater,
}

/// One semantic class's source tile (path relative to the ME_Theme_Sorter dir,
/// plus a pixel offset into that file) and how to render its edge rim.
struct ClassDef {
    label: &'static str,
    rel: &'static str,
    x: u32,
    y: u32,
    rim: Rim,
    /// Multiplicative brightness applied to the base before rim baking (1.0 = none).
    /// Used to derive ParkGrass (a richer/darker green) from the single MX grass tone.
    tint: f32,
}

const TERR_SINGLES: &str = "ME_Theme_Sorter_32x32/1_Terrains_and_Fences_Singles_32x32";
const CITY_SINGLES: &str = "ME_Theme_Sorter_32x32/2_City_Terrains_Singles_32x32";
const MB_SINGLES: &str = "ME_Theme_Sorter_32x32/5_Floor_Modular_Building_Singles_32x32";

fn classes() -> Vec<ClassDef> {
    vec![
        // 0 Grass — clean solid lawn tile.
        ClassDef { label: "Grass", rel: ts("ME_Singles_Terrains_and_Fences_32x32_Grass_1_22.png"), x: 0, y: 0, rim: Rim::DarkLand, tint: 1.0 },
        // 1 ParkGrass — same MX grass, darkened to a richer park green (only one MX grass tone exists).
        ClassDef { label: "ParkGrass", rel: ts("ME_Singles_Terrains_and_Fences_32x32_Grass_1_22.png"), x: 0, y: 0, rim: Rim::DarkLand, tint: 0.86 },
        // 2 Sand — beach sheet wavy sand.
        ClassDef { label: "Sand", rel: "ME_Theme_Sorter_32x32/21_Beach_32x32.png", x: 288, y: 224, rim: Rim::DarkLand, tint: 1.0 },
        // 3 Path — terrains sheet clay-orange dirt.
        ClassDef { label: "Path", rel: "ME_Theme_Sorter_32x32/1_Terrains_and_Fences_32x32.png", x: 928, y: 320, rim: Rim::DarkLand, tint: 1.0 },
        // 4 Sidewalk — warm-gray slab.
        ClassDef { label: "Sidewalk", rel: cs("ME_Singles_City_Terrains_32x32_Sidewalk_1_25.png"), x: 0, y: 0, rim: Rim::DarkLand, tint: 1.0 },
        // 5 Road — plain asphalt (no lane markings).
        ClassDef { label: "Road", rel: cs("ME_Singles_City_Terrains_32x32_Asphalt_1_Variation_17.png"), x: 0, y: 0, rim: Rim::DarkLand, tint: 1.0 },
        // 6 Stairs — no MX terrain stair; nearest is cobblestone paving (distinct stone).
        ClassDef { label: "Stairs", rel: "ME_Theme_Sorter_32x32/1_Terrains_and_Fences_32x32.png", x: 800, y: 320, rim: Rim::DarkLand, tint: 0.9 },
        // 7 CliffFace — rocky/dirt mound face.
        ClassDef { label: "CliffFace", rel: ts("ME_Singles_Terrains_and_Fences_32x32_Mound_2_6.png"), x: 0, y: 0, rim: Rim::DarkLand, tint: 1.0 },
        // 8 BuildingFloor (Low) — terracotta pitched roof (placeholder; Phase 2 redraws per-building).
        ClassDef { label: "BuildingFloor", rel: mb("ME_Singles_Floor_Modular_Building_32x32_Roof_2.png"), x: 96, y: 128, rim: Rim::DarkLand, tint: 1.0 },
        // 9 BuildingWall — brick wall body.
        ClassDef { label: "BuildingWall", rel: mb("ME_Singles_Floor_Modular_Building_32x32_Middle_Floor_1.png"), x: 32, y: 96, rim: Rim::DarkLand, tint: 1.0 },
        // 10 Water — deep blue bay water.
        ClassDef { label: "Water", rel: ts("ME_Singles_Terrains_and_Fences_32x32_Deep_Water_1_11.png"), x: 0, y: 0, rim: Rim::LightWater, tint: 1.0 },
        // 11 Plaza — gray cobblestone paving.
        ClassDef { label: "Plaza", rel: "ME_Theme_Sorter_32x32/1_Terrains_and_Fences_32x32.png", x: 800, y: 288, rim: Rim::DarkLand, tint: 1.0 },
        // 12 Shoreline — sand band meeting water (light foam rim).
        ClassDef { label: "Shoreline", rel: "ME_Theme_Sorter_32x32/21_Beach_32x32.png", x: 288, y: 224, rim: Rim::LightWater, tint: 1.0 },
        // 13 BuildingMid — flat tan/gray roof (placeholder).
        ClassDef { label: "BuildingMid", rel: mb("ME_Singles_Floor_Modular_Building_32x32_Roof_6.png"), x: 96, y: 128, rim: Rim::DarkLand, tint: 1.0 },
        // 14 BuildingTall — flat concrete roof (placeholder).
        ClassDef { label: "BuildingTall", rel: mb("ME_Singles_Floor_Modular_Building_32x32_Roof_10.png"), x: 96, y: 150, rim: Rim::DarkLand, tint: 1.0 },
    ]
}

fn ts(name: &str) -> &'static str {
    Box::leak(format!("{TERR_SINGLES}/{name}").into_boxed_str())
}
fn cs(name: &str) -> &'static str {
    Box::leak(format!("{CITY_SINGLES}/{name}").into_boxed_str())
}
fn mb(name: &str) -> &'static str {
    Box::leak(format!("{MB_SINGLES}/{name}").into_boxed_str())
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

    let palette = load_palette(&mx_dir.join("../Palette.png"))
        .or_else(|_| load_palette(&home.join("Downloads/modernexteriors-win/Palette.png")))
        .context("load MX master palette")?;

    // variant index (0..46) -> canonical corner-suppressed neighbour mask.
    let mut variant_mask = [0u8; 47];
    for m in 0u16..256 {
        let sm = suppress_corners(m as u8);
        let v = BLOB_VARIANTS[sm as usize];
        variant_mask[v as usize] = sm;
    }

    let atlas_w = VARIANTS * TS;
    let atlas_h = CLASSES * TS;
    let mut atlas = RgbaImage::new(atlas_w, atlas_h);

    for (class_idx, def) in classes().iter().enumerate() {
        let path = mx_dir.join(def.rel);
        let img = image::open(&path).with_context(|| format!("open {}", path.display()))?;
        let mut base = extract_tile(&img, def.x, def.y);
        if (def.tint - 1.0).abs() > 1e-3 {
            tint_tile(&mut base, def.tint, &palette);
        }
        let rim_color = rim_color_for(&base, def.rim, &palette);

        for variant in 0..VARIANTS {
            let tile = generate_variant(&base, variant_mask[variant as usize], rim_color, &palette);
            blit(&mut atlas, &tile, variant * TS, class_idx as u32 * TS);
        }
        println!(
            "  row {:2} — {:<14} {}@({},{}) tint={:.2}",
            class_idx, def.label, def.rel, def.x, def.y, def.tint
        );
    }

    std::fs::create_dir_all("assets").context("create assets/")?;
    atlas.save("assets/atlas.png").context("save atlas.png")?;
    println!(
        "atlas written: assets/atlas.png ({}×{} px, {} classes × {} variants, MX-sourced)",
        atlas_w, atlas_h, CLASSES, VARIANTS
    );
    Ok(())
}

/// Bake the organic rim for one blob variant into a copy of the base tile.
fn generate_variant(base: &RgbaImage, mask: u8, rim_color: Rgba<u8>, palette: &[[u8; 3]]) -> RgbaImage {
    // bit set = neighbour SAME class; "open" = neighbour DIFFERENT (a class boundary).
    let open_n = mask & 0x01 == 0;
    let open_e = mask & 0x04 == 0;
    let open_s = mask & 0x10 == 0;
    let open_w = mask & 0x40 == 0;
    // Inner (concave) corner: both flanking cardinals are SAME but the diagonal differs.
    let inner_ne = !open_n && !open_e && (mask & 0x02 == 0);
    let inner_se = !open_e && !open_s && (mask & 0x08 == 0);
    let inner_sw = !open_s && !open_w && (mask & 0x20 == 0);
    let inner_nw = !open_n && !open_w && (mask & 0x80 == 0);
    // Outer (convex) corner: both flanking cardinals face a boundary.
    let outer_ne = open_n && open_e;
    let outer_se = open_e && open_s;
    let outer_sw = open_s && open_w;
    let outer_nw = open_n && open_w;

    const RIM_W: i32 = 3;
    const CORNER_R: i32 = 5; // outer-corner rounding radius
    const RIM_STRENGTH: f32 = 0.6;
    let n = TS as i32;
    let mut out = base.clone();

    for y in 0..n {
        for x in 0..n {
            let mut s = 0.0f32;
            if open_n {
                s = s.max(falloff(y, RIM_W));
            }
            if open_s {
                s = s.max(falloff(n - 1 - y, RIM_W));
            }
            if open_w {
                s = s.max(falloff(x, RIM_W));
            }
            if open_e {
                s = s.max(falloff(n - 1 - x, RIM_W));
            }
            if inner_ne {
                s = s.max(corner_falloff(x, y, n - 1, 0, RIM_W));
            }
            if inner_se {
                s = s.max(corner_falloff(x, y, n - 1, n - 1, RIM_W));
            }
            if inner_sw {
                s = s.max(corner_falloff(x, y, 0, n - 1, RIM_W));
            }
            if inner_nw {
                s = s.max(corner_falloff(x, y, 0, 0, RIM_W));
            }
            // Round convex corners: pixels in the corner box beyond the arc become rim.
            if outer_ne {
                s = s.max(outer_corner_shade(x, y, n - 1, 0, CORNER_R));
            }
            if outer_se {
                s = s.max(outer_corner_shade(x, y, n - 1, n - 1, CORNER_R));
            }
            if outer_sw {
                s = s.max(outer_corner_shade(x, y, 0, n - 1, CORNER_R));
            }
            if outer_nw {
                s = s.max(outer_corner_shade(x, y, 0, 0, CORNER_R));
            }
            if s <= 0.0 {
                continue;
            }
            let p = *out.get_pixel(x as u32, y as u32);
            if p[3] == 0 {
                continue;
            }
            let t = s * RIM_STRENGTH;
            let mixed = [
                lerp(p[0], rim_color[0], t) as f32,
                lerp(p[1], rim_color[1], t) as f32,
                lerp(p[2], rim_color[2], t) as f32,
            ];
            let snapped = nearest_palette(mixed, palette);
            out.put_pixel(x as u32, y as u32, Rgba([snapped[0], snapped[1], snapped[2], p[3]]));
        }
    }
    out
}

/// Rim intensity at distance `d` from an open edge (1.0 at the edge, 0 beyond RIM_W).
fn falloff(d: i32, rim_w: i32) -> f32 {
    if d < 0 {
        0.0
    } else if d < rim_w {
        (rim_w - d) as f32 / rim_w as f32
    } else {
        0.0
    }
}

/// Rounds a convex corner at (cx, cy): pixels inside the corner box but beyond the
/// quarter-circle of radius `r` get full rim, with a 1px soft ring at the arc.
fn outer_corner_shade(x: i32, y: i32, cx: i32, cy: i32, r: i32) -> f32 {
    let dx = (x - cx).abs();
    let dy = (y - cy).abs();
    if dx < r && dy < r {
        let d = ((dx * dx + dy * dy) as f32).sqrt();
        ((d - (r as f32 - 1.5)) / 1.5).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Rim intensity near an inner-corner point (cx, cy).
fn corner_falloff(x: i32, y: i32, cx: i32, cy: i32, rim_w: i32) -> f32 {
    let dx = (x - cx).abs();
    let dy = (y - cy).abs();
    let d = ((dx * dx + dy * dy) as f32).sqrt();
    if d < rim_w as f32 {
        (rim_w as f32 - d) / rim_w as f32
    } else {
        0.0
    }
}

fn lerp(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 * (1.0 - t) + b as f32 * t).round().clamp(0.0, 255.0) as u8
}

/// Derive the rim colour from the base tile's average colour: darker for land,
/// lighter (foam) for water. Snapped to the MX palette.
fn rim_color_for(base: &RgbaImage, rim: Rim, palette: &[[u8; 3]]) -> Rgba<u8> {
    let (mut r, mut g, mut b, mut count) = (0u64, 0u64, 0u64, 0u64);
    for p in base.pixels() {
        if p[3] > 0 {
            r += p[0] as u64;
            g += p[1] as u64;
            b += p[2] as u64;
            count += 1;
        }
    }
    let count = count.max(1);
    let avg = [(r / count) as f32, (g / count) as f32, (b / count) as f32];
    let target = match rim {
        Rim::DarkLand => [avg[0] * 0.62, avg[1] * 0.62, avg[2] * 0.62],
        Rim::LightWater => [
            avg[0] + (255.0 - avg[0]) * 0.55,
            avg[1] + (255.0 - avg[1]) * 0.55,
            avg[2] + (255.0 - avg[2]) * 0.55,
        ],
    };
    let snapped = nearest_palette(target, palette);
    Rgba([snapped[0], snapped[1], snapped[2], 255])
}

/// Multiply every opaque pixel's brightness and snap to palette (ParkGrass tinting).
fn tint_tile(tile: &mut RgbaImage, factor: f32, palette: &[[u8; 3]]) {
    for p in tile.pixels_mut() {
        if p[3] == 0 {
            continue;
        }
        let mixed = [p[0] as f32 * factor, p[1] as f32 * factor, p[2] as f32 * factor];
        let snapped = nearest_palette(mixed, palette);
        *p = Rgba([snapped[0], snapped[1], snapped[2], p[3]]);
    }
}

fn extract_tile(img: &DynamicImage, x: u32, y: u32) -> RgbaImage {
    let mut out = RgbaImage::new(TS, TS);
    for py in 0..TS {
        for px in 0..TS {
            let (sx, sy) = (x + px, y + py);
            if sx < img.width() && sy < img.height() {
                out.put_pixel(px, py, img.get_pixel(sx, sy));
            }
        }
    }
    out
}

fn blit(atlas: &mut RgbaImage, tile: &RgbaImage, dx: u32, dy: u32) {
    for py in 0..TS {
        for px in 0..TS {
            atlas.put_pixel(dx + px, dy + py, *tile.get_pixel(px, py));
        }
    }
}

fn load_palette(path: &Path) -> Result<Vec<[u8; 3]>> {
    let img = image::open(path).with_context(|| format!("open palette {}", path.display()))?;
    let mut set: std::collections::HashSet<[u8; 3]> = std::collections::HashSet::new();
    for (_, _, p) in img.pixels() {
        if p[3] > 0 {
            set.insert([p[0], p[1], p[2]]);
        }
    }
    anyhow::ensure!(!set.is_empty(), "palette is empty: {}", path.display());
    Ok(set.into_iter().collect())
}

fn nearest_palette(c: [f32; 3], palette: &[[u8; 3]]) -> [u8; 3] {
    let mut best = palette[0];
    let mut best_d = f32::MAX;
    for &p in palette {
        let dr = c[0] - p[0] as f32;
        let dg = c[1] - p[1] as f32;
        let db = c[2] - p[2] as f32;
        let d = dr * dr + dg * dg + db * db;
        if d < best_d {
            best_d = d;
            best = p;
        }
    }
    best
}
