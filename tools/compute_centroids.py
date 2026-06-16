#!/usr/bin/env python3
"""Compute per-city PUMA grid centroids for data/cities/<slug>.toml.

For each city we take the TIGER 2020-vintage PUMA polygons for its state, clip
them to the city's WGS-84 bbox (so a PUMA that spills into the Everglades or the
ocean is represented by its in-frame, developed part), reproject to the city's
UTM zone, and reduce each PUMA to a chunk-grid CentroidEntry {puma, cx, cy,
radius}. The chunk mapping matches geo.rs exactly:
    cx = (utm_x - bbox_utm.min_x) / chunk_meters     (+gx is east)
    cy = (bbox_utm.max_y - utm_y) / chunk_meters      (+gy is south)
radius (chunks) is half the PUMA's clipped extent, so sample_residential_cell
draws homes across the whole PUMA footprint, not a point.

Pure-Python geometry (shoelace area-weighted centroid); projection/clip via GDAL
ogr2ogr. No shapely/pyproj needed. Run from the repo root.
"""
import json, math, os, sqlite3, subprocess, sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SHP = "/tmp/gaz"  # tl_2023_<fips>_puma20 shapefiles unzipped into <fips>st / fl12

# slug -> (state shapefile dir, shapefile basename, wgs84 bbox W,S,E,N)
CITIES = {
    "neu_york":  ("36st",  "tl_2023_36_puma20", (-74.05, 40.55, -73.70, 40.92)),
    "synth_la":  ("06st",  "tl_2023_06_puma20", (-118.62, 33.93, -118.16, 34.25)),
    "cybercago": ("17st",  "tl_2023_17_puma20", (-87.85, 41.64, -87.52, 42.02)),
    "simami":    ("fl12",  "tl_2023_12_puma20", (-80.33, 25.62, -80.10, 25.93)),
}
RADIUS_MIN, RADIUS_MAX = 4, 18


def manifest(slug):
    db = sqlite3.connect(os.path.join(REPO, f"artifacts/tiles_{slug}.db"))
    m = json.loads(db.execute("SELECT value FROM meta WHERE key='manifest'").fetchone()[0])
    mx, my = db.execute("SELECT MAX(cx)+1, MAX(cy)+1 FROM chunks WHERE lod=0").fetchone()
    db.close()
    b = m["bbox_utm"]
    return dict(crs=m["crs"], chunk_m=m["chunk_meters"], cpc=m["cells_per_chunk"],
               min_x=b["min_x"], max_y=b["max_y"], chunks_x=mx, chunks_y=my)


def config_pumas(slug):
    line = next(l for l in open(os.path.join(REPO, f"data/cities/{slug}.toml"))
                if l.strip().startswith("pumas"))
    return set(int(x) for x in line.split("[")[1].split("]")[0].split(","))


def ogr_clip_reproject(slug, shpdir, base, bbox, epsg):
    """Return (clipped_to_bbox_features, full_unclipped_features), both in UTM."""
    src = os.path.join(SHP, shpdir, base + ".shp")
    w, s, e, n = bbox
    clip = f"/tmp/gaz/{slug}_clip4326.json"
    clip_utm = f"/tmp/gaz/{slug}_clip_utm.json"
    full_utm = f"/tmp/gaz/{slug}_full_utm.json"
    for f in (clip, clip_utm, full_utm):
        if os.path.exists(f):
            os.remove(f)
    # clipped-to-bbox PUMA parts (accurate in-frame centroid)
    subprocess.run(["ogr2ogr", "-f", "GeoJSON", clip, src,
                    "-clipsrc", str(w), str(s), str(e), str(n)], check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    subprocess.run(["ogr2ogr", "-f", "GeoJSON", "-t_srs", epsg, clip_utm, clip], check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    # full PUMA polygons (for out-of-frame PUMAs -> directional edge clamp)
    subprocess.run(["ogr2ogr", "-f", "GeoJSON", "-t_srs", epsg, full_utm, src], check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    return json.load(open(clip_utm)), json.load(open(full_utm))


def ring_centroid_area(ring):
    """Shoelace signed area + area-weighted centroid of one ring (UTM coords)."""
    a = cx = cy = 0.0
    for i in range(len(ring) - 1):
        x0, y0 = ring[i][0], ring[i][1]
        x1, y1 = ring[i + 1][0], ring[i + 1][1]
        cross = x0 * y1 - x1 * y0
        a += cross
        cx += (x0 + x1) * cross
        cy += (y0 + y1) * cross
    a *= 0.5
    if abs(a) < 1e-9:
        return 0.0, 0.0, 0.0
    return a, cx / (6 * a), cy / (6 * a)


def poly_centroid(geom):
    """Area-weighted centroid + bbox of a (Multi)Polygon's outer rings (UTM)."""
    polys = geom["coordinates"] if geom["type"] == "MultiPolygon" else [geom["coordinates"]]
    tot = sx = sy = 0.0
    minx = miny = math.inf
    maxx = maxy = -math.inf
    for poly in polys:
        outer = poly[0]
        a, cx, cy = ring_centroid_area(outer)
        w = abs(a)
        tot += w
        sx += cx * w
        sy += cy * w
        for x, y in outer:
            minx, miny = min(minx, x), min(miny, y)
            maxx, maxy = max(maxx, x), max(maxy, y)
    if tot == 0:
        return None
    return sx / tot, sy / tot, (minx, miny, maxx, maxy)


def chunk_land_mask(slug, chunks_x, chunks_y, cpc):
    """Per-chunk boolean: True if the chunk is majority non-water (land where
    residents can stand). Built from the LOD-0 render class map in tiles.db."""
    import numpy as np, zstandard as zstd
    db = sqlite3.connect(os.path.join(REPO, f"artifacts/tiles_{slug}.db"))
    dctx = zstd.ZstdDecompressor()
    land = np.zeros((chunks_y, chunks_x), bool)
    for cx, cy, blob, w, h in db.execute("SELECT cx,cy,render,w,h FROM chunks WHERE lod=0"):
        raw = dctx.decompress(blob, max_output_size=w * h * 4)
        cls = (np.frombuffer(raw, dtype='<u4', count=w * h).reshape(h, w) & 0xFF)
        land[cy, cx] = (cls == 10).mean() < 0.5  # class 10 = Water
    db.close()
    return land


def snap_to_land(cx, cy, land):
    """Nearest majority-land chunk to (cx,cy), searching outward in rings."""
    cys, cxs = land.shape
    if 0 <= cy < cys and 0 <= cx < cxs and land[cy, cx]:
        return cx, cy
    for rad in range(1, max(cys, cxs)):
        best = None
        for dy in range(-rad, rad + 1):
            for dx in range(-rad, rad + 1):
                if max(abs(dx), abs(dy)) != rad:
                    continue
                nx, ny = cx + dx, cy + dy
                if 0 <= ny < cys and 0 <= nx < cxs and land[ny, nx]:
                    d = dx * dx + dy * dy
                    if best is None or d < best[0]:
                        best = (d, nx, ny)
        if best:
            return best[1], best[2]
    return cx, cy


def main():
    out = {}
    for slug, (shpdir, base, bbox) in CITIES.items():
        man = manifest(slug)
        want = config_pumas(slug)
        clipped, full = ogr_clip_reproject(slug, shpdir, base, bbox, man["crs"])

        def to_entry(geom):
            res = poly_centroid(geom)
            if not res:
                return None
            ux, uy, (minx, miny, maxx, maxy) = res
            cx = max(0, min(man["chunks_x"] - 1, int(round((ux - man["min_x"]) / man["chunk_m"]))))
            cy = max(0, min(man["chunks_y"] - 1, int(round((man["max_y"] - uy) / man["chunk_m"]))))
            extent_m = 0.25 * ((maxx - minx) + (maxy - miny))
            radius = max(RADIUS_MIN, min(RADIUS_MAX, int(round(extent_m / man["chunk_m"]))))
            return (cx, cy, radius)

        entries = {}
        for feat in clipped["features"]:
            p = feat["properties"]
            puma = int(p.get("PUMACE20") or p.get("PUMACE10") or -1)
            if puma in want and feat.get("geometry") and puma not in entries:
                e = to_entry(feat["geometry"])
                if e:
                    entries[puma] = e
        # out-of-frame PUMAs: full-polygon centroid clamped to the nearest grid edge,
        # with a tighter radius so they hug that edge rather than sprawl inward.
        clamped = []
        for feat in full["features"]:
            p = feat["properties"]
            puma = int(p.get("PUMACE20") or p.get("PUMACE10") or -1)
            if puma in want and puma not in entries and feat.get("geometry"):
                e = to_entry(feat["geometry"])
                if e:
                    entries[puma] = (e[0], e[1], min(e[2], 7))
                    clamped.append(puma)
        # snap any centroid that landed on water to the nearest land chunk, so the
        # sampling box is centred where residents actually live.
        land = chunk_land_mask(slug, man["chunks_x"], man["chunks_y"], man["cpc"])
        snapped = 0
        for puma, (cx, cy, r) in list(entries.items()):
            ncx, ncy = snap_to_land(cx, cy, land)
            if (ncx, ncy) != (cx, cy):
                snapped += 1
            entries[puma] = (ncx, ncy, r)
        missing = sorted(want - set(entries))
        out[slug] = (man, entries, missing)
        print(f"# {slug}: {len(entries)}/{len(want)} mapped "
              f"({len(clamped)} edge-clamped, {snapped} snapped off-water), "
              f"grid {man['chunks_x']}x{man['chunks_y']}, missing={missing}")
    # emit TOML blocks
    with open("/tmp/gaz/centroids.toml", "w") as f:
        for slug, (man, entries, missing) in out.items():
            f.write(f"\n##### {slug} #####\n")
            for puma in sorted(entries):
                cx, cy, r = entries[puma]
                f.write(f"[[centroids]]\npuma = {puma}\ncx = {cx}\ncy = {cy}\nradius = {r}\n")
    print("\nwrote /tmp/gaz/centroids.toml")
    json.dump({s: {str(p): v for p, v in e.items()} for s, (m, e, mi) in out.items()},
              open("/tmp/gaz/centroids.json", "w"))


if __name__ == "__main__":
    main()
