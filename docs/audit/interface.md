# Interface audit: controls that lie, gestures that cannot be reached

Read-only audit of `umber-app` against the standard `CLAUDE.md` states for
itself: *a disabled control with an explanatory tooltip is better than a live one
that lies, and a control that is simply not drawn is better than either*; and
*every operation a control offers has a `can_` beside it, sharing its plan*.

Scope: what an artist can and cannot do, and whether the interface says so.
Not pixels, not crashes, not performance.

**One blocking finding.** The gesture layer — the part with the worst history —
came through clean; every one of the pen-versus-mouse traps `CLAUDE.md` records
is closed, and the two newest overlays (the loupe, the eyedropper's screen read)
are the best-argued code in the crate. What is not closed is the *text layer*:
it is the third thing a layer can refuse a stroke for, and unlike the lock and
the folder it is invisible on the row, unmarked in the panel, and still live on
two Cut controls.

Every finding below is marked **confirmed** (read the code) or **inferred**.

---

## Ranked

| # | Rank | Finding | Where |
|---|---|---|---|
| 1 | **BLOCKING** | A text layer silently refuses every stroke and nothing anywhere on the layer row or in the Layers panel says so. The lock has a mark on the row for exactly this reason. | `widgets.rs:2582`, `panels.rs:1508`, `editor.rs:1557` |
| 2 | SUBSTANTIVE | Cut is drawn **enabled** on a text layer in both places it appears, and answers with a dialog. `CLAUDE.md` records that this button is disabled to match its gate; it is, for the lock only. | `ui.rs:883`, `ui.rs:1719`, `app.rs:1534` |
| 3 | SUBSTANTIVE | The README's "what is not there yet" says there is no magnifier for the eyedropper. There is: `loupe.rs` is built, wired and drawn. | `README.md:349`, `ui.rs:1334` |
| 4 | SUBSTANTIVE | Layer effects round-trip through the document format, bake, draw, and can be refused for VRAM — and there is no control anywhere to author, edit or switch one off. `can_set_effect`, `can_set_effects` and `can_set_effect_enabled` have zero call sites in `umber-app`. | `layer.rs:1672`, `dock.rs:125`, `docimport/mod.rs:1140` |
| 5 | SUBSTANTIVE | With a **folder** selected, "Clear layer", Cut and Copy are all drawn live and do nothing at all — Clear layer not even the history wipe its own tooltip promises. | `ui.rs:1637`, `app.rs:2080`, `app.rs:1461` |
| 6 | MINOR | The add-layer `＋` disables at `LayerStack::MAX` and keeps the tooltip describing what it would do. `icon_button`'s own docs say a disabled tooltip is meant to carry the reason. | `panels.rs:1468`, `ui.rs:2371` |
| 7 | MINOR | Two refusal notices send the artist to "the Text panel", which is not in `DEFAULT_DOCK` and is closed on a fresh install. | `app.rs:1539`, `app.rs:6154`, `dock.rs:152` |
| 8 | MINOR | `Editor::notice` is a single slot written by assignment; two notices raised in one frame silently lose the first. Reachable today between the effect-bake refusal and the autosave's. | `app.rs:3913`, `app.rs:3922` |
| 9 | MINOR (already recorded) | A `TextEdit` keeps focus after the pointer leaves it, so the first Space-drag, Escape or Enter over the canvas after typing is swallowed. Self-heals on the second attempt; nothing on screen says so. | `shortcuts.rs:608` |

---

## Evidence

### 1. BLOCKING — a text layer refuses the brush in silence, and the row does not say

**Confirmed by reading.**

`Editor::begin_stroke` is the single gate for every route to a stroke, and it
refuses on `LayerStack::active_refusal`, which answers for the lock, the folder
*and* the text record together:

```rust
// editor.rs:1557
if self.layers.active_refusal(target).is_some() {
    return false;
}
```

The refusal is deliberately silent, and that is right — `editor.rs:1543` says so:
"this is reached every time the pen goes down and a notice there would be a
dialog over the canvas." The problem is what is supposed to carry the
information instead. For a lock, it is the row. `widgets::LayerRow` at
`widgets.rs:2624` says exactly why:

> The mark is drawn either way, because an entry that refuses strokes,
> transforms and deletion has to say so — a stack where the lock is real and
> invisible is one where every one of those refusals arrives as a surprise.

A text layer refuses strokes, refuses a cut and refuses a paste. `LayerRow` has
`locked`, `locked_by_folder`, `clipped`, `has_mask`, `link`, `folder` — and **no
text field at all** (`widgets.rs:2582`–`2649`). Nothing in `panels.rs` reads
`Layer::is_text`, `LayerStack::active_text` or `text_at`; grepping the whole
crate, the only non-test readers are in `textpanel.rs` (`textpanel.rs:508`,
`525`, `540`).

The Layers panel's flags row is drawn for a text layer exactly as for a plain
one (`panels.rs:1508`, `if !is_folder`), including the Layer/Mask switch at
`panels.rs:1590` — so on a text layer with a mask the artist is offered a
two-position switch whose **"Layer" position paints nothing**, with the tooltip
"Strokes land in the layer's pixels".

And the module that *would* explain it, Text, is not in `DEFAULT_DOCK`
(`dock.rs:152` — Tools, Colour, Brushes, Layers). So a document opened from a
`.ora` carrying text records lands in a workspace where the brush silently stops
working on one layer and there is no mark, no tooltip, no notice and no open
panel anywhere that names the cause.

This is the folder case done correctly and the text case not done: `panels.rs:1499`
argues at length that a folder's inapplicable controls are *not drawn* rather
than drawn disabled, because it is a whole block that will never apply. Text got
neither treatment.

**Guard I would write** — `a_text_layer_says_so_on_its_own_row`, in
`panels.rs`'s test module, measuring what was drawn rather than what the model
answers. Build a stack whose layer 0 carries a `TextObject`, run `layers_body`
through `ctx.run_ui`, and require some shape or galley in that row that is absent
for an otherwise identical plain layer — the same shape as
`ticking_a_layer_does_not_move_the_layer_list`, which already drives that body
headlessly. A second, cheaper one:
`every_reason_a_layer_refuses_a_stroke_is_visible_in_the_list`, walking
`EditRefusal::ALL` and asserting each has a mark, so a fifth refusal cannot be
added without deciding what the row shows. That is the `EditKind`/`edit_icon`
pattern applied to refusals.

Note the trap `CLAUDE.md` names twice: a guard on `refusal_at` is not a guard on
the panel. `LayerStack::refusal_at` is well covered and covers nothing here.

### 2. SUBSTANTIVE — Cut lights up on a text layer and answers with a dialog

**Confirmed by reading.**

`App::cut_selection` refuses four ways, exhaustively and with no catch-all
(`app.rs:1518`–`1551`): `Locked` raises a notice, `Text` raises a notice,
`Folder | Missing` return silently.

Both controls that offer Cut read the lock alone:

```rust
// ui.rs:878, in selection_buttons
let locked = ed.layers.active_is_locked();
… (Icon::Cut, !locked),
```

```rust
// ui.rs:1719, the Edit menu
if menu_item(ui, Action::Cut, !ed.layers.active_is_locked())
```

So on a text layer the strip's Cut is drawn live with the tooltip "Cut the
selection" and the menu row is drawn live with "Takes the selection onto the
clipboard", and pressing either produces the modal at `app.rs:1534`. That is
precisely the arrangement `CLAUDE.md` says was avoided:

> **A cut is gated on the lock once**, in `cut_selection`, and the button is
> *disabled* to match — the rule "Clear layer" already follows, so the gate
> catches a keystroke rather than being the only thing between a live control and
> a dialog.

The sentence is still true of the lock and false of the refusal added beside it.
The fix is one reading: `active_refusal(EditTarget::Layer)` in place of
`active_is_locked()` at both sites, with the disabled tooltip taken from the
refusal so the strip's Cut can say "This layer holds text" the way it already
says "The layer is locked, so nothing can be cut out of it."

Paste is a deliberate exception and should stay one — `ui.rs:1737` argues it
correctly (what a paste puts down is the *desktop's* clipboard, read at paste
time, and reading it per frame blocks).

**Guard** — `no_control_offers_a_cut_the_model_will_refuse`: for each
`EditRefusal`, put the stack in that state, draw both `selection_buttons` and
`edit_menu` through `run_ui`, and require the Cut control to be insensitive
wherever `cut_selection` would return early. Driving both call sites is the
point; a guard on `active_refusal` alone would pass with the defect in place.

### 3. SUBSTANTIVE — the README denies a feature that shipped

**Confirmed by reading.**

`README.md:346`–`349`, inside "What is not there yet":

> Picking a colour from outside the window is Windows only. … **There is no
> magnifier under the cursor yet, so a one-pixel target takes a steady hand.**

`loupe.rs` is a complete module with its own placement rule, `syspick::
sample_patch` reads an 11×11 block in one `BitBlt`, `Editor::loupe` is populated
by `App::read_under_cursor` (`app.rs:2896`), and `ui::loupe_overlay`
(`ui.rs:1334`) draws it on every frame of a pick. `syspick::outside_detail()`
even advertises it to the artist: "The loupe shows what a release would take".

`CLAUDE.md` holds that section to the same standard as the rest of the shop
window — "a feature that half-works is named, with what it does and does not
do". This is the opposite failure and the rarer one: a shipped feature named as
absent, so a reader decides against something Umber has. (The literal words are
also now doubly wrong: the loupe is deliberately *not* under the cursor, because
it is read off the same screen it is drawn on.)

Also worth a line in that section: layer effects (finding 4) appear nowhere in
the README, neither as a feature nor as a gap.

**Guard** — this class is guardable the way the download table already is.
`the_readme_does_not_deny_a_module_that_exists`: a test in `umber-desktop`'s
`release.rs` neighbourhood mapping a handful of sentinel phrases in "What is not
there yet" to a `cfg`/module predicate, so deleting `loupe.rs` and deleting that
sentence are the two ways to make it pass. Cheap, and it is the one part of the
README that goes stale by things being *built*.

### 4. SUBSTANTIVE — layer effects are authorable by no control

**Confirmed by reading.**

`LayerStack` exposes `plan_set_effect`/`can_set_effect` (`layer.rs:1672`),
`can_set_effects` (`layer.rs:1741`) and `can_set_effect_enabled`
(`layer.rs:1786`). Grepping `crates/umber-app/src` for all three returns
**nothing outside tests**; the only `set_effect` calls in the crate are in
`app.rs:6368`, `autosave.rs:3527` and `editor.rs:2827`, all under `#[cfg(test)]`
or fixtures. `PanelKind::ALL` is eight kinds and none of them is Effects
(`dock.rs:125`).

Meanwhile the feature is live end to end: it raised `umber-version` to 3, both
writers emit `umber/effects/`, `bake_effects` runs on the frame path, and a
refused bake now has its own sentence (`vram::effect_refused`, `app.rs:3913`).
`CLAUDE.md`'s "Stage 0 only: nothing bakes, nothing draws" is stale.

The clearest symptom is a message: `ImportWarning::EffectsOverBudget`
(`docimport/mod.rs:1140`) tells the artist "*{disabled} of them were switched
off … Their settings were kept and are saved with the document*" — a state
report with no control anywhere to act on it, and `CLAUDE.md` already notes that
the wording deliberately avoids telling anyone to switch an effect off "because
no such control exists".

This is exactly the lens `CLAUDE.md` prescribes for finding a thin module: *a
field the file format round-trips that the interface cannot author.* It is the
largest such field in the codebase — ten parameters, two kinds, per layer.

Not ranked blocking, because nothing in the interface claims otherwise: no
control lies, there is simply none. But an artist who opens a Krita or Photoshop
file with a drop shadow, or their own Umber document, sees the effect drawn and
cannot touch it.

**Guard** — none to write; this is a build item, and the interim honest move is
one line in the README's "what is not there yet" saying effects are read, drawn
and saved but not yet editable. If a panel is built, the guard is
`every_effect_parameter_has_a_control`, in `Brush`'s shape: an exhaustive match
over `EffectKind` × parameter, so a new field cannot be added without a control
— the rule the brush editor already lives by ("adding one means adding a
control, or the library can use a brush nobody can make").

### 5. SUBSTANTIVE — three "layer" commands are live on a folder and do nothing

**Confirmed by reading.**

- **Clear layer** (`ui.rs:1637`) is enabled on `!active_is_locked()` alone and
  hovers "Empties the layer, and clears the undo history with it."
  `clear_active_layer` returns at `app.rs:2080` on `active_slot()` being `None`
  — before `history.clear()` — so with a folder selected the row is live, says
  it will do two things, and does neither.
- **Copy** (`ui.rs:1730`, never disabled) returns silently at `app.rs:1462`.
- **Cut** returns silently at `app.rs:1549`/`1557`.

The silence itself is argued and I do not dispute it: `app.rs:1546` is right
that "a folder is a perfectly ordinary thing to have selected" and a notice on
every Ctrl+C would be worse. The defect is the *control*, not the handler — the
Layers panel already sets the standard two rows away, drawing none of a folder's
inapplicable flags rather than drawing them dead (`panels.rs:1499`), and the
bulk trash button at `panels.rs:1811` carries a real reason in its disabled
tooltip.

Clear layer is the one worth fixing first, because its tooltip is a specific
promise about the undo history.

**Guard** — `no_layer_command_is_offered_for_a_folder`: draw `file_menu` and
`edit_menu` through `run_ui` with a folder selected and require the Clear layer
and Cut rows insensitive. Measure the drawn rows, not `active_is_folder`.

### 6. MINOR — a dead `＋` with a live tooltip

**Confirmed by reading.** `panels.rs:1468`:

```rust
icon_button(ui, p, Icon::Plus, ed.layers.len() < LayerStack::MAX,
    if ed.layers.active_is_folder() { "Add a layer inside the selected group" }
    else { "Add a layer above the current one" })
```

One tooltip for both states. `ui::icon_button`'s own docs (`ui.rs:2371`) explain
that a disabled control keeps its hover *precisely* so the reason can be shown —
"Several callers pass the *reason* it is dead as the tooltip". This caller does
not, so at 64 entries the artist gets a greyed plus that still says it will add a
layer. Compare `panels.rs:1817`'s "A document needs a layer to paint on".

**Guard** — `every_disabled_icon_button_says_why` is not writable generally, but
`the_add_layer_mark_says_why_a_full_stack_refuses_it` is: fill a stack to `MAX`,
draw the flags row, and require the hover text to differ from the enabled one.

### 7. MINOR — a remedy that names a closed module

**Confirmed by reading.** `app.rs:1539` and `app.rs:6154` both end with
"Convert it to paint in the Text panel". The control exists
(`textpanel.rs:759`), but `PanelKind::Text` is not in `DEFAULT_DOCK`
(`dock.rs:152`), so on a fresh install the named panel is not on screen and the
sentence sends somebody looking for something that is not there. One clause —
"open it from the Window menu" — closes it. The same wording is already handled
well elsewhere: the Cut refusal says "Unlock it in the Layers panel", and Layers
*is* in the default dock.

### 8. MINOR — a notice can be overwritten before it is seen

**Confirmed by reading.** `Editor::notice` is `Option<Notice>` and every producer
writes it by assignment. Within one frame, `app.rs:3913` (a refused effect bake)
and `app.rs:3922` (the autosave's) can both fire, and the second wins. The
effect refusal is latched once per episode (`take_effect_refusal`), so the one it
loses does not come back. Low likelihood; worth a `set_notice` that declines to
overwrite an unseen one, or a small queue.

### 9. MINOR — already recorded, no user-facing signal

`shortcuts::direct`'s docs (`shortcuts.rs:608`) state the accepted cost plainly:
type in the brush search box, move to the canvas, hold Space and drag, and you
paint where you meant to pan; the second attempt works because the press takes
the focus off the field. I am not re-proposing a fix — the docs already name the
honest one (make the suspension mean "the interface has the keyboard") — only
recording that it is the one reachable gesture failure left in the input path,
and that it is invisible to the artist.

---

## Checked and found sound

Recording these so the next pass does not redo them.

- **The pen/mouse gesture split.** No `match` on `Tool` survives in either event
  arm. `window_event`'s mouse arm (`app.rs:5511`) and touch arm (`app.rs:5621`)
  both build a `gesture::Pointer` and hand it to one `gesture::press`; the touch
  arm consults `self.modifiers` for Alt and Space, takes `ui_owns` from the
  *event's* position, and routes through `gesture::contact`. The eyedropper's new
  drag reaches a pen: `Press::Eyedropper` sets `Interaction::Picking` at
  `app.rs:4851` and `Contact::Drag` drives `pointer_moved`. `pointer_released`'s
  `Interaction` match is exhaustive with no wildcard and says why.
- **Canvas overlays.** All three sets — `scroll_bars`, `transform_buttons`,
  `selection_buttons` — are cleared at the top of their drawing function and
  recorded before any early return, on every frame, including for a *disabled*
  button (`ui.rs:895`). `canvas_overlay_owns_pointer` (`editor.rs:886`) is the
  single reader and both pointers reach it through `pointer_over_canvas`. The
  loupe is deliberately a bare layer painter and not an `Area`, so it stays out of
  `layer_id_at` (`ui.rs:1356`).
- **`layer_id_at` under a modal.** Both consumers that mean "refuse"
  (`ui_owns_pointer`, `pen_dot`) share one statement in `editor::over_egui_area`,
  and the one that means "observe" (`InputLog`) skips obscured frames and counts
  them (`inputlog.rs:543`).
- **Hover-revealed widgets.** `palettelib.rs:1096`, `settings.rs:3659` and
  `widgets.rs:1582` all use `contains_pointer`/`rect_contains_pointer`, with the
  oscillation argued at each.
- **Focus and Escape.** `ui::draw` calls `set_typing(ctx.text_edit_focused())`
  once for the whole interface (`ui.rs:427`); `set_capturing` is the chord
  recorder's alone (`settings.rs:3250`). `direct` now answers to the same
  suspension the table does.
- **The tool options strip.** Every optional group reads `available_width()`
  against a stated budget, and the three that follow an unconditional sentence
  (Select's combine line, the eyedropper's second line, Pan/Zoom's) re-read it
  *after* that sentence rather than against a stale figure. `navigate_hint` is
  exhaustive over `Tool` with the five unreachable arms named.
- **Menus.** `menu_item` takes the label and the chord from `shortcuts`, never
  from the call site, and carries `enabled` so a dead row still names its key.
  Undo/Redo/Deselect/Flip/Close document all have real `on_disabled_hover_text`.
- **Disabled-with-a-reason done right**, for comparison: the ticked-layers strip
  (`panels.rs:1811`–`1849`), where the trash, the chain and the link-group ceiling
  each state the specific obstacle.
- **Nine of sixteen tools are not drawn at all** (`panels.rs:863`), which is the
  standard's preferred answer.
- **The new VRAM refusals** (`vram.rs`) are three distinct sentences with a guard
  refusing any claim about what the card holds, and a lever in each. The module
  itself records that its three call sites are unguarded; I confirmed that, and
  it is honestly stated rather than hidden.

---

## The one thing

**Mark a text layer in the Layers panel** (finding 1), and take finding 2 with it
by reading `active_refusal` where the two Cut controls read `active_is_locked`.
The lock's own mark exists because "an entry that refuses strokes, transforms and
deletion has to say so"; text is the second such entry, it has been shipped
without the mark, and the panel that would explain it is closed on a fresh
install.
