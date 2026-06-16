/// CRS reprojection helpers.
///
/// The pipeline works in a single projected CRS: EPSG:32610 (UTM Zone 10N).
/// This gives linear meter coordinates over the San Francisco area, so that
/// "2 m per cell" is exact and Euclidean distances are accurate.
///
/// All OSM input arrives in EPSG:4326 (WGS-84 geographic, lon/lat degrees).
/// All DEM data is reprojected into 32610 at ingest time.
///
/// The `proj` crate wraps PROJ via FFI. We create one `Proj` converter per
/// thread (it is not `Send`); for rayon workers, each worker constructs its
/// own converter on demand using `new_proj()`.

use anyhow::{Context, Result};
use proj::Proj;

use crate::config::{BboxUtm, BboxWgs84};

pub const EPSG_WGS84: &str = "EPSG:4326";
/// Back-compat default target CRS (San Francisco, UTM Zone 10N). New cities
/// pass their own zone via config (`[crs] utm_epsg`).
pub const EPSG_UTM10N: &str = "EPSG:32610";

/// Build a WGS-84 → UTM converter for the given target CRS (e.g. "EPSG:32618").
pub fn new_proj(utm_epsg: &str) -> Result<Proj> {
    Proj::new_known_crs(EPSG_WGS84, utm_epsg, None)
        .with_context(|| format!("create WGS-84→{utm_epsg} PROJ transform"))
}

/// Reproject a (longitude, latitude) point to (easting, northing) in meters.
///
/// Note: PROJ expects (lon, lat) order for EPSG:4326 in the source CRS.
pub fn wgs84_to_utm(proj: &Proj, lon: f64, lat: f64) -> Result<(f64, f64)> {
    proj.convert((lon, lat)).context("reproject lon/lat→UTM")
}

/// Reproject the WGS-84 bounding box to UTM 10N by projecting all four corners
/// and taking the enclosing envelope.
pub fn reproject_bbox(bbox: &BboxWgs84, utm_epsg: &str) -> Result<BboxUtm> {
    let p = new_proj(utm_epsg)?;
    let corners = [
        (bbox.west, bbox.south),
        (bbox.east, bbox.south),
        (bbox.west, bbox.north),
        (bbox.east, bbox.north),
    ];
    let mut xs = Vec::with_capacity(4);
    let mut ys = Vec::with_capacity(4);
    for (lon, lat) in corners {
        let (x, y) = wgs84_to_utm(&p, lon, lat)?;
        xs.push(x);
        ys.push(y);
    }
    Ok(BboxUtm {
        min_x: xs.iter().cloned().fold(f64::INFINITY, f64::min),
        min_y: ys.iter().cloned().fold(f64::INFINITY, f64::min),
        max_x: xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        max_y: ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
    })
}
