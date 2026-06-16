/// Phase 8 tests: rayon parallelisation correctness.
///
/// The parallel chunk loop must produce identical semantic output to
/// the sequential loop for the same inputs. We test this by running
/// the same small synthetic grid through the pipeline helpers in both
/// a sequential loop and a rayon par_iter, comparing results cell-by-cell.

use sim_maps::{
    config::ElevationConfig,
    osm::FeatureIndex,
    pipeline::{semantic_to_collision, semantic_to_render},
    raster::rasterize_semantic_grid,
    topo::apply_topography,
    types::{Grid, SemanticClass, tile_id_class},
    dem::compute_max_rise,
};
use rayon::prelude::*;

fn chunk_render_serial(cx: i32, cy: i32) -> Grid<u32> {
    let cells = 32u32;
    let mpc = 2.0_f64;
    let ox = cx as f64 * 64.0;
    let oy = cy as f64 * 64.0;

    let features = FeatureIndex::empty();
    let mut semantic = rasterize_semantic_grid(&features, ox, oy, cells, cells, mpc);
    let mut elevation = Grid::filled(cells, cells, 0.0_f32);
    let elev_cfg = ElevationConfig { walkable_threshold_m: 1.5, flatten_buildings: false };
    apply_topography(&mut semantic, &mut elevation, &[], ox, oy, mpc, &elev_cfg);
    semantic_to_render(&semantic)
}

fn chunk_render_parallel(coords: &[(i32, i32)]) -> Vec<Grid<u32>> {
    coords.par_iter().map(|&(cx, cy)| {
        chunk_render_serial(cx, cy)
    }).collect()
}

#[test]
fn parallel_matches_serial_for_empty_features() {
    let coords: Vec<(i32, i32)> = (0..4).flat_map(|cy| (0..4).map(move |cx| (cx, cy))).collect();

    let serial: Vec<Grid<u32>> = coords.iter().map(|&(cx, cy)| chunk_render_serial(cx, cy)).collect();
    let parallel = chunk_render_parallel(&coords);

    assert_eq!(serial.len(), parallel.len());
    for (i, (s, p)) in serial.iter().zip(parallel.iter()).enumerate() {
        assert_eq!(s.width,  p.width,  "chunk {i} width mismatch");
        assert_eq!(s.height, p.height, "chunk {i} height mismatch");
        for row in 0..s.height {
            for col in 0..s.width {
                assert_eq!(
                    s.get(col, row), p.get(col, row),
                    "chunk {i} cell ({col},{row}) mismatch"
                );
            }
        }
    }
}

#[test]
fn parallel_chunk_count_matches_grid_size() {
    let nx = 3i32;
    let ny = 4i32;
    let coords: Vec<(i32, i32)> = (0..ny).flat_map(|cy| (0..nx).map(move |cx| (cx, cy))).collect();
    let results = chunk_render_parallel(&coords);
    assert_eq!(results.len(), (nx * ny) as usize);
}

#[test]
fn parallel_all_grass_chunks_are_uniform_grass() {
    let coords: Vec<(i32, i32)> = (0..2).flat_map(|cy| (0..2).map(move |cx| (cx, cy))).collect();
    let results = chunk_render_parallel(&coords);
    for (i, grid) in results.iter().enumerate() {
        for row in 0..grid.height {
            for col in 0..grid.width {
                let class = SemanticClass::from_u8(tile_id_class(*grid.get(col, row)));
                assert_eq!(class, SemanticClass::Grass,
                    "chunk {i} ({col},{row}): expected Grass, got {class:?}");
            }
        }
    }
}

#[test]
fn parallel_slope_collision_consistent() {
    // Verify that the slope-aware collision is stable across rayon calls.
    let sem = Grid::filled(32, 32, SemanticClass::Grass);
    let rise = Grid::filled(32, 32, 0.8_f32); // gentle slope → +1

    let results: Vec<Grid<u8>> = (0..8).into_par_iter().map(|_| {
        semantic_to_collision(&sem, &rise, false, 1.5)
    }).collect();

    let first = &results[0];
    for (i, r) in results.iter().enumerate().skip(1) {
        for row in 0..first.height {
            for col in 0..first.width {
                assert_eq!(
                    first.get(col, row), r.get(col, row),
                    "parallel run {i} collision mismatch at ({col},{row})"
                );
            }
        }
    }
}

#[test]
fn rayon_and_sequential_max_rise_match() {
    let elevations: Vec<Grid<f32>> = (0..8).map(|i| {
        Grid::from_fn(16, 16, |col, row| (col + row + i) as f32 * 0.1)
    }).collect();

    let serial: Vec<Grid<f32>>   = elevations.iter().map(compute_max_rise).collect();
    let parallel: Vec<Grid<f32>> = elevations.par_iter().map(compute_max_rise).collect();

    for (i, (s, p)) in serial.iter().zip(parallel.iter()).enumerate() {
        for row in 0..s.height {
            for col in 0..s.width {
                assert!(
                    (s.get(col, row) - p.get(col, row)).abs() < 1e-6,
                    "max_rise mismatch in grid {i} at ({col},{row})"
                );
            }
        }
    }
}
