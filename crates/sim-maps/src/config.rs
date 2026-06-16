use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Top-level pipeline configuration, loaded from `pipeline.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub pipeline: PipelineConfig,
    pub bbox_wgs84: BboxWgs84,
    pub paths: PathsConfig,
    pub elevation: ElevationConfig,
    pub collision: CollisionConfig,
    pub atlas: AtlasConfig,
    /// Target projected CRS (UTM zone). Defaults to SF's EPSG:32610 so existing
    /// configs without a `[crs]` section keep working unchanged.
    #[serde(default)]
    pub crs: CrsConfig,
    /// A point known to be on land inside the bbox (WGS-84), used to select the
    /// correct landmass when closing coastline polygons. Defaults to bbox centre.
    #[serde(default)]
    pub land_ref: Option<LandRefWgs84>,
    /// Multiple on-land points (one per landmass) for multi-island cities like NYC
    /// (Manhattan + Brooklyn + the Bronx) or Miami (mainland + Miami Beach).
    #[serde(default)]
    pub land_refs: Vec<LandRefWgs84>,
    /// Optional DEM sanity-check landmarks (WGS-84); logged only, no output effect.
    #[serde(default)]
    pub landmarks: Vec<Landmark>,
    /// Iconic chunks rendered by the `verify` binary.
    #[serde(default)]
    pub verify: VerifyConfig,
}

fn default_utm_epsg() -> String {
    crate::crs::EPSG_UTM10N.to_string()
}

/// Projected CRS for the city (UTM zone). SF=32610, NYC=32618, LA=32611,
/// Chicago=32616, Miami=32617.
#[derive(Debug, Clone, Deserialize)]
pub struct CrsConfig {
    #[serde(default = "default_utm_epsg")]
    pub utm_epsg: String,
}

impl Default for CrsConfig {
    fn default() -> Self {
        Self { utm_epsg: default_utm_epsg() }
    }
}

/// A reference point in WGS-84 (lon/lat degrees).
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct LandRefWgs84 {
    pub lon: f64,
    pub lat: f64,
}

/// DEM spot-check landmark (WGS-84). Affects log output only.
#[derive(Debug, Clone, Deserialize)]
pub struct Landmark {
    pub name: String,
    pub lon: f64,
    pub lat: f64,
    #[serde(default)]
    pub expected_m: f64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct VerifyConfig {
    #[serde(default)]
    pub test_chunks: Vec<TestChunk>,
}

/// An iconic chunk to render for visual verification.
#[derive(Debug, Clone, Deserialize)]
pub struct TestChunk {
    pub cx: i32,
    pub cy: i32,
    pub label: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PipelineConfig {
    pub meters_per_cell: f64,
    pub chunk_meters: f64,
    pub lod_levels: u32,
    /// Optional elevation ceiling (metres) for the water flood-fill. When set,
    /// the flood only spreads through cells at or below this height, so it cannot
    /// climb undeveloped high ground (foothills, inland basins) and mistake it for
    /// water. Leave unset for low/flat coastal cities where land sits near 0 m.
    #[serde(default)]
    pub water_max_elev_m: Option<f32>,
}

/// Bounding box in WGS-84 (EPSG:4326).
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct BboxWgs84 {
    pub west: f64,
    pub south: f64,
    pub east: f64,
    pub north: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PathsConfig {
    pub osm_pbf: PathBuf,
    pub dem_tif: PathBuf,
    pub output_db: PathBuf,
    pub mapping_json: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ElevationConfig {
    pub walkable_threshold_m: f64,
    pub flatten_buildings: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CollisionConfig {
    pub building_floor_blocked: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AtlasConfig {
    pub tile_width: u32,
    pub tile_height: u32,
    pub columns: u32,
    pub path: String,
}

impl Config {
    pub fn from_file(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config: {}", path.display()))?;
        toml::from_str(&text).context("parsing pipeline.toml")
    }

    /// Cells per chunk side (same for width and height at a given LOD).
    pub fn cells_per_chunk(&self) -> u32 {
        (self.pipeline.chunk_meters / self.pipeline.meters_per_cell).round() as u32
    }
}

/// UTM 10N bounding box derived after reprojection (meters).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct BboxUtm {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

impl BboxUtm {
    /// Chunk grid dimensions: how many chunks fit in each axis.
    pub fn chunk_grid_dims(&self, chunk_meters: f64) -> (i32, i32) {
        let nx = ((self.max_x - self.min_x) / chunk_meters).ceil() as i32;
        let ny = ((self.max_y - self.min_y) / chunk_meters).ceil() as i32;
        (nx, ny)
    }

    /// World-space origin (min corner) for a given chunk (cx, cy).
    pub fn chunk_origin(&self, cx: i32, cy: i32, chunk_meters: f64) -> (f64, f64) {
        (
            self.min_x + cx as f64 * chunk_meters,
            self.min_y + cy as f64 * chunk_meters,
        )
    }
}
