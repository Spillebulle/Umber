# Found and not built

Written at the end of a session that merged eleven branches. Everything here was
confirmed against the source and deliberately left, either because it was out of
the remit that found it or because it wants a decision rather than a patch.
Ordered by what it costs somebody if it stays unfixed.

## Bugs

- **`settings::token_row` applies a half-typed hex on Escape, and writes the
  theme file to disk.** The same defect the Colour panel's hex field had, fixed
  there and not here, and worse because it persists. `egui::Modal::show` draws
  its content and *then* consumes Escape, so on the Escape pass the pane is
  drawn, the caret has already gone, `lost_focus` fires and `write_theme`
  commits. Two comments in that file state the opposite premise. The right fix
  is probably to move the abandon rule into `inset_field`, so there is one field
  and one rule.
- **`Editor::set_color` wipes the picker's saturation for any colour with
  `v == 0`.** Hue is guarded, saturation is not, and saturation is as undefined
  for black as hue is for grey. Reachable through `swap_colors` with a black
  secondary, a black palette swatch, or the eyedropper on a black pixel: the
  marker jumps to the grey axis and raising the value afterwards gives grey.
  One symmetric line, `if next.v > 0.0 { self.hsv.s = next.s; }`.
- **Case-sensitivity in the picture directory.** `read_masks` accepts the
  extension case-insensitively while ten sites in `preset.rs` hard-code
  lowercase `"{name}.png"`. On a case-sensitive filesystem a `Rough.PNG` is read
  into the library and invisible to every writer, deleter and free-stem probe,
  so names collide and a swept picture returns on the next load. Fix the probes
  to agree with the reader, not the reverse: files on disk have whatever case
  they have.
- **An unreferenced picture in `tips/` is swept by any library write.**
  `read_masks` loads every PNG into `self.tips` while `kept_tips` comes only
  from the file, so a hand-copied picture is an orphan to `prune_tips`. The
  rename path now adopts it; nothing else does.
- **Copy and Cut are enabled with a folder selected and return silently**, since
  `active_slot()` is `None`. Pre-existing on the canvas strip; the Edit menu
  widened the surface.
- **`Enter` is in `BINDABLE` yet `direct` claims it before the table is
  consulted**, so a user who binds Enter in Settings gets a row that never
  fires.
- **A selection drag interrupted by a middle press** leaves the draft standing
  with `Interaction` cleared, so the release never reaches `selection_release`.
  Intended for the polygon; a rubber band that follows the pointer with the
  button up for the other two.

## Structural

- **`pointer_released` should end a stroke on `stroke.is_active()`, not on
  `Interaction`.** The best-specified item here. `finish_stroke` already sets
  `Idle` and `Selecting` cannot coexist with a live stroke, so it is small — and
  with it in place no future `Press` variant and no new writer of `Interaction`
  could strand a stroke at all. It supersedes `gesture::supersedes_stroke`
  rather than complementing it, which is why it wants building deliberately.
- **`set_typing` is narrower than "the interface has the keyboard".** Escape and
  Enter are answered in `handle_keys` before `shortcuts::resolve`, and only
  `resolve` consults the suspension. So with a modal up and no field focused,
  one Escape both closes the modal and throws away a standing float. There are
  now two text fields riding on this, not one.
- **A layer cannot be renamed.** `docs/layer-rename.md` is the design: seven
  files, one commit, no core-only half because `panels::edit_icon` is a
  cross-crate compile break.
- **There is no undo for a palette anywhere in Umber**, and the panel now offers
  a rearrange gesture. Needs palettes in `EditKind`, which the "a variant only
  for something the engine can restore" rule governs.

## Tests and tooling

- **Eight `#[ignore]`d preview tests are the only visual check on the panels
  they cover, and CI runs none of them.** The largest unowned item. The honest
  fix is a CI job running them by name on a software adapter against committed
  reference images: about a day, and it needs a decision on where references
  live. Cheap interim: assert every `#[ignore]`d test's name appears in a
  documented list, so one added silently is caught. Two of the seventeen
  (`logo::regenerate_icons`, `installart::regenerate_installer_art`) rewrite
  committed binary art and must never be swept by a bare `--ignored`.
- **`history::set_default_budget` stores into a process-global `AtomicUsize`**
  and `prefs::apply` is called from several tests in one binary, so
  `the_undo_budget_reaches_the_history_and_back` can be raced by a neighbour. A
  `gputest::lock()`-shaped mutex is the answer.
- **Five test scratch paths still key only on a name**, so concurrent worktrees
  share one directory: `palettelib.rs` (three), `stamplib.rs`,
  `docformat/mod.rs`. The other three now carry `std::process::id()`.
- **The GPU suite contends across processes, not only within one.**
  `gpu_pipeline` fails under concurrent worktree runs and passes alone; a
  process id cannot fix it. Gating under a fan-out needs the gates serialised.
- **`packaging/windows/*.bmp` may be stale** against the `cputext` clamp change.
  A later `regenerate_installer_art` diff is that fix, not damage.

## Interface debt

- **Brush size is typable nowhere**, though `widgets::number_row` exists for
  exactly "type it or drag it".
- **Three rails still cannot show the value they hold.** `drag_track` no longer
  destroys an out-of-span value, but the readout is the pinned knob, so an
  airbrush rate of 300/s, a spacing above 0.5 and a stroke span outside
  `1..=500` are invisible on their own rails.
- **`controls::banner` claims 6.4 px past the column it is given**, because its
  trailing `with_layout` reserves item spacing even when the closure draws
  nothing — which is what both real call sites pass.
- **`controls::Glyph` wants unifying with `icons::Icon`**, and
  `settings::inset_field` wants moving to `controls.rs` beside `search_field`.
  Both want the same file; schedule them together.
- **`panels::dashed_rect` should replace `drop_ring`'s body.**
  `rounded_outline` below radius 0.5 short-circuits to exactly the five corners
  `drop_ring` builds by hand. One word, `fn` to `pub(crate)`.
- **The options strip's Deselect link types its own label**, against the rule
  `menu_item` now states. Fixing it changes `shortcuts::labelled`'s contract and
  touches every rail tooltip and tool button.
- **`Palette::columns` is carried by `.gpl`, honoured by the grid and written
  back, and is settable nowhere.**

## Documentation

- **CLAUDE.md's `step_at` section claims "246 of the 258 shipped presets" and
  the figure could not be reproduced** — the nearest reconstruction finds
  fourteen materially affected against the twelve named. A threshold question
  worth an hour, not worth a guess.
- **`4f537c0`'s commit message states a mechanism measurement has refuted** and
  cannot be amended; the retraction lives in the code instead.
