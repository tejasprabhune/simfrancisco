/// OSM PBF ingestion: reads node coordinates, way geometry, and multipolygon
/// relations; reprojects everything to UTM 10N; returns a FeatureIndex ready
/// for per-chunk rasterization.
///
/// Strategy (two file passes):
///   Pass 1 — build node_map (id → UTM) AND collect multipolygon relation
///             metadata (outer/inner way-IDs + feature kind).
///   Pass 2 — build per-feature geometry for standalone ways;
///             cache geometry for relation-member ways.
///   Post-process — assemble multipolygon outer rings; coastline land polygon.
///
/// Precedence when overlapping a cell (highest overrides):
///   Water > BuildingWall > BuildingFloor > CliffFace > Stairs
///   > Road > Sidewalk > Path > ParkGrass > Sand > Grass

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use geo::algorithm::contains::Contains;
use geo::{Coord, LineString, Point, Polygon};
use osmpbf::{Element, ElementReader, RelMemberType};

use crate::config::BboxUtm;
use crate::crs::new_proj;
use crate::types::SemanticClass;

// ── public types ─────────────────────────────────────────────────────────────

pub struct RoadFeature {
    pub line: LineString<f64>,
    pub semantic: SemanticClass,
    pub buffer_m: f64,
}

#[derive(Clone, Copy, Debug)]
pub enum BuildingTier { Low, Mid, Tall }

pub struct BuildingRecord {
    pub poly: Polygon<f64>,
    pub tier: BuildingTier,
}

/// All OSM features loaded for the full area, in UTM 10N coordinates.
pub struct FeatureIndex {
    pub buildings: Vec<BuildingRecord>,
    pub water_polys: Vec<Polygon<f64>>,
    pub park_polys: Vec<Polygon<f64>>,
    pub plaza_polys: Vec<Polygon<f64>>,
    pub roads: Vec<RoadFeature>,
    /// Assembled land polygons from natural=coastline ways, one per landmass
    /// (Manhattan, Brooklyn, the Bronx, ...). Cells inside the full bbox but
    /// OUTSIDE every one of these are water (ocean, harbor, the Great Lakes).
    pub coastline_land_polys: Vec<Polygon<f64>>,
}

impl FeatureIndex {
    pub fn empty() -> Self {
        Self {
            buildings: vec![],
            water_polys: vec![],
            park_polys: vec![],
            plaza_polys: vec![],
            roads: vec![],
            coastline_land_polys: vec![],
        }
    }

    /// Load features from an OSM PBF file, filtering to `bbox`.
    /// Returns `empty()` with a warning if the file is absent.
    pub fn from_pbf(
        path: &Path,
        bbox: &BboxUtm,
        utm_epsg: &str,
        land_refs: &[(f64, f64)],
    ) -> Result<Self> {
        let (node_map, mp_relations) = pass1_nodes_and_relations(path, utm_epsg)?;

        let relation_outer_ids: HashSet<i64> = mp_relations
            .iter()
            .flat_map(|r| r.outer_ids.iter().copied())
            .collect();

        pass2_ways(path, &node_map, &mp_relations, &relation_outer_ids, bbox, land_refs)
    }
}

// ── internal data ────────────────────────────────────────────────────────────

type NodeMap = HashMap<i64, (f64, f64)>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AreaKind { Water, Park, Building, Plaza }

struct MpRelation {
    kind: AreaKind,
    outer_ids: Vec<i64>,
    // inner_ids unused in v1 (holes not rasterized)
}

// ── pass 1: nodes + relations ────────────────────────────────────────────────

fn pass1_nodes_and_relations(path: &Path, utm_epsg: &str) -> Result<(NodeMap, Vec<MpRelation>)> {
    let proj = new_proj(utm_epsg)?;
    let mut nodes: HashMap<i64, (f64, f64)> = HashMap::new();
    let mut relations: Vec<MpRelation> = Vec::new();

    let reader = ElementReader::from_path(path)
        .with_context(|| format!("open OSM PBF: {}", path.display()))?;

    reader.for_each(|element| match element {
        Element::Node(n) => {
            if let Ok((x, y)) = proj.convert((n.lon(), n.lat())) {
                nodes.insert(n.id(), (x, y));
            }
        }
        Element::DenseNode(n) => {
            if let Ok((x, y)) = proj.convert((n.lon(), n.lat())) {
                nodes.insert(n.id(), (x, y));
            }
        }
        Element::Relation(r) => {
            let is_mp = r.tags().any(|(k, v)| k == "type" && v == "multipolygon");
            if !is_mp {
                return;
            }
            let kind = match area_kind_from_tags(r.tags()) {
                Some(k) => k,
                None => return,
            };
            let mut outer_ids = Vec::new();
            for m in r.members() {
                if m.member_type != RelMemberType::Way {
                    continue;
                }
                match m.role().unwrap_or("") {
                    "outer" | "" => outer_ids.push(m.member_id),
                    _ => {}
                }
            }
            if !outer_ids.is_empty() {
                relations.push(MpRelation { kind, outer_ids });
            }
        }
        _ => {}
    })?;

    Ok((nodes, relations))
}

// ── pass 2: ways ─────────────────────────────────────────────────────────────

fn pass2_ways(
    path: &Path,
    node_map: &NodeMap,
    mp_relations: &[MpRelation],
    relation_outer_ids: &HashSet<i64>,
    bbox: &BboxUtm,
    land_refs: &[(f64, f64)],
) -> Result<FeatureIndex> {
    let mut idx = FeatureIndex::empty();
    // Cache for way geometries that are needed by multipolygon relations.
    let mut way_cache: HashMap<i64, Vec<(f64, f64)>> = HashMap::new();
    // Coastline segments (open ways) collected for later assembly.
    let mut coastline_segs: Vec<Vec<(f64, f64)>> = Vec::new();

    let reader = ElementReader::from_path(path)
        .with_context(|| format!("open OSM PBF (pass 2): {}", path.display()))?;

    reader.for_each(|element| {
        let w = match element {
            Element::Way(w) => w,
            _ => return,
        };

        let refs: Vec<i64> = w.refs().collect();
        if refs.len() < 2 {
            return;
        }
        let is_closed = refs.first() == refs.last() && refs.len() >= 4;

        // Cache geometry if this way is a relation outer member.
        if relation_outer_ids.contains(&w.id()) {
            let pts = way_coords(&refs, node_map);
            if pts.len() >= 2 {
                way_cache.insert(w.id(), pts);
            }
        }

        // Classify by tags.
        let mut is_building = false;
        let mut is_coastline = false;
        let mut area_kind: Option<AreaKind> = None;
        let mut highway: Option<(SemanticClass, f64)> = None;
        let mut building_levels: Option<u32> = None;
        let mut building_value: &str = "yes";

        for (k, v) in w.tags() {
            match k {
                "building" | "building:part" => {
                    if !matches!(v, "no") {
                        is_building = true;
                        building_value = v;
                    }
                }
                "building:levels" => {
                    building_levels = v.parse::<u32>().ok();
                }
                "natural" if v == "coastline" => is_coastline = true,
                _ => {}
            }
            if area_kind.is_none() {
                area_kind = area_kind_from_tag(k, v);
            }
            if highway.is_none() {
                highway = classify_highway(k, v);
            }
        }

        let pts = way_coords(&refs, node_map);
        if pts.is_empty() {
            return;
        }

        if is_coastline {
            // Only keep nodes within (or very close to) the bbox so that
            // snap_to_bbox works correctly and the assembled polygon is
            // well-formed.  A 10-m tolerance retains nodes that osmium
            // clipped exactly to the bbox boundary.
            let tolerance = 10.0_f64;
            let in_bbox: Vec<(f64, f64)> = pts
                .into_iter()
                .filter(|&(x, y)| {
                    x >= bbox.min_x - tolerance
                        && x <= bbox.max_x + tolerance
                        && y >= bbox.min_y - tolerance
                        && y <= bbox.max_y + tolerance
                })
                .collect();
            if in_bbox.len() >= 2 {
                coastline_segs.push(in_bbox);
            }
            return;
        }

        if is_closed {
            let poly = pts_to_polygon(pts);
            if !bbox_intersects_poly(&poly, bbox) {
                return;
            }
            if is_building {
                let tier = building_tier_from_tags(building_levels, building_value);
                idx.buildings.push(BuildingRecord { poly, tier });
            } else if let Some(AreaKind::Water) = area_kind {
                idx.water_polys.push(poly);
            } else if let Some(AreaKind::Park) = area_kind {
                idx.park_polys.push(poly);
            } else if let Some(AreaKind::Plaza) = area_kind {
                idx.plaza_polys.push(poly);
            }
        } else if let Some((semantic, buffer_m)) = highway {
            let line = pts_to_linestring(pts);
            if !bbox_intersects_line(&line, bbox) {
                return;
            }
            idx.roads.push(RoadFeature { line, semantic, buffer_m });
        }
    })?;

    // Assemble multipolygon relation polygons.
    for rel in mp_relations {
        let ring = assemble_ring(&rel.outer_ids, &way_cache);
        if ring.len() < 4 {
            continue;
        }
        let poly = pts_to_polygon(ring);
        if !bbox_intersects_poly(&poly, bbox) {
            continue;
        }
        match rel.kind {
            AreaKind::Water    => idx.water_polys.push(poly),
            AreaKind::Park     => idx.park_polys.push(poly),
            AreaKind::Building => idx.buildings.push(BuildingRecord { poly, tier: BuildingTier::Low }),
            AreaKind::Plaza    => idx.plaza_polys.push(poly),
        }
    }

    // Assemble coastline land polygon (best-effort).
    if !coastline_segs.is_empty() {
        idx.coastline_land_polys =
            assemble_land_polygon(coastline_segs, bbox, land_refs);
    }

    log::info!(
        "OSM loaded — {} buildings, {} water, {} parks, {} plazas, {} road features, coastline={}",
        idx.buildings.len(),
        idx.water_polys.len(),
        idx.park_polys.len(),
        idx.plaza_polys.len(),
        idx.roads.len(),
        !idx.coastline_land_polys.is_empty(),
    );

    Ok(idx)
}

// ── building tier classification ─────────────────────────────────────────────

fn building_tier_from_tags(levels: Option<u32>, building_val: &str) -> BuildingTier {
    if let Some(n) = levels {
        return if n <= 3 {
            BuildingTier::Low
        } else if n <= 7 {
            BuildingTier::Mid
        } else {
            BuildingTier::Tall
        };
    }
    match building_val {
        "house" | "detached" | "bungalow" | "terrace" | "semidetached_house"
        | "shed" | "garage" | "garages" | "barn" | "greenhouse" | "cabin"
        | "warehouse" | "industrial" | "hangar" => BuildingTier::Low,
        "tower" | "skyscraper" => BuildingTier::Tall,
        _ => BuildingTier::Low,
    }
}

// ── geometry helpers ─────────────────────────────────────────────────────────

fn way_coords(refs: &[i64], node_map: &NodeMap) -> Vec<(f64, f64)> {
    refs.iter()
        .filter_map(|id| node_map.get(id).copied())
        .collect()
}

fn pts_to_polygon(pts: Vec<(f64, f64)>) -> Polygon<f64> {
    let coords: Vec<Coord<f64>> = pts.iter().map(|&(x, y)| Coord { x, y }).collect();
    Polygon::new(LineString::new(coords), vec![])
}

fn pts_to_linestring(pts: Vec<(f64, f64)>) -> LineString<f64> {
    LineString::new(pts.iter().map(|&(x, y)| Coord { x, y }).collect())
}

/// Chain way segments from `way_ids` into a single ordered point ring.
/// Tries both forward and reversed orientations for each segment.
fn assemble_ring(way_ids: &[i64], cache: &HashMap<i64, Vec<(f64, f64)>>) -> Vec<(f64, f64)> {
    let mut segments: Vec<Vec<(f64, f64)>> = way_ids
        .iter()
        .filter_map(|id| cache.get(id).cloned())
        .collect();

    if segments.is_empty() {
        return vec![];
    }

    let mut ring = segments.remove(0);
    let mut changed = true;
    while changed && !segments.is_empty() {
        changed = false;
        let mut i = 0;
        while i < segments.len() {
            let tail = *ring.last().unwrap();
            let head = segments[i][0];
            let rhead = *segments[i].last().unwrap();
            if approx_eq(tail, head) {
                let seg = segments.remove(i);
                ring.extend_from_slice(&seg[1..]);
                changed = true;
            } else if approx_eq(tail, rhead) {
                let mut seg = segments.remove(i);
                seg.reverse();
                ring.extend_from_slice(&seg[1..]);
                changed = true;
            } else {
                i += 1;
            }
        }
    }
    ring
}

const SNAP_M: f64 = 1.0; // 1-metre snapping tolerance for way endpoints

fn approx_eq(a: (f64, f64), b: (f64, f64)) -> bool {
    (a.0 - b.0).abs() < SNAP_M && (a.1 - b.1).abs() < SNAP_M
}

/// Attempt to assemble coastline segments into a land polygon closed with bbox
/// boundary edges.  Returns None if the coastline is too fragmented.
///
/// Strategy: build all connected chains from the segments, then close each
/// one with bbox boundary edges and check whether a known-land reference
/// point (Twin Peaks, UTM 10N) falls inside the resulting polygon.  This
/// correctly handles bboxes that contain coastlines from multiple land
/// masses (e.g. SF + Marin headlands).
fn assemble_land_polygon(
    segs: Vec<Vec<(f64, f64)>>,
    bbox: &BboxUtm,
    land_refs: &[(f64, f64)],
) -> Vec<Polygon<f64>> {
    // Chain all segments into connected chains.
    let mut remaining = segs;
    let mut chains: Vec<Vec<(f64, f64)>> = Vec::new();

    while !remaining.is_empty() {
        let mut chain = remaining.remove(0);
        let mut changed = true;
        while changed {
            changed = false;
            let mut i = 0;
            while i < remaining.len() {
                let tail = *chain.last().unwrap();
                let chead = chain[0];
                let seg_head = remaining[i][0];
                let seg_tail = *remaining[i].last().unwrap();
                if approx_eq(tail, seg_head) {
                    // Append seg to tail.
                    let seg = remaining.remove(i);
                    chain.extend_from_slice(&seg[1..]);
                    changed = true;
                } else if approx_eq(tail, seg_tail) {
                    // Append reversed seg to tail.
                    let mut seg = remaining.remove(i);
                    seg.reverse();
                    chain.extend_from_slice(&seg[1..]);
                    changed = true;
                } else if approx_eq(chead, seg_tail) {
                    // Prepend seg to head.
                    let mut seg = remaining.remove(i);
                    seg.pop(); // remove last point (≈ chead)
                    seg.extend_from_slice(&chain);
                    chain = seg;
                    changed = true;
                } else if approx_eq(chead, seg_head) {
                    // Prepend reversed seg to head.
                    let mut seg = remaining.remove(i);
                    seg.reverse();
                    seg.pop();
                    seg.extend_from_slice(&chain);
                    chain = seg;
                    changed = true;
                } else {
                    i += 1;
                }
            }
        }
        chains.push(chain);
    }

    // Points known to be on land, one per landmass (config `land_refs`/`land_ref`,
    // else bbox centre). Any closed coastline ring containing at least one of them
    // is land; cells outside every land ring are water.
    let mut polys: Vec<Polygon<f64>> = Vec::new();
    let corners = bbox_corners_cw(bbox);

    for (ci, mut ring) in chains.into_iter().enumerate() {
        let _ = ci;
        if ring.len() < 3 {
            continue;
        }

        // Close the ring along bbox boundary.
        let start = ring[0];
        let end = *ring.last().unwrap();
        let end_snap = snap_to_bbox(end, bbox);
        let start_snap = snap_to_bbox(start, bbox);
        let closing = walk_bbox_cw(&corners, end_snap, start_snap);
        ring.extend(closing);
        if !approx_eq(*ring.last().unwrap(), ring[0]) {
            ring.push(ring[0]);
        }
        if ring.len() < 4 {
            continue;
        }

        // Ensure CCW winding so geo::Contains works correctly.
        let signed_area: f64 = ring
            .windows(2)
            .map(|w| w[0].0 * w[1].1 - w[1].0 * w[0].1)
            .sum();
        if signed_area < 0.0 {
            ring.reverse();
        }

        let poly = pts_to_polygon(ring);
        if land_refs
            .iter()
            .any(|&(rx, ry)| poly.contains(&Point::new(rx, ry)))
        {
            polys.push(poly);
        }
    }

    polys
}

/// Clockwise bbox corners: NW, NE, SE, SW.
fn bbox_corners_cw(b: &BboxUtm) -> [(f64, f64); 4] {
    [
        (b.min_x, b.max_y),
        (b.max_x, b.max_y),
        (b.max_x, b.min_y),
        (b.min_x, b.min_y),
    ]
}

/// Snap a point to the nearest bbox edge.
/// Points outside the bbox are first clamped to the bbox, then snapped to
/// the closest edge of the clamped position.
fn snap_to_bbox(p: (f64, f64), b: &BboxUtm) -> (f64, f64) {
    let cx = p.0.max(b.min_x).min(b.max_x);
    let cy = p.1.max(b.min_y).min(b.max_y);
    let dx_w = (cx - b.min_x).abs();
    let dx_e = (cx - b.max_x).abs();
    let dy_s = (cy - b.min_y).abs();
    let dy_n = (cy - b.max_y).abs();
    let min_d = dx_w.min(dx_e).min(dy_s).min(dy_n);
    if min_d == dx_w { (b.min_x, cy) }
    else if min_d == dx_e { (b.max_x, cy) }
    else if min_d == dy_s { (cx, b.min_y) }
    else { (cx, b.max_y) }
}

/// Walk clockwise along the bbox perimeter from `from` to `to`,
/// inserting any corners encountered along the way.
fn walk_bbox_cw(corners: &[(f64, f64); 4], from: (f64, f64), to: (f64, f64)) -> Vec<(f64, f64)> {
    let edge_of = |p: (f64, f64)| -> usize {
        let (x, y) = p;
        // 0=north, 1=east, 2=south, 3=west
        let dn = (y - corners[0].1).abs();
        let de = (x - corners[1].0).abs();
        let ds = (y - corners[2].1).abs();
        let dw = (x - corners[3].0).abs();
        [dn, de, ds, dw]
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap()
            .0
    };
    let fe = edge_of(from);
    let te = edge_of(to);
    let mut out = Vec::new();
    if fe != te {
        // Insert corners from (fe+1) to te inclusive (clockwise order).
        // corners[te] is the junction entering edge te; it must be inserted
        // so the path follows the bbox boundary and never cuts diagonally.
        let mut e = (fe + 1) % 4;
        loop {
            out.push(corners[e]);
            if e == te {
                break;
            }
            e = (e + 1) % 4;
        }
    }
    out.push(to);
    out
}

// ── tag classification ────────────────────────────────────────────────────────

fn area_kind_from_tags<'a>(mut tags: impl Iterator<Item = (&'a str, &'a str)>) -> Option<AreaKind> {
    tags.find_map(|(k, v)| area_kind_from_tag(k, v))
}

fn area_kind_from_tag(k: &str, v: &str) -> Option<AreaKind> {
    match (k, v) {
        ("natural", "water") | ("natural", "bay") | ("waterway", "riverbank") => {
            Some(AreaKind::Water)
        }
        ("water", _) => Some(AreaKind::Water),
        ("leisure", "park") | ("leisure", "garden") | ("leisure", "nature_reserve") => {
            Some(AreaKind::Park)
        }
        ("landuse", "grass")
        | ("landuse", "meadow")
        | ("landuse", "village_green")
        | ("landuse", "cemetery")
        | ("landuse", "recreation_ground") => Some(AreaKind::Park),
        ("natural", "wood") | ("natural", "scrub") | ("natural", "heath") => Some(AreaKind::Park),
        ("building", v) if v != "no" => Some(AreaKind::Building),
        // Pedestrian plazas: closed highway=pedestrian ways, explicit plaza tags.
        ("highway", "pedestrian") => Some(AreaKind::Plaza),
        ("landuse", "plaza") | ("place", "square") | ("amenity", "marketplace") => {
            Some(AreaKind::Plaza)
        }
        _ => None,
    }
}

/// Return (SemanticClass, buffer_m) for a highway tag pair, or None.
pub fn classify_highway(k: &str, v: &str) -> Option<(SemanticClass, f64)> {
    if k != "highway" {
        return None;
    }
    Some(match v {
        "motorway" | "trunk"                          => (SemanticClass::Road,     14.0),
        "primary"                                      => (SemanticClass::Road,     10.0),
        "secondary"                                    => (SemanticClass::Road,      8.0),
        "tertiary"                                     => (SemanticClass::Road,      6.0),
        "residential" | "unclassified" | "road"       => (SemanticClass::Road,      4.0),
        "living_street" | "service"                    => (SemanticClass::Road,      3.0),
        "pedestrian"                                   => (SemanticClass::Sidewalk,  4.0),
        "footway" | "steps" | "cycleway"               => (SemanticClass::Path,      2.0),
        "path" | "track" | "bridleway"                 => (SemanticClass::Path,      2.0),
        _ => return None,
    })
}

// ── bbox filter helpers ───────────────────────────────────────────────────────

fn bbox_intersects_poly(poly: &Polygon<f64>, b: &BboxUtm) -> bool {
    use geo::algorithm::bounding_rect::BoundingRect;
    match poly.bounding_rect() {
        Some(r) => {
            r.max().x >= b.min_x
                && r.min().x <= b.max_x
                && r.max().y >= b.min_y
                && r.min().y <= b.max_y
        }
        None => false,
    }
}

fn bbox_intersects_line(line: &LineString<f64>, b: &BboxUtm) -> bool {
    use geo::algorithm::bounding_rect::BoundingRect;
    match line.bounding_rect() {
        Some(r) => {
            r.max().x >= b.min_x
                && r.min().x <= b.max_x
                && r.max().y >= b.min_y
                && r.min().y <= b.max_y
        }
        None => false,
    }
}
