/// Phase 5: blob autotile variant computation.
///
/// Each cell inspects its 8 cardinal+diagonal neighbours.  If a neighbour
/// has the same SemanticClass it contributes a bit to the raw 8-bit mask.
/// Corner bits are then suppressed (a diagonal only counts if *both* adjacent
/// edge neighbours are also the same class), yielding exactly 47 distinct
/// normalised configurations that map to autotile variant indices 0–46.
///
/// Bit layout (clockwise from north):
///   bit 0 = N,  bit 1 = NE, bit 2 = E,  bit 3 = SE
///   bit 4 = S,  bit 5 = SW, bit 6 = W,  bit 7 = NW
///
/// Out-of-bounds neighbours are treated as the same class so that chunk edges
/// do not produce spurious autotile borders.

use crate::types::{AutotileVariant, Grid, SemanticClass};

/// Compute the raw 8-bit same-class neighbour mask for one cell.
pub fn neighbor_mask(grid: &Grid<SemanticClass>, col: u32, row: u32) -> u8 {
    let center = *grid.get(col, row);
    let w = grid.width as i32;
    let h = grid.height as i32;

    let offsets: [(i32, i32, u8); 8] = [
        ( 0, -1, 0), // N
        ( 1, -1, 1), // NE
        ( 1,  0, 2), // E
        ( 1,  1, 3), // SE
        ( 0,  1, 4), // S
        (-1,  1, 5), // SW
        (-1,  0, 6), // W
        (-1, -1, 7), // NW
    ];

    let mut mask = 0u8;
    for (dc, dr, bit) in offsets {
        let nc = col as i32 + dc;
        let nr = row as i32 + dr;
        let same = if nc < 0 || nr < 0 || nc >= w || nr >= h {
            true  // out-of-bounds → same class (no border at chunk edge)
        } else {
            *grid.get(nc as u32, nr as u32) == center
        };
        if same {
            mask |= 1 << bit;
        }
    }
    mask
}

/// Suppress corner bits whose adjacent edge neighbours are not both set.
///
/// A diagonal is only meaningful when the two flanking edge cells are also
/// the same class; otherwise it is cleared.
pub fn suppress_corners(raw: u8) -> u8 {
    let n  = (raw >> 0) & 1;
    let ne = (raw >> 1) & 1;
    let e  = (raw >> 2) & 1;
    let se = (raw >> 3) & 1;
    let s  = (raw >> 4) & 1;
    let sw = (raw >> 5) & 1;
    let w  = (raw >> 6) & 1;
    let nw = (raw >> 7) & 1;

    let ne = ne & n & e;
    let se = se & s & e;
    let sw = sw & s & w;
    let nw = nw & n & w;

    n | (ne << 1) | (e << 2) | (se << 3) | (s << 4) | (sw << 5) | (w << 6) | (nw << 7)
}

/// Lookup table: corner-suppressed neighbour mask → blob variant index (0–46).
///
/// Exactly 47 normalised 8-bit values survive corner suppression; all other
/// entries are unreachable and map to 0 (isolated tile, safe fallback).
///
/// Variants are ordered by ascending normalised mask value:
///   variant 0 → mask 0x00 (isolated)
///   variant 46 → mask 0xFF (fully surrounded)
pub static BLOB_VARIANTS: [AutotileVariant; 256] = [
    // 0x00–0x0F
     0,  1,  0,  0,  2,  3,  0,  4,  0,  0,  0,  0,  0,  0,  0,  0,
    // 0x10–0x1F
     5,  6,  0,  0,  7,  8,  0,  9,  0,  0,  0,  0, 10, 11,  0, 12,
    // 0x20–0x3F  (unreachable after suppression)
     0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
     0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
    // 0x40–0x4F
    13, 14,  0,  0, 15, 16,  0, 17,  0,  0,  0,  0,  0,  0,  0,  0,
    // 0x50–0x5F
    18, 19,  0,  0, 20, 21,  0, 22,  0,  0,  0,  0, 23, 24,  0, 25,
    // 0x60–0x6F  (unreachable)
     0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
    // 0x70–0x7F
    26, 27,  0,  0, 28, 29,  0, 30,  0,  0,  0,  0, 31, 32,  0, 33,
    // 0x80–0xBF  (unreachable)
     0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
     0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
     0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
     0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
    // 0xC0–0xCF
     0, 34,  0,  0,  0, 35,  0, 36,  0,  0,  0,  0,  0,  0,  0,  0,
    // 0xD0–0xDF
     0, 37,  0,  0,  0, 38,  0, 39,  0,  0,  0,  0,  0, 40,  0, 41,
    // 0xE0–0xEF  (unreachable)
     0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,  0,
    // 0xF0–0xFF
     0, 42,  0,  0,  0, 43,  0, 44,  0,  0,  0,  0,  0, 45,  0, 46,
];

/// Return the blob variant index (0–46) for one cell.
pub fn blob_variant(raw: u8) -> AutotileVariant {
    BLOB_VARIANTS[suppress_corners(raw) as usize]
}

/// Compute a full grid of autotile variants from the semantic grid.
pub fn autotile_grid(semantic: &Grid<SemanticClass>) -> Grid<AutotileVariant> {
    Grid::from_fn(semantic.width, semantic.height, |col, row| {
        blob_variant(neighbor_mask(semantic, col, row))
    })
}

/// Number of distinct blob tile variants.
pub const VARIANT_COUNT: u8 = 47;
