# Print and cut: registration marks, the print sheet, and the Silhouette

Project 4 of four on the way to cutting stickers on a Silhouette Portrait 3.
Projects 1 to 3 (vector layers, the Path tool, Trace outline) are merged.

## Intent

The artist draws a sheet of stickers, traces the cut lines, prints the sheet on
an ordinary printer, loads it into the Portrait 3 and presses one button in
Umber. The machine finds the printed registration marks and cuts along the Cut
paths, lined up with the print.

The artist approved this shape on 2026-11-26 ("Sounds good, proceed"). Whether
Silhouette Studio must keep working beside Umber was asked and not answered;
this design takes the answer that can be undone, which is yes: nothing here
replaces a driver.

## Goals

- A document can carry registration marks, drawn where the machine will look.
- A print sheet at exact physical size, marks included, cut lines left out.
- A cut job sent to a Portrait 3 over USB with no driver change.
- Cutting without marks as well (vinyl, card, a pen test on plain paper).
- Everything that can be decided without the machine is decided and tested
  without it; the part that cannot is small and says so.

## Non-goals (this project)

- Printing from inside Umber. The sheet is a PDF the artist prints at actual
  size. Driving a printer directly is a later piece.
- Other machines, Bluetooth, macOS and Linux transports. The encoder is not
  Windows-specific; the transport is.
- Material presets, overcut, weeding lines, tiling, a second tool holder,
  cutting Cut and Plot in one job (the Portrait 3 has one holder, so a tool
  change sits between them anyway).
- Clipping paths at the cuttable area. A path outside it refuses the job.
- Moving or resizing the marks. One layout, derived from the document size.

## Anti-goals

- A job retransmitted after an uncertain write. Half a sheet cut twice is
  worse than a sheet not cut.
- A driver swap that stops Silhouette Studio seeing the machine.
- A print sheet whose marks are not where the cut job says they are. Both
  come from one function.
- A control that claims the machine is ready, or a mark was found, on a reply
  it did not understand.
- Any command the protocol notes call permanent or calibration-only.

## Approaches

**Reaching the machine**

1. **The Windows USB printing interface the machine already has.** Open the
   device's own USBPRINT interface with `CreateFileW` and write the protocol
   to it. Chosen: no driver change, Studio keeps working (though not at the
   same instant), and one other person has reported it working on a Cameo 4.
   It has never been run on a Portrait 3, which is why a probe comes first.
2. **libusb after swapping the driver with Zadig.** What inkscape-silhouette
   documents. Rejected as the first route: it asks the artist to replace a
   driver, and is likely to cut Studio off until it is swapped back.
3. **Export a file for Silhouette Studio.** Rejected as the goal: it is not
   "send directly", and Studio's free tier does not open SVG. It remains the
   fallback if route 1 fails on this machine.

**Printing**

1. **A PDF at the sheet's exact size.** Chosen: it is bytes, so it is tested
   with no printer; it works on every platform; marks are vector. The cost is
   one instruction the artist has to follow: print at actual size.
2. **Print from Umber through the system dialog.** Removes that instruction.
   Windows-only, untestable here, and a large surface. Later.

**Where the sheet lives**

1. **The document is the sheet.** Chosen. The canvas dialogs already make a
   document at a paper size and resolution, so document pixels map to
   millimetres by the resolution and nothing has to be placed on anything.
2. **A separate sheet onto which a document is placed.** Rejected: a second
   coordinate space, a placement step and a layout editor, for nothing the
   first does not do.

## Design

### Units

`mm = pixels * 25.4 / dpi`, with `dpi` the document's resolution. The machine
counts in units of 0.05 mm (20 to the millimetre).

### Registration marks: `umber-core::sheet`

`Marks::for_page(width_mm, height_mm) -> Option<Marks>`, a pure function, and
the only statement of where the marks are.

- Inset `INSET_MM = 10.0` from the left and top edges, and the same from the
  right and bottom.
- Top left: a filled square, `SQUARE_MM = 5.0`, its top left corner at
  (inset, inset).
- Top right and bottom left: an L, each arm `ARM_MM = 20.0` long and
  `STROKE_MM = 0.5` thick, its corner at (width - inset, inset) and
  (inset, height - inset), arms pointing into the page.
- `span_x = width - 2 * inset`, `span_y = height - 2 * inset`.
- The **keep-clear** zones are the 20 mm squares at the three marked corners
  of the span rectangle. Artwork there can stop the sensor finding a mark.
- The **cut area** is the span rectangle.
- `None` where a span would be under `MIN_SPAN_MM = 60.0`, or where
  `span_x` exceeds `MAX_CUT_WIDTH_MM = 203.0`.

This is the layout inkscape-silhouette draws for the "Cameo, Portrait" mark
type, with the stroke its plot code announces to the machine (0.5 mm).

A document has marks or it has not: `Document::marks: bool`, off by default,
toggled by `Action::ToggleSheetMarks` ("Registration marks", File group,
unbound, a ticked row in the File menu). Turning it on where `for_page`
answers `None` is refused with a sentence naming the reason. It is saved as
`umber-regmarks="1"` on the ORA image element and does not raise
`umber-version`: a build that ignores it shows the same picture.

A resize keeps the flag where the new size still admits marks and drops it
with a notice where it does not. A flip leaves it alone: the marks belong to
the paper, not the picture.

On the canvas the marks are an overlay, like the vector paths: the three
marks in the mark colour, and the keep-clear zones as a faint hatch. They are
never in a layer, an export or a trace.

### The print sheet: `umber-core::printsheet`

`write(out: &mut impl Write, sheet: &Sheet) -> io::Result<()>` writes a
one-page PDF:

- MediaBox is the document's physical size in points (`mm * 72 / 25.4`).
- The flattened picture as one image filling the page, 8-bit RGB, Flate
  compressed, composited onto white where the document is transparent.
- The marks drawn over it as vector fills in pure black, where the document
  has marks.
- Nothing else: no cut lines, no plot lines, no keep-clear hatch.

`Action::ExportPrintSheet` ("Export print sheet…", File group, unbound). It
reads pixels through `export_rgba`, as Export does, and writes through
`docformat::write_with`. The notice after a successful write says to print at
actual size, 100%, with no fit to page.

### The cut job: `umber-core::cutjob`

Pure. `plan(paths, &JobSettings, page) -> Result<Job, JobRefusal>`.

`JobSettings { role: PathRole, use_marks: bool, pressure: u8, speed: u8,
passes: u8 }`. Pressure 1 to 33, speed 1 to 10, passes 1 to 5. Defaults:
Cut 10/5/1, Plot 5/8/1. Role is Cut or Plot.

- Paths of the chosen role on visible vector layers are flattened to
  polylines within `FLATTEN_MM = 0.05`.
- Closed paths are ordered smallest area first, so a hole is cut before the
  outline that would free the piece it is in. Open paths follow, in order.
- With marks, coordinates are relative to the top left mark's corner:
  `(x_mm - INSET_MM, y_mm - INSET_MM)`. Without, relative to the page's top
  left.
- A point is sent as `y,x` in machine units, rounded to nearest.
- Refusals, each a sentence: no paths of that role; a path leaves the cut
  area (with marks) or the page (without); the page is wider than the
  machine; marks asked for on a document that has none; more than
  `MAX_POINTS = 200_000` points.

`Job::blocks() -> Vec<Vec<u8>>` is the byte stream, in blocks the sender
writes one at a time, waiting for ready between them. Commands end in ETX
(`0x03`). The sequence follows inkscape-silhouette's `Graphtec.py` for the
Cameo 4 family, which the Portrait 3 belongs to:

1. Setup: `TG0`, `FN0`, `TB50,0`, `\0,0`, `Z<length>,<width>`.
2. Tool: `FX<pressure>,1`, `!<speed>,1`, then for Cut `FC18,1,1` (the blade's
   0.9 mm offset) and for Plot `FC0,1,1`; `FE0,1`.
3. With marks: `TB99`, `TB52,2`, `TB51,400`, `TB53,10`, `TB55,1`,
   `TB123,<span_y>,<span_x>,0,0`. This block ends there; its reply decides
   whether the job continues.
4. Paths: `M<y>,<x>` then `D<y>,<x>` per further point, repeated `passes`
   times per path, in blocks of at most 1024 bytes split at a command
   boundary.
5. End: `M0,0`, then `SO0` is **not** sent: it moves the machine's origin for
   the next job, which is what a roll feed wants and a print-and-cut sheet
   does not.

The exact strings are pinned as literals in tests. The builder checks each
against `Graphtec.py` at a named commit and records any difference in the
design's "as built" note rather than guessing.

### The machine: `umber-app::cutter`

`trait Link { fn write(&mut self, bytes: &[u8]) -> Result<(), LinkError>;
fn read_reply(&mut self, timeout: Duration) -> Result<Vec<u8>, LinkError>; }`
A reply is the bytes up to and including ETX, spaces preserved.

`send(link: &mut dyn Link, job: &Job, progress) -> Result<(), SendError>` is
the whole conversation and is tested against a scripted fake:

- `ESC EOT` to initialise, then `FG` and read the firmware string.
- Status is `ESC ENQ`; the reply `0` ETX is ready, `1` moving, `2` no media.
  Anything else is **unknown** and is reported raw, never read as ready.
- No media before the first block is a sentence: load the sheet and try
  again.
- After the marks block, one read of up to 40 seconds. Exactly
  `"    0"` ETX is found. Anything else ends the job before a single cut,
  with a sentence saying the marks were not found.
- Between path blocks, poll status until ready, up to 120 seconds.
- A failed or uncertain write ends the job. Nothing is ever resent.

`UsbPrint` is the Windows `Link`: enumerate the USBPRINT device interface
class `{28d78fad-5a12-11d1-ae5b-0000f803a8c2}`, take the path naming
`vid_0b4d&pid_113a`, open it with `CreateFileW` for read and write,
exclusive. A sharing violation is a sentence: close Silhouette Studio and
try again. Elsewhere the command is drawn disabled with a sentence.

The send runs on a worker thread that wakes the loop, as the updater's does,
and owns the link for the job's life.

`Action::SendToCutter` ("Send to cutter…", File group, unbound) opens a
dialog: role, registration marks on or off, pressure, speed, passes, a line
saying how many paths and how long the cut is in millimetres, and Send. While
a job runs it shows which stage it is in and no bar, because the machine
reports no progress within a block.

### The probe comes before the dialog

Nobody has run this route on a Portrait 3. `cargo run -p umber-app --example
probe-cutter` opens the machine, sends initialise, asks for the firmware
string and the status, prints every byte both ways, and sends nothing that
moves the machine. The artist runs it with the machine plugged in and Studio
closed. Its output decides whether `UsbPrint` stands or the design changes.

So the work is two plans:

- **Plan A, no machine needed:** marks, the flag and its overlay, the print
  sheet, the cut job encoder, the `Link` conversation against a fake, and the
  probe.
- **Plan B, after the probe's output:** the dialog, the worker, and the first
  real cuts, starting with a pen on plain paper.

### What the first tests on the machine have to settle

- Whether USBPRINT opens and answers on a Portrait 3 at all.
- Which way round a sheet goes in, and whether `y,x` lands as expected.
- Whether the printed marks are found at 10 mm inset with a search origin of
  zero, and with a 0.5 mm stroke.
- Whether the machine corrects for a print that came out slightly scaled.
- The pressure and speed that suit sticker paper.

### Sources and licence

Protocol facts are from inkscape-silhouette (fablabnbg), GPL-2.0-or-later,
which Umber's GPL-3.0-or-later may build on. Nothing is copied; the commands
are reimplemented from its `Graphtec.py`, `Commands.md` and
`docs/cameo4-usbprint-observations.md`.

### Left to the builder

The PDF's internal object layout; the overlay's hatch; where the dialog's
session settings live; the internal shape of the flattening.
