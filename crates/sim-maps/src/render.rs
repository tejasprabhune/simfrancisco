/// Shared tile rendering used by both `export_map` (full-city + detail PNGs) and
/// `verify` (the self-verification harness). Centralising it keeps the building /
/// facade draw path in one place so the harness renders exactly what ships.
///
/// Coordinate conventions (LOD 0, see INTEGRATION.md §2/§8):
///   - chunk render grid is row-major, row 0 = NORTH.
///   - building `cell_y` is measured from the SOUTH edge of the chunk.
///   - facades hang BELOW a footprint's south edge; the rooftop is drawn on top.
///
/// Phase 2 building model: each footprint is one discrete MX modular building.
/// The rooftop is filled with one of several native MX roof colours chosen by a
/// hash of the building's coordinates (so neighbours differ); the south wall is
/// composed from MX facade slots (cornice / window floors / base) scaled to the
/// tier's floor count; and a drop shadow (sun top-left) is cast to the lower-right
/// so the building reads as a separate 3D structure. Painter's order is north-first
/// so nearer (south) buildings occlude farther (north) ones.

use anyhow::Result;
use image::{DynamicImage, GenericImageView, Rgb, RgbImage};
use rusqlite::{params, Connection};

use crate::db::decompress_u32;
use crate::types::{tile_id_class, tile_id_variant, SemanticClass};

pub const ATLAS_TILE: u32 = 32;
pub const ATLAS_COLS: u32 = 47;

// Facade atlas layout (assets/facade_atlas.png, 192×128, built by build_facade_atlas).
const FACADE_TS: u32 = 32;
const SLOT_CORNICE: u32 = 0;
const SLOT_WIN_TOP: u32 = 1;
const SLOT_WIN_BOT: u32 = 2;
const SLOT_BASE: u32 = 3;
const WALL_VARIANTS: u64 = 3;
const ROOF_ROW: u32 = 3;
const ROOF_VARIANTS: u64 = 4;

const SHADOW_W: u32 = 5; // drop-shadow band width (px)
const SHADOW_MUL: f32 = 0.55; // ground darkening factor under the shadow

/// One building footprint record (chunk-local cell coords, origin = SW corner).
#[derive(Debug, Clone, Copy)]
pub struct BuildingRec {
    pub cell_x: f32,
    pub cell_y: f32,
    pub cell_w: f32,
    pub cell_h: f32,
    pub tier: u8,
}

/// Result of rendering one chunk at native 32px/cell with oblique facades.
pub struct DetailRender {
    pub img: RgbImage,
    pub buildings: Vec<BuildingRec>,
    pub w_cells: u32,
    pub h_cells: u32,
}

/// Per-building visual style chosen deterministically from its coordinates.
#[derive(Clone, Copy)]
pub struct Style {
    pub wall: u32,
    pub roof: u32,
}

/// Deterministic FNV-1a hash of (cx, cy, cell_x, cell_y) → wall & roof variant,
/// so adjacent buildings get different colours and read as separate structures.
pub fn building_style(cx: i32, cy: i32, cell_x: f32, cell_y: f32) -> Style {
    let qx = (cell_x * 4.0).round() as i64;
    let qy = (cell_y * 4.0).round() as i64;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for v in [cx as i64, cy as i64, qx, qy] {
        h ^= v as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    Style {
        wall: ((h >> 7) % WALL_VARIANTS) as u32,
        roof: ((h >> 23) % ROOF_VARIANTS) as u32,
    }
}

/// Facade drop height in pixels for a given tier (scales with floor count).
pub fn facade_height_px(tier: u8) -> u32 {
    let floors = match tier {
        2 => 7, // Tall
        1 => 4, // Mid
        _ => 2, // Low
    };
    floors * ATLAS_TILE
}

/// (x, y, w, h) pixel rectangle of a building's rooftop footprint.
pub fn footprint_rect(b: &BuildingRec, h_cells: u32) -> (u32, u32, u32, u32) {
    let ft = ATLAS_TILE;
    let img_top_row = (h_cells as f32 - b.cell_y - b.cell_h).max(0.0) as u32;
    let img_south_edge = (h_cells as f32 - b.cell_y).clamp(0.0, h_cells as f32) as u32;
    let x = (b.cell_x * ft as f32) as u32;
    let wpx = (b.cell_w * ft as f32).max(1.0) as u32;
    (x, img_top_row * ft, wpx, img_south_edge.saturating_sub(img_top_row) * ft)
}

/// (x, y, w, h) pixel rectangle of a building's facade (below the south edge).
pub fn facade_rect(b: &BuildingRec, h_cells: u32) -> (u32, u32, u32, u32) {
    let ft = ATLAS_TILE;
    let img_south_edge = (h_cells as f32 - b.cell_y).clamp(0.0, h_cells as f32) as u32;
    let x = (b.cell_x * ft as f32) as u32;
    let wpx = (b.cell_w * ft as f32).max(1.0) as u32;
    (x, img_south_edge * ft, wpx, facade_height_px(b.tier))
}

/// Render one chunk at native tile size (32px/cell) with oblique building facades.
/// Returns `Ok(None)` if the chunk is not present at LOD 0.
pub fn render_detail_chunk(
    conn: &Connection,
    atlas: &DynamicImage,
    facade_atlas: &DynamicImage,
    cx: i32,
    cy: i32,
) -> Result<Option<DetailRender>> {
    let mut stmt =
        conn.prepare("SELECT render, w, h FROM chunks WHERE cx=?1 AND cy=?2 AND lod=0")?;
    let res = stmt.query_row(params![cx, cy], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, u32>(1)?,
            row.get::<_, u32>(2)?,
        ))
    });
    let (render_blob, w, h) = match res {
        Ok(t) => t,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(e) => return Err(e.into()),
    };

    let tiles = decompress_u32(&render_blob, (w * h) as usize)?;
    let full_tile = ATLAS_TILE;
    let mut img = RgbImage::new(w * full_tile, h * full_tile);
    fill_atlas_bg(&mut img, atlas, SemanticClass::Grass, full_tile);

    // Ground pass.
    for cell_row in 0..h {
        for cell_col in 0..w {
            let tile_id = tiles[(cell_row * w + cell_col) as usize];
            blit_atlas_tile(
                &mut img,
                atlas,
                tile_id_class(tile_id) as u32,
                tile_id_variant(tile_id) as u32,
                cell_col * full_tile,
                cell_row * full_tile,
                full_tile,
            );
        }
    }

    // Buildings, north-first (largest cell_y first) so nearer south buildings
    // are painted last and occlude farther north ones.
    let mut bstmt = conn.prepare(
        "SELECT cell_x, cell_y, cell_w, cell_h, tier FROM buildings
         WHERE cx=?1 AND cy=?2 ORDER BY cell_y DESC",
    )?;
    let buildings: Vec<BuildingRec> = bstmt
        .query_map(params![cx, cy], |row| {
            Ok(BuildingRec {
                cell_x: row.get(0)?,
                cell_y: row.get(1)?,
                cell_w: row.get(2)?,
                cell_h: row.get(3)?,
                tier: row.get(4)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    // Pass 1: cast all drop shadows onto the ground first.
    for b in &buildings {
        let (fx, fy, fw, fh) = footprint_rect(b, h);
        if fw == 0 || fh == 0 {
            continue;
        }
        draw_drop_shadow(&mut img, fx, fy, fw, fh + facade_height_px(b.tier));
    }
    // Greedy adjacency colouring: neighbours never share a roof colour, so even
    // overlapping same-hash footprints read as separate structures.
    let roof_colors = greedy_roof_colors(&buildings, h, cx, cy);

    // Pass 2: facades + roof fills, north-first.
    for (i, b) in buildings.iter().enumerate() {
        draw_building(&mut img, facade_atlas, &tiles, w, h, b, cx, cy, roof_colors[i]);
    }
    // Pass 3: roof bevel edges on top, so an overlapping neighbour's roof cannot
    // erase a building's boundary seam (keeps abutting buildings visually distinct).
    for b in &buildings {
        let (fx, fy, fw, fh) = footprint_rect(b, h);
        if fw == 0 || fh == 0 {
            continue;
        }
        draw_roof_edge(&mut img, fx, fy, fw, fh);
    }

    // Pass 4 (optional): scatter MX trees on grass/park cells if a props atlas exists.
    // Mark every cell covered by a building (footprint + facade) so a tree whose
    // canopy would overlap a building is not planted (no trees on rooftops).
    if let Ok(props) = image::open("assets/props_atlas.png") {
        let mut building_cells = vec![false; (w * h) as usize];
        for b in &buildings {
            let top = (h as f32 - b.cell_y - b.cell_h).max(0.0) as u32;
            let south = (h as f32 - b.cell_y).clamp(0.0, h as f32) as u32;
            let bottom = (south + facade_height_px(b.tier) / ATLAS_TILE).min(h);
            let c0 = b.cell_x.max(0.0) as u32;
            let c1 = ((b.cell_x + b.cell_w).ceil() as u32).min(w);
            for r in top..bottom {
                for c in c0..c1 {
                    building_cells[(r * w + c) as usize] = true;
                }
            }
        }
        draw_trees(&mut img, &props, &tiles, &building_cells, w, h, cx, cy);
    }

    Ok(Some(DetailRender {
        img,
        buildings,
        w_cells: w,
        h_cells: h,
    }))
}

/// Draw one building: composed MX facade below the south edge, then the rooftop.
/// `roof` is the greedy-assigned roof colour; the wall colour comes from the hash.
pub fn draw_building(
    img: &mut RgbImage,
    facade_atlas: &DynamicImage,
    tiles: &[u32],
    w: u32,
    h_cells: u32,
    b: &BuildingRec,
    cx: i32,
    cy: i32,
    roof: u32,
) {
    let (fx, fy, fw, fh) = footprint_rect(b, h_cells);
    if fw == 0 || fh == 0 {
        return;
    }
    let style = building_style(cx, cy, b.cell_x, b.cell_y);
    let facade_h = facade_height_px(b.tier);
    // If the building fronts a diagonal road, shear the facade base to follow it.
    let slope = road_slope(tiles, w, h_cells, b);

    // Facade below the south edge.
    draw_facade(img, facade_atlas, style.wall, fx, fy + fh, fw, facade_h, slope);
    // Rooftop fill over the footprint (per-building colour); bevel edge added later.
    draw_roof_fill(img, facade_atlas, roof, fx, fy, fw, fh);
}

/// Estimate the slope of the road fronting a building's south edge, as road-row
/// depth per cell of x (signed; 0 if no clear road below). Used to shear facades
/// so they follow diagonal streets a little, like buildings on a hill.
fn road_slope(tiles: &[u32], w: u32, h: u32, b: &BuildingRec) -> f32 {
    const SCAN: i32 = 10;
    let south_row = (h as f32 - b.cell_y) as i32;
    let c0 = b.cell_x.floor() as i32;
    let cw = b.cell_w.max(1.0) as i32;
    let mut pts: Vec<(f32, f32)> = Vec::new();
    for cc in 0..cw {
        let c = c0 + cc;
        if c < 0 || c >= w as i32 {
            continue;
        }
        for d in 0..SCAN {
            let r = south_row + d;
            if r < 0 || r >= h as i32 {
                break;
            }
            if tile_id_class(tiles[(r as u32 * w + c as u32) as usize]) == 5 {
                pts.push((cc as f32, d as f32));
                break;
            }
        }
    }
    if pts.len() < 3 {
        return 0.0;
    }
    let n = pts.len() as f32;
    let sx: f32 = pts.iter().map(|p| p.0).sum();
    let sy: f32 = pts.iter().map(|p| p.1).sum();
    let sxx: f32 = pts.iter().map(|p| p.0 * p.0).sum();
    let sxy: f32 = pts.iter().map(|p| p.0 * p.1).sum();
    let denom = n * sxx - sx * sx;
    if denom.abs() < 1e-3 {
        return 0.0;
    }
    ((n * sxy - sx * sy) / denom).clamp(-0.5, 0.5)
}

/// Assign each building a roof colour so that no two adjacent/overlapping
/// footprints share one. Preference comes from the per-building hash; conflicts
/// are resolved greedily in a deterministic order.
fn greedy_roof_colors(buildings: &[BuildingRec], h_cells: u32, cx: i32, cy: i32) -> Vec<u32> {
    let n = buildings.len();
    let rects: Vec<(i32, i32, i32, i32)> = buildings
        .iter()
        .map(|b| {
            let (x, y, w, hh) = footprint_rect(b, h_cells);
            (x as i32, y as i32, w as i32, hh as i32)
        })
        .collect();
    let touch = |a: usize, c: usize| -> bool {
        let (ax, ay, aw, ah) = rects[a];
        let (bx, by, bw, bh) = rects[c];
        // overlap when rect a is padded by 1px (touching counts as adjacent)
        ax - 1 < bx + bw && bx < ax + aw + 1 && ay - 1 < by + bh && by < ay + ah + 1
    };
    let mut colors = vec![u32::MAX; n];
    for i in 0..n {
        let pref = building_style(cx, cy, buildings[i].cell_x, buildings[i].cell_y).roof;
        let mut forbidden = [false; ROOF_VARIANTS as usize];
        for j in 0..n {
            if j != i && colors[j] != u32::MAX && touch(i, j) {
                forbidden[colors[j] as usize] = true;
            }
        }
        let chosen = if !forbidden[pref as usize] {
            pref
        } else {
            (0..ROOF_VARIANTS as u32)
                .find(|c| !forbidden[*c as usize])
                .unwrap_or(pref)
        };
        colors[i] = chosen;
    }
    colors
}

/// Fill a footprint with a per-building roof colour tile (from the facade atlas
/// roof row), tiled across the footprint. Bevel edges are added in a later pass.
fn draw_roof_fill(img: &mut RgbImage, fa: &DynamicImage, roof: u32, x: u32, y: u32, w: u32, h: u32) {
    let rx = roof * FACADE_TS;
    let ry = ROOF_ROW * FACADE_TS;
    let (iw, ih) = (img.width(), img.height());
    for dy in 0..h {
        for dx in 0..w {
            let (px, py) = (x + dx, y + dy);
            if px >= iw || py >= ih {
                continue;
            }
            let sp = fa.get_pixel(rx + dx % FACADE_TS, ry + dy % FACADE_TS);
            if sp[3] != 0 {
                img.put_pixel(px, py, Rgb([sp[0], sp[1], sp[2]]));
            }
        }
    }
}

/// Bevel a roof footprint in place: darken the right/bottom edges (self-shadow,
/// sun top-left) and lighten the top/left edges. Drawn on top of all roof fills
/// so an overlapping neighbour cannot erase the boundary seam.
fn draw_roof_edge(img: &mut RgbImage, x: u32, y: u32, w: u32, h: u32) {
    const EDGE: u32 = 3;
    let (iw, ih) = (img.width(), img.height());
    for dy in 0..h {
        for dx in 0..w {
            let (px, py) = (x + dx, y + dy);
            if px >= iw || py >= ih {
                continue;
            }
            let from_right = w - 1 - dx;
            let from_bottom = h - 1 - dy;
            let f = if from_right < EDGE || from_bottom < EDGE {
                0.5 // shaded right/bottom edge (3D bevel + boundary seam)
            } else if dx < 2 || dy < 2 {
                1.14 // lit top/left highlight
            } else {
                continue;
            };
            let p = *img.get_pixel(px, py);
            img.put_pixel(px, py, Rgb([scale(p[0], f), scale(p[1], f), scale(p[2], f)]));
        }
    }
}

fn scale(v: u8, f: f32) -> u8 {
    (v as f32 * f).round().clamp(0.0, 255.0) as u8
}

/// Scatter trees on Grass/ParkGrass cells using a deterministic hash, drawn
/// north-to-south so southern trees overlap northern ones. Each tree's trunk is
/// anchored to the bottom of its cell and the 96×128 sprite extends up/out.
fn draw_trees(
    img: &mut RgbImage,
    props: &DynamicImage,
    tiles: &[u32],
    building_cells: &[bool],
    w: u32,
    h: u32,
    cx: i32,
    cy: i32,
) {
    const TREE_W: i32 = 96;
    const TREE_H: i32 = 128;
    const CELL: i32 = ATLAS_TILE as i32;
    const N_VARIANTS: u64 = 3;
    const DENSITY: u64 = 7; // percent of eligible cells that get a tree
    let (iw, ih) = (img.width() as i32, img.height() as i32);

    for row in 0..h {
        for col in 0..w {
            let cls = tile_id_class(tiles[(row * w + col) as usize]);
            if cls != 0 && cls != 1 {
                continue; // Grass (0) / ParkGrass (1) only
            }
            let hsh = tree_hash(cx, cy, col, row);
            if hsh % 100 >= DENSITY {
                continue;
            }
            // Skip if the sprite (3 cells wide, 4 tall, canopy extending north)
            // would overlap any building cell — keeps trees off rooftops/facades.
            if tree_overlaps_building(building_cells, w, h, col, row) {
                continue;
            }
            let v = (hsh / 100 % N_VARIANTS) as i32;
            let base_x = col as i32 * CELL + CELL / 2 - TREE_W / 2;
            let base_y = (row as i32 + 1) * CELL - TREE_H;
            let sx0 = v * TREE_W;
            for dy in 0..TREE_H {
                for dx in 0..TREE_W {
                    let (px, py) = (base_x + dx, base_y + dy);
                    if px < 0 || py < 0 || px >= iw || py >= ih {
                        continue;
                    }
                    let sp = props.get_pixel((sx0 + dx) as u32, dy as u32);
                    if sp[3] > 64 {
                        img.put_pixel(px as u32, py as u32, Rgb([sp[0], sp[1], sp[2]]));
                    }
                }
            }
        }
    }
}

/// True if a tree anchored at (col,row) would cover any building cell. The 96×128
/// sprite spans cols [col-1..col+1] and rows [row-3..row] (canopy extends north).
fn tree_overlaps_building(building_cells: &[bool], w: u32, h: u32, col: u32, row: u32) -> bool {
    let r0 = row.saturating_sub(3);
    let c0 = col.saturating_sub(1);
    let c1 = (col + 1).min(w - 1);
    for r in r0..=row {
        for c in c0..=c1 {
            if r < h && building_cells[(r * w + c) as usize] {
                return true;
            }
        }
    }
    false
}

fn tree_hash(cx: i32, cy: i32, col: u32, row: u32) -> u64 {
    let mut h: u64 = 0x9e37_79b9_7f4a_7c15;
    for v in [cx as i64 as u64, cy as i64 as u64, col as u64, row as u64, 0x7233] {
        h ^= v;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Compose a facade: stack CORNICE / window floors / BASE to fill `height_px`,
/// tiled to `width_px`. The wall colour row is `wall`. `slope` shears the base:
/// downhill tile-columns are drawn taller so the ground line follows a diagonal
/// road (the cornice stays flush with the roof at the top).
fn draw_facade(
    img: &mut RgbImage,
    fa: &DynamicImage,
    wall: u32,
    x: u32,
    y: u32,
    width_px: u32,
    height_px: u32,
    slope: f32,
) {
    let n_cols = (width_px / FACADE_TS).max(1);
    let row_y = wall * FACADE_TS;
    let (iw, ih) = (img.width(), img.height());

    // Per-tile-column vertical extension (downhill taller), normalised so the
    // shallowest column is flush (ext 0) and capped to a couple of tiles.
    let slope_px = slope * FACADE_TS as f32;
    let raw: Vec<f32> = (0..n_cols).map(|tc| slope_px * tc as f32).collect();
    let minv = raw.iter().cloned().fold(f32::INFINITY, f32::min);

    for tc in 0..n_cols {
        let ext = ((raw[tc as usize] - minv).round().max(0.0) as u32).min(2 * FACADE_TS);
        let n_rows = ((height_px + ext) / FACADE_TS).max(1);
        let col_x = x + tc * FACADE_TS;
        for r in 0..n_rows {
            let slot = if n_rows >= 3 && r == 0 {
                SLOT_CORNICE
            } else if r == n_rows - 1 {
                SLOT_BASE
            } else {
                let p = if n_rows >= 3 { r - 1 } else { r };
                if p % 2 == 0 {
                    SLOT_WIN_TOP
                } else {
                    SLOT_WIN_BOT
                }
            };
            let sx = slot * FACADE_TS;
            for dy in 0..FACADE_TS {
                for dx in 0..FACADE_TS {
                    let (px, py) = (col_x + dx, y + r * FACADE_TS + dy);
                    if px >= iw || py >= ih {
                        continue;
                    }
                    let sp = fa.get_pixel(sx + dx, row_y + dy);
                    if sp[3] > 64 {
                        img.put_pixel(px, py, Rgb([sp[0], sp[1], sp[2]]));
                    }
                }
            }
        }
    }
}

/// Cast an L-shaped drop shadow (sun top-left) on the ground to the lower-right
/// of a building's silhouette rectangle (x, y, w, h_total = footprint + facade).
fn draw_drop_shadow(img: &mut RgbImage, x: u32, y: u32, w: u32, h_total: u32) {
    let (iw, ih) = (img.width(), img.height());
    let s = SHADOW_W;
    // Right band, offset down by s.
    for sy in (y + s)..(y + h_total + s) {
        for sx in (x + w)..(x + w + s) {
            darken(img, sx, sy, iw, ih);
        }
    }
    // Bottom band, offset right by s.
    for sy in (y + h_total)..(y + h_total + s) {
        for sx in (x + s)..(x + w + s) {
            darken(img, sx, sy, iw, ih);
        }
    }
}

fn darken(img: &mut RgbImage, x: u32, y: u32, iw: u32, ih: u32) {
    if x >= iw || y >= ih {
        return;
    }
    let p = img.get_pixel(x, y);
    img.put_pixel(
        x,
        y,
        Rgb([
            (p[0] as f32 * SHADOW_MUL) as u8,
            (p[1] as f32 * SHADOW_MUL) as u8,
            (p[2] as f32 * SHADOW_MUL) as u8,
        ]),
    );
}

/// Blit an atlas tile into the destination using nearest-neighbour scaling.
pub fn blit_atlas_tile(
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
            if px_x < dst_w
                && px_y < dst_h
                && atlas_x + src_dx < atlas.width()
                && atlas_y + src_dy < atlas.height()
            {
                let p = atlas.get_pixel(atlas_x + src_dx, atlas_y + src_dy);
                dst.put_pixel(px_x, px_y, Rgb([p[0], p[1], p[2]]));
            }
        }
    }
}

/// Fill the whole image with a class's solid interior tile (variant 46 centre).
pub fn fill_atlas_bg(dst: &mut RgbImage, atlas: &DynamicImage, class: SemanticClass, px_size: u32) {
    let atlas_x = 46 * ATLAS_TILE;
    let atlas_y = class as u32 * ATLAS_TILE;
    let p = atlas.get_pixel(atlas_x + 16, atlas_y + 16);
    let bg = Rgb([p[0], p[1], p[2]]);
    let _ = px_size;
    for px in dst.pixels_mut() {
        *px = bg;
    }
}
