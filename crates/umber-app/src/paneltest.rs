//! Reading back what a panel actually drew, without a window.
//!
//! `CLAUDE.md`'s central testing rule is that a guard has to measure the
//! output rather than restate the rule, and for anything this crate *paints*
//! that means asking egui what it put on the screen. `Context::run_ui` returns
//! a `FullOutput` whose `shapes` carry every galley, so "is this label there"
//! is a genuine panel test needing neither a device nor a display — which is
//! what caught a canvas dialog offering a 16384 button on a 4096 machine, and
//! an export dialog whose loss warnings nothing had ever drawn.
//!
//! This module exists because that walk was written twice, verbatim, in
//! `canvasdlg` and `exportdlg`, and a third headless panel guard is plainly
//! coming. Ten lines duplicated is not expensive; two copies of a rule that
//! can drift is the thing this codebase refuses everywhere else, and the
//! precedent for putting it here rather than in one of the two is
//! [`crate::gputest`] — a shared `#[cfg(test)]` module for a rule the whole
//! crate answers to.

/// Every string a pass drew, in the order the shapes were emitted.
///
/// A `Shape::Vec` nests, which is how egui returns a widget that painted
/// several things, so this recurses rather than walking the top level — a flat
/// pass finds the frame's own shapes and none of the labels inside them.
pub fn text_of(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
    let mut into = Vec::new();
    for clipped in shapes {
        walk(&clipped.shape, &mut into);
    }
    into
}

fn walk(shape: &egui::Shape, into: &mut Vec<String>) {
    match shape {
        egui::Shape::Text(text) => into.push(text.galley.text().to_owned()),
        egui::Shape::Vec(inner) => {
            for shape in inner {
                walk(shape, into);
            }
        }
        _ => {}
    }
}
