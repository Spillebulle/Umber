//! Read a `.clip`'s schema out of the file and say what every layer really is.
//!
//! ```sh
//! cargo run --release -p umber-core --example survey-clip-schema -- a.clip
//! ```
//!
//! Written to settle one report — a 45 MB `.clip` refused as "contains no
//! layers", which reads as a corrupt file and is not one. `survey-documents`
//! says a document was refused and `survey-residency` says which slices could
//! not be measured; neither can answer the question those two raise, which is
//! *what the file holds instead*. Answering it by guessing table names would
//! have been the wrong instrument twice over: a name guessed wrong reads back
//! as "no such thing in the file", and "does Clip Studio cache a rasterisation
//! anywhere" is exactly a question a guessed list cannot answer. So this walks
//! `sqlite_master` ([`Database::table_names`]) and prints what is there.
//!
//! Four things per layer, because between them they decide what Umber can do:
//!
//! * `LayerType` and `SpecialRenderType`, which is what
//!   [`docimport::clipstudio`] reads a vector layer and a Paper sheet off.
//! * **Every level of the mipmap chain**, not only the base one the reader
//!   follows. The reader deliberately takes `BaseMipmapInfo` and stops, so a
//!   layer whose base holds nothing might still have a smaller level that does
//!   — and if it did, the refusal below would be the wrong answer. This is the
//!   column that says it does not.
//! * Whether each level's `Offscreen` names an external chunk **the container
//!   actually holds**, since that is where a real vector layer fails: it has
//!   the whole chain and the chunk is simply absent.
//! * The layer's own `LayerRenderThumbnail` / `LayerThumbnail`, which is the
//!   only raster Clip Studio keeps for a vector layer and is nowhere near a
//!   layer's size.
//!
//! Kept rather than thrown away, for `survey-sut`'s reason: a figure nobody can
//! re-derive is a figure that goes stale, and the next "why will this file not
//! open" arrives eventually. It decodes nothing and allocates nothing per
//! canvas — it reads the database and the chunk directory, and prints.

use std::path::PathBuf;

use umber_core::sqlite::{Database, Table, Value};

/// The chunk directory: which `extrnlid…` names the container holds, and how
/// many bytes each carries.
///
/// A second copy of `clipstudio::split`'s walk, deliberately: that one is
/// `pub(super)` and returns borrowed slices tied to a private type, and the
/// alternative — widening a reader's API so an example can see inside it — is
/// the drift this codebase refuses elsewhere. What is duplicated is thirty
/// lines of framing that no pixel depends on; nothing here decides a pixel.
fn chunks(bytes: &[u8]) -> (Vec<(String, usize)>, Option<(usize, usize)>) {
    let be64 = |at: usize| -> Option<usize> {
        let raw = bytes.get(at..at.checked_add(8)?)?;
        usize::try_from(u64::from_be_bytes(raw.try_into().ok()?)).ok()
    };
    let mut external = Vec::new();
    let mut database = None;
    let mut at = 24;
    while at + 16 <= bytes.len() {
        let tag = &bytes[at..at + 8];
        let Some(size) = be64(at + 8) else { break };
        let body = at + 16;
        let Some(end) = body.checked_add(size).filter(|e| *e <= bytes.len()) else {
            break;
        };
        match tag {
            b"CHNKExta" => {
                if let Some(name_len) = be64(body).filter(|n| *n > 0 && *n <= 256)
                    && let Some(id) = bytes.get(body + 8..body + 8 + name_len)
                    && let Some(data_len) = be64(body + 8 + name_len)
                {
                    external.push((String::from_utf8_lossy(id).into_owned(), data_len));
                }
            }
            b"CHNKSQLi" => database = Some((body, end)),
            b"CHNKFoot" => break,
            _ => {}
        }
        at = end;
    }
    (external, database)
}

/// A column of a row, by name, as an integer. `0` where the column is absent —
/// which is what the reader does, and the point is to print what it sees.
fn int(table: &Table, row: &umber_core::sqlite::Row, name: &str) -> i64 {
    table
        .column(name)
        .map(|i| row.get(i))
        .and_then(Value::as_i64)
        .unwrap_or(0)
}

fn text(table: &Table, row: &umber_core::sqlite::Row, name: &str) -> String {
    table
        .column(name)
        .map(|i| row.get(i))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// How big a blob a column holds, or `None` where it holds no blob at all.
fn blob_len(table: &Table, row: &umber_core::sqlite::Row, name: &str) -> Option<usize> {
    table
        .column(name)
        .map(|i| row.get(i))
        .and_then(Value::as_blob)
        .map(<[u8]>::len)
}

fn main() {
    let Some(path) = std::env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: survey-clip-schema <file.clip>");
        std::process::exit(2);
    };
    let bytes = std::fs::read(&path).expect("the file could be read");
    println!("{} — {:.1} MB\n", path.display(), bytes.len() as f64 / 1e6);

    let (external, database) = chunks(&bytes);
    let (start, end) = database.expect("it holds a CHNKSQLi chunk");
    let db = Database::open(&bytes[start..end]).expect("the database opened");

    // ---- the schema, read rather than guessed --------------------------
    let names = db.table_names().expect("its schema could be read");
    println!("{} tables:", names.len());
    for name in &names {
        let Ok(Some(table)) = db.table(name) else {
            continue;
        };
        let rows = db.rows(&table).map(|r| r.len()).unwrap_or(0);
        println!("  {name:<28} {rows:>6} rows, {} columns", table.columns().len());
    }
    println!();

    // ---- the chunk directory -------------------------------------------
    println!(
        "{} external chunks, {:.1} MB",
        external.len(),
        external.iter().map(|(_, n)| *n).sum::<usize>() as f64 / 1e6
    );
    for (id, len) in &external {
        println!("  {id}  {len} bytes");
    }
    // Which table and column each chunk belongs to, which is what says whether
    // the file's bulk is layer pixels or something else entirely.
    for name in ["ExternalChunk", "ExternalTableAndColumnName"] {
        let Ok(Some(table)) = db.table(name) else {
            continue;
        };
        let Ok(rows) = db.rows(&table) else { continue };
        println!("\n{name} ({:?}):", table.columns());
        for row in &rows {
            let cells: Vec<String> = (0..table.columns().len())
                .map(|i| match row.get(i) {
                    Value::Blob(b) => String::from_utf8_lossy(b).into_owned(),
                    v => format!("{v:?}"),
                })
                .collect();
            println!("  {}", cells.join("  |  "));
        }
    }
    println!();
    let held: std::collections::HashSet<&str> =
        external.iter().map(|(id, _)| id.as_str()).collect();

    // ---- the four-table chain, every level ------------------------------
    let mipmap = db.table("Mipmap").ok().flatten();
    let info = db.table("MipmapInfo").ok().flatten();
    let offscreen = db.table("Offscreen").ok().flatten();
    if let Some(t) = &info {
        println!("MipmapInfo columns: {:?}", t.columns());
    }
    if let Some(t) = &offscreen {
        println!("Offscreen columns: {:?}", t.columns());
    }
    println!();

    let rows_of = |t: &Option<Table>| -> Vec<umber_core::sqlite::Row> {
        t.as_ref()
            .and_then(|t| db.rows(t).ok())
            .unwrap_or_default()
    };
    let mipmap_rows = rows_of(&mipmap);
    let info_rows = rows_of(&info);
    let offscreen_rows = rows_of(&offscreen);

    // `Mipmap.MainId` -> `BaseMipmapInfo`, and `MipmapInfo.MainId` -> the row.
    let base_of = |id: i64| -> Option<i64> {
        let t = mipmap.as_ref()?;
        mipmap_rows
            .iter()
            .find(|r| int(t, r, "MainId") == id)
            .map(|r| int(t, r, "BaseMipmapInfo"))
    };

    /// One level of a chain: which `Offscreen` it names and whether the chunk
    /// that offscreen points at is in the container.
    struct Level {
        info: i64,
        offscreen: i64,
        block: String,
        present: bool,
        /// `MipmapInfo.ThisScale`: 100 is the base level, 50 is half, and so on.
        scale: i64,
        /// How many bytes the `Offscreen`'s `Attribute` blob holds. A level with
        /// no attribute has no bitmap described at all.
        attribute: usize,
    }

    let chain = |mipmap_id: i64| -> Vec<Level> {
        let (Some(it), Some(ot)) = (info.as_ref(), offscreen.as_ref()) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut at = base_of(mipmap_id).unwrap_or(0);
        let mut guard = 0;
        while at != 0 && guard < 32 {
            guard += 1;
            let Some(row) = info_rows.iter().find(|r| int(it, r, "MainId") == at) else {
                break;
            };
            let off = int(it, row, "Offscreen");
            let (block, present) = offscreen_rows
                .iter()
                .find(|r| int(ot, r, "MainId") == off)
                .map(|r| {
                    // **`BlockData` is a blob, not text.** Reading it with
                    // `as_str` answers the empty string, which compares equal
                    // to nothing in the chunk directory and so reports every
                    // level as ABSENT — a confident wrong answer to the one
                    // question this example exists to ask.
                    let name = ot
                        .column("BlockData")
                        .map(|i| r.get(i))
                        .and_then(Value::as_blob)
                        .map(|b| String::from_utf8_lossy(b).into_owned())
                        .unwrap_or_default();
                    let present = held.contains(name.as_str());
                    (name, present)
                })
                .unwrap_or_else(|| (String::new(), false));
            out.push(Level {
                info: at,
                offscreen: off,
                block,
                present,
                scale: int(it, row, "ThisScale"),
                attribute: offscreen_rows
                    .iter()
                    .find(|r| int(ot, r, "MainId") == off)
                    .and_then(|r| blob_len(ot, r, "Attribute"))
                    .unwrap_or(0),
            });
            // The chain's own link. `NextIndex` is what a `MipmapInfo` uses to
            // name the next, smaller level; a schema without it stops here,
            // which is the honest answer rather than a guess at another name.
            at = int(it, row, "NextIndex");
        }
        out
    };

    // ---- the layers ------------------------------------------------------
    let Ok(Some(layers)) = db.table("Layer") else {
        println!("no Layer table");
        return;
    };
    println!("Layer columns ({}): {:?}\n", layers.columns().len(), layers.columns());
    let layer_rows = db.rows(&layers).expect("its Layer table could be read");

    for row in &layer_rows {
        let id = int(&layers, row, "MainId");
        println!(
            "layer {id}  {:?}\n  LayerType={}  LayerFolder={}  SpecialRenderType={}  \
             LayerVisibility={}  LayerOpacity={}",
            text(&layers, row, "LayerName"),
            int(&layers, row, "LayerType"),
            int(&layers, row, "LayerFolder"),
            int(&layers, row, "SpecialRenderType"),
            int(&layers, row, "LayerVisibility"),
            int(&layers, row, "LayerOpacity"),
        );
        // **Every non-zero column, printed.** The first draft of this example
        // filtered to columns whose name mentioned a vector or a thumbnail, and
        // that filter is what hid `ResizableOriginalMipmap` — the column this
        // whole investigation turned on. A survey of six rows can afford to
        // print everything, and a filter over somebody else's schema is the
        // guess this example exists to avoid making.
        let extras: Vec<String> = layers
            .columns()
            .iter()
            .filter(|c| *c != "LayerName")
            .filter_map(|c| match blob_len(&layers, row, c) {
                Some(n) => Some(format!("{c}=<blob {n}B>")),
                None => match int(&layers, row, c) {
                    0 => None,
                    v => Some(format!("{c}={v}")),
                },
            })
            .collect();
        println!("  {}", extras.join("  "));
        for (label, column) in [
            ("render", "LayerRenderMipmap"),
            ("mask", "LayerLayerMaskMipmap"),
            // **Not a mipmap the reader follows.** An imported-image layer keeps
            // the picture it was made from here, at full resolution, so that
            // Clip Studio can re-rasterise it at any scale. If a layer with no
            // render bitmap has one of these, the pixels are in the file after
            // all — which is the difference between refusing the document and
            // opening it.
            ("resizable original", "ResizableOriginalMipmap"),
        ] {
            let m = int(&layers, row, column);
            if m == 0 {
                println!("  {label}: names no mipmap");
                continue;
            }
            let levels = chain(m);
            if levels.is_empty() {
                println!("  {label}: mipmap {m}, chain empty");
                continue;
            }
            println!("  {label}: mipmap {m}, {} level(s)", levels.len());
            for (i, l) in levels.iter().enumerate() {
                println!(
                    "    [{i}] scale={:<4} MipmapInfo={} Offscreen={} Attribute={}B \
                     BlockData={:?} chunk={}",
                    l.scale,
                    l.info,
                    l.offscreen,
                    l.attribute,
                    l.block,
                    if l.present { "PRESENT" } else { "ABSENT" },
                );
            }
        }
        println!();
    }

    // ---- who names each chunk the container actually holds ----------------
    //
    // The decisive question, and the one no guess could have answered. A layer
    // whose every mipmap level names an absent chunk has no pixels in the file;
    // whether the file nonetheless holds *something* usable depends entirely on
    // what the chunks it does hold are attached to. So this asks the file:
    // every table, every column, every row, matched against the ten ids.
    println!("what names each held chunk:");
    for (id, len) in &external {
        let mut found = Vec::new();
        for name in &names {
            let Ok(Some(table)) = db.table(name) else {
                continue;
            };
            let Ok(rows) = db.rows(&table) else { continue };
            for (r, row) in rows.iter().enumerate() {
                for (c, column) in table.columns().iter().enumerate() {
                    let hit = match row.get(c) {
                        Value::Blob(b) => String::from_utf8_lossy(b) == id.as_str(),
                        Value::Text(t) => t == id,
                        _ => false,
                    };
                    if hit {
                        found.push(format!("{name}.{column} (row {r})"));
                    }
                }
            }
        }
        println!(
            "  {id} {len:>9}B  <- {}",
            if found.is_empty() {
                "NOTHING IN THE DATABASE".to_string()
            } else {
                found.join(", ")
            }
        );
    }
    println!();

    // ---- anything else that might hold a raster --------------------------
    // Every table whose name suggests a picture, with the size of the largest
    // blob in it. This is the "is there a cached rasterisation anywhere"
    // question asked over the whole file rather than over a guess.
    println!("blob-bearing tables:");
    for name in &names {
        let Ok(Some(table)) = db.table(name) else {
            continue;
        };
        let Ok(rows) = db.rows(&table) else { continue };
        for column in table.columns() {
            let biggest = rows
                .iter()
                .filter_map(|r| blob_len(&table, r, column))
                .max()
                .unwrap_or(0);
            if biggest > 0 {
                println!("  {name}.{column}: largest blob {biggest} bytes");
            }
        }
    }
}
