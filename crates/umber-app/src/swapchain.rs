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
    /// Every answer, so the tests below can sweep them rather than naming the
    /// cases they happen to remember — which is how a variant added later ends
    /// up outside the one property that matters. `cfg(test)` because the sweep
    /// is the only caller and the shipped binary should not carry a table
    /// nothing reads.
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
}

/// What the frame that received an [`Acquisition`] does about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    /// Whether this frame draws into the texture it was handed.
    pub draws: bool,
    /// Whether the surface has to be configured again before the next
    /// acquisition. *When* is [`Frame::reconfigure_now`]'s answer, not this
    /// one's — reading this field alone is the bug this module documents.
    pub reconfigure: bool,
}

impl Frame {
    /// Whether the reconfigure may be carried out on the spot.
    ///
    /// Only where the frame is not drawing, because only then is there no
    /// surface texture alive to refuse it.
    pub fn reconfigure_now(self) -> bool {
        self.reconfigure && !self.draws
    }

    /// Whether the reconfigure has to wait until the texture in hand has been
    /// presented — in practice, until just before the next acquisition.
    pub fn reconfigure_later(self) -> bool {
        self.reconfigure && self.draws
    }
}

/// What to do with an acquisition.
pub fn plan(acquisition: Acquisition) -> Frame {
    match acquisition {
        // Nothing to put right.
        Acquisition::Fresh => Frame {
            draws: true,
            reconfigure: false,
        },
        // The texture is usable, so the frame is drawn rather than thrown
        // away — a dropped frame during a resize drag is a window that
        // visibly stutters. The reconfigure is what the *next* acquisition
        // needs, and waits for it.
        Acquisition::Suboptimal => Frame {
            draws: true,
            reconfigure: true,
        },
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
        Acquisition::Outdated | Acquisition::Lost => Frame {
            draws: false,
            reconfigure: true,
        },
        // A minimised window and a swapchain that was busy are both "come back
        // later". Reconfiguring would be a stall for nothing, and on a
        // minimised window it would be a stall for nothing every frame.
        Acquisition::Occluded | Acquisition::Timeout | Acquisition::Failed => Frame {
            draws: false,
            reconfigure: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole of the crash, as a property rather than as one case: nothing
    /// that keeps a surface texture may reconfigure while it holds it. Written
    /// over `ALL` so an acquisition added later is covered by construction.
    #[test]
    fn no_frame_reconfigures_while_it_holds_a_texture() {
        for acquisition in Acquisition::ALL {
            let frame = plan(acquisition);
            assert!(
                !(frame.draws && frame.reconfigure_now()),
                "{acquisition:?} would configure the surface with its texture still alive"
            );
        }
    }

    /// A reconfigure happens once — now or later, never both and never
    /// neither. Reading `reconfigure` and picking a moment by hand at the call
    /// site is what this replaces.
    #[test]
    fn a_reconfigure_is_either_immediate_or_deferred_and_never_both() {
        for acquisition in Acquisition::ALL {
            let frame = plan(acquisition);
            assert_eq!(
                frame.reconfigure,
                frame.reconfigure_now() ^ frame.reconfigure_later(),
                "{acquisition:?}"
            );
        }
    }

    /// The intent the old comment stated and the old code did not: a
    /// suboptimal acquisition is still drawn, and its reconfigure is pending.
    #[test]
    fn a_suboptimal_frame_draws_and_leaves_a_reconfigure_pending() {
        let frame = plan(Acquisition::Suboptimal);
        assert!(frame.draws);
        assert!(frame.reconfigure_later());
        assert!(!frame.reconfigure_now());
    }

    #[test]
    fn a_fresh_acquisition_draws_and_touches_nothing() {
        let frame = plan(Acquisition::Fresh);
        assert!(frame.draws);
        assert!(!frame.reconfigure);
    }

    /// Where no texture came back there is nothing to wait for, so the
    /// reconfigure is immediate — which is what the surface needs before the
    /// next acquisition can succeed.
    #[test]
    fn an_outdated_or_lost_surface_is_reconfigured_at_once() {
        for acquisition in [Acquisition::Outdated, Acquisition::Lost] {
            let frame = plan(acquisition);
            assert!(!frame.draws, "{acquisition:?}");
            assert!(frame.reconfigure_now(), "{acquisition:?}");
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
            let frame = plan(acquisition);
            assert!(!frame.draws, "{acquisition:?}");
            assert!(!frame.reconfigure, "{acquisition:?}");
        }
    }
}
