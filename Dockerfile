# Multi-stage Rust build for the SF digital-twin server (debian-slim runtime).
FROM rust:1-bookworm AS builder
WORKDIR /app
# Workspace manifests + sources. Only the server bin is built; the sim-maps crate
# (heavy GIS deps) is a workspace member but not in the server's dependency graph,
# so `-p simfrancisco --bin server` never compiles it.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release -p simfrancisco --bin server

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/server /usr/local/bin/server
# Data assets baked into the image (public Census/OSM data; no secrets).
# Slim per-city tiles (LOD-0 collision only, ~108 MB vs ~1.4 GB full), each
# city's backend profile, committed PUMS subsets, and today's news cache.
COPY server_tiles ./server_tiles
COPY data/cities ./data/cities
COPY data/news ./data/news
COPY data/sf_pums.csv data/neu_york_pums.csv data/synth_la_pums.csv \
     data/cybercago_pums.csv data/simami_pums.csv ./data/
ENV PORT=8080 \
    TILES_DB=server_tiles/sf.db \
    RUST_LOG=info
EXPOSE 8080
CMD ["server"]
