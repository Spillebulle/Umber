# Lifecycle audit: state machines, cancellation, and things that outlive their owner

An audit of *when* things happen and what is left behind. Not pixel
correctness, not panics as such — but the consequences of a panic on a worker
are squarely in this remit, because a dead worker is a job with no driver.

The starting point was the shape recorded in `cd44fa1`:

> **A disowned job with no driver.** `cancel_capture` marks a job and does not
> free it — freeing happens inside `take_capture` — while the interrupting path
> emptied the id that was the only route to calling it.

That shape is endemic. **Five further instances were found**, two of them
blocking. Every one has the same signature: something is put into a state that
only one collector can leave, and the path that created the state is not the
path that owns the collector.

The capture's own instance is fixed and correct as of `8ea00d6` / `cd44fa1` —
`CanvasRenderer::settle_capture` called for **every** canvas in
`autosave::collect` is the right shape, and the comment about why it is every
canvas rather than the active one is the right argument. Nothing below
retracts that.

## Findings

| # | Rank | Finding | Evidence |
|---|---|---|---|
| 1 | **BLOCKING** | A panic in the document-decoding worker freezes Umber for ever behind an uncancellable modal | confirmed by reading |
| 2 | **BLOCKING** | A panic in the autosave writer thread stops autosaving for the rest of the run, silently, with no respawn and nothing on screen | confirmed by reading |
| 3 | SUBSTANTIVE | A recovery offer that is `at_risk`-only is dropped on the floor: its markers are never forgotten, its file locks are held for the whole run, and the markers accumulate for ever | confirmed by reading |
| 4 | SUBSTANTIVE | A VRAM refusal during a **background** document's autosave bake is recorded on a canvas nobody collects from | confirmed by reading; the refusal itself is inferred (needs a card to refuse) |
| 5 | SUBSTANTIVE | `suspended()` drops every renderer while leaving the stroke, the touch map, the pinch and `Interaction` live | confirmed by reading; path has never been run |
| 6 | SUBSTANTIVE | `umber-shellext`'s `LIVE` counter is a process-global written by parallel tests with no lock — three separate races in one test | confirmed by reading |
| 7 | MINOR | The Windows installer window stops ticking on a vanished worker and holds its last step | confirmed by reading |
| 8 | MINOR | The effect-refusal latch is defeated on any frame where two bakes run | confirmed by reading |
| 9 | MINOR | `DllCanUnloadNow` reads `LIVE` `Relaxed`, so unmapping is not ordered after the last `Drop` | confirmed by reading |
| 10 | MINOR | Two worker threads are spawned unnamed, so the panic log cannot say which one died | confirmed by reading |
| 11 | MINOR | `crash::note_autosave` never prunes `Context::copies` | confirmed by reading |
| 12 | MINOR | `Fonts::forget` mid-scan detaches a several-hundred-file scan | confirmed by reading |
| 13 | MINOR | `installwin::Installer::worker`'s comment claims a dropped `JoinHandle` joins; it detaches | confirmed by reading |
| 14 | MINOR | The defensive `return` at `app.rs:3717` would drop an encoder holding a capture copy and leak egui's frees | confirmed by reading; unreachable today |

Counts: **2 blocking, 4 substantive, 8 minor.** Instances of the disowned-job
pattern: **5** (#1, #2, #3, #4, #7).

### What is *not* wrong

Checked and clean, so the next audit need not re-derive them:

- **Tab switching.** `Editor::take_document` / `install_document`
  (`editor.rs:1159`–`1195`) still move exactly `DocumentState`'s six fields, and
  everything above the `--- documents ---` line that could plausibly be
  per-document carries an argument for why it is not. `float_text` is the one
  that looks forgotten and is not — `editor.rs:549`–`561` names all three sites
  that clear `float` and leave it alone, and explains why each is safe.
  `selection_outline` / `selection_screen` / `selection_dashes` are per-frame
  scratch, rebuilt in `ui.rs:476`. `Thumbs` is keyed by document and
  `Thumbs::follow` runs from the top of `render` (`app.rs:3453`) *before* the
  interface is built, which is the correct order.
- **A thumbnail across a tab switch** is suspended, not stranded. `drive_thumb`
  / `submit_thumb` / `take_thumb` only ever touch the active canvas
  (`app.rs:3818`, `3899`), and a job left in `Mapping` on a background canvas
  resumes when the tab returns. The held resource is one 16 KB staging buffer.
  `take_thumb`'s early `abandoned && state != Mapping` drop (`canvas.rs:8140`)
  is the third solution to the same problem and is correct.
- **Shutdown.** No `process::exit` outside `examples/`. Both entry points
  (`lib.rs:183`, `lib.rs:205`) call `ended_cleanly` after `run_app` returns, and
  the `?` placement is deliberate and right.
- **egui texture ordering.** `submit_frame` (`app.rs:401`) is the single
  statement, and every early return in `render` that skips drawing calls
  `release_finished_textures` first (`app.rs:3671`) — with one unreachable
  exception, #14.
- **`shortcuts::set_capturing`.** `settings::show` calls `stop_listening`
  unconditionally on every frame the pane is not in front (`settings.rs:132`,
  `140`, `180`), so the egui temp entry can never be evicted while the flag is
  set. `ed.input.end_probe()` beside it is the same pattern handled.
- **Suspend does not strand the autosave scheduler.** `flight` survives
  `gfx = None`, but the first `collect` after `resumed` finds a fresh renderer
  with no job, falls into `else if !canvas.capture_in_flight()`
  (`autosave.rs:2031`) and abandons. That self-heal is load-bearing and worth
  keeping in mind before that branch is ever narrowed.

---

## 1. BLOCKING — a panicking decode worker freezes Umber for ever

**Files.** `crates/umber-app/src/loading.rs:149`,
`crates/umber-app/src/app.rs:4473`–`4523`,
`crates/umber-app/src/tabs.rs:676`–`700`,
`crates/umber-app/src/crash/mod.rs:346`–`355`.

**The sequence.**

1. `App::begin_open` refuses to start a second decode while
   `self.editor.loading.is_some()` (`app.rs:4473`) and spawns the worker
   (`loading.rs:93`).
2. `tabs::loading` draws an `egui::Modal` for as long as `ed.loading.is_some()`.
   Its docs say it is **"Modal, and deliberately without a Cancel"**
   (`tabs.rs:668`), and `should_close()` is never read — the return value of
   `.show(...)` is discarded — so Escape and the click outside do nothing.
3. `editor.loading` is cleared from exactly one place, `collect_loading`, and
   only when `Loading::take()` answers `Some`.
4. `Loading::take` is:

   ```rust
   pub fn take(&self) -> Option<Result<ImportedDocument, ImportError>> {
       self.outcome.try_recv().ok()
   }
   ```

   `try_recv()` answers `Err(TryRecvError::Disconnected)` when the sender was
   dropped without sending — which is exactly what a panicking worker does.
   `.ok()` maps that to `None`, **indistinguishable from "not yet"**.
5. `Cargo.toml` sets no `panic = "abort"`, so panics unwind, and
   `crash::report_panic` deliberately returns early for any thread that is not
   `main`: *"A worker died. The application is still running, so a box saying it
   has stopped would be false."*

**The result.** A panic anywhere in `umber_core::docimport::import_reporting` —
the readers for `.ora`, `.kra`, `.psd` and `.clip`, which is the most
adversarial input surface in the codebase — leaves an uncancellable modal on
screen for the rest of the process. Every other open tab is behind it. No
further wake ever arrives (`Wake` is only sent by that thread), so under
`ControlFlow::Wait` the window sits still and reads as hung. The only exit is
killing Umber, which discards every unsaved document that is not covered by an
autosave copy. The artist's one signal is a `log::error!` line they will never
see.

**The comparison that makes this a defect rather than a hazard.** The same
crate already solves it, twice, and says why:

- `update::Updates::poll` (`update/mod.rs:282`):
  `Err(TryRecvError::Disconnected) => self.worker_vanished()`, with the comment
  *"The thread ended without reporting, which can only be a panic in it. Say so
  rather than sitting on 'Checking…' for ever."*
- `textpanel::Fonts::poll` (`textpanel.rs:229`):
  `Err(TryRecvError::Disconnected) => { self.pending = None; false }`, *"The
  worker died. The built-in face stands."*

`loading` is the newest of the three and the only one behind a modal with no way
out, and it is the only one that does not distinguish the case.

**The guard.** `a_decode_whose_worker_vanished_does_not_hold_the_window`. It
needs no GPU and no real file: build a `Loading` by hand from a channel whose
sender is dropped without a send, call `take()`, and assert it answers a
`Some(Err(..))` rather than `None`. That drives the whole rule, because
`collect_loading` already routes an `Err` to a notice and clears `loading`. The
guard would be **vacuous if written against `editor.loading`** — a test that
asserts the modal is gone after a *successful* decode proves nothing. It has to
start from the disconnected channel, which is the case the code cannot currently
see.

**The fix, stated so the guard is not written against the wrong thing.**
`Loading::take` should return `Some(Err(ImportError::…))` — or a distinguished
outcome — on `Disconnected`, so `collect_loading`'s existing error arm takes the
dialog down and names it. Do **not** fix it by giving the modal a Cancel: the
docs at `tabs.rs:668` argue correctly that a Cancel that cannot interrupt the
worker is a control that lies, and that argument is untouched by this.

---

## 2. BLOCKING — a panicking autosave writer stops autosaving, silently

**Files.** `crates/umber-app/src/autosave.rs:1506`–`1515`, `1740`–`1759`,
`1812`–`1873`; contrast `crates/umber-app/src/prefs.rs:313`–`325`.

**The sequence.**

1. `Autosave::writer` spawns the thread **only when `self.tx.is_none()`**
   (`autosave.rs:1813`). A thread that panics leaves `self.tx` as a live-looking
   but disconnected `Sender`, so it is never respawned.
2. `Autosave::send` meets the failure with a log line and nothing else:

   ```rust
   Some(tx) => {
       if tx.send(job).is_err() {
           log::error!("the autosave writer has gone; nothing was written");
       }
   }
   ```
3. `Autosave::poll` reads `self.rx`, which is disconnected, so it answers an
   empty `Vec` for ever. `Report::Failed` — the one mechanism that reaches the
   artist, and the one the module's own rule *"A failure says so once and carries
   on"* is about — travels down that same dead channel.

**The result.** Every capture still runs: `next_due` nominates, `begin_capture`
succeeds, the readback bands across frames and costs its ~1 ms per frame, and
`finish` sends a `Job::Finish` into a dead channel. Nothing is written. No
`Report::Written` arrives, so `mark_autosaved` never runs and no internal copy
appears. The scheduler does **not** strand — `finish` takes `flight` — so the
loop repeats every five minutes, for ever, doing the full GPU readback and
throwing it away. Nothing on screen says so.

That is precisely the failure CLAUDE.md's autosave section exists to prevent,
arriving by a different door: *"the artist believes their work is being written
every five minutes and it is not."*

**The comparison.** `prefs::save` in the same crate handles the identical
situation correctly and says why:

```rust
if tx.send(text.clone()).is_err() {
    // The writer thread died. Better a blocking write than lost
    // settings — this is a settings interaction, not a stroke.
    write_now(&text);
}
```

An autosave cannot take that remedy — a blocking encode on the frame loop is the
whole thing `begin_capture` exists to avoid — but it has two others available:
clear `self.tx`/`self.rx` on a failed send so the next job respawns the thread,
and raise the existing `Report::Failed` path locally rather than through the
channel that has just been shown to be dead.

**The guard.** `an_autosave_whose_writer_vanished_says_so_and_starts_another`.
It needs no GPU. Build an `Autosave` with `marks_dir` pointed at scratch, force
`writer()` to run, drop the receiving end so the next `send` fails, then assert
(a) that a subsequent `send` re-spawns — `self.tx` is a *different* sender — and
(b) that `poll()` yields a `Report::Failed`. Both halves are needed and the
second is the one that catches the bug: a respawn that stays silent would leave
the artist in the same position for the five minutes until the next attempt.

**Where a guard would be vacuous.** Asserting that `send` logs proves nothing —
a log line is what the defect *is*. The observable has to be a `Report` reaching
`poll`, because that is what `autosave::collect` turns into a `Notice`.

---

## 3. SUBSTANTIVE — an `at_risk`-only offer leaks its markers and their locks

**Files.** `crates/umber-app/src/autosave.rs:824`–`826`, `953`–`991`,
`1568`–`1588`, `2001`–`2010`, `2562`–`2587`;
`crates/umber-app/src/recoverdlg.rs:143`; `crates/umber-app/src/app.rs:4768`.

**The sequence.**

1. `collect_offer` walks the markers directory. A marker whose documents produce
   **no** `found` and **no** `at_risk` is removed on the spot
   (`autosave.rs:977`–`986`) — that is the *"A marker that names nothing is
   forgotten"* rule, correctly implemented.
2. A marker producing `at_risk` but no `found` falls through to line 987 and is
   pushed into `offer.marks` **and** its lock into `held`.
3. `begin_run` stores `held` in `self.claimed` (`autosave.rs:1579`), held open
   and exclusively locked "for as long as the offer is on screen".
4. `autosave::collect` then does:

   ```rust
   let offer = editor.autosave.begin_run(SystemTime::now());
   if !offer.is_empty() {
       …
       editor.recovery.offer(offer);
   }
   ```

   and `Offer::is_empty` is `self.found.is_empty()` — deliberately, and the
   argument at `autosave.rs:811`–`823` for why `at_risk` alone must not raise a
   dialog is correct.
5. So the `Offer` — **including `offer.marks`** — is dropped on the floor.
   `forget_marks` is reachable only from `Recovery::dismiss()` via
   `app.rs:4768`, and `Recovery` never received this offer.

**The result.**

- The marker is never removed. Nothing else can remove it: `sweep_now` sweeps
  `internal_dir()` and `sessions_dir()` is a *subdirectory* of it
  (`autosave.rs:174`), and `Reaper` does not recurse and only takes names an
  autosave writes.
- `self.claimed` holds an open, exclusively locked file handle per such marker
  for the whole run.
- Every subsequent start re-scans, re-claims, re-reads and re-drops it. The
  markers directory grows without bound.

**Why this is the common case rather than a corner.** The module's own comment
says it: *"an operating system restart force-kills applications, and Umber
refuses to close while a document holds unsaved work, so an ordinary reboot
leaves a marker every time."* And `offer_from` (`autosave.rs:873`–`879`) puts a
modified document with **no copy** — an untitled document that has not yet
reached its first five-minute autosave — straight into `at_risk`. A reboot with
one such document open is enough. A document whose copy is *superseded*
(line 890) contributes to neither list, so a marker holding one superseded
document and one never-copied modified document is exactly this case.

**The guard.** `a_marker_naming_only_documents_with_no_copy_is_forgotten`. It is
a file-system test with no GPU and no window — `collect_offer` already has
sibling tests in the same module using `scratch(…)`. Write a marker naming one
modified document with no copy, run the offer collection through the same route
`autosave::collect` takes, spend the frame, and assert the marker file is gone
and `claimed` is empty. The **subtle half** is that it must go through the
`is_empty()` gate rather than calling `collect_offer` directly: a guard on
`collect_offer` alone would pass today, because that function's behaviour is
defensible in isolation — the loss happens at the call site. This is the "a
guard on a model is not a guard on the panel" rule, in the autosave.

---

## 4. SUBSTANTIVE — a background canvas's VRAM refusal has no collector

**Files.** `crates/umber-render/src/canvas.rs:8370`, `8619`–`8653`;
`crates/umber-app/src/autosave.rs:1910`–`1929`;
`crates/umber-app/src/app.rs:3764`–`3785`, `3912`–`3914`.

**The sequence.** `autosave::drive` calls `bake_effects` on the **due** canvas,
which is `next_due`'s first *modified* tab and is routinely **not** the active
one — the same asymmetry `autosave::interrupt`'s docs make a point of at
`autosave.rs:1952`–`1960`. If that bake is refused a page, the renderer records
it:

```rust
if let BakeError::Refused(refused) = what {
    self.effects.refusing = true;
    if !was_refusing {
        self.effects.refused = Some(refused);
    }
}
```

`take_effect_refusal` is called from exactly one place — `app.rs:3785`, on the
**active** canvas. A refusal recorded on a background canvas is therefore held
until that document is switched to, and then surfaces as a
`vram::effect_refused` notice attributed to whatever the artist has just done.
If the tab is never revisited it is never shown at all — which is the case that
matters, because *"it is the only thing on this path that tells the artist their
effects are not being drawn."*

**The guard.** `a_refusal_on_a_document_nobody_is_looking_at_still_reaches_the
_artist`. This is one where a **real** guard is hard: producing a genuine page
refusal needs a card under memory pressure, and `canvas.rs:3186` already records
that a CI runner has no card to put under it. What stands in for it: a
test-only hook that plants a `Vram` in `effects.refused` on a background
canvas, then drives one `autosave::collect` and asserts the notice arrives —
i.e. test the *collection* rather than the refusal. That is honest about which
half is covered. Structurally, the better answer is to collect the refusal from
every canvas in `autosave::collect`, exactly as `settle_capture` now is, which
would make the guard unnecessary and is the same fix for the same shape.

---

## 5. SUBSTANTIVE — suspend drops every renderer and leaves the gesture live

**File.** `crates/umber-app/src/app.rs:5345`–`5356`.

`suspended()` is four lines:

```rust
self.editor.float = None;
self.gfx = None;
```

It correctly reasons about the float, and correctly does not touch
`float_text`. What it leaves behind:

- **The stroke.** `editor.stroke` may be active with pending dabs and
  `interaction == Interaction::Drawing`. Its coverage lives in a scratch texture
  that has just been dropped. On resume, the new renderer's scratch is empty; the
  next `finish_stroke` reads the damage the builder accumulated, captures an undo
  patch for a rectangle nothing changed and commits an empty scratch — a history
  entry for a stroke that produced no mark.
- **`editor.touches`.** Never cleared anywhere but per-touch `remove`
  (`app.rs:5778`, `5788`). Backgrounding with a finger down leaves an entry with
  no matching "up" — and CLAUDE.md's own pen rule already records what a stale
  entry costs: *"the next press … would count as a second finger and be read as a
  pinch."*
- **`drawing_touch`, `pinch`, `brush_resize`, `selection_draft`,
  `interaction`.** All survive a suspend that has taken away the surface they
  were aiming at.

Compare `switch_document`, which finishes the transform and the stroke first, and
`install_document`, which resets `interaction` explicitly and says why.

**Ranking.** Substantive rather than blocking because the path is Android's and
CLAUDE.md is explicit that mobile *"has never been built or run"*. It is worth
recording now precisely because it will be discovered in front of somebody
otherwise, and because the fix is three lines beside two that are already there.

**The guard.** `suspending_leaves_no_gesture_behind`. No GPU and no window
needed if `suspended`'s state-clearing half is factored into an
`Editor::abandon_gesture` the way `gesture::press` was factored out of
`window_event` — the same division. Drive it from an editor with a live stroke,
two touches and a pinch, and assert all of them are gone. As it stands the
method takes `&ActiveEventLoop` and touches `self.gfx`, so it cannot be driven
at all: **that is itself the finding**, and factoring is the fix rather than a
tidy-up.

---

## 6. SUBSTANTIVE — `LIVE` is a process-global written by parallel tests with no lock

**File.** `crates/umber-shellext/src/lib.rs:90`, `632`–`650`.

```rust
let before = LIVE.load(Ordering::Relaxed);
{
    let _held: IThumbnailProvider = ThumbnailProvider::new().into();
    assert!(LIVE.load(Ordering::Relaxed) > before);
    …
}
assert_eq!(LIVE.load(Ordering::Relaxed), before);
```

The comment claims *"Serialised against the other tests' objects by taking the
reading relative to itself rather than against zero"*. Reading relative to
itself does not serialise anything. Three races, all live:

1. `assert!(… > before)` fails if another test's provider is dropped between the
   two loads.
2. `assert_eq!(…, before)` at the end fails if another test has created a
   provider that is still alive.
3. It equally fails if another test dropped one it had created before `before`
   was read.

At least two other tests in the same module create providers —
`the_dll_hands_out_the_class_it_is_registered_as` (line 617) and the
`GetThumbnail` test around line 602 — and the harness runs them on parallel
threads. This is the flake.

CLAUDE.md already states the rule and the remedy: *"A test that writes a
process-global must take a lock, and the harness will not tell you it does
not"*, and *"two mutexes serialise nothing"*. The rule was recorded for
`umber-app` (`gputest::lock`) and `umber-core`/`prefs` (`prefs_lock`) and never
applied to `umber-shellext`, which is a **third test binary**. That is the
"enforced at N sites, forgotten at the N+1th" shape the rule itself warns about.

**The guard.** There is no guard here; the fix is the lock. Add a
`pub(crate) fn live_lock() -> MutexGuard<'static, ()>` beside `LIVE` — beside
the global, not inside `mod tests`, for the exact reason `prefs_lock` is beside
`set_undo_budget` — and take it in **every** test that constructs a
`ThumbnailProvider`, not only the one that reads the counter. Then verify the
way CLAUDE.md prescribes: filter the binary down to this module and run it
twenty times.

---

## 7. MINOR — the installer window holds its last step on a vanished worker

**File.** `crates/umber-app/src/update/installwin.rs:283`–`304`.

```rust
Err(TryRecvError::Disconnected) => return None,
```

with the comment *"The worker has finished and dropped its end. Whatever it last
said is the final answer, so stop asking."* That reading is right for a worker
that finished and wrong for one that panicked: the last thing it said would be
`Step::Installing`, and the window then holds "Installing…" with no further
ticks. `Updates::poll` distinguishes the two (`worker_vanished`) and this one
does not. Rank minor because the window belongs to a short-lived helper process
and the package is either in place or not regardless.

## 8. MINOR — the refusal latch is defeated when two bakes run in one frame

**File.** `crates/umber-render/src/canvas.rs:8370`, with
`crates/umber-app/src/autosave.rs:1912` and `app.rs:3766`.

`bake_effects` opens with
`let was_refusing = std::mem::replace(&mut self.effects.refusing, false);`. On a
frame where the autosave's bake runs on the *active* canvas — the single-tab
case — the frame's own bake then reads `was_refusing == false` because the
autosave's bake consumed it, and records `refused` again for a refusal already
reported. One duplicate notice per autosave attempt, not one per frame, because
`next_due` fires rarely. Worth stating because the latch's docs claim the
transition is what is recorded, and with two bakes per frame it is not.

## 9. MINOR — `DllCanUnloadNow` provides no happens-before edge

**File.** `crates/umber-shellext/src/lib.rs:129`, `136`, `414`.

`fetch_add` / `fetch_sub` / `load` are all `Relaxed`. Answering `S_OK` unmaps the
DLL, so the load wants `Acquire` and the decrement in `Drop` wants `Release`, or
the unmap is not ordered after the last destructor's stores. Unobservable on x86
in practice; the cost of fixing it is two words, and the comment above `LIVE`
already states the stake correctly ("answering yes while an object is still live
frees the vtable somebody is about to call").

## 10. MINOR — two workers are spawned unnamed

`loading.rs:93` and `installwin.rs:174` use bare `std::thread::spawn`. Every
other worker in the crate uses `thread::Builder::new().name(…)`.
`crash::report_panic` reads `thread.name().unwrap_or("<unnamed>")`, so the one
log line a dead worker produces cannot say which worker it was — and for
`loading.rs` that is the only signal that finding #1 has happened.

## 11. MINOR — `crash::note_autosave` never prunes

**File.** `crates/umber-app/src/crash/mod.rs:301`–`315`. `note_documents`
rebuilds `ctx.docs` wholesale, so a closed document leaves it. `note_autosave`
only ever inserts or replaces in `ctx.copies`, keyed by `DocId`, which is unique
per document ever opened. `Context::documents` filters by live doc, so nothing is
*reported* wrongly; the vector simply grows by one `String` path per document
autosaved in the run. Named because every other field on that path is bounded
(`FIELD_LIMIT`, `TITLE_LIMIT`) and this one is not.

## 12. MINOR — `Fonts::forget` detaches a scan

**File.** `crates/umber-app/src/textpanel.rs:241`–`247`. Setting
`pending = None` and `started = false` drops the receiver while the scan thread
is still walking several hundred font files; the next `start` spawns a second.
Bounded by how fast somebody can change the font-folder preference, so it is
wasted work rather than a leak.

## 13. MINOR — a dropped `JoinHandle` does not join

**File.** `crates/umber-app/src/update/installwin.rs:248`–`249`. The field's
comment reads *"Held so the thread is joined rather than detached when the window
goes."* Dropping a `JoinHandle` **detaches**; only `join()` joins. The comment
describes a guarantee the code does not make. Harmless here — the helper process
exits either way — but it is the kind of claim that gets relied on.

## 14. MINOR — the defensive `return` at `app.rs:3717`

```rust
let Some(canvas) = gfx.canvases.get_mut(&self.editor.session.active_id()) else {
    return;
};
```

Unreachable today: `has_canvas` was established at line 3661 and the acquisition
was filtered on it at 3666, which the comment at 3713 says out loud. If it were
ever reached it would return **after** `autosave::drive` had recorded a
`copy_texture_to_buffer` into `encoder` — dropping the encoder unsubmitted while
the job sits in `StepState::Rendering`, so the next `submit_capture` would
`map_async` a buffer whose copy was never submitted. That is exactly the hazard
`submit_capture` is split out to prevent. It would also leak
`textures_delta.free`. Worth a line rather than a change: if the guard at 3666 is
ever loosened, this is what it was holding.

---

## What I would most want fixed

**Finding #1.** It is three lines, it needs no new mechanism, and the codebase
already contains two correct implementations of the same rule with the reasoning
written beside them. Every other finding here degrades something; this one takes
the whole application away from somebody who opened a file, and takes their
other tabs with it.

And beyond the individual fix, the generalisation is worth writing down beside
the one `cd44fa1` already recorded:

> **`Result::ok()` on a `try_recv` is a place where a dead worker becomes
> silence.** Three modules in `umber-app` poll a worker through an
> `mpsc::Receiver`. Two match on `TryRecvError::Disconnected` and say so; the
> third calls `.ok()` and cannot tell a panicked worker from a busy one. The
> shape to grep for is `try_recv().ok()`, and the question to ask at every
> channel is the same one `cancel_capture` asks: *when this ends unexpectedly,
> who notices?*
