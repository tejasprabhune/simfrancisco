/// Phase 1 integration tests: BLOB round-trip and chunk schema.
use sim_maps::{
    db::{ChunkWrite, DbWriter, decompress_u32, decompress_u8},
    types::{ChunkCoord, Grid, SemanticClass, collision, make_tile_id, tile_id_class},
};
use tempfile::NamedTempFile;

fn make_render(w: u32, h: u32) -> Grid<u32> {
    let mut g = Grid::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let cls = if (x + y) % 2 == 0 { SemanticClass::Grass } else { SemanticClass::Water };
            g.set(x, y, make_tile_id(cls, 0));
        }
    }
    g
}

fn make_collision(w: u32, h: u32) -> Grid<u8> {
    let mut g: Grid<u8> = Grid::filled(w, h, collision::WALKABLE);
    g.set(0, 0, collision::BLOCKED);
    g
}

#[test]
fn blob_round_trip() {
    let tmp = NamedTempFile::new().unwrap();
    let writer = DbWriter::open(tmp.path()).unwrap();

    let w = 32u32;
    let h = 32u32;
    writer
        .send_chunk(ChunkWrite {
            coord: ChunkCoord { cx: 3, cy: 7 },
            lod: 0,
            render: make_render(w, h),
            collision: make_collision(w, h),
        })
        .unwrap();
    writer.shutdown().unwrap();

    use rusqlite::{Connection, params};
    let conn = Connection::open(tmp.path()).unwrap();
    let (render_blob, collision_blob, dw, dh): (Vec<u8>, Vec<u8>, u32, u32) = conn
        .query_row(
            "SELECT render, collision, w, h FROM chunks WHERE cx=3 AND cy=7 AND lod=0",
            params![],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();

    assert_eq!(dw, w);
    assert_eq!(dh, h);

    let render = decompress_u32(&render_blob, (w * h) as usize).unwrap();
    let coll = decompress_u8(&collision_blob).unwrap();

    assert_eq!(render.len(), (w * h) as usize);
    assert_eq!(coll.len(), (w * h) as usize);

    // (0,0) → Grass, (1,0) → Water.
    assert_eq!(tile_id_class(render[0]), SemanticClass::Grass as u8);
    assert_eq!(tile_id_class(render[1]), SemanticClass::Water as u8);
    // Cell (0,0) must be blocked.
    assert_eq!(coll[0], collision::BLOCKED);
    // Cell (1,0) must be walkable.
    assert_eq!(coll[1], collision::WALKABLE);
}

#[test]
fn multi_lod_batch() {
    let tmp = NamedTempFile::new().unwrap();
    let writer = DbWriter::open(tmp.path()).unwrap();

    for lod in 0u32..3 {
        let side = 125u32 >> lod;
        writer
            .send_chunk(ChunkWrite {
                coord: ChunkCoord { cx: 0, cy: 0 },
                lod,
                render: make_render(side, side),
                collision: make_collision(side, side),
            })
            .unwrap();
    }
    writer.shutdown().unwrap();

    use rusqlite::Connection;
    let conn = Connection::open(tmp.path()).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM chunks WHERE cx=0 AND cy=0", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 3, "expected 3 LOD rows");
}
