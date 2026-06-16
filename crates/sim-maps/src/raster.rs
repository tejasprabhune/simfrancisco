/// Per-chunk semantic grid rasterization.
///
/// Paints features onto a `Grid<SemanticClass>` in precedence order
/// (low → high, so higher-priority features overwrite lower ones).
/// Painting order matches the precedence chain:
///   Grass → ParkGrass → Path/Sidewalk → Road → BuildingFloor
///   → [BuildingWall detection] → Water (including coastline)

use geo::algorithm::{bounding_rect::BoundingRect, contains::Contains,
                     euclidean_distance::EuclideanDistance};
use geo::{Coord, LineString, Point, Polygon};

use crate::osm::{BuildingTier, FeatureIndex, RoadFeature};
use crate::types::{Grid, SemanticClass};

// ── public entry point ────────────────────────────────────────────────────────

/// Build the semantic grid for one chunk.
///
/// `origin_x`, `origin_y`: UTM SW corner of the chunk.
/// Row 0 = northernmost; row `height-1` = southernmost.
pub fn rasterize_semantic_grid(
    features: &FeatureIndex,
    origin_x: f64,
    origin_y: f64,
    width: u32,
    height: u32,
    meters_per_cell: f64,
) -> Grid<SemanticClass> {
    let mut grid = Grid::filled(width, height, SemanticClass::Grass);
    let ctx = Ctx { origin_x, origin_y, width, height, mpc: meters_per_cell };

    // 1. Parks (low precedence).
    for poly in &features.park_polys {
        paint_polygon(&mut grid, poly, SemanticClass::ParkGrass, &ctx);
    }

    // 2. Plazas (painted before roads so Road can overwrite at intersections).
    for poly in &features.plaza_polys {
        paint_polygon(&mut grid, poly, SemanticClass::Plaza, &ctx);
    }

    // 3. Roads / paths / sidewalks.
    for road in &features.roads {
        paint_road(&mut grid, road, &ctx);
    }

    // 4. Buildings (floor — walls detected after).
    for bldg in &features.buildings {
        let class = match bldg.tier {
            BuildingTier::Low  => SemanticClass::BuildingFloor,
            BuildingTier::Mid  => SemanticClass::BuildingMid,
            BuildingTier::Tall => SemanticClass::BuildingTall,
        };
        paint_polygon(&mut grid, &bldg.poly, class, &ctx);
    }
    detect_building_walls(&mut grid);

    // 5. Explicit water polygons.
    for poly in &features.water_polys {
        paint_polygon(&mut grid, poly, SemanticClass::Water, &ctx);
    }

    // 6. Coastline water: cells outside the assembled land polygon.
    // Ocean / lake / bay water is determined GLOBALLY by the pipeline's flood-fill
    // pass (flood_fill_water in main.rs), which is robust to OSM's inconsistent
    // coastline-vs-water tagging. The per-chunk coastline polygons are no longer
    // painted here (they were fragile for multi-island and Great-Lakes cities).
    let _ = &features.coastline_land_polys;

    // 7. Shoreline: 1-cell-wide band of transitional ground at the water edge.
    detect_shoreline(&mut grid);

    grid
}

// ── painting primitives ───────────────────────────────────────────────────────

struct Ctx {
    origin_x: f64,
    origin_y: f64,
    width: u32,
    height: u32,
    mpc: f64,
}

impl Ctx {
    fn cell_center(&self, col: u32, row: u32) -> Point<f64> {
        Point::new(
            self.origin_x + (col as f64 + 0.5) * self.mpc,
            self.origin_y + (self.height as f64 - row as f64 - 0.5) * self.mpc,
        )
    }

    /// UTM x → column range (clamped).
    fn col_range(&self, x_min: f64, x_max: f64) -> (u32, u32) {
        let lo = ((x_min - self.origin_x) / self.mpc).floor() as i64;
        let hi = ((x_max - self.origin_x) / self.mpc).ceil()  as i64;
        (lo.max(0) as u32, hi.min(self.width as i64 - 1).max(0) as u32)
    }

    /// UTM y → row range (clamped). Larger y → smaller row.
    fn row_range(&self, y_min: f64, y_max: f64) -> (u32, u32) {
        let top = self.origin_y + self.height as f64 * self.mpc;
        let lo = ((top - y_max) / self.mpc).floor() as i64;
        let hi = ((top - y_min) / self.mpc).ceil()  as i64;
        (lo.max(0) as u32, hi.min(self.height as i64 - 1).max(0) as u32)
    }
}

fn paint_polygon(
    grid: &mut Grid<SemanticClass>,
    poly: &Polygon<f64>,
    class: SemanticClass,
    ctx: &Ctx,
) {
    let rect = match poly.bounding_rect() {
        Some(r) => r,
        None => return,
    };
    let (c0, c1) = ctx.col_range(rect.min().x, rect.max().x);
    let (r0, r1) = ctx.row_range(rect.min().y, rect.max().y);

    for row in r0..=r1 {
        for col in c0..=c1 {
            let pt = ctx.cell_center(col, row);
            if poly.contains(&pt) {
                write_if_higher(grid, col, row, class);
            }
        }
    }
}

fn paint_road(grid: &mut Grid<SemanticClass>, road: &RoadFeature, ctx: &Ctx) {
    let rect = match road.line.bounding_rect() {
        Some(r) => r,
        None => return,
    };
    let buf = road.buffer_m;
    let (c0, c1) = ctx.col_range(rect.min().x - buf, rect.max().x + buf);
    let (r0, r1) = ctx.row_range(rect.min().y - buf, rect.max().y + buf);

    for row in r0..=r1 {
        for col in c0..=c1 {
            let pt = ctx.cell_center(col, row);
            if road.line.euclidean_distance(&pt) <= buf {
                write_if_higher(grid, col, row, road.semantic);
            }
        }
    }
}

/// After all buildings are painted, promote boundary cells
/// (those adjacent to a non-building cell) to `BuildingWall`.
pub fn detect_building_walls(grid: &mut Grid<SemanticClass>) {
    let w = grid.width;
    let h = grid.height;
    let mut walls = Vec::new();

    for row in 0..h {
        for col in 0..w {
            if !matches!(*grid.get(col, row),
                SemanticClass::BuildingFloor | SemanticClass::BuildingMid | SemanticClass::BuildingTall)
            {
                continue;
            }
            let is_boundary = [(0i32, -1i32), (0, 1), (-1, 0), (1, 0)]
                .iter()
                .any(|&(dc, dr)| {
                    let nc = col as i32 + dc;
                    let nr = row as i32 + dr;
                    if nc < 0 || nr < 0 || nc >= w as i32 || nr >= h as i32 {
                        true
                    } else {
                        !matches!(*grid.get(nc as u32, nr as u32),
                            SemanticClass::BuildingFloor | SemanticClass::BuildingMid | SemanticClass::BuildingTall)
                    }
                });
            if is_boundary {
                walls.push((col, row));
            }
        }
    }

    for (col, row) in walls {
        grid.set(col, row, SemanticClass::BuildingWall);
    }
}

/// Mark land cells immediately adjacent to Water as Shoreline.
/// Only affects Grass, ParkGrass, and Sand — roads and buildings are left alone.
fn detect_shoreline(grid: &mut Grid<SemanticClass>) {
    let w = grid.width;
    let h = grid.height;
    let mut to_shore = Vec::new();

    for row in 0..h {
        for col in 0..w {
            if !matches!(
                *grid.get(col, row),
                SemanticClass::Grass | SemanticClass::ParkGrass | SemanticClass::Sand
            ) {
                continue;
            }
            let touches_water = [(0i32, -1i32), (0, 1), (-1, 0), (1, 0)]
                .iter()
                .any(|&(dc, dr)| {
                    let nc = col as i32 + dc;
                    let nr = row as i32 + dr;
                    nc >= 0 && nr >= 0 && nc < w as i32 && nr < h as i32
                        && *grid.get(nc as u32, nr as u32) == SemanticClass::Water
                });
            if touches_water {
                to_shore.push((col, row));
            }
        }
    }

    for (col, row) in to_shore {
        grid.set(col, row, SemanticClass::Shoreline);
    }
}

#[inline]
fn write_if_higher(grid: &mut Grid<SemanticClass>, col: u32, row: u32, class: SemanticClass) {
    let existing = *grid.get(col, row);
    if class.precedence() > existing.precedence() {
        grid.set(col, row, class);
    }
}

// ── coordinate helpers (pub for tests) ───────────────────────────────────────

/// Build a synthetic polygon (axis-aligned rectangle) in UTM coordinates.
pub fn rect_poly(x0: f64, y0: f64, x1: f64, y1: f64) -> Polygon<f64> {
    Polygon::new(
        LineString::new(vec![
            Coord { x: x0, y: y0 },
            Coord { x: x1, y: y0 },
            Coord { x: x1, y: y1 },
            Coord { x: x0, y: y1 },
            Coord { x: x0, y: y0 },
        ]),
        vec![],
    )
}

/// Build a synthetic road line.
pub fn road_line(pts: &[(f64, f64)]) -> LineString<f64> {
    LineString::new(pts.iter().map(|&(x, y)| Coord { x, y }).collect())
}
