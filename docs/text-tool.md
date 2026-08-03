# The text tool

Umber has no text tool. The design draws sixteen tools and six are built;
`panels::tools_body` says so at the call site and the README says so in *What is
not there yet*. This document is the design for one of the missing ten, written
before any of it exists, so that whoever builds it is not starting from the
beginning and so that nobody starts it thinking it is small.

**It is not small.** Three of the seven sections below describe work that is
genuinely hard — fonts and their licences, the on-canvas caret, and what a piece
of text *is* once the file is closed — and each of them has a cheap-looking
answer that is wrong. The two things that make it look small are both real, and
they are worth saying first because they decide the order everything else is
built in:

- **The pixels have somewhere to go already.** Rasterised text placed on the
  canvas and moved, scaled and turned before it is put down *is* a paste. The
  `Float` machinery — one function called twice, so the preview and the commit
  cannot disagree — is the right shape for it almost exactly, and §4 is the
  audit of "almost".
- **The font machinery is nearly all in the tree.** `skrifa` is a direct
  dependency of `umber-app` and `harfrust` — a complete port of HarfBuzz — comes
  in under `epaint`. `cputext.rs` already turns a string into coverage with no
  GPU at all. What is missing is layout, not rasterisation.

What is *not* nearly there is the request's second half: "preferably with every
open source or licence-allowing font we can find". Taken literally that is a
1.1-gigabyte download attached to a painting application, and §2 is the number
that decides it.

---

## 1. What a text tool has to do

Five things, in rising order of cost. The line to draw is between the third and
the fourth.

1. **Turn a string, a font and a size into coverage.** Shaping, line breaking,
   rasterisation. Costs a crate and a module; §5.
2. **Put that coverage on the canvas in the artist's colour**, undoably, in the
   active layer, respecting the lock and the selection. Costs almost nothing —
   it is a paste; §4.
3. **Let it be moved, scaled and turned before it is put down.** Also almost
   nothing, and the same gesture the transform tool already has; §4.
4. **Let it be typed *on the canvas*, with a caret, an insertion point, a
   selection and a keyboard.** This is where the cost is. §6 and §7.
5. **Let it still be text tomorrow** — a text layer that reopens editable. §3.

(1) to (3) together are a useful feature: it is what "add a caption to this
picture" needs, and it is what every image editor without a text engine
eventually grows. (4) is what makes it feel like a text tool. (5) is what makes
it a *layer* rather than a stamp, and it is the one that reaches into the file
format.

---

## 2. Fonts, and the megabyte that decides it

The request is "preferably with every open source or licence-allowing font we
can find". The good version of that request is not the literal one, and the
reason is a number.

### What "every font" costs

| Source | Families | Size | Notes |
|---|---|---|---|
| Google Fonts, whole catalogue | ~1,934 (546 variable), June 2026 | **~1.1 GB** as `google/fonts/archive/main.zip`; the repository itself is **3.2 GB** on GitHub's own `size` field | OFL 1.1, a few Apache-2.0, one or two UFL |
| Noto alone | ~200 fonts, 1,000 languages, 162 scripts | hundreds of MB; the CJK faces are ~16 MB *each* | OFL 1.1 |
| Archivo, the one font Umber ships today | 1 (variable, `wdth`+`wght`) | **643 KB** (`658,596` bytes, measured) | OFL 1.1 |

Those numbers are consistent with each other, which is a useful check: 1.1 GB
over 1,934 families is about 570 KB a family, and Archivo — a two-axis variable
font with Latin, Greek and Cyrillic — is 643 KB. There is no compression trick
hiding in there. A font *is* about half a megabyte.

Now the context. Umber's **entire** `assets/` tree is 59 MB, and 58 MB of that is
brush packs that are downloaded rather than vendored and never reach a release.
The largest single thing in it is David Revoy's Krita bundle at 8.5 MB. Umber
also **updates itself in place** and fetches a whole release archive to do it, so
anything bundled is paid for by every user on every platform on every update.

Bundling the Google Fonts catalogue would therefore multiply the download by
something like twenty, to ship 1,900 typefaces to a painter who will use four.
That is not a trade, it is a mistake with a licence footnote.

### "Free" is three different things

The other half of the problem is that "licence-allowing" is doing an enormous
amount of work in that sentence.

| Class | Example | May Umber *bundle* it? | May Umber *use* it? |
|---|---|---|---|
| OFL 1.1 | most of Google Fonts, Noto, Archivo | **Yes**, with the licence file alongside | Yes |
| Apache-2.0 | Roboto, Open Sans (older releases) | **Yes**, with `LICENSE`/`NOTICE` | Yes |
| Ubuntu Font Licence | Ubuntu, Ubuntu Mono | **Yes** | Yes |
| Free *for personal use* | most of what a font aggregator calls free | **No** — no redistribution right at all | Yes, if the user installed it |
| System fonts | Segoe UI, Calibri, SF Pro, Helvetica | **No** — licensed to the machine, not to a redistributor | Yes, if the user's machine has it |
| Commercial, bought by the user | anything from a foundry | **No** | Yes — they paid for it |

Two of those rows are the whole argument. **Every font on the machine is already
licensed to the person using it.** Enumerating them costs nothing, ships
nothing, redistributes nothing, and gives the artist the fonts they actually
want — including the ones they paid for and the ones the operating system came
with, which no bundle could ever legally contain. That is what every other paint
application does, and it is what Umber should do first.

Two constraints ride along with OFL 1.1, and both matter later rather than now:

- **The licence and copyright notice must travel with the font.** `assets/fonts/`
  already does this correctly for Archivo — `OFL.txt` beside the `.ttf`, with a
  `README.md` recording where it came from and when. That pattern scales.
- **Reserved Font Names.** A *modified* copy may not keep the original's primary
  name. Umber does not modify fonts, so this only bites if a bundling script ever
  subsets or re-instances one — which is the obvious way to shrink a bundle and is
  therefore worth writing down before somebody tries it.

### What Umber should actually do

Three sources, in this order.

**A. Every font installed on the machine.** `fontdb` walks the platform's font
directories, memory-maps what it finds and answers CSS-ish family/style queries.
Zero megabytes shipped, zero licence exposure, and on a typical machine it is
several hundred faces. This is the feature; the other two are conveniences
around it.

**B. A small curated bundle, held to the brush library's rule.** The point of a
bundle is not choice, it is that a fresh Linux install with four fonts on it can
still set a caption in something other than DejaVu Sans. Ten to twenty families
covering the obvious needs — a grotesque, a humanist sans, a transitional serif,
a slab, a mono, a script, a couple of display faces, and Noto Sans as the Latin
fallback — is **3 to 13 MB** at the measured half-megabyte a family, and the
target should be under 10, which is one Krita brush bundle.

It should be built by the mechanism that already exists for brushes and not a
second one: `tools/fetch-fonts.ps1` and its `.sh` twin, downloading rather than
vendoring, refusing anything whose licence is not **verified inside the
download**, and generating `assets/fonts/LICENSES.md` the way
`assets/brushes/LICENSES.md` is generated. `docs/brush-sources.md`'s rule
transfers verbatim:

> If a source's licence cannot be verified **from its own files**, it does not
> ship. A licence stated on a web page next to a download is not the same thing
> as a licence inside the download.

Google Fonts passes that test trivially — every family directory carries its own
`OFL.txt` — which is exactly why it is the right *source* and the wrong *bundle*.

**C. A folder the user points Umber at.** One preference, one directory, scanned
into the same `fontdb` at startup. This is how somebody with a foundry licence or
a work font library uses their own faces, and it is four lines of code. Umber
must **not** copy anything out of that folder: the moment it does, it is
redistributing, and it is doing it inside somebody's document folder.

The two rejected options, named:

- **Bundling the catalogue.** 1.1 GB, above. Also: an artist has to *find* a font
  in a list of 1,900, which is a search problem nobody asked for, and the list
  would be wrong the week it shipped.
- **Downloading fonts on demand from a font service.** Zero install cost and a
  genuinely attractive feature, and it fails on Umber's own terms. It is a second
  network path with a second set of promises, and the update code is emphatic
  that **Umber does not sign its releases** and that nothing may be described as
  verified when only its length was checked. A font fetcher would inherit that
  whole argument and add a mirror somebody has to keep alive. It is a fine thing
  to build *after* signing exists; it is not the first font feature.

---

## 3. What a piece of text *is* in the document

Two answers, and the second is a superset of the first rather than an
alternative to it.

### The one that ships: text is pixels

Text is rasterised at commit and written into the active layer, exactly as a
paste is. There is no text object, no new layer kind, no new `EditKind` — a
commit records `EditKind::Transform`, which is what a paste already records, on
the rule that *two rows that undo identically must not have two names*. The
format does not change and `umber-version` does not move, because nothing new is
in the file.

What it costs: the text is not re-editable. Fixing a typo means undoing and
typing it again. That is what Krita's "paint text" does and it is what MyPaint
offers instead of text at all, and it is a completely defensible place to stop.

### The one worth designing for: a re-editable text layer

The tempting version is an `umber-text` attribute on the `<layer>` pointing at an
entry under `umber/text/`, beside the layer's ordinary rasterised PNG — the same
shape `umber-mask` and `umber-history` already use, and for the same reason
(`stack.xml` should not carry a paragraph of somebody's prose with XML
metacharacters in it).

And the tempting *argument* is the folder argument: an older Umber, or GIMP, or
Krita, ignores an attribute it has never heard of, decodes the PNG, and shows the
identical picture. Plainer, not wrong. That is the line `umber-version` is drawn
on, so it does not move.

**That argument is nearly right, and it has two holes. Both have to be plugged
before it holds.**

**Hole one: the pixels and the text can come apart.** An older Umber opens the
file, the artist paints over the text layer, saves. The PNG now says one thing
and `umber-text` says another. This build reopens it, decides the layer is text,
re-renders — and the artist's paint is gone. That is not "plainer", it is a
document quietly damaged, which is the one outcome this codebase loses a history
over.

The fix is the fix the history manifest already uses, and it is the same
sentence: **anything that does not line up exactly is dropped, whole.** The text
entry carries a fingerprint of the pixels it rendered — the rectangle and a hash
of its bytes — and on load, a mismatch discards the text object and keeps the
picture. The layer becomes an ordinary painted layer, which is what it now is.
`a_text_layer_painted_on_by_an_older_build_opens_as_paint` is the guard, and it
is exactly `saving_and_reopening_does_not_move_a_pixel`'s neighbour in intent.

**Hole two: the font may not be on the next machine.** Re-rendering with a
substituted face changes the picture, silently, which is worse than not
re-rendering at all. So: the text object records the family, the style and the
PostScript name; if the exact face is not found on open, the text is **frozen** —
the saved pixels stand, the layer draws them, and trying to edit raises a notice
naming the font that is missing. Never a silent fallback. Same standard as
`ImportWarning`: an import that loses something must say so.

**Embedding the font in the `.ora` is the obvious repair and is refused.** It is
font redistribution performed by the artist, without their knowledge, in a file
they may email — and for a machine-licensed system font that is a licence breach
they did not commit. It also puts half a megabyte to sixteen megabytes into every
document. The whole of §2 exists to keep Umber out of the business of moving font
files around, and this would put it right back in.

**Hole three, which is not about the format at all: whose layer is it?** A text
layer that re-renders cannot also hold brush strokes, so it is a layer *kind* —
with a paint gate, a mark on its row, a rule for what happens when somebody
paints on it anyway, an answer for masks, for clipping, for a folder containing
one, and for `LayerStack::MAX`. That is a model change of the same size as
folders were.

**Conclusion.** The re-editable text layer is coherent, it does *not* need
`umber-version` to move, and it is a second feature rather than a variation on
the first. It goes last in §9 for that reason, and the fingerprint rule is what
makes the version argument honest rather than merely convenient.

---

## 4. Reusing `Float`, and what it does not have

This is the strongest idea available, so it is worth testing rather than
assuming.

A `Float` is: pixels held in a canvas-sized texture at identity, a `base`
holding the layer as it will sit underneath, a `Transform`, and `render_float` —
one function that restores the damaged rectangle out of `base` and draws the
transformed copy over it, into a spare layer slice for the preview and into the
layer's own slice for the commit. The preview and the commit are therefore *the
same two commands run twice*, and the layer is untouched until the commit, which
is what makes Escape free.

Placed text is that shape exactly, and specifically it is the **paste** shape
rather than the lift shape:

| A paste | Placed text |
|---|---|
| `begin_float(rect, Some(&pixels))` | the same call, with rasterised glyph coverage |
| `lifted: false`, so no hole to restore | the same |
| no mask pass — the clip was applied when it was copied | the same, and the selection clip belongs to a later stage |
| commits as `EditKind::Transform`, patch = destination only | the same |
| gated once on the lock and on "the active entry is a folder" | the same gate, free |
| counts as busy for the autosave | the same, free |
| refused when every layer slice is in use, with a notice | the same, with different wording |

So the answer to "what does it need that `Float` does not have" is **three
things**, and only the third is difficult.

### (a) `Transform::reseat` — the box has to be able to grow

`Transform`'s `source` rectangle is fixed at construction, and its centre is the
pivot for the scale and the rotation. Typing another word makes the text wider;
the source rectangle has to grow with it, and a naive
`Transform::identity(new_rect)` throws away every drag the artist has made.

Compensating for it is one line, and it is exact. With `m = R·S`:

```
apply(p) = m·(p − pivot) + pivot + offset
```

so moving the pivot from `pivot` to `q` and setting

```
offset' = offset + (m − I)·(q − pivot)
```

leaves `apply` **identical for every point**. Not approximately — the pivot term
cancels completely, so every glyph already on screen stays exactly where it is
and the new ones land in identity space and are carried by the same matrix. At
identity `m = I`, so the correction is zero and `is_identity()` still reads true,
which is what stops a click that typed nothing from recording an edit.

`a_reseated_transform_maps_every_point_exactly_where_it_did` is the guard, it
needs no GPU, and it belongs in `transform.rs` beside
`a_transform_and_its_inverse_are_exact_opposites`.

### (b) `CanvasRenderer::retype_float` — `begin_float` is far too expensive per keystroke

`begin_float` allocates two canvas-sized textures, copies the whole layer into
`base`, copies `base` into the preview slice and submits twice. That is right
**once per gesture** and ruinous per keystroke:

| Canvas | One `begin_float` |
|---|---|
| 2048² | 32 MB allocated, 32 MB copied |
| 10000² | **800 MB allocated, 800 MB copied** |

Per character typed. It has to be a different call. The good news is that it is a
*small* different call, and it is small precisely **because text is a paste**:
`base` is the layer copied whole and, with no lift and no mask pass, it does not
depend on the source rectangle at all. So re-typing is:

1. clear the union of the old and the new source rectangles in the floating
   texture, and
2. `write_texture` the new coverage into the new rectangle.

Both are rectangle-sized. `base`, the uniforms, the bind group and the preview
slice are all untouched, and `render_float` is not touched at all — so the
guarantee that the preview and the commit cannot disagree survives without being
restated. A 2000×400 block of text is 3.2 MB per upload, which is fine for a
keystroke and is worth thinking about for a per-frame drag; see (c).

### (c) The hard one: scaling text must not be a bilinear resample

`Float` scales by sampling. That is right for photographic pixels and it is the
one thing text cannot tolerate: a caption dragged to twice the size comes out
soft, and dragged to a third of it comes out mush. Every application that has a
text tool re-renders instead.

Three ways out, and the third is the recommendation.

- **Accept the blur during the drag and re-rasterise on release.** Cheap, and it
  is a lie: the preview is not what commits, which is the exact property `Float`
  is built to guarantee. It also snaps visibly at the moment the artist lets go,
  which is the stroke-jump bug wearing a different hat.
- **Drive the *point size* from the scale handle and hold the matrix at 1.**
  Correct-looking and wrong in a subtle way: scaling text is not the same as
  setting it larger, because hinting, optical sizing and a variable font's own
  `opsz` axis all change what the shapes are. It also has no answer for rotation,
  which genuinely does need the matrix.
- **Rasterise *through* the transform.** `skrifa`'s `DrawSettings` takes an
  affine, so the outlines can be drawn already scaled and rotated and the float's
  own matrix held at identity. Every frame of a drag is then a fresh, sharp
  rasterisation and an upload of the destination rectangle.

The third one is right and it has a bound that has to be stated rather than
discovered: the cost is the *destination* area, so text dragged to fill a 10000²
canvas is 400 MB of upload a frame. The rule is therefore **re-rasterise up to a
budget and fall back to the sampler above it** — the same shape as the autosave's
four-megabytes-a-frame, and it should be measured by an example in
`crates/umber-core/examples/` before a number is written down, exactly as
`measure-history.rs` and `measure-pressure.rs` are.

### One more thing the invariant needs

`app.rs` checks once per frame that a float exists only with **the transform tool
in hand**, deliberately in one place rather than at the five controls that could
break it. `pick_tool` and `paste` state the same thing again. A text float would
be up with the *text* tool in hand, so all three learn a second tool — which is
the sixth call site the comment there is worried about. The fix is to stop
naming the tool: `Floating` carries the tool that owns it, or `Tool` grows an
`owns_float()`, and the check compares against that. One statement, two tools,
and the next tool that floats something needs no fourth edit.

---

## 5. Layout, shaping and where the crate boundary falls

### What is already in the tree

This is better than it looks. `Cargo.lock` already contains:

| Crate | Version | How it gets there |
|---|---|---|
| `skrifa` | 0.42.1 | **direct dependency of `umber-app`** (`cputext.rs`), and again under `epaint` |
| `read-fonts`, `font-types` | 0.39.2 / — | under `skrifa` |
| `harfrust` | 0.7.0 | under `epaint` — "a complete HarfBuzz shaping algorithm port to Rust" |
| `ab_glyph_rasterizer` | 0.1.10 | direct dependency of `umber-app` |
| `ttf-parser`, `memmap2`, `slotmap`, `tinyvec` | — | scattered under `ab_glyph`, `winit`, `rfd` |

And `cputext.rs` already turns a string into coverage with no GPU, no window and
no egui, using `skrifa` for variable-instanced outlines and
`ab_glyph_rasterizer` for coverage. It exists because the splash paints before
wgpu does.

**What `cputext.rs` does is not enough for a text tool, and the reason is worth
being precise about.** It maps characters to glyphs through the `charmap` and
sums advance widths. That is fine for four ASCII runs of one known font and it
is *wrong* — not plainer, wrong — for real text: no kerning, no ligatures, no
mark positioning, no bidirectional reordering, and unshaped Arabic is
unreadable rather than merely plain. A text tool that renders Arabic as
disconnected isolated forms is the control that lies.

### `cosmic-text`, and its actual cost

`cosmic-text` 0.19 is MIT-or-Apache-2.0, pure data, **no GPU dependency
whatsoever** — `glyphon` is a separate crate and is what people mean when they
say cosmic-text needs wgpu. It gives shaping, bidi, line breaking, wrapping,
font fallback and a cursor/selection model, and it has recently moved *onto* the
same fontations stack Umber is already on: 0.19 uses `skrifa` and `harfrust`, not
`ttf-parser` and `rustybuzz`.

So it can live in `umber-core` beside `zip`, `png`, `image`, `psd` and `ron`
without touching the crate boundary at all. What crosses into `umber-render` is
what already crosses: **a rectangle of RGBA8, sRGB-encoded with alpha
premultiplied in linear space**, which is `ImportedLayer::pixels`' contract and
`Clip::place`'s output. `umber-render` never learns there is text, exactly as it
never learns there is a paste.

The honest costs, checked against this repository's own lockfile rather than
recalled:

- **Six or seven new crates**: `fontdb`, `rangemap`, `unicode-bidi`,
  `unicode-linebreak`, `unicode-script`, `smol_str` 0.3 (a second copy beside the
  0.2.2 already there), and on Linux `fontconfig-parser`. All pure Rust, all
  small, none with a C toolchain — which is the standard `ureq`'s rustls choice
  and the whole `docimport` dependency set are held to.
- **Two duplicated crates, today.** `cosmic-text` 0.19 requires `skrifa ^0.40`
  and `harfrust ^0.5`; `epaint` 0.35 requires `skrifa ^0.42.1` and
  `harfrust ^0.7.0`. Those are 0.x caret ranges, so Cargo cannot unify them and
  **both are built twice**. That is a compile-time and binary-size cost for no
  behaviour, and it is temporary by nature — both crates track the same
  fontations releases — but it is real today and should be re-checked rather than
  assumed away when this is picked up.
- **`swash` is a default feature and should be switched off.** `default-features
  = false, features = ["std", "fontconfig"]` drops it, which is a third font
  parser and a second rasteriser Umber does not need: the outlines come from
  `skrifa` and the coverage from `ab_glyph_rasterizer`, **which is what
  `cputext.rs` already does**. One rasteriser, two callers, and `cputext::Font`
  is the thing to generalise rather than the thing to duplicate.

### The alternative, named

Assemble it by hand: `harfrust` (already present, at the version `epaint` wants,
so no duplicate) plus `skrifa` plus `unicode-bidi` plus `unicode-linebreak`.
Four new small crates instead of seven, no duplicated builds, and roughly eight
hundred lines of line-breaking, bidi reordering and font-fallback glue that
`cosmic-text` has already debugged against real scripts. Font *fallback* — asking
"which of the machine's four hundred faces has this codepoint" and splitting a
run across the answers — is the piece that looks trivial and is not.

**Recommendation: `cosmic-text`**, with the duplicate-version cost written down
and re-measured before the branch merges. Hand-assembly is the answer only if
that duplication turns out to be worse than it looks.

### `glyphon` is not wanted

`glyphon` is the wgpu renderer for `cosmic-text`, MIT-or-Apache-or-Zlib, and it
is the wrong shape for this in three ways. It keeps its own glyph atlas and its
own pipeline, so it would be a **second path to the canvas** that
`composite.wgsl` and `commit.wgsl` know nothing about — the divergence the
selection clip, the export path and `render_float` all exist to prevent. It
renders into a surface, where these pixels have to end up in an
`Rgba8UnormSrgb` layer slice. And the machinery for getting a rectangle of
pixels into a layer slice already exists and is called by paste. An upload of a
few hundred kilobytes is not the bottleneck; a second blend implementation is.

---

## 6. The on-canvas interaction

### Click to place, or drag a box

Both, and they mean different things — every application draws this distinction
and it is not cosmetic.

- **Click to place** (point text): the caret starts where you clicked, the line
  grows as you type, and a newline is the only thing that wraps. The source
  rectangle grows on every keystroke, which is what `Transform::reseat` in §4 is
  for.
- **Drag a box** (area text): the rectangle is the frame you drew, typing wraps
  inside it, and the box is *not* reseated by typing — it is reseated by dragging
  a handle, which reflows the text rather than scaling it.

Area text is the one that needs the scale handle to mean "reflow" rather than
"resize", so it is the later of the two. Point text is the smaller feature and
goes first.

### The caret, and the thing that must not happen

The selection's marquee is a **dashed line, not marching ants**, because
animating it means requesting a frame for ever and that is the cost
`render`'s `repaint_at` exists to avoid.

A blinking caret is not the same case, and the difference is the rate.
`repaint_at` is driven from egui's own `repaint_delay`, so
`ctx.request_repaint_after(500ms)` costs **two frames a second while a caret is
on screen and nothing at all when it is not**. That is affordable where sixty is
not. It should be said out loud at the call site, because "the selection outline
does not animate" reads as a blanket rule and is not one.

### Handles, and the rule they answer to

`ui::transform_box` draws the eight handles from `Transform::grab`'s own
`handle_at` answers, so a handle cannot be painted where the hit test disagrees
with it. A text box inherits that for free by being a `Float`, and it must not
grow a second set of positions.

The rotation *mark* is instructive rather than interactive, for the reason
`Handle::Rotate` gives — outside the box is already the whole target. Text
changes nothing there.

### Controls over the canvas

Anything drawn over the canvas that can be pressed **must** record its rectangle
and go through `canvas_overlay_owns_pointer`, or a press on it is also a press on
the canvas — with a brush in hand that is a dab under the button that was
clicked, and the pen reaches the same test through `pointer_over_canvas`, which
is what stops it being a control that works with a mouse and paints with a pen.

**A text float should need no canvas buttons at all**, and that is the
recommendation: the commit and the cancel are keys and a click outside, exactly
as a floating transform's are, and the tool options strip says so in a hint line
the way the transform tool's already does. If buttons are added anyway they
follow the **flip pair's** rule — not offered when they do not fit — and
deliberately not `overlay::place_strip`'s, because that module is the
*selection strip's* rule and says so: a selection cannot be dragged back into
reach and a text box can.

---

## 7. Keyboard capture, which is a real trap

Key dispatch happens at the **winit** level, before egui sees a keystroke.
`ui::draw` calls `shortcuts::set_typing(ctx.text_edit_focused())` once for the
whole interface, which covers every real `TextEdit`. **A caret on the canvas is
not a `TextEdit`**, so without a change, typing "brush" into it selects the
brush tool, then the eraser, and a couple more on the way to the end of the word
— which is the precise bug `set_typing` was written for.

Four changes, and they are all findable in advance.

**One: `set_typing` gets a second term, in the one place it is already called.**

```rust
shortcuts::set_typing(root.ctx().text_edit_focused() || ed.text_caret.is_some());
```

Not a `set_capturing` call from the text tool: that lever belongs to the chord
recorder in Settings alone, and a second writer per frame is how the two end up
disagreeing. One writer, one line, one place — which is the rule `set_typing`'s
own documentation states.

**Two: the characters have to come from `event.text`, not from `physical_key`.**
`app.rs` reads `logical_key` today only to disambiguate punctuation for
shortcuts. Text entry wants winit's `KeyEvent::text`, which is what applies
Shift, AltGr and dead keys — a Norwegian keyboard's `ø` is not reachable any
other way.

**Three: `Enter` and `Escape` are already claimed, and one of them has to
change.** `handle_keys` intercepts `Escape` to cancel a float and `Enter` to
commit one, before the binding table is consulted. With a caret live, `Enter`
must be a newline. So:

- `Escape` cancels — unchanged, and it is what Escape means everywhere.
- `Ctrl+Enter` commits, which is what Photoshop spells it as.
- A click outside the box commits, which the float already does via
  `PUT_DOWN_SLOP`.
- Both `Enter` arms in `handle_keys` gate on there being no live caret, or the
  first newline somebody types puts their text down.

**Four: IME, and this is the one that is genuinely awkward.** `egui-winit`'s
`on_window_event` is called with every event, so an egui `TextEdit` gets
`WindowEvent::Ime` — preedit, candidate windows, the lot — for nothing. A canvas
caret gets none of it: Umber never calls `set_ime_allowed`, never handles
`WindowEvent::Ime`, and has no preedit to draw. **Without that work, a canvas
text tool cannot type Chinese, Japanese or Korean at all.**

That is not a footnote, it is a reason to sequence the feature the way §9 does:
the first stage types into a real `TextEdit`, where IME already works, and
on-canvas typing is the stage that has to pay for it.

**Five, while we are here: there is no system clipboard.** `egui-winit` is built
`default-features = false`, so its `clipboard` feature is not compiled in and
`arboard` is not in the lockfile — the same reason the crash box has no "Copy
details" button and `about::link_row` paints its own hyperlink. That means
**Ctrl+V cannot bring text in from another application**, in a `TextEdit` or on
the canvas. For a text tool that is a much sharper limitation than it is for a
crash dialog, and it must be said in the interface rather than discovered. It is
also the same blocked door `docs/architecture.md`'s roadmap already names for
pixels.

---

## 8. The interface

**I could not read the design.** The `DesignSync` tool named in `CLAUDE.md` is
not available in this session, so everything below is derived from the source and
from the design rules that are written down, and the "Umber app" screen should be
checked before any of it is drawn. What the source says is that `panels::
tools_body` draws six tools where the design shows sixteen, and that the rest are
*not drawn at all* rather than shown as buttons that do nothing. Text is
presumably one of the ten, but which mark it uses and what its options strip
carries are questions this document cannot answer.

What follows from the rules regardless:

- **`icons::Icon` gains a `Text` variant, added at the end of the enum.** That
  file says why in a comment: the enum is shared and renumbering it is a merge
  that compiles and draws the wrong marks. And nothing may put a Unicode glyph in
  the interface — Archivo carries none of them and they render as blank boxes.
- **The tool options strip follows `options_strip`'s existing shape**: the mark,
  the tool's name, a divider, then the controls, then a hint line. For text the
  controls are the font family, the style, the size, and the alignment — and each
  needs an entry in `strip_budget`, because the strip is a single unwrapped row
  that does not reflow and the budgets decide which groups appear at all. They
  should drop in reverse order of how constantly they are reached for:
  alignment first, then style, then size, with the family the last to go.
- **The font picker is `widgets::dropdown`**, because there is one dropdown and
  this is what it is for. `DropdownWidth::Exact` on the strip; the menu is
  `egui::Popup::menu` at the call site, never a flag on `Editor::ui`.
- **The size is `widgets::number_row`**, because it is a figure somebody types
  as often as they drag it, and that is what that control exists for.
- **Colour is not on the strip.** It comes from the Colour panel, exactly as the
  brush's does, and a second colour control would be a second answer to one
  question.
- **The font list is the brush list's problem again.** Several hundred faces,
  each wanting to be drawn in its own face, in a list that scrolls. The rules
  that apply are already written: nothing on the drawing path may allocate per
  frame; search folds case in place; rows scrolled out of view skip painting;
  and — the one that bites — **a texture cache keyed on an address must have the
  shape it drew in in the key too**, or the same face rendered at two sizes in
  two lists on screen together frees a live texture and panics wgpu. That was a
  real bug in the brush library and a font list is the same shape of thing.
- **A missing font, a frozen text layer and a dropped text object each need a
  sentence**, and the rule is the import warnings' rule: an operation that loses
  something must say so, in a finished sentence written for the user.

---

## 9. A staged plan, and which piece is the risky one

Each stage is buildable, testable and mergeable on its own, and the first is
useful by itself.

### Stage 0 — the font pipeline (small)

`tools/fetch-fonts.ps1` and its `.sh` twin, on `fetch-brushes`' pattern:
download, verify the licence **inside** the download, generate
`assets/fonts/LICENSES.md`, git-ignore the fonts themselves. Nothing user-visible.
Half a day, and it is what stops the curated bundle being assembled by hand and
going stale.

### Stage 1 — type it in a panel, place it as a float ★ *the useful first piece*

A `umber-core::text` module wrapping `cosmic-text`: a string, a font, a size, an
alignment and a colour in; a `PixelRect` and an RGBA8 buffer out, in the layer
texture's own form. A Text module in the dock with a real egui `TextEdit`, the
controls of §8, and a **Place** button that calls `begin_float(rect,
Some(&pixels))` and switches to the transform tool — which is, line for line,
what `paste` already does.

What this buys: you can set a caption, in any font on the machine, move it,
scale it, rotate it, flip it, put it down, and undo it. What it deliberately
avoids: no canvas keyboard, no caret, no IME, no `Transform::reseat`, no
`retype_float`, no new GPU code, no format change, no new `EditKind`. IME and
clipboard work because the typing happens in a `TextEdit`.

It is also the stage where the fonts land — §2's A, B and C — because the panel
is where they are chosen.

### Stage 2 — the font list, properly

System enumeration through `fontdb`, the curated bundle, the user's own folder,
search, and each row drawn in its own face with a cache keyed by face **and
shape**. Separable from stage 1 because stage 1 can ship with a plain list of
family names.

### Stage 3 — on canvas ★ *the risky one*

The Text tool, the caret, the text selection, click-to-place, `Transform::reseat`,
`CanvasRenderer::retype_float`, the `set_typing` change, `event.text`, the
`Enter`/`Ctrl+Enter` split, IME, and the re-rasterise-under-transform policy with
its measured budget.

**This is where the risk is, and it is worth being specific about why.** It is
not one hard thing, it is five independent ones — a caret that behaves, a
keyboard that reaches the right place, an IME that exists at all, a transform
that reseats exactly, and a rasterisation policy with a number behind it — and
four of the five are invisible on the machine this is written on. IME in
particular cannot be tested by anyone who does not type in a language that needs
it, which is the same shape of problem as `Settings → Input & pen`: nobody
working on Umber has the hardware. It may deserve the same answer — an
observation pane — or it may deserve to be scoped out and named in the README,
which is a perfectly honest thing to ship.

### Stage 4 — text layers that reopen editable

`umber-text` under `umber/text/`, the pixel fingerprint, the frozen-on-missing-
font rule, and the layer kind of §3. No `umber-version` bump, on the argument in
§3 and only with the fingerprint. This is the largest stage and the least
urgent.

### Stage 5 — text as a selection, if it is ever wanted

Glyph outlines *are* closed rings, and `Selection::from_rings` already takes
rings in document space and fills them **nonzero**, which is exactly the winding
rule TrueType counters need — the hole in an `o` is an opposite-wound contour and
falls out with no special case. What is needed is flattening `skrifa`'s
quadratics and cubics into polylines. Small, self-contained, and genuinely
useful for cutting text out of a photograph.

---

## 10. What this design refuses

- **Bundling the Google Fonts catalogue**, or anything like it. §2.
- **Embedding fonts in the `.ora`.** §3.
- **A second reader, a second blend, or a second path to the canvas.** No
  `glyphon`; the pixels go where a paste's pixels go. §5.
- **Re-rendering a text layer in a substituted font.** Frozen and named, never
  silent. §3.
- **A blur that snaps sharp on release.** The preview is what commits, or the
  whole `Float` argument is worth nothing. §4(c).
- **Drawing any of it before it works.** A font dropdown listing faces that
  cannot be set, or a text layer that says it is editable and is not, is the
  control that lies — which this project refuses everywhere else and should
  refuse here.
