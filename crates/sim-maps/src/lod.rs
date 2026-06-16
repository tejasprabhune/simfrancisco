/// Phase 6: LOD downsampling.
///
/// LOD 0 is the full-resolution raster (2 m/cell).
/// LOD n is produced by pooling (2^n × 2^n) blocks of LOD-0 cells:
///   - semantic: majority vote with precedence-based tie-breaking
///   - collision: max-pool (most restrictive cost survives)
///
/// Re-autotiling is done in main.rs after downsampling the semantic grid so
/// that tile borders look correct at every zoom level.

use crate::types::{Grid, SemanticClass};

/// Downsample a semantic grid by `factor` using majority vote.
///
/// Output cell (oc, or) collects all input cells in the rectangle
/// [oc*factor .. (oc+1)*factor) × [or*factor .. (or+1)*factor).
/// Among classes with the highest count, the one with the highest precedence
/// wins ties so that impassable features (Water, BuildingWall, CliffFace)
/// survive aggressive downsampling.
pub fn downsample_semantic(grid: &Grid<SemanticClass>, factor: u32) -> Grid<SemanticClass> {
    assert!(factor >= 1, "downsample factor must be ≥ 1");
    let out_w = (grid.width  + factor - 1) / factor;
    let out_h = (grid.height + factor - 1) / factor;

    Grid::from_fn(out_w, out_h, |oc, or_| {
        let x0 = oc * factor;
        let y0 = or_ * factor;
        let x1 = (x0 + factor).min(grid.width);
        let y1 = (y0 + factor).min(grid.height);
        majority_class(grid, x0, y0, x1, y1)
    })
}

/// Downsample a u8 collision grid by `factor` using max-pool.
///
/// The highest (most restrictive) cost in each block propagates, so that
/// a single BLOCKED cell (255) keeps the whole LOD block impassable.
pub fn downsample_collision(grid: &Grid<u8>, factor: u32) -> Grid<u8> {
    assert!(factor >= 1, "downsample factor must be ≥ 1");
    let out_w = (grid.width  + factor - 1) / factor;
    let out_h = (grid.height + factor - 1) / factor;

    Grid::from_fn(out_w, out_h, |oc, or_| {
        let x0 = oc * factor;
        let y0 = or_ * factor;
        let x1 = (x0 + factor).min(grid.width);
        let y1 = (y0 + factor).min(grid.height);
        let mut max_cost = 0u8;
        for row in y0..y1 {
            for col in x0..x1 {
                max_cost = max_cost.max(*grid.get(col, row));
            }
        }
        max_cost
    })
}

const NUM_CLASSES: usize = 15;

fn majority_class(
    grid: &Grid<SemanticClass>,
    x0: u32, y0: u32,
    x1: u32, y1: u32,
) -> SemanticClass {
    let mut counts = [0u32; NUM_CLASSES];
    for row in y0..y1 {
        for col in x0..x1 {
            let idx = *grid.get(col, row) as usize;
            if idx < NUM_CLASSES {
                counts[idx] += 1;
            }
        }
    }
    let max_count = *counts.iter().max().unwrap_or(&0);
    (0u8..NUM_CLASSES as u8)
        .filter(|&i| counts[i as usize] == max_count)
        .map(SemanticClass::from_u8)
        .max_by_key(|c| c.precedence())
        .unwrap_or(SemanticClass::Grass)
}
