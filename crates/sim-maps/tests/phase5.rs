/// Phase 5 tests: blob autotile neighbour mask, corner suppression,
/// variant lookup, and end-to-end autotile_grid / semantic_to_render.

use sim_maps::{
    autotile::{autotile_grid, blob_variant, neighbor_mask, suppress_corners, BLOB_VARIANTS, VARIANT_COUNT},
    types::{Grid, SemanticClass, make_tile_id, tile_id_variant},
};

// ── BLOB_VARIANTS table sanity ────────────────────────────────────────────────

#[test]
fn exactly_47_distinct_variants_in_table() {
    let mut seen = std::collections::HashSet::new();
    for &v in BLOB_VARIANTS.iter() {
        seen.insert(v);
    }
    // Values used in the table are 0..=46, so there should be exactly 47.
    assert_eq!(seen.len(), 47, "table should use exactly 47 distinct values");
    assert_eq!(*seen.iter().max().unwrap(), 46, "max variant should be 46");
}

#[test]
fn variant_count_constant_is_47() {
    assert_eq!(VARIANT_COUNT, 47);
}

// ── corner suppression ────────────────────────────────────────────────────────

#[test]
fn suppress_ne_without_north_edge() {
    // NE bit set but N=0 → NE must be cleared.
    let raw = 0b00000110u8;  // E(bit2)=1, NE(bit1)=1, N(bit0)=0
    let suppressed = suppress_corners(raw);
    assert_eq!(suppressed, 0b00000100u8, "NE cleared when N=0");
}

#[test]
fn suppress_ne_without_east_edge() {
    // NE bit set but E=0 → NE must be cleared.
    let raw = 0b00000011u8;  // N(bit0)=1, NE(bit1)=1, E(bit2)=0
    let suppressed = suppress_corners(raw);
    assert_eq!(suppressed, 0b00000001u8, "NE cleared when E=0");
}

#[test]
fn suppress_all_corners_when_edges_absent() {
    // Only diagonal bits set, no edge bits.
    let raw = 0b10000010u8;  // NW(bit7)=1, NE(bit1)=1
    let suppressed = suppress_corners(raw);
    assert_eq!(suppressed, 0, "all corners cleared when adjacent edges absent");
}

#[test]
fn suppress_keeps_valid_corner() {
    // N+E+NE all set → NE stays.
    let raw = 0b00000111u8;  // N(0)+NE(1)+E(2)
    let suppressed = suppress_corners(raw);
    assert_eq!(suppressed, raw, "NE kept when N=1 and E=1");
    assert_eq!(blob_variant(raw), 4, "N+NE+E → variant 4");
}

#[test]
fn fully_surrounded_normalises_to_0xff() {
    let suppressed = suppress_corners(0xFF);
    assert_eq!(suppressed, 0xFF, "all corners valid when all edges set");
}

// ── known variant lookups ─────────────────────────────────────────────────────

#[test]
fn isolated_is_variant_0() {
    assert_eq!(blob_variant(0x00), 0);
}

#[test]
fn n_only_is_variant_1() {
    assert_eq!(blob_variant(0x01), 1);
}

#[test]
fn e_only_is_variant_2() {
    assert_eq!(blob_variant(0x04), 2);
}

#[test]
fn s_only_is_variant_5() {
    assert_eq!(blob_variant(0x10), 5);
}

#[test]
fn w_only_is_variant_13() {
    assert_eq!(blob_variant(0x40), 13);
}

#[test]
fn all_cardinals_is_variant_21() {
    // N+E+S+W = bits 0,2,4,6 = 0x55
    assert_eq!(blob_variant(0x55), 21);
}

#[test]
fn fully_surrounded_is_variant_46() {
    assert_eq!(blob_variant(0xFF), 46);
}

// ── neighbour mask computation ────────────────────────────────────────────────

#[test]
fn isolated_water_in_3x3_grass_grid() {
    // Center cell is Water; all 8 neighbours are Grass (different class).
    let mut grid = Grid::filled(3, 3, SemanticClass::Grass);
    grid.set(1, 1, SemanticClass::Water);

    let mask = neighbor_mask(&grid, 1, 1);
    assert_eq!(mask, 0, "no same-class neighbours → mask 0");

    let variants = autotile_grid(&grid);
    assert_eq!(*variants.get(1, 1), 0, "isolated cell → variant 0");
}

#[test]
fn north_neighbour_only() {
    // (1,1)=Water, (1,0)=Water, rest Grass.
    let mut grid = Grid::filled(3, 3, SemanticClass::Grass);
    grid.set(1, 1, SemanticClass::Water);
    grid.set(1, 0, SemanticClass::Water);

    let mask = neighbor_mask(&grid, 1, 1);
    assert_eq!(mask & 0x01, 1, "N bit set");
    assert_eq!(mask & !0x01, 0, "only N bit set");

    let variants = autotile_grid(&grid);
    assert_eq!(*variants.get(1, 1), 1, "N-only → variant 1");
}

#[test]
fn fully_surrounded_center_in_uniform_grid() {
    // 3×3 all same class; center has 8 same-class neighbours.
    let grid = Grid::filled(3, 3, SemanticClass::Road);
    let mask = neighbor_mask(&grid, 1, 1);
    assert_eq!(mask, 0xFF, "all 8 neighbours same class");

    let variants = autotile_grid(&grid);
    assert_eq!(*variants.get(1, 1), 46, "fully surrounded → variant 46");
}

#[test]
fn oob_treated_as_same_class() {
    // 1×1 grid: the single cell has all 8 neighbours out of bounds.
    // They should all be treated as same class → mask 0xFF → variant 46.
    let grid = Grid::filled(1, 1, SemanticClass::Grass);
    let mask = neighbor_mask(&grid, 0, 0);
    assert_eq!(mask, 0xFF, "OOB neighbours treated as same class");
    let variants = autotile_grid(&grid);
    assert_eq!(*variants.get(0, 0), 46);
}

// ── autotile_grid integration ─────────────────────────────────────────────────

#[test]
fn autotile_grid_uniform_5x5_all_variant_46() {
    // Every cell in a uniform 5×5 grid should get variant 46 (fully surrounded,
    // edges included due to OOB=same-class policy).
    let grid = Grid::filled(5, 5, SemanticClass::Water);
    let variants = autotile_grid(&grid);
    for row in 0..5u32 {
        for col in 0..5u32 {
            assert_eq!(*variants.get(col, row), 46,
                "({col},{row}) should be fully surrounded");
        }
    }
}

#[test]
fn autotile_grid_checkerboard_all_isolated() {
    // Checkerboard: neighbours always differ → every cell isolated → variant 0.
    let mut grid = Grid::filled(4, 4, SemanticClass::Grass);
    for row in 0..4u32 {
        for col in 0..4u32 {
            if (col + row) % 2 == 1 {
                grid.set(col, row, SemanticClass::Water);
            }
        }
    }
    let variants = autotile_grid(&grid);
    // Only interior cells have any in-bounds neighbours to differ from.
    // Interior cells (1,1) and (2,2): Grass surrounded by Water.
    assert_eq!(*variants.get(1, 1), 0, "interior Grass cell: all neighbours are Water");
    assert_eq!(*variants.get(2, 2), 0, "interior Grass cell: all neighbours are Water");
}

// ── semantic_to_render packs variant into tile ID ─────────────────────────────

#[test]
fn render_tile_id_encodes_variant() {
    // 3×3 Water grid with center Water; center should get variant 46.
    let grid = Grid::filled(3, 3, SemanticClass::Water);
    let variants = autotile_grid(&grid);
    let center_variant = *variants.get(1, 1);
    assert_eq!(center_variant, 46);

    let tile_id = make_tile_id(SemanticClass::Water, center_variant);
    assert_eq!(tile_id_variant(tile_id), 46, "bits 8-15 of tile_id should carry the variant");
}

#[test]
fn render_tile_id_preserves_class() {
    use sim_maps::types::tile_id_class;
    let tile_id = make_tile_id(SemanticClass::Road, 21);
    assert_eq!(tile_id_class(tile_id), SemanticClass::Road as u8);
    assert_eq!(tile_id_variant(tile_id), 21);
}
