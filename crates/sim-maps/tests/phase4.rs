/// Phase 4 tests: topography rules — cliff/stairs classification, building
/// flattening, and water/building immunity.

use sim_maps::{
    dem::compute_max_rise,
    osm::{BuildingRecord, BuildingTier, FeatureIndex},
    raster::{rect_poly, rasterize_semantic_grid},
    topo::{apply_topography, elevation_bands},
    types::{Grid, SemanticClass},
    config::ElevationConfig,
};

fn default_elev_cfg() -> ElevationConfig {
    ElevationConfig { walkable_threshold_m: 1.5, flatten_buildings: true }
}

// ── cliff classification ──────────────────────────────────────────────────────

#[test]
fn steep_grass_becomes_cliff() {
    // 5×5 flat elevation grid with a 5 m step between col 2 and col 3.
    let mut semantic: Grid<SemanticClass> = Grid::filled(5, 5, SemanticClass::Grass);
    let mut elevation: Grid<f32> = Grid::from_fn(5, 5, |col, _row| {
        if col < 3 { 0.0 } else { 5.0 }
    });

    let features = FeatureIndex::empty();
    apply_topography(&mut semantic, &mut elevation, &[],
                     0.0, 0.0, 2.0, &default_elev_cfg());

    // Cells at the step boundary (col 2 and 3) have max-rise of 5 m → CliffFace.
    assert_eq!(*semantic.get(2, 2), SemanticClass::CliffFace);
    assert_eq!(*semantic.get(3, 2), SemanticClass::CliffFace);

    // Cells far from the step stay Grass.
    assert_eq!(*semantic.get(0, 0), SemanticClass::Grass);
    assert_eq!(*semantic.get(4, 4), SemanticClass::Grass);
}

#[test]
fn gentle_slope_stays_grass() {
    // Max rise = 1.0 m per cell → below 1.5 m threshold.
    let mut semantic: Grid<SemanticClass> = Grid::filled(5, 5, SemanticClass::Grass);
    let mut elevation: Grid<f32> = Grid::from_fn(5, 5, |col, _row| col as f32 * 1.0);

    let features = FeatureIndex::empty();
    apply_topography(&mut semantic, &mut elevation, &[],
                     0.0, 0.0, 2.0, &default_elev_cfg());

    for row in 0..5u32 {
        for col in 0..5u32 {
            assert_eq!(*semantic.get(col, row), SemanticClass::Grass,
                       "gentle slope should stay Grass at ({col},{row})");
        }
    }
}

// ── stairs classification ─────────────────────────────────────────────────────

#[test]
fn steep_road_becomes_stairs() {
    let mut semantic: Grid<SemanticClass> = Grid::filled(5, 5, SemanticClass::Road);
    let mut elevation: Grid<f32> = Grid::from_fn(5, 5, |col, _row| {
        if col < 3 { 0.0 } else { 5.0 }
    });

    let features = FeatureIndex::empty();
    apply_topography(&mut semantic, &mut elevation, &[],
                     0.0, 0.0, 2.0, &default_elev_cfg());

    assert_eq!(*semantic.get(2, 2), SemanticClass::Stairs,
               "steep road should become Stairs");
    assert_eq!(*semantic.get(3, 2), SemanticClass::Stairs,
               "steep road should become Stairs");
}

#[test]
fn steep_path_becomes_stairs() {
    let mut semantic: Grid<SemanticClass> = Grid::filled(3, 3, SemanticClass::Path);
    let mut elevation: Grid<f32> = Grid::from_fn(3, 3, |col, _row| {
        if col == 0 { 0.0 } else { 5.0 }
    });

    let features = FeatureIndex::empty();
    apply_topography(&mut semantic, &mut elevation, &[],
                     0.0, 0.0, 2.0, &default_elev_cfg());

    assert_eq!(*semantic.get(0, 1), SemanticClass::Stairs);
}

// ── water and building immunity ───────────────────────────────────────────────

#[test]
fn water_is_immune_to_cliff() {
    let mut semantic: Grid<SemanticClass> = Grid::filled(5, 5, SemanticClass::Water);
    let mut elevation: Grid<f32> = Grid::from_fn(5, 5, |col, _row| {
        if col < 3 { 0.0 } else { 10.0 }
    });

    let features = FeatureIndex::empty();
    apply_topography(&mut semantic, &mut elevation, &[],
                     0.0, 0.0, 2.0, &default_elev_cfg());

    for row in 0..5u32 {
        for col in 0..5u32 {
            assert_eq!(*semantic.get(col, row), SemanticClass::Water,
                       "Water cells must not be reclassified");
        }
    }
}

#[test]
fn building_floor_is_immune_to_cliff() {
    let mut semantic: Grid<SemanticClass> = Grid::filled(5, 5, SemanticClass::BuildingFloor);
    let mut elevation: Grid<f32> = Grid::from_fn(5, 5, |col, _row| {
        if col < 3 { 0.0 } else { 10.0 }
    });

    let features = FeatureIndex::empty();
    apply_topography(&mut semantic, &mut elevation, &[],
                     0.0, 0.0, 2.0, &default_elev_cfg());

    for row in 0..5u32 {
        for col in 0..5u32 {
            assert_eq!(*semantic.get(col, row), SemanticClass::BuildingFloor,
                       "BuildingFloor must not be reclassified");
        }
    }
}

// ── building flattening ───────────────────────────────────────────────────────

#[test]
fn building_footprint_is_flattened() {
    // 10×10 chunk, 2 m/cell → 20 m × 20 m area.
    // Elevation increases linearly: col * 2.0 m.
    // Building covers x=4..16, y=4..16 (cells col 2..8, approx).
    let cells = 10u32;
    let mpc = 2.0_f64;

    let building = rect_poly(4.0, 4.0, 16.0, 16.0);

    let mut features = FeatureIndex::empty();
    features.buildings.push(BuildingRecord { poly: building.clone(), tier: BuildingTier::Low });

    let mut semantic = rasterize_semantic_grid(&features, 0.0, 0.0, cells, cells, mpc);

    let mut elevation: Grid<f32> = Grid::from_fn(cells, cells, |col, _row| col as f32 * 2.0);
    let building_polys = vec![building.clone()];

    apply_topography(&mut semantic, &mut elevation, &building_polys,
                     0.0, 0.0, mpc, &default_elev_cfg());

    // Collect all cells inside the building polygon.
    let mut inside_elevs: Vec<f32> = Vec::new();
    for row in 0..cells {
        for col in 0..cells {
            let x = 0.0 + (col as f64 + 0.5) * mpc;
            let y = 0.0 + (cells as f64 - row as f64 - 0.5) * mpc;
            let pt = geo::Point::new(x, y);
            if geo::algorithm::contains::Contains::contains(&building, &pt) {
                inside_elevs.push(*elevation.get(col, row));
            }
        }
    }

    assert!(!inside_elevs.is_empty(), "building footprint should contain cells");

    // All cells inside must share the same elevation (the average).
    let first = inside_elevs[0];
    for &e in &inside_elevs {
        assert!(
            (e - first).abs() < 0.01,
            "all building cells should share the flattened elevation, got {e} vs {first}"
        );
    }
}

// ── elevation_bands ───────────────────────────────────────────────────────────

#[test]
fn elevation_bands_min_is_zero() {
    let elevation: Grid<f32> = Grid::filled(4, 4, 0.0);
    let bands = elevation_bands(&elevation, 0.0, 100.0);
    for row in 0..4u32 {
        for col in 0..4u32 {
            assert_eq!(*bands.get(col, row), 0, "flat zero elevation → band 0");
        }
    }
}

#[test]
fn elevation_bands_max_is_seven() {
    let elevation: Grid<f32> = Grid::filled(4, 4, 100.0);
    let bands = elevation_bands(&elevation, 0.0, 100.0);
    for row in 0..4u32 {
        for col in 0..4u32 {
            assert_eq!(*bands.get(col, row), 7, "max elevation → band 7");
        }
    }
}

#[test]
fn compute_max_rise_detects_step() {
    // 1×5 grid: values 0,0,0,5,5 → rise at col 2 and 3 should be 5.
    let elev: Grid<f32> = Grid::from_fn(5, 1, |col, _row| {
        if col < 3 { 0.0 } else { 5.0 }
    });
    let rise = compute_max_rise(&elev);
    assert!(*rise.get(2, 0) >= 5.0, "col 2 has a 5 m rise to its east neighbor");
    assert!(*rise.get(3, 0) >= 5.0, "col 3 has a 5 m rise to its west neighbor");
    assert_eq!(*rise.get(0, 0), 0.0, "col 0 has no rise");
}
