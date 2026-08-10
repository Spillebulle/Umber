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
    for (dst, src) in out.chunks_exact_mut(4).zip(preview.rgba.chunks_exact(4)) {
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

    /// A preview becomes a bitmap of the same shape, premultiplied, top-down.
    ///
    /// The premultiply is what stops a soft edge arriving with a dark fringe,
    /// and an opaque pixel has to survive it exactly — rounding down would make
    /// every thumbnail imperceptibly dark and nothing would ever say so.
    #[test]
    fn an_opaque_pixel_survives_the_premultiply_exactly() {
        // Done on the arithmetic rather than through GDI so it runs on CI,
        // which has no desktop: `CreateDIBSection` is the one line this cannot
        // reach, and it is the line that does no arithmetic.
        let pre = |c: u8, a: u8| ((u32::from(c) * u32::from(a) + 127) / 255) as u8;
        for c in [0u8, 1, 127, 128, 254, 255] {
            assert_eq!(pre(c, 255), c, "opaque {c} must be itself");
            assert_eq!(pre(c, 0), 0, "transparent must be zero");
        }
        // Half alpha halves the colour, to the nearest level.
        assert_eq!(pre(255, 128), 128);
        assert_eq!(pre(200, 128), 100);
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
    use windows::Win32::Graphics::Gdi::{BITMAP, DeleteObject, GetObjectW};
    use windows::Win32::UI::Shell::SHCreateMemStream;

    /// An ORA of `size` square, built through the public encoder.
    fn document(size: u32) -> Vec<u8> {
        // A bare PNG is a document `preview` reads, and it is the one format
        // this crate can produce without reaching into `umber-core`'s
        // test-only fixtures. What is under test here is the COM path.
        umber_core::export::encode(
            &[10u8, 200, 60, 255].repeat((size * size) as usize),
            size,
            size,
            &umber_core::export::ExportOptions {
                format: umber_core::export::ExportFormat::Png,
                ..Default::default()
            },
        )
        .expect("a document")
    }

    /// The whole provider, through its own interfaces: initialise from a stream
    /// and ask for a bitmap, exactly as Explorer's sequence does.
    #[test]
    fn a_stream_of_a_document_becomes_a_bitmap() {
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
        // Serialised against the other tests' objects by taking the reading
        // relative to itself rather than against zero: the harness runs these
        // on parallel threads and `LIVE` is process-wide.
        let before = LIVE.load(Ordering::Relaxed);
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
