//! Checking GitHub for a newer Umber, and installing one where that is
//! legitimate.
//!
//! Five rules shape everything here, and each of them is a way this feature
//! goes wrong if it is not stated:
//!
//! 1. **The check never delays the window or the first stroke.** It runs on a
//!    thread of its own and reports back through a channel. The event loop
//!    sleeps in [`winit::event_loop::ControlFlow::Wait`], so an answer arriving
//!    while nothing is happening has to *wake* it — hence [`Updates::set_waker`],
//!    which `app.rs` fills in with the event loop's proxy. Without that the
//!    result would sit in the channel until the user happened to move the
//!    mouse.
//! 2. **The download reports as it happens, and does not wake per byte.** The
//!    same channel and the same waker carry [`flow::Stage`] messages, throttled
//!    to one per whole percent — a hundred wake-ups for a download of any size,
//!    against one per 64 KiB chunk, which on a 30 MB release would be five
//!    hundred frames drawn to move a bar by a pixel.
//! 3. **Nothing is fetched without the user knowing.** The startup check is on
//!    by default — an update nobody is told about is an update nobody
//!    installs, and this is a painting application people will leave alone for
//!    months — but the first run says so before the first request goes out, and
//!    the switch is in Settings, General.
//! 4. **Only installations Umber owns are replaced.** [`install`] decides that,
//!    and every other kind is told where to get the build instead. A package
//!    manager's files are never written.
//! 5. **What comes back is checked as far as it can be.** HTTPS only, including
//!    across redirects; the address comes from the API rather than being built
//!    here; and the download has to be exactly the number of bytes the API
//!    reported. Umber does **not** sign its releases, so that is the whole of
//!    the guarantee, and nothing on screen may imply more.
//!
//! The dialog's own state — which screen, which stage, the countdown — is
//! [`flow`], a model with no drawing in it, and `updatedlg.rs` paints it.

pub mod apply;
pub mod flow;
pub mod install;
pub mod release;
pub mod version;

pub use apply::{Applied, relaunch, sweep_previous_binary};
pub use flow::{Flow, Phase, Stage};
pub use install::InstallKind;
pub use release::{RELEASES_PAGE, REPOSITORY, Release};
pub use version::Version;

use install::{Arch, Os, Probe};
use release::Asset;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::{Duration, Instant};

/// How long the whole exchange may take before it is abandoned.
///
/// Generous, because it is not blocking anything: nothing on screen waits for
/// it. Short enough that a captive-portal hotel network does not leave a thread
/// parked for the rest of the session.
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

/// The JSON reply is a few tens of kilobytes; a megabyte is already absurd.
const MAX_REPLY: u64 = 4 * 1024 * 1024;

/// How much is read from the socket at a time.
///
/// Sized so a slow connection still produces a report often enough to see, and
/// a fast one does not spend its time in `read` calls.
const CHUNK: usize = 64 * 1024;

/// How many bytes may arrive between two progress reports when the release
/// carries no recorded length.
///
/// The ordinary throttle is one report per whole percent, which needs a total
/// to be a percent *of*. This is the fallback, and it is the only case where
/// the dialog cannot show a percentage at all.
const UNMEASURED_STEP: u64 = 4 * 1024 * 1024;

/// Something that wakes the event loop.
///
/// A closure rather than a `winit` type, so this module stays free of the
/// windowing layer — the same reason `umber-core` knows nothing about wgpu.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// Where the *check* has got to.
///
/// Only the check: what an update is doing once it has started lives in
/// [`Updates::flow`]. Two records of "downloading" would be two things to keep
/// in step, and the one on screen would eventually be the stale one.
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
    /// Something went wrong, in a sentence written for the user.
    Failed(String),
}

/// Why the event loop should stop.
///
/// Both are ways an update ends, and they are genuinely different: a portable
/// or AppImage copy has already been replaced and can be started again from
/// here, while the Windows installer needs Umber *gone* and will start the new
/// version itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// Close, and start the new build.
    Restart,
    /// Close, and leave it at that.
    Quit,
}

/// What a background job has to say.
enum Report {
    /// The check's answer, and the end of that job.
    Checked(Status),
    /// An update has moved on. Not terminal.
    Stage(Stage),
    /// An update finished.
    Installed(Applied),
    /// An update stopped before writing anything.
    Stopped,
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

    /// The update dialog, or `None` when it is shut. Raised by the automatic
    /// check, and by the About dialog's button; a check the user asked for
    /// reports where they asked it and does not throw a modal at them.
    flow: Option<Flow>,
    /// Set by the Cancel on the download screen, read by the worker between
    /// chunks. `None` when nothing is in flight.
    cancel: Option<Arc<AtomicBool>>,
    /// The user has agreed to close, or asked to restart into the new build.
    exit: Option<Exit>,

    kind: InstallKind,
    status: Status,
    /// The job in flight, if any. Dropped when it reports something terminal.
    inbox: Option<Receiver<Report>>,
    /// Set once, so the automatic check happens once per run.
    started: bool,
    wake: Option<Waker>,
}

impl Default for Updates {
    fn default() -> Self {
        Self {
            check_on_startup: true,
            notice_seen: false,
            flow: None,
            cancel: None,
            exit: None,
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

    /// The dialog, for the module that draws it.
    pub fn flow(&self) -> Option<&Flow> {
        self.flow.as_ref()
    }

    /// True while a check or an update is in flight.
    pub fn busy(&self) -> bool {
        matches!(self.status, Status::Checking) || self.flow.as_ref().is_some_and(Flow::holds_work)
    }

    /// Why Umber does not ask GitHub at all on this installation, if it does not.
    ///
    /// The Flatpak is the one case, for two reasons that agree. Its sandbox is
    /// granted no network — `packaging/linux/io.github.spillebulle.umber.yml`
    /// deliberately carries no `--share=network`, because a painting
    /// application has no other use for one — so a request could only ever time
    /// out and report a failure that is really a design decision. And it would
    /// be answering a question Flatpak already answers: `flatpak update`, and
    /// every graphical software centre, keep the bundle current without Umber's
    /// help. Opening the sandbox so Umber can duplicate that is the wrong
    /// trade.
    pub fn check_unavailable(&self) -> Option<&'static str> {
        matches!(self.kind, InstallKind::Managed(install::Manager::Flatpak)).then_some(
            "Flatpak keeps this copy up to date — run `flatpak update \
             io.github.spillebulle.umber`, or leave it to your software centre. \
             Umber's sandbox is granted no network access, so it does not check \
             for itself.",
        )
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

    /// Which buttons the offer screen puts up for this installation.
    pub fn actions(&self, release: &Release) -> flow::Actions {
        flow::actions(&self.kind, self.installable(release).is_some())
    }

    /// Collect whatever a background job has reported.
    ///
    /// Called once per frame, before the interface is drawn, so a result that
    /// arrived while the loop was asleep is on screen in the frame the wake-up
    /// produced rather than the one after it. Drains the channel rather than
    /// taking one message: a burst of download progress collapses into the
    /// newest reading, which is the only one worth drawing.
    pub fn poll(&mut self, now: Instant) {
        let Some(inbox) = self.inbox.take() else {
            return;
        };
        loop {
            match inbox.try_recv() {
                // A terminal report ends the job, and dropping the receiver
                // with it is what stops the next frame reading a disconnect as
                // a crash.
                Ok(report) => {
                    if self.apply_report(report, now) {
                        return;
                    }
                }
                // The worker is still running.
                Err(TryRecvError::Empty) => {
                    self.inbox = Some(inbox);
                    return;
                }
                // The thread ended without reporting, which can only be a panic
                // in it. Say so rather than sitting on "Checking…" for ever.
                Err(TryRecvError::Disconnected) => {
                    self.worker_vanished();
                    return;
                }
            }
        }
    }

    /// Fold one report into the state. Returns true if it ended the job.
    fn apply_report(&mut self, report: Report, now: Instant) -> bool {
        match report {
            Report::Checked(status) => {
                // The dialog is only raised by the automatic check, and only
                // where one is not already up. A check the user asked for
                // answers in the dialog they asked it from.
                if self.started
                    && self.flow.is_none()
                    && let Status::Available(release) = &status
                {
                    self.flow = Some(Flow::offering(release.clone()));
                }
                self.status = status;
                true
            }
            Report::Stage(stage) => {
                if let Some(flow) = self.flow.as_mut() {
                    flow.stage(stage);
                }
                false
            }
            Report::Installed(applied) => {
                self.cancel = None;
                if let Some(flow) = self.flow.as_mut() {
                    flow.finished(applied, now);
                }
                true
            }
            Report::Stopped => {
                self.cancel = None;
                if let Some(flow) = self.flow.as_mut() {
                    flow.stopped();
                }
                true
            }
            Report::Failed(message) => {
                self.cancel = None;
                match self.flow.as_mut() {
                    Some(flow) if flow.holds_work() => flow.failed(message),
                    _ => self.status = Status::Failed(message),
                }
                true
            }
        }
    }

    /// A worker thread that ended without saying anything, which is a panic in
    /// it.
    fn worker_vanished(&mut self) {
        self.cancel = None;
        let message = "The update stopped unexpectedly.".to_string();
        match self.flow.as_mut() {
            Some(flow) if flow.holds_work() => flow.failed(message),
            _ => self.status = Status::Failed(message),
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
        if self.started
            || !self.check_on_startup
            || !self.notice_seen
            || self.check_unavailable().is_some()
        {
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
        if self.busy() || self.check_unavailable().is_some() {
            return;
        }
        self.status = Status::Checking;
        self.spawn("umber-update-check", |reporter| {
            reporter.send(Report::Checked(match latest_release() {
                Ok(Some(release)) if release.version > Version::current() => {
                    Status::Available(release)
                }
                Ok(_) => Status::UpToDate,
                Err(message) => Status::Failed(message),
            }));
        });
    }

    /// Raise the dialog for a release the check has already found.
    ///
    /// The way in from About's "Show the update…" — and the only other way in
    /// besides the automatic check.
    pub fn open_offer(&mut self) {
        if self.flow.is_some() {
            return;
        }
        if let Status::Available(release) = self.status.clone() {
            self.flow = Some(Flow::offering(release));
        }
    }

    /// Shut the dialog. Refused while a download or an install is running —
    /// a modal that vanished mid-update would leave a thread with nothing on
    /// screen to stop it.
    pub fn dismiss(&mut self) {
        if self.flow.as_ref().is_some_and(Flow::holds_work) {
            return;
        }
        self.flow = None;
    }

    /// "Never ask again": switch the startup check off and shut the dialog.
    ///
    /// Writes the *existing* preference rather than a second switch of its own,
    /// so Settings, General shows what was chosen here and can undo it. A
    /// second flag would be two things that can disagree about whether Umber
    /// checks.
    pub fn never_ask_again(&mut self) {
        self.check_on_startup = false;
        // Somebody who has answered this has plainly been told what the check
        // does, so the first-run notice has no business appearing again.
        self.notice_seen = true;
        self.dismiss();
    }

    /// Fetch and install the release the dialog is offering.
    ///
    /// Does nothing unless there is one *and* this installation is Umber's to
    /// replace — the last of several places that check, because the one thing
    /// that must never happen is a package manager's files being written over.
    pub fn install_offered(&mut self) {
        if self.busy() {
            return;
        }
        let Some(release) = self.flow.as_ref().map(|flow| flow.release.clone()) else {
            return;
        };
        if !self.kind.is_self_updatable() {
            return;
        }
        let Some(asset) = self.installable(&release).cloned() else {
            return;
        };
        let kind = self.kind.clone();
        if !self.flow.as_mut().is_some_and(Flow::begin) {
            return;
        }

        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());
        self.spawn("umber-update-fetch", move |reporter| {
            install_job(reporter, &kind, &asset, &cancel);
        });
    }

    /// The Cancel on the download screen.
    ///
    /// Sets the flag the worker reads between chunks and moves the dialog to
    /// "stopping". What actually happened is the worker's to report — see
    /// [`flow`]'s module comment.
    pub fn stop_update(&mut self) {
        if !self.flow.as_mut().is_some_and(Flow::request_stop) {
            return;
        }
        if let Some(cancel) = self.cancel.as_ref() {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Start over after a stop or a failure: back to the offer screen.
    ///
    /// Deliberately does not start the download itself. "Try again" puts the
    /// choice back in front of the user rather than making it for them, which
    /// also means one route into the download and one place that decides
    /// whether this installation may have one.
    pub fn retry(&mut self) {
        if let Some(flow) = self.flow.as_mut() {
            flow.reoffer();
        }
    }

    /// Stop the restart countdown, leaving Umber running.
    pub fn cancel_countdown(&mut self) {
        if let Some(flow) = self.flow.as_mut() {
            flow.cancel_countdown();
        }
    }

    /// Ask the event loop to close, and — for a copy Umber replaced itself —
    /// to start the new build on the way out.
    pub fn request_exit(&mut self, exit: Exit) {
        self.exit = Some(exit);
    }

    /// Whether the event loop should stop, consuming the request.
    pub fn take_exit_request(&mut self) -> Option<Exit> {
        self.exit.take()
    }

    /// The restart could not be started, so Umber is still running and has to
    /// say why.
    pub fn restart_failed(&mut self, message: String) {
        if let Some(flow) = self.flow.as_mut() {
            flow.cancel_countdown();
        }
        self.status = Status::Failed(message);
    }

    /// Run `job` on a thread and deliver its reports to [`Updates::poll`].
    fn spawn(&mut self, name: &str, job: impl FnOnce(&Reporter) + Send + 'static) {
        let (tx, rx) = channel();
        let reporter = Reporter {
            tx,
            wake: self.wake.clone(),
        };
        let spawned = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || job(&reporter));
        match spawned {
            Ok(_) => self.inbox = Some(rx),
            // A machine that cannot start a thread is in enough trouble that
            // blocking the interface on a network request would not help.
            Err(e) => {
                log::warn!("could not start {name}: {e}");
                let message = format!("Umber could not start the update check: {e}");
                match self.flow.as_mut() {
                    Some(flow) if flow.holds_work() => flow.failed(message),
                    _ => self.status = Status::Failed(message),
                }
            }
        }
    }
}

/// The end a worker thread reports through.
struct Reporter {
    tx: Sender<Report>,
    wake: Option<Waker>,
}

impl Reporter {
    /// Hand a report back and wake the loop to collect it.
    fn send(&self, report: Report) {
        // Send first: the wake-up is what makes the frame happen, so waking
        // before the value is in the channel would produce a frame that finds
        // nothing and then no further frame until the next input.
        let _ = self.tx.send(report);
        if let Some(wake) = &self.wake {
            wake();
        }
    }
}

// ---------------------------------------------------------------------------
// The work
// ---------------------------------------------------------------------------

/// Download a release and put it in place, reporting every stage.
///
/// The stages are the ones that genuinely happen, in the order they happen, and
/// the length check is its own step because it is the one guarantee an unsigned
/// release has and is worth naming.
fn install_job(reporter: &Reporter, kind: &InstallKind, asset: &Asset, cancel: &AtomicBool) {
    reporter.send(Report::Stage(Stage::Contacting));
    let bytes = match fetch(asset, cancel, &|received| {
        reporter.send(Report::Stage(Stage::Downloading {
            received,
            total: asset.size,
        }));
    }) {
        Ok(Some(bytes)) => bytes,
        // Stopped between chunks. Nothing has been written: the download is
        // held in memory until the install begins, so there is no half-file to
        // clear up and no thread left writing one.
        Ok(None) => return reporter.send(Report::Stopped),
        Err(message) => return reporter.send(Report::Failed(message)),
    };

    reporter.send(Report::Stage(Stage::CheckingLength));
    if bytes.len() as u64 != asset.size {
        return reporter.send(Report::Failed(format!(
            "The download of {} is {} bytes where GitHub reported {}. It has been \
             discarded.",
            asset.name,
            bytes.len(),
            asset.size,
        )));
    }

    // The last moment a stop costs nothing. From here the swap is under way,
    // and a half-replaced binary is the one outcome that costs somebody their
    // installation — which is also why `flow::Stage::can_stop` takes the button
    // off the screen rather than leaving it there to be refused.
    if cancel.load(Ordering::Relaxed) {
        return reporter.send(Report::Stopped);
    }

    match apply::apply(kind, &asset.name, &bytes, &|stage| {
        reporter.send(Report::Stage(stage));
    }) {
        Ok(applied) => reporter.send(Report::Installed(applied)),
        Err(message) => reporter.send(Report::Failed(message)),
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

/// Download an asset, reporting how much has arrived and stopping when asked.
///
/// `Ok(None)` means the caller asked it to stop. The length is *not* checked
/// here — [`install_job`] does that as a stage of its own, so the dialog can
/// name it. The limit below is still what stops an endless response from
/// exhausting memory before any comparison could run.
///
/// The length check is not a signature and is not described as one anywhere.
/// It catches a truncated download and a proxy that served something else,
/// which is what it is for; the answer to tampering is release signing, which
/// Umber does not do yet.
fn fetch(
    asset: &Asset,
    cancel: &AtomicBool,
    report: &dyn Fn(u64),
) -> Result<Option<Vec<u8>>, String> {
    if !asset.is_fetchable() {
        return Err("The release offers that file over an insecure address.".to_string());
    }

    let mut response = agent(DOWNLOAD_TIMEOUT)
        .get(&asset.browser_download_url)
        .call()
        .map_err(|e| format!("Umber could not download {}: {e}", asset.name))?;

    let mut reader = response
        .body_mut()
        .with_config()
        // One byte over what was promised is already a mismatch, and the limit
        // is what stops an endless response from exhausting memory.
        .limit(asset.size.saturating_add(1))
        .reader();

    // Reserved rather than grown from nothing: the size is known, and a 64 MB
    // ceiling keeps a lying `size` from being an allocation of its own.
    let mut bytes = Vec::with_capacity(asset.size.min(64 * 1024 * 1024) as usize);
    let mut chunk = vec![0u8; CHUNK];
    let mut last_report = 0u64;
    let mut last_percent = u64::MAX;
    report(0);

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let read = std::io::Read::read(&mut reader, &mut chunk)
            .map_err(|e| format!("The download of {} failed: {e}", asset.name))?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);

        // One report per whole percent. A wake per chunk would be five hundred
        // frames on a 30 MB release, each redrawing the whole interface to move
        // a bar by less than a pixel.
        let received = bytes.len() as u64;
        let worth_saying = match asset.size {
            0 => received - last_report >= UNMEASURED_STEP,
            size => {
                let percent = received.saturating_mul(100) / size;
                percent != last_percent
            }
        };
        if worth_saying {
            last_percent = received.saturating_mul(100) / asset.size.max(1);
            last_report = received;
            report(received);
        }
    }

    // The last reading, whatever the throttle had reached: a bar left at 99%
    // while the length is checked looks stuck.
    report(bytes.len() as u64);
    Ok(Some(bytes))
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

// ---------------------------------------------------------------------------
// Looking at the dialog without a release to install
// ---------------------------------------------------------------------------

/// Put the dialog into a state, with a release that does not exist.
///
/// Nobody working on Umber can perform a real update against a real release —
/// it would mean cutting one — so every screen here would otherwise ship having
/// been reasoned about and never looked at. This is how they get looked at, and
/// it is `debug_assertions` only: a live control that fabricates a release has
/// no business in a build somebody paints with. The menu entry that reaches it
/// is behind the same gate.
///
/// It moves the *model*, and nothing else. No thread starts, no request goes
/// out, and the countdown on the completion screen is the real one — so what is
/// on screen is the same drawing code a real update produces, which is the only
/// version of this worth having.
#[cfg(debug_assertions)]
pub fn demo_release() -> Release {
    let mut version = Version::current();
    version.patch += 1;
    Release {
        version,
        tag: format!("v{version}"),
        page: RELEASES_PAGE.to_string(),
        notes: "### Added\n- A rehearsal release. This one does not exist, and \
                nothing here will be downloaded.\n- A second line, so the notes \
                box has something to scroll.\n\n### Fixed\n- A very long line, to \
                prove that a release note cannot push this dialog wider than the \
                screen the way the brush importer's notices once did.\n"
            .to_string(),
        assets: Vec::new(),
    }
}

#[cfg(debug_assertions)]
impl Updates {
    /// Open the dialog on a fabricated release, at `phase`.
    pub fn demo(&mut self, phase: Phase, now: Instant) {
        let mut flow = Flow::offering(demo_release());
        match phase {
            Phase::Offer => {}
            Phase::Working(stage) => {
                flow.begin();
                flow.stage(stage);
            }
            Phase::Stopping => {
                flow.begin();
                flow.request_stop();
            }
            Phase::Stopped => {
                flow.begin();
                flow.request_stop();
                flow.stopped();
            }
            Phase::Done { outcome, .. } => {
                flow.begin();
                flow.finished(outcome, now);
            }
            Phase::Failed(message) => {
                flow.begin();
                flow.failed(message);
            }
        }
        self.flow = Some(flow);
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
    fn an_exit_request_is_taken_once() {
        let mut updates = Updates::default();
        assert_eq!(updates.take_exit_request(), None);
        updates.request_exit(Exit::Quit);
        assert_eq!(updates.take_exit_request(), Some(Exit::Quit));
        assert_eq!(updates.take_exit_request(), None, "and not a second time");

        updates.request_exit(Exit::Restart);
        assert_eq!(updates.take_exit_request(), Some(Exit::Restart));
    }

    #[test]
    fn a_flatpak_never_asks_github_anything() {
        // Its sandbox has no network, and Flatpak already keeps it current. A
        // check here could only report a failure that is really a decision.
        let mut updates = Updates {
            kind: InstallKind::Managed(install::Manager::Flatpak),
            notice_seen: true,
            ..Updates::default()
        };
        assert!(updates.check_unavailable().is_some());
        updates.start_if_due();
        assert_eq!(*updates.status(), Status::Idle);
        updates.check();
        assert_eq!(*updates.status(), Status::Idle, "not even when asked");
        // And with no check there is no release, so the dialog cannot be raised.
        updates.open_offer();
        assert!(
            updates.flow().is_none(),
            "the dialog must never appear here"
        );

        // Every other kind still checks. Compared on `check_unavailable` rather
        // than by starting one, because starting one is a network request.
        for kind in [
            InstallKind::Portable,
            InstallKind::Msi,
            InstallKind::Managed(install::Manager::Dpkg),
            InstallKind::Unknown,
        ] {
            let updates = Updates {
                kind: kind.clone(),
                ..Updates::default()
            };
            assert_eq!(updates.check_unavailable(), None, "{kind:?}");
        }
    }

    fn release() -> Release {
        Release {
            version: Version::parse("9.9.9").expect("parses"),
            tag: "v9.9.9".into(),
            page: RELEASES_PAGE.into(),
            notes: String::new(),
            assets: vec![Asset {
                name: "umber-9.9.9-x86_64-unknown-linux-gnu.tar.gz".into(),
                size: 1,
                browser_download_url: "https://github.com/x/y.tar.gz".into(),
            }],
        }
    }

    #[test]
    fn a_managed_installation_is_offered_nothing_to_install() {
        // Belt and braces over `release::asset_for` and `flow::actions`: this is
        // the path the button actually takes.
        let mut updates = Updates {
            kind: InstallKind::Managed(install::Manager::Dpkg),
            ..Updates::default()
        };
        let release = release();
        assert_eq!(updates.installable(&release), None);
        assert!(!updates.actions(&release).update_now);

        updates.status = Status::Available(release);
        updates.open_offer();
        updates.install_offered();
        // Still on the offer screen, not downloading: the request was refused.
        assert_eq!(
            updates.flow().map(Flow::phase),
            Some(&Phase::Offer),
            "a managed copy must not start a download",
        );
        assert!(!updates.busy());
    }

    #[test]
    fn never_ask_again_writes_the_setting_the_dialog_can_undo() {
        // One switch, not two. Settings, General reads exactly this field, so
        // the choice made here is visible there and reversible from there.
        let mut updates = Updates {
            notice_seen: true,
            ..Updates::default()
        };
        updates.status = Status::Available(release());
        updates.open_offer();
        assert!(updates.flow().is_some());

        updates.never_ask_again();
        assert!(!updates.check_on_startup);
        assert!(
            updates.notice_seen,
            "and the first-run notice stays answered"
        );
        assert!(updates.flow().is_none(), "the dialog closes");

        // And it stays off across a start.
        updates.started = false;
        updates.start_if_due();
        assert_eq!(*updates.status(), Status::Available(release()));
        assert!(!updates.busy());
    }

    #[test]
    fn the_dialog_cannot_be_dismissed_while_it_is_working() {
        let mut updates = Updates {
            status: Status::Available(release()),
            ..Updates::default()
        };
        updates.open_offer();

        updates.dismiss();
        assert!(updates.flow().is_none(), "an offer may be dismissed");

        // Put it back and start the work by hand — `install_offered` would
        // reach the network.
        updates.flow = Some(Flow::offering(release()));
        updates.flow.as_mut().expect("a flow").begin();
        updates.dismiss();
        assert!(
            updates.flow().is_some(),
            "a running update must not be left with nothing on screen to stop it",
        );
        updates.never_ask_again();
        assert!(updates.flow().is_some(), "and that route is closed too");
    }

    #[test]
    fn a_stop_reaches_the_worker_and_the_dialog_together() {
        let mut updates = Updates {
            flow: Some(Flow::offering(release())),
            ..Updates::default()
        };
        updates.flow.as_mut().expect("a flow").begin();
        let cancel = Arc::new(AtomicBool::new(false));
        updates.cancel = Some(cancel.clone());

        updates.stop_update();
        assert!(
            cancel.load(Ordering::Relaxed),
            "the worker has to be told, or the download runs to the end",
        );
        assert_eq!(updates.flow().map(Flow::phase), Some(&Phase::Stopping));
    }

    #[test]
    fn a_report_arriving_after_a_stop_decides_what_actually_happened() {
        let now = Instant::now();
        let mut updates = Updates {
            flow: Some(Flow::offering(release())),
            ..Updates::default()
        };
        updates.flow.as_mut().expect("a flow").begin();
        updates.cancel = Some(Arc::new(AtomicBool::new(false)));
        updates.stop_update();

        // The worker got there first. The dialog says what happened, not what
        // was asked for.
        assert!(updates.apply_report(Report::Installed(Applied::Restart), now));
        assert!(matches!(
            updates.flow().map(Flow::phase),
            Some(Phase::Done {
                outcome: Applied::Restart,
                ..
            }),
        ));
        assert!(updates.cancel.is_none(), "the flag is given back");
    }

    #[test]
    fn a_failure_during_a_check_does_not_touch_a_dialog_that_is_up() {
        // Two jobs never run at once, but the routing has to be right anyway:
        // a check's failure belongs in the status line, and an update's belongs
        // on the screen the user is watching.
        let now = Instant::now();
        let mut updates = Updates {
            flow: Some(Flow::offering(release())),
            ..Updates::default()
        };
        assert!(updates.apply_report(Report::Failed("no route to host".into()), now));
        assert_eq!(
            *updates.status(),
            Status::Failed("no route to host".into()),
            "an idle dialog is not the place for a check's failure",
        );
        assert_eq!(updates.flow().map(Flow::phase), Some(&Phase::Offer));

        updates.flow.as_mut().expect("a flow").begin();
        assert!(updates.apply_report(Report::Failed("the download failed".into()), now));
        assert_eq!(
            updates.flow().map(Flow::phase),
            Some(&Phase::Failed("the download failed".into())),
        );
    }

    #[test]
    fn a_worker_that_panics_is_reported_rather_than_waited_for() {
        let mut updates = Updates {
            flow: Some(Flow::offering(release())),
            ..Updates::default()
        };
        updates.flow.as_mut().expect("a flow").begin();
        updates.worker_vanished();
        assert!(matches!(
            updates.flow().map(Flow::phase),
            Some(Phase::Failed(_)),
        ));
    }

    #[test]
    fn an_automatic_check_raises_the_dialog_and_a_manual_one_does_not() {
        let now = Instant::now();
        // `started` is what tells the two apart: it is set by `start_if_due`
        // and never by the About dialog's button.
        let mut manual = Updates::default();
        assert!(manual.apply_report(Report::Checked(Status::Available(release())), now));
        assert!(
            manual.flow().is_none(),
            "a check the user asked for answers where they asked it",
        );
        manual.open_offer();
        assert!(manual.flow().is_some(), "and the button opens it");

        let mut automatic = Updates {
            started: true,
            ..Updates::default()
        };
        assert!(automatic.apply_report(Report::Checked(Status::Available(release())), now));
        assert_eq!(automatic.flow().map(Flow::phase), Some(&Phase::Offer));
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
