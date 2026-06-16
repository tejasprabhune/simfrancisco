/// Phase 4 topography rules.
///
/// Applied after Phase 3 rasterization (semantic grid) and Phase 2 DEM
/// sampling (elevation grid).  Mutates both grids in-place.
///
/// Steps:
///   1. Building flattening — for each building polygon, compute the mean
///      elevation of all enclosed cells and write that value back, so the
///      building sits on a flat pad rather than following the hillside.
///   2. Cliff / stairs classification — any cell whose max-absolute-rise to
///      a cardinal neighbor exceeds `walkable_threshold_m` is reclassified:
///        Road / Path / Sidewalk  →  Stairs   (steep but walkable)
///        everything else (except Water / Building / Stairs) → CliffFace
///
/// Water cells, building cells, and already-classified cliff/stairs cells
/// are never overridden here.

use geo::{Contains, Point, Polygon};
use geo::algorithm::bounding_rect::BoundingRect;

use crate::config::ElevationConfig;
use crate::dem::compute_max_rise;
use crate::types::{Grid, SemanticClass};

/// Apply all Phase-4 topography rules to one chunk.
///
/// `origin_x`, `origin_y` — UTM SW corner.
/// `mpc` — metres per cell (2.0).
pub fn apply_topography(
    semantic: &mut Grid<SemanticClass>,
    elevation: &mut Grid<f32>,
    building_polys: &[Polygon<f64>],
    origin_x: f64,
    origin_y: f64,
    mpc: f64,
    cfg: &ElevationConfig,
) {
    // Step 1: flatten each building footprint to its mean DEM elevation.
    if cfg.flatten_buildings {
        for poly in building_polys {
            flatten_building(elevation, poly, origin_x, origin_y, mpc);
        }
    }

    // Step 2: classify steep cells.
    let rise = compute_max_rise(elevation);
    let threshold = cfg.walkable_threshold_m as f32;

    for row in 0..semantic.height {
        for col in 0..semantic.width {
            if *rise.get(col, row) <= threshold {
                continue;
            }
            let cls = *semantic.get(col, row);
            let new_cls = match cls {
                // Steep traversable infrastructure → Stairs.
                SemanticClass::Road | SemanticClass::Path | SemanticClass::Sidewalk => {
                    SemanticClass::Stairs
                }
                // Water, buildings, and already-set cliffs/stairs unchanged.
                SemanticClass::Water
                | SemanticClass::BuildingFloor
                | SemanticClass::BuildingWall
                | SemanticClass::CliffFace
                | SemanticClass::Stairs => cls,
                // Natural terrain that is too steep → CliffFace.
                _ => SemanticClass::CliffFace,
            };
            if new_cls != cls {
                semantic.set(col, row, new_cls);
            }
        }
    }
}

/// Flatten the elevation grid under one building polygon.
///
/// Finds all cells whose centre lies inside `poly`, computes their mean
/// elevation, and writes that value back to every one of them.
fn flatten_building(
    elevation: &mut Grid<f32>,
    poly: &Polygon<f64>,
    origin_x: f64,
    origin_y: f64,
    mpc: f64,
) {
    let w = elevation.width;
    let h = elevation.height;

    let rect = match poly.bounding_rect() {
        Some(r) => r,
        None => return,
    };

    // Cell range that overlaps the polygon bounding box.
    let top = origin_y + h as f64 * mpc;
    let c0 = ((rect.min().x - origin_x) / mpc).floor().max(0.0) as u32;
    let c1 = ((rect.max().x - origin_x) / mpc).ceil().min(w as f64 - 1.0).max(0.0) as u32;
    let r0 = ((top - rect.max().y) / mpc).floor().max(0.0) as u32;
    let r1 = ((top - rect.min().y) / mpc).ceil().min(h as f64 - 1.0).max(0.0) as u32;

    let mut sum = 0.0f64;
    let mut cells: Vec<(u32, u32)> = Vec::new();

    for row in r0..=r1 {
        for col in c0..=c1 {
            let pt = cell_center(col, row, origin_x, origin_y, h, mpc);
            if poly.contains(&pt) {
                sum += (*elevation.get(col, row)) as f64;
                cells.push((col, row));
            }
        }
    }

    if cells.is_empty() {
        return;
    }
    let avg = (sum / cells.len() as f64) as f32;
    for (col, row) in cells {
        elevation.set(col, row, avg);
    }
}

/// Cell centre in UTM coordinates.  Row 0 = northernmost.
fn cell_center(col: u32, row: u32, origin_x: f64, origin_y: f64, height: u32, mpc: f64) -> Point<f64> {
    Point::new(
        origin_x + (col as f64 + 0.5) * mpc,
        origin_y + (height as f64 - row as f64 - 0.5) * mpc,
    )
}

/// Quantise elevation values to 8 bands (0 = lowest, 7 = highest).
/// Useful for elevation-based tinting of the render layer.
pub fn elevation_bands(elevation: &Grid<f32>, min_m: f32, max_m: f32) -> Grid<u8> {
    let range = (max_m - min_m).max(1.0);
    Grid::from_fn(elevation.width, elevation.height, |col, row| {
        let e = (*elevation.get(col, row)).clamp(min_m, max_m);
        ((e - min_m) / range * 7.0) as u8
    })
}

/// Movement-cost modifier for sloped walkable terrain.
///
/// Cells above threshold are already reclassified to CliffFace/Stairs by
/// apply_topography, so those cells return 0 here (their base collision
/// cost already reflects the impassable/steep classification).
///
/// Bands (relative to walkable_threshold_m):
///   rise < threshold/3   →  +0 (flat)
///   rise < threshold*2/3 →  +1 (gentle)
///   rise < threshold     →  +2 (approaching cliff limit)
///   rise >= threshold    →  +0 (already cliff/stairs)
pub fn slope_cost_modifier(rise: f32, threshold: f32) -> u8 {
    let t = threshold.max(f32::EPSILON);
    if rise >= t              { 0 }
    else if rise >= t * 2.0 / 3.0 { 2 }
    else if rise >= t / 3.0        { 1 }
    else                           { 0 }
}
