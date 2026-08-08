# Reconciling the six designs

> **The slot ceiling moved after this was written.** `LayerStack::MAX_SLOTS` is
> **256**, not 129 — it is the device's `max_texture_array_layers` guarantee
> rather than `MAX * 2 + 1`, and 127 of it is reserved as the layer-effect
> budget. Growth is no longer plain doubling either: it doubles inside a byte
> budget and rounds to a quantum past it. Every figure below that names 129, or
> describes growth as doubling, needs re-deriving before it is acted on; the
> arguments do not change, the arithmetic does. See `docs/layer-effects.md`
> §6.3 and `grown_capacity` in `canvas.rs`.
>
> **This document's plan to raise `MAX_SLOTS` from 129 to 136 has to be
> re-derived in particular**: there is no longer open room above the ceiling,
> only the 127 slices layer effects have claimed, so eight more slots must now
> be taken *from* that budget rather than added on top of it.

`structural-undo.md`, `linked-transform.md`, `group-compositing.md`,
`text-tool.md`, `mobile.md` and `pen-platforms.md` were written in parallel by
six agents who could not read each other. Each is good on its own subject. This
is the document to read before picking any of them up: where they collide, which
one is right when they do, what order they have to be built in, and what is
already true that some of them still describe as outstanding.

It does not summarise them. Read them.

---

## 1. The collisions nobody flagged

These are the dangerous ones — the two the authors did flag are in §2, already
known.

### 1.1 Three documents rewrite `Editor::layer_draws`, and one changes its type

- `group-compositing` stage 1 makes `LayerDraw::slot` an `Option<u32>`, adds the
  `open` count and the close marker, and changes `active_draw_index` from
  "count non-folders" to "count entries that produce a draw".
- `linked-transform` stage 5 turns `float_preview`'s single `(from, to)` pair
  into a slice and makes the swap a **lookup** instead of a comparison.
- `text-tool` does not change it and depends on it working.

Neither of the first two mentions the other. They meet in one expression —
`editor.rs:1319`, `(slot, Some((from, to))) if from == slot => to` — and where
they meet there is a trap that `group-compositing` §5 names in its own terms and
that gets **worse** in `linked-transform`'s shape: with `slot` an `Option`, a
folder's `None` must not match. A comparison against one pair is easy to guard;
a lookup keyed on `Option<u32>` matches every folder in the stack at once, and
the symptom is one layer's preview composited in another layer's place — which
`linked-transform` §10 already calls invisible to every CPU test.

**Order: `group-compositing` stage 1 first, unconditionally.** It changes the
type; everything else changes the contents. Stage 1 ships nothing visible and is
where that risk is bought down, which is exactly what its author designed it for.

### 1.2 The container the two undo documents were told to agree on is needed by one of them

The brief, and the commit message, frame `EditBody` as a shared primitive. It is
not. `structural-undo` §6 says so plainly and is right: a structural entry stores
no pixels at all, and the folder-delete case is answered by `StackShape`, not by
a plurality of patches. **`Pixels(Vec<PixelPatch>)` is required by
`linked-transform` alone.**

That makes the settlement cheap rather than a compromise — see §2.1 — and it
means the sequencing constraint between the two is only that they touch the same
`enum`, not that either is blocked on the other's design.

### 1.3 `MAX_SLOTS`: one document raises it, the other spends it, and neither has the arithmetic

- `linked-transform` §4 raises `MAX_SLOTS` from 129 to 136 to reserve eight
  preview slices.
- `structural-undo` §7 treats 129 as a hard ceiling that history entries now
  compete for, and answers exhaustion by evicting entries until one releases a
  claim.

The joining fact is in neither: **`slot_capacity_needed()` is `next_slot`**
(`layer.rs:728`), one past the highest slot *ever handed out*, and it never
decreases. Two consequences.

The reassuring one first, because it is the corruption everybody will worry
about and it does not exist: `begin_float` takes `preview_slot = reserved =
next_slot` (`canvas.rs:2777`), which is above every parked slice by construction,
so a float preview can never be rendered into a deleted layer's parked pixels.
Write that down at the code, because the obvious "tidy-up" — making
`slot_capacity_needed` the *live* high-water mark — creates it.

The unflagged one: parking means a delete no longer returns its number to
`free_slots`, so `next_slot` climbs on every delete-then-add cycle, and
`begin_float`'s `reserved >= MAX_SLOTS` refusal is reachable by ordinary work
rather than by a 64-layer document. `linked-transform`'s `+ 8` does not protect
against that; it moves the wall by eight. And `structural-undo`'s "the history
gives a slot back" is written about `add` — it has to cover `add_mask` and
`begin_float` too, and it cannot live in `umber-render`, which is where the float
gate is and which cannot see a `History`. **Three gates, one release, in
`app.rs`, and nobody owns it.**

The ceiling raise still stands: 64 fully masked layers are 128 live slices and no
eviction can help there. But it should be justified as "the worst *live* case
plus a set", not as a budget for floats.

### 1.4 `text-tool` contradicts itself about the clipboard

§9 stage 1: *"IME and clipboard work because the typing happens in a
`TextEdit`."* §7, point five, in the same document: *"**Ctrl+V cannot bring text
in from another application**, in a `TextEdit` or on the canvas."*

§7 is the correct half — `egui-winit` is built `default-features = false`, so the
`clipboard` feature is not compiled in and `arboard` is not in the lockfile,
which is the same fact that leaves the crash box without a "Copy details" button.
Stage 1 buys IME and buys nothing else. That matters because pasting a paragraph
in from somewhere else is most of what a text tool in a panel is *for*, and it
moves `architecture.md`'s "system clipboard" roadmap row from a convenience to a
prerequisite for stage 1 being worth shipping.

### 1.5 The two float-cost tables disagree on the page and agree underneath

`text-tool` §4(b): `begin_float` is "32 MB allocated, 32 MB copied" at 2048².
`linked-transform` §4: a float today is 50.3 MB at 2048².

Both are right. The first counts the two fresh textures (`base`, `source`); the
second counts the preview *slice* as well, which is part of the layer array
rather than a new allocation — until `ensure_slots` has to grow, when it
reallocates the whole array and copies it. Say it once, here, so that nobody
"corrects" one document to the other. The number that matters for `retype_float`
is `text-tool`'s, and the number that matters for a cap is `linked-transform`'s.

### 1.6 Neither pen document looked at `winit-android` 0.31

`pen-platforms` tabulates `winit-win32`, `winit-wayland`, `winit-x11` and
`winit-appkit` at `0.31.0-beta.2`. `mobile` §4.3 names the missing `ToolType` —
and therefore the absence of palm rejection — as "the single most consequential
gap" on Android. Whether the 0.31 backend fills `TabletToolData` for Android, and
whether `PointerKind::TabletTool` is distinguished from `Touch` there, is the
question that decides whether these two are one piece of work or two, and neither
document answers it. **That is the one measurement to take before either is
scheduled.** (I could not take it either: only `winit-0.30.13` is in this
machine's registry, so every claim about the beta crates in `pen-platforms` is
unverified here and rests on that author's downloads.)

### 1.7 Neither pen document costs the 0.31 upgrade itself

`WindowEvent::Touch` **is gone** in 0.31 — `pen-platforms` §2 says so and then
budgets one new `Route` arm for the upgrade; `mobile` assumes the touch arm as it
stands, because it is the only channel Android has. `Touch` is also the only
channel a Windows pen has. So the upgrade rewrites `window_event`'s touch family,
`gesture::contact`, `gesture::press`, the pinch, the hover-that-never-`Started`
rule, `PressureModel`'s route and `InputLog::Route`.

It is not a footnote to a tilt feature. It is also, in the other direction, the
change that *retires* three of Umber's own inferences — a pen is a touch, a
`Moved` without a `Started` is a hover, and absent-versus-zero for tablet tools —
so it makes that code smaller. Both of those are worth saying; neither document
says either.

---

## 2. The two flagged collisions, settled

### 2.1 One `Edit`, several patches

**Adopt three arms, and no entry mixes them:**

```rust
pub enum EditBody {
    Pixels(Vec<PixelPatch>),   // linked-transform
    Structure(Box<StackShape>),// structural-undo
    Flip,
}
```

with `Edit::patches() -> &[PixelPatch]` replacing `patch()`, `byte_len` summing,
and `swap_patch` a loop. Whichever lands first writes `patches()`; the other adds
only its own arm.

`linked-transform` §6's alternative — a structural arm carrying patches *beside*
a structural record — is the one to refuse, and `structural-undo` §6's last
paragraph has the reason: a body that means the same thing two different ways
depending on the kind. Check it against the rules:

- **"`EditKind` has a variant only for something the engine can restore."**
  Untouched. `EditBody` is machinery; `EditKind` is the row. `linked-transform`
  adds no variant (a six-layer transform *is* a Transform) and `structural-undo`
  adds six, each for something the engine now genuinely can restore.
- **"Two rows that undo identically must not have two names."** This governs
  `EditKind`, not `EditBody`, and `structural-undo` §5's reading is the right
  one: the rule is about what the painter did, or it collapses every structural
  edit into one row called "Layers". Add, Delete and Move pass it; a folder
  delete and a layer delete do not, and it correctly refuses to name them apart.
- **"Stepped rather than seeked."** This is what makes the three arms
  independent rather than a compound entry. A delete and the paint before it are
  reached in order, so the paint never has to be carried inside the delete —
  which is precisely why nothing needs a body holding both.
- **An unreadable manifest is discarded, not refused.** See below.

**Does `required_history_version` generalise to structural entries? Yes, and it
should replace the shared-number question entirely.** `linked-transform` §7
mirrors `docformat::required_version`: the manifest states the lowest revision it
needs — 3 for a history of single-patch entries, 4 when one carries more. Apply
the same to `Structure`, and the two features stop having to agree on a number at
all: each contributes a clause, exactly as masks and clipping each contribute one
to the document's version.

It is *safer* here than for the document, and for a reason worth writing into
`docformat::history`'s module docs: an old build handed a document revision it
does not know **refuses the file**, whereas an old build handed a history
revision it does not know **discards the history and opens the picture**. So the
per-file revision costs nothing in safety and buys that the overwhelming majority
of histories — one patch an entry, no structural rows — stay readable by every
build that ever read them. `structural-undo` §6.3's "one bump, not two" becomes
unnecessary rather than wrong.

Two amendments to `linked-transform` §7 while it is being written:

- The manifest field should not be called `patches`. It holds the patches *after*
  the first, because the first stays in the flat fields for byte-identity — a
  field named `patches` that excludes a patch is the kind of name that gets
  misread by the reader written a year later. `extra_patches`.
- `structural-undo` §8's save-time truncation at the newest pixel-resurrecting
  entry is orthogonal to all of this and stands as written. Its theorem — a patch
  on a deleted layer is necessarily older than the delete — is sound, and is the
  same stepped-not-seeked argument one level up.

### 2.2 winit: one piece of work, or two?

**One core change, two platform shells, and a shared upgrade that is larger than
either document thinks.**

The two documents agree on the fact — `Touch::force` is 0.30.13's only stylus
channel anywhere, and tilt reaches it only as iOS's `altitude_angle` inside
`Force::Calibrated`. They diverge on what to do because they are looking at
different cells, and the split is clean:

| | owner |
|---|---|
| `InputPoint::tilt: Vec2` → `Option<Vec2>`; the stated convention; `DabInput::TiltDeclination` / `TiltAscension`; the pane's capability line; "absent is never zero" | **one piece of work**, `pen-platforms` stages 1–3, platform-independent, testable without a device |
| Windows `GetPointerPenInfo` re-call, `penMask`-gated | `pen-platforms` stage 1's shell, ~40 lines, **deleted at 0.31** |
| Android raw `MotionEvent` reader for tilt / orientation / `ToolType` | `mobile` §4.4 refuses it as a first move and is right — two consumers of one input queue |
| The 0.31 upgrade | **neither**, and see §1.7 |

`pen-platforms` §6 is the piece of shared thinking both need and only one has:
tilt's absent-versus-zero is *worse* than pressure's, because zero tilt is the
modal reading rather than an edge case, so the capability has to come from the
platform (`penMask`, per-axis `Option`, `NSEventSubtypeTabletPoint`) and never be
inferred from values, and there must be no latch. That rule is what Android
needs too, and `mobile` never states it.

What the upgrade does to each: Wayland pressure *and* tilt arrive for free and
are unreachable any other way; Windows moves off the local call; X11 gains
nothing (the axes are still unread upstream); macOS gains nothing (zero tablet
code on master); Android is **unknown**, per §1.6, and is the cell that decides
whether palm rejection is upstream's problem or Umber's. Nothing here is
schedulable, because it is blocked on egui PR #7731, which is an open draft that
does not run.

---

## 3. What is already fixed

Both bugs the design work turned up were committed before the documents landed,
and two documents still present them as outstanding.

- **`begin_float`'s ceiling** — fixed in `0cd8c35`; `canvas.rs:2773` now tests
  `MAX_SLOTS`. `linked-transform` §4's "A bug found on the way" and **stage 1 of
  its plan are done**. Its arithmetic is unaffected.
- **`isolation="auto"`** — fixed in `78cc899` for the *writing* half only
  (`docformat/mod.rs:928`, guarded at `:1307`). `group-compositing`'s §4.5 table
  row "Umber, today … **no, and §4.1 is the bug**" is stale, and §4.1 describes a
  file Umber no longer writes. **Half of its stage 0 remains**: the reader.
  `docimport::openraster::parse_stack` still does not look at `isolation` at all,
  so the three-case split in §4.4 is entirely unbuilt.

---

## 4. Where a document is wrong

**`group-compositing`'s load-bearing correction is right, and I checked it.**
Its claim is that `layer-folders.md` §7's `clip_stack` is unnecessary because
nothing ever reads a restored `clip_alpha`. `composite.wgsl:257–261` is the whole
of the evidence:

```wgsl
if (clipped) { lay = lay * clip_alpha; }
else { clip_alpha = select(0.0, lay.a, visible && opacity > 0.0); }
```

Every entry either reads `clip_alpha` or overwrites it; there is no third case.
A group close is an unclipped entry, so it overwrites — and the value saved at the
matching push is dead from that instant. Seven registers and a second array
saved, and the shader is smaller than `layer-folders.md` says.

**Its stated precondition is the real one and must be kept visible**: this holds
only while a folder can never itself be clipped, because a clipped close would
*read* `clip_alpha` rather than write it. §6 defers clipped folders for
independent reasons; the deferral is now also what keeps `MAX_GROUP_DEPTH` a
single array. Anyone reviving a clipped folder is reviving `clip_stack` with it.

Two more of its numbers check out: `MAX_GROUP_DEPTH = 7` is exact (depths 0..=7,
a folder at 7 can hold nothing and empty groups emit no entry, so seven folders
open at once and the push writes indices 0..=6), and `draws ≤ entries ≤ 64` holds
because `LayerStack::MAX` counts folders. Both are worth the CPU tests §9 asks
for, because both are equalities a later change breaks in silence.

Elsewhere:

- **`text-tool` §9 stage 1 is wrong about the clipboard** — §1.4.
- **`structural-undo` §5's `AddMask`** is, as its own author says, the weakest of
  the six variants. Keep it: a `RemoveMask` with no `AddMask` beside it is a
  half-drawn pair, and the enum is exhaustive over `panels::edit_icon` so the
  asymmetry would have to be defended at the icon too.
- **`linked-transform` §10's stage order puts the cap (stage 6) before the cost
  reduction (stage 7).** That is backwards. Eight members × three canvas-sized
  textures is 1.61 GB at 4096², which the document itself calls "the wrong side
  of comfortable", and stage 7 — region-sized `base` and `source` — is a 3× cut
  that grows with N. Do stage 7 first and the cap is a much less
  arbitrary-looking eight. Stage 7 is also what makes a 10000² *text* float
  affordable, which is a second caller it does not know it has.
- **`group-compositing` §8 says a folder's opacity is not undoable, which is
  correct and incomplete.** A folder's opacity is the one property in the model
  whose value decides `required_version` — dragging a slider to 99% changes what
  revision the file declares and therefore whether it opens in an older Umber at
  all. That makes §7's "say somewhere that a group below 100% is isolated" not
  optional garnish; it is the only warning the artist gets.

---

## 5. Sequencing

```
group-compositing 0 (reader half) ─── independent, do now
structural-undo 1 (slot pool)     ─── independent
structural-undo 6 (clear as Erase)─── independent
pen-platforms 1–3                 ─── independent of the whole engine
mobile 1 (cross-compile in CI)    ─── independent
text-tool 0 (font pipeline)       ─── independent

group-compositing 1  ──►  group-compositing 2  ──►  3  ──►  4
        │
        └──────────────────────────────────────►  linked-transform 5

structural-undo 2 ──► 3 ──► 4 ──► 5
        ╲
         ╳  serialise on EditBody / Edit::patches() / swap_patch / App::reverse
        ╱
linked-transform 3 ──► 4 ──► 7 ──► 6

text-tool 1 ──► 2 ──► 3        (3 is also gated on the clipboard row)
        ╲
         ── shares `Floating` and the once-per-frame float gate in `render`
            with linked-transform 6

pen-platforms 4 (winit 0.31)  ──  blocked on egui, unschedulable
mobile 4 (Android stylus)     ──  blocked on §1.6's measurement
```

Three edges are worth naming because they are not obvious from any single
document:

- **`group-compositing` 1 blocks `linked-transform` 5** (§1.1). It does not
  block anything else, including its own stage 2.
- **`structural-undo` 2 and `linked-transform` 3 must not run concurrently**,
  even in separate worktrees. They edit the same four sites and the merge is not
  textual — whoever is second inherits the other's `patches()` signature.
- **`text-tool` 3 and `linked-transform` 6 both rewrite the float ownership
  gate** in `render` — one to stop naming the tool, one to ask `float.holds(...)`
  instead of comparing a slot. Both changes are right and they are one edit.
  `Floating` grows a fixed-capacity slot array *and* the owning tool in the same
  commit, or one of them rewrites the other.

---

## 6. Ranked by value against cost

Value is to a painter, cost is to whoever builds it. "Done" and "half done" are
§3.

| | Stage | Value | Cost | Verdict |
|---|---|---|---|---|
| 1 | `structural-undo` 2 — `Structure`, add + delete recorded | **Highest in the set.** Deleting a layer stops silently destroying the afternoon | High, and the author is right that it cannot be made smaller | **Build first.** The one feature here that stops losing work |
| 2 | `group-compositing` 0 (reader half) | Files Umber writes stop meaning the wrong thing elsewhere | Hours | **Do now.** Finishes a bug fix already half in |
| 3 | `structural-undo` 6 — Clear layer as an `Erase` entry | A whole command joins the history | Very low, independent of everything | **Do now.** Could ship this week |
| 4 | `structural-undo` 1 — the slot pool | None on its own; makes 2 possible | Low, pure `umber-core` | Build, as 2's first commit |
| 5 | `text-tool` 1 — type in a panel, place as a float | A text tool that did not exist. Captions, in any font on the machine | Moderate: one crate, one module, one panel. No GPU, no format change | **Build.** The best value-per-line in the set, and see §1.4 before promising paste |
| 6 | `pen-platforms` 1–3 — the tilt core, Windows shell, the pane | Eleven shipped brushes come alive; the brush editor gains two inputs | Low, and most of it is platform-independent and testable | **Build.** Ship the pane instrument in the same change, never after |
| 7 | `group-compositing` 1 — the draw list, nothing isolated | None visible | Moderate | **Build**, because it unblocks §1.1 and buys the risk down |
| 8 | `structural-undo` 3, 4 — masks, reorder, group | The rest of the history's honesty | Low, riding on 2 | Build with 2 |
| 9 | `group-compositing` 2 — the shader | Group opacity and blend, which is the point | **The riskiest thing in the set.** An accumulator array that may land in scratch on a driver nobody here can profile | Build, and **measure before merging** — RGA, `dxc`, Mali offline. §2.4 names the fallback; factor the loop body out whether or not it is used |
| 10 | `group-compositing` 3, 4 — model, file, interface | Completes 2 | Low once 2 lands | Build with 2 |
| 11 | `text-tool` 2 — the font list | A usable font picker rather than a list of names | Moderate; the cache-key trap is a known wgpu panic | Build with or shortly after 5 |
| 12 | `structural-undo` 5 — the history in the file | A saved history survives a delete, truncated | Moderate | Build, after 2–4 have settled |
| 13 | `linked-transform` 7 — region-sized `base`/`source` | 3× less float memory, for every float including text's | Moderate; touches `render_float` | **Promote above the rest of its document** — §4 |
| 14 | `linked-transform` 2, 3, 4 — `transform_set`, plural `EditBody`, the file | Prepares the feature; 3 is the §2.1 settlement | Low each | Build 3 whenever the undo work is quiet; 2 and 4 are cheap |
| 15 | `linked-transform` 5, 6 — `FloatSet`, the cap, the gates | Moving six linked layers in one gesture, instead of six | **High**, and the lowest value-per-unit-cost of the engine work: a `FloatSet` split, a cap, a notice, two gates, a GPU test that reads four slices | **Build last of the engine work**, and only after 13 |
| 16 | `mobile` 1 — `cargo build --target aarch64-linux-android` in CI | Evidence, which is the only thing anybody here can get | One CI job | **Do now.** It costs a job and settles whether the graph compiles |
| 17 | `text-tool` 0, 5 — the font pipeline; text as a selection | Small, self-contained, genuinely useful | Half a day each | Build when convenient |
| 18 | `text-tool` 3 — on canvas | What makes it feel like a text tool | **Five independent hard things, four invisible on this machine**, one of which (IME) cannot be tested by anybody here | Build only after 5 and 11 have been used in anger. Scoping IME out and naming it in the README is an honest ship |
| 19 | `mobile` 2–9 — Gradle, storage, SAF, the tablet dock | A tablet build | Large, and a Java build system enters the repository | Defer. §16 first; nothing after it is worth starting until the compile is green and somebody has a device |
| 20 | `pen-platforms` 4, 5; `mobile` iPad; X11 | — | — | **Not now**, and all three documents say so themselves. Endorsed |

### What should not be built

- **`text-tool` stage 4 — text layers that reopen editable.** The document is
  honest that it is "a model change of the same size as folders were": a layer
  *kind*, with a paint gate, a row mark, an answer for masks, for clipping, for a
  folder containing one, for `LayerStack::MAX` — plus a pixel fingerprint and a
  frozen-on-missing-font rule to keep the version argument true. It also collides
  with two other features in this set: `StackShape::Gone` would have to carry the
  kind, and a text layer inside an isolated group is a third thing to reason
  about. Against all that, Krita's answer is "paint text", which is stage 1. Do
  not build it until a text tool has been in somebody's hands for a while, and do
  not let stage 1 acquire any of its machinery in advance.
- **`structural-undo` 7 — deleted layers' images in the saved history.** The
  document correctly refuses to decide it without a measurement. Leave it open;
  the truncation is a perfectly good permanent answer until somebody complains.
- **`linked-transform`'s `targets` alternative** — its §2 already refuses it, and
  the refusal is right for a reason worth repeating: a press on the canvas is not
  a command, and ticks left over from an operation three minutes ago would
  silently move layers with nothing on the canvas to say so.

---

## 7. What nobody owns

- **Releasing a history-held slice before an operation that needs one.**
  `structural-undo` §7 answers it for `add`. `add_mask` and `begin_float` need
  the same release, the release cannot happen inside `umber-render`, and the
  refusal it prevents is a notice saying Umber has run out of room. §1.3.
- **A byte budget for float storage.** `linked-transform` §11 explicitly disowns
  it; `text-tool` needs it for a 10000² text float; `group-compositing` §2.5
  rejects the pass-per-group partly on transient memory. All three circle the
  same fact: an allocation failure in `ensure_slots` or `make_float_texture` is
  an uncaptured device error, which `crash::device_error` makes fatal. So the
  failure mode of "this document is too large to transform" is the crash
  reporter. **This is a pre-existing bug, not one these features create**, and it
  is the largest unowned risk in the set.
- **The winit 0.31 upgrade.** §1.7. It is nobody's stage and it is the
  prerequisite for three cells across two documents.
- **Whether `winit-android` 0.31 answers palm rejection.** §1.6.
- **`architecture.md`'s roadmap has two rows that are one piece of work.**
  "Native tablet pressure on desktop" and "Tilt support" are separate entries;
  `pen-platforms` §0 shows that wherever a backend delivers pressure it delivers
  tilt in the same struct, and the two are never separate work. Its "Android and
  iOS build scaffolding" is likewise one row for two nearly unrelated projects,
  one of which `mobile` recommends refusing outright. Neither is in scope for
  this document to change, and both should be when somebody next edits that file.
- **The seventh feature.** The roadmap has no row for a text tool at all — it
  lives only in the README's "ten of sixteen tools". If `text-tool` 1 is built,
  that is the row to add.
