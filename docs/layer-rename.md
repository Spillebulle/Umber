# Renaming a layer

A layer cannot be renamed. Not in a dialog, not in place, not from a menu — the
interface has no path to `Layer::name` at all. A document opened from Krita
shows its author's layer names, and a document made in Umber is "Layer 1",
"Layer 2", "Layer 3" for as long as it exists.

This document is the design for closing that, written before the code because
the change crosses `umber-core` and `umber-app` in one commit and because two
of its decisions are decisions rather than mechanics. It is meant to be
complete enough that whoever picks it up writes code and chooses nothing.

**The short reason it was not simply built:** it needs a new `EditKind`
variant, and `panels::edit_icon` is an exhaustive match over that enum. So
there is no `umber-core`-only half that compiles — the engine and the app
changes land together or `main` does not build — and four of the seven files it
touches were contested when this was written.

---

## 1. What is actually missing

`Layer::name` is a `pub String` on the stack entry. It is written in exactly
two places in the whole codebase:

- `LayerStack::push_imported`, which is an import-only path, and
- `panels.rs`, in the *new layer* command, as `format!("Layer {}", n + 1)`.

`widgets::layer_row` takes `name: &str` and paints it through `elide`. There is
no `TextEdit` anywhere in `panels.rs`. `LayerStack` has no rename method.

Meanwhile the name is load-bearing in five places:

1. **The ORA file.** `docformat` writes it as the `name` attribute of `<layer>`
   and of a folder's `<stack>`, and `docimport::openraster` reads it back.
2. **Three importers.** `.ora`, `.kra` and `.psd` all bring layer names in.
   That is the whole of why the field is ever interesting today: Umber can
   *display* a name it can never *author*.
3. **The autosave's metadata snapshot**, taken when a capture begins precisely
   so that a layer renamed mid-readback cannot produce a file whose names and
   pixels came from different instants — a hazard that, today, no user action
   can actually cause.
4. **The undo history's manifest fingerprint.** `Manifest::layers` is "layer
   names, bottom first, as a fingerprint of the stack the positions index
   into", and a document that comes back without that exact list has its whole
   history dropped.
5. **`docformat::clean_name`**, which exists to strip control characters from a
   layer name so the fingerprint records what the file will come back with.

Point 5 is the sharp version of the absurdity. There is a function in the
document format whose entire job is to sanitise a string the user has never
been able to set, and a test — `a_layer_name_the_xml_cannot_hold_does_not_cost_
the_history` — that constructs a layer called `In\u{7}k` to prove it. Today
that name can only arrive from another application. After this change somebody
can type it.

---

## 2. The model

`LayerStack::rename(index: usize, name: impl Into<String>) -> Option<String>`,
returning the name it displaced, or `None` where the index names nothing.

Three things about that signature, and each is the house rule restated:

- **An out-of-range index answers `None` rather than panicking.** Same rule as
  `Palette::remove` and for the same reason: the index came from a list drawn
  against last frame's stack.
- **It returns the displaced name**, which is what makes the undo entry cheap —
  see §4.
- **A folder renames exactly as a layer does.** A folder is an entry in the same
  `Vec` and its `<stack>` tag carries a name; nothing here needs to know which
  it has.

**What the model does not do is decide what a name may contain.** That is
`docformat::clean_name`'s, it already exists, and duplicating its rule in
`LayerStack` would be the second implementation this codebase refuses
everywhere. See §6 for where the call goes and why it is not here.

---

## 3. Undo: a new `EditBody` arm

`EditKind` gains `Rename`. `EditBody` gains a fourth arm:

```rust
/// A layer renamed. Holds the name the layer had *before* the edit, and
/// the id of the entry it belongs to — never a position, for the reason
/// `StackShape::active` is an id: the selection follows the layer, and so
/// does this.
Rename { layer: u32, name: String },
```

### Why not a names table on `StackShape`

This is the tempting answer and it is wrong, for the precise reason
`ShapeEntry::Kept` carries no mask.

`StackShape` **restores shape, not values**. `Kept { id, depth }` deliberately
holds neither an opacity nor a blend mode, because an undo of a reorder that
reverted an opacity somebody set afterwards would be an undo damaging something
it was never asked about. `StackShape::masks` is the one exception and it says
why in its own docs: a mask is a *slice*, so taking one off frees storage, and
leaving it out is not merely lossy but unsound.

A name is a value, not a slice. Put a `names: Vec<(u32, String)>` on every
shape and undoing a reorder made *before* a rename puts the old name back — the
exact failure the `Kept` rule exists to prevent, reintroduced through the door
that was left open for masks. Record names only on the shapes that rename
records, and `StackShape` now means two different things depending on which
kind produced it, which is what `EditBody`'s own docs refuse when they say **no
entry mixes them**.

So: its own arm. It is also the honest description. A rename is not an edit to
the *shape* of the stack at all — the stack has the same entries in the same
order at the same depths before and after.

### Why not `EditBody::Flip`'s "nothing at all"

A flip is its own inverse. A rename is not: undoing needs the old name and
redoing needs the new one. §4 is how one string covers both.

### Cost

`EditBody::byte_len` gains an arm returning `size_of::<Self>() + name.len()`.
A rename can therefore never be the entry that reaches the budget, and — like a
flip — it is still evicted in timeline order, because what ages out is the
oldest and not the largest.

---

## 4. The swap pattern, spelled out

`LayerStack::restore_shape(&mut self, target: StackShape) -> StackShape` is the
pattern already: apply the recorded state, hand back the state it displaced, and
the caller stores that back into the entry. `app.rs::reverse` is the caller —
a `match` on `EditBody` that returns the body to be pushed onto the other stack.

A rename is the same shape and simpler:

```rust
EditBody::Rename { layer, name } => {
    // `rename_by_id`, not by position: an undo may be reached after a
    // reorder, and the layer the entry names is the layer it named.
    let displaced = self.editor.layers.rename_by_id(layer, name);
    EditBody::Rename { layer, name: displaced }
}
```

One string serves both directions, because the body that goes onto the redo
stack is the one this returned. That is why §2's `rename` returns the displaced
name rather than a `bool`.

**`rename_by_id` and not `rename(index)`** is the load-bearing half. The
timeline is stepped rather than seeked, so an undo of a rename is reached with
every later edit already undone — but a *reorder* recorded before the rename is
not, and positions are not stable across one. `StackShape::active` is an id for
exactly this reason and this follows it.

If `rename_by_id` finds no such id, it must answer without changing anything and
the entry passes through unchanged. That state is unreachable — a layer that
left the stack took its `Gone` entry with it and the timeline steps past that
first — but the same was true of the guards in `SaveHistory::new`, and they are
there.

---

## 5. Verdict on `history::VERSION`: it does not move

Not a shrug — the evidence is in `docformat/history.rs`, and it is decisive.

**No structural entry is written to the file today.** `SaveHistory::new` has
this, in the loop over entries:

```rust
if edit.kind.is_structural() {
    skipped += usize::from(i < history.position());
    continue;
}
```

Add, move, group and add-mask are simply left out and everything around them is
saved whole; delete and remove-mask additionally *cut* the timeline, because a
patch older than one of those names a parked slice. The two groups are told
apart by `EditKind::resurrects_pixels`, which is `DeleteLayer | RemoveMask`.

A rename frees no slice and resurrects no pixels, so every patch either side of
it still names the layer it was captured from and still resolves. It belongs in
the "left out" group. **Left out means the file gains no bytes at all.** An
older build handed such a file reads byte-for-byte what it reads today.

`VERSION`'s own docs state the bar: "a revision an older build would
**misread**". Nothing is written, so there is nothing to misread. Compare the
two bumps that were earned — revision 2, where a build took an entry's first PNG
for the whole rectangle and wrote it over pixels that were never part of the
edit; revision 3, where dropping a flip wrote every older patch back mirrored.
Both are silent damage. This is not even degradation.

The same argument says `docformat::VERSION` (the *document* revision, at 2)
does not move either: no document byte changes, and a name in `stack.xml` is
baseline ORA that every reader already shows.

**What would earn a bump is writing structural entries at all**, which the
`VERSION` docs already name as the future revision 4, and a rename would ride
along with that rather than causing it.

### Consequence to state in the module docs

A rename is not restored from a saved history. Reopen a document and the layer
keeps the name it had when you saved; the row for the rename is not in the list
and the stack does not step back through it. That is exactly what an add, a
move, a group and a mask already do, and it is honest degradation rather than a
lie — the History panel's foot already says the list covers changes to the
layer stack, and `dropped` is deliberately not incremented for a skipped
entry, because an entry missing from the middle is a different thing from a
list that stops short of the beginning.

---

## 6. Two landmines, both silent

These are the reason this document exists rather than a ticket.

### `is_structural` is a `matches!`, and getting it wrong saves a rename as a canvas flip

```rust
pub fn is_structural(self) -> bool {
    matches!(self, Self::AddLayer | Self::DeleteLayer | ...)
}
```

A new variant answers **false** here, silently — no compile error. Now follow
what `SaveHistory::new` does with an entry it did not skip:

```rust
let body = match edit.patches().first() {
    Some(patch) => { ... SaveBody::Pixels { .. } }
    None => SaveBody::Flip,
};
```

`EditBody::Rename` carries no patch, so `patches()` returns the empty slice and
the rename is written into the ORA **as a canvas flip**. On reload, undoing past
it mirrors the picture. That is the sharpest kind of silent document damage
this codebase has a rule about, and it is reachable by adding one enum variant
and forgetting one `matches!`.

So: `is_structural` must include `Rename`, and the guard for it is a test that
saves a history containing a rename and asserts the reopened one holds no flip.
`resurrects_pixels` is also a `matches!` and also answers false — which is the
*correct* answer, by luck rather than by design, so say so in a comment.

For contrast, these three are exhaustive matches and will fail the build, which
is the behaviour to prefer: `EditKind::label`, `EditKind::flip_axis`, and
`panels::edit_icon`. Consider turning `is_structural` and `resurrects_pixels`
into exhaustive matches in the same commit; it costs six arms each and converts
both landmines into compile errors.

### `EditKind::ALL` is hand-written and unguarded

`pub const ALL: [EditKind; 11]`. Adding a variant without extending it compiles
cleanly — the array is still eleven long and still valid. `kind_from_id` is
`EditKind::ALL.into_iter().find(...)`, so a manifest naming `"Rename"` would
answer `None` and **the whole history is dropped on load**. Less severe than
the flip, still silent. Extend `ALL` to twelve, and note that no test can
currently catch the omission.

---

## 7. `clean_name`, and where it is called

A name typed by a user can contain anything. `docformat::clean_name` drops
control characters, `attribute` escapes for XML on top of that, and the history
manifest records the *cleaned* names because it has to fingerprint what the file
will come back with.

**Do not clean in `LayerStack::rename`.** Two reasons. First, `clean_name` is
`pub(crate)` to `docformat` and is the file format's rule, not the model's — the
model has no opinion about XML. Second, cleaning on the way in would make the
name the user sees differ from the name they typed, silently, and the existing
arrangement already handles it: the file writer cleans, the fingerprint uses the
same cleaned names, and the two cannot disagree.

**Do bound the length**, and this is new. There is no bound on a layer name
today; `themelib::MAX_NAME` is 64 and its docs argue for it because an unbounded
name has to be laid out and cut to fit, per card, per frame. A layer name is
elided by `widgets::elide`, which binary-searches and so costs a handful of
`layout_no_wrap` calls rather than one per character — but it also allocates a
`Vec<usize>` of char indices and a `format!` per search step, per row, per
frame. An import can already bring an unbounded name; a rename field should not
add a second way in. Put the bound on the *field*, not on the model, so an
imported name is displayed as it arrived.

---

## 8. The interface: in place, not a dialog

Rename in place on the row, on double-click, with Enter to commit and Escape to
abandon.

**Why not a dialog.** `palettelib` and `settings`'s Themes pane both put rename
in a modal, and that is right for them: a palette and a theme are managed in a
library browser where there is room to say what each command does. A layer is
renamed in the list it lives in, while looking at the picture, often several in
a row. A modal per layer is the interaction every other application declines.

**Why double-click and not a button on the row.** The flags row and the eye are
already on that row and `metrics::PANEL` is 264 px. A rename button would be a
fourth target competing for the width the name itself needs, and CLAUDE.md's
"All" box argument applies: a control says which control it is by where it
sits, and there is nowhere left to sit.

### What the implementer has to get right

- **The drag must not fight the rename.** `layerdrag` presses land strictly
  inside a row. A double-click is two presses, so the first will start a drag
  aim. Settle this by having the rename take the row's id out of drag
  consideration for as long as the field is up, and pin it with a test on
  `layerdrag` — that module is a model with no drawing in it and this is
  testable without a window.
- **`shortcuts::set_typing` is already handled** for the whole interface by
  `ui::draw` calling `ctx.text_edit_focused()`. A real `TextEdit` needs no
  `set_capturing`; `set_capturing` belongs to the chord recorder alone. Do not
  add one.
- **A revealed control must be allocated unconditionally** and tested with
  `contains_pointer`, never `hovered` — egui stops its hover search at the
  topmost interactive widget, and a control that only exists while its row
  reports hovered oscillates once a frame. This was a real bug on the Shortcuts
  page's `+`.
- **An empty name.** Refuse it and keep the old one rather than storing `""`.
  A row with no name is unclickable in the list and indistinguishable from a
  drawing bug. `palette::one_line` takes the other route (fall back to a
  placeholder) and that is right for a palette, whose name is shown in a
  dropdown; a layer's is the only thing identifying the row.
- **The rename is one undo entry per commit, not per keystroke.** Record on
  Enter or on focus loss, and only when the name actually changed —
  `Transform::is_identity` is the precedent for not recording a no-op.
- **`layers_panel_preview` is `#[ignore]`d and is the only visual check on this
  list.** Run it by name and look at it. Never `-- --ignored` without a test
  name: two of the seventeen ignored tests in this workspace overwrite
  committed binary art.

---

## 9. File-by-file change list

Seven files, one commit. Four were contested when this was written.

| File | Change |
|---|---|
| `crates/umber-core/src/history.rs` | `EditKind::Rename`; extend `ALL` to 12; `label`; `is_structural`; comment on `resurrects_pixels`; `EditBody::Rename` arm; `byte_len` arm. Consider making both `matches!` exhaustive. |
| `crates/umber-core/src/layer.rs` | `LayerStack::rename` and `rename_by_id`, returning the displaced name. |
| `crates/umber-core/src/docformat/history.rs` | Nothing, if `is_structural` is right. Add the guard test that proves a saved rename is not read back as a flip. |
| `crates/umber-app/src/app.rs` | The `EditBody::Rename` arm in `reverse`; the command that records the edit. |
| `crates/umber-app/src/panels.rs` | The double-click, the field, and the `edit_icon` arm. |
| `crates/umber-app/src/widgets.rs` | `layer_row` draws a field instead of a label while renaming. |
| `crates/umber-app/src/editor.rs` | Which row is being renamed. **Per-document**, so below the `--- documents ---` line: a tab switch must abandon it, exactly as it abandons a `SelectionDraft`. |

`crates/umber-app/src/icons.rs` needs an arm only if `edit_icon` wants a mark of
its own; reusing an existing one is probably right, since a rename has no
control of its own to echo.

### Tests to write

- The model: a rename is a permutation of nothing — same entries, same order,
  same depths, same slots, only the string moved.
- `rename_by_id` after a reorder renames the layer, not the position.
- Undo and redo of a rename restore both names, and a rename recorded before a
  reorder is not reverted by undoing the reorder. That last one is the guard
  for §3's whole argument and it is the test that would have failed under the
  names-table design.
- A saved history containing a rename reopens with no flip in it, and the
  picture is not mirrored by stepping back through it. §6's landmine.
- A rename of a folder is a rename.
- An empty name is refused and the old one stands.

---

## 10. A correction that must not be confused with this feature

CLAUDE.md's Undo section currently reads:

> It is Paint, Erase, Transform, the two canvas flips, and the six structural
> edits — an entry exists where a patch was captured, where the edit is its own
> inverse (the flips), or where the *shape* of the stack can be put back (a
> delete, an add, a reorder, **a rename**, a mask added or removed).

There is no `Rename` variant. The enum's sixth structural arm is **`Group`**.
The prose describes a feature that was never built, and two agents found the
error independently in the same session.

That is a documentation fix and it is **not** this feature. Correct the prose to
say `Group` first, on its own, so that the record is true of the build that
exists. `EditKind::Rename` would then land in the slot the prose used to claim
wrongly — and if the two changes are made together, or the prose is left alone
on the grounds that this feature will make it true, nobody afterwards can tell
whether the file was describing an intention or a mistake.
