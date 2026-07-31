//! The startup splash.
//!
//! A splash screen is an awkward thing to add to an application whose entire
//! reason for existing is latency, so it is worth being precise about what this
//! one is and is not.
//!
//! The conventional splash is a window shown *while* the application loads,
//! covering a gap the user would otherwise spend staring at nothing. Umber does
//! have such a gap, but it is not in a place a splash can reach. Timing
//! `UmberApp::resumed` stage by stage on the development machine (Windows,
//! D3D12) gives, from a warm start:
//!
//! | stage | cost |
//! |---|---|
//! | creating the window | 50–250 ms |
//! | `Gpu::new` — adapter request and device creation | 0.7–5 s |
//! | configuring the surface | 0.5–0.75 s |
//! | `CanvasRenderer::new` — pipelines and shaders | 10–90 ms |
//! | egui context and Archivo | under 10 ms |
//!
//! Effectively all of it is the graphics driver bringing a device up, and the
//! window already exists — blank — while that happens. Everything after the
//! device is ready costs under a tenth of a second put together. So a splash
//! drawn on the GPU cannot cover the wait: it could not appear until the very
//! thing it would be covering had finished. Covering it properly needs the
//! window painted *without* a device — a software framebuffer such as
//! `softbuffer`, or deferring the window until the first frame is ready — which
//! is a second rendering path and a separate change. `resumed` logs the split at
//! info level so the decision can be revisited against real numbers.
//!
//! So this is deliberately not a loading screen. It is a brand overlay drawn
//! into the first real frame, on top of an interface that is already live,
//! fading out over [`FADE`]. Three properties follow, and all three are the
//! point:
//!
//! * **It never delays the first paint.** The first frame is submitted exactly
//!   when it would have been without it; the splash is simply some more shapes
//!   in that frame. There is no sleep anywhere in this module, and no timed
//!   hold before the fade starts — the clock begins on the frame that paints.
//! * **It costs nothing once it is gone.** [`Splash::draw`] returns `false` on
//!   the frame it stops drawing and the caller drops it, leaving one `Option`
//!   check per frame that is `None` forever after.
//! * **It is skippable.** Any key or pointer press ends it immediately.
//!
//! While it is visible it does claim pointer input, which is what stops the
//! click that dismisses it also laying a dab on the canvas underneath.

use crate::logo;
use crate::theme::{Palette, text};
use egui::{Align2, FontFamily, FontId, Vec2, pos2, vec2};
use std::time::Instant;

/// How long the overlay takes to fade out.
///
/// Long enough to register as a deliberate reveal rather than a flicker, short
/// enough that nobody reaches for the mouse before it has gone. The interface
/// underneath is fully drawn and fully live throughout.
const FADE: f32 = 0.28;

/// Mark size, in points. Not from `theme::metrics` — the splash is not in the
/// design, so these are this module's own and belong here rather than in the
/// design's token table.
const MARK: f32 = 88.0;

/// Gap between the mark and the wordmark.
const GAP: f32 = 22.0;

/// Wordmark size, in points.
const WORDMARK: f32 = 26.0;

#[derive(Default)]
pub struct Splash {
    /// Set on the first *painted* frame rather than at construction.
    ///
    /// The gap between the two is GPU start-up, which on a cold driver can be
    /// most of a second. Starting the clock at construction would spend the
    /// whole fade before a single pixel had been shown, and the splash would
    /// appear as a one-frame flash of brown.
    began: Option<Instant>,
}

impl Splash {
    /// Draw the overlay over the finished frame.
    ///
    /// Returns `true` while it is still on screen; the caller drops it and stops
    /// asking for frames the moment this is `false`.
    pub fn draw(&mut self, ui: &egui::Ui, palette: &Palette) -> bool {
        let ctx = ui.ctx();
        let began = *self.began.get_or_insert_with(Instant::now);

        // Any input at all ends it — a click, a key, a wheel notch. Someone who
        // has already reached for the pen is not waiting to be shown a logo.
        let dismissed = ctx.input(|i| {
            i.pointer.any_pressed()
                || i.smooth_scroll_delta != Vec2::ZERO
                || i.events
                    .iter()
                    .any(|e| matches!(e, egui::Event::Key { pressed: true, .. }))
        });
        if dismissed {
            return false;
        }

        let alpha = fade_alpha(began.elapsed().as_secs_f32());
        if alpha <= 0.0 {
            return false;
        }

        let screen = ctx.viewport_rect();
        egui::Area::new(egui::Id::new("umber-splash"))
            .order(egui::Order::Foreground)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                // Sized to the window and sensing clicks, so egui reports that
                // it wants the pointer and the click that dismisses the splash
                // does not also start a stroke on the canvas behind it.
                ui.allocate_response(screen.size(), egui::Sense::click());
                ui.multiply_opacity(alpha);
                let painter = ui.painter();

                // The backdrop, not the chrome: the splash reads as the
                // application's own surface rather than as a floating panel,
                // and it fades into the canvas surround it is already sitting
                // on top of.
                painter.rect_filled(screen, 0.0, palette.backdrop);

                let centre = screen.center();
                let mark_rect = egui::Rect::from_center_size(
                    centre - vec2(0.0, MARK * 0.45),
                    Vec2::splat(MARK),
                );
                logo::draw_mark(painter, mark_rect, palette);

                painter.text(
                    pos2(centre.x, mark_rect.max.y + GAP),
                    Align2::CENTER_TOP,
                    "Umber",
                    FontId::new(WORDMARK, FontFamily::Proportional),
                    palette.text_strong,
                );
                painter.text(
                    pos2(centre.x, mark_rect.max.y + GAP + WORDMARK + 6.0),
                    Align2::CENTER_TOP,
                    env!("CARGO_PKG_VERSION"),
                    FontId::new(text::SMALL, FontFamily::Proportional),
                    palette.text_muted,
                );
            });

        // egui is told the frame is not final; the caller separately asks winit
        // for the next redraw, because the event loop is `ControlFlow::Wait` and
        // would otherwise sleep until the user did something.
        ctx.request_repaint();
        true
    }
}

/// Opacity of the overlay `elapsed` seconds after the first painted frame.
///
/// Smoothstep, so the fade eases out of full opacity and into nothing rather
/// than stopping dead at either end. Written as arithmetic rather than with
/// `powf` on purpose: a base that has drifted a hair below zero with a
/// fractional exponent is NaN, and this value goes straight into
/// `Ui::multiply_opacity`, which asserts.
fn fade_alpha(elapsed: f32) -> f32 {
    let t = (elapsed / FADE).clamp(0.0, 1.0);
    1.0 - t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fade_starts_opaque_and_ends_clear() {
        assert_eq!(fade_alpha(0.0), 1.0);
        assert_eq!(fade_alpha(FADE), 0.0);
        // Past the end it stays clear rather than going negative and wrapping
        // into a second, inverted fade.
        assert_eq!(fade_alpha(FADE * 10.0), 0.0);
    }

    #[test]
    fn the_fade_never_leaves_the_range_an_opacity_may_take() {
        // `multiply_opacity` asserts on anything outside 0..=1, and a NaN is
        // the way this has gone wrong before. Walk well past both ends.
        let mut t = -1.0;
        while t < FADE * 3.0 {
            let a = fade_alpha(t);
            assert!(a.is_finite(), "alpha at {t} is {a}");
            assert!((0.0..=1.0).contains(&a), "alpha at {t} is {a}");
            t += FADE / 64.0;
        }
    }

    #[test]
    fn the_fade_only_ever_decreases() {
        let mut previous = f32::INFINITY;
        for step in 0..=64 {
            let a = fade_alpha(FADE * step as f32 / 64.0);
            assert!(a <= previous, "alpha rose at step {step}");
            previous = a;
        }
    }
}
