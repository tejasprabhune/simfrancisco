/// Phase 2 tests: DEM sampling, gradient, building-elevation flattening.

use sim_maps::dem::{DemReader, average_elevation, compute_max_rise};
use sim_maps::types::Grid;

// ── helpers ──────────────────────────────────────────────────────────────────

/// Flat DEM: every pixel at `elev` meters, 100×100, native CRS = meters.
fn flat_dem(elev: f32) -> DemReader {
    DemReader::synthetic(0.0, 100.0, 1.0, 1.0, 100, 100, move |_, _| elev)
}

/// Ramp DEM: elevation increases linearly with column.
/// elev(col, row) = col as f32 * rise_per_pixel
fn ramp_dem(rise_per_pixel: f32) -> DemReader {
    DemReader::synthetic(0.0, 100.0, 1.0, 1.0, 100, 100, move |col, _row| {
        col as f32 * rise_per_pixel
    })
}

// ── sampling ─────────────────────────────────────────────────────────────────

#[test]
fn flat_dem_sample_center() {
    let dem = flat_dem(42.0);
    // CRS x=50.5, y=49.5 → inside grid
    let elev = dem.sample_crs(50.5, 49.5);
    assert!(elev.is_some(), "expected Some elevation");
    let e = elev.unwrap();
    assert!((e - 42.0).abs() < 1e-4, "flat DEM should return 42.0, got {e}");
}

#[test]
fn out_of_bounds_returns_none() {
    let dem = flat_dem(0.0);
    // x=200 is well outside the 100-wide raster
    assert!(dem.sample_crs(200.0, 50.0).is_none());
    // Negative col
    assert!(dem.sample_crs(-1.0, 50.0).is_none());
}

#[test]
fn bilinear_interpolation_midpoint() {
    // Ramp: elev = col * 2.0
    // At col=5.5 we should get ≈ 11.0 (midpoint between 10.0 and 12.0)
    let dem = ramp_dem(2.0);
    // x_origin=0, pixel_width=1 → col = (x - 0) / 1 = x
    // y_origin=100, pixel_height=1 → row = (100 - y) / 1 = 100 - y
    // Sample at x=5.5, y=50 → col=5.5, row=50
    let elev = dem.sample_crs(5.5, 50.0).expect("in bounds");
    assert!(
        (elev - 11.0).abs() < 0.01,
        "bilinear at col=5.5 should give 11.0, got {elev}"
    );
}

#[test]
fn sample_grid_utm_on_synthetic() {
    // Synthetic DEM with no projector: sample_utm falls through to sample_crs.
    // Flat at 100.0 m, covers 0..100 x 0..100 in native coords.
    let dem = flat_dem(100.0);
    // Sample a 4×4 grid at native CRS scale, cell size = 1.0 m.
    let grid = dem.sample_grid_utm(0.0, 0.0, 4, 4, 1.0);
    for row in 0..4 {
        for col in 0..4 {
            let v = *grid.get(col, row);
            assert!(
                (v - 100.0).abs() < 1e-4,
                "grid cell ({col},{row}) = {v}, expected 100.0"
            );
        }
    }
}

// ── gradient / max-rise ───────────────────────────────────────────────────────

#[test]
fn flat_terrain_has_zero_rise() {
    let mut elev: Grid<f32> = Grid::filled(10, 10, 50.0);
    let rise = compute_max_rise(&elev);
    for row in 0..10 {
        for col in 0..10 {
            assert_eq!(*rise.get(col, row), 0.0, "flat terrain rise must be 0");
        }
    }
    // Silence unused-mut warning.
    let _ = &mut elev;
}

#[test]
fn cliff_edge_detected() {
    // 10×10 grid: left half at 0 m, right half at 5 m.
    // The boundary cells (col=4 and col=5) have a 5 m rise to their neighbor.
    let elev = Grid::from_fn(10, 10, |col, _row| if col < 5 { 0.0f32 } else { 5.0 });
    let rise = compute_max_rise(&elev);

    // col=4 is next to col=5 (5 m higher) → rise = 5.0
    assert_eq!(*rise.get(4, 5), 5.0);
    // col=5 is next to col=4 (5 m lower) → rise = 5.0
    assert_eq!(*rise.get(5, 5), 5.0);
    // Interior of left half → rise = 0.0
    assert_eq!(*rise.get(1, 5), 0.0);
    // Interior of right half → rise = 0.0
    assert_eq!(*rise.get(8, 5), 0.0);

    // Walkable threshold from spec: 1.5 m/cell. Cells at boundary are steep.
    let threshold = 1.5f32;
    assert!(*rise.get(4, 5) > threshold, "boundary should be steep");
    assert!(*rise.get(1, 5) <= threshold, "interior should be flat");
}

#[test]
fn walkable_slope_below_threshold() {
    // 0.5 m rise per cell → below the 1.5 m threshold everywhere.
    let elev = Grid::from_fn(10, 10, |col, _row| col as f32 * 0.5);
    let rise = compute_max_rise(&elev);
    for row in 0..10 {
        for col in 0..10 {
            assert!(
                *rise.get(col, row) <= 1.5,
                "gentle slope should be walkable at ({col},{row})"
            );
        }
    }
}

// ── building footprint flattening ─────────────────────────────────────────────

#[test]
fn average_elevation_flat_region() {
    let elev: Grid<f32> = Grid::filled(20, 20, 30.0);
    let avg = average_elevation(&elev, 5, 5, 10, 10);
    assert!((avg - 30.0).abs() < 1e-4);
}

#[test]
fn average_elevation_mixed_region() {
    // Left half = 0, right half = 10 → average over 0..=9 cols = 5.0
    let elev = Grid::from_fn(10, 10, |col, _| if col < 5 { 0.0f32 } else { 10.0 });
    let avg = average_elevation(&elev, 0, 0, 9, 9);
    assert!((avg - 5.0).abs() < 0.1, "mixed region avg = {avg}, expected 5.0");
}
