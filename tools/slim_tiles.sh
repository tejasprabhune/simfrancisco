#!/usr/bin/env bash
# Build slim server-side tiles DBs for deployment.
#
# The backend only ever reads the LOD-0 collision grid + the meta manifest
# (geo.rs: sample_residential_cell + pathfinding). It never touches the render
# layer, LODs 1-5, or the buildings table. So we copy just those into
# server_tiles/<slug>.db, replacing the render blob with a 1-byte placeholder.
# This turns ~1.4 GB of full tiles into ~40 MB that is safe to bake into the
# deploy image and commit to git.
#
# Usage: tools/slim_tiles.sh   (run from repo root)
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p server_tiles

# slug:source-full-tiles.db pairs (portable; no associative arrays for bash 3.2)
PAIRS="sf:tiles.db \
neu_york:artifacts/tiles_neu_york.db \
synth_la:artifacts/tiles_synth_la.db \
cybercago:artifacts/tiles_cybercago.db \
simami:artifacts/tiles_simami.db"

for pair in $PAIRS; do
  slug="${pair%%:*}"
  src="${pair#*:}"
  out="server_tiles/${slug}.db"
  if [[ ! -f "$src" ]]; then echo "!! missing $src — skip $slug"; continue; fi
  rm -f "$out"
  sqlite3 "$out" <<SQL
ATTACH '$src' AS full;
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
INSERT INTO meta SELECT key, value FROM full.meta;
CREATE TABLE chunks (
  cx INTEGER NOT NULL, cy INTEGER NOT NULL, lod INTEGER NOT NULL,
  render BLOB NOT NULL, collision BLOB NOT NULL,
  w INTEGER NOT NULL, h INTEGER NOT NULL,
  PRIMARY KEY (cx, cy, lod)
);
INSERT INTO chunks SELECT cx, cy, lod, X'00', collision, w, h
  FROM full.chunks WHERE lod = 0;
CREATE TABLE buildings (
  cx INTEGER NOT NULL, cy INTEGER NOT NULL,
  cell_x REAL NOT NULL, cell_y REAL NOT NULL,
  cell_w REAL NOT NULL, cell_h REAL NOT NULL,
  tier INTEGER NOT NULL,
  PRIMARY KEY (cx, cy, cell_x, cell_y)
);
DETACH full;
VACUUM;
SQL
  printf "  %-10s %s -> %s  (%s)\n" "$slug" "$(du -h "$src" | cut -f1)" "$out" "$(du -h "$out" | cut -f1)"
done
echo "total server_tiles: $(du -sh server_tiles | cut -f1)"
