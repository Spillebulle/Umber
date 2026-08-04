//! What a frame does with the answer `Surface::get_current_texture` gave.
//!
//! A model with no wgpu in it, the same division `gesture.rs` keeps against
//! `window_event` and `dock.rs` keeps against `panels.rs`: `app.rs` translates
//! wgpu's `CurrentSurfaceTexture` into an [`Acquisition`], reads the [`Frame`]
//! back and does what it says. That is the only way any of this is checked at
//! all — the headless GPU harness is created with **no surface**, deliberately,
//! so a surface-lifecycle rule cannot be tested there, and the platform this
//! one is broken on is one nobody working on Umber runs.
//!
//! The rule it exists to hold: **the surface is reconfigured only while no
//! surface texture is in hand.** wgpu-core's `configure` waits for the device
//! to go idle and then refuses outright if the surface still has an acquired
//! texture — `PreviousOutputExists`, printed as "`SurfaceOutput` must be
//! dropped before a new `Surface` is made" — and `crash::device_error` makes
//! any uncaptured device error fatal, as it must. `Suboptimal` is the one
//! answer that both hands over a texture *and* asks for a reconfigure, so it
//! is the one place the two can be got the wrong way round; the reconfigure
//! therefore waits until that texture has been presented.
//!
//! This was a crash, reported against 0.0.6 on Debian/Wayland: dragging the
//! window to the top edge of a second monitor for the maximise preview
//! produces a burst of configure events, Wayland reports `Suboptimal` far more
//! readily than the other backends, and the frame that got one reconfigured
//! the surface with the texture it was about to draw into still alive.

/// What `Surface::get_current_texture` answered, with wgpu's texture taken out
/// so this module needs no device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Acquisition {
    /// A texture, and the surface matches the window it is presented to.
    Fresh,
    /// A texture, but the surface no longer matches the window. The texture is
    /// usable; wgpu asks for a reconfigure for the sake of the *next* frame.
    Suboptimal,
    /// No texture: the surface's configuration is out of date.
    Outdated,
    /// No texture: the swapchain is gone.
    Lost,
    /// No texture: the window is minimised or hidden.
    Occluded,
    /// No texture: the swapchain did not answer in time.
    Timeout,
    /// No texture, for a reason this build does not recognise.
    Failed,
}

impl Acquisition {
    /// Every answer, so the tests below sweep them rather than naming the
    /// cases they happen to remember.
    ///
    /// It is **not** exhaustive by construction and saying so matters: a
    /// variant added later has to be written in here by hand. What is forced
    /// is the *implementation* — `plan` and [`Acquisition::carries_texture`]
    /// are exhaustive matches, so neither builds until the new answer has been
    /// thought about. `cfg(test)` because the sweep is the only caller, which
    /// is deliberately not true of `carries_texture`; see there.
    #[cfg(test)]
    pub const ALL: [Self; 7] = [
        Self::Fresh,
        Self::Suboptimal,
        Self::Outdated,
        Self::Lost,
        Self::Occluded,
        Self::Timeout,
        Self::Failed,
    ];

    /// Whether wgpu handed a surface texture over with this answer.
    ///
    /// A fact about `CurrentSurfaceTexture`, written down separately from
    /// [`plan`] and deliberately not derived from it — that is the whole
    /// reason the tests below say anything. A property checked against a
    /// restatement of the code it is checking is a tautology, and two of them
    /// shipped in the first draft of this module.
    ///
    /// It is **not** `cfg(test)`, and that was the second thing this module
    /// got wrong. The model can only be right about a translation it is shown:
    /// `app.rs` maps wgpu's answers onto these by hand, and mapping a
    /// texture-carrying one onto `Outdated` would bring the crash straight
    /// back with every test here still passing, because the plan was never
    /// wrong — the translation was. So `app.rs` asserts this against
    /// `Option::is_some` at the one line where the two meet, which it cannot
    /// do against something that exists only under `cargo test`.
    ///
    /// An exhaustive `match` rather than a `matches!`, so an answer added
    /// later cannot default to "no texture" — which is the reading that would
    /// quietly make the safety property hold by never being tested.
    pub fn carries_texture(self) -> bool {
        match self {
            Self::Fresh | Self::Suboptimal => true,
            Self::Outdated | Self::Lost | Self::Occluded | Self::Timeout | Self::Failed => false,
        }
    }
}

/// What the frame that received an [`Acquisition`] does about it.
///
/// An enum of the four whole answers rather than a `draws` flag beside a
/// `reconfigure` flag, and the gain is exactly one thing, stated precisely
/// because the first attempt at this comment overstated it: **there is no
/// field a call site can read and act on at the wrong moment.** The old shape
/// exposed `reconfigure` publicly, which says *whether* and never *when*, and
/// acting on it directly is the whole of the bug — its own doc said so, which
/// is a warning where this is a type.
///
/// It is not a guarantee that the crash is unreachable. Nothing here can stop
/// `app.rs` calling `configure` with a texture in hand; what the enum removes
/// is the value that would make doing so look reasonable. And a test asserting
/// `draws` and `reconfigure_now` are never both true says nothing under either
/// spelling, since both are derived from the same value — which is what the
/// first draft shipped, unfalsifiably, twice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Frame {
    /// Draw into the texture. The surface is as it should be.
    Draw,
    /// Draw into the texture, and configure the surface before the *next*
    /// acquisition — never before this frame has let go of it.
    DrawThenReconfigure,
    /// No texture came back, so there is nothing to let go of: configure now
    /// and skip the frame.
    ReconfigureAndSkip,
    /// No texture came back and there is nothing to put right. Skip.
    Skip,
}

impl Frame {
    /// Whether this frame draws into the texture it was handed.
    pub fn draws(self) -> bool {
        matches!(self, Self::Draw | Self::DrawThenReconfigure)
    }

    /// Whether the surface may be configured on the spot.
    ///
    /// Only where the frame is not drawing, because only then is there no
    /// surface texture alive for `configure` to refuse. That sentence is why
    /// the ordering at the call site is not arbitrary, and it belongs here.
    pub fn reconfigure_now(self) -> bool {
        matches!(self, Self::ReconfigureAndSkip)
    }

    /// Whether a reconfigure has to wait until the texture in hand has been
    /// presented — in practice, until just before the next acquisition.
    pub fn reconfigure_later(self) -> bool {
        matches!(self, Self::DrawThenReconfigure)
    }
}

/// What to do with an acquisition.
pub fn plan(acquisition: Acquisition) -> Frame {
    match acquisition {
        // Nothing to put right.
        Acquisition::Fresh => Frame::Draw,
        // The texture is usable, so the frame is drawn rather than thrown
        // away — a dropped frame during a resize drag is a window that
        // visibly stutters. The reconfigure is what the *next* acquisition
        // needs, and waits for it.
        Acquisition::Suboptimal => Frame::DrawThenReconfigure,
        // No texture was handed over, so there is nothing to hold the
        // reconfigure back and every reason to do it before returning: wgpu's
        // guidance for `Outdated` is "configure and try again".
        //
        // `Lost` is the same answer and is deliberately not a stronger one.
        // wgpu says a lost surface should be created again through
        // `Instance::create_surface`, which means a new window handle and a
        // new `egui_wgpu::Renderer`; nothing has ever done that here, and a
        // reconfigure is what Umber has always tried. Making that a rebuild is
        // a change worth making on evidence that it happens, not on the way
        // past.
        Acquisition::Outdated | Acquisition::Lost => Frame::ReconfigureAndSkip,
        // A minimised window and a swapchain that was busy are both "come back
        // later". Reconfiguring would be a stall for nothing, and on a
        // minimised window it would be a stall for nothing every frame.
        Acquisition::Occluded | Acquisition::Timeout | Acquisition::Failed => Frame::Skip,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The crash, stated against something other than itself: an answer that
    /// carries a surface texture must never be planned to configure the
    /// surface on the spot.
    ///
    /// `carries_texture` is a fact about wgpu's API written down beside
    /// `plan` rather than read out of it, which is the only reason this says
    /// anything. Asserting `!(frame.draws() && frame.reconfigure_now())`
    /// instead is a tautology — with either spelling of `Frame`, `draws` and
    /// `reconfigure_now` are derived from the same value — and two such tests
    /// shipped in the first draft of this module and could not have failed.
    #[test]
    fn nothing_that_was_handed_a_texture_reconfigures_on_the_spot() {
        for acquisition in Acquisition::ALL {
            assert!(
                !(acquisition.carries_texture() && plan(acquisition).reconfigure_now()),
                "{acquisition:?} would configure the surface with its texture still alive"
            );
        }
    }

    /// The other half, and the reason the one above cannot be satisfied by
    /// simply never drawing: every texture wgpu hands over is drawn into, and
    /// no frame claims to draw without one.
    #[test]
    fn a_texture_that_came_back_is_the_one_that_is_drawn_into() {
        for acquisition in Acquisition::ALL {
            assert_eq!(
                plan(acquisition).draws(),
                acquisition.carries_texture(),
                "{acquisition:?}"
            );
        }
    }

    /// The intent the old comment stated and the old code did not: a
    /// suboptimal acquisition is still drawn, and its reconfigure is pending.
    #[test]
    fn a_suboptimal_frame_draws_and_leaves_a_reconfigure_pending() {
        assert_eq!(plan(Acquisition::Suboptimal), Frame::DrawThenReconfigure);
    }

    #[test]
    fn a_fresh_acquisition_draws_and_touches_nothing() {
        assert_eq!(plan(Acquisition::Fresh), Frame::Draw);
    }

    /// Where no texture came back there is nothing to wait for, so the
    /// reconfigure is immediate — which is what the surface needs before the
    /// next acquisition can succeed.
    #[test]
    fn an_outdated_or_lost_surface_is_reconfigured_at_once() {
        for acquisition in [Acquisition::Outdated, Acquisition::Lost] {
            assert_eq!(
                plan(acquisition),
                Frame::ReconfigureAndSkip,
                "{acquisition:?}"
            );
        }
    }

    /// A minimised window must not be handed a device stall per frame.
    #[test]
    fn a_frame_with_nothing_to_draw_on_reconfigures_nothing() {
        for acquisition in [
            Acquisition::Occluded,
            Acquisition::Timeout,
            Acquisition::Failed,
        ] {
            assert_eq!(plan(acquisition), Frame::Skip, "{acquisition:?}");
        }
    }
}
