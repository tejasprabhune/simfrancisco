/// SQLite writer thread.
///
/// All DB writes are serialized through a single thread (schema below).
/// Worker threads send `WriteCmd` over an `mpsc` channel; the writer
/// batches them into transactions of up to `BATCH_SIZE` chunks for
/// throughput while avoiding lock contention.
///
/// Schema:
///   chunks(cx, cy, lod, render BLOB, collision BLOB, w, h) PK(cx,cy,lod)
///   meta(key TEXT PK, value TEXT)
///
/// BLOB byte layout (both layers):
///   render    — little-endian u32 array, row-major, w*h elements, then zstd-compressed
///   collision — little-endian u8 array,  row-major, w*h elements, then zstd-compressed

use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OpenFlags};
use zstd::encode_all;

use crate::types::{ChunkCoord, Grid};

const BATCH_SIZE: usize = 32;

/// Commands sent to the writer thread.
pub enum WriteCmd {
    Chunk(ChunkWrite),
    Building(BuildingWrite),
    Meta { key: String, value: String },
    Flush,
    Shutdown,
}

pub struct ChunkWrite {
    pub coord: ChunkCoord,
    pub lod: u32,
    pub render: Grid<u32>,
    pub collision: Grid<u8>,
}

pub struct BuildingWrite {
    pub cx: i32,
    pub cy: i32,
    pub cell_x: f32,
    pub cell_y: f32,
    pub cell_w: f32,
    pub cell_h: f32,
    pub tier: u8,
}

/// Handle to the background writer thread.
pub struct DbWriter {
    tx: Sender<WriteCmd>,
    handle: Option<JoinHandle<Result<()>>>,
}

impl DbWriter {
    pub fn open(path: &std::path::Path) -> Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .with_context(|| format!("opening DB: {}", path.display()))?;

        init_schema(&conn)?;

        let (tx, rx) = mpsc::channel::<WriteCmd>();

        let handle = thread::spawn(move || writer_loop(conn, rx));

        Ok(Self { tx, handle: Some(handle) })
    }

    pub fn send_chunk(&self, cmd: ChunkWrite) -> Result<()> {
        self.tx.send(WriteCmd::Chunk(cmd)).context("send chunk to writer")
    }

    pub fn send_building(&self, b: BuildingWrite) -> Result<()> {
        self.tx.send(WriteCmd::Building(b)).context("send building to writer")
    }

    pub fn send_meta(&self, key: &str, value: &str) -> Result<()> {
        self.tx
            .send(WriteCmd::Meta { key: key.to_owned(), value: value.to_owned() })
            .context("send meta to writer")
    }

    pub fn flush(&self) -> Result<()> {
        self.tx.send(WriteCmd::Flush).context("send flush")
    }

    pub fn shutdown(mut self) -> Result<()> {
        self.tx.send(WriteCmd::Shutdown).context("send shutdown")?;
        if let Some(h) = self.handle.take() {
            h.join().map_err(|_| anyhow::anyhow!("writer thread panicked"))??;
        }
        Ok(())
    }
}

fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         CREATE TABLE IF NOT EXISTS chunks (
             cx        INTEGER NOT NULL,
             cy        INTEGER NOT NULL,
             lod       INTEGER NOT NULL,
             render    BLOB    NOT NULL,
             collision BLOB    NOT NULL,
             w         INTEGER NOT NULL,
             h         INTEGER NOT NULL,
             PRIMARY KEY (cx, cy, lod)
         );
         CREATE TABLE IF NOT EXISTS meta (
             key   TEXT PRIMARY KEY,
             value TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS buildings (
             cx     INTEGER NOT NULL,
             cy     INTEGER NOT NULL,
             cell_x REAL    NOT NULL,
             cell_y REAL    NOT NULL,
             cell_w REAL    NOT NULL,
             cell_h REAL    NOT NULL,
             tier   INTEGER NOT NULL,
             PRIMARY KEY (cx, cy, cell_x, cell_y)
         );",
    )
    .context("init schema")
}

/// A compressed chunk row, buffered until shutdown so chunks are written in a
/// deterministic order. Rayon workers finish in a nondeterministic parallel
/// order, and sqlite's on-disk layout depends on insertion order — sorting by
/// (cx, cy, lod) before writing makes the output tiles.db byte-reproducible.
struct ChunkRow {
    cx: i32,
    cy: i32,
    lod: u32,
    render: Vec<u8>,
    collision: Vec<u8>,
    w: u32,
    h: u32,
}

fn writer_loop(conn: Connection, rx: mpsc::Receiver<WriteCmd>) -> Result<()> {
    let mut chunks: Vec<ChunkRow> = Vec::new();

    for cmd in rx {
        match cmd {
            WriteCmd::Chunk(c) => {
                // Compress on arrival (peak memory stays near the compressed db
                // size); defer the insert to shutdown so insertion order is fixed.
                let render = compress_u32(c.render.as_slice())?;
                let collision = compress_u8(c.collision.as_slice())?;
                chunks.push(ChunkRow {
                    cx: c.coord.cx,
                    cy: c.coord.cy,
                    lod: c.lod,
                    render,
                    collision,
                    w: c.render.width,
                    h: c.render.height,
                });
            }
            WriteCmd::Building(b) => {
                conn.execute(
                    "INSERT OR IGNORE INTO buildings(cx, cy, cell_x, cell_y, cell_w, cell_h, tier)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![b.cx, b.cy, b.cell_x, b.cell_y, b.cell_w, b.cell_h, b.tier],
                )
                .context("write building")?;
            }
            WriteCmd::Meta { key, value } => {
                conn.execute(
                    "INSERT OR REPLACE INTO meta(key, value) VALUES(?1, ?2)",
                    params![key, value],
                )
                .context("write meta")?;
            }
            // Chunks are written in one deterministic, sorted pass at shutdown.
            WriteCmd::Flush => {}
            WriteCmd::Shutdown => break,
        }
    }

    chunks.sort_by_key(|r| (r.cx, r.cy, r.lod));
    write_chunks(&conn, &chunks)
}

fn write_chunks(conn: &Connection, rows: &[ChunkRow]) -> Result<()> {
    for batch in rows.chunks(BATCH_SIZE) {
        let tx = conn.unchecked_transaction().context("begin transaction")?;
        {
            let mut stmt = tx
                .prepare_cached(
                    "INSERT OR REPLACE INTO chunks(cx, cy, lod, render, collision, w, h)
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )
                .context("prepare insert")?;
            for r in batch {
                stmt.execute(params![r.cx, r.cy, r.lod, r.render, r.collision, r.w, r.h])
                    .context("insert chunk")?;
            }
        }
        tx.commit().context("commit transaction")?;
    }
    Ok(())
}

/// Serialize u32 slice as little-endian bytes, then zstd-compress.
fn compress_u32(data: &[u32]) -> Result<Vec<u8>> {
    let bytes: Vec<u8> = data.iter().flat_map(|v| v.to_le_bytes()).collect();
    encode_all(&bytes[..], 3).context("zstd compress u32")
}

/// Serialize u8 slice, then zstd-compress.
fn compress_u8(data: &[u8]) -> Result<Vec<u8>> {
    encode_all(data, 3).context("zstd compress u8")
}

/// Decompress a render BLOB back to a Vec<u32> (for testing / debug reads).
pub fn decompress_u32(blob: &[u8], expected_len: usize) -> Result<Vec<u32>> {
    let bytes = zstd::decode_all(blob).context("zstd decompress u32")?;
    anyhow::ensure!(
        bytes.len() == expected_len * 4,
        "decompress_u32: expected {} bytes, got {}",
        expected_len * 4,
        bytes.len()
    );
    Ok(bytes
        .chunks_exact(4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}

/// Decompress a collision BLOB back to a Vec<u8> (for testing / debug reads).
pub fn decompress_u8(blob: &[u8]) -> Result<Vec<u8>> {
    zstd::decode_all(blob).context("zstd decompress u8")
}
