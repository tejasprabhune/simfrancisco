/// Phase 7 tests: slope-aware collision costs.

use sim_maps::{
    dem::compute_max_rise,
    pipeline::semantic_to_collision,
    topo::slope_cost_modifier,
    types::{Grid, SemanticClass, collision},
};

// ── slope_cost_modifier unit tests ───────────────────────────────────────────

#[test]
fn flat_terrain_no_modifier() {
    assert_eq!(slope_cost_modifier(0.0,  1.5), 0, "zero rise → +0");
    assert_eq!(slope_cost_modifier(0.49, 1.5), 0, "below threshold/3 → +0");
}

#[test]
fn gentle_slope_adds_one() {
    // threshold/3 = 0.5, threshold*2/3 = 1.0
    assert_eq!(slope_cost_modifier(0.51, 1.5), 1, "just above threshold/3 → +1");
    assert_eq!(slope_cost_modifier(0.9,  1.5), 1, "mid gentle slope → +1");
    assert_eq!(slope_cost_modifier(0.99, 1.5), 1, "just below threshold*2/3 → +1");
}

#[test]
fn steep_slope_adds_two() {
    // threshold*2/3 = 1.0, threshold = 1.5
    assert_eq!(slope_cost_modifier(1.0,  1.5), 2, "at threshold*2/3 → +2");
    assert_eq!(slope_cost_modifier(1.2,  1.5), 2, "mid steep slope → +2");
    assert_eq!(slope_cost_modifier(1.49, 1.5), 2, "just below threshold → +2");
}

#[test]
fn at_or_above_threshold_no_modifier() {
    assert_eq!(slope_cost_modifier(1.5, 1.5), 0, "at threshold → already cliff/stairs");
    assert_eq!(slope_cost_modifier(3.0, 1.5), 0, "well above threshold → +0");
}

#[test]
fn zero_threshold_does_not_panic() {
    let _ = slope_cost_modifier(0.0, 0.0);
    let _ = slope_cost_modifier(1.0, 0.0);
}

#[test]
fn modifier_scales_with_threshold() {
    let t = 3.0_f32;
    // gentle band: (t/3, t*2/3) = (1.0, 2.0) → +1
    assert_eq!(slope_cost_modifier(t * 0.4, t), 1, "40% of threshold is in gentle band");
    // steep band: [t*2/3, t) = [2.0, 3.0) → +2
    assert_eq!(slope_cost_modifier(t * 0.8, t), 2, "80% of threshold is in steep band");
    // above threshold → +0
    assert_eq!(slope_cost_modifier(t * 1.2, t), 0, "above threshold → cliff/stairs, no modifier");
}

// ── slope modifier integration: collision layer ───────────────────────────────

fn coll(class: SemanticClass, rise_val: f32) -> u8 {
    let sem  = Grid::filled(1, 1, class);
    let rise = Grid::filled(1, 1, rise_val);
    *semantic_to_collision(&sem, &rise, false, 1.5).get(0, 0)
}

#[test]
fn grass_on_flat_has_base_cost() {
    assert_eq!(coll(SemanticClass::Grass, 0.0), collision::GRASS_COST);
}

#[test]
fn grass_on_gentle_slope_adds_one() {
    assert_eq!(coll(SemanticClass::Grass, 0.6), collision::GRASS_COST + 1);
}

#[test]
fn grass_on_steep_slope_adds_two() {
    assert_eq!(coll(SemanticClass::Grass, 1.2), collision::GRASS_COST + 2);
}

#[test]
fn blocked_cells_unchanged_by_slope() {
    assert_eq!(coll(SemanticClass::Water, 1.4), collision::BLOCKED);
}

#[test]
fn road_cost_flat_zero() {
    assert_eq!(coll(SemanticClass::Road, 0.0), collision::ROAD_COST);
}

#[test]
fn road_on_steep_slope_gets_modifier() {
    assert_eq!(coll(SemanticClass::Road, 1.2), collision::ROAD_COST + 2);
}

#[test]
fn cost_never_reaches_blocked_from_modifier() {
    assert!(coll(SemanticClass::Stairs, 1.2) < collision::BLOCKED);
}

#[test]
fn building_floor_blocked_ignores_slope() {
    let sem  = Grid::filled(1, 1, SemanticClass::BuildingFloor);
    let rise = Grid::filled(1, 1, 1.2_f32);
    let c = *semantic_to_collision(&sem, &rise, true, 1.5).get(0, 0);
    assert_eq!(c, collision::BLOCKED, "building_blocked=true → BLOCKED regardless of slope");
}

// ── compute_max_rise integration ──────────────────────────────────────────────

#[test]
fn flat_elevation_zero_rise() {
    let elev = Grid::filled(4, 4, 5.0_f32);
    let rise = compute_max_rise(&elev);
    for row in 0..4 {
        for col in 0..4 {
            assert_eq!(*rise.get(col, row), 0.0, "flat grid → zero rise everywhere");
        }
    }
}

#[test]
fn step_edge_has_expected_rise() {
    // 4×1 grid: [0, 0, 10, 10] — cols 1 and 2 border the 10m step.
    let mut elev = Grid::filled(4, 1, 0.0_f32);
    elev.set(2, 0, 10.0);
    elev.set(3, 0, 10.0);
    let rise = compute_max_rise(&elev);
    assert_eq!(*rise.get(1, 0), 10.0, "col 1 neighbors col 2 with rise 10");
    assert_eq!(*rise.get(2, 0), 10.0, "col 2 neighbors col 1 with rise 10");
    assert_eq!(*rise.get(0, 0),  0.0, "col 0 isolated from step");
    assert_eq!(*rise.get(3, 0),  0.0, "col 3 uniform with col 2");
}

#[test]
fn slope_cost_applied_through_rise_grid() {
    // 3×1 elevation: [0, 1.2, 1.2] — col 0 borders a 1.2m rise.
    let mut elev = Grid::filled(3, 1, 1.2_f32);
    elev.set(0, 0, 0.0);
    let rise = compute_max_rise(&elev);
    let sem = Grid::filled(3, 1, SemanticClass::Grass);
    let coll_grid = semantic_to_collision(&sem, &rise, false, 1.5);

    // col 0: rise = 1.2 → modifier +2; col 1: rise = 1.2 → +2; col 2: rise = 0 → +0
    assert_eq!(*coll_grid.get(0, 0), collision::GRASS_COST + 2);
    assert_eq!(*coll_grid.get(1, 0), collision::GRASS_COST + 2);
    assert_eq!(*coll_grid.get(2, 0), collision::GRASS_COST);
}
