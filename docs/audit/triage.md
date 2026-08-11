# The five-way audit, ordered

Five audits sit beside this file. They are good and this does not summarise them
— read them. This is the cross-cutting view none of the five authors had: where
they found the same defect from different angles, which findings are cheap only
because another is being fixed, which one is wrong, and what should not be built.

Read against `bc77837`. The audits read `cd44fa1`, one commit behind — and that
one commit is `crashes.md` finding 6 already applied, so **that finding is
closed**; the `CLAUDE.md` sentence is inverted and `panic = "abort"` is recorded
as a thing that must not be set.

**Five fix agents are in flight and their findings are not re-planned here.**
What follows is everything else, plus three messages that should reach those
agents before they finish.

---

## 1. The order

### Wave 0 — three messages, no new agents. Send now.

Each is a correction to an agent already at work, and each is something that
agent cannot see from inside its own remit.

| to | what |
|---|---|
| **agent 3** (allocation) | The flip's crash and the flip's silent paint loss are **two ends of one condition**. See §2.1. Making `ensure_pages` fallible without propagating the refusal converts a crash box into permanent, unrecoverable, silent loss. Take `correctness.md` #2 with it. |
| **agent 4** (mutations) | **Five** tests already construct a `ThumbnailProvider` (`lib.rs:531, 569, 581, 595, 642`), not the two `lifecycle.md` #6 names. A sixth without a lock makes the `LIVE` flake worse. Take `lifecycle.md` #6 and #9 first, or build no provider in the new bitmap guard. |
| **agent 1** (dead workers) | `autosave.rs:1745`'s `while let Ok(report) = rx.try_recv()` **is** `.ok()` spelled as a loop, and is the third step of `lifecycle.md` #2. The grep that finding prescribes does not find it. See §2.4. |

### Wave 1 — two agents, parallel, worktrees. Start now.

Neither touches a file the five hold, except `app.rs`, where agent 2's change is
four lines in `App::reverse` and a conflict there is trivial.

| | what | files | critic |
|---|---|---|---|
| **1a** | **The controls that read a narrower predicate than the model.** `interface.md` #1, #2, #5, #6, #7 are one defect with five faces: the row, the two Cut gates, the three folder commands, the `＋`'s tooltip and the notice naming a closed panel. All are "read `active_refusal`, not `active_is_locked`/`active_slot`" plus a mark on the row. Doing them apart is five commits and five arguments about the same rule. | `widgets.rs`, `panels.rs`, `ui.rs`, `app.rs` | **yes** — a wrong refusal reading disables a control that should work, which is the opposite defect |
| **1b** | **The mechanical exhaustiveness sweep.** `coverage.md` B3 (`DabTarget::is_colour` → a `match`, plus an arms-index-`ALL` guard), B4 (three `matches!` over `ExportFormat` → one exhaustive answer), A5 (delete the loop that is its own subject), B7 (`Grid::tiles_over`'s clip guard, and its stale "no caller outside the tests"). | `dynamics.rs`, `export.rs`, `tile.rs` | no |

**1a is the one to start first.** It is the only blocking finding left after the
five, and the panel that would explain the refusal (`PanelKind::Text`) is not in
`DEFAULT_DOCK`, so on a fresh install nothing anywhere names the cause.

### Wave 2 — after the five merge. Three agents, parallel.

| | what | files | critic |
|---|---|---|---|
| **2a** | **The effect refusal is unreliable in three independent ways.** §2.3. Collect it from every canvas the way `settle_capture` now is (`lifecycle.md` #4), fix the latch that two bakes in one frame defeat (#8), and only then ask whether `Editor::notice`'s single slot needs anything (`interface.md` #8 — it probably does not, once 2a lands). | `canvas.rs`, `autosave.rs`, `app.rs` | **yes** — moving a collection point across canvases is the shape `settle_capture` took three attempts to get right |
| **2b** | **The `at_risk`-only offer leaks its markers and their locks** (`lifecycle.md` #3). The common case after any reboot. | `autosave.rs`, `app.rs`, `recoverdlg.rs` | **yes** — it deletes files; `Marks`' containment is the thing to get right |
| **2c** | **The saved-history reader's eleven refusals, seven of them undriven** (`coverage.md` A4/B6). Tests only. The fixture must not be 64 × 64: that is the shape which makes the `x`/`w` and `y`/`h` halves of both bounds checks indistinguishable, and the existing one is square. | `docimport/history.rs` | no |

Handed to agent 3 rather than dispatched, because they are in files it holds and
each is under thirty lines: **`preview.rs`'s `fit_within`** (§2.2) and **the
Krita `saturating_add`** (`crashes.md` #9). If agent 3 declines either, they are
wave 2.

### Wave 3 — smaller, and a design document.

- `correctness.md` #4 (`Capture::empty` left stale beside a cleared `gaps`) and
  #5 (a figure of 2 where the answer is now 4). One line each.
- `lifecycle.md` #7 (`installwin` holds its last step on a vanished worker) and
  #13 (a comment claiming a dropped `JoinHandle` joins). Same file, same rule as
  wave 0's message to agent 1.
- **Layer effects have no interface**, and that is a build item rather than a fix
  — see §4.5. What is owed now is prose, in §5.

---

## 2. What none of the five could see

### 2.1 The flip crashes and the flip loses paint, and it is one condition

`crashes.md` #5 and `correctness.md` #2 are 160 lines apart in the same function.

`flip_layers` grows the atlas through the **infallible** `ensure_pages`
(`canvas.rs:6542`), so a card that cannot supply one more page plus a scratch
turns Image → Flip into the crash box. Then, at `canvas.rs:6695`, a tile the free
list cannot back is dropped with a `log::error!` and a `continue`.

Today the second is unreachable *because* of the first: the growth asked for
exactly `wanted`, so `free.pop()` cannot fail. **Agent 3's fix inverts that.** The
moment growth becomes fallible, the `continue` becomes the live path — and a
flip's undo is another flip, carrying no pixels, so the dropped tiles are in no
patch, in no parked slice, and reproducible by no gesture. The naive fix trades
a crash box the artist can see for permanent silent loss they cannot.

`back_tiles`' reasoning — "the tiles that could not be backed are logged and
skipped, and the stroke loses them; refusing the whole stroke loses more" — is
sound for a stroke and does not carry here. The flip needs the refusal channel
`take_effect_refusal` already has, and needs to refuse **whole**, which is the
rule a locked layer already forces on it.

Neither auditor could write this: the correctness reader did not know growth was
about to become fallible, and the crashes reader did not rank the tile drop.

### 2.2 `preview.rs` is three findings and one file

`coverage.md` A2 (no portrait fixture, so `w.max(h)` → `w` is green and every
A4-shaped document gets a wrongly-shaped Explorer thumbnail), `crashes.md` #10
and `coverage.md` B9 (the same unreachable arm returning a `Preview` whose size
disagrees with its buffer), and `crashes.md` #2's second half (`decode_png` with
no header ceiling) are twenty lines of one file. Agent 3 is in it. One commit,
or three across two waves.

### 2.3 One notice, three ways of not arriving

The effect refusal is, in `lifecycle.md`'s own words, "the only thing on this
path that tells the artist their effects are not being drawn". Across two audits
it is:

- **recorded on a canvas nobody collects from** — `take_effect_refusal` reads the
  *active* canvas and `autosave::drive` bakes the *due* one (`lifecycle.md` #4);
- **duplicated** where two bakes run in one frame, because the second reads a
  latch the first consumed (`lifecycle.md` #8);
- **overwritten** by the autosave's own notice nine lines later, and it is
  latched once per episode so it does not come back (`interface.md` #8).

And it names a feature with **no control anywhere to act on it**
(`interface.md` #4). A message about a state the artist cannot change, which may
not arrive, may arrive twice, and may be silently replaced. Fix the collection
and the latch; the single-slot `Notice` is then a hypothetical rather than a
live loss, which is the right order — see §4.6.

### 2.4 The greppable pattern is wider than the grep

`lifecycle.md` closes by prescribing `try_recv().ok()` as the shape to grep for.
The workspace has six `try_recv` sites; that grep finds **one** of the two that
are defective. The other, `autosave.rs:1745`, is

```rust
while let Ok(report) = rx.try_recv() { … }
```

which discards `Disconnected` identically and is step 3 of that audit's own
finding #2. The three sites that are correct (`textpanel.rs:215`,
`installwin.rs:288`, `update/mod.rs:266`) all `match`; `prefs.rs:339` is a worker
draining its own inbox and is fine.

**The rule is `match`, not the spelling.** `while let Ok(_) = try_recv()` and
`try_recv().ok()` are one defect with two shapes, and a grep for either finds
half of it.

### 2.5 A finding that is worse than reported

`lifecycle.md` #6 says "at least two other tests in the same module create
providers". There are **four** (`lib.rs:531, 569, 581, 595`) besides the one that
reads the counter. The flake is a five-way race, not a three-way one, and the
guard agent 4 is about to add is the sixth participant unless it is told.

### 2.6 A finding that is not wrong but is already closed

`crashes.md` #6 (`panic = "abort"` is not set, and `umber-shellext::guard`
depends on that) landed as `bc77837` before this triage was written. Recorded so
nobody dispatches it.

---

## 3. The disowned-job pattern: yes, a rule — but not a shape

Six confirmed instances in two days: `cancel_capture`, `touch_slot`,
`Loading::take`, the autosave writer, `Offer::marks`, and — the one the audits
name but do not connect — `autosave::poll`'s dead receiver.

Three fixes already in the tree, and **all three are different shapes**, because
the situations genuinely differ:

- `take_thumb` **abandons at the collector**, because the resource is one 16 KB
  buffer and the result is discardable;
- the capture **settles from a driver that runs unconditionally**, because
  something already runs every frame for every canvas;
- the update worker **reports its own death**, because the job is on another
  thread and its ending is only visible as a channel state.

So a mechanical rule — "every job gets a `Drop`", "every state gets a timeout" —
would be wrong three ways. The rule that fits all six is a **question asked where
the state is made**, not a mechanism:

> **A state that only one collector can leave must name that collector where the
> state is created, and say what happens if it is never reached.** Not "who
> clears this" — that is usually written down — but *when this ends
> unexpectedly, who notices?* Six instances in two days; the three repairs in the
> tree are three different shapes, so the rule cannot be a shape. Its one
> greppable corollary is a channel: `try_recv().ok()` and
> `while let Ok(_) = rx.try_recv()` are the same defect in two spellings, and a
> dead worker is indistinguishable from a busy one under either. Match on
> `Disconnected`.

That belongs in `CLAUDE.md` beside the `cd44fa1` note it generalises. The prose
is §5's.

---

## 4. What should not be fixed

`CLAUDE.md` is full of deliberate refusals with reasons, and a finding that
re-proposes one costs an agent. These are the ones to leave alone, with why.

**4.1 Do not give the loading modal a Cancel.** `tabs.rs:668` argues it and both
auditors who found the wedge agree: a Cancel that cannot interrupt the worker is
a control that lies. The fix is one line away from the wrong one, which is why it
is named here.

**4.2 Do not bound `import_reporting`'s file read** (`crashes.md` #8) at a figure
a real document could meet. A measured `.clip` is 307 MB and the artist chose the
file; the hostile-file path is the shell extension's, and that one *is* bounded,
at 1 GiB. If a ceiling is wanted it must be a stated one well past every surveyed
document, and the survey has to be re-run before the figure is written. What is
free is documenting the asymmetry, which is where the value was anyway.

**4.3 Do not set `panic = "abort"`**, and note the sentence has already been
inverted. `umber-shellext::guard` and `photoshop::catch` both depend on unwinding
and the first is loaded into a process nobody here owns.

**4.4 Do not fix `suspended()`'s stranded gesture as a behaviour change now**
(`lifecycle.md` #5). Android has never been built or run, so a three-line fix to
an unrunnable path, guarded by a test of a method invented for the test, is a
guard agreeing with itself. What is genuinely owed is the *factoring* the audit
proposes — and it should be taken by whoever is next in that path, not by an
agent sent there for it.

**4.5 Do not build an Effects panel from `interface.md` #4 alone.** It is right
that this is the largest field the format round-trips and the interface cannot
author, and it is a build item on the scale of a brush-editor section: ten
parameters over two kinds, with `plan_set_effect`'s budget refusal and the
over-budget import warning both needing somewhere to land. `docs/layer-effects.md`
§4 is the standing design and nobody has checked it against what actually
shipped. That check is the next step, not a panel. What is owed *today* is two
sentences of prose (§5).

**4.6 Do not turn `Editor::notice` into a queue** (`interface.md` #8). The only
demonstrated loss is the effect refusal, and §2.3's collection fix removes the
frame in which the two collide. A queue of notices is a thing that accumulates
and then needs a policy for what it drops; decide that when a second real
collision exists.

**4.7 Do not act on `Fonts::forget`** (`lifecycle.md` #12) — wasted work bounded
by how fast somebody can change a preference — or on **`app.rs:3717`**
(`lifecycle.md` #14), which the audit already refuses in favour of a comment.

**4.8 Do not re-find the Photoshop `is_clipped` inversion.** It reads as a bug
and is Adobe's own byte; `correctness.md` caught itself and recorded the
near-miss. Recorded again here so the next pass does not spend the agent.

---

## 5. Prose owed, for the coordinator

No agent may edit these three files; this is the collected list.

**`CLAUDE.md`**

- Layer effects: *"Stage 0 only: nothing bakes, nothing draws"* is stale. They
  bake on the frame path, draw, round-trip the format at `umber-version` 3, and
  can be refused for VRAM. What is true is that **nothing authors them**.
- Add the disowned-job rule from §3, beside the `cd44fa1` note.
- The stale-claim class is worth one sentence of its own. Nine were found in this
  audit — `try_reserve`'s enumeration, `thumbs.rs`'s "every method",
  `tile.rs`'s "no caller outside the tests" (now ten), `vram.rs`'s "three call
  sites" (now four), `installwin`'s joining `JoinHandle`,
  `docformat/history.rs`'s `umber-version` 2, the effects stage, the README's
  loupe, and `panic = "abort"`. The file already says *"a doc comment that names
  a call site is a claim"*; what this adds is that **a claim about a count goes
  stale the moment the count moves, and nothing compiles it.**

**`README.md`**

- Remove *"There is no magnifier under the cursor yet"* from "What is not there
  yet". `loupe.rs` is built, wired, drawn and advertised by
  `syspick::outside_detail()` (`interface.md` #3). The words are doubly wrong: the
  loupe is deliberately **not** under the cursor, because it is read off the same
  screen it is drawn on.
- Add layer effects to that section: read, drawn and saved, not yet editable.

**`CHANGELOG.md`** — nothing from this triage. Every item above is a fix or a
test rather than a release note.
