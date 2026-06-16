/// Shared pipeline helpers used by main.rs and integration tests.

use crate::{dem, topo, autotile, types};
use types::{Grid, SemanticClass, collision, make_tile_id};

/// Convert a semantic grid to the autotiled render layer.
/// Tile ID bits 0-7 = SemanticClass, bits 8-15 = blob autotile variant (0-46).
pub fn semantic_to_render(semantic: &Grid<SemanticClass>) -> Grid<u32> {
    let variants = autotile::autotile_grid(semantic);
    Grid::from_fn(semantic.width, semantic.height, |col, row| {
        make_tile_id(*semantic.get(col, row), *variants.get(col, row))
    })
}

/// Derive the collision layer from the semantic grid.
/// Phase 7: walkable cells on sloped terrain receive a cost modifier
/// proportional to the local gradient relative to the walkable threshold.
pub fn semantic_to_collision(
    semantic: &Grid<SemanticClass>,
    rise: &Grid<f32>,
    building_blocked: bool,
    walkable_threshold: f32,
) -> Grid<u8> {
    Grid::from_fn(semantic.width, semantic.height, |col, row| {
        let base = match semantic.get(col, row) {
            SemanticClass::Water
            | SemanticClass::BuildingWall
            | SemanticClass::CliffFace    => collision::BLOCKED,
            SemanticClass::BuildingFloor
            | SemanticClass::BuildingMid
            | SemanticClass::BuildingTall =>
                if building_blocked { collision::BLOCKED } else { collision::WALKABLE },
            SemanticClass::Stairs         => collision::STAIRS_COST,
            SemanticClass::Road
            | SemanticClass::Sidewalk
            | SemanticClass::Plaza        => collision::ROAD_COST,
            SemanticClass::Path           => collision::PATH_COST,
            SemanticClass::ParkGrass
            | SemanticClass::Shoreline    => collision::PARK_COST,
            _                             => collision::GRASS_COST,
        };
        if base >= collision::BLOCKED {
            base
        } else {
            let m = topo::slope_cost_modifier(*rise.get(col, row), walkable_threshold);
            base.saturating_add(m).min(collision::BLOCKED - 1)
        }
    })
}

/// Compute collision with a zero-rise grid (convenience for callers without elevation).
pub fn semantic_to_collision_flat(
    semantic: &Grid<SemanticClass>,
    building_blocked: bool,
) -> Grid<u8> {
    let rise = Grid::filled(semantic.width, semantic.height, 0.0_f32);
    semantic_to_collision(semantic, &rise, building_blocked, 1.5)
}

/// Full LOD-0 chunk processing: topography + render + collision.
///
/// Returns `(render, collision)` for LOD 0.
pub fn process_chunk_lod0(
    semantic: &mut Grid<SemanticClass>,
    elevation: &mut Grid<f32>,
    building_polys: &[geo::Polygon<f64>],
    origin_x: f64,
    origin_y: f64,
    mpc: f64,
    elev_cfg: &crate::config::ElevationConfig,
    building_blocked: bool,
) -> (Grid<u32>, Grid<u8>) {
    topo::apply_topography(semantic, elevation, building_polys, origin_x, origin_y, mpc, elev_cfg);
    let rise = dem::compute_max_rise(elevation);
    let render = semantic_to_render(semantic);
    let collision = semantic_to_collision(
        semantic, &rise, building_blocked, elev_cfg.walkable_threshold_m as f32,
    );
    (render, collision)
}
