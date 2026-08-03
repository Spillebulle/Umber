# Pen platforms: pressure beyond Windows, and tilt everywhere

Umber reads pen pressure on Windows and nowhere else, and reads tilt nowhere at
all. `CLAUDE.md` says so in two places, and both are still true of the winit
this repository pins.

This document settles *why*, at the level of winit's own source rather than
from memory, and says what each of the four missing cells would cost. It then
designs the part that is Umber's — where tilt enters the brush engine, what the
absent-versus-zero rule has to be, and what Settings → Input & pen must show —
so that the day the data arrives the design is not being invented in a hurry.

**The headline is that this changed under us.** The reasoning recorded in
`CLAUDE.md` was written against winit 0.30, where the answer was "no platform
but Windows, and Windows only for pressure". That is still exactly what
`winit = "0.30.13"` does. But winit's development line has since grown a
complete tablet API, and two of the four cells are *implemented upstream today*.
The question is no longer mostly "how big is the patch" — it is "when can Umber
take the upgrade", and the thing standing in the way is egui, not winit.

---

## 0. The verdict

Against **winit 0.30.13**, which is what `Cargo.toml` pins and what ships:

| Cell | Reachable without patching winit? | Cost |
|---|---|---|
| **Pressure, macOS** | **No** — but reachable *around* winit, from `umber-app`, via an AppKit event monitor | No upstream patch exists to take; a local monitor is ~150 lines in `umber-app` |
| **Pressure, Linux/Wayland** | **No**, and structurally not — winit owns the Wayland connection | Upstream only. **Already done upstream** |
| **Pressure, Linux/X11** | **Values arrive but cannot be attributed.** Effectively no | Upstream. **Not done upstream either** |
| **Tilt, Windows** | **No** from the event — but reachable from `umber-app` by re-calling `GetPointerPenInfo` with the id winit already hands over | **Already done upstream.** Locally, ~40 lines |
| **Tilt, Wayland** | No | **Already done upstream** |
| **Tilt, X11** | No | Upstream, not done |
| **Tilt, macOS** | Same monitor as pressure — one route serves both | As above |

And the same table against **winit 0.31.0-beta.2**, released to crates.io on
2025-11-16 and carrying a full `TabletToolData` API:

| | pressure | tilt | twist / rotation | barrel buttons |
|---|---|---|---|---|
| **Windows** | ✔ | ✔ | ✔ | ✔ |
| **Wayland** | ✔ | ✔ | ✔ | ✔ |
| **X11** | ✘ (pen *detected*, no axes) | ✘ | ✘ | ✘ |
| **macOS** | ✘ (nothing at all) | ✘ | ✘ | ✘ |

So the four cells, answered directly:

1. **Pressure on macOS** — not reachable without patching winit, and no upstream
   patch is pending; the AppKit backend has *zero* tablet code on 0.30.13 **and
   on master**. macOS is the one platform winit's tablet work has not touched.
   It is, however, the one platform where going around winit is clean, because
   AppKit will hand any process its own copy of the event stream.
2. **Pressure on Linux** — two different answers. **Wayland is done upstream**
   and is not reachable any other way, because a second Wayland client cannot
   see events for a surface it does not own. **X11 is not done upstream**, and
   the values that do arrive on 0.30.13 cannot be attributed to a device.
3. **Tilt everywhere** — follows pressure exactly. Wherever a backend delivers
   pressure it delivers tilt in the same struct; the two are never separate
   work.
4. **Tilt on Windows** — the cheapest cell by a wide margin, exactly as
   suspected. winit 0.30.13 fetches the struct that contains it and reads one
   field out of it. Upstream already reads all four.

**Recommendation, in one line:** do not patch winit and do not fork it. Do
Windows tilt now as a small, contained, well-guarded local call; put the rest
behind the winit 0.31 upgrade, which Umber wants for other reasons and which is
blocked on egui rather than on anything here. Section 8 stages it.

---

## 1. What winit 0.30.13 actually does

All four readings are from the pinned source in
`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/winit-0.30.13/`.

### Windows — pressure only, and tilt is discarded one line later

`src/platform_impl/windows/event_loop.rs`, in the `WM_POINTER` handler, around
line 2089:

```rust
PT_PEN => {
    let mut pen_info = mem::MaybeUninit::uninit();
    util::GET_POINTER_PEN_INFO.and_then(|GetPointerPenInfo| {
        match unsafe {
            GetPointerPenInfo(pointer_info.pointerId, pen_info.as_mut_ptr())
        } {
            0 => None,
            _ => normalize_pointer_pressure(unsafe {
                pen_info.assume_init().pressure
            }),
        }
    })
},
```

The whole `POINTER_PEN_INFO` is fetched. `tiltX`, `tiltY` and `rotation` are
sitting in it, already filled in by Windows, and the expression takes
`.pressure` and drops the struct. There is nowhere for them to go: the event
that gets sent is a `WindowEvent::Touch(Touch { .. })`, and `Touch` carries only
`force: Option<Force>`.

Two further details of this block matter later:

- `normalize_pointer_pressure` is `1..=1024 => Some(Force::Normalized(p/1024.0))`
  and `None` otherwise — the exact behaviour `PressureModel::resolve`'s latch
  exists to work around, and it is confirmed at line 994-996.
- **`Touch::id` is `pointer_info.pointerId as u64`.** The identifier needed to
  ask Windows for the pen info again is passed through to the application
  verbatim. That is what makes §4 possible.

`penMask` is never consulted. Upstream's newer code does consult it, and that is
a genuine correctness gain and not just plumbing — see §4.

### macOS — no tablet code whatsoever

`src/platform_impl/macos/view.rs` contains exactly one pressure-related method,
at line 758:

```rust
#[method(pressureChangeWithEvent:)]
fn pressure_change_with_event(&self, event: &NSEvent) {
    trace_scope!("pressureChangeWithEvent:");
    self.queue_event(WindowEvent::TouchpadPressure {
        device_id: ...,
        pressure: unsafe { event.pressure() },
        ...
    });
}
```

**This is the Force Touch trackpad and must not be mistaken for a stylus.**
winit's own documentation for `TouchpadPressure` says "Only supported on Apple
forcetouch-capable macbooks". A grep of the entire macOS backend for `tablet`,
`tilt` or `rotation` returns only `RotationGesture` — the two-finger trackpad
rotate.

There is no `tabletPoint:`, no `tabletProximity:`, no reading of
`NSEventSubtypeTabletPoint`, and macOS emits **no `WindowEvent::Touch` at all**.
A tablet on macOS therefore reaches Umber as ordinary `CursorMoved` and
`MouseInput` with no force field anywhere, which is why `PressureSource::Device`
resolves it to a flat 1.0 and why the pane's `Route` column would read "Mouse".

### Wayland — the protocol is not bound at all

A grep of `src/platform_impl/linux/wayland/` for `tablet`, `pressure` or `tilt`
returns **nothing**. `zwp_tablet_manager_v2` is never bound. A pen on Wayland
arrives as whatever the compositor synthesises onto `wl_pointer` — that is,
plain motion and clicks with no axes.

### X11 — the values arrive, unlabelled, and cannot be attributed

This is the interesting one, and it is the only cell where 0.30.13 is not simply
silent. `src/platform_impl/linux/x11/event_processor.rs`, in
`xinput2_mouse_motion`, walks every valuator in the event and emits:

```rust
let event = if let Some(...) = physical_device.scroll_axes.iter_mut()... {
    WindowEvent::MouseWheel { ... }
} else {
    WindowEvent::AxisMotion { device_id, axis: i as u32, value: unsafe { *value } }
};
```

So `Abs Pressure`, `Abs Tilt X` and `Abs Tilt Y` **do** reach the application, as
`WindowEvent::AxisMotion` with a bare axis index and a raw device-unit value.
`DeviceEvent::Motion` carries the same for raw events (line ~1465).

That sounds like a free win, and it is not, for three compounding reasons — and
they are exactly the reasons upstream gives for rejecting this shape of API:

1. **No labels and no ranges.** The axis index is a valuator number. Learning
   that valuator 2 is `Abs Pressure`, and that its range is 0..2047 on this
   device and 0..8191 on that one, needs `XIQueryDevice`, which winit does not
   expose.
2. **No device attribution.** `AxisMotion` is labelled with
   `mkdid(event.deviceid)` — the **master** pointer — while the valuator
   numbering belongs to `event.sourceid`, the physical slave device, which winit
   uses internally for its own lookup and never passes on. Every device attached
   to the same master pointer shares one `device_id` in the event stream. A
   mouse and a pen on one master are indistinguishable, and their valuator
   numbering collides.
3. **No way to recover the raw id.** There is a `DeviceIdExtWindows` in
   `src/platform/windows.rs`; there is **no** `DeviceIdExtX11`. winit's
   `DeviceId` cannot be turned back into an XInput2 device id from outside the
   crate at all.

A single-tablet machine could be made to work by opening a second X11
connection, enumerating devices, finding the unique one with an `Abs Pressure`
valuator and hoping its numbering does not collide with the mouse's. That is a
guess dressed as a feature, it fails silently on a two-tablet machine, and its
failure mode is a pressure axis driven by a scroll wheel. It is not worth
building, and §8 does not stage it.

---

## 2. What winit 0.31 changes, and why it is the answer

winit master has been split into per-backend crates (`winit-core`, `winit-win32`,
`winit-x11`, `winit-wayland`, `winit-appkit`, `winit-uikit`, …), and `0.31.0-beta.2`
of each is on crates.io as of 2025-11-16. `WindowEvent::Touch` is **gone**,
replaced by a pointer model: `PointerKind`, `PointerSource` and `ButtonSource`,
each of which has a `TabletTool` arm.

`winit-core-0.31.0-beta.2/src/event.rs` defines:

```rust
pub struct TabletToolData {
    pub force: Option<Force>,
    pub tangential_force: Option<f32>,   // barrel pressure, -1..1
    pub twist: Option<u16>,              // barrel rotation, 0..359 degrees
    pub tilt: Option<TabletToolTilt>,    // plane angle, degrees
    pub angle: Option<TabletToolAngle>,  // altitude/azimuth, radians
}
```

with `tilt()` and `angle()` converting between the two representations, so a
backend may report either and the application reads whichever it wants.

Three things about this are worth naming, because they are precisely the
problems Umber has had to solve by hand:

- **Every field is an `Option`, per axis, and the backends set them from the
  device's own capability bits.** That is the `None`-versus-zero ambiguity fixed
  *at the source* rather than latched around downstream. See §6 — it does not
  retire `PressureModel`'s latch, but it does mean tilt never needs one.
- **A tablet tool is not a touch.** `PointerKind::TabletTool` is distinct from
  `PointerKind::Touch`, so the "is this a pen or a second finger" reasoning that
  `gesture::contact` carries stops being an inference. This is the API design
  the stalled PRs were rejected for not having.
- **`Force::normalized` now takes the angle**, so the iOS-style perpendicular-
  force correction is a method rather than something each caller reimplements.

### Which backends actually implement it

Measured by grepping the released beta crates, not master:

| crate (0.31.0-beta.2) | `TabletToolData` mentions | verdict |
|---|---|---|
| `winit-win32` | 3 | implemented |
| `winit-wayland` | 8 | implemented |
| `winit-x11` | 0 | **not** implemented |
| `winit-appkit` | 0 | **not** implemented |

**Windows**, `winit-win32-0.31.0-beta.2/src/event_loop.rs`:

```rust
fn tablet_tool_info_for_pen(pointer_id: u32) -> (u32, TabletToolData) {
    ...
    if pen_info.penMask & PEN_MASK_PRESSURE != 0 {
        tool_data.force = normalize_pointer_pressure(pen_info.pressure);
    }
    if pen_info.penMask & (PEN_MASK_TILT_X | PEN_MASK_TILT_Y) != 0 {
        tool_data.tilt =
            Some(TabletToolTilt { x: pen_info.tiltX as i8, y: pen_info.tiltY as i8 });
    }
    if pen_info.penMask & PEN_MASK_ROTATION != 0 {
        tool_data.twist = Some(pen_info.rotation as u16);
    }
    tool_button = pen_info.penFlags;
}
```

Note the `penMask` gating, which 0.30.13 does not do. This is what makes "the
device has no tilt sensor" distinguishable from "the pen is upright", and it is
the single most important line in this whole document — see §6. Barrel and
eraser buttons come through `pen_flags_to_button`.

**Wayland**, `winit-wayland-0.31.0-beta.2/src/types/wp_tablet_input_v2.rs`, 387
lines binding `zwp_tablet_manager_v2` / `zwp_tablet_seat_v2` /
`zwp_tablet_tool_v2`, including pads, rings, strips and dials:

```rust
ToolEvent::Tilt { tilt_x, tilt_y } => {
    data.tool_state.tilt = Some(TabletToolTilt { x: tilt_x as i8, y: tilt_y as i8 });
}
ToolEvent::Pressure { pressure } => {
    data.tool_state.force = Some(Force::Normalized(pressure as f64 / u16::MAX as f64));
}
```

**X11** uses the tilt atoms, but only to *classify* the device.
`winit-x11-0.31.0-beta.2/src/event_loop.rs` around line 1050 checks whether a
valuator's label is one of `ABS_X`, `ABS_Y`, `ABS_PRESSURE`, `ABS_TILT_X`,
`ABS_TILT_Y` and, if so, records `DeviceType::Pen` (or `Eraser`, if the device
name contains "eraser"). The axis *values* are still not read into a
`TabletToolData`. X11 knows a pen is a pen and still will not say how hard it is
pressed.

**macOS** is untouched: `winit-appkit-0.31.0-beta.2` has zero mentions of
`tablet`, `tilt` or `TabletToolData`, and `view.rs` still has only
`pressureChangeWithEvent:` → `TouchpadPressure`.

### The upstream history, briefly

Pen support has been open as [issue #99](https://github.com/rust-windowing/winit/issues/99)
since 2016, labelled "needs discussion" and "hard". Three PRs tried and stalled:
[#1879](https://github.com/rust-windowing/winit/pull/1879) (X11),
[#2396](https://github.com/rust-windowing/winit/pull/2396) (Windows/Android) and
[#2647](https://github.com/rust-windowing/winit/pull/2647) (X11). The author of
#2647 said in September 2023: *"I was not really happy with gluing everything
onto the touch event"* — and the maintainers' recorded objections were that pen
data should not ride on touch events, that hover breaks the touch-phase
contract, and that `AxisMotion` cannot say which tablet it came from. The
[Pointer API sketch, #3001](https://github.com/rust-windowing/winit/pull/3001),
is what eventually landed as the 0.31 model, and the tablet work went in on top
of it. **Every objection that killed the old PRs is one this document
independently rediscovered by reading 0.30.13.** That is a good sign that the
0.31 API is the right thing to wait for rather than route around.

---

## 3. The blocker is egui, not winit

`egui-winit` 0.35.0 declares:

```toml
[dependencies.winit]
version = "0.30.13"
```

which is `^0.30.13` — satisfied by no 0.31. Umber depends on `egui-winit 0.35`,
`egui-wgpu 0.35` and `winit 0.30.13`, so **winit cannot be upgraded until egui
is**, unless Umber drops `egui-winit` and writes its own winit↔egui bridge.

egui's own progress is [PR #7731, "Winit 0.31 draft"](https://github.com/emilk/egui/pull/7731),
opened 2025-11-19 and still an open draft at 2026-07-20. The author's summary:

> I was curious how much work the winit update would be, so I tried getting
> hello world simple to run. It doesn't run yet but I got it to compile. Looks
> like this will be a lot of effort to get perfect. … I commented a ton of code
> to try to get something to compile, so this will need a *lot* more work.

So this is not imminent, and it is not something Umber can schedule. It is also
not something Umber should try to accelerate by forking: see §8's rejection of
the fork.

Note that `winit 0.30.13` was published 2026-03-02, *after* the 0.31 betas —
0.30 is a maintained line, not an abandoned one. Sitting on it is not
accumulating risk.

---

## 4. Windows tilt today, without patching anything

This is the one cell that is cheap right now, and the reason is that winit hands
over the only thing needed to ask Windows again: `Touch::id` **is**
`pointer_info.pointerId`.

So from `umber-app`'s `WindowEvent::Touch` arm, on Windows only:

```rust
// Sketch. `GetPointerPenInfo` resolved once via GetProcAddress, as winit does —
// it is not present on every Windows Umber supports, so it must not be linked.
let mut info = MaybeUninit::<POINTER_PEN_INFO>::uninit();
if GetPointerPenInfo(touch.id as u32, info.as_mut_ptr()) != 0 {
    let info = info.assume_init();
    let tilt = (info.penMask & (PEN_MASK_TILT_X | PEN_MASK_TILT_Y) != 0)
        .then(|| (info.tiltX as f32, info.tiltY as f32));   // degrees, -90..=90
    let twist = (info.penMask & PEN_MASK_ROTATION != 0)
        .then(|| info.rotation as f32);                     // degrees, 0..=359
}
```

**The `penMask` gating is not optional.** `POINTER_PEN_INFO` is a plain struct;
if the device has no tilt sensor the fields are simply zero, and zero is a
perfectly ordinary tilt reading meaning "pen held upright". Reading them without
the mask is precisely the bug §6 is about, and it would be a worse one than the
pressure blob, because it would be silent: every mouse-mode tablet would report
a permanently vertical pen and every tilt-driven brush would sit at one end of
its curve for ever.

### What is uncertain about this, honestly

**Whether the pointer info is still available when Umber's handler runs.** winit
calls `SkipPointerFrameMessages(pointer_id)` at the end of the `WM_POINTER`
block, and Windows retires a pointer id once the contact ends and its frame is
released. Umber's `window_event` runs after winit has queued the event, not
inside the `WndProc`. For `Started`/`Moved` the pointer is live and the call
will succeed; for `Ended` it may already have been retired, in which case
`GetPointerPenInfo` returns 0 and the sketch above yields `None` — which is the
correct answer anyway, since a pen that has left the glass has no tilt. This is
a "measure it on the hardware" question, not a design question, and it is
exactly the sort of thing Settings → Input & pen exists to answer.

**A second risk with a name:** this is the only place in Umber that would call a
Win32 pointer API directly, and it is a second reader of state winit already
owns. That is the drift this codebase refuses everywhere else. The mitigation is
that it is *deleted* at the 0.31 upgrade rather than kept — see §8, which makes
that explicit rather than hoping somebody remembers.

---

## 5. macOS, and why going around winit is defensible there

macOS is the only cell with no upstream path at all — not implemented, not in
progress, not on master. If macOS pressure is wanted before somebody writes the
AppKit backend, it has to come from Umber.

AppKit makes this unusually clean, because **tablet data rides on ordinary mouse
events**. From `NSEvent.h` (10.13 SDK):

```objc
/* -pressure is valid for all mouse down/up/drag events, and is also valid for
   NSEventTypeTabletPoint events on 10.4 or later and NSEventTypePressure on
   10.10.3 or later */
@property (readonly) float pressure;

/* these messages are valid for mouse events with subtype NSEventSubtypeTabletPoint,
   and for NSEventTypeTabletPoint events */
/* range is -1 to 1 for both axes */
@property (readonly) NSPoint tilt;
/* tangential pressure on the device; range is -1 to 1 */
@property (readonly) float tangentialPressure;
@property (readonly) float rotation;   // In degrees.
```

**The trap named in the brief is real and the header states it exactly.**
`pressure` is valid for *all* mouse events — on a MacBook that is Force Touch,
and on a mouse it is the click. The only thing that makes it a stylus reading is
`event.subtype == NSEventSubtypeTabletPoint`. A macOS pressure path that reads
`pressure` without checking the subtype would make every trackpad click a
pressure ramp. `NSEventSubtypeTabletProximity` carries
`vendorPointingDeviceType` (pen / cursor / eraser) and `isEnteringProximity`,
which is how the eraser end is told from the nib.

### The route: a local event monitor, not a subclass

`NSEvent.addLocalMonitorForEventsMatchingMask:handler:` lets a process observe
its own event stream and return each event unchanged. It requires no access to
winit's `NSView`, no subclassing, no swizzling, and no cooperation from winit at
all. `objc2-app-kit` 0.6.4 is **already in Umber's dependency tree** (pulled in
by winit itself), so this adds no new linked dependency — though Umber would
have to declare it explicitly for `cfg(target_os = "macos")`.

The monitor writes the latest `(pressure, tilt, subtype, proximity)` into a
small shared cell; the `CursorMoved` arm reads it. Correlation is by arrival
order, which is sound because AppKit delivers the monitor callback for the same
event winit is about to process — the monitor runs first, on the same thread, in
the same iteration of the run loop.

### Does this break the layering rule?

`CLAUDE.md`'s rule is that `gesture.rs` is *a model with no winit in it*, and
that `umber-core` must stay free of platform types. **A macOS event monitor does
not touch either.** It sits in `umber-app`, which is already the crate that
translates platform input, beside `keylayout.rs` — which is itself a platform
query behind a pure function — and `taskbar.rs`, which already calls Win32 and
reads Wayland app ids. The rule this must respect is the one `keylayout` states:
the platform is asked from the *input* path, never while painting, and the pure
part is testable without it.

So: a `cfg`-gated module in `umber-app` supplying an `Option<PenAxes>`, with the
reading injected exactly as `keylayout::name_for` takes an injected reading, and
`gesture.rs` never learning it exists. That is the same division, not an
exception to it.

**But it should still not be built first**, for a reason that is not about
layering: it is unverifiable here (§9), it is the only cell with no upstream
implementation to check against, and macOS is one universal binary that already
only gets its native slice tested. A macOS-only input path written blind by
somebody with no Mac and no tablet is the worst risk-to-reward ratio of the
four cells.

---

## 6. The absent-versus-zero rule, generalised to tilt

`PressureModel::resolve` settles pressure's ambiguity with a per-stroke latch,
because winit's Windows normaliser makes a genuine zero indistinguishable from
"no sensor". The brief asks whether tilt has the same problem. **It has the same
shape and it is worse, and it must not be solved the same way.**

Worse, because a tilt of zero is not an edge case the way a pressure of zero is.
Pressure zero means the pen is off the glass — a state that only occurs at the
very ends of a stroke. **Tilt zero means the pen is held upright, which is how
most people hold a pen most of the time.** So "absent" and "zero" are not merely
confusable; the ambiguous value is the *modal* value. A latch of the pressure
kind — "once we have seen a real reading, treat later gaps as zero" — would be
actively wrong here, because there is no reason a tilt reading should ever go
absent mid-stroke on a device that has the sensor, and if it did, the last known
tilt is a far better answer than "upright".

The rule, therefore:

- **Tilt is `Option`, end to end, and is never defaulted to zero.** Not in the
  event, not in `InputPoint`, not in `DabInputs`. A device with no tilt sensor
  reports `None` for the whole session and every stroke on it.
- **The capability comes from the platform, not from the values.** This is what
  `penMask` is for on Windows, what per-axis `Option` is for in
  `TabletToolData`, and what `NSEventSubtypeTabletPoint` is for on macOS. All
  three platforms *can* say "this device has no tilt", and Umber must ask rather
  than infer. Inferring from the values is the bug: a pen genuinely held upright
  for two seconds would be reclassified as a device without a sensor.
- **A modulation reading tilt on a device that has none evaluates at the input's
  neutral**, which is what `mypaint.rs` already does for every input Umber
  cannot produce, and is what MyPaint itself renders on the same machine. It is
  *not* skipped and the brush is *not* refused. §7 shows why this composes
  exactly.
- **There is no latch and no smoothing.** If the platform says the device has
  tilt, every sample has tilt; if it does not, none do. There is no third state
  to carry across a stroke, so there is nothing for a latch to be per-stroke
  *about* — which is the whole reason `PressureModel`'s is scoped the way it is.

`PressureModel`'s latch stays exactly as it is on 0.30.13, because
`normalize_pointer_pressure` is still what it always was. It becomes
*unnecessary* on 0.31 for tablet tools — `penMask & PEN_MASK_PRESSURE` answers
the question directly — but it must not be removed then either, because it is
still the right answer for `PointerSource::Touch`, where a touchscreen with no
force sensor must still draw. That is the same asymmetry the current comment
records, and the upgrade does not change it.

---

## 7. Where tilt enters the brush engine, and what it turns on

### The input path is already prepared

`umber-core::input::InputPoint` already carries it:

```rust
/// Stylus tilt as a unit-ish vector, `(0, 0)` when unknown. Not consumed by
/// the brush engine yet; carried so the input path doesn't need reworking
/// when tilt-driven brushes land.
pub tilt: Vec2,
```

Two changes are needed and both are small. It must become `Option<Vec2>` for §6's
reason — `(0, 0)` is exactly the sentinel that rule forbids, and it currently
means "upright" and "unknown" at once. And "unit-ish vector" has to be pinned
down to a stated convention, because the four platforms disagree:

| Platform | pressure | tilt | rotation |
|---|---|---|---|
| Windows `WM_POINTER` | `0..=1024` int | `tiltX`/`tiltY`, **degrees**, `-90..=90`, signed | `0..=359` degrees |
| Wayland `tablet_v2` | `0..=65535` int | **degrees** from the tablet's z-axis | degrees |
| X11 XI2 | device units, range from `XIValuatorClassInfo` | device units, usually degrees | `Abs Wheel` |
| macOS `NSEvent` | `0.0..=1.0` float | **normalised `-1..=1`**, both axes | degrees |

macOS is the odd one out and cannot be converted without knowing the device's
maximum tilt angle, which AppKit does not report. So the convention has to be
the one every platform can express, and the one MyPaint's inputs are already
written in.

### `DabInput` gains two variants, not three

MyPaint's own definitions, from `libmypaint`'s `brushsettings.json`:

| input | range | neutral | meaning |
|---|---|---|---|
| `tilt_declination` | `0..90` | **0.0** | "0 when stylus is parallel to tablet and 90.0 when it's perpendicular" |
| `tilt_ascension` | `-180..180` | 0.0 | "0 when stylus working end points to you, +90 rotated clockwise" |
| `barrel_rotation` | `-180..180` | 0.0 | twist about the pen's own axis |
| `attack_angle` | `-180..180` | 0.0 | "difference … between the angle the stylus is pointing and the angle of the stroke movement" |

So the natural additions to `DabInput` are **`TiltDeclination`** and
**`TiltAscension`**, with `domain()` and `neutral()` filled straight from that
table. `myb_name` gains the two strings, and `DabInput::ALL` goes from 6 to 8.

`BarrelRotation` is a third variant and should wait: Windows and Wayland both
carry it, macOS carries it, and **no shipped brush uses it** (§7's count found
zero live `barrel_rotation` mappings across 196 files). Adding an input nothing
reads is a control that lies in the brush editor.

`AttackAngle` should **not** be a `DabInput`. It is not a device axis — it is
`tilt_ascension` minus the stroke heading, both of which Umber would already
have. It is derived, and deriving it in `StrokeBuilder` beside the existing
`Direction` computation is one line; making it an input would put a fourth
platform-dependent thing in the table that is not platform-dependent at all.

**Nothing else in `dynamics.rs` changes.** `DabTarget` is untouched, the
fixed-capacity table is untouched, `Modulation` is untouched, and the fast path
is untouched — an empty table is still empty. The new inputs are guarded exactly
as `Random` is: a brush that reads no tilt costs nothing, and a machine with no
tilt evaluates them at neutral.

### The neutral composes exactly, which is the good news

`mypaint.rs`'s `Env::get` returns `0.0` for every input it does not model,
including tilt:

```rust
// Everything else — tilt, `custom`, `attack_angle`, `viewzoom`,
// the gridmap pair — reads zero on a desktop with a mouse, which is
// what MyPaint would read there too. The mapping is still evaluated
// *at* zero, so its contribution is kept rather than dropped.
_ => 0.0,
```

`0.0` **is** `tilt_declination`'s documented neutral, so Umber's shipped library
is already baked at exactly the value MyPaint bakes at. And the modulation form
is `base + map(x) - map(neutral)` with `map(neutral)` already folded into the
base — so adding a tilt modulation to an existing preset composes to the same
number at neutral and diverges only as the pen tilts. **No shipped preset's base
value has to be recomputed, and a re-import is not required for correctness.**
That is a real saving and it falls out of the existing design rather than being
arranged.

### How many shipped brushes would come alive: **11 of 196**

Counted directly from `assets/brushes/mypaint/brushes/**/*.myb`, treating a
mapping as live only where it has ≥2 points and a non-constant output — 26 files
*mention* a tilt input, but 15 of those carry flat or degenerate mappings that
contribute nothing:

| brush | mappings on a target Umber has |
|---|---|
| `Dieterle/8B_Pencil#1` | Size, Opacity, Scatter, Smudge — all from `tilt_declination` |
| `Dieterle/Fount-offset#1` | Size (`attack_angle`) |
| `Dieterle/Fountain_SF#1` | Size (`attack_angle`) |
| `classic/blending_knife` | Angle (`tilt_ascension`) |
| `classic/imp_blending` | Size, Ratio, Scatter |
| `classic/imp_details` | Size, Ratio, Scatter |
| `classic/impressionism` | Size, Ratio, Scatter |
| `classic/marker_fat` | Size, Ratio, Angle |
| `classic/marker_small` | Size, Ratio, Angle |
| `classic/puantilism` | Ratio, Scatter |
| `classic/puantilism2` | Ratio, Scatter, Angle |

All 11 are in the shipped library — `mypaint/dieterle/8b-pencil-1`,
`mypaint/classic/marker-fat` and the rest are all present in
`crates/umber-core/assets/builtin-brushes.ron`. Every one of the 11 has at least
one mapping on a target Umber already has, so none of them needs a new
`DabTarget` to benefit.

By input, across the live mappings: `tilt_declination` 28, `attack_angle` 8,
`tilt_ascension` 4, `barrel_rotation` 0.

Twelve further mappings land on MyPaint settings Umber has no target for —
`offset_multiplier`, `offset_angle_2`, `dabs_per_actual_radius`, `anti_aliasing`,
`smudge_length` and `custom_input_slowness` — and those stay dropped and stay
named by `dropped_features`, exactly as now.

**The table capacity is not a problem.** `ModulationTable::MAX` is 12, and the
busiest of the 11 currently holds 5 entries (`imp-details`, `impressionism`,
`puantilism2`); the largest addition is 4. Nothing overflows, and no brush would
have to be truncated.

**How to read the number 11.** It is small, and it should be reported as small.
It is not the argument for doing this — a painter with a tilt-capable pen wants
tilt on brushes *they* build, and the brush editor exposing two new inputs is
worth more than eleven presets changing behaviour. The eleven are the argument
for the *importer* being right already, and for the change being additive rather
than a re-import.

---

## 8. Observability: what Settings → Input & pen must show

`CLAUDE.md` is unambiguous that this is not optional — nobody working on Umber
has a pen, three gestures once shipped that a tablet could not reach, and the
pane is the only instrument. Anything designed here has to be visible there
*before* it is trusted anywhere else.

The pane's existing rules apply unchanged and each has a specific consequence:

- **Observation only; nothing downstream may read it.** `Sample` gains
  `tilt: Option<Vec2>` and `twist: Option<f32>`, and `Editor::input` stays above
  the `--- documents ---` line. No brush, no gesture and no stroke may read the
  log — the tilt that drives a dab comes from `InputPoint`, and the tilt in the
  pane comes from the recorded sample, and they are the same reading recorded
  once, not two calls.
- **The resolved figure is recorded, never recomputed.** Whatever resolves a
  tilt reading — the `penMask` check, the `TabletToolData` field, the AppKit
  subtype test — runs **once**, on the input path, and its answer is *stored*.
  The pane must not re-derive it. This matters more for tilt than for pressure:
  the natural mistake is to have the pane call `GetPointerPenInfo` itself for a
  number to draw, and by then the pointer frame may have been retired, so the
  pane would report "no tilt" for a device that had just supplied some.
- **An absent reading is never drawn as a zero.** This is the rule §6 exists to
  serve, and on the page it is sharper than for pressure. A tilt meter sitting at
  the centre is a *valid* reading — an upright pen. So an absent tilt must not be
  drawn as a centred needle either; it must be drawn as *no needle*, with the
  existing `value_meter`'s `Option` handling and a line that says the device
  reports none.
- **The ring is fixed-capacity**, so `Sample` grows by two `Option`s and no
  allocation appears on the pointer path.

What the pane should gain, concretely:

1. **A tilt readout that is a real instrument, replacing the sentence.** The
   current text — *"There is no tilt reading. The only place winit has for one is
   the stylus altitude inside a `Force::Calibrated` … and macOS and Linux send no
   pen events at all"* — becomes false the moment any cell lands, and it is
   currently the honest answer. The replacement is a small two-axis mark showing
   declination and ascension together, because tilt is a direction and two
   independent bars would be the readout that technically reports and never
   communicates. It draws nothing at all when tilt is `None`.
2. **A capability line, which is the genuinely new column.** "This device
   reports: pressure ✓, tilt ✓, twist ✗" — sourced from the platform's own
   capability bits, not from whether a non-zero value has been seen. This is the
   one reading that distinguishes "the tablet has no tilt sensor" from "Umber's
   tilt path is broken", and without it every bug report about tilt is
   unanswerable.
3. **The `Route` column gains a `TabletTool` arm** at the 0.31 upgrade, since
   `PointerKind` finally says so rather than Umber inferring pen-versus-finger
   from a touch id. Until then `Route::Touch`'s label "Touch / pen" stays exactly
   as honest as it is now.
4. **The test strip extends to tilt**, on its private copy of the model, for the
   reason it already exists: it is dragged while no stroke is running.

And the standing rule about controls that lie applies to the brush editor too:
**the two new `DabInput` variants must not appear in the Inputs section until a
platform actually delivers them**, or — better, and consistent with how Umber
already handles this — they appear and are *disabled*, with a tooltip naming
which platforms report tilt and whether this device does. A brush somebody
builds around an input their machine cannot produce is a brush that paints
nothing they designed.

---

## 9. Recommendation and staged plan

### Do not fork winit, and do not vendor a patch

Both were considered and both lose:

- **A fork** means carrying the 0.30→0.31 architectural split by hand — winit
  master is seven crates where 0.30 is one — while egui's own 0.31 work is still
  a draft that does not run. The merge burden is not a patch, it is a rebase
  against a moving API, indefinitely, on the crate that owns the event loop on
  five platforms.
- **A vendored patch to 0.30.13** would have to add a field to `Touch` or a new
  event, which is exactly the API design upstream rejected three times, so it
  could never be upstreamed and would be carried for ever. And it buys nothing
  the local `GetPointerPenInfo` call in §4 does not, on the one platform where
  0.30 can be made to work at all.

The maintenance cost of *waiting* is close to zero: 0.30.13 is a maintained
line, published after the 0.31 betas.

### The stages

**Stage 1 — Windows tilt, locally.** ~40 lines in `umber-app`, `cfg`-gated,
`GetProcAddress`-resolved like winit's own, `penMask`-gated, feeding
`InputPoint::tilt` as an `Option`. Plus the `Option` change to `InputPoint` and
the `DabInput::TiltDeclination` / `TiltAscension` variants, which are
platform-independent and are most of the actual work. **Do this one first**
because it is the only cell reachable today, because Windows is the platform
Umber's pen path is already known to work on, and because it makes the other
three cells a matter of filling in a source rather than designing anything.

**Stage 2 — the pane, in the same change.** The capability line and the tilt
mark, with tilt `None` everywhere but Windows. Non-negotiable per §8: shipping
stage 1 without it repeats exactly the mistake that shipped three unreachable
gestures.

**Stage 3 — the brush editor's two inputs**, disabled where the device reports
no tilt, and the eleven presets left exactly as they are (§7 shows they compose
correctly with no re-import).

**Stage 4 — the winit 0.31 upgrade, when egui lands it.** This is where Wayland
pressure *and* tilt arrive for free, Windows moves from Umber's local call to
`TabletToolData`, and **§4's local call is deleted**. Write that deletion into
the plan now — a temporary platform call that outlives its reason is the drift
this codebase refuses, and the only defence is naming its removal in advance.

**Stage 5 — macOS, if somebody with a Mac and a tablet wants it.** The AppKit
monitor of §5. Deliberately last: no upstream implementation to check against,
no way to verify it here, and the subtype trap makes a wrong version worse than
nothing.

**Not staged: X11.** Not implemented upstream even on master, and not reliably
reachable from outside for the three compounding reasons in §1. The honest
position is that X11 pen pressure needs an upstream contribution, and that
contributing it — reading the valuator labels and ranges in `Device::new`, which
already looks at exactly those atoms to classify the device, and filling a
`TabletToolData` — is a *tractable* upstream patch that somebody with a tablet
on X11 could write. It is the one cell where Umber's best move might be to fix
winit rather than work around it. Nobody here can, for the reason below.

### None of this can be verified by anybody working on this repository

This is the standing rule and it governs how every stage ships. Nobody working
on Umber has a pen. Nothing above — not the Windows tilt call, not the
`penMask` gating, not the AppKit subtype test, not whether
`GetPointerPenInfo` still answers after `SkipPointerFrameMessages` — can be
tested here. CI cannot test it either: GitHub's runners have no tablet.

What follows:

- **Every stage ships with its pane instrument in the same change**, never
  after. The pane is the only verification mechanism that exists, and it works
  by being in the hands of somebody who has the hardware.
- **The pure parts are tested without a device, and that is where the logic
  goes.** `DabInput`'s new domains and neutrals, the tilt-vector convention and
  its conversions, the "absent is not zero" handling — all of it belongs in
  `umber-core` and all of it is testable. The platform call is the thin,
  untestable shell; make it as thin as possible, for exactly the reason
  `gesture.rs` exists.
- **No claim in the README or anywhere else that tilt works on a platform
  until somebody with that hardware has reported it working.** The rule
  `CLAUDE.md` states for mobile — "do not claim mobile support works, it has
  never been built or run" — is the same rule, and pen tilt is in the same
  position. A brush editor offering a tilt input on a platform nobody has
  confirmed is a control that lies.
- **Prefer the disabled control with a tooltip** over the live one, everywhere
  this touches the interface, until a report comes back.

---

## Sources

Source read directly, at the pinned version, in
`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/winit-0.30.13/`:
`src/platform_impl/windows/event_loop.rs` (994-996, 2081-2130),
`src/platform_impl/macos/view.rs` (751-765),
`src/platform_impl/linux/x11/event_processor.rs` (1104-1175, 1434-1470),
`src/platform_impl/linux/x11/mod.rs` (1010-1060),
`src/platform/windows.rs` (647-655), `src/event.rs` (861-905).

Released beta crates, downloaded and read: `winit-core`, `winit-win32`,
`winit-x11`, `winit-wayland`, `winit-appkit`, all at `0.31.0-beta.2`.
`egui-winit-0.35.0/Cargo.toml` for the version constraint.

Brush figures computed over `assets/brushes/mypaint/brushes/**/*.myb` (196
files) and `crates/umber-core/assets/builtin-brushes.ron`.

Apple's `NSEvent.h` from the 10.13 SDK for the tablet property ranges.
libmypaint's `brushsettings.json` for the input domains and neutrals.

- [winit issue #99 — Pen/Tablet Input Support](https://github.com/rust-windowing/winit/issues/99)
- [winit PR #3001 — Pointer API Sketch](https://github.com/rust-windowing/winit/pull/3001)
- [winit PR #2647 — implement pen for X11](https://github.com/rust-windowing/winit/pull/2647)
- [winit PR #2396 — Pen and pressure support for Windows and Android](https://github.com/rust-windowing/winit/pull/2396)
- [winit PR #1879 — Pen tablet support on X11](https://github.com/rust-windowing/winit/pull/1879)
- [egui PR #7731 — Winit 0.31 draft](https://github.com/emilk/egui/pull/7731)
- [Wayland tablet-v2 protocol](https://wayland.app/protocols/tablet-v2)
- [POINTER_PEN_INFO (winuser.h)](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-pointer_pen_info)
- [NSEvent.EventType.tabletPoint](https://developer.apple.com/documentation/appkit/nsevent/eventtype/tabletpoint)
- [Wacom — macOS NSEvent basics](https://developer-docs.wacom.com/docs/icbt/macos/ns-events/ns-events-basics/)
