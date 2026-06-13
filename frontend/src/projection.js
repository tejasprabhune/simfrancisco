// ─────────────────────────────────────────────────────────────────────────
// Geographic projection + point-in-polygon helpers.
//
// SF is small enough that a local equirectangular projection (longitude scaled
// by cos(centre latitude)) preserves shape with no visible distortion. We
// project lon/lat -> planar units, then fit the outline's planar bbox into the
// viewport, preserving aspect ratio and centering. Agents' real lon/lat run
// through the exact same transform, so dots land on the coastline.
// ─────────────────────────────────────────────────────────────────────────

const DEG = Math.PI / 180;

// Flatten a GeoJSON Polygon/MultiPolygon into an array of rings (each ring is
// an array of [lon, lat]). Exterior rings only — we dropped holes upstream.
export function ringsOf(feature) {
  const g = feature.geometry;
  if (g.type === "Polygon") return g.coordinates;
  if (g.type === "MultiPolygon") return g.coordinates.flat();
  return [];
}

export function makeProjection(feature) {
  const rings = ringsOf(feature);
  let minLon = Infinity, maxLon = -Infinity, minLat = Infinity, maxLat = -Infinity;
  for (const ring of rings) {
    for (const [lon, lat] of ring) {
      if (lon < minLon) minLon = lon;
      if (lon > maxLon) maxLon = lon;
      if (lat < minLat) minLat = lat;
      if (lat > maxLat) maxLat = lat;
    }
  }
  const lat0 = (minLat + maxLat) / 2;
  const kx = Math.cos(lat0 * DEG); // longitude compression at this latitude

  // planar coords: x grows east, y grows *down* (screen-friendly)
  const toPlanar = (lon, lat) => ({ x: (lon - minLon) * kx, y: (maxLat - lat) });
  const planarW = (maxLon - minLon) * kx;
  const planarH = (maxLat - minLat);

  // Viewport fit — recomputed on resize via fit(). `pad` is either a uniform
  // fraction of the viewport, or an {top,right,bottom,left} object in px (used
  // to reserve the top-right summary-card zone so the map never hides behind it).
  let scale = 1, offX = 0, offY = 0;
  function fit(width, height, pad = 0.08) {
    let top, right, bottom, left;
    if (typeof pad === "number") {
      top = bottom = height * pad;
      left = right = width * pad;
    } else {
      ({ top = 0, right = 0, bottom = 0, left = 0 } = pad);
    }
    const availW = Math.max(1, width - left - right);
    const availH = Math.max(1, height - top - bottom);
    scale = Math.min(availW / planarW, availH / planarH);
    offX = left + (availW - planarW * scale) / 2;
    offY = top + (availH - planarH * scale) / 2;
  }

  function project(lon, lat) {
    const p = toPlanar(lon, lat);
    return { x: offX + p.x * scale, y: offY + p.y * scale };
  }

  return {
    fit,
    project,
    rings,
    bbox: { minLon, maxLon, minLat, maxLat },
    get scale() { return scale; },
    // expose planar transform for noise fields / sampling in planar space
    toPlanar,
    planarSize: { w: planarW, h: planarH },
  };
}

// Ray-casting point-in-ring (lon/lat space). Even-odd across all rings ==
// inside the multipolygon (exterior rings, no holes).
function inRing(lon, lat, ring) {
  let inside = false;
  for (let i = 0, j = ring.length - 1; i < ring.length; j = i++) {
    const xi = ring[i][0], yi = ring[i][1];
    const xj = ring[j][0], yj = ring[j][1];
    const hit = (yi > lat) !== (yj > lat) &&
      lon < ((xj - xi) * (lat - yi)) / (yj - yi) + xi;
    if (hit) inside = !inside;
  }
  return inside;
}

export function makeInside(feature) {
  const rings = ringsOf(feature);
  // centroid (for pulling stray points back inside)
  let cx = 0, cy = 0, n = 0;
  for (const ring of rings) for (const [lon, lat] of ring) { cx += lon; cy += lat; n++; }
  cx /= n; cy /= n;

  const inside = (lon, lat) => rings.some((r) => inRing(lon, lat, r));

  // If a point sits just outside the (simplified) coastline, step it toward the
  // centroid until it's inside. Keeps every agent visibly on land.
  function clamp(lon, lat) {
    if (inside(lon, lat)) return [lon, lat];
    let L = lon, T = lat;
    for (let k = 0; k < 12; k++) {
      L = L + (cx - L) * 0.18;
      T = T + (cy - T) * 0.18;
      if (inside(L, T)) return [L, T];
    }
    return [cx, cy];
  }

  return { inside, clamp, centroid: [cx, cy] };
}
