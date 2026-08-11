//! A read-only reader for the SQLite file format.
//!
//! Clip Studio Paint stores a brush (`.sut`) and a brush group (`.sutg`) as an
//! ordinary SQLite database — see [`crate::brushimport::clipstudio`]. Reading
//! one needs a table scan and nothing else: no SQL, no indexes, no writing, no
//! transactions, no WAL.
//!
//! # Why this is not a dependency
//!
//! The obvious crate is `rusqlite`, and with the `bundled` feature it compiles
//! SQLite's amalgamation — a C toolchain in the build. Umber cross-compiles to
//! aarch64 and builds inside a Flatpak sandbox, and `CLAUDE.md` already records
//! the same decision once: `ureq` is built with `rustls` and no default
//! features precisely so that a release build never has to satisfy OpenSSL on
//! those two paths. A C library would be that problem again, and worse — it
//! would only be discovered when a release was cut, because the desktop build
//! everybody develops against has a C compiler.
//!
//! Against that, what is actually needed here is small and completely
//! specified. The on-disk format is documented and frozen: a file header, a
//! b-tree of pages per table, and a record encoding of variable-length
//! integers and typed columns. This module is a few hundred lines of it, it
//! adds no dependency at all, and — being ours — it is testable from fixtures
//! this crate builds itself. That is the same argument `crate::time` makes for
//! not taking a date crate.
//!
//! # What it deliberately does not do
//!
//! Only **table** b-trees are walked, in rowid order, whole. Index pages are
//! refused rather than followed: nothing here looks a row up, so an index is
//! only another way to reach rows this already returns. The rowid alias
//! (`INTEGER PRIMARY KEY`) is not substituted for the `NULL` that is stored in
//! its place, because no caller reads one. The freelist and the pointer map are
//! ignored, which is correct for a reader: a page on the freelist is not
//! reachable from any table's root.
//!
//! Everything is bounds-checked and every loop is bounded by the page count.
//! These are files a stranger wrote.

use std::collections::BTreeSet;
use std::fmt;

/// The first sixteen bytes of every SQLite database.
const MAGIC: &[u8; 16] = b"SQLite format 3\0";

/// Smallest page SQLite will write, and the smallest this accepts.
const MIN_PAGE_SIZE: usize = 512;

/// A page's *usable* size — what is left after the reserved region — may not
/// go below this, by the format's own rule. It is what makes the payload
/// arithmetic in [`Database::local_payload`] safe.
const MIN_USABLE: usize = 480;

/// Anything wrong with the bytes.
///
/// One variant with a sentence in it rather than a taxonomy: every caller does
/// the same thing with it — reports that the file could not be read — and a
/// reader of somebody else's file needs the message to say *where* it gave up
/// far more than it needs a matchable code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SqliteError(String);

impl SqliteError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for SqliteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SqliteError {}

type Result<T> = std::result::Result<T, SqliteError>;

/// How text columns are stored. Recorded in the header at offset 56.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TextEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

/// One column of one row.
///
/// Owned rather than borrowed because a value large enough to matter is
/// exactly the one that does *not* live in a contiguous slice: a payload past
/// the page's local limit is scattered down a chain of overflow pages and has
/// to be gathered anyway. Paying a copy for the small ones keeps one type
/// instead of two.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl Value {
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Integer(v) => Some(*v),
            // A column declared INTEGER can still hold a float — SQLite's
            // affinity is a suggestion, and Clip Studio writes both.
            Self::Real(v) => Some(*v as i64),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Real(v) => Some(*v),
            Self::Integer(v) => Some(*v as f64),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Text(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_blob(&self) -> Option<&[u8]> {
        match self {
            Self::Blob(v) => Some(v),
            _ => None,
        }
    }
}

/// Where a table's b-tree starts, and what its columns are called.
#[derive(Clone, Debug)]
pub struct Table {
    root: u32,
    columns: Vec<String>,
}

impl Table {
    /// Column names in declaration order, which is the order [`Row`] holds
    /// values in.
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// Index of a column, matched without regard to case — SQLite's own rule
    /// for unquoted identifiers.
    pub fn column(&self, name: &str) -> Option<usize> {
        self.columns
            .iter()
            .position(|c| c.eq_ignore_ascii_case(name))
    }
}

/// One row, in column order.
///
/// Shorter than the column list where the row was written by an older schema:
/// SQLite stores a record with fewer fields and reports `NULL` for the rest,
/// which is what [`Row::get`] does.
#[derive(Clone, Debug, Default)]
pub struct Row {
    values: Vec<Value>,
}

impl Row {
    /// The value at a column index, or `NULL` past the end of the record.
    pub fn get(&self, index: usize) -> &Value {
        self.values.get(index).unwrap_or(&Value::Null)
    }

    /// Every value in the record, in column order.
    ///
    /// For a table found through the schema, [`Row::get`] is the way in — the
    /// column names are what a caller has. This is for [`Database::scan`],
    /// where there is no schema at all and a row is identified by what is *in*
    /// it.
    pub fn values(&self) -> &[Value] {
        &self.values
    }
}

/// A database open over a byte slice.
#[derive(Debug)]
pub struct Database<'a> {
    bytes: &'a [u8],
    page_size: usize,
    /// Page size less the reserved region at the end of every page.
    usable: usize,
    /// How many whole pages the slice actually holds. Taken from the length
    /// rather than from the header's count, because a truncated file must fail
    /// on the page that is missing rather than be trusted about its own size.
    pages: u32,
    /// The page number the slice *starts* at. `1` for an ordinary file, and
    /// something else for [`Database::headerless`] — see its docs.
    first_page: u32,
    encoding: TextEncoding,
}

impl<'a> Database<'a> {
    /// Read the file header and check it describes something walkable.
    pub fn open(bytes: &'a [u8]) -> Result<Self> {
        if bytes.len() < 100 || &bytes[..16] != MAGIC {
            return Err(SqliteError::new("this is not a SQLite database"));
        }

        // A page size of 1 means 65536: the field is 16 bits and the largest
        // page does not fit in it.
        let raw = u16::from_be_bytes([bytes[16], bytes[17]]) as usize;
        let page_size = if raw == 1 { 65536 } else { raw };
        if page_size < MIN_PAGE_SIZE || !page_size.is_power_of_two() {
            return Err(SqliteError::new(format!(
                "its page size is {page_size}, which is not a power of two of at least {MIN_PAGE_SIZE}"
            )));
        }

        let reserved = bytes[20] as usize;
        let usable = page_size
            .checked_sub(reserved)
            .filter(|u| *u >= MIN_USABLE)
            .ok_or_else(|| {
                SqliteError::new(format!(
                    "it reserves {reserved} bytes of every {page_size}-byte page, leaving too little"
                ))
            })?;

        let encoding = match u32::from_be_bytes([bytes[56], bytes[57], bytes[58], bytes[59]]) {
            // Zero is what a freshly created, never-written database holds.
            0 | 1 => TextEncoding::Utf8,
            2 => TextEncoding::Utf16Le,
            3 => TextEncoding::Utf16Be,
            other => {
                return Err(SqliteError::new(format!(
                    "its text encoding is {other}, which is not one SQLite writes"
                )));
            }
        };

        let pages = (bytes.len() / page_size) as u32;
        if pages == 0 {
            return Err(SqliteError::new("it is shorter than one page"));
        }

        Ok(Self {
            bytes,
            page_size,
            usable,
            pages,
            first_page: 1,
            encoding,
        })
    }

    /// Open a database that has **no file header and no first pages**.
    ///
    /// Clip Studio stores a material's full-resolution pixels this way: the
    /// `dATA` chunk of `data/material_0.layer` whose flag is zero is an
    /// ordinary SQLite database with its header and its first six pages sliced
    /// off, so the slice begins part way through the page sequence and a page
    /// number `n` names the byte range `(n - first_page) * page_size`. See
    /// [`crate::brushimport::clipstudio`].
    ///
    /// Two things the header would have said have to be supplied instead, and
    /// one is simply gone:
    ///
    /// - the **page size**, which the caller states;
    /// - the **text encoding**, which is only ever needed for column names this
    ///   database has none of — every value read out of one of these is an
    ///   integer or a blob — so it is taken as UTF-16LE, which is what the
    ///   fragments of `CREATE TABLE` text in the sample materials are;
    /// - the **schema**, which lived on page 1. There is no `sqlite_master` to
    ///   find a table's root with, which is why the way in is [`Database::scan`]
    ///   rather than [`Database::table`].
    ///
    /// The reserved region is taken as zero. SQLite only ever writes a non-zero
    /// one when an extension asks for it, and none of the arithmetic below can
    /// tell — a wrong answer here would slice every large blob in the wrong
    /// place, which is exactly the failure the round-trip check in
    /// `clipstudio`'s tests exists to catch.
    pub fn headerless(bytes: &'a [u8], page_size: usize, first_page: u32) -> Result<Self> {
        if page_size < MIN_PAGE_SIZE || !page_size.is_power_of_two() {
            return Err(SqliteError::new(format!(
                "its page size is {page_size}, which is not a power of two of at least {MIN_PAGE_SIZE}"
            )));
        }
        if first_page == 0 {
            return Err(SqliteError::new("page numbering starts at one"));
        }
        let pages = (bytes.len() / page_size) as u32;
        if pages == 0 {
            return Err(SqliteError::new("it is shorter than one page"));
        }
        Ok(Self {
            bytes,
            page_size,
            usable: page_size,
            pages,
            first_page,
            encoding: TextEncoding::Utf16Le,
        })
    }

    /// Every record on every **table-leaf** page in the slice, in page order.
    ///
    /// The answer to a database with no schema. Interior pages are passed over
    /// rather than descended, because their leaves are in the slice too and
    /// would then be read twice; index pages and anything that is not a b-tree
    /// page are passed over as well.
    ///
    /// **A page that is not a b-tree page can still look like one**, and this
    /// is the reason the result is a bag of rows rather than a table: an
    /// overflow page holds raw bytes, so one whose first byte happens to be
    /// `0x0d` is decoded as a leaf and yields whatever the arithmetic makes of
    /// the compressed pixels underneath it. Everything is bounds-checked and a
    /// page that will not decode is dropped, so the cost is a spurious row and
    /// never a panic — but a caller must identify what it wants by what is
    /// *in* a row, never by counting them.
    pub fn scan(&self) -> Vec<Row> {
        let mut out = Vec::new();
        for index in 0..self.pages {
            // `checked_add`, because `first_page` is a caller's number and this
            // is a `pub` API: a debug build panics on the overflow.
            let Some(page) = self.first_page.checked_add(index) else {
                break;
            };
            let Ok(bytes) = self.page_bytes(page) else {
                continue;
            };
            let base = if page == 1 { 100 } else { 0 };
            if bytes.get(base) != Some(&0x0d) {
                continue;
            }
            let cells = u16::from_be_bytes([bytes[base + 3], bytes[base + 4]]) as usize;
            let pointers = base + 8;
            if pointers + cells * 2 > self.usable {
                continue;
            }
            for i in 0..cells {
                let at = pointers + i * 2;
                let cell = u16::from_be_bytes([bytes[at], bytes[at + 1]]) as usize;
                if cell >= self.usable {
                    continue;
                }
                if let Ok(row) = self.leaf_cell(bytes, cell) {
                    out.push(row);
                }
            }
        }
        out
    }

    /// One whole page, by its own page number.
    fn page_bytes(&self, page: u32) -> Result<&'a [u8]> {
        let index = page
            .checked_sub(self.first_page)
            .filter(|i| *i < self.pages)
            .ok_or_else(|| {
                SqliteError::new(format!(
                    "it points at page {page}, which is not in the file"
                ))
            })?;
        let start = index as usize * self.page_size;
        Ok(&self.bytes[start..start + self.page_size])
    }

    /// `sqlite_master`, the one table whose shape is not written down anywhere:
    /// it is always rooted at page 1 with these five columns.
    fn master() -> Table {
        Table {
            root: 1,
            columns: ["type", "name", "tbl_name", "rootpage", "sql"]
                .iter()
                .map(|c| (*c).to_string())
                .collect(),
        }
    }

    /// Every table the schema declares, in the order it declares them.
    ///
    /// [`Self::table`] answers about a name somebody already has, which is what
    /// a reader wants and is exactly wrong for a *survey*: a question of the
    /// form "does this file hold a cached raster anywhere" cannot be asked by
    /// guessing names, and a list guessed wrong reads as an answer of "no".
    /// Used by `examples/survey-clip-schema.rs`; no reader calls it, and none
    /// should — a reader that walked the schema would be one whose behaviour
    /// depended on a table it had never been written against.
    pub fn table_names(&self) -> Result<Vec<String>> {
        Ok(self
            .rows(&Self::master())?
            .iter()
            .filter(|row| row.get(0).as_str() == Some("table"))
            .filter_map(|row| row.get(1).as_str().map(str::to_string))
            .collect())
    }

    /// Look a table up in the schema.
    ///
    /// `Ok(None)` for a database that simply has no such table, which is a
    /// thing a caller reasonably tolerates — a `.sut` written by an older Clip
    /// Studio has fewer of them.
    pub fn table(&self, name: &str) -> Result<Option<Table>> {
        for row in self.rows(&Self::master())? {
            if row.get(0).as_str() != Some("table") {
                continue;
            }
            if !row
                .get(1)
                .as_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
            {
                continue;
            }
            let root = row.get(3).as_i64().unwrap_or(0);
            let root = u32::try_from(root).map_err(|_| {
                SqliteError::new(format!("the table `{name}` names page {root} as its root"))
            })?;
            let sql = row.get(4).as_str().unwrap_or_default();
            return Ok(Some(Table {
                root,
                columns: column_names(sql),
            }));
        }
        Ok(None)
    }

    /// Every row of a table, in rowid order.
    pub fn rows(&self, table: &Table) -> Result<Vec<Row>> {
        let mut out = Vec::new();
        // A corrupt or hostile file can point a child at a page already on the
        // path. Recording every page visited bounds the walk absolutely rather
        // than by a depth guess.
        let mut seen = BTreeSet::new();
        self.walk(table.root, &mut seen, &mut out)?;
        Ok(out)
    }

    /// Walk one page of a table b-tree, appending every leaf record below it.
    fn walk(&self, page: u32, seen: &mut BTreeSet<u32>, out: &mut Vec<Row>) -> Result<()> {
        let page_bytes = self.page_bytes(page)?;
        if !seen.insert(page) {
            return Err(SqliteError::new(format!(
                "page {page} appears twice in one table, so the file is not a tree"
            )));
        }

        // Page 1 carries the hundred-byte file header before its b-tree page
        // header; every other page starts with the b-tree header.
        let base = if page == 1 { 100 } else { 0 };

        let kind = page_bytes[base];
        let header_len = match kind {
            // Table leaf: cells are the rows.
            0x0d => 8,
            // Table interior: cells are child pointers.
            0x05 => 12,
            0x0a | 0x02 => {
                return Err(SqliteError::new(format!(
                    "page {page} is an index page, and only tables are read"
                )));
            }
            other => {
                return Err(SqliteError::new(format!(
                    "page {page} has type {other:#04x}, which is not a b-tree page"
                )));
            }
        };

        let cells = u16::from_be_bytes([page_bytes[base + 3], page_bytes[base + 4]]) as usize;
        let pointers = base + header_len;
        if pointers + cells * 2 > self.usable {
            return Err(SqliteError::new(format!(
                "page {page} claims {cells} cells, more than fit on it"
            )));
        }

        for i in 0..cells {
            let at = pointers + i * 2;
            let cell = u16::from_be_bytes([page_bytes[at], page_bytes[at + 1]]) as usize;
            if cell >= self.usable {
                return Err(SqliteError::new(format!(
                    "a cell on page {page} starts at {cell}, past the usable area"
                )));
            }
            if kind == 0x0d {
                out.push(self.leaf_cell(page_bytes, cell)?);
            } else {
                // `.get`, not an index. The check above bounds `cell` by the
                // usable area, which says nothing about the four bytes *after*
                // it — a pointer two bytes short of the end of the page would
                // slice past it and panic, and a panic here takes the whole
                // application down because somebody opened the wrong file.
                let child = page_bytes
                    .get(cell..cell + 4)
                    .and_then(|b| <[u8; 4]>::try_from(b).ok())
                    .map(u32::from_be_bytes)
                    .ok_or_else(|| {
                        SqliteError::new(format!(
                            "a child pointer on page {page} runs off the end of it"
                        ))
                    })?;
                self.walk(child, seen, out)?;
            }
        }

        if kind == 0x05 {
            // The rightmost child hangs off the page header rather than off a
            // cell, because it has no key to the right of it.
            let right = u32::from_be_bytes(
                page_bytes[base + 8..base + 12]
                    .try_into()
                    .expect("the header was sized for this"),
            );
            self.walk(right, seen, out)?;
        }
        Ok(())
    }

    /// Decode one row out of a table leaf cell.
    fn leaf_cell(&self, page: &[u8], cell: usize) -> Result<Row> {
        let (payload_len, n) = varint(page, cell)?;
        // The rowid follows and is skipped: no caller reads one, and the
        // column that aliases it is stored as NULL either way.
        let (_, m) = varint(page, cell + n)?;
        let body = cell + n + m;

        let payload_len = usize::try_from(payload_len)
            .map_err(|_| SqliteError::new("a record claims a negative length"))?;
        let local = self.local_payload(payload_len);
        if body + local > self.usable {
            return Err(SqliteError::new(
                "a record runs past the end of the page holding it",
            ));
        }

        let mut payload = page[body..body + local].to_vec();
        if local < payload_len {
            // `.get` for the reason the child pointer above takes one: the
            // check is that the *record* fits inside the usable area, and the
            // overflow pointer sits four bytes past its end. A record ending
            // exactly at the page boundary would slice past it and panic.
            let next = page
                .get(body + local..body + local + 4)
                .and_then(|b| <[u8; 4]>::try_from(b).ok())
                .map(u32::from_be_bytes)
                .ok_or_else(|| SqliteError::new("a record ends before its overflow pointer"))?;
            self.gather_overflow(next, payload_len - local, &mut payload)?;
        }

        self.record(&payload)
    }

    /// How much of a record of `total` bytes lives on its own page.
    ///
    /// Straight out of the format's definition. The constants are not
    /// adjustable: they are what SQLite itself computes when it writes the
    /// page, so a reader that rounded differently would slice every large blob
    /// in the wrong place.
    fn local_payload(&self, total: usize) -> usize {
        let max_local = self.usable - 35;
        if total <= max_local {
            return total;
        }
        let min_local = ((self.usable - 12) * 32 / 255) - 23;
        let k = min_local + ((total - min_local) % (self.usable - 4));
        if k <= max_local { k } else { min_local }
    }

    /// Follow an overflow chain, appending exactly `remaining` bytes.
    fn gather_overflow(&self, first: u32, mut remaining: usize, out: &mut Vec<u8>) -> Result<()> {
        let mut page = first;
        let mut seen = BTreeSet::new();
        // Each overflow page spends four bytes on the pointer to the next.
        let per_page = self.usable - 4;
        while remaining > 0 {
            let bytes = self.page_bytes(page).map_err(|_| {
                SqliteError::new(format!(
                    "an overflow chain reaches page {page}, which is not in the file"
                ))
            })?;
            if !seen.insert(page) {
                return Err(SqliteError::new("an overflow chain loops back on itself"));
            }
            let take = remaining.min(per_page);
            out.extend_from_slice(&bytes[4..4 + take]);
            remaining -= take;
            page = u32::from_be_bytes(
                bytes[..4]
                    .try_into()
                    .expect("a page is longer than four bytes"),
            );
        }
        Ok(())
    }

    /// Decode a record: a header of serial types, then the values themselves.
    fn record(&self, payload: &[u8]) -> Result<Row> {
        let (header_len, n) = varint(payload, 0)?;
        let header_len = usize::try_from(header_len)
            .ok()
            .filter(|len| *len >= n && *len <= payload.len())
            .ok_or_else(|| SqliteError::new("a record's header does not fit inside it"))?;

        let mut types = Vec::new();
        let mut at = n;
        while at < header_len {
            let (serial, used) = varint(payload, at)?;
            types.push(serial);
            at += used;
        }

        let mut values = Vec::with_capacity(types.len());
        let mut at = header_len;
        for serial in types {
            let (value, used) = self.value(payload, at, serial)?;
            values.push(value);
            at += used;
        }
        Ok(Row { values })
    }

    /// One value, and how many bytes of the record body it consumed.
    fn value(&self, payload: &[u8], at: usize, serial: i64) -> Result<(Value, usize)> {
        let take = |len: usize| -> Result<&[u8]> {
            payload
                .get(at..at + len)
                .ok_or_else(|| SqliteError::new("a record ends inside one of its values"))
        };

        Ok(match serial {
            0 => (Value::Null, 0),
            // Signed big-endian integers of 1, 2, 3, 4, 6 and 8 bytes.
            1..=6 => {
                let len = [1usize, 2, 3, 4, 6, 8][serial as usize - 1];
                let bytes = take(len)?;
                // Sign-extend from the top byte, which is what makes the
                // three- and six-byte widths work at all.
                let mut v = if bytes[0] & 0x80 != 0 { -1i64 } else { 0 };
                for b in bytes {
                    v = (v << 8) | i64::from(*b);
                }
                (Value::Integer(v), len)
            }
            7 => {
                let bytes = take(8)?;
                let v = f64::from_be_bytes(bytes.try_into().expect("eight bytes"));
                (Value::Real(v), 8)
            }
            // The two constants that cost no bytes at all.
            8 => (Value::Integer(0), 0),
            9 => (Value::Integer(1), 0),
            // Reserved for internal use; a file that contains one is not
            // something to guess about.
            10 | 11 => {
                return Err(SqliteError::new(format!(
                    "a value has the reserved type {serial}"
                )));
            }
            n if n >= 12 && n % 2 == 0 => {
                let len = ((n - 12) / 2) as usize;
                (Value::Blob(take(len)?.to_vec()), len)
            }
            n if n >= 13 => {
                let len = ((n - 13) / 2) as usize;
                (Value::Text(self.text(take(len)?)), len)
            }
            n => {
                return Err(SqliteError::new(format!("a value has the type {n}")));
            }
        })
    }

    /// Decode a text value in whatever encoding the header declared.
    ///
    /// Lossy on purpose. Text out of somebody else's file is the *name* of a
    /// brush, and a name with one bad byte in it should arrive with a
    /// replacement character rather than fail the whole import.
    fn text(&self, bytes: &[u8]) -> String {
        match self.encoding {
            TextEncoding::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
            TextEncoding::Utf16Le | TextEncoding::Utf16Be => {
                let le = self.encoding == TextEncoding::Utf16Le;
                let units: Vec<u16> = bytes
                    .chunks_exact(2)
                    .map(|p| {
                        if le {
                            u16::from_le_bytes([p[0], p[1]])
                        } else {
                            u16::from_be_bytes([p[0], p[1]])
                        }
                    })
                    .collect();
                String::from_utf16_lossy(&units)
            }
        }
    }
}

/// SQLite's variable-length integer: up to nine bytes, seven bits each, high
/// bit set on every byte but the last. The ninth byte, if it is reached,
/// contributes all eight of its bits.
fn varint(bytes: &[u8], at: usize) -> Result<(i64, usize)> {
    let mut value: u64 = 0;
    for i in 0..9 {
        let byte = *bytes
            .get(at + i)
            .ok_or_else(|| SqliteError::new("a variable-length integer runs off the end"))?;
        if i == 8 {
            value = (value << 8) | u64::from(byte);
            return Ok((value as i64, 9));
        }
        value = (value << 7) | u64::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            return Ok((value as i64, i + 1));
        }
    }
    unreachable!("the ninth byte returns")
}

/// Column names out of a `CREATE TABLE` statement.
///
/// The schema is stored as the text that made it, so this is the only way to
/// learn what a column is called — and it has to be name-based rather than
/// positional, because Clip Studio's own schema is not fixed: a `.sutg`
/// holding a fill tool declares twenty-seven columns a `.sut` holding one
/// brush does not, and they are interleaved with the rest.
///
/// Deliberately shallow. It splits the parenthesised list at depth one,
/// respecting quotes, and takes the first identifier of each item — which is
/// the column name — skipping the table constraints that can appear in the
/// same list. It is not a SQL parser and does not need to be: anything it
/// cannot make sense of costs one column its name, and the caller then finds
/// no column by that name and treats the setting as absent.
fn column_names(sql: &str) -> Vec<String> {
    let Some(open) = sql.find('(') else {
        return Vec::new();
    };

    let mut out = Vec::new();
    let mut item = String::new();
    let mut depth = 0usize;
    // The quote character we are inside, if any. SQLite accepts four kinds and
    // Clip Studio's DDL uses none of them, but a default value with a comma in
    // it would otherwise split a column in half.
    let mut quote: Option<char> = None;

    for c in sql[open..].chars() {
        if let Some(q) = quote {
            item.push(c);
            if c == q || (q == '[' && c == ']') {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' | '`' | '[' => {
                quote = Some(c);
                item.push(c);
            }
            '(' => {
                depth += 1;
                if depth > 1 {
                    item.push(c);
                }
            }
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    push_column(&item, &mut out);
                    break;
                }
                item.push(c);
            }
            ',' if depth == 1 => {
                push_column(&item, &mut out);
                item.clear();
            }
            _ => item.push(c),
        }
    }
    out
}

/// Take the column name off the front of one item of a `CREATE TABLE` list.
fn push_column(item: &str, out: &mut Vec<String>) {
    let item = item.trim();
    if item.is_empty() {
        return;
    }
    // A quoted name may contain spaces, so the identifier ends at its closing
    // quote rather than at the first gap.
    let mut chars = item.chars();
    let unquoted: String = match chars.next() {
        Some(open @ ('"' | '`' | '[' | '\'')) => {
            let close = if open == '[' { ']' } else { open };
            chars.take_while(|c| *c != close).collect()
        }
        Some(first) => std::iter::once(first)
            .chain(chars.take_while(|c| !c.is_whitespace() && *c != '('))
            .collect(),
        None => return,
    };
    if unquoted.is_empty() {
        return;
    }
    // A table constraint sits in the same comma-separated list as the columns
    // and starts with one of these words. Taking it as a column would shift
    // every name after it — except that names are looked up rather than
    // indexed, so it would only add one that nothing asks for. Skipped anyway,
    // because a column list that reports a phantom column is a confusing thing
    // to debug.
    const CONSTRAINTS: [&str; 6] = [
        "constraint",
        "primary",
        "unique",
        "check",
        "foreign",
        "exclude",
    ];
    if CONSTRAINTS.iter().any(|k| unquoted.eq_ignore_ascii_case(k)) {
        return;
    }
    out.push(unquoted);
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Minimal SQLite databases, built in memory.
///
/// The same bargain `docimport::fixtures` records: a generated fixture is
/// readable in a diff and says out loud what the test believes the format to
/// be, but it tests the reader against *this file's* understanding rather than
/// against SQLite. Two things offset that here. The writer is deliberately the
/// inverse of the reader rather than a copy of it — it lays out pages, spills
/// oversized records into overflow chains and splits a table across leaves
/// under an interior page, all of which the reader has to undo. And the reader
/// was developed against real Clip Studio files, which is the check a fixture
/// cannot be.
#[cfg(test)]
pub(crate) mod fixture {
    use super::Value;

    /// One table to write: its name, its columns and its rows.
    pub struct TableSpec {
        pub name: &'static str,
        pub columns: Vec<&'static str>,
        pub rows: Vec<Vec<Value>>,
    }

    impl TableSpec {
        pub fn new(name: &'static str, columns: &[&'static str]) -> Self {
            Self {
                name,
                columns: columns.to_vec(),
                rows: Vec::new(),
            }
        }

        pub fn row(mut self, values: Vec<Value>) -> Self {
            self.rows.push(values);
            self
        }
    }

    /// Write a database with **no file header and no pages before
    /// `first_page`** — what a Clip Studio material carries, and what
    /// [`super::Database::headerless`] reads.
    ///
    /// **One leaf page per record**, each followed by whatever overflow chain
    /// that record needs. There is no interior page and no tree, because
    /// [`super::Database::scan`] does not walk one — it visits every page and
    /// decodes the leaves, which is what a database with no `sqlite_master` to
    /// find a root in leaves it able to do.
    ///
    /// The overflow chain is what makes this worth building: its page numbers
    /// are absolute, so a reader whose `first_page` is off by one reaches the
    /// wrong bytes — which is the mistake `docs/brushes.md` recorded, and the
    /// reason the number is now pinned by a test rather than by a note.
    pub fn headerless(records: &[Vec<Value>], page_size: usize, first_page: u32) -> Vec<u8> {
        let max_local = page_size - 35;
        let min_local = ((page_size - 12) * 32 / 255) - 23;
        let per_page = page_size - 4;

        let mut out: Vec<u8> = Vec::new();
        for (i, values) in records.iter().enumerate() {
            let record = encode_record(values);
            let total = record.len();
            let local = if total <= max_local {
                total
            } else {
                let k = min_local + ((total - min_local) % per_page);
                if k <= max_local { k } else { min_local }
            };

            let mut cell = varint(total as i64);
            cell.extend_from_slice(&varint(i as i64 + 1));
            cell.extend_from_slice(&record[..local]);

            // This record's own pages start where the file has reached; the
            // leaf is the first of them and the chain follows it.
            let leaf_page = first_page + (out.len() / page_size) as u32;
            let mut overflow: Vec<Vec<u8>> = Vec::new();
            if local < total {
                let mut rest = &record[local..];
                let first = leaf_page + 1;
                let count = rest.len().div_ceil(per_page);
                for step in 0..count {
                    let take = rest.len().min(per_page);
                    let mut buffer = vec![0u8; page_size];
                    let next = if step + 1 < count {
                        first + step as u32 + 1
                    } else {
                        0
                    };
                    buffer[..4].copy_from_slice(&next.to_be_bytes());
                    buffer[4..4 + take].copy_from_slice(&rest[..take]);
                    rest = &rest[take..];
                    overflow.push(buffer);
                }
                cell.extend_from_slice(&first.to_be_bytes());
            }

            let mut leaf = vec![0u8; page_size];
            leaf[0] = 0x0d;
            leaf[3..5].copy_from_slice(&1u16.to_be_bytes());
            let at = page_size - cell.len();
            leaf[at..].copy_from_slice(&cell);
            leaf[5..7].copy_from_slice(&(at as u16).to_be_bytes());
            leaf[8..10].copy_from_slice(&(at as u16).to_be_bytes());

            out.extend_from_slice(&leaf);
            for page in overflow {
                out.extend_from_slice(&page);
            }
        }
        out
    }

    /// Write a database holding exactly these tables.
    ///
    /// The page size is Clip Studio's own 4096, so a blob of any real size
    /// spills the way it does in the files this exists to read.
    pub fn database(tables: &[TableSpec]) -> Vec<u8> {
        const PAGE: usize = 4096;
        // Usable size: the fixtures reserve nothing at the end of a page.
        const USABLE: usize = PAGE;

        // Page 1 is the schema; every other page is allocated as it is needed.
        let mut pages: Vec<Vec<u8>> = vec![vec![0u8; PAGE]];
        let mut schema_rows: Vec<Vec<Value>> = Vec::new();

        for table in tables {
            let records: Vec<Vec<u8>> = table.rows.iter().map(|r| encode_record(r)).collect();
            let root = write_btree(&mut pages, &records, PAGE, USABLE);
            let sql = format!(
                "CREATE TABLE {}({})",
                table.name,
                table
                    .columns
                    .iter()
                    .map(|c| format!("{c} BLOB DEFAULT NULL"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            schema_rows.push(vec![
                Value::Text("table".to_string()),
                Value::Text(table.name.to_string()),
                Value::Text(table.name.to_string()),
                Value::Integer(i64::from(root)),
                Value::Text(sql),
            ]);
        }

        // The schema itself is a table b-tree, and it has to be rooted at page
        // 1 — so it is laid out last, once the pages it may need are free to
        // allocate, and then moved into place.
        let schema: Vec<Vec<u8>> = schema_rows.iter().map(|r| encode_record(r)).collect();
        let schema_root = write_btree(&mut pages, &schema, PAGE, USABLE);
        // Page 1's b-tree begins after the hundred-byte file header, so its
        // contents are shifted rather than copied straight over.
        let laid_out = std::mem::take(&mut pages[(schema_root - 1) as usize]);
        pages[0] = shift_for_header(&laid_out, PAGE);
        // The page the schema was laid out on is now unreferenced. Left in
        // place as an empty page rather than removed, so every root already
        // handed out still points where it did.
        pages[(schema_root - 1) as usize] = vec![0u8; PAGE];

        let mut out: Vec<u8> = pages.concat();
        write_header(&mut out, PAGE, pages.len() as u32);
        out
    }

    /// The hundred-byte file header.
    fn write_header(out: &mut [u8], page: usize, pages: u32) {
        out[..16].copy_from_slice(b"SQLite format 3\0");
        out[16..18].copy_from_slice(&(page as u16).to_be_bytes());
        out[18] = 1; // write version: legacy
        out[19] = 1; // read version: legacy
        out[20] = 0; // no reserved region
        out[21] = 64;
        out[22] = 32;
        out[23] = 32;
        out[24..28].copy_from_slice(&1u32.to_be_bytes()); // change counter
        out[28..32].copy_from_slice(&pages.to_be_bytes());
        out[56..60].copy_from_slice(&1u32.to_be_bytes()); // UTF-8
        out[92..96].copy_from_slice(&1u32.to_be_bytes()); // valid-for
        out[96..100].copy_from_slice(&3_050_004u32.to_be_bytes());
    }

    /// Move a laid-out page down by the hundred bytes page 1 gives to the file
    /// header.
    fn shift_for_header(page: &[u8], size: usize) -> Vec<u8> {
        let mut out = vec![0u8; size];
        // An interior page carries the rightmost child pointer in four extra
        // header bytes, so how much moves depends on which kind it is.
        let header = if page[0] == 0x05 { 12 } else { 8 };
        out[100..100 + header].copy_from_slice(&page[..header]);
        let cells = u16::from_be_bytes([page[3], page[4]]) as usize;
        for i in 0..cells {
            let at = header + i * 2;
            let cell = u16::from_be_bytes([page[at], page[at + 1]]) as usize;
            let to = 100 + header + i * 2;
            out[to..to + 2].copy_from_slice(&(cell as u16).to_be_bytes());
        }
        // The cell content itself is measured from the end of the page, so it
        // does not move at all.
        let content = u16::from_be_bytes([page[5], page[6]]) as usize;
        out[content..].copy_from_slice(&page[content..]);
        out
    }

    /// Lay records out as a table b-tree and return its root page number.
    fn write_btree(
        pages: &mut Vec<Vec<u8>>,
        records: &[Vec<u8>],
        page: usize,
        usable: usize,
    ) -> u32 {
        let max_local = usable - 35;
        let min_local = ((usable - 12) * 32 / 255) - 23;

        // Cells, in rowid order, with any overflow already written out.
        let mut cells: Vec<(i64, Vec<u8>)> = Vec::new();
        for (i, record) in records.iter().enumerate() {
            let rowid = i as i64 + 1;
            let total = record.len();
            let local = if total <= max_local {
                total
            } else {
                let k = min_local + ((total - min_local) % (usable - 4));
                if k <= max_local { k } else { min_local }
            };

            let mut cell = Vec::new();
            cell.extend_from_slice(&varint(total as i64));
            cell.extend_from_slice(&varint(rowid));
            cell.extend_from_slice(&record[..local]);
            if local < total {
                let first = write_overflow(pages, &record[local..], page, usable);
                cell.extend_from_slice(&first.to_be_bytes());
            }
            cells.push((rowid, cell));
        }

        // Pack the cells into leaves, greedily, leaving room for the eight-byte
        // page header and each cell's two-byte pointer.
        let mut leaves: Vec<(i64, u32)> = Vec::new();
        let mut batch: Vec<(i64, Vec<u8>)> = Vec::new();
        let mut used = 8usize;
        for (rowid, cell) in cells {
            let cost = cell.len() + 2;
            if !batch.is_empty() && used + cost > usable {
                leaves.push(flush_leaf(pages, &batch, page, usable));
                batch.clear();
                used = 8;
            }
            used += cost;
            batch.push((rowid, cell));
        }
        // Always emit a leaf, so an empty table still has a root to name.
        leaves.push(flush_leaf(pages, &batch, page, usable));

        if leaves.len() == 1 {
            return leaves[0].1;
        }

        // More than one leaf needs an interior page above them: a cell per
        // child but the last, then the rightmost pointer in the header.
        let mut body = Vec::new();
        let mut pointers = Vec::new();
        let mut end = usable;
        let (last, rest) = leaves.split_last().expect("at least two leaves");
        for (rowid, child) in rest {
            let mut cell = Vec::new();
            cell.extend_from_slice(&child.to_be_bytes());
            cell.extend_from_slice(&varint(*rowid));
            end -= cell.len();
            pointers.push(end as u16);
            body.push((end, cell));
        }

        let mut buffer = vec![0u8; page];
        buffer[0] = 0x05;
        buffer[3..5].copy_from_slice(&(pointers.len() as u16).to_be_bytes());
        buffer[5..7].copy_from_slice(&(end as u16).to_be_bytes());
        buffer[8..12].copy_from_slice(&last.1.to_be_bytes());
        for (i, at) in pointers.iter().enumerate() {
            buffer[12 + i * 2..14 + i * 2].copy_from_slice(&at.to_be_bytes());
        }
        for (at, cell) in body {
            buffer[at..at + cell.len()].copy_from_slice(&cell);
        }
        pages.push(buffer);
        pages.len() as u32
    }

    /// Write one leaf page and return its largest rowid and its number.
    fn flush_leaf(
        pages: &mut Vec<Vec<u8>>,
        cells: &[(i64, Vec<u8>)],
        page: usize,
        usable: usize,
    ) -> (i64, u32) {
        let mut buffer = vec![0u8; page];
        buffer[0] = 0x0d;
        buffer[3..5].copy_from_slice(&(cells.len() as u16).to_be_bytes());

        let mut end = usable;
        let mut pointers = Vec::new();
        for (_, cell) in cells {
            end -= cell.len();
            buffer[end..end + cell.len()].copy_from_slice(cell);
            pointers.push(end as u16);
        }
        buffer[5..7].copy_from_slice(&(end as u16).to_be_bytes());
        for (i, at) in pointers.iter().enumerate() {
            buffer[8 + i * 2..10 + i * 2].copy_from_slice(&at.to_be_bytes());
        }

        pages.push(buffer);
        let last = cells.last().map_or(1, |(rowid, _)| *rowid);
        (last, pages.len() as u32)
    }

    /// Write the tail of an oversized record as a chain of overflow pages.
    fn write_overflow(
        pages: &mut Vec<Vec<u8>>,
        mut rest: &[u8],
        page: usize,
        usable: usize,
    ) -> u32 {
        let per_page = usable - 4;
        // Reserve the whole chain first, so each page can be given the number
        // of the one after it.
        let count = rest.len().div_ceil(per_page);
        let first = pages.len() as u32 + 1;
        for i in 0..count {
            let take = rest.len().min(per_page);
            let mut buffer = vec![0u8; page];
            let next = if i + 1 < count {
                first + i as u32 + 1
            } else {
                0
            };
            buffer[..4].copy_from_slice(&next.to_be_bytes());
            buffer[4..4 + take].copy_from_slice(&rest[..take]);
            rest = &rest[take..];
            pages.push(buffer);
        }
        first
    }

    /// Encode one row as a record: serial types in a header, then the bodies.
    fn encode_record(values: &[Value]) -> Vec<u8> {
        let mut types = Vec::new();
        let mut body = Vec::new();
        for value in values {
            match value {
                Value::Null => types.push(varint(0)),
                Value::Integer(v) => {
                    // Always the widest width, which is legal and keeps the
                    // writer from quietly agreeing with a reader that got the
                    // narrow ones wrong.
                    types.push(varint(6));
                    body.extend_from_slice(&v.to_be_bytes());
                }
                Value::Real(v) => {
                    types.push(varint(7));
                    body.extend_from_slice(&v.to_be_bytes());
                }
                Value::Text(v) => {
                    types.push(varint(v.len() as i64 * 2 + 13));
                    body.extend_from_slice(v.as_bytes());
                }
                Value::Blob(v) => {
                    types.push(varint(v.len() as i64 * 2 + 12));
                    body.extend_from_slice(v);
                }
            }
        }

        let types_len: usize = types.iter().map(Vec::len).sum();
        // The header length counts itself, which makes it very slightly
        // self-referential: adding the length can push it over a varint
        // boundary. Settled by trying the shorter answer first.
        let mut header_len = types_len + 1;
        if varint(header_len as i64).len() > 1 {
            header_len = types_len + varint((types_len + 2) as i64).len();
        }

        let mut out = varint(header_len as i64);
        for t in types {
            out.extend_from_slice(&t);
        }
        out.extend_from_slice(&body);
        out
    }

    /// SQLite's variable-length integer, written.
    fn varint(value: i64) -> Vec<u8> {
        let value = value as u64;
        if value <= 0x7f {
            return vec![value as u8];
        }
        let mut groups = Vec::new();
        let mut v = value;
        while v > 0 {
            groups.push((v & 0x7f) as u8);
            v >>= 7;
        }
        groups.reverse();
        let last = groups.len() - 1;
        for (i, g) in groups.iter_mut().enumerate() {
            if i != last {
                *g |= 0x80;
            }
        }
        groups
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{TableSpec, database};
    use super::*;

    #[test]
    fn a_file_that_is_not_a_database_is_refused_by_name() {
        let err = Database::open(b"not a database at all, not even nearly").unwrap_err();
        assert!(err.to_string().contains("not a SQLite database"), "{err}");
        // Shorter than the header, which must not index out of bounds.
        assert!(Database::open(b"SQLite").is_err());
    }

    #[test]
    fn a_variable_length_integer_reads_every_width() {
        assert_eq!(varint(&[0x00], 0).unwrap(), (0, 1));
        assert_eq!(varint(&[0x7f], 0).unwrap(), (127, 1));
        assert_eq!(varint(&[0x81, 0x00], 0).unwrap(), (128, 2));
        assert_eq!(varint(&[0xff, 0x7f], 0).unwrap(), (16383, 2));
        // Nine bytes: the last one contributes all eight of its bits, which is
        // the one case that is not seven at a time.
        assert_eq!(
            varint(&[0x81, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x00], 0).unwrap(),
            (1 << 57, 9)
        );
        assert!(varint(&[0x81], 0).is_err());
    }

    #[test]
    fn column_names_come_off_a_create_table_statement() {
        let sql = "CREATE TABLE Variant(_PW_ID INTEGER PRIMARY KEY AUTOINCREMENT, \
                   BrushSize REAL DEFAULT NULL, TextureImage BLOB DEFAULT NULL)";
        assert_eq!(
            column_names(sql),
            ["_PW_ID", "BrushSize", "TextureImage"]
                .map(str::to_string)
                .to_vec()
        );

        // A default value carrying a comma must not split a column in two, and
        // a table constraint in the same list is not a column.
        let awkward = "CREATE TABLE t(a TEXT DEFAULT 'x, y', b INT, PRIMARY KEY(a, b))";
        assert_eq!(
            column_names(awkward),
            ["a", "b"].map(str::to_string).to_vec()
        );

        // A quoted name arrives unquoted, and a type with its own parentheses
        // does not end the list.
        let quoted = r#"CREATE TABLE t("odd name" VARCHAR(20), [b] INT)"#;
        assert_eq!(
            column_names(quoted),
            ["odd name", "b"].map(str::to_string).to_vec()
        );

        assert!(column_names("not a statement").is_empty());
    }

    #[test]
    fn every_value_type_survives_a_round_trip() {
        let spec = TableSpec::new("Bits", &["n", "r", "t", "b", "z"]).row(vec![
            Value::Integer(-4_000_000_000),
            Value::Real(0.5),
            Value::Text("Sketch".to_string()),
            Value::Blob(vec![1, 2, 3]),
            Value::Null,
        ]);
        let bytes = database(&[spec]);

        let db = Database::open(&bytes).expect("open");
        let table = db.table("Bits").expect("look up").expect("present");
        assert_eq!(table.columns(), ["n", "r", "t", "b", "z"]);
        assert_eq!(table.column("BITS"), None);
        assert_eq!(table.column("N"), Some(0));

        let rows = db.rows(&table).expect("rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get(0).as_i64(), Some(-4_000_000_000));
        assert_eq!(rows[0].get(1).as_f64(), Some(0.5));
        assert_eq!(rows[0].get(2).as_str(), Some("Sketch"));
        assert_eq!(rows[0].get(3).as_blob(), Some(&[1u8, 2, 3][..]));
        assert_eq!(*rows[0].get(4), Value::Null);
        // Past the end of the record is NULL, not a panic: a row written by an
        // older schema is shorter than the column list.
        assert_eq!(*rows[0].get(99), Value::Null);
    }

    /// The path every real `.sut` takes: a bitmap tip is far larger than a
    /// page, so its blob is scattered down an overflow chain and has to come
    /// back in one piece and in order.
    #[test]
    fn a_blob_longer_than_a_page_comes_back_whole() {
        // Deliberately not a round number of pages, so the last one is partly
        // used — the case an off-by-one in the tail arithmetic survives.
        let big: Vec<u8> = (0..30_001u32).map(|i| (i % 251) as u8).collect();
        let spec = TableSpec::new("MaterialFile", &["FileData"])
            .row(vec![Value::Blob(big.clone())])
            .row(vec![Value::Blob(vec![7; 3])]);
        let bytes = database(&[spec]);

        let db = Database::open(&bytes).expect("open");
        let table = db.table("MaterialFile").expect("look up").expect("present");
        let rows = db.rows(&table).expect("rows");
        assert_eq!(rows[0].get(0).as_blob(), Some(&big[..]));
        assert_eq!(rows[1].get(0).as_blob(), Some(&[7u8, 7, 7][..]));
    }

    /// A brush group holds a row per sub-tool, and enough of them stop fitting
    /// on one page — at which point the root becomes an interior page and the
    /// rows are only reachable by descending through it.
    #[test]
    fn a_table_spread_over_several_pages_reads_back_in_order() {
        let mut spec = TableSpec::new("Node", &["NodeName", "Padding"]);
        for i in 0..200 {
            spec = spec.row(vec![
                Value::Text(format!("Brush {i}")),
                Value::Blob(vec![0; 100]),
            ]);
        }
        let bytes = database(&[spec]);

        let db = Database::open(&bytes).expect("open");
        let table = db.table("Node").expect("look up").expect("present");
        let rows = db.rows(&table).expect("rows");
        assert_eq!(rows.len(), 200);
        for (i, row) in rows.iter().enumerate() {
            assert_eq!(row.get(0).as_str(), Some(format!("Brush {i}").as_str()));
        }
    }

    #[test]
    fn several_tables_and_a_missing_one() {
        let bytes = database(&[
            TableSpec::new("Manager", &["Version"]).row(vec![Value::Integer(144)]),
            TableSpec::new("Node", &["NodeName"]).row(vec![Value::Text("Sketch".into())]),
        ]);
        let db = Database::open(&bytes).expect("open");

        let manager = db.table("Manager").expect("look up").expect("present");
        assert_eq!(
            db.rows(&manager).expect("rows")[0].get(0).as_i64(),
            Some(144)
        );
        let node = db.table("Node").expect("look up").expect("present");
        assert_eq!(
            db.rows(&node).expect("rows")[0].get(0).as_str(),
            Some("Sketch")
        );
        // A table the file does not have is not an error. A `.sut` written by
        // an older Clip Studio has fewer of them, and the importer decides
        // what it can still do.
        assert!(db.table("Variant").expect("look up").is_none());
    }

    /// The file is somebody else's. A page pointing into itself must stop,
    /// rather than recurse until the stack runs out.
    /// A database with no file header and no first pages — a Clip Studio
    /// material's, see [`crate::brushimport::csmaterial`]. There is no
    /// `sqlite_master`, so the way in is the page scan; and the record has to
    /// be large enough to spill, because an overflow chain names **absolute**
    /// page numbers and is the one thing that tells a right `first_page` from
    /// a wrong one.
    #[test]
    fn a_headerless_database_is_scanned_and_its_overflow_chains_resolve() {
        let big: Vec<u8> = (0..9_001u32).map(|i| (i % 251) as u8).collect();
        let records = vec![
            vec![Value::Integer(3), Value::Blob(big.clone())],
            vec![Value::Integer(7), Value::Blob(vec![1, 2, 3])],
        ];
        let bytes = fixture::headerless(&records, 1024, 6);

        let db = Database::headerless(&bytes, 1024, 6).expect("open");
        let rows = db.scan();
        let found: Vec<&Row> = rows.iter().filter(|r| r.values().len() == 2).collect();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].get(0).as_i64(), Some(3));
        assert_eq!(found[0].get(1).as_blob(), Some(&big[..]));
        assert_eq!(found[1].get(1).as_blob(), Some(&[1u8, 2, 3][..]));

        // Off by one on either side and the chain reaches the wrong bytes, so
        // the large record does not come back. This is the check that settles
        // the page number rather than a note saying what it is.
        for wrong in [5u32, 7] {
            let db = Database::headerless(&bytes, 1024, wrong).expect("open");
            assert!(
                !db.scan()
                    .iter()
                    .any(|r| r.get(1).as_blob() == Some(&big[..])),
                "first_page {wrong} should not resolve the chain"
            );
        }
    }

    #[test]
    fn a_page_that_points_at_itself_is_refused_rather_than_followed() {
        let mut bytes = database(&[
            TableSpec::new("A", &["x"]).row(vec![Value::Integer(1)]),
            TableSpec::new("B", &["x"]).row(vec![Value::Integer(2)]),
        ]);
        // Turn table A's root into an interior page whose only child is
        // itself: type byte, no cells, and the rightmost pointer aimed home.
        let root = {
            let db = Database::open(&bytes).expect("open");
            db.table("A").expect("look up").expect("present").root
        };
        let at = (root as usize - 1) * 4096;
        bytes[at] = 0x05;
        bytes[at + 3..at + 5].copy_from_slice(&0u16.to_be_bytes());
        bytes[at + 8..at + 12].copy_from_slice(&root.to_be_bytes());

        let db = Database::open(&bytes).expect("open");
        let table = db.table("A").expect("look up").expect("present");
        let err = db.rows(&table).unwrap_err();
        assert!(err.to_string().contains("twice"), "{err}");
    }

    #[test]
    fn a_child_past_the_end_of_the_file_is_refused() {
        let mut bytes = database(&[TableSpec::new("A", &["x"]).row(vec![Value::Integer(1)])]);
        let root = {
            let db = Database::open(&bytes).expect("open");
            db.table("A").expect("look up").expect("present").root
        };
        let at = (root as usize - 1) * 4096;
        bytes[at] = 0x05;
        bytes[at + 3..at + 5].copy_from_slice(&0u16.to_be_bytes());
        bytes[at + 8..at + 12].copy_from_slice(&9999u32.to_be_bytes());

        let db = Database::open(&bytes).expect("open");
        let table = db.table("A").expect("look up").expect("present");
        assert!(db.rows(&table).unwrap_err().to_string().contains("9999"));
    }

    /// A cell pointer is bounds-checked against the *usable area*, which says
    /// nothing about the bytes that follow it — so a pointer two bytes short of
    /// the end of the page used to slice past it and panic. These are files a
    /// stranger wrote and a panic takes the whole application down, with every
    /// unsaved document in it, because somebody opened the wrong one.
    #[test]
    fn a_cell_pointer_at_the_very_end_of_a_page_is_refused_rather_than_read_past() {
        let mut bytes = database(&[TableSpec::new("A", &["x"]).row(vec![Value::Integer(1)])]);
        let root = {
            let db = Database::open(&bytes).expect("open");
            db.table("A").expect("look up").expect("present").root
        };
        let at = (root as usize - 1) * 4096;
        // One interior cell whose four-byte child pointer would end two bytes
        // past the page.
        bytes[at] = 0x05;
        bytes[at + 3..at + 5].copy_from_slice(&1u16.to_be_bytes());
        bytes[at + 12..at + 14].copy_from_slice(&4094u16.to_be_bytes());

        let db = Database::open(&bytes).expect("open");
        let table = db.table("A").expect("look up").expect("present");
        let err = db.rows(&table).unwrap_err();
        assert!(err.to_string().contains("child pointer"), "{err}");
    }

    /// The other half of the same hole, and the one a real `.sut` would reach
    /// first: a record that spills into an overflow chain keeps the pointer to
    /// it in the four bytes *after* its local payload, which the "does the
    /// record fit in the usable area" check does not cover.
    #[test]
    fn an_overflow_pointer_past_the_end_of_a_page_is_refused_rather_than_read_past() {
        let mut bytes = database(&[TableSpec::new("A", &["x"]).row(vec![Value::Integer(1)])]);
        let root = {
            let db = Database::open(&bytes).expect("open");
            db.table("A").expect("look up").expect("present").root
        };
        let at = (root as usize - 1) * 4096;

        // Hand-built, because a page SQLite itself laid out cannot show this:
        // it packs the last cell flush to the end, which puts the pointer
        // exactly on the boundary. A record declaring 4062 bytes keeps
        // `min_local` of them here — 489 on a 4096-byte page — so a cell placed
        // at 3601 ends its payload at 4093 and its overflow pointer one byte
        // past the page, while still satisfying the "the record fits in the
        // usable area" check that is the only bound on this read.
        let page = &mut bytes[at..at + 4096];
        page.fill(0);
        page[0] = 0x0d;
        page[3..5].copy_from_slice(&1u16.to_be_bytes());
        page[5..7].copy_from_slice(&3601u16.to_be_bytes());
        page[8..10].copy_from_slice(&3601u16.to_be_bytes());
        // varint(4062), then varint(rowid 1).
        page[3601..3604].copy_from_slice(&[0x9f, 0x5e, 0x01]);

        let db = Database::open(&bytes).expect("open");
        let table = db.table("A").expect("look up").expect("present");
        let err = db.rows(&table).unwrap_err();
        assert!(err.to_string().contains("overflow pointer"), "{err}");
    }

    /// An index b-tree holds the same rows in a different order, so following
    /// one would produce duplicates rather than more data. Refusing says which
    /// page, which is what makes a surprising file diagnosable.
    #[test]
    fn an_index_page_is_refused_rather_than_walked() {
        let mut bytes = database(&[TableSpec::new("A", &["x"]).row(vec![Value::Integer(1)])]);
        let root = {
            let db = Database::open(&bytes).expect("open");
            db.table("A").expect("look up").expect("present").root
        };
        bytes[(root as usize - 1) * 4096] = 0x0a;

        let db = Database::open(&bytes).expect("open");
        let table = db.table("A").expect("look up").expect("present");
        let err = db.rows(&table).unwrap_err();
        assert!(err.to_string().contains("index page"), "{err}");
    }
}
