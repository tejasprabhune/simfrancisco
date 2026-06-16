/// DEM (Digital Elevation Model) ingestion and per-cell elevation sampling.
///
/// Reads USGS 3DEP GeoTIFF files (Float32, EPSG:4326) using the `tiff` crate.
/// Parses GeoTIFF ModelPixelScaleTag + ModelTiepointTag to build an affine
/// geotransform, then reprojects query points from UTM 10N to WGS-84 on the fly
/// so callers always work in the pipeline's native projected CRS.
///
/// Assumed DEM CRS: EPSG:4326 (WGS-84 geographic, lon/lat).
/// USGS 3DEP 1/3 arc-second and 1 m products use this CRS.
/// If a different source DEM is used, override source_crs in DemReader::from_file.

use std::io::BufReader;
use std::path::Path;

use anyhow::{bail, Context, Result};
use proj::Proj;
use tiff::decoder::{Decoder, DecodingResult};
use tiff::tags::Tag;

use crate::types::Grid;

const NODATA_THRESHOLD: f32 = -9000.0;

/// Affine geotransform: maps pixel (col, row) ↔ CRS (x, y).
///
/// `pixel_height` is the absolute per-row CRS step (positive).
/// CRS y decreases as row increases (north-up rasters).
#[derive(Debug, Clone)]
pub struct GeoTransform {
    pub x_origin: f64,
    pub y_origin: f64,
    pub pixel_width: f64,
    pub pixel_height: f64,
}

impl GeoTransform {
    /// CRS (x, y) → fractional pixel (col, row).
    pub fn crs_to_pixel(&self, x: f64, y: f64) -> (f64, f64) {
        (
            (x - self.x_origin) / self.pixel_width,
            (self.y_origin - y) / self.pixel_height,
        )
    }
}

/// In-memory DEM raster with UTM-coordinate sampling.
pub struct DemReader {
    data: Vec<f32>,
    pub width: u32,
    pub height: u32,
    pub transform: GeoTransform,
    /// Reprojects UTM 10N (EPSG:32610) → DEM native CRS (EPSG:4326).
    utm_to_dem: Option<Proj>,
}

impl DemReader {
    /// Load a GeoTIFF DEM. Assumes DEM is in EPSG:4326 (WGS-84).
    /// Supports Float32, Float64, Int16, UInt16, Int32 pixel formats.
    /// `utm_epsg` is the pipeline's projected CRS; sampling reprojects UTM→WGS-84.
    pub fn from_file(path: &Path, utm_epsg: &str) -> Result<Self> {
        let file = std::fs::File::open(path)
            .with_context(|| format!("open DEM: {}", path.display()))?;
        let mut dec = Decoder::new(BufReader::new(file))
            .context("init TIFF decoder")?;

        let (width, height) = dec.dimensions().context("DEM dimensions")?;

        // ModelPixelScaleTag (33550): [scale_x, scale_y, scale_z].
        let scale = dec
            .get_tag_f64_vec(Tag::ModelPixelScaleTag)
            .context("ModelPixelScaleTag missing — is this a GeoTIFF?")?;
        anyhow::ensure!(scale.len() >= 2, "ModelPixelScaleTag must have ≥ 2 values");

        // ModelTiepointTag (33922): [I, J, K, X, Y, Z, ...].
        // First tiepoint: pixel (I, J) corresponds to CRS (X, Y).
        let tp = dec
            .get_tag_f64_vec(Tag::ModelTiepointTag)
            .context("ModelTiepointTag missing")?;
        anyhow::ensure!(tp.len() >= 6, "ModelTiepointTag must have ≥ 6 values");

        // Adjust origin so it refers to pixel (0, 0).
        let x_origin = tp[3] - tp[0] * scale[0];
        let y_origin = tp[4] + tp[1] * scale[1];

        let transform = GeoTransform {
            x_origin,
            y_origin,
            pixel_width: scale[0],
            pixel_height: scale[1],
        };

        let raw = dec.read_image().context("read DEM image data")?;
        let data = decode_to_f32(raw)?;

        anyhow::ensure!(
            data.len() == (width * height) as usize,
            "DEM data len {} ≠ w*h {}",
            data.len(),
            width * height
        );

        // UTM → WGS-84 for coordinate conversion when sampling.
        let utm_to_dem = Some(
            Proj::new_known_crs(utm_epsg, "EPSG:4326", None)
                .with_context(|| format!("build {utm_epsg}→WGS84 projector"))?,
        );

        Ok(Self { data, width, height, transform, utm_to_dem })
    }

    /// Construct a synthetic DEM for testing.
    ///
    /// `elevation_fn(col, row)` provides elevation at each pixel.
    /// No CRS reprojection — queries are in native CRS units.
    pub fn synthetic(
        x_origin: f64,
        y_origin: f64,
        pixel_width: f64,
        pixel_height: f64,
        width: u32,
        height: u32,
        elevation_fn: impl Fn(u32, u32) -> f32,
    ) -> Self {
        let mut data = Vec::with_capacity((width * height) as usize);
        for row in 0..height {
            for col in 0..width {
                data.push(elevation_fn(col, row));
            }
        }
        Self {
            data,
            width,
            height,
            transform: GeoTransform { x_origin, y_origin, pixel_width, pixel_height },
            utm_to_dem: None,
        }
    }

    /// Sample elevation at a CRS (lon for WGS-84 DEMs) point.
    /// Returns None if outside the raster extent.
    pub fn sample_crs(&self, x: f64, y: f64) -> Option<f32> {
        let (col, row) = self.transform.crs_to_pixel(x, y);
        self.bilinear(col, row)
    }

    /// Sample elevation at a UTM 10N (easting, northing) coordinate.
    /// When loaded from a real file, reprojects to WGS-84 first.
    /// On synthetic DEMs (no projector), treats input as native CRS.
    pub fn sample_utm(&self, easting: f64, northing: f64) -> Option<f32> {
        if let Some(proj) = &self.utm_to_dem {
            let (lon, lat) = proj.convert((easting, northing)).ok()?;
            self.sample_crs(lon, lat)
        } else {
            self.sample_crs(easting, northing)
        }
    }

    /// Build a `w × h` elevation grid for one chunk.
    ///
    /// `origin_x`, `origin_y`: UTM coordinates of the chunk's SW corner.
    /// Samples at the center of each cell.
    /// Cells outside the DEM extent are filled with 0.0 (sea level).
    pub fn sample_grid_utm(
        &self,
        origin_x: f64,
        origin_y: f64,
        w: u32,
        h: u32,
        meters_per_cell: f64,
    ) -> Grid<f32> {
        let mut grid = Grid::filled(w, h, 0.0f32);
        for row in 0..h {
            for col in 0..w {
                // Cell center in UTM: x increases east, y increases north.
                // Grid row 0 = northernmost → subtract from top.
                let x = origin_x + (col as f64 + 0.5) * meters_per_cell;
                let y = origin_y + (h as f64 - row as f64 - 0.5) * meters_per_cell;
                if let Some(elev) = self.sample_utm(x, y) {
                    grid.set(col, row, elev);
                }
            }
        }
        grid
    }

    /// Bilinear interpolation at fractional pixel (col, row).
    ///
    /// Returns None if (col, row) is completely outside the raster.
    /// At the far edge, the upper/right neighbor is clamped to the boundary
    /// pixel so we don't return None for points that lie on the last pixel row
    /// or column.
    fn bilinear(&self, col: f64, row: f64) -> Option<f32> {
        let c0 = col.floor() as i64;
        let r0 = row.floor() as i64;

        if c0 < 0 || r0 < 0 || c0 >= self.width as i64 || r0 >= self.height as i64 {
            return None;
        }

        // Clamp upper neighbor to last valid pixel at the raster edge.
        let c1 = (c0 + 1).min(self.width as i64 - 1);
        let r1 = (r0 + 1).min(self.height as i64 - 1);

        let fc = (col - col.floor()) as f32;
        let fr = (row - row.floor()) as f32;

        let v00 = self.pixel_clamped(c0 as u32, r0 as u32);
        let v10 = self.pixel_clamped(c1 as u32, r0 as u32);
        let v01 = self.pixel_clamped(c0 as u32, r1 as u32);
        let v11 = self.pixel_clamped(c1 as u32, r1 as u32);

        Some(
            v00 * (1.0 - fc) * (1.0 - fr)
                + v10 * fc * (1.0 - fr)
                + v01 * (1.0 - fc) * fr
                + v11 * fc * fr,
        )
    }

    #[inline]
    fn pixel_clamped(&self, col: u32, row: u32) -> f32 {
        let v = self.data[(row * self.width + col) as usize];
        if v < NODATA_THRESHOLD { 0.0 } else { v }
    }
}

fn decode_to_f32(result: DecodingResult) -> Result<Vec<f32>> {
    match result {
        DecodingResult::F32(v) => Ok(v),
        DecodingResult::F64(v) => Ok(v.into_iter().map(|x| x as f32).collect()),
        DecodingResult::I16(v) => Ok(v.into_iter().map(|x| x as f32).collect()),
        DecodingResult::U16(v) => Ok(v.into_iter().map(|x| x as f32).collect()),
        DecodingResult::I32(v) => Ok(v.into_iter().map(|x| x as f32).collect()),
        DecodingResult::U32(v) => Ok(v.into_iter().map(|x| x as f32).collect()),
        other => bail!("unsupported DEM pixel format: {:?}", std::mem::discriminant(&other)),
    }
}

/// Per-cell gradient computation.
///
/// Returns a grid of the maximum absolute rise (in meters) between a cell
/// and any of its 4 cardinal neighbors. Used for cliff/stairs classification
/// (Phase 4): rise > WALKABLE_THRESHOLD_M → steep cell.
pub fn compute_max_rise(elevation: &Grid<f32>) -> Grid<f32> {
    let w = elevation.width;
    let h = elevation.height;
    let mut out = Grid::filled(w, h, 0.0f32);

    for row in 0..h {
        for col in 0..w {
            let here = *elevation.get(col, row);
            let mut max_rise = 0.0f32;

            // 4-neighbor max absolute rise.
            for (dc, dr) in [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)] {
                let nc = col as i32 + dc;
                let nr = row as i32 + dr;
                if nc >= 0 && nr >= 0 && nc < w as i32 && nr < h as i32 {
                    let neighbor = *elevation.get(nc as u32, nr as u32);
                    let rise = (here - neighbor).abs();
                    if rise > max_rise {
                        max_rise = rise;
                    }
                }
            }
            out.set(col, row, max_rise);
        }
    }
    out
}

/// Average elevation over a rectangular region (inclusive).
/// Returns 0.0 if the region is empty or entirely outside the grid.
pub fn average_elevation(
    elevation: &Grid<f32>,
    col_min: u32,
    row_min: u32,
    col_max: u32,
    row_max: u32,
) -> f32 {
    let w = elevation.width;
    let h = elevation.height;
    let c0 = col_min.min(w.saturating_sub(1));
    let c1 = col_max.min(w.saturating_sub(1));
    let r0 = row_min.min(h.saturating_sub(1));
    let r1 = row_max.min(h.saturating_sub(1));

    let mut sum = 0.0f64;
    let mut count = 0u64;
    for row in r0..=r1 {
        for col in c0..=c1 {
            sum += *elevation.get(col, row) as f64;
            count += 1;
        }
    }
    if count == 0 { 0.0 } else { (sum / count as f64) as f32 }
}
