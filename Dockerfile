# Multi-stage Rust build for the SF digital-twin server (debian-slim runtime).
FROM rust:1-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --bin server

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/server /usr/local/bin/server
# data assets baked into the image (public Census/OSM data; no secrets)
COPY tiles.db rubric.yaml ./
COPY data/sf_pums.csv ./data/sf_pums.csv
ENV PORT=8080 \
    TILES_DB=tiles.db \
    RUST_LOG=info
EXPOSE 8080
CMD ["server"]
