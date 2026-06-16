/// Phase 6 tests: LOD semantic majority-vote downsample and collision max-pool.

use sim_maps::{
    lod::{downsample_collision, downsample_semantic},
    types::{Grid, SemanticClass, collision},
};

// ── downsample_semantic ───────────────────────────────────────────────────────

#[test]
fn uniform_grid_stays_same_class() {
    // 4×4 uniform Water grid, factor 2 → 2×2, all Water.
    let grid = Grid::filled(4, 4, SemanticClass::Water);
    let ds = downsample_semantic(&grid, 2);
    assert_eq!(ds.width, 2);
    assert_eq!(ds.height, 2);
    for row in 0..2u32 {
        for col in 0..2u32 {
            assert_eq!(*ds.get(col, row), SemanticClass::Water);
        }
    }
}

#[test]
fn majority_class_wins() {
    // 2×2 block: 3 Grass + 1 Road → Grass wins.
    let mut grid = Grid::filled(2, 2, SemanticClass::Grass);
    grid.set(1, 1, SemanticClass::Road);
    let ds = downsample_semantic(&grid, 2);
    assert_eq!(*ds.get(0, 0), SemanticClass::Grass);
}

#[test]
fn tie_broken_by_higher_precedence() {
    // 2×2 block: 2 Grass (precedence 0) + 2 Water (precedence 10) → Water wins.
    let mut grid = Grid::filled(2, 2, SemanticClass::Grass);
    grid.set(1, 0, SemanticClass::Water);
    grid.set(0, 1, SemanticClass::Water);
    let ds = downsample_semantic(&grid, 2);
    assert_eq!(*ds.get(0, 0), SemanticClass::Water,
        "tie should resolve to highest-precedence class");
}

#[test]
fn factor_4_reduces_to_correct_size() {
    // 8×8 → 2×2 with factor 4.
    let grid = Grid::filled(8, 8, SemanticClass::Road);
    let ds = downsample_semantic(&grid, 4);
    assert_eq!(ds.width, 2);
    assert_eq!(ds.height, 2);
}

#[test]
fn non_divisible_width_rounds_up() {
    // 5×5 grid, factor 2 → ceil(5/2)=3 → 3×3.
    let grid = Grid::filled(5, 5, SemanticClass::Grass);
    let ds = downsample_semantic(&grid, 2);
    assert_eq!(ds.width, 3);
    assert_eq!(ds.height, 3);
}

#[test]
fn non_divisible_border_block_uses_available_cells() {
    // 3×1 grid: [Road, Grass, Grass], factor 2.
    // Block 0: covers col 0-1 → Road(1), Grass(1) → tie → Road (higher precedence).
    // Block 1: covers col 2 → Grass(1) → Grass.
    let mut grid = Grid::filled(3, 1, SemanticClass::Grass);
    grid.set(0, 0, SemanticClass::Road);
    let ds = downsample_semantic(&grid, 2);
    assert_eq!(ds.width, 2);
    assert_eq!(*ds.get(0, 0), SemanticClass::Road,
        "tie between Road(prec 5) and Grass(prec 0) → Road");
    assert_eq!(*ds.get(1, 0), SemanticClass::Grass);
}

#[test]
fn single_cell_grid_factor_2_produces_1x1() {
    let grid = Grid::filled(1, 1, SemanticClass::Path);
    let ds = downsample_semantic(&grid, 2);
    assert_eq!(ds.width, 1);
    assert_eq!(ds.height, 1);
    assert_eq!(*ds.get(0, 0), SemanticClass::Path);
}

#[test]
fn lod1_then_lod2_matches_direct_factor4() {
    // Downsampling twice by 2 should give the same result as once by 4
    // when the block is uniform (majority stays stable).
    let grid = Grid::filled(8, 8, SemanticClass::Water);
    let lod1 = downsample_semantic(&grid, 2);
    let lod2_chained = downsample_semantic(&lod1, 2);
    let lod2_direct  = downsample_semantic(&grid, 4);
    assert_eq!(lod2_chained.width,  lod2_direct.width);
    assert_eq!(lod2_chained.height, lod2_direct.height);
    for row in 0..lod2_direct.height {
        for col in 0..lod2_direct.width {
            assert_eq!(
                *lod2_chained.get(col, row),
                *lod2_direct.get(col, row),
                "chained and direct downsample should agree for uniform grid"
            );
        }
    }
}

// ── downsample_collision ──────────────────────────────────────────────────────

#[test]
fn collision_max_pool_picks_highest_cost() {
    // 2×2 block: WALKABLE, GRASS_COST, BLOCKED, PATH_COST → max = BLOCKED.
    let mut grid = Grid::filled(2, 2, collision::WALKABLE);
    grid.set(0, 0, collision::GRASS_COST);
    grid.set(1, 0, collision::BLOCKED);
    grid.set(0, 1, collision::PATH_COST);
    let ds = downsample_collision(&grid, 2);
    assert_eq!(*ds.get(0, 0), collision::BLOCKED,
        "single BLOCKED cell should dominate the block");
}

#[test]
fn collision_uniform_walkable_stays_walkable() {
    let grid = Grid::filled(4, 4, collision::WALKABLE);
    let ds = downsample_collision(&grid, 2);
    for row in 0..2u32 {
        for col in 0..2u32 {
            assert_eq!(*ds.get(col, row), collision::WALKABLE);
        }
    }
}

#[test]
fn collision_size_matches_semantic_downsample() {
    let sem = Grid::filled(125, 125, SemanticClass::Grass);
    let coll = Grid::filled(125, 125, collision::GRASS_COST);
    let sem_ds  = downsample_semantic(&sem,  2);
    let coll_ds = downsample_collision(&coll, 2);
    assert_eq!(sem_ds.width,  coll_ds.width,  "semantic and collision LOD1 widths must match");
    assert_eq!(sem_ds.height, coll_ds.height, "semantic and collision LOD1 heights must match");
}

#[test]
fn lod1_size_for_125_cells() {
    // 125 × 125 cells at factor 2 → ceil(125/2) = 63 × 63.
    let grid = Grid::filled(125, 125, SemanticClass::Grass);
    let ds = downsample_semantic(&grid, 2);
    assert_eq!(ds.width,  63, "LOD1 width should be 63");
    assert_eq!(ds.height, 63, "LOD1 height should be 63");
}

#[test]
fn lod2_size_for_125_cells() {
    // 125 × 125 cells at factor 4 → ceil(125/4) = 32 × 32.
    let grid = Grid::filled(125, 125, SemanticClass::Grass);
    let ds = downsample_semantic(&grid, 4);
    assert_eq!(ds.width,  32, "LOD2 width should be 32");
    assert_eq!(ds.height, 32, "LOD2 height should be 32");
}

#[test]
fn lod3_size_for_125_cells() {
    // factor 8 → ceil(125/8) = 16 × 16.
    let grid = Grid::filled(125, 125, SemanticClass::Grass);
    let ds = downsample_semantic(&grid, 8);
    assert_eq!(ds.width,  16);
    assert_eq!(ds.height, 16);
}

#[test]
fn lod4_size_for_125_cells() {
    // factor 16 → ceil(125/16) = 8 × 8.
    let grid = Grid::filled(125, 125, SemanticClass::Grass);
    let ds = downsample_semantic(&grid, 16);
    assert_eq!(ds.width,  8);
    assert_eq!(ds.height, 8);
}

#[test]
fn lod5_size_for_125_cells() {
    // factor 32 → ceil(125/32) = 4 × 4.
    let grid = Grid::filled(125, 125, SemanticClass::Grass);
    let ds = downsample_semantic(&grid, 32);
    assert_eq!(ds.width,  4);
    assert_eq!(ds.height, 4);
}

// ── SemanticClass::from_u8 round-trip ────────────────────────────────────────

#[test]
fn from_u8_round_trips_all_classes() {
    use sim_maps::types::SemanticClass;
    let classes = [
        SemanticClass::Grass,
        SemanticClass::ParkGrass,
        SemanticClass::Sand,
        SemanticClass::Path,
        SemanticClass::Sidewalk,
        SemanticClass::Road,
        SemanticClass::Stairs,
        SemanticClass::CliffFace,
        SemanticClass::BuildingFloor,
        SemanticClass::BuildingWall,
        SemanticClass::Water,
    ];
    for cls in classes {
        assert_eq!(SemanticClass::from_u8(cls as u8), cls,
            "from_u8({}) should round-trip", cls as u8);
    }
}

#[test]
fn from_u8_unknown_defaults_to_grass() {
    use sim_maps::types::SemanticClass;
    assert_eq!(SemanticClass::from_u8(255), SemanticClass::Grass);
    assert_eq!(SemanticClass::from_u8(200), SemanticClass::Grass);
}
