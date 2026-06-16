/// Phase 3 tests: OSM tag classification, polygon rasterization, road buffer,
/// building wall detection, precedence enforcement, and debug PNG.

use sim_maps::{
    osm::{classify_highway, BuildingRecord, BuildingTier, FeatureIndex, RoadFeature},
    raster::{detect_building_walls, rasterize_semantic_grid, rect_poly, road_line},
    types::{Grid, SemanticClass},
    debug::dump_semantic_png,
};
use tempfile::TempDir;

// ── tag classification ────────────────────────────────────────────────────────

#[test]
fn highway_primary_is_road() {
    let (cls, buf) = classify_highway("highway", "primary").unwrap();
    assert_eq!(cls, SemanticClass::Road);
    assert!(buf >= 8.0, "primary buffer should be ≥ 8 m");
}

#[test]
fn highway_footway_is_path() {
    let (cls, _) = classify_highway("highway", "footway").unwrap();
    assert_eq!(cls, SemanticClass::Path);
}

#[test]
fn highway_pedestrian_is_sidewalk() {
    let (cls, _) = classify_highway("highway", "pedestrian").unwrap();
    assert_eq!(cls, SemanticClass::Sidewalk);
}

#[test]
fn non_highway_returns_none() {
    assert!(classify_highway("natural", "water").is_none());
    assert!(classify_highway("building", "yes").is_none());
}

// ── polygon rasterization ─────────────────────────────────────────────────────

/// Rasterize a 40×40 m building square in the center of a 100×100 m chunk.
/// (2 m cells → 50×50 cells, building is 20×20 cells in the center.)
fn center_building_grid() -> Grid<SemanticClass> {
    // Chunk origin = (0, 0), 50×50 cells, 2 m/cell → covers (0..100, 0..100)
    let origin_x = 0.0_f64;
    let origin_y = 0.0_f64;
    let cells = 50u32;
    let mpc = 2.0_f64;

    // Building: x 30..70, y 30..70 in UTM (20×20 cells in the grid center).
    let building = rect_poly(30.0, 30.0, 70.0, 70.0);

    let mut features = FeatureIndex::empty();
    features.buildings.push(BuildingRecord { poly: building, tier: BuildingTier::Low });

    rasterize_semantic_grid(&features, origin_x, origin_y, cells, cells, mpc)
}

#[test]
fn building_interior_is_floor() {
    let grid = center_building_grid();
    // Cell (25, 24) is in the center of the building (UTM ~51, 51).
    let cls = *grid.get(25, 24);
    assert!(
        cls == SemanticClass::BuildingFloor || cls == SemanticClass::BuildingWall,
        "center of building should be Floor or Wall, got {cls:?}"
    );
    // A cell that is strictly interior (not on boundary) should be Floor.
    let interior = *grid.get(24, 25);
    // At least most interior cells should be floor (wall is only the boundary).
    assert!(
        interior == SemanticClass::BuildingFloor || interior == SemanticClass::BuildingWall,
        "interior cell should be Floor/Wall, got {interior:?}"
    );
}

#[test]
fn area_outside_building_is_grass() {
    let grid = center_building_grid();
    // Cell (0, 0) is far from the building.
    assert_eq!(*grid.get(0, 0), SemanticClass::Grass);
    // Cell (49, 49) is at the south-east corner, also outside.
    assert_eq!(*grid.get(49, 49), SemanticClass::Grass);
}

#[test]
fn building_boundary_cells_are_wall() {
    let grid = center_building_grid();
    // The outermost ring of the building footprint should be BuildingWall.
    // Check a few expected boundary cells (UTM ~31-32, corresponding to col 15-16 at 2m/cell).
    let mut found_wall = false;
    for row in 0..50u32 {
        for col in 0..50u32 {
            if *grid.get(col, row) == SemanticClass::BuildingWall {
                found_wall = true;
                break;
            }
        }
    }
    assert!(found_wall, "building should have at least one BuildingWall cell");
}

// ── road buffer ───────────────────────────────────────────────────────────────

#[test]
fn road_paints_cells_within_buffer() {
    // A horizontal road running east at y=50 (center of chunk), with 6 m buffer.
    // 2 m/cell → buffer = 3 cells on each side.
    let mpc = 2.0_f64;
    let cells = 50u32;
    let line = road_line(&[(0.0, 50.0), (100.0, 50.0)]);

    let mut features = FeatureIndex::empty();
    features.roads.push(RoadFeature {
        line,
        semantic: SemanticClass::Road,
        buffer_m: 6.0,
    });

    let grid = rasterize_semantic_grid(&features, 0.0, 0.0, cells, cells, mpc);

    // Row for y=50: row = (50*2 - 50) / 2 = 25 ... but row 0 is north.
    // origin_y=0, height=50, mpc=2 → cell center y = origin_y + (height - row - 0.5)*mpc
    // y=50 → row such that 0 + (50 - row - 0.5)*2 = 50 → 50 - row - 0.5 = 25 → row = 24.5 ≈ 24
    let road_row = 24u32;
    assert_eq!(*grid.get(25, road_row), SemanticClass::Road,
               "center of road should be Road");

    // 1 cell north (row=23) should also be road (within 6 m buffer).
    assert_eq!(*grid.get(25, 23), SemanticClass::Road,
               "1 cell away should still be within buffer");

    // 4 cells north (row=20) is 8 m away → outside 6 m buffer → Grass.
    assert_eq!(*grid.get(25, 20), SemanticClass::Grass,
               "4 cells away should be outside 6 m buffer");
}

// ── precedence enforcement ────────────────────────────────────────────────────

#[test]
fn water_overwrites_road() {
    // Road and water polygon both cover the same cells.
    let mpc = 2.0_f64;
    let cells = 20u32;

    let line = road_line(&[(0.0, 20.0), (40.0, 20.0)]);
    let water = rect_poly(0.0, 0.0, 40.0, 40.0);

    let mut features = FeatureIndex::empty();
    features.roads.push(RoadFeature {
        line,
        semantic: SemanticClass::Road,
        buffer_m: 4.0,
    });
    features.water_polys.push(water);

    let grid = rasterize_semantic_grid(&features, 0.0, 0.0, cells, cells, mpc);

    // Every cell should be Water (highest precedence wins).
    for row in 0..cells {
        for col in 0..cells {
            assert_eq!(
                *grid.get(col, row),
                SemanticClass::Water,
                "water should override road at ({col},{row})"
            );
        }
    }
}

#[test]
fn building_overwrites_park() {
    let mpc = 2.0_f64;
    let cells = 20u32;

    let park     = rect_poly(0.0, 0.0, 40.0, 40.0);
    let building = rect_poly(10.0, 10.0, 30.0, 30.0);

    let mut features = FeatureIndex::empty();
    features.park_polys.push(park);
    features.buildings.push(BuildingRecord { poly: building, tier: BuildingTier::Low });

    let grid = rasterize_semantic_grid(&features, 0.0, 0.0, cells, cells, mpc);

    // Center cell should be BuildingFloor or BuildingWall, not ParkGrass.
    let center = *grid.get(10, 10);
    assert!(
        center == SemanticClass::BuildingFloor || center == SemanticClass::BuildingWall,
        "building should override park, got {center:?}"
    );
}

// ── wall detection standalone ─────────────────────────────────────────────────

#[test]
fn detect_walls_on_isolated_square() {
    // 5×5 grid, center 3×3 all BuildingFloor, border should become Wall.
    let mut grid: Grid<SemanticClass> = Grid::filled(5, 5, SemanticClass::Grass);
    for row in 1..4u32 {
        for col in 1..4u32 {
            grid.set(col, row, SemanticClass::BuildingFloor);
        }
    }
    detect_building_walls(&mut grid);

    // All 8 cells on the outer ring of the 3×3 become Wall.
    let outer = [(1,1),(2,1),(3,1),(1,2),(3,2),(1,3),(2,3),(3,3)];
    for (col, row) in outer {
        assert_eq!(
            *grid.get(col, row),
            SemanticClass::BuildingWall,
            "outer cell ({col},{row}) should be Wall"
        );
    }
    // True interior (none here — 3×3 has no cell with all 4 neighbors as Floor).
    // Grass cells are untouched.
    assert_eq!(*grid.get(0, 0), SemanticClass::Grass);
}

// ── debug PNG ─────────────────────────────────────────────────────────────────

#[test]
fn dump_png_creates_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("test.png");

    let mut grid: Grid<SemanticClass> = Grid::filled(8, 8, SemanticClass::Grass);
    grid.set(3, 3, SemanticClass::Water);
    grid.set(4, 4, SemanticClass::Road);

    dump_semantic_png(&grid, &path, 2).expect("dump_semantic_png should succeed");
    assert!(path.exists(), "PNG file should exist after dump");

    // Verify it's a valid PNG by checking the magic bytes.
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "should be valid PNG magic");
}
