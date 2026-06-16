/// Self-verification harness for the SF map beautification work.
///
/// Renders the three iconic test chunks to `verify_out/*.png` and runs objective
/// checks V1-V8 (see goal spec), writing a PASS/FAIL report to
/// `verify_out/report.txt`. Run via `tools/verify` (which builds the atlases,
/// runs this twice for determinism, and checks the tiles.db hash baseline).
///
/// Usage: cargo run --release --bin verify [-- --db tiles.db]

use anyhow::{Context, Result};
use clap::Parser;
use sim_maps::{
    db::decompress_u32,
    render::{self, facade_rect, footprint_rect, render_detail_chunk, BuildingRec, DetailRender},
    types::{tile_id_class, tile_id_variant},
};
use image::{DynamicImage, GenericImageView, RgbImage};
use rusqlite::{params, Connection};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::PathBuf;

const ATLAS_TILE: u32 = 32;

/// (cx, cy, label) for the three iconic test chunks (see INTEGRATION.md projection).
const TEST_CHUNKS: [(i32, i32, &str); 4] = [
    (32, 34, "alamo"), // Alamo Square / Painted Ladies — Low Victorians
    (43, 43, "fidi"),  // FiDi / Transamerica — Tall towers
    (46, 43, "ferry"), // Ferry Building / Embarcadero — waterfront + mid-rise
    (43, 35, "soma"),  // SoMa — Market St diagonal (facade shear check)
];

/// Ground classes that have meaningful class boundaries (need autotile variants).
const GROUND_CLASSES: [(u32, &str); 9] = [
    (0, "Grass"),
    (1, "ParkGrass"),
    (2, "Sand"),
    (3, "Path"),
    (4, "Sidewalk"),
    (5, "Road"),
    (10, "Water"),
    (11, "Plaza"),
    (12, "Shoreline"),
];

#[derive(Parser, Debug)]
#[command(name = "verify", about = "Render test chunks and run beautification checks V1-V8")]
struct Cli {
    #[arg(long, default_value = "tiles.db")]
    db: PathBuf,
    /// Load db path, test chunks, and per-city baseline from config/cities/<city>.toml.
    #[arg(long)]
    city: Option<String>,
    #[arg(long, default_value = "assets/atlas.png")]
    atlas: PathBuf,
    #[arg(long, default_value = "assets/facade_atlas.png")]
    facade: PathBuf,
    #[arg(long, default_value = "verify_out")]
    out: PathBuf,
    /// MX master palette PNG (for V5 palette-adherence).
    #[arg(long)]
    palette: Option<PathBuf>,
}

struct Check {
    id: &'static str,
    pass: bool,
    detail: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let palette_path = cli.palette.clone().unwrap_or_else(|| {
        home.join("Downloads/modernexteriors-win/Palette.png")
    });

    std::fs::create_dir_all(&cli.out)?;

    // Resolve db path, test chunks, and baseline from config/cities/<city>.toml
    // when --city is given; otherwise the SF defaults (--db + hardcoded TEST_CHUNKS).
    let (db_path, test_chunks, baseline_path): (PathBuf, Vec<(i32, i32, String)>, PathBuf) =
        if let Some(city) = &cli.city {
            let cfg = sim_maps::config::Config::from_file(&PathBuf::from(format!(
                "config/cities/{city}.toml"
            )))
            .with_context(|| format!("load config/cities/{city}.toml"))?;
            let chunks = cfg
                .verify
                .test_chunks
                .iter()
                .map(|t| (t.cx, t.cy, t.label.clone()))
                .collect();
            (
                cfg.paths.output_db.clone(),
                chunks,
                PathBuf::from(format!("tools/baseline_{city}.sha256")),
            )
        } else {
            (
                cli.db.clone(),
                TEST_CHUNKS.iter().map(|&(cx, cy, l)| (cx, cy, l.to_string())).collect(),
                PathBuf::from("tools/baseline_db.sha256"),
            )
        };

    let atlas = image::open(&cli.atlas).with_context(|| format!("open {}", cli.atlas.display()))?;
    let facade =
        image::open(&cli.facade).with_context(|| format!("open {}", cli.facade.display()))?;
    let conn = Connection::open(&db_path).with_context(|| format!("open {}", db_path.display()))?;

    // Render each test chunk to verify_out/.
    let mut renders: Vec<(String, DetailRender)> = Vec::new();
    for (cx, cy, label) in &test_chunks {
        match render_detail_chunk(&conn, &atlas, &facade, *cx, *cy)? {
            Some(dr) => {
                let path = cli.out.join(format!("{label}_detail.png"));
                dr.img.save(&path).with_context(|| format!("save {}", path.display()))?;
                println!(
                    "rendered {label} ({cx},{cy}) -> {} ({}x{} px, {} buildings)",
                    path.display(),
                    dr.img.width(),
                    dr.img.height(),
                    dr.buildings.len()
                );
                renders.push((label.to_string(), dr));
            }
            None => println!("WARNING: chunk {label} ({cx},{cy}) not found at LOD 0"),
        }
    }

    let palette = load_palette(&palette_path)?;
    println!("palette: {} unique colors from {}", palette.len(), palette_path.display());

    let mut checks: Vec<Check> = Vec::new();
    checks.push(check_v1_diversity(&atlas));
    checks.push(check_v2_distinctness(&renders));
    checks.push(check_v3_facade_depth(&renders));
    checks.push(check_v4_holes(&conn, &atlas));
    checks.push(check_v5_palette(&renders, &palette));
    checks.push(check_v6_db_integrity(&db_path, &baseline_path));
    checks.push(check_v7_determinism(&conn, &atlas, &facade, &test_chunks));
    checks.push(check_v8_format(&atlas, &facade));

    // Report.
    let mut report = String::new();
    writeln!(report, "=== SF map beautification verification report ===\n")?;
    let mut all_pass = true;
    for c in &checks {
        let tag = if c.pass { "PASS" } else { "FAIL" };
        if !c.pass {
            all_pass = false;
        }
        writeln!(report, "[{tag}] {}: {}", c.id, c.detail)?;
    }
    writeln!(report, "\nOVERALL: {}", if all_pass { "ALL PASS" } else { "FAILURES PRESENT" })?;

    let report_path = cli.out.join("report.txt");
    std::fs::write(&report_path, &report)?;
    print!("\n{report}");
    println!("report written: {}", report_path.display());

    Ok(())
}

/// V1 — autotile diversity: each ground class must have >=20 of its 47 variant
/// sub-tiles be pixel-distinct in atlas.png.
fn check_v1_diversity(atlas: &DynamicImage) -> Check {
    let mut worst: Vec<String> = Vec::new();
    let mut min_distinct = u32::MAX;
    let mut pass = true;
    for (class, name) in GROUND_CLASSES {
        let mut seen: HashSet<Vec<u8>> = HashSet::new();
        for variant in 0..47u32 {
            seen.insert(tile_bytes(atlas, variant * ATLAS_TILE, class * ATLAS_TILE));
        }
        let distinct = seen.len() as u32;
        min_distinct = min_distinct.min(distinct);
        if distinct < 20 {
            pass = false;
            worst.push(format!("{name}={distinct}"));
        }
    }
    let detail = if pass {
        format!("all ground classes have >=20 distinct variants (min observed {min_distinct}/47)")
    } else {
        format!(
            "classes with <20 distinct variants: [{}] (need >=20/47)",
            worst.join(", ")
        )
    };
    Check { id: "V1 autotile-diversity", pass, detail }
}

/// V2 — building distinctness. The goal spec describes per-pixel connected
/// components, but MX roofs/windows are richly textured, so naive pixel CC
/// shatters each building into thousands of fragments (C >> N) and gates nothing.
/// We implement the stated INTENT instead — "flat blobs give C << N, separated
/// buildings give C ~= N" — as BUILDING-level connected components: nodes are
/// footprints, two touching footprints merge iff their dominant roof colors are
/// within tolerance. Uniform-pink rowhouses merge into a few blobs (C << N);
/// per-building color variants keep neighbours distinct (C ~= N).
fn check_v2_distinctness(renders: &[(String, DetailRender)]) -> Check {
    const TOL: i32 = 18; // per-channel tolerance: roofs within this read as the same colour
    let mut lines: Vec<String> = Vec::new();
    let mut densest: Option<(f64, String, u32, u32)> = None;
    let mut densest_n = 0u32;

    for (label, dr) in renders {
        let n = dr.buildings.len() as u32;
        if n == 0 {
            lines.push(format!("{label}: N=0 (no buildings)"));
            continue;
        }
        let (c, _) = building_distinctness(dr, TOL);
        let ratio = c as f64 / n as f64;
        lines.push(format!("{label}: C={c} N={n} C/N={ratio:.2}"));
        if n > densest_n {
            densest_n = n;
            densest = Some((ratio, label.clone(), c, n));
        }
    }

    let (pass, detail) = match densest {
        Some((ratio, label, c, n)) => {
            let pass = ratio >= 0.7;
            (
                pass,
                format!(
                    "densest={label} C={c} N={n} C/N={ratio:.2} (need >=0.70) | {}",
                    lines.join("; ")
                ),
            )
        }
        None => (false, "no buildings in any test chunk".to_string()),
    };
    Check { id: "V2 building-distinctness", pass, detail }
}

/// V3 — facade depth: facades drawn below south edge with content, height Tall>Mid>Low.
fn check_v3_facade_depth(renders: &[(String, DetailRender)]) -> Check {
    let (lo, mid, hi) = (
        render::facade_height_px(0),
        render::facade_height_px(1),
        render::facade_height_px(2),
    );
    let monotonic = hi > mid && mid > lo;

    // Per tier, find max fraction of facade-rect pixels that are non-background
    // across all buildings of that tier (proves facades actually render content).
    let mut content_ok = true;
    let mut content_notes: Vec<String> = Vec::new();
    for (label, dr) in renders {
        let bg = background_color(&dr.img);
        for tier in 0u8..3 {
            let mut best = 0.0f64;
            let mut count = 0u32;
            for b in dr.buildings.iter().filter(|b| b.tier == tier) {
                count += 1;
                let frac = facade_content_fraction(dr, b, bg);
                if frac > best {
                    best = frac;
                }
            }
            if count > 0 {
                content_notes.push(format!("{label} t{tier}: n={count} maxfill={best:.2}"));
                if best < 0.10 {
                    content_ok = false;
                }
            }
        }
    }

    let pass = monotonic && content_ok;
    let detail = format!(
        "heights(px) Low={lo} Mid={mid} Tall={hi} monotonic={monotonic}; content_ok={content_ok} [{}]",
        content_notes.join(", ")
    );
    Check { id: "V3 facade-depth", pass, detail }
}

/// V4 — no holes: every (class,variant) referenced in the test chunks resolves to
/// a non-empty atlas tile, and rendered output has no magenta sentinel pixels.
fn check_v4_holes(conn: &Connection, atlas: &DynamicImage) -> Check {
    let mut referenced: HashSet<(u32, u32)> = HashSet::new();
    for (cx, cy, _) in TEST_CHUNKS {
        if let Ok((blob, w, h)) = chunk_render(conn, cx, cy) {
            if let Ok(tiles) = decompress_u32(&blob, (w * h) as usize) {
                for t in tiles {
                    referenced.insert((tile_id_class(t) as u32, tile_id_variant(t) as u32));
                }
            }
        }
    }
    let mut empty: Vec<String> = Vec::new();
    for &(class, variant) in &referenced {
        if atlas_tile_empty(atlas, variant.min(46) * ATLAS_TILE, class * ATLAS_TILE) {
            empty.push(format!("c{class}v{variant}"));
        }
    }
    let pass = empty.is_empty();
    let detail = if pass {
        format!("all {} referenced (class,variant) tiles are non-empty", referenced.len())
    } else {
        format!("empty atlas tiles referenced: [{}]", empty.join(", "))
    };
    Check { id: "V4 no-holes", pass, detail }
}

/// V5 — palette adherence: >=95% of rendered pixels within a small distance of an
/// MX palette color.
fn check_v5_palette(renders: &[(String, DetailRender)], palette: &[(u8, u8, u8)]) -> Check {
    const THRESH_SQ: i32 = 900; // ~per-channel 17
    let mut lines: Vec<String> = Vec::new();
    let mut pass = true;
    let mut cache: HashMap<(u8, u8, u8), bool> = HashMap::new();

    for (label, dr) in renders {
        let mut total = 0u64;
        let mut within = 0u64;
        for px in dr.img.pixels() {
            let key = (px[0], px[1], px[2]);
            let ok = *cache
                .entry(key)
                .or_insert_with(|| nearest_within(key, palette, THRESH_SQ));
            total += 1;
            if ok {
                within += 1;
            }
        }
        let frac = within as f64 / total.max(1) as f64;
        lines.push(format!("{label}={:.1}%", frac * 100.0));
        if frac < 0.95 {
            pass = false;
        }
    }
    Check {
        id: "V5 palette-adherence",
        pass,
        detail: format!("within-palette fraction (need >=95%): {}", lines.join(", ")),
    }
}

/// V6 — DB integrity: sha256(tiles.db) unchanged vs tools/baseline_db.sha256.
fn check_v6_db_integrity(db: &PathBuf, baseline_path: &PathBuf) -> Check {
    let baseline = std::fs::read_to_string(baseline_path)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let current = sha256_file(db).unwrap_or_default();
    let pass = !baseline.is_empty() && baseline == current;
    let detail = if pass {
        format!("tiles.db sha256 matches baseline ({})", &baseline[..baseline.len().min(12)])
    } else if baseline.is_empty() {
        format!("no baseline at {} (current={})", baseline_path.display(), short(&current))
    } else {
        format!("baseline={} current={}", short(&baseline), short(&current))
    };
    Check { id: "V6 db-integrity", pass, detail }
}

/// V7 — determinism: rendering a chunk twice yields byte-identical pixels.
fn check_v7_determinism(
    conn: &Connection,
    atlas: &DynamicImage,
    facade: &DynamicImage,
    test_chunks: &[(i32, i32, String)],
) -> Check {
    let Some((cx, cy, label)) = test_chunks.first() else {
        return Check {
            id: "V7 determinism",
            pass: true,
            detail: "skipped (no test_chunks configured)".to_string(),
        };
    };
    let a = render_detail_chunk(conn, atlas, facade, *cx, *cy);
    let b = render_detail_chunk(conn, atlas, facade, *cx, *cy);
    let pass = match (a, b) {
        (Ok(Some(ra)), Ok(Some(rb))) => ra.img.as_raw() == rb.img.as_raw(),
        _ => false,
    };
    Check {
        id: "V7 determinism",
        pass,
        detail: if pass {
            format!("two consecutive renders of {label} are byte-identical")
        } else {
            "renders differ or chunk missing".to_string()
        },
    }
}

/// V8 — format: atlas.png exactly 1504x480; facade_atlas dims tile-aligned.
fn check_v8_format(atlas: &DynamicImage, facade: &DynamicImage) -> Check {
    let (aw, ah) = (atlas.width(), atlas.height());
    let (fw, fh) = (facade.width(), facade.height());
    let atlas_ok = aw == 1504 && ah == 480;
    let facade_ok = fw % 32 == 0 && fh % 32 == 0 && fw > 0 && fh > 0;
    let pass = atlas_ok && facade_ok;
    Check {
        id: "V8 format",
        pass,
        detail: format!(
            "atlas={aw}x{ah} (need 1504x480: {atlas_ok}); facade={fw}x{fh} (tile-aligned: {facade_ok})"
        ),
    }
}

// ---- helpers ----

fn chunk_render(conn: &Connection, cx: i32, cy: i32) -> Result<(Vec<u8>, u32, u32)> {
    let mut stmt = conn.prepare("SELECT render, w, h FROM chunks WHERE cx=?1 AND cy=?2 AND lod=0")?;
    let r = stmt.query_row(params![cx, cy], |row| {
        Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, u32>(1)?, row.get::<_, u32>(2)?))
    })?;
    Ok(r)
}

fn tile_bytes(atlas: &DynamicImage, x0: u32, y0: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity((ATLAS_TILE * ATLAS_TILE * 4) as usize);
    for y in 0..ATLAS_TILE {
        for x in 0..ATLAS_TILE {
            if x0 + x < atlas.width() && y0 + y < atlas.height() {
                let p = atlas.get_pixel(x0 + x, y0 + y);
                v.extend_from_slice(&[p[0], p[1], p[2], p[3]]);
            } else {
                v.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    v
}

fn atlas_tile_empty(atlas: &DynamicImage, x0: u32, y0: u32) -> bool {
    for y in 0..ATLAS_TILE {
        for x in 0..ATLAS_TILE {
            if x0 + x < atlas.width() && y0 + y < atlas.height() {
                let p = atlas.get_pixel(x0 + x, y0 + y);
                if p[3] > 0 {
                    return false;
                }
            }
        }
    }
    true
}

fn background_color(img: &RgbImage) -> [u8; 3] {
    // Corner pixel is grass background in every detail render.
    let p = img.get_pixel(0, 0);
    [p[0], p[1], p[2]]
}

/// Fraction of a building's facade-rect pixels that differ from background.
fn facade_content_fraction(dr: &DetailRender, b: &BuildingRec, bg: [u8; 3]) -> f64 {
    let (x, y, w, h) = facade_rect(b, dr.h_cells);
    let (iw, ih) = (dr.img.width(), dr.img.height());
    let mut total = 0u64;
    let mut non_bg = 0u64;
    for py in y..(y + h).min(ih) {
        for px in x..(x + w).min(iw) {
            total += 1;
            let p = dr.img.get_pixel(px, py);
            if (p[0] as i32 - bg[0] as i32).abs() > 10
                || (p[1] as i32 - bg[1] as i32).abs() > 10
                || (p[2] as i32 - bg[2] as i32).abs() > 10
            {
                non_bg += 1;
            }
        }
    }
    if total == 0 {
        0.0
    } else {
        non_bg as f64 / total as f64
    }
}

/// Building-level connected components: (C = distinct visual groups, N = building count).
/// For every shared boundary between two footprints, we sample the pixels on each
/// side: they "connect" if the colors are within `tol` (continuous roof) and form a
/// "seam" otherwise (a color change or a baked dark edge/shadow). Two buildings
/// merge into one visual group only if the MAJORITY of their shared boundary
/// connects. Uniform abutting rooftops (no edge, same color) merge → C << N (the
/// flat-blob failure); per-building colors + baked roof edges keep neighbours
/// separated → C ~= N.
fn building_distinctness(dr: &DetailRender, tol: i32) -> (u32, u32) {
    let n = dr.buildings.len();
    if n == 0 {
        return (0, 0);
    }
    let (w, h) = (dr.img.width() as usize, dr.img.height() as usize);

    // Label each footprint pixel with its building index (painter order: later wins).
    let mut labels = vec![-1i32; w * h];
    for (i, b) in dr.buildings.iter().enumerate() {
        let (rx, ry, rw, rh) = footprint_rect(b, dr.h_cells);
        let x1 = (rx + rw).min(w as u32);
        let y1 = (ry + rh).min(h as u32);
        for y in ry..y1 {
            for x in rx..x1 {
                labels[y as usize * w + x as usize] = i as i32;
            }
        }
    }

    // Tally connect/total boundary pixels per building pair.
    let mut pairs: HashMap<(u32, u32), (u32, u32)> = HashMap::new();
    let mut tally = |a: i32, b: i32, pa: &image::Rgb<u8>, pb: &image::Rgb<u8>| {
        let (lo, hi) = (a.min(b) as u32, a.max(b) as u32);
        let e = pairs.entry((lo, hi)).or_insert((0, 0));
        e.1 += 1;
        if (pa[0] as i32 - pb[0] as i32).abs() <= tol
            && (pa[1] as i32 - pb[1] as i32).abs() <= tol
            && (pa[2] as i32 - pb[2] as i32).abs() <= tol
        {
            e.0 += 1;
        }
    };
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let li = labels[i];
            if li < 0 {
                continue;
            }
            if x + 1 < w {
                let r = labels[i + 1];
                if r >= 0 && r != li {
                    tally(li, r, dr.img.get_pixel(x as u32, y as u32), dr.img.get_pixel(x as u32 + 1, y as u32));
                }
            }
            if y + 1 < h {
                let d = labels[i + w];
                if d >= 0 && d != li {
                    tally(li, d, dr.img.get_pixel(x as u32, y as u32), dr.img.get_pixel(x as u32, y as u32 + 1));
                }
            }
        }
    }

    let mut uf = UnionFind::new(n);
    let mut adj_pairs = 0u32;
    let mut merged = 0u32;
    let mut samples: Vec<String> = Vec::new();
    for ((a, b), (connect, total)) in pairs {
        adj_pairs += 1;
        // Merge only if the shared boundary is mostly continuous (no seam/edge).
        if total > 0 && connect * 2 > total {
            uf.union(a as usize, b as usize);
            merged += 1;
            if samples.len() < 8 {
                samples.push(format!("({a},{b}):{connect}/{total}"));
            }
        }
    }
    if std::env::var("V2_DEBUG").is_ok() {
        eprintln!(
            "V2 debug: N={n} adjacent_pairs={adj_pairs} merged_pairs={merged} samples=[{}]",
            samples.join(" ")
        );
    }

    let mut roots: HashSet<usize> = HashSet::new();
    for i in 0..n {
        roots.insert(uf.find(i));
    }
    (roots.len() as u32, n as u32)
}

struct UnionFind {
    parent: Vec<u32>,
}
impl UnionFind {
    fn new(n: usize) -> Self {
        Self { parent: (0..n as u32).collect() }
    }
    fn find(&mut self, x: usize) -> usize {
        let mut r = x;
        while self.parent[r] as usize != r {
            r = self.parent[r] as usize;
        }
        // path compression
        let mut c = x;
        while self.parent[c] as usize != r {
            let next = self.parent[c] as usize;
            self.parent[c] = r as u32;
            c = next;
        }
        r
    }
    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[ra] = rb as u32;
        }
    }
}

fn load_palette(path: &PathBuf) -> Result<Vec<(u8, u8, u8)>> {
    let img = image::open(path).with_context(|| format!("open palette {}", path.display()))?;
    let mut set: HashSet<(u8, u8, u8)> = HashSet::new();
    for (_, _, p) in img.pixels() {
        if p[3] > 0 {
            set.insert((p[0], p[1], p[2]));
        }
    }
    Ok(set.into_iter().collect())
}

fn nearest_within(c: (u8, u8, u8), palette: &[(u8, u8, u8)], thresh_sq: i32) -> bool {
    for &(r, g, b) in palette {
        let dr = c.0 as i32 - r as i32;
        let dg = c.1 as i32 - g as i32;
        let db = c.2 as i32 - b as i32;
        if dr * dr + dg * dg + db * db <= thresh_sq {
            return true;
        }
    }
    false
}

fn sha256_file(path: &PathBuf) -> Option<String> {
    let out = std::process::Command::new("shasum")
        .arg("-a")
        .arg("256")
        .arg(path)
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    s.split_whitespace().next().map(|x| x.to_string())
}

fn short(s: &str) -> String {
    if s.is_empty() {
        "<none>".to_string()
    } else {
        s[..s.len().min(12)].to_string()
    }
}
