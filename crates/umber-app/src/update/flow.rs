//! What the update dialog is showing, and what it may do next.
//!
//! This is the model, with no drawing in it at all — the same division
//! `dock.rs` keeps against `panels.rs` and `brushdrag.rs` against `brushlib.rs`.
//! `updatedlg.rs` paints it. Keeping the two apart is what makes the whole of
//! an update — offer, download, unpack, install, the countdown, and every
//! failure and cancellation on the way — testable without a window and without
//! a socket, which is the only way any of it gets tested at all: nobody can cut
//! a release to try the real thing against.
//!
//! Three rules the shape here exists to hold:
//!
//! 1. **The screens are one enum, not a set of booleans.** "Downloading" and
//!    "done" cannot both be true, and a phase that carries its own data cannot
//!    be read while the field it needs is stale.
//! 2. **Only the worker decides how the work ended.** A cancel *asks*
//!    ([`Phase::Stopping`]); whether it arrived in time is answered by the
//!    thread, which is the only thing that knows whether the swap had already
//!    happened. Marking the flow cancelled at the click would let the dialog
//!    say a release was not installed after it was.
//! 3. **A stage report that arrives late changes nothing.** [`Flow::stage`] is
//!    refused unless the work is still running, so a progress message queued
//!    behind a failure cannot put the bar back up.

use super::apply::Applied;
use super::install::{Arch, InstallKind};
use super::release::Release;
use super::version::Version;
use std::time::{Duration, Instant};

/// How long the completion screen waits before it restarts — or, for the
/// Windows installer, closes — on its own.
///
/// Five seconds, which is what the user asked for and is about right: long
/// enough to read the sentence above it and reach the Cancel, short enough that
/// somebody who has walked away comes back to the new version rather than to a
/// dialog.
pub const RESTART_DELAY: Duration = Duration::from_secs(5);

/// Where an update has got to.
#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    /// Screen one: a newer release exists, and here is what it says.
    Offer,
    /// Screen two: the work, with a bar and the stage it is in.
    Working(Stage),
    /// The user has asked to stop and the worker has not answered yet. Its
    /// answer is what decides between [`Phase::Stopped`] and the update having
    /// landed anyway — see the module comment.
    ///
    /// It carries the stage it was stopped from, so the bar holds the reading
    /// it had. Emptying it would read as a reset, and the download is still
    /// running until the worker says otherwise.
    Stopping(Stage),
    /// Stopped before anything was written.
    Stopped,
    /// The new build is in place, or an installer is running. The countdown is
    /// live unless it has been cancelled.
    Done {
        outcome: Applied,
        countdown: Countdown,
    },
    /// Something went wrong, in a sentence written for the user.
    Failed(String),
}

/// What the work is doing, for the bar and the line under it.
///
/// Each variant is a stage that genuinely happens, in the order it happens. No
/// stage is invented to make the bar move: the one place Umber cannot report
/// real progress — Windows' own installer, once it has the package — is a
/// *stage of its own* that says so, rather than a bar creeping forward over
/// something it knows nothing about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Opening the connection and asking GitHub for the file. Short on a good
    /// connection and the whole of the wait on a bad one, which is why it is
    /// named rather than folded into the download.
    Contacting,
    /// Bytes are arriving. `total` is the length the release API reported, so
    /// the percentage is a real one.
    Downloading { received: u64, total: u64 },
    /// Comparing what arrived with that length.
    ///
    /// Deliberately **not** called verifying. Umber does not sign its releases;
    /// a length is a length, and a word that a reader would take for a
    /// signature check would be the dialog claiming a guarantee that does not
    /// exist.
    CheckingLength,
    /// Lifting the binary out of the archive.
    Unpacking,
    /// Writing the new build over the old one.
    Installing,
    /// Handing the package to `msiexec`. Windows owns the installation from
    /// here, and the completion screen says so.
    HandingOver,
}

impl Stage {
    /// How far along the bar sits, 0..=1.
    ///
    /// Weighted by what each stage actually costs, exactly as `splash::Stage`
    /// is: the download is nearly all of the wall clock, so it gets nearly all
    /// of the bar and the rest are the thin slices they are. `None` means there
    /// is no honest figure — the bar is drawn as a track with no fill rather
    /// than as a guess.
    pub fn progress(self) -> Option<f32> {
        Some(match self {
            Self::Contacting => 0.03,
            Self::Downloading { received, total } => {
                // A release with no recorded length cannot produce a
                // percentage, and inventing one is the thing this whole module
                // refuses to do.
                if total == 0 {
                    return None;
                }
                let fraction = (received as f64 / total as f64).clamp(0.0, 1.0) as f32;
                0.05 + 0.80 * fraction
            }
            Self::CheckingLength => 0.88,
            Self::Unpacking => 0.92,
            Self::Installing | Self::HandingOver => 0.97,
        })
    }

    /// The line under the bar, in the splash's lower-case present-participle
    /// voice.
    pub fn label(self) -> String {
        match self {
            Self::Contacting => "contacting GitHub".to_string(),
            Self::Downloading { received, total } => match total {
                0 => format!("downloading, {} so far", megabytes(received)),
                _ => format!(
                    "downloading, {} of {} ({}%)",
                    megabytes(received),
                    megabytes(total),
                    (received.min(total) * 100 / total.max(1)),
                ),
            },
            Self::CheckingLength => "checking the length against GitHub's figure".to_string(),
            Self::Unpacking => "unpacking the archive".to_string(),
            Self::Installing => "putting the new build in place".to_string(),
            Self::HandingOver => "installing, Windows is handling this from here".to_string(),
        }
    }

    /// Whether stopping now would leave the installation untouched.
    ///
    /// True for everything up to and including the length check: nothing has
    /// been written at that point, so abandoning costs a download and no more.
    /// From [`Stage::Unpacking`] on, a stop could land in the middle of the
    /// swap, and a half-replaced binary is the one outcome that costs somebody
    /// their installation. The Cancel is therefore taken off the screen rather
    /// than offered and refused.
    pub fn can_stop(self) -> bool {
        matches!(
            self,
            Self::Contacting | Self::Downloading { .. } | Self::CheckingLength
        )
    }
}

/// A size in the unit somebody would say out loud.
fn megabytes(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let mb = bytes as f64 / MB;
    if mb < 10.0 {
        format!("{mb:.1} MB")
    } else {
        format!("{mb:.0} MB")
    }
}

/// The wait before the completion screen acts on its own.
///
/// Holds a deadline rather than a remaining duration, so it is a pure function
/// of the clock the caller passes in — which is what lets every case below be
/// tested without sleeping for five seconds, or at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Countdown {
    deadline: Option<Instant>,
}

impl Countdown {
    /// A countdown running from `now`.
    pub fn started(now: Instant) -> Self {
        Self {
            deadline: Some(now + RESTART_DELAY),
        }
    }

    /// One that is not running, which is what a cancel leaves behind.
    pub fn stopped() -> Self {
        Self { deadline: None }
    }

    /// Stop it. The dialog stays up and Umber carries on running.
    pub fn cancel(&mut self) {
        self.deadline = None;
    }

    pub fn running(self) -> bool {
        self.deadline.is_some()
    }

    /// How long is left, or `None` if it is not running.
    pub fn remaining(self, now: Instant) -> Option<Duration> {
        self.deadline
            .map(|deadline| deadline.saturating_duration_since(now))
    }

    /// The figure the screen prints: whole seconds, rounded up, so it reads
    /// 5, 4, 3, 2, 1 rather than starting at 4.
    pub fn seconds_left(self, now: Instant) -> Option<u64> {
        self.remaining(now)
            .map(|left| left.as_secs_f64().ceil() as u64)
    }

    /// Whether the wait is over. False for a cancelled countdown, for ever —
    /// which is the whole of what Cancel buys.
    pub fn elapsed(self, now: Instant) -> bool {
        self.remaining(now).is_some_and(|left| left.is_zero())
    }
}

/// What the offer screen may put in front of the user.
///
/// "Not now" and "Never ask again" are on every offer and are therefore not
/// fields — there is no installation for which dismissing the dialog or
/// switching the check off is unavailable. The two that vary are whether Umber
/// may carry the update out itself, and, where it may not, why.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Actions {
    /// Draw "Update now". Only where this installation is Umber's to replace
    /// *and* the release carries a build for this machine.
    pub update_now: bool,
    /// Draw "Open the releases page" — the answer in every other case, because
    /// being unable to install something is not a reason to leave somebody
    /// unaware that it exists.
    pub open_page: bool,
    /// Why Umber will not do it itself: a package manager owns this copy, or
    /// Umber cannot tell where it is.
    pub obstacle: Option<String>,
    /// The installation is one Umber could replace, but the release published
    /// nothing for this machine.
    pub no_build: bool,
}

/// Which actions belong on the offer screen for this installation.
///
/// `has_asset` is whether [`super::Updates::installable`] found a file — the
/// two questions are separate on purpose, because "a package manager owns this"
/// and "that release carries no aarch64 build" have different answers even
/// though both end at the releases page.
///
/// `version` and `arch` are what the obstacle needs to name the package a
/// managed installation should fetch. They are the release's version, not this
/// build's: the sentence is about the thing being offered.
pub fn actions(
    kind: &InstallKind,
    version: &Version,
    arch: Option<Arch>,
    has_asset: bool,
) -> Actions {
    // The obstacle is asked for first and wins outright. A managed installation
    // that happens to have a matching asset in the release must still never be
    // offered "Update now" — that button is the first half of writing over a
    // package manager's files.
    if let Some(obstacle) = kind.cannot_update(version, arch) {
        return Actions {
            update_now: false,
            open_page: true,
            obstacle: Some(obstacle),
            no_build: false,
        };
    }
    Actions {
        update_now: has_asset,
        open_page: !has_asset,
        obstacle: None,
        no_build: !has_asset,
    }
}

/// One update, from the offer to whatever it turned into.
#[derive(Clone, Debug, PartialEq)]
pub struct Flow {
    /// The release being offered or installed. Held for the whole of the flow,
    /// not read off the check's status, so the completion screen can still name
    /// the version after the status has moved on.
    pub release: Release,
    phase: Phase,
}

impl Flow {
    /// A flow at its first screen.
    pub fn offering(release: Release) -> Self {
        Self {
            release,
            phase: Phase::Offer,
        }
    }

    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    /// Move to screen two. Returns false — and changes nothing — if the work
    /// has already begun, so a double click on "Update now" cannot start two
    /// downloads.
    pub fn begin(&mut self) -> bool {
        if !matches!(self.phase, Phase::Offer) {
            return false;
        }
        self.phase = Phase::Working(Stage::Contacting);
        true
    }

    /// A progress report from the worker.
    ///
    /// Ignored unless the work is still running: a message queued behind a
    /// failure, or one sent while the user's stop request was in flight, must
    /// not put the bar back up.
    pub fn stage(&mut self, stage: Stage) {
        if matches!(self.phase, Phase::Working(_)) {
            self.phase = Phase::Working(stage);
        }
    }

    /// Whether a Cancel belongs on screen right now.
    pub fn can_stop(&self) -> bool {
        matches!(&self.phase, Phase::Working(stage) if stage.can_stop())
    }

    /// The user has asked to stop. The worker answers.
    pub fn request_stop(&mut self) -> bool {
        let Phase::Working(stage) = &self.phase else {
            return false;
        };
        if !stage.can_stop() {
            return false;
        }
        self.phase = Phase::Stopping(*stage);
        true
    }

    /// The worker stopped without writing anything.
    pub fn stopped(&mut self) {
        if self.running() {
            self.phase = Phase::Stopped;
        }
    }

    /// The worker finished. `now` starts the countdown.
    ///
    /// Accepted even out of [`Phase::Stopping`], and that is the point: a stop
    /// that arrived after the swap had happened did not stop anything, and the
    /// screen has to say what is true rather than what was asked for.
    pub fn finished(&mut self, outcome: Applied, now: Instant) {
        if self.running() {
            self.phase = Phase::Done {
                outcome,
                countdown: Countdown::started(now),
            };
        }
    }

    pub fn failed(&mut self, message: String) {
        if self.running() {
            self.phase = Phase::Failed(message);
        }
    }

    /// Go back to the offer, after a stop or a failure.
    ///
    /// The way "Try again" is spelt. Only from a screen where nothing is
    /// running and nothing was installed: retrying a *completed* update would
    /// download the same release over the copy of it already in place, and
    /// retrying one still in flight would start a second download.
    pub fn reoffer(&mut self) -> bool {
        if !matches!(self.phase, Phase::Stopped | Phase::Failed(_)) {
            return false;
        }
        self.phase = Phase::Offer;
        true
    }

    /// Whether a worker's answer is still expected.
    fn running(&self) -> bool {
        matches!(self.phase, Phase::Working(_) | Phase::Stopping(_))
    }

    /// Stop the countdown, leaving Umber running. Returns false where there is
    /// no countdown to stop.
    pub fn cancel_countdown(&mut self) -> bool {
        match &mut self.phase {
            Phase::Done { countdown, .. } if countdown.running() => {
                countdown.cancel();
                true
            }
            _ => false,
        }
    }

    /// The countdown, where there is one.
    pub fn countdown(&self) -> Option<Countdown> {
        match &self.phase {
            Phase::Done { countdown, .. } => Some(*countdown),
            _ => None,
        }
    }

    /// What the completion screen should do now, if the wait is over.
    ///
    /// Read once a frame. Returns `None` on every frame but the one the
    /// deadline passes on, and never for a cancelled countdown.
    pub fn due(&self, now: Instant) -> Option<Applied> {
        match &self.phase {
            Phase::Done { outcome, countdown } if countdown.elapsed(now) => Some(*outcome),
            _ => None,
        }
    }

    /// Whether closing the dialog would abandon work in flight.
    ///
    /// Escape and the click-outside are refused while this is true: a modal
    /// that vanished mid-download would leave a thread running with nothing on
    /// screen to stop it.
    pub fn holds_work(&self) -> bool {
        self.running()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::install::Manager;
    use crate::update::release::Asset;
    use crate::update::version::Version;
    use std::path::PathBuf;

    fn release() -> Release {
        Release {
            version: Version::parse("9.9.9").expect("parses"),
            tag: "v9.9.9".into(),
            page: "https://github.com/Spillebulle/umber/releases/tag/v9.9.9".into(),
            notes: "Added\n- A thing.\n".into(),
            assets: vec![Asset {
                name: "umber-9.9.9-x64.msi".into(),
                size: 40,
                browser_download_url: "https://github.com/x/umber-9.9.9-x64.msi".into(),
            }],
        }
    }

    fn flow() -> Flow {
        Flow::offering(release())
    }

    /// The version the offer is about, which is what an obstacle names a
    /// package file for.
    fn offered() -> Version {
        release().version
    }

    /// The whole happy path, in the order it happens.
    #[test]
    fn an_update_runs_from_the_offer_to_the_restart() {
        let t0 = Instant::now();
        let mut flow = flow();
        assert_eq!(*flow.phase(), Phase::Offer);
        assert!(flow.begin());
        assert_eq!(*flow.phase(), Phase::Working(Stage::Contacting));

        for stage in [
            Stage::Downloading {
                received: 0,
                total: 40,
            },
            Stage::Downloading {
                received: 20,
                total: 40,
            },
            Stage::Downloading {
                received: 40,
                total: 40,
            },
            Stage::CheckingLength,
            Stage::Unpacking,
            Stage::Installing,
        ] {
            flow.stage(stage);
            assert_eq!(*flow.phase(), Phase::Working(stage));
        }

        flow.finished(Applied::Restart, t0);
        let Phase::Done { outcome, countdown } = flow.phase().clone() else {
            panic!("the flow should be done, not {:?}", flow.phase());
        };
        assert_eq!(outcome, Applied::Restart);
        assert!(countdown.running());
        assert_eq!(flow.due(t0), None, "the wait has not passed yet");
        assert_eq!(
            flow.due(t0 + RESTART_DELAY),
            Some(Applied::Restart),
            "and it fires exactly once the delay is up",
        );
    }

    #[test]
    fn a_second_click_on_update_now_does_not_start_a_second_download() {
        let mut flow = flow();
        assert!(flow.begin());
        assert!(!flow.begin(), "the work is already running");
        flow.stage(Stage::Unpacking);
        assert!(!flow.begin(), "and cannot be restarted from under itself");
        assert_eq!(*flow.phase(), Phase::Working(Stage::Unpacking));
    }

    #[test]
    fn the_countdown_reads_five_four_three_two_one() {
        let t0 = Instant::now();
        let c = Countdown::started(t0);
        // Rounded up, so the first frame reads 5 rather than 4.
        assert_eq!(c.seconds_left(t0), Some(5));
        assert_eq!(c.seconds_left(t0 + Duration::from_millis(100)), Some(5));
        assert_eq!(c.seconds_left(t0 + Duration::from_millis(4_100)), Some(1));
        assert_eq!(c.seconds_left(t0 + Duration::from_millis(5_000)), Some(0));
        assert!(!c.elapsed(t0 + Duration::from_millis(4_999)));
        assert!(c.elapsed(t0 + Duration::from_millis(5_000)));
        // A clock that ran on well past the deadline is still just "over".
        assert!(c.elapsed(t0 + Duration::from_secs(3_600)));
    }

    #[test]
    fn cancelling_the_countdown_stops_it_for_good() {
        let t0 = Instant::now();
        let mut flow = flow();
        flow.begin();
        flow.finished(Applied::Restart, t0);
        assert!(flow.cancel_countdown());

        // Not now, not in five seconds, not in an hour: a cancelled restart is
        // cancelled, and Umber carries on running with the new build waiting
        // for the next start.
        for after in [0, 5, 60, 3_600] {
            assert_eq!(
                flow.due(t0 + Duration::from_secs(after)),
                None,
                "{after}s after the cancel",
            );
        }
        assert!(!flow.cancel_countdown(), "and cannot be cancelled twice");
        let countdown = flow.countdown().expect("the screen still has one to draw");
        assert!(!countdown.running());
        assert_eq!(countdown.seconds_left(t0), None);
    }

    /// The cancel path in full. The click only *asks*; what happened is the
    /// worker's to say.
    #[test]
    fn a_stop_that_arrives_in_time_leaves_the_installation_alone() {
        let mut flow = flow();
        flow.begin();
        flow.stage(Stage::Downloading {
            received: 10,
            total: 40,
        });
        assert!(flow.can_stop());
        assert!(flow.request_stop());
        assert_eq!(
            *flow.phase(),
            Phase::Stopping(Stage::Downloading {
                received: 10,
                total: 40,
            }),
            "the bar keeps the reading it had while the worker answers",
        );
        assert!(!flow.can_stop(), "there is nothing left to ask twice");

        // A progress message already in the channel when the stop was clicked.
        flow.stage(Stage::Downloading {
            received: 12,
            total: 40,
        });
        assert!(
            matches!(flow.phase(), Phase::Stopping(_)),
            "a late report changes nothing",
        );

        flow.stopped();
        assert_eq!(*flow.phase(), Phase::Stopped);
    }

    #[test]
    fn a_stop_that_arrives_too_late_reports_the_update_that_happened() {
        // The one case a flag on the button would get wrong: by the time the
        // click was read, the swap had already been made. Saying "cancelled"
        // would tell somebody their installation was untouched when it was
        // replaced.
        let t0 = Instant::now();
        let mut flow = flow();
        flow.begin();
        flow.stage(Stage::Downloading {
            received: 40,
            total: 40,
        });
        flow.request_stop();
        flow.finished(Applied::Restart, t0);
        assert!(matches!(
            flow.phase(),
            Phase::Done {
                outcome: Applied::Restart,
                ..
            },
        ));
    }

    #[test]
    fn a_cancel_is_never_offered_once_bytes_are_being_written() {
        // Stopping mid-swap is the one outcome that costs somebody their
        // installation, so the control is taken off the screen rather than
        // drawn and refused.
        for stage in [
            Stage::Contacting,
            Stage::Downloading {
                received: 1,
                total: 2,
            },
            Stage::CheckingLength,
        ] {
            assert!(stage.can_stop(), "{stage:?}");
        }
        for stage in [Stage::Unpacking, Stage::Installing, Stage::HandingOver] {
            assert!(!stage.can_stop(), "{stage:?}");
            let mut flow = flow();
            flow.begin();
            flow.stage(stage);
            assert!(!flow.request_stop(), "{stage:?}");
            assert_eq!(*flow.phase(), Phase::Working(stage));
        }
    }

    #[test]
    fn a_failure_ends_the_flow_and_nothing_reopens_it() {
        let t0 = Instant::now();
        let mut flow = flow();
        flow.begin();
        flow.failed("The download failed.".into());
        assert_eq!(*flow.phase(), Phase::Failed("The download failed.".into()));

        // Everything the worker could still send is refused.
        flow.stage(Stage::Installing);
        flow.stopped();
        flow.finished(Applied::Restart, t0);
        assert_eq!(*flow.phase(), Phase::Failed("The download failed.".into()));
        assert!(!flow.holds_work(), "and the dialog may be closed");
    }

    #[test]
    fn try_again_is_offered_only_where_nothing_was_installed() {
        let t0 = Instant::now();
        // After a stop, and after a failure: both left the installation as it
        // was, so starting over is exactly what the user means.
        for end in [Phase::Stopped, Phase::Failed("no route to host".into())] {
            let mut flow = flow();
            flow.begin();
            match &end {
                Phase::Stopped => {
                    flow.request_stop();
                    flow.stopped();
                }
                _ => flow.failed("no route to host".into()),
            }
            assert_eq!(*flow.phase(), end);
            assert!(flow.reoffer());
            assert_eq!(*flow.phase(), Phase::Offer);
            assert!(flow.begin(), "and it can be started again");
        }

        // Never from a screen where a download is running or the release is
        // already in place.
        let mut flow = flow();
        assert!(!flow.reoffer(), "the offer is already the offer");
        flow.begin();
        assert!(!flow.reoffer(), "a second download must not be startable");
        flow.finished(Applied::Restart, t0);
        assert!(
            !flow.reoffer(),
            "reinstalling what was just installed is not what Try again means",
        );
    }

    #[test]
    fn the_dialog_may_not_be_closed_while_a_download_is_running() {
        let t0 = Instant::now();
        let mut flow = flow();
        assert!(!flow.holds_work(), "the offer is dismissable");
        flow.begin();
        assert!(flow.holds_work());
        flow.request_stop();
        assert!(
            flow.holds_work(),
            "a stop that has not landed is still work"
        );
        flow.finished(Applied::Restart, t0);
        assert!(!flow.holds_work());
    }

    #[test]
    fn the_bar_never_goes_backwards_along_the_stages() {
        let stages = [
            Stage::Contacting,
            Stage::Downloading {
                received: 0,
                total: 100,
            },
            Stage::Downloading {
                received: 50,
                total: 100,
            },
            Stage::Downloading {
                received: 100,
                total: 100,
            },
            Stage::CheckingLength,
            Stage::Unpacking,
            Stage::Installing,
        ];
        let mut previous = 0.0_f32;
        for stage in stages {
            let p = stage.progress().expect("a real figure");
            assert!((0.0..=1.0).contains(&p), "{stage:?} reports {p}");
            assert!(p >= previous, "{stage:?} went backwards: {p} < {previous}");
            previous = p;
        }
        assert!(!Stage::Contacting.label().is_empty());
    }

    #[test]
    fn a_release_with_no_recorded_length_gets_no_percentage() {
        // The bar is drawn as an empty track rather than as a guess, and the
        // line says how much has arrived without claiming to know how much is
        // coming.
        let stage = Stage::Downloading {
            received: 1_048_576,
            total: 0,
        };
        assert_eq!(stage.progress(), None);
        assert!(stage.label().contains("so far"), "{}", stage.label());
        assert!(!stage.label().contains('%'));
    }

    #[test]
    fn the_download_line_carries_real_figures() {
        let stage = Stage::Downloading {
            received: 5 * 1024 * 1024,
            total: 20 * 1024 * 1024,
        };
        let label = stage.label();
        assert!(label.contains("5.0 MB"), "{label}");
        assert!(label.contains("20 MB"), "{label}");
        assert!(label.contains("25%"), "{label}");
    }

    #[test]
    fn no_stage_calls_anything_verified() {
        // Umber does not sign its releases. Every guarantee is about the
        // transport, and a word a reader would take for a signature check is
        // the dialog claiming one.
        for stage in [
            Stage::Contacting,
            Stage::Downloading {
                received: 1,
                total: 2,
            },
            Stage::CheckingLength,
            Stage::Unpacking,
            Stage::Installing,
            Stage::HandingOver,
        ] {
            let label = stage.label().to_lowercase();
            for word in ["verif", "authentic", "secure", "signed", "signature"] {
                assert!(!label.contains(word), "{stage:?} says {label:?}");
            }
        }
    }

    // --- which actions belong on the offer ---------------------------------

    #[test]
    fn only_an_installation_umber_owns_is_offered_update_now() {
        for kind in [
            InstallKind::Portable,
            InstallKind::Msi,
            InstallKind::AppImage(PathBuf::from("/home/a/Umber.AppImage")),
        ] {
            let a = actions(&kind, &offered(), Some(Arch::X86_64), true);
            assert!(a.update_now, "{kind:?}");
            assert!(!a.open_page, "{kind:?}");
            assert_eq!(a.obstacle, None, "{kind:?}");
        }
    }

    #[test]
    fn a_package_managers_copy_is_never_offered_update_now() {
        // Even with a matching asset in hand: that button is the first half of
        // writing over files a manager keeps a record of.
        for manager in [
            Manager::Flatpak,
            Manager::Dpkg { archive: true },
            Manager::Dpkg { archive: false },
            Manager::Rpm { archive: true },
            Manager::Rpm { archive: false },
            Manager::Pacman,
            Manager::Unknown,
        ] {
            let a = actions(
                &InstallKind::Managed(manager),
                &offered(),
                Some(Arch::X86_64),
                true,
            );
            assert!(!a.update_now, "{manager:?}");
            assert!(a.open_page, "{manager:?}");
            let obstacle = a.obstacle.expect("a managed copy says why");
            assert!(!obstacle.is_empty(), "{manager:?}");
        }
    }

    #[test]
    fn an_unrecognised_installation_is_told_where_the_build_is() {
        let a = actions(&InstallKind::Unknown, &offered(), Some(Arch::X86_64), true);
        assert!(!a.update_now);
        assert!(a.open_page);
        assert!(a.obstacle.is_some());
    }

    #[test]
    fn a_release_with_no_build_for_this_machine_says_so_rather_than_nothing() {
        // Umber owns this copy and would happily replace it; the release simply
        // published nothing for this architecture. Different sentence, same
        // destination.
        let a = actions(
            &InstallKind::Portable,
            &offered(),
            Some(Arch::X86_64),
            false,
        );
        assert!(!a.update_now);
        assert!(a.open_page);
        assert!(a.no_build);
        assert_eq!(a.obstacle, None, "there is no obstacle — there is no file");
    }
}
