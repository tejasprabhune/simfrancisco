#!/usr/bin/env bash
# Build a city's pixel-tile map end-to-end from its config.
#
#   scripts/build_city.sh <slug> [--export]
#
# Produces artifacts/tiles_<slug>.db (deterministic) and runs the verify harness.
# With --export, also stitches the whole-city PNG into artifacts/export_<slug>/
# (used by the frontend; see Phase 7). Atlases (assets/*.png) are city-agnostic
# and built once via `cargo run -p sim-maps --bin build_atlas`.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

CITY="${1:?usage: build_city.sh <slug> [--export]}"
CFG="config/cities/${CITY}.toml"
[ -f "$CFG" ] || { echo "no config: $CFG" >&2; exit 1; }
DB="artifacts/tiles_${CITY}.db"
EXPORT="${2:-}"

echo "[build_city] $CITY: pipeline -> $DB"
rm -f "$DB" "$DB-wal" "$DB-shm"
cargo run -p sim-maps --release --bin pipeline -- --config "$CFG"

if [ "$EXPORT" = "--export" ]; then
  echo "[build_city] $CITY: export whole-city PNG"
  cargo run -p sim-maps --release --bin export_map -- \
    --config "$CFG" --db "$DB" --out "artifacts/export_${CITY}" --full
fi

echo "[build_city] $CITY: verify"
cargo run -p sim-maps --release --bin verify -- --city "$CITY" || true

echo "[build_city] $CITY: done -> $DB"
