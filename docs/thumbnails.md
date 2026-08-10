# Thumbnails in the file manager

What is built, and what macOS would take.

Umber reads five document formats and, until this was done, every one of them
drew the operating system's generic page icon in a file manager. The complaint
that started it was a folder of `.clip` files that all looked identical.

## The shared half: never composite

**Every format Umber reads already stores a flattened preview**, because every
application that wrote one needed the same picture for its own browser:

| | where |
|---|---|
| `.ora` | `mergedimage.png`, which the specification requires |
| `.kra` | `mergedimage.png`, beside the layer tiles |
| `.clip` | the `CanvasPreview` table's `ImageData`, a PNG |
| `.psd` | the composite section, which is what `Psd::rgba` returns |
| `.png` | itself |

`umber_core::docimport::preview` reads one entry and decodes one image.
Nothing in it walks a layer stack, allocates a canvas or touches the GPU.

That is not an optimisation, it is what makes the feature possible at all. The
alternative — `docimport::import`, then the composite — allocates a canvas-sized
buffer per layer, which `survey-documents` measures at **12.3 GB** for one real
document on this machine. No file manager can be asked to pay that, and on
Windows the code runs inside Explorer's own process.

Measured over 33 real `.clip` files with `survey-previews`: every one carries a
preview, 1250 to 1920 px on its long edge, worst case 155 ms of which nearly all
is reading a 300 MB file off disk (about 0.4 ms/MB). The decode itself is
nothing.

The cost of the choice is stated at the module: this is whatever the writing
application last saved. It can be stale, and a `.clip` thumbnail is Clip
Studio's rendering rather than Umber's. Right for a thumbnail, wrong for
anything else — **nothing that decides pixels may read it.**

## Linux: built

`umber --thumbnail <input> <output.png> <size>`, named by a `.thumbnailer` entry
under `share/thumbnailers/`. Nautilus, Thunar, Nemo, Caja and PCManFM all read
that directory, so one file covers the desktops people use.

A mode of the one binary rather than a second executable, which is the shape
`umber-app` already keeps for `--crash-report` and `--install-update`.

`image/png` is deliberately not claimed: gdk-pixbuf already draws a PNG
in-process, so claiming it would make Umber the slower answer to a question
already answered.

The Flatpak installs the file and it does **not** work from inside the sandbox:
the host's file manager runs its own thumbnailers and cannot see
`/app/bin/umber`. It is carried so a Flatpak is not silently missing what the
other packages have. Making it work needs a host-visible helper, which is a
sandbox hole rather than a packaging line, and has not been taken.

## Windows: built

Explorer has no command-line contract; it wants an in-process COM server.
`umber-shellext` is a DLL exposing `IThumbnailProvider` and
`IInitializeWithStream`, registered by the MSI.

**On the extension keys, not on Umber's ProgIDs.** Explorer resolves a handler
from the ProgID that owns the type first and falls back to the extension. So
Photoshop keeps drawing `.psd` where it is installed, and Umber fills the gap
for a `.clip`. Registering on `Umber.psd` would do nothing at all unless
somebody had made Umber the default, which the associations deliberately avoid.

`IInitializeWithStream` rather than `IInitializeWithFile` is the safer of the
two: a stream-initialised handler runs in Explorer's isolated surrogate, and it
never opens a path the shell did not already hand over.

What is verified without installing: the whole COM sequence driven in-process,
`DllGetClassObject` handing out the class and refusing one it does not have, a
null out-parameter answering `E_POINTER`, rubbish producing no thumbnail and no
panic, and the unload count holding the DLL while an object is alive. What
cannot be: whether Explorer chooses to call it. That needs an install.

## macOS: not built, and here is what it needs

Quick Look is the mechanism, and the blockers are packaging rather than code.

**1. There is no `.app` bundle.** The release ships a bare `lipo`'d binary in a
tarball. A Quick Look thumbnail extension is an `.appex` — an application
extension — and an `.appex` can only be delivered inside a host `.app`'s
`Contents/PlugIns/`. So the bundle has to exist first.

Building one is not large in itself: a `Contents/MacOS/umber`, an `Info.plist`
naming `CFBundleIdentifier` (`io.github.spillebulle.umber`, the same string
`taskbar::APP_ID` already carries everywhere else), an icon set, and the
directory layout. `release.yml` would produce a `.app` and zip that instead of
the bare binary.

**It would also unblock file associations on macOS**, which are
`CFBundleDocumentTypes` in the same `Info.plist` and are the other half of what
this repository now does on Windows and Linux. That makes the bundle worth more
than the thumbnails alone.

**2. The extension has to be signed.** macOS refuses to load an unsigned app
extension, and Gatekeeper refuses an unsigned bundle downloaded from the
internet without an explicit override. Umber signs nothing today — the update
checker's own documentation says so and is careful never to imply otherwise.
Signing means an Apple Developer identity, a certificate in CI, and notarisation
on every release.

**3. Nobody working on Umber has a Mac**, which is recorded elsewhere in
`CLAUDE.md` for the clipboard's macOS path and applies with more force here: a
Quick Look extension that has never been run is not something to put in a
release.

### The shape it would take

`QLThumbnailProvider`, a subclass in a small Objective-C or Swift target, whose
`provideThumbnail(for:)` calls into a C ABI exposed by a Rust staticlib over
`docimport::preview` — the same extractor both other platforms use. The
`Info.plist` declares `QLSupportedContentTypes` with the four UTIs, and `.clip`
needs a `UTExportedTypeDeclarations` entry because Apple has no type for it,
exactly as `packaging/*.mime.xml` declares one for freedesktop.

So the work is: the bundle, a signing identity, a small extension target, and a
machine to run it on. The extractor is already written and is the part that
would otherwise have been hard.
