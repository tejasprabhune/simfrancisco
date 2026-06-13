//! Geography: the `tiles.db` map, coordinate transforms, and home/work placement.
//!
//! Grid: 67×60 chunks, 125 cells/chunk, 2 m/cell (LOD 0). Global cell grid is
//! (67·125)=8375 wide × (60·125)=7500 tall. Cell (0,0) is the NW corner, anchored
//! at UTM (min_x, max_y); +gx is east, +gy is south. CRS is UTM Zone 10N (EPSG:32610).
//!
//! Coordinate contract (documented in INTEGRATION.md):
//!   utm_x = min_x + (gx + 0.5)·m_per_cell
//!   utm_y = max_y − (gy + 0.5)·m_per_cell
//! UTM→lat/lng uses the standard inverse transverse-Mercator series for zone 10N.

use anyhow::{Context, Result};
use rand::Rng;
use rusqlite::Connection;
use std::collections::HashMap;
use std::sync::Mutex;

pub const CELLS_PER_CHUNK: i64 = 125;
pub const M_PER_CELL: f64 = 2.0;

#[derive(Clone, Debug, serde::Serialize)]
pub struct Manifest {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
    pub west: f64,
    pub east: f64,
    pub south: f64,
    pub north: f64,
    pub cells_per_chunk: i64,
    pub meters_per_cell: f64,
    pub chunks_x: i64,
    pub chunks_y: i64,
    pub cells_x: i64,
    pub cells_y: i64,
    pub crs: String,
}

/// A global cell coordinate on the LOD-0 grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Cell {
    pub x: i64,
    pub y: i64,
}

impl Cell {
    pub fn new(x: i64, y: i64) -> Self {
        Cell { x, y }
    }
}

pub struct TilesDb {
    conn: Mutex<Connection>,
    pub manifest: Manifest,
    /// cache of decompressed LOD-0 collision grids keyed by (cx,cy).
    cache: Mutex<HashMap<(i64, i64), Vec<u8>>>,
}

impl TilesDb {
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("open tiles db {path}"))?;
        let manifest_json: String = conn
            .query_row("SELECT value FROM meta WHERE key='manifest'", [], |r| r.get(0))
            .context("read manifest from tiles.db")?;
        let m: serde_json::Value = serde_json::from_str(&manifest_json)?;
        let bbox = &m["bbox_utm"];
        let wgs = &m["bbox_wgs84"];
        let cells_per_chunk = m["cells_per_chunk"].as_i64().unwrap_or(125);
        let meters_per_cell = m["meters_per_cell"].as_f64().unwrap_or(2.0);
        // grid extent from chunk table
        let (max_cx, max_cy): (i64, i64) = conn.query_row(
            "SELECT MAX(cx), MAX(cy) FROM chunks WHERE lod=0",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let chunks_x = max_cx + 1;
        let chunks_y = max_cy + 1;
        let manifest = Manifest {
            min_x: bbox["min_x"].as_f64().unwrap(),
            min_y: bbox["min_y"].as_f64().unwrap(),
            max_x: bbox["max_x"].as_f64().unwrap(),
            max_y: bbox["max_y"].as_f64().unwrap(),
            west: wgs["west"].as_f64().unwrap_or(-122.5247),
            east: wgs["east"].as_f64().unwrap_or(-122.3366),
            south: wgs["south"].as_f64().unwrap_or(37.6983),
            north: wgs["north"].as_f64().unwrap_or(37.8312),
            cells_per_chunk,
            meters_per_cell,
            chunks_x,
            chunks_y,
            cells_x: chunks_x * cells_per_chunk,
            cells_y: chunks_y * cells_per_chunk,
            crs: m["crs"].as_str().unwrap_or("EPSG:32610").to_string(),
        };
        Ok(TilesDb {
            conn: Mutex::new(conn),
            manifest,
            cache: Mutex::new(HashMap::new()),
        })
    }

    pub fn in_bounds(&self, c: Cell) -> bool {
        c.x >= 0 && c.y >= 0 && c.x < self.manifest.cells_x && c.y < self.manifest.cells_y
    }

    /// Decompressed LOD-0 collision grid for a chunk (row-major, 125×125). Cached.
    fn chunk_grid(&self, cx: i64, cy: i64) -> Option<Vec<u8>> {
        if let Some(g) = self.cache.lock().unwrap().get(&(cx, cy)) {
            return Some(g.clone());
        }
        let blob: Option<Vec<u8>> = {
            let conn = self.conn.lock().unwrap();
            conn.query_row(
                "SELECT collision FROM chunks WHERE cx=?1 AND cy=?2 AND lod=0",
                [cx, cy],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .ok()
        };
        let blob = blob?;
        let cpc = self.manifest.cells_per_chunk as usize;
        let raw = zstd::stream::decode_all(&blob[..]).ok()?;
        if raw.len() != cpc * cpc {
            return None;
        }
        self.cache.lock().unwrap().insert((cx, cy), raw.clone());
        Some(raw)
    }

    /// Terrain cost at a cell: 0 free … 8 stairs, 255 blocked. Out-of-bounds = 255.
    pub fn cost(&self, c: Cell) -> u8 {
        if !self.in_bounds(c) {
            return 255;
        }
        let cpc = self.manifest.cells_per_chunk;
        let (cx, cy) = (c.x / cpc, c.y / cpc);
        let (lx, ly) = (c.x % cpc, c.y % cpc);
        match self.chunk_grid(cx, cy) {
            Some(g) => g[(ly * cpc + lx) as usize],
            None => 255,
        }
    }

    pub fn walkable(&self, c: Cell) -> bool {
        self.cost(c) < 255
    }

    /// Movement cost to enter a cell: 1 + terrain penalty (blocked = None).
    pub fn step_cost(&self, c: Cell) -> Option<u32> {
        let v = self.cost(c);
        if v >= 255 {
            None
        } else {
            Some(1 + v as u32)
        }
    }

    // ---- coordinate transforms ----

    pub fn cell_to_utm(&self, c: Cell) -> (f64, f64) {
        let m = &self.manifest;
        let x = m.min_x + (c.x as f64 + 0.5) * m.meters_per_cell;
        let y = m.max_y - (c.y as f64 + 0.5) * m.meters_per_cell;
        (x, y)
    }

    pub fn utm_to_cell(&self, x: f64, y: f64) -> Cell {
        let m = &self.manifest;
        let gx = ((x - m.min_x) / m.meters_per_cell - 0.5).round() as i64;
        let gy = ((m.max_y - y) / m.meters_per_cell - 0.5).round() as i64;
        Cell::new(gx.clamp(0, m.cells_x - 1), gy.clamp(0, m.cells_y - 1))
    }

    pub fn cell_to_lonlat(&self, c: Cell) -> (f64, f64) {
        let (x, y) = self.cell_to_utm(c);
        utm10n_to_lonlat(x, y)
    }

    /// Sample a walkable cell near a PUMA's approximate grid centroid. Deterministic
    /// given `rng`. Falls back to a city-center scan if rejection sampling fails.
    pub fn sample_residential_cell(&self, puma: u32, rng: &mut impl Rng) -> Cell {
        let (ccx, ccy, radius) = puma_centroid_chunks(puma);
        let cpc = self.manifest.cells_per_chunk;
        for _ in 0..400 {
            let dcx = rng.gen_range(-radius..=radius);
            let dcy = rng.gen_range(-radius..=radius);
            let cx = (ccx + dcx).clamp(0, self.manifest.chunks_x - 1);
            let cy = (ccy + dcy).clamp(0, self.manifest.chunks_y - 1);
            let lx = rng.gen_range(0..cpc);
            let ly = rng.gen_range(0..cpc);
            let cell = Cell::new(cx * cpc + lx, cy * cpc + ly);
            // prefer low-penalty walkable cells (roads/sidewalks/plazas)
            if let Some(sc) = self.step_cost(cell) {
                if sc <= 3 {
                    return cell;
                }
            }
        }
        // fallback: spiral out from city center
        self.nearest_walkable(Cell::new(
            (ccx * cpc + cpc / 2).clamp(0, self.manifest.cells_x - 1),
            (ccy * cpc + cpc / 2).clamp(0, self.manifest.cells_y - 1),
        ))
        .unwrap_or(Cell::new(self.manifest.cells_x / 2, self.manifest.cells_y / 2))
    }

    pub fn nearest_walkable(&self, c: Cell) -> Option<Cell> {
        if self.walkable(c) {
            return Some(c);
        }
        for r in 1..200i64 {
            for dy in -r..=r {
                for dx in -r..=r {
                    if dx.abs() != r && dy.abs() != r {
                        continue;
                    }
                    let n = Cell::new(c.x + dx, c.y + dy);
                    if self.walkable(n) {
                        return Some(n);
                    }
                }
            }
        }
        None
    }
}

/// Approximate (chunk_cx, chunk_cy, radius_chunks) per SF PUMA, from SF geography.
/// Grid: +x east, +y south, 67×60 chunks. Light placement only (BRIEF §3.1).
pub fn puma_centroid_chunks(puma: u32) -> (i64, i64, i64) {
    match puma {
        7507 => (48, 44, 8), // Bayview & Hunters Point (SE)
        7508 => (14, 18, 8), // Richmond, Presidio (NW)
        7509 => (44, 14, 6), // Chinatown, North Beach, Russian Hill (NE)
        7510 => (40, 28, 7), // SoMa & Mission (E-central)
        7511 => (34, 38, 7), // Central & Bernal Heights
        7512 => (16, 38, 9), // Outer & Inner Sunset (SW)
        7513 => (28, 48, 8), // Ingleside & South-central (S)
        7514 => (30, 15, 6), // Western Addition & Marina (N-central)
        _ => (33, 30, 12),   // city center fallback
    }
}

/// Inverse transverse-Mercator (UTM) → (lon, lat) in degrees. Zone 10N, WGS84.
/// Snyder series; accurate to well under a meter across the SF bbox.
pub fn utm10n_to_lonlat(easting: f64, northing: f64) -> (f64, f64) {
    let a = 6_378_137.0_f64; // WGS84 semi-major
    let f = 1.0_f64 / 298.257_223_563;
    let k0 = 0.9996_f64;
    let e2 = f * (2.0 - f);
    let ep2 = e2 / (1.0 - e2);
    let zone = 10.0_f64;
    let lon0 = ((zone - 1.0) * 6.0 - 180.0 + 3.0).to_radians();

    let x = easting - 500_000.0;
    let y = northing; // northern hemisphere

    let m = y / k0;
    let e1 = (1.0 - (1.0 - e2).sqrt()) / (1.0 + (1.0 - e2).sqrt());
    let mu = m / (a * (1.0 - e2 / 4.0 - 3.0 * e2 * e2 / 64.0 - 5.0 * e2 * e2 * e2 / 256.0));
    let phi1 = mu
        + (3.0 * e1 / 2.0 - 27.0 * e1.powi(3) / 32.0) * (2.0 * mu).sin()
        + (21.0 * e1 * e1 / 16.0 - 55.0 * e1.powi(4) / 32.0) * (4.0 * mu).sin()
        + (151.0 * e1.powi(3) / 96.0) * (6.0 * mu).sin()
        + (1097.0 * e1.powi(4) / 512.0) * (8.0 * mu).sin();

    let sin_phi1 = phi1.sin();
    let cos_phi1 = phi1.cos();
    let tan_phi1 = phi1.tan();
    let n1 = a / (1.0 - e2 * sin_phi1 * sin_phi1).sqrt();
    let t1 = tan_phi1 * tan_phi1;
    let c1 = ep2 * cos_phi1 * cos_phi1;
    let r1 = a * (1.0 - e2) / (1.0 - e2 * sin_phi1 * sin_phi1).powf(1.5);
    let d = x / (n1 * k0);

    let lat = phi1
        - (n1 * tan_phi1 / r1)
            * (d * d / 2.0
                - (5.0 + 3.0 * t1 + 10.0 * c1 - 4.0 * c1 * c1 - 9.0 * ep2) * d.powi(4) / 24.0
                + (61.0 + 90.0 * t1 + 298.0 * c1 + 45.0 * t1 * t1 - 252.0 * ep2 - 3.0 * c1 * c1)
                    * d.powi(6)
                    / 720.0);
    let lon = lon0
        + (d - (1.0 + 2.0 * t1 + c1) * d.powi(3) / 6.0
            + (5.0 - 2.0 * c1 + 28.0 * t1 - 3.0 * c1 * c1 + 8.0 * ep2 + 24.0 * t1 * t1) * d.powi(5)
                / 120.0)
            / cos_phi1;

    (lon.to_degrees(), lat.to_degrees())
}

pub fn lonlat_to_utm10n(lon_deg: f64, lat_deg: f64) -> (f64, f64) {
    let a = 6_378_137.0_f64;
    let f = 1.0_f64 / 298.257_223_563;
    let k0 = 0.9996_f64;
    let e2 = f * (2.0 - f);
    let ep2 = e2 / (1.0 - e2);
    let zone = 10.0_f64;
    let lon0 = ((zone - 1.0) * 6.0 - 180.0 + 3.0).to_radians();
    let phi = lat_deg.to_radians();
    let lam = lon_deg.to_radians();
    let n = a / (1.0 - e2 * phi.sin().powi(2)).sqrt();
    let t = phi.tan().powi(2);
    let c = ep2 * phi.cos().powi(2);
    let aa = phi.cos() * (lam - lon0);
    let m = a
        * ((1.0 - e2 / 4.0 - 3.0 * e2 * e2 / 64.0 - 5.0 * e2.powi(3) / 256.0) * phi
            - (3.0 * e2 / 8.0 + 3.0 * e2 * e2 / 32.0 + 45.0 * e2.powi(3) / 1024.0) * (2.0 * phi).sin()
            + (15.0 * e2 * e2 / 256.0 + 45.0 * e2.powi(3) / 1024.0) * (4.0 * phi).sin()
            - (35.0 * e2.powi(3) / 3072.0) * (6.0 * phi).sin());
    let easting = k0
        * n
        * (aa + (1.0 - t + c) * aa.powi(3) / 6.0
            + (5.0 - 18.0 * t + t * t + 72.0 * c - 58.0 * ep2) * aa.powi(5) / 120.0)
        + 500_000.0;
    let northing = k0
        * (m + n * phi.tan()
            * (aa * aa / 2.0
                + (5.0 - t + 9.0 * c + 4.0 * c * c) * aa.powi(4) / 24.0
                + (61.0 - 58.0 * t + t * t + 600.0 * c - 330.0 * ep2) * aa.powi(6) / 720.0));
    (easting, northing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utm_roundtrip_sf() {
        // SF City Hall ≈ (-122.4194, 37.7793)
        let (e, n) = lonlat_to_utm10n(-122.4194, 37.7793);
        let (lon, lat) = utm10n_to_lonlat(e, n);
        assert!((lon - -122.4194).abs() < 1e-6, "lon {lon}");
        assert!((lat - 37.7793).abs() < 1e-6, "lat {lat}");
    }

    #[test]
    fn utm_corner_matches_bbox() {
        // bbox_utm min_x,max_y should map near (west, north) of bbox_wgs84.
        let (lon, lat) = utm10n_to_lonlat(541825.66, 4187293.87);
        assert!((lon - -122.5247).abs() < 0.01, "lon {lon}");
        assert!((lat - 37.8312).abs() < 0.01, "lat {lat}");
    }
}
