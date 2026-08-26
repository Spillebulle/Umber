//! Umber's Windows thumbnail handler.
//!
//! Explorer has no command-line thumbnailer contract — the thing every Linux
//! desktop has and `umber-app`'s `thumbnail` module answers. What it wants is
//! an **in-process COM server**: a DLL exposing [`IThumbnailProvider`], which
//! Explorer loads inside its own `dllhost.exe` surrogate and calls once per
//! file. So this crate exists for one reason, and it is a DLL with no binary in
//! it.
//!
//! # What it must never do
//!
//! This code runs in a process nobody here owns, next to every other shell
//! extension on the machine, on the thread that draws somebody's folder. Three
//! rules follow and all three are structural rather than remembered:
//!
//! * **No GPU.** [`preview`] reads the flattened picture the document already
//!   carries, so there is no wgpu in this crate's dependency tree at all — it
//!   depends on `umber-core` and nothing else of Umber's.
//! * **No panics across the boundary.** A panic unwinding into COM is
//!   undefined behaviour, so every entry point catches. See [`guard`].
//! * **No slow paths.** A full import is 12.3 GB for one real document;
//!   extracting the embedded preview is milliseconds. `survey-previews`
//!   measures it.
//!
//! # Registration is the installer's, not this DLL's
//!
//! There is deliberately no `DllRegisterServer`. The MSI writes the registry
//! directly, which is what Windows Installer asks for — self-registration is
//! opaque to the transaction, cannot be rolled back and is why `regsvr32` state
//! outlives uninstalls. `packaging/windows/umber.wxs` holds the keys.
//!
//! **Registered on the *extension* rather than on Umber's ProgID**, which is
//! the same "offer, do not take" rule the file associations follow and it
//! matters more here. Explorer resolves a thumbnail handler from the ProgID
//! that actually owns the type first, and falls back to the extension key. So
//! writing the extension key means: where Photoshop is installed and provides
//! its own handler, Photoshop's is used; where nothing does — which is a `.clip`
//! on the machine this was reported from — Umber's fills the gap. Registering
//! on `Umber.psd` instead would do nothing at all unless somebody had made
//! Umber the default.

#![cfg(windows)]
// **LNK4104, and it is the linker asking for something Rust cannot express.**
// MSVC knows `DllGetClassObject` and `DllCanUnloadNow` as COM's own entry
// points and wants them marked `PRIVATE` in the export table — meaning
// "reachable by `GetProcAddress`, never linkable by name", which is exactly how
// COM calls them. Rust's generated `.def` has no way to say `PRIVATE`, so the
// warning is unavoidable and is about the *import library* rather than about
// the exports, which are correct and are what COM will find.
//
// Allowed rather than left to fail, because CI builds with `-D warnings` and
// `linker_messages` is a rustc lint: without this the whole release build stops
// on a note about a file nobody consumes. Narrowed to this crate, which is the
// only one that exports a C symbol at all.
#![allow(linker_messages)]

use std::sync::atomic::{AtomicUsize, Ordering};

use windows::Win32::Foundation::{E_FAIL, E_INVALIDARG, E_POINTER, S_OK};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateDIBSection, DIB_RGB_COLORS, HBITMAP,
};
use windows::Win32::System::Com::IStream;
use windows::Win32::System::Com::STATFLAG_NONAME;
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::Win32::UI::Shell::PropertiesSystem::{
    IInitializeWithStream, IInitializeWithStream_Impl,
};
use windows::Win32::UI::Shell::{IThumbnailProvider, IThumbnailProvider_Impl, WTS_ALPHATYPE};
use windows::core::{GUID, HRESULT, IUnknown, Interface, Ref, implement};

use umber_core::docimport::preview::{self, Preview};

/// Umber's thumbnail handler class.
///
/// **This value is a promise.** It is written into the registry by the
/// installer and read by Explorer, so it may never change: a new one would
/// leave the old key behind pointing at a class the DLL no longer exposes,
/// which is a shell extension that fails to load on every file. The same rule
/// the MSI's `UpgradeCode` lives by, and it is stated in both places.
///
/// `packaging/windows/umber.wxs` carries the text form and
/// `the_thumbnail_handler_clsid_matches_the_installer` pins the two together.
pub const CLSID_THUMBNAIL: GUID = GUID::from_u128(0x9c2f5b31_4de8_47a6_b0d1_5e3a8f7c26b4);

/// How many objects this DLL is keeping alive.
///
/// COM asks through `DllCanUnloadNow` whether it may unmap us; answering yes
/// while an object is still live frees the vtable somebody is about to call.
static LIVE: AtomicUsize = AtomicUsize::new(0);

/// Take the right to be the only test holding a [`ThumbnailProvider`].
///
/// **A test that writes a process-global must take a lock**, and [`LIVE`] is
/// one: every `ThumbnailProvider` in this file raises it and every drop lowers
/// it, while the harness runs the tests on parallel threads. Being an
/// `AtomicUsize` makes each *write* well defined and does nothing whatever for
/// the assertions, which are the actual claim — `the_dll_says_when_it_may_be_
/// unloaded` reads the counter, makes an object, and asserts the counter came
/// back to where it started, and a sibling holding an object of its own across
/// that window makes it false.
///
/// The comment on that test used to say it was safe because the reading was
/// "relative to itself". Relative to itself is exactly what it cannot be: the
/// two loads are a whole object's lifetime apart and anything may happen
/// between them. **Seven tests here build a provider** — six directly, and
/// `the_dll_hands_out_the_class_it_is_registered_as` through the class factory,
/// which is the one easiest to miss when counting.
///
/// It lives beside the counter rather than inside `mod tests` for
/// `prefs::prefs_lock`'s reason — the rule is about everything that touches the
/// global, not about one module's idea of who does. **The rule is also per test
/// binary**, and this crate is a third binary that never had one; `umber-app`'s
/// `gputest` is the second, and it exists because the same rule went unapplied
/// there until a runner died at process exit.
///
/// Poisoning is recovered from, so one failing test reports its own assertion
/// rather than turning every later one into a mutex error.
#[cfg(test)]
fn live_lock() -> std::sync::MutexGuard<'static, ()> {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Run `body`, turning a panic into an error rather than letting it out.
///
/// **Unwinding across an `extern "system"` boundary is undefined behaviour**,
/// and the boundary here is Explorer. Every entry point below is wrapped, and
/// the failure a caller sees is an ordinary `E_FAIL` — a file with no
/// thumbnail, which is what it had before this DLL existed.
fn guard<T>(what: &str, body: impl FnOnce() -> windows::core::Result<T>) -> windows::core::Result<T>
where
    T: Default,
{
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        Ok(result) => result,
        Err(_) => {
            log::error!("umber-shellext: panic in {what}");
            Err(E_FAIL.into())
        }
    }
}

// ---------------------------------------------------------------------------
// The provider
// ---------------------------------------------------------------------------

/// One thumbnail request: the bytes Explorer handed over, then the picture.
#[implement(IThumbnailProvider, IInitializeWithStream)]
#[derive(Default)]
pub struct ThumbnailProvider {
    /// The document, as read from the stream Explorer supplied.
    ///
    /// A `Mutex` rather than a bare field because COM objects are
    /// `Send`-shaped by the apartment model and `#[implement]` gives out `&self`
    /// — the interior mutability is what lets `Initialize` fill it in.
    document: std::sync::Mutex<Option<Vec<u8>>>,
}

impl ThumbnailProvider {
    fn new() -> Self {
        LIVE.fetch_add(1, Ordering::Relaxed);
        Self::default()
    }
}

impl Drop for ThumbnailProvider {
    fn drop(&mut self) {
        LIVE.fetch_sub(1, Ordering::Relaxed);
    }
}

impl IInitializeWithStream_Impl for ThumbnailProvider_Impl {
    /// Take the whole document into memory.
    ///
    /// **`IInitializeWithStream` rather than `IInitializeWithFile`**, and that
    /// is the safer of the two rather than the more convenient: a stream-
    /// initialised handler runs in Explorer's *isolated* surrogate, where a
    /// file-initialised one may be loaded straight into `explorer.exe`. It also
    /// means this never opens a path itself, so there is no file the shell did
    /// not already decide we may read.
    ///
    /// The whole stream, because every reader below wants a slice: an ORA is a
    /// ZIP whose directory is at the end, and a `.clip`'s chunk stream is
    /// walked from the front. Measured at about 0.4 ms/MB, which is the cost of
    /// the read itself.
    fn Initialize(&self, stream: Ref<'_, IStream>, _mode: u32) -> windows::core::Result<()> {
        guard("Initialize", || {
            let stream = stream.ok()?;

            // Ask how long it is so the buffer is allocated once. A stream that
            // will not answer is read in chunks instead rather than refused.
            let length = unsafe {
                let mut stat = Default::default();
                match stream.Stat(&mut stat, STATFLAG_NONAME) {
                    Ok(()) => usize::try_from(stat.cbSize).unwrap_or(0),
                    Err(_) => 0,
                }
            };

            let mut bytes = Vec::with_capacity(length.min(MAX_DOCUMENT_BYTES));
            let mut chunk = vec![0u8; 64 * 1024];
            loop {
                let mut read = 0u32;
                // SAFETY: `chunk` is a live buffer of `chunk.len()` bytes and
                // `read` is a live `u32`; both outlive the call.
                unsafe {
                    stream
                        .Read(
                            chunk.as_mut_ptr().cast(),
                            chunk.len() as u32,
                            Some(&mut read),
                        )
                        .ok()?;
                }
                if read == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..read as usize]);
                // A hostile or absurd file must not take the shell's memory
                // with it. Past the bound there is no thumbnail, which is the
                // state every one of these files is in today anyway.
                if bytes.len() > MAX_DOCUMENT_BYTES {
                    log::warn!("umber-shellext: document past {MAX_DOCUMENT_BYTES} bytes");
                    return Err(E_FAIL.into());
                }
            }

            *self.document.lock().map_err(|_| E_FAIL)? = Some(bytes);
            Ok(())
        })
    }
}

/// The largest document this will read into Explorer's process.
///
/// Not a limit on what Umber opens — the editor reads far larger — but on what
/// is worth pulling into somebody else's process to draw a 256-pixel picture.
/// The largest real document measured here is 307 MB, so this is generous; past
/// it the file keeps the generic icon it has today.
const MAX_DOCUMENT_BYTES: usize = 1 << 30;

impl IThumbnailProvider_Impl for ThumbnailProvider_Impl {
    /// Hand back a bitmap no larger than `cx` on its longest edge.
    ///
    /// The raw pointers are COM's and the signature is not ours to change —
    /// clippy's `not_unsafe_ptr_arg_deref` wants the function marked `unsafe`,
    /// which a trait implementation cannot be. Both are checked for null before
    /// anything is written through them, which is the whole of what the lint is
    /// asking for.
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    fn GetThumbnail(
        &self,
        cx: u32,
        phbmp: *mut HBITMAP,
        pdwalpha: *mut WTS_ALPHATYPE,
    ) -> windows::core::Result<()> {
        guard("GetThumbnail", || {
            if phbmp.is_null() || pdwalpha.is_null() {
                return Err(E_POINTER.into());
            }
            let guard = self.document.lock().map_err(|_| E_FAIL)?;
            let bytes = guard
                .as_ref()
                .ok_or_else(|| windows::core::Error::from(E_FAIL))?;

            // The format is not known here: Explorer hands over a stream and
            // never says what it is. Every reader is tried in turn, cheapest
            // first, which is also what lets one registration serve four
            // extensions. A file that is none of them simply has no thumbnail.
            let preview = read_any(bytes).ok_or_else(|| windows::core::Error::from(E_FAIL))?;
            let preview = preview.fit_within(cx);

            let bitmap = to_bitmap(&preview)?;
            // SAFETY: both pointers were checked non-null above and are the
            // out-parameters Explorer supplied for exactly this.
            unsafe {
                *phbmp = bitmap;
                // The picture carries alpha: a document on transparency has to
                // composite onto whatever the folder view is drawn on rather
                // than arriving on black.
                *pdwalpha = WTSAT_ARGB;
            }
            Ok(())
        })
    }
}

/// `WTS_ALPHATYPE`'s "this bitmap has an alpha channel".
///
/// Named here because the constant's own binding moves between `windows`
/// releases and a wrong value is a thumbnail drawn on black.
const WTSAT_ARGB: WTS_ALPHATYPE = WTS_ALPHATYPE(2);

/// Try each reader until one answers.
///
/// Explorer supplies bytes and no name, so the format has to be discovered.
/// The order is cheapest-first by what each has to look at: the two container
/// formats check a signature in the first few bytes, the `.clip` walk reads a
/// chunk header, and the PSD parse is last because it is the one that decodes.
fn read_any(bytes: &[u8]) -> Option<Preview> {
    use umber_core::docimport::SourceFormat::*;
    for format in [OpenRaster, Krita, ClipStudio, Png, Photoshop] {
        if let Ok(found) = preview::from_bytes(bytes, format) {
            return Some(found);
        }
    }
    None
}

/// Turn a preview into the 32-bit bitmap Explorer wants.
///
/// A top-down DIB section in **BGRA with premultiplied alpha**, which is what
/// `WTSAT_ARGB` means in practice: the shell composites the result, and
/// straight alpha there produces a dark fringe on every soft edge. The
/// premultiply is the same arithmetic `srgb` does for a layer, done here on
/// eight-bit sRGB deliberately — the shell's compositor works in the encoded
/// space, so converting to linear and back would be wrong in the other
/// direction.
fn to_bitmap(preview: &Preview) -> windows::core::Result<HBITMAP> {
    let (width, height) = (preview.size.x, preview.size.y);
    if width == 0 || height == 0 {
        return Err(E_INVALIDARG.into());
    }

    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            // Negative: a DIB is bottom-up unless it is told otherwise, and a
            // preview's first row is its top one. Getting this wrong is a
            // thumbnail that is upside down and nothing else.
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut pixels: *mut core::ffi::c_void = std::ptr::null_mut();
    // SAFETY: `info` describes a 32-bit top-down DIB of the size above, and
    // `pixels` receives the buffer the call allocates. A null device context
    // asks for a DIB that is not tied to one, which is what a thumbnail is.
    let bitmap = unsafe { CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut pixels, None, 0)? };
    if pixels.is_null() {
        return Err(E_FAIL.into());
    }

    let count = width as usize * height as usize;
    // SAFETY: `CreateDIBSection` allocated exactly `count` 32-bit pixels for
    // the header above, and nothing else holds the buffer yet.
    let out = unsafe { std::slice::from_raw_parts_mut(pixels.cast::<u8>(), count * 4) };
    for (dst, src) in out
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(preview.rgba.as_chunks::<4>().0.iter())
    {
        let a = u32::from(src[3]);
        // Rounded rather than truncated, so an opaque pixel is exactly itself.
        let pre = |c: u8| ((u32::from(c) * a + 127) / 255) as u8;
        dst[0] = pre(src[2]);
        dst[1] = pre(src[1]);
        dst[2] = pre(src[0]);
        dst[3] = src[3];
    }
    Ok(bitmap)
}

// ---------------------------------------------------------------------------
// The class factory, and what a DLL has to export
// ---------------------------------------------------------------------------

#[implement(IClassFactory)]
struct Factory;

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<'_, IUnknown>,
        iid: *const GUID,
        object: *mut *mut core::ffi::c_void,
    ) -> windows::core::Result<()> {
        guard("CreateInstance", || {
            if object.is_null() {
                return Err(E_POINTER.into());
            }
            // SAFETY: checked non-null; this is the out-parameter COM supplied.
            unsafe { *object = std::ptr::null_mut() };
            if outer.is_some() {
                // Aggregation, which this object does not support. The
                // documented answer rather than a generic failure.
                return Err(windows::Win32::Foundation::CLASS_E_NOAGGREGATION.into());
            }
            let provider: IThumbnailProvider = ThumbnailProvider::new().into();
            // SAFETY: `iid` is COM's, and `query` writes through `object`,
            // which was checked above.
            unsafe { provider.query(iid, object).ok() }
        })
    }

    fn LockServer(&self, lock: windows::core::BOOL) -> windows::core::Result<()> {
        if lock.as_bool() {
            LIVE.fetch_add(1, Ordering::Relaxed);
        } else {
            LIVE.fetch_sub(1, Ordering::Relaxed);
        }
        Ok(())
    }
}

/// COM's way in. The only class this DLL exposes is [`CLSID_THUMBNAIL`].
///
/// # Safety
///
/// Called by COM with pointers it owns; `ppv` receives the interface.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut core::ffi::c_void,
) -> HRESULT {
    let result = guard("DllGetClassObject", || {
        if rclsid.is_null() || riid.is_null() || ppv.is_null() {
            return Err(E_POINTER.into());
        }
        // SAFETY: checked non-null just above.
        unsafe {
            *ppv = std::ptr::null_mut();
            if *rclsid != CLSID_THUMBNAIL {
                return Err(windows::Win32::Foundation::CLASS_E_CLASSNOTAVAILABLE.into());
            }
            let factory: IClassFactory = Factory.into();
            factory.query(riid, ppv).ok()
        }
    });
    match result {
        Ok(()) => S_OK,
        Err(e) => e.code(),
    }
}

/// Whether COM may unmap this DLL.
///
/// # Safety
///
/// Called by COM on a thread of its choosing.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllCanUnloadNow() -> HRESULT {
    if LIVE.load(Ordering::Relaxed) == 0 {
        S_OK
    } else {
        windows::Win32::Foundation::S_FALSE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The identifier Explorer is told to look for. It is in the registry as
    /// text, so it may never change; see the constant's own note.
    #[test]
    fn the_thumbnail_handler_clsid_matches_the_installer() {
        let wxs = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../packaging/windows/umber.wxs"),
        )
        .expect("the WiX source");

        // The dashed, upper-case form the registry uses, taken off the constant
        // rather than typed out — a copy here would agree with itself while the
        // installer drifted.
        let formatted = format!("{CLSID_THUMBNAIL:?}").to_uppercase();
        assert!(
            wxs.to_uppercase().contains(&formatted),
            "the installer registers a different class than this DLL exposes: {formatted}"
        );
    }

    /// Every format the previewer reads is tried, so one registration serves
    /// all four extensions — Explorer supplies bytes and never says which.
    #[test]
    fn discovery_covers_every_format_the_previewer_reads() {
        use umber_core::docimport::SourceFormat;
        let tried = [
            SourceFormat::OpenRaster,
            SourceFormat::Krita,
            SourceFormat::ClipStudio,
            SourceFormat::Png,
            SourceFormat::Photoshop,
        ];
        assert_eq!(
            tried.len(),
            umber_core::docimport::supported_extensions().len(),
            "a format was added to the importer and not to `read_any`, so files \
             of it would get no thumbnail"
        );
    }

    /// Nothing readable in, nothing out — and no panic, which is the property
    /// that matters inside Explorer.
    #[test]
    fn a_file_that_is_no_format_at_all_has_no_preview() {
        assert!(read_any(b"not a document").is_none());
        assert!(read_any(&[]).is_none());
    }
}

#[cfg(test)]
mod com_tests {
    //! Driving the COM object the way Explorer does, without registering it.
    //!
    //! Everything above is reachable from an ordinary test *except* the part
    //! that matters most: whether the vtables, the stream read and the bitmap
    //! actually work when called through the interfaces. That does not need the
    //! registry — the class can be created directly, and through the exported
    //! `DllGetClassObject` — so the one thing that genuinely requires
    //! installing is whether Explorer chooses to call us, and nothing else.

    use super::*;
    use windows::Win32::Graphics::Gdi::{BITMAP, DIBSECTION, DeleteObject, GetObjectW};
    use windows::Win32::UI::Shell::SHCreateMemStream;

    /// A PNG of `width` × `height` holding exactly `rgba`, straight-alpha sRGB.
    ///
    /// A bare PNG is a document `preview` reads, and it is the one format this
    /// crate can produce without reaching into `umber-core`'s test-only
    /// fixtures. What is under test is the COM path and what comes out the far
    /// end of it, so the bytes have to survive the encode unchanged — which
    /// `a_document_round_trips_its_pixels_through_the_encoder` is what checks,
    /// because every pixel assertion below rests on it.
    fn png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
        umber_core::export::encode(
            rgba,
            width,
            height,
            &umber_core::export::ExportOptions {
                format: umber_core::export::ExportFormat::Png,
                ..Default::default()
            },
        )
        .expect("a document")
    }

    /// A square, single-colour, fully opaque document — for the tests that are
    /// about the COM plumbing rather than about the picture.
    fn document(size: u32) -> Vec<u8> {
        png(
            size,
            size,
            &[10u8, 200, 60, 255].repeat((size * size) as usize),
        )
    }

    // ----------------------------------------------------------- the picture

    /// **Three wide and two tall, six pixels no two of which agree.**
    ///
    /// Every property `to_bitmap` has is a property of *where a number ends
    /// up*, so a fixture that is square, or one colour, or wholly opaque
    /// cannot see any of them. This one is none of the three, and each choice
    /// buys a specific mutation:
    ///
    /// * **Non-square** — transposing `biWidth` and `biHeight` is caught.
    /// * **Distinct per channel** — dropping the BGRA swap is caught. Every
    ///   pixel has a different value in each of red, green and blue, so a
    ///   channel that arrived from the wrong place is a wrong number rather
    ///   than a coincidence.
    /// * **One pixel at alpha 200** — the premultiply's rounding is caught.
    ///   `200 × 200 = 40000`, which is `156.86` of 255: rounded is 157 and
    ///   truncated is 156, so dropping the `+ 127` moves that byte. Its green
    ///   and blue deliberately do *not* move, which is why the red is the one
    ///   the assertion turns on.
    /// * **One pixel fully transparent, and it is not black** — a premultiply
    ///   that was skipped leaves `(10, 20, 30)` standing where zero belongs.
    /// * **Two fully opaque pixels** — the property the deleted arithmetic test
    ///   used to claim: an opaque pixel is exactly itself, and `+ 127` rounding
    ///   is what makes that true rather than one level dark.
    ///
    /// Row-major, top row first, straight-alpha sRGB — the form a `Preview`
    /// holds.
    const SHAPE: (u32, u32) = (3, 2);
    #[rustfmt::skip]
    const PIXELS: [u8; 24] = [
        200,   0,   0, 255,   0, 210,   0, 255,   0,   0, 220, 255,
        200, 100,  50, 200,  10,  20,  30,   0, 255, 254, 253, 255,
    ];

    /// What [`to_bitmap`] must write, in the order a DIB holds it: **BGRA,
    /// premultiplied**, one row after another from the top.
    ///
    /// Worked out by hand rather than by calling the code under test, which is
    /// the whole point — `(c × a + 127) / 255` per channel, alpha carried
    /// through untouched.
    #[rustfmt::skip]
    const BGRA: [u8; 24] = [
        // Opaque: the colour itself, with blue and red exchanged.
          0,   0, 200, 255,    0, 210,   0, 255,  220,   0,   0, 255,
        // 50→39, 100→78, 200→157 at alpha 200; then zeroes; then near-white.
         39,  78, 157, 200,    0,   0,   0,   0,  253, 254, 255, 255,
    ];

    /// What came back out of GDI: the shape, the buffer, and whether GDI
    /// believes the buffer runs top row first.
    struct ReadBack {
        width: u32,
        height: u32,
        /// The bitmap's own storage, in the order [`to_bitmap`] wrote it.
        pixels: Vec<u8>,
        /// Whether this is a **top-down** DIB.
        ///
        /// **Not read off `dsBmih.biHeight`**, and that was measured rather
        /// than assumed: GDI hands that field back as `+2` for the bitmap this
        /// crate creates with `-2`, so the sign is normalised away and an
        /// assertion on it fails against correct code. Nor can the buffer
        /// settle it — the rows sit in memory in the same order either way,
        /// and the orientation is only ever a statement about how to *read*
        /// them.
        ///
        /// What does settle it is asking GDI to convert. `GetDIBits` into a
        /// **bottom-up** destination (a positive `biHeight`) returns the rows
        /// in the order that destination wants, flipping them where the source
        /// disagrees. So a source GDI holds as top-down comes back reversed
        /// against its own buffer, and a bottom-up one comes back identical:
        /// the flip is GDI's own answer to the question, rather than ours.
        ///
        /// **The comparison is on colour alone**, because that instrument
        /// carries a portability surface the rest of this test does not: a
        /// 32-bit `BI_RGB` copy out of `GetDIBits` is not contractually
        /// alpha-preserving, CI builds this crate for `windows-11-arm`, and no
        /// runner has ever executed it. Dropping the alpha byte from the
        /// comparison costs nothing — the flip is unambiguous from RGB, and
        /// every alpha assertion lives on `bmBits`, which is the section's own
        /// storage and goes through no conversion. What is kept as a backstop
        /// is the two-way test: an answer that is neither the buffer nor its
        /// mirror fails naming exactly that, rather than silently reporting
        /// "bottom-up".
        top_down: bool,
    }

    /// Read a bitmap back the way the shell would have to.
    fn read_back(bitmap: HBITMAP) -> ReadBack {
        use windows::Win32::Graphics::Gdi::{CreateCompatibleDC, DeleteDC, GetDIBits};

        let mut ds = DIBSECTION::default();
        let wrote = unsafe {
            GetObjectW(
                bitmap.into(),
                std::mem::size_of::<DIBSECTION>() as i32,
                Some((&raw mut ds).cast()),
            )
        };
        assert_eq!(
            wrote as usize,
            std::mem::size_of::<DIBSECTION>(),
            "GDI did not describe this as a DIB section"
        );
        assert!(!ds.dsBm.bmBits.is_null(), "a DIB section with no pixels");
        assert_eq!(ds.dsBm.bmBitsPixel, 32, "the shell was promised 32-bit");

        let (width, height) = (ds.dsBm.bmWidth, ds.dsBm.bmHeight);
        // `BITMAP::bmHeight` is documented positive and measures positive here.
        // Checked anyway, because this very struct's `dsBmih.biHeight` came
        // back with a sign the documentation did not lead us to expect — so
        // "GDI reports what the docs say" is a premise this function has
        // already seen fail once. A negative here would make `height as usize`
        // about 2^64 and the `from_raw_parts` below undefined behaviour rather
        // than a failed assertion.
        assert!(
            width > 0 && height > 0,
            "GDI reported a {width} x {height} bitmap"
        );
        let count = width as usize * height as usize * 4;
        // SAFETY: `bmBits` is the buffer `CreateDIBSection` allocated for a
        // 32-bit bitmap of the shape GDI has just reported, and the bitmap
        // outlives the copy.
        let pixels =
            unsafe { std::slice::from_raw_parts(ds.dsBm.bmBits.cast::<u8>(), count) }.to_vec();

        // Ask for the same pixels bottom-up and see whether GDI turns them
        // over. A memory device context, because none of this touches a screen.
        let dc = unsafe { CreateCompatibleDC(None) };
        assert!(!dc.is_invalid(), "no GDI device context on this machine");
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                // Positive: "give me these bottom-up".
                biHeight: height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bottom_up = vec![0u8; count];
        // SAFETY: `bottom_up` holds exactly the bytes `info` describes, and
        // both it and `info` outlive the call.
        let lines = unsafe {
            GetDIBits(
                dc,
                bitmap,
                0,
                height as u32,
                Some(bottom_up.as_mut_ptr().cast()),
                &mut info,
                DIB_RGB_COLORS,
            )
        };
        let _ = unsafe { DeleteDC(dc) };
        assert_eq!(lines, height, "GetDIBits did not return every row");

        // **Compared on colour alone, with the alpha byte dropped.** The
        // orientation is unambiguous from RGB with this fixture, and asking
        // `GetDIBits` for a 32-bit `BI_RGB` copy is not contractually
        // alpha-preserving — so a driver that zeroed that byte would turn a
        // correct build red for a reason that has nothing to do with the code
        // under test. Nothing is given up: the alpha assertions live on
        // `bmBits`, which is the DIB section's own storage and goes through no
        // conversion at all. CI builds this crate for `windows-11-arm` and no
        // runner has ever executed this test, which is why the risk is removed
        // rather than documented.
        let stride = width as usize * 4;
        let rgb = |b: &[u8]| -> Vec<u8> {
            b.as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2]])
                .collect()
        };
        let upright = rgb(&pixels);
        let flipped: Vec<u8> = rgb(&pixels
            .chunks_exact(stride)
            .rev()
            .flatten()
            .copied()
            .collect::<Vec<u8>>());
        let seen = rgb(&bottom_up);
        // Without this the reading is a coin toss: a picture that is its own
        // mirror answers both ways at once, and the caller's fixture is the
        // only thing stopping it. The same trap as a square fixture one line
        // up, so it is refused here rather than remembered there.
        assert_ne!(
            flipped, upright,
            "this picture is its own vertical mirror, so nothing about it can \
             say which way up it is"
        );
        assert!(
            seen == flipped || seen == upright,
            "GetDIBits returned neither the buffer nor its mirror, so this \
             reading says nothing about the orientation"
        );
        ReadBack {
            width: width as u32,
            height: height as u32,
            top_down: seen == flipped,
            pixels,
        }
    }

    /// Every pixel assertion below rests on the encoder handing its bytes back,
    /// so that is checked rather than assumed. A PNG that quantised, matted or
    /// premultiplied would make [`BGRA`] wrong for a reason that has nothing to
    /// do with the code this file is guarding.
    #[test]
    fn a_document_round_trips_its_pixels_through_the_encoder() {
        let bytes = png(SHAPE.0, SHAPE.1, &PIXELS);
        let preview = read_any(&bytes).expect("a preview");
        assert_eq!((preview.size.x, preview.size.y), SHAPE);
        assert_eq!(
            preview.rgba, PIXELS,
            "the encoder did not preserve the fixture"
        );
    }

    /// **What Explorer is actually handed.** The whole route — a stream in, a
    /// GDI bitmap out — measured pixel by pixel against [`BGRA`].
    ///
    /// This is the only guard over `to_bitmap`, and it is deliberately one
    /// test rather than four: the four properties (the channel order, the
    /// axes, which way up, the rounding) are all statements about the same one
    /// buffer, and splitting them would mean four documents, four COM
    /// sequences and four chances to build a fixture too tame to see anything.
    ///
    /// Demonstrated by mutation, all four of them:
    ///
    /// * `dst[0] = pre(src[0]) … dst[2] = pre(src[2])` — the swap dropped.
    /// * `biHeight: height as i32` — bottom-up, an upside-down thumbnail.
    /// * `biWidth: height`, `biHeight: -(width)` — the axes transposed.
    /// * `(c * a) / 255` — the rounding dropped.
    ///
    /// Every one of them passed all eleven tests in this crate before this
    /// existed.
    #[test]
    fn the_bitmap_explorer_is_handed_is_the_picture_the_document_holds() {
        let _live = live_lock();
        let bytes = png(SHAPE.0, SHAPE.1, &PIXELS);
        let provider: IInitializeWithStream = ThumbnailProvider::new().into();
        // SAFETY: the slice outlives the call; `SHCreateMemStream` copies it.
        let stream = unsafe { SHCreateMemStream(Some(&bytes)) }.expect("a stream");
        unsafe { provider.Initialize(&stream, 0) }.expect("initialise");

        let thumbs: IThumbnailProvider = provider.cast().expect("both interfaces");
        let mut bitmap = HBITMAP::default();
        let mut alpha = WTS_ALPHATYPE(0);
        // A box larger than either edge, so `fit_within` is the identity and
        // the pixels that come back are the pixels that went in. Scaling is
        // `Preview`'s own rule and has its own tests; what is under test here
        // is everything after it.
        // SAFETY: both out-parameters are live for the call.
        unsafe { thumbs.GetThumbnail(8, &mut bitmap, &mut alpha) }.expect("a thumbnail");
        assert!(!bitmap.is_invalid(), "no bitmap came back");

        let back = read_back(bitmap);
        assert_eq!(
            (back.width, back.height),
            SHAPE,
            "the bitmap is not the shape of the picture — the axes are transposed"
        );
        assert!(
            back.top_down,
            "GDI holds this as a bottom-up DIB, so Explorer draws the thumbnail \
             upside down"
        );
        assert_eq!(
            back.pixels, BGRA,
            "the bytes Explorer composites are not the picture: expected BGRA, \
             premultiplied with rounding"
        );

        // Explorer owns the bitmap in the real flow; here the test does.
        let _ = unsafe { DeleteObject(bitmap.into()) };
    }

    /// The whole provider, through its own interfaces: initialise from a stream
    /// and ask for a bitmap, exactly as Explorer's sequence does.
    #[test]
    fn a_stream_of_a_document_becomes_a_bitmap() {
        let _live = live_lock();
        let bytes = document(64);
        let provider: IInitializeWithStream = ThumbnailProvider::new().into();

        // SAFETY: the slice outlives the call; `SHCreateMemStream` copies it.
        let stream = unsafe { SHCreateMemStream(Some(&bytes)) }.expect("a stream");
        unsafe { provider.Initialize(&stream, 0) }.expect("initialise");

        let thumbs: IThumbnailProvider = provider.cast().expect("the provider is both interfaces");
        let mut bitmap = HBITMAP::default();
        let mut alpha = WTS_ALPHATYPE(0);
        // SAFETY: both out-parameters are live for the call.
        unsafe { thumbs.GetThumbnail(32, &mut bitmap, &mut alpha) }.expect("a thumbnail");

        assert!(!bitmap.is_invalid(), "no bitmap came back");
        assert_eq!(alpha, WTSAT_ARGB, "the shell has to be told there is alpha");

        // The bitmap is the shape that was asked for, top-down and 32-bit.
        let mut info = BITMAP::default();
        let wrote = unsafe {
            GetObjectW(
                bitmap.into(),
                std::mem::size_of::<BITMAP>() as i32,
                Some((&raw mut info).cast()),
            )
        };
        assert!(wrote > 0, "GDI did not describe the bitmap");
        assert_eq!(info.bmWidth, 32);
        assert_eq!(info.bmHeight, 32);
        assert_eq!(info.bmBitsPixel, 32);

        // Explorer owns the bitmap in the real flow; here the test does.
        let _ = unsafe { DeleteObject(bitmap.into()) };
    }

    /// `GetThumbnail` before `Initialize` is a sequence Explorer will not
    /// produce and a fuzzer will. It must fail rather than read uninitialised
    /// state.
    #[test]
    fn asking_for_a_thumbnail_before_the_stream_fails_cleanly() {
        let _live = live_lock();
        let thumbs: IThumbnailProvider = ThumbnailProvider::new().into();
        let mut bitmap = HBITMAP::default();
        let mut alpha = WTS_ALPHATYPE(0);
        let result = unsafe { thumbs.GetThumbnail(32, &mut bitmap, &mut alpha) };
        assert!(result.is_err(), "an uninitialised provider must not answer");
    }

    /// A null out-parameter is `E_POINTER`, not a crash. Explorer will not do
    /// this either; something else on the machine might.
    #[test]
    fn a_null_out_parameter_is_refused() {
        let _live = live_lock();
        let bytes = document(8);
        let provider: IInitializeWithStream = ThumbnailProvider::new().into();
        let stream = unsafe { SHCreateMemStream(Some(&bytes)) }.expect("a stream");
        unsafe { provider.Initialize(&stream, 0) }.expect("initialise");

        let thumbs: IThumbnailProvider = provider.cast().expect("both interfaces");
        let mut alpha = WTS_ALPHATYPE(0);
        let result = unsafe { thumbs.GetThumbnail(32, std::ptr::null_mut(), &mut alpha) };
        assert_eq!(result.unwrap_err().code(), E_POINTER);
    }

    /// Bytes that are no document at all produce no thumbnail and no panic —
    /// the property that matters most, because this runs inside Explorer.
    #[test]
    fn rubbish_in_a_stream_produces_no_thumbnail_and_no_panic() {
        let _live = live_lock();
        let provider: IInitializeWithStream = ThumbnailProvider::new().into();
        let stream = unsafe { SHCreateMemStream(Some(b"not a document at all")) }.expect("stream");
        unsafe { provider.Initialize(&stream, 0) }.expect("initialise");

        let thumbs: IThumbnailProvider = provider.cast().expect("both interfaces");
        let mut bitmap = HBITMAP::default();
        let mut alpha = WTS_ALPHATYPE(0);
        assert!(unsafe { thumbs.GetThumbnail(32, &mut bitmap, &mut alpha) }.is_err());
    }

    /// **The exported entry point COM actually calls.** Everything above builds
    /// the object directly; this is the route Explorer takes — ask the DLL for
    /// a class factory by CLSID, then ask the factory for the object.
    #[test]
    fn the_dll_hands_out_the_class_it_is_registered_as() {
        // The factory builds a real provider, so this raises `LIVE` too.
        let _live = live_lock();
        let mut ptr: *mut core::ffi::c_void = std::ptr::null_mut();
        let hr = unsafe { DllGetClassObject(&CLSID_THUMBNAIL, &IClassFactory::IID, &mut ptr) };
        assert_eq!(hr, S_OK, "the DLL refused its own class");
        assert!(!ptr.is_null());

        // SAFETY: `DllGetClassObject` returned an owned `IClassFactory`.
        let factory: IClassFactory = unsafe { IClassFactory::from_raw(ptr) };
        let made: IThumbnailProvider = unsafe { factory.CreateInstance(None) }.expect("an object");
        // It really is the provider: it answers to the other interface too.
        let _: IInitializeWithStream = made.cast().expect("both interfaces");
    }

    /// A class this DLL does not expose is refused rather than handed something.
    #[test]
    fn the_dll_refuses_a_class_it_does_not_have() {
        let other = GUID::from_u128(0x00000000_0000_0000_0000_000000000001);
        let mut ptr: *mut core::ffi::c_void = std::ptr::null_mut();
        let hr = unsafe { DllGetClassObject(&other, &IClassFactory::IID, &mut ptr) };
        assert_ne!(hr, S_OK);
        assert!(ptr.is_null(), "a refusal must not leave a pointer behind");
    }

    /// While an object is alive COM may not unmap the DLL, and once it is gone
    /// it may. Answering yes too early frees a vtable somebody is about to
    /// call.
    #[test]
    fn the_dll_says_when_it_may_be_unloaded() {
        // **The reading is against zero, and it can be, because of the lock.**
        // This used to take it relative to itself and say that was what made it
        // safe from the harness's parallel threads. It is not: the two loads
        // are a whole object's lifetime apart, so a sibling test holding a
        // provider across that window makes the second one larger and the
        // assertion below false. Six other tests here build one.
        let _live = live_lock();
        let before = LIVE.load(Ordering::Relaxed);
        assert_eq!(before, 0, "the lock did not keep the other tests out");
        {
            let _held: IThumbnailProvider = ThumbnailProvider::new().into();
            assert!(LIVE.load(Ordering::Relaxed) > before);
            assert_eq!(
                unsafe { DllCanUnloadNow() },
                windows::Win32::Foundation::S_FALSE
            );
        }
        assert_eq!(LIVE.load(Ordering::Relaxed), before);
    }
}
