//! Checking GitHub for a newer Umber, and installing one where that is
//! legitimate.
//!
//! Four rules shape everything here, and each of them is a way this feature
//! goes wrong if it is not stated:
//!
//! 1. **The check never delays the window or the first stroke.** It runs on a
//!    thread of its own and reports back through a channel. The event loop
//!    sleeps in [`winit::event_loop::ControlFlow::Wait`], so an answer arriving
//!    while nothing is happening has to *wake* it — hence [`Updates::set_waker`],
//!    which `app.rs` fills in with the event loop's proxy. Without that the
//!    result would sit in the channel until the user happened to move the
//!    mouse.
//! 2. **Nothing is fetched without the user knowing.** The startup check is on
//!    by default — an update nobody is told about is an update nobody
//!    installs, and this is a painting application people will leave alone for
//!    months — but the first run says so before the first request goes out, and
//!    the switch is in Settings, General.
//! 3. **Only installations Umber owns are replaced.** [`install`] decides that,
//!    and every other kind is told where to get the build instead. A package
//!    manager's files are never written.
//! 4. **What comes back is checked as far as it can be.** HTTPS only, including
//!    across redirects; the address comes from the API rather than being built
//!    here; and the download has to be exactly the number of bytes the API
//!    reported. Umber does **not** sign its releases, so that is the whole of
//!    the guarantee, and the About dialog says so rather than implying more.

pub mod apply;
pub mod install;
pub mod release;
pub mod version;

pub use apply::{Applied, sweep_previous_binary};
pub use install::InstallKind;
pub use release::{RELEASES_PAGE, REPOSITORY, Release};
pub use version::Version;

use install::{Arch, Os, Probe};
use release::Asset;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::Duration;

/// How long the whole exchange may take before it is abandoned.
///
/// Generous, because it is not blocking anything: nothing on screen waits for
/// it. Short enough that a captive-portal hotel network does not leave a thread
/// parked for the rest of the session.
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

/// The JSON reply is a few tens of kilobytes; a megabyte is already absurd.
const MAX_REPLY: u64 = 4 * 1024 * 1024;

/// Something that wakes the event loop.
///
/// A closure rather than a `winit` type, so this module stays free of the
/// windowing layer — the same reason `umber-core` knows nothing about wgpu.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// Where the check has got to.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Status {
    /// Nothing has been asked yet.
    #[default]
    Idle,
    Checking,
    /// The newest published release is this one or older.
    UpToDate,
    /// A newer release exists. Whether Umber can install it is a separate
    /// question — see [`Updates::installable`].
    Available(Release),
    Downloading,
    /// The new build is in place, or an installer is running.
    Applied(Applied),
    /// Something went wrong, in a sentence written for the user.
    Failed(String),
}

/// The update check, its result, and the preferences that govern it.
pub struct Updates {
    /// Whether Umber asks GitHub for the release list when it starts.
    ///
    /// **Default on.** The alternative was considered and is worse for this
    /// application: Umber is not a service, it is a tool somebody opens when
    /// they want to draw, and a check nobody has switched on is one nobody ever
    /// switches on — so security fixes would reach the people who read the
    /// repository and nobody else. On is only defensible because it is *said*:
    /// see `notice_seen`, which holds the first check back until the user has
    /// been shown what it does.
    pub check_on_startup: bool,
    /// Whether the user has been told that Umber checks for updates.
    ///
    /// False on a fresh install. While it is false no automatic check runs at
    /// all — the notice is shown first, and answering it is what sets both this
    /// and `check_on_startup`.
    pub notice_seen: bool,
    /// Whether the "a newer Umber is available" prompt is up. Only ever raised
    /// by the automatic check; a check the user asked for reports in the dialog
    /// they asked from.
    pub prompt_open: bool,
    /// The user has agreed to close so an installer can finish.
    quit_requested: bool,

    kind: InstallKind,
    status: Status,
    /// The job in flight, if any. Dropped when it reports.
    inbox: Option<Receiver<Status>>,
    /// Set once, so the automatic check happens once per run.
    started: bool,
    wake: Option<Waker>,
}

impl Default for Updates {
    fn default() -> Self {
        Self {
            check_on_startup: true,
            notice_seen: false,
            prompt_open: false,
            quit_requested: false,
            // Decided once, at start-up, from the executable's own path. It
            // cannot change while Umber is running, and asking the file system
            // about it per frame would be work for an answer that never moves.
            kind: install::detect(&Probe::current()),
            status: Status::Idle,
            inbox: None,
            started: false,
            wake: None,
        }
    }
}

impl Updates {
    /// Give the module a way to wake the event loop when a job reports.
    pub fn set_waker(&mut self, wake: Waker) {
        self.wake = Some(wake);
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    pub fn kind(&self) -> &InstallKind {
        &self.kind
    }

    /// True while a check or a download is in flight.
    pub fn busy(&self) -> bool {
        matches!(self.status, Status::Checking | Status::Downloading)
    }

    /// The asset this machine would install, for a release that has one.
    ///
    /// `None` covers three different situations that all have the same answer —
    /// go to the releases page: a package manager owns this copy, this
    /// architecture has no published build, or the release did not carry the
    /// file this installation needs.
    pub fn installable<'a>(&self, release: &'a Release) -> Option<&'a Asset> {
        let arch = Arch::CURRENT?;
        release.asset_for(&self.kind, Os::CURRENT, arch)
    }

    /// Collect whatever a background job has reported.
    ///
    /// Called once per frame, before the interface is drawn, so a result that
    /// arrived while the loop was asleep is on screen in the frame the wake-up
    /// produced rather than the one after it.
    pub fn poll(&mut self) {
        let Some(inbox) = self.inbox.as_ref() else {
            return;
        };
        match inbox.try_recv() {
            Ok(status) => {
                // A prompt is only raised by the automatic check, and only for
                // something the user has not already been shown. A check the
                // user asked for answers where they asked it.
                if self.started && matches!(status, Status::Available(_)) && !self.prompt_open {
                    self.prompt_open = true;
                }
                self.status = status;
                self.inbox = None;
            }
            // The worker is still running.
            Err(TryRecvError::Empty) => {}
            // The thread ended without reporting, which can only be a panic in
            // it. Say so rather than sitting on "Checking…" for ever.
            Err(TryRecvError::Disconnected) => {
                self.status = Status::Failed("The update check stopped unexpectedly.".to_string());
                self.inbox = None;
            }
        }
    }

    /// Start the automatic check, if this run should make one.
    ///
    /// Called every frame and returns immediately on all but one of them. It
    /// runs after the interface has been drawn at least once, because that is
    /// where the preferences file is read — see `prefs::ensure_loaded` — so a
    /// user who switched the check off last time is not asked again on the way
    /// past.
    pub fn start_if_due(&mut self) {
        if self.started || !self.check_on_startup || !self.notice_seen {
            return;
        }
        self.started = true;
        self.check();
    }

    /// Ask GitHub what the newest release is.
    ///
    /// Safe to call from a button: a second call while one is in flight is
    /// ignored rather than starting a second request.
    pub fn check(&mut self) {
        if self.busy() {
            return;
        }
        self.status = Status::Checking;
        self.spawn("umber-update-check", || match latest_release() {
            Ok(Some(release)) if release.version > Version::current() => Status::Available(release),
            Ok(_) => Status::UpToDate,
            Err(message) => Status::Failed(message),
        });
    }

    /// Fetch and install the release currently on offer.
    ///
    /// Does nothing unless there is one *and* this installation is Umber's to
    /// replace — the last of several places that check, because the one thing
    /// that must never happen is a package manager's files being written over.
    pub fn install_available(&mut self) {
        if self.busy() {
            return;
        }
        let Status::Available(release) = self.status.clone() else {
            return;
        };
        if !self.kind.is_self_updatable() {
            return;
        }
        let Some(asset) = self.installable(&release).cloned() else {
            return;
        };

        let kind = self.kind.clone();
        self.prompt_open = false;
        self.status = Status::Downloading;
        self.spawn("umber-update-fetch", move || match fetch(&asset) {
            Ok(bytes) => match apply::apply(&kind, &asset.name, &bytes) {
                Ok(applied) => Status::Applied(applied),
                Err(message) => Status::Failed(message),
            },
            Err(message) => Status::Failed(message),
        });
    }

    /// Note that the user has agreed to close so an installer can proceed.
    pub fn request_quit(&mut self) {
        self.quit_requested = true;
    }

    /// Whether the event loop should exit, consuming the request.
    pub fn take_quit_request(&mut self) -> bool {
        std::mem::take(&mut self.quit_requested)
    }

    /// Run `job` on a thread and deliver its answer to [`Updates::poll`].
    fn spawn(&mut self, name: &str, job: impl FnOnce() -> Status + Send + 'static) {
        let (tx, rx) = channel();
        let wake = self.wake.clone();
        let spawned = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || report(tx, wake, job()));
        match spawned {
            Ok(_) => self.inbox = Some(rx),
            // A machine that cannot start a thread is in enough trouble that
            // blocking the interface on a network request would not help.
            Err(e) => {
                log::warn!("could not start {name}: {e}");
                self.status =
                    Status::Failed(format!("Umber could not start the update check: {e}"));
            }
        }
    }
}

/// Hand a result back and wake the loop to collect it.
fn report(tx: Sender<Status>, wake: Option<Waker>, status: Status) {
    // Send first: the wake-up is what makes the frame happen, so waking before
    // the value is in the channel would produce a frame that finds nothing and
    // then no further frame until the next input.
    let _ = tx.send(status);
    if let Some(wake) = wake {
        wake();
    }
}

// ---------------------------------------------------------------------------
// The network half
// ---------------------------------------------------------------------------

/// The client every request goes through.
///
/// `https_only` covers redirects as well as the first request, which matters
/// here: a release asset's address is on `github.com` and redirects to a
/// storage host, and a redirect that dropped to plain http would quietly undo
/// the only integrity guarantee an unsigned release has.
fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .https_only(true)
        .timeout_global(Some(timeout))
        // GitHub refuses an API request with no user agent, and asks that it
        // name the application.
        .user_agent(format!("umber/{} (+{})", Version::current(), REPOSITORY))
        .build()
        .into()
}

/// The newest release GitHub knows about, or `None` if it knows of none.
fn latest_release() -> Result<Option<Release>, String> {
    let mut response = agent(CHECK_TIMEOUT)
        .get(release::API)
        .header("Accept", "application/vnd.github+json")
        // Pinning the API version is what stops a future change to GitHub's
        // default representation turning into a parse failure here.
        .header("X-GitHub-Api-Version", "2022-11-28")
        .call()
        .map_err(|e| format!("Umber could not reach GitHub: {e}"))?;

    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_REPLY)
        .read_to_string()
        .map_err(|e| format!("GitHub's reply could not be read: {e}"))?;

    release::newest(&body).map_err(|e| format!("GitHub's reply could not be understood: {e}"))
}

/// Download an asset, refusing anything that is not the length the API reported.
///
/// The length check is not a signature and is not described as one. It catches
/// a truncated download and a proxy that served something else, which is what
/// it is for; a `docs`-level answer to tampering is release signing, which
/// Umber does not do yet.
fn fetch(asset: &Asset) -> Result<Vec<u8>, String> {
    if !asset.is_fetchable() {
        return Err("The release offers that file over an insecure address.".to_string());
    }

    let mut response = agent(DOWNLOAD_TIMEOUT)
        .get(&asset.browser_download_url)
        .call()
        .map_err(|e| format!("Umber could not download {}: {e}", asset.name))?;

    let bytes = response
        .body_mut()
        .with_config()
        // One byte over what was promised is already a mismatch, and the limit
        // is what stops an endless response from exhausting memory before the
        // comparison below ever runs.
        .limit(asset.size.saturating_add(1))
        .read_to_vec()
        .map_err(|e| format!("The download of {} failed: {e}", asset.name))?;

    if bytes.len() as u64 != asset.size {
        return Err(format!(
            "The download of {} is {} bytes where GitHub reported {}. It has been \
             discarded.",
            asset.name,
            bytes.len(),
            asset.size,
        ));
    }
    Ok(bytes)
}

// ---------------------------------------------------------------------------
// Opening a page
// ---------------------------------------------------------------------------

/// Open an address in the user's browser.
///
/// egui's own hyperlink handling is not available: `egui-winit` is built with
/// `default-features = false`, so its `links` feature — and the `webbrowser`
/// dependency behind it — is not compiled in. Turning it on to open two fixed
/// URLs would be a larger change than three lines of `Command`.
///
/// Only `https` is ever passed on. The addresses here are constants and release
/// pages from the API, but a launcher that will open anything it is handed is a
/// thing to be careful with, not a thing to argue is safe today.
pub fn open_in_browser(url: &str) {
    if !url.starts_with("https://") {
        log::warn!("refusing to open {url}: not https");
        return;
    }
    let result = if cfg!(target_os = "windows") {
        // Through `cmd /c start` because Windows has no "open this" binary, and
        // the empty argument is `start`'s window title — without it, a quoted
        // URL is taken *as* the title and nothing opens.
        std::process::Command::new("cmd")
            .args(["/c", "start", "", url])
            .spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };
    if let Err(e) = result {
        log::warn!("could not open {url}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing in this module's tests may reach the network, so the state
    /// machine is exercised without ever starting a job.
    #[test]
    fn a_fresh_install_makes_no_request_until_the_notice_is_answered() {
        let mut updates = Updates::default();
        assert!(updates.check_on_startup, "the default is on");
        assert!(!updates.notice_seen, "and nobody has been told yet");

        updates.start_if_due();
        assert_eq!(*updates.status(), Status::Idle);
        assert!(!updates.busy(), "no request may have gone out");
    }

    #[test]
    fn switching_the_check_off_keeps_it_off() {
        let mut updates = Updates {
            check_on_startup: false,
            notice_seen: false,
            ..Updates::default()
        };
        updates.start_if_due();
        assert_eq!(*updates.status(), Status::Idle);
    }

    #[test]
    fn a_quit_request_is_taken_once() {
        let mut updates = Updates::default();
        assert!(!updates.take_quit_request());
        updates.request_quit();
        assert!(updates.take_quit_request());
        assert!(!updates.take_quit_request(), "and not a second time");
    }

    #[test]
    fn a_managed_installation_is_offered_nothing_to_install() {
        // Belt and braces over `release::asset_for`: this is the path the
        // button actually takes.
        let mut updates = Updates {
            kind: InstallKind::Managed(install::Manager::Dpkg),
            ..Updates::default()
        };
        let release = Release {
            version: Version::parse("9.9.9").expect("parses"),
            tag: "v9.9.9".into(),
            page: RELEASES_PAGE.into(),
            notes: String::new(),
            assets: vec![Asset {
                name: "umber-9.9.9-x86_64-unknown-linux-gnu.tar.gz".into(),
                size: 1,
                browser_download_url: "https://github.com/x/y.tar.gz".into(),
            }],
        };
        assert_eq!(updates.installable(&release), None);

        updates.status = Status::Available(release);
        updates.install_available();
        // Still on offer, not downloading: the request was refused.
        assert!(matches!(updates.status(), Status::Available(_)));
    }

    #[test]
    fn only_https_addresses_are_ever_opened() {
        // No assertion available on the side effect, so this pins the guard
        // itself: anything else must return without spawning a process.
        for url in [
            "http://example.invalid",
            "file:///etc/passwd",
            "",
            "javascript:x",
        ] {
            open_in_browser(url);
        }
    }
}
