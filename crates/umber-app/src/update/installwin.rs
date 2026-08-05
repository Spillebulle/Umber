//! The update installer's window, and the worker under it.
//!
//! [`super::installer`] is the model — what the command line means, what
//! `msiexec` is asked, which step the bar is on — and this is the part that
//! opens a window and touches the operating system. The same division `dock.rs`
//! keeps against `panels.rs`, and here it is what makes any of this checkable:
//! nobody can cut a release to run the real thing against, so everything that
//! can be decided without one is decided over there.
//!
//! The window is [`crate::shell`]'s, shared with the crash reporter.

use super::installer::{Command, Job, Step, stage_helper};
use crate::shell::{self, Page};
use crate::theme::{Palette, text};
use crate::{controls, prefs, tabs, widgets};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::Duration;

/// The window, in logical points. Small: it says one thing.
const WINDOW: [f32; 2] = [440.0, 260.0];

/// How often the window looks for news from the worker.
///
/// Eight frames a second, which is enough for a label to change and few enough
/// that a window sitting through a two-minute install costs nothing worth
/// measuring. The bar it drives does not animate — see `Step::progress`.
const TICK: Duration = Duration::from_millis(125);

/// Start the helper for a package, from a copy of this executable that the
/// installer will not be replacing.
///
/// Called by [`super::apply`] on the MSI path, immediately before Umber exits.
/// Umber's own process id goes with it so the helper knows what to wait for.
pub fn spawn(package: &Path, version: &str) -> Result<(), String> {
    let dir = package.parent().unwrap_or_else(|| Path::new("."));
    let helper = stage_helper(dir)?;
    std::process::Command::new(&helper)
        .arg(super::installer::FLAG)
        .arg(package)
        .arg(std::process::id().to_string())
        .arg(version)
        // Where to start Umber from afterwards. This helper is a copy in the
        // temporary directory, so its own `current_exe` is the updater.
        .arg(std::env::current_exe().unwrap_or_default())
        .spawn()
        .map_err(|e| {
            format!(
                "Umber could not start the updater at {}: {e}",
                helper.display()
            )
        })?;
    Ok(())
}

/// Be the installer. Returns once the window has closed.
///
/// An update starts working immediately — it was asked for, and the artist is
/// looking at a countdown in the Umber that spawned this. Setup waits: it was
/// double-clicked by somebody who has not agreed to anything yet, so the window
/// opens on [`Step::Ready`] with an Install button.
pub fn show(mut job: Job) -> Result<(), Box<dyn std::error::Error>> {
    // Setup carries its package on its own end. Lifting it out here rather than
    // on the worker means a file that is not an installer says so at once,
    // instead of after a button has been pressed.
    if job.setup {
        match unpack_payload() {
            Ok((package, version)) => {
                job.package = package;
                if job.version.is_empty() {
                    job.version = version;
                }
            }
            Err(why) => return show_failed(job, why),
        }
    }

    let start = job.setup;
    let mut page = Installer {
        palette: {
            let prefs = prefs::load();
            crate::themelib::resolve(prefs.theme, prefs.accent, prefs.custom_theme.as_deref())
        },
        step: if start {
            Step::Ready
        } else {
            Step::WaitingForUmber
        },
        news: None,
        worker: None,
        log: None,
        job,
    };
    if !start {
        page.begin();
    }
    shell::run(&mut page)
}

/// A window that only reports why there is nothing to install.
///
/// Setup with no package on it, which is a build that went wrong or a file that
/// was truncated on the way down. Saying so in the same window is better than a
/// console message nobody sees behind a double-click.
fn show_failed(job: Job, why: String) -> Result<(), Box<dyn std::error::Error>> {
    let mut page = Installer {
        palette: {
            let prefs = prefs::load();
            crate::themelib::resolve(prefs.theme, prefs.accent, prefs.custom_theme.as_deref())
        },
        step: Step::Failed(why),
        news: None,
        worker: None,
        log: None,
        job,
    };
    shell::run(&mut page)
}

/// Lift the package off the end of this executable and write it down.
///
/// The version is taken from the package's own file name, which is what
/// `examples/make-setup.rs` puts there.
fn unpack_payload() -> Result<(std::path::PathBuf, String), String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("Umber could not read its own program file: {e}"))?;
    let bytes = std::fs::read(&exe)
        .map_err(|e| format!("Umber could not read its own program file: {e}"))?;
    let package = super::payload::read(&bytes).ok_or_else(|| {
        "This copy of Umber's installer carries no package. Take a fresh one          from the releases page."
            .to_string()
    })?;

    let dir = std::env::temp_dir();
    let name = exe
        .file_stem()
        .map(|s| format!("{}.msi", s.to_string_lossy()))
        .unwrap_or_else(|| "umber-setup.msi".to_string());
    let to = dir.join(name);
    std::fs::write(&to, package)
        .map_err(|e| format!("Umber could not write the package to {}: {e}", to.display()))?;

    // `umber-setup-0.0.8-x64` -> `0.0.8`, and nothing if it is not shaped like
    // that. Only ever displayed, so a miss costs a heading and not a install.
    let version = exe
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .and_then(|stem| {
            stem.split('-')
                .find(|part| {
                    part.split('.').count() == 3
                        && part.chars().all(|c| c.is_ascii_digit() || c == '.')
                })
                .map(|v| v.to_string())
        })
        .unwrap_or_default();
    Ok((to, version))
}

impl Installer {
    /// Start the work. Called once, either straight away for an update or from
    /// the Install button for setup.
    fn begin(&mut self) {
        let (tx, rx) = channel();
        self.news = Some(rx);
        let package = self.job.package.clone();
        let parent = self.job.parent;
        let target = self.job.target.clone();
        let setup = self.job.setup;
        // The work runs on a thread and the window draws: the alternative is a
        // window that stops answering for as long as `msiexec` takes, which on
        // a slow machine is the whole install. Same reason the check threads.
        self.worker = Some(std::thread::spawn(move || {
            // Setup has no Umber to wait for: it *is* the first one. An update
            // does, and it is the step that could hang, so it is named rather
            // than folded into the next.
            if !setup && let Some(pid) = parent {
                let _ = tx.send(Step::WaitingForUmber);
                wait_for(pid);
            }
            let _ = tx.send(Step::AskingPermission);
            let command = Command::for_package(&package);
            // The prompt is up until `install` reports the installer has started,
            // which is the moment the consent was given. Two steps rather than one
            // because they fail differently and look different: a prompt waiting on
            // somebody, and Windows working.
            let running = tx.clone();
            match install(&command, &move || {
                let _ = running.send(Step::Installing);
            }) {
                Ok(()) => {
                    let _ = tx.send(Step::Starting);
                    match start_installed(target.as_deref()) {
                        Ok(()) => {
                            let _ = tx.send(Step::Finished);
                        }
                        // The update *worked*; only the relaunch did not. Saying so
                        // is the honest reading, and it is the same guarantee
                        // `relaunch` makes: never leave somebody believing an
                        // update failed when their next start will be the new one.
                        Err(why) => {
                            let _ = tx.send(Step::Failed(format!(
                                "Umber was updated, but could not be started again: {why}\n\n\
                             Start it from the Start menu."
                            )));
                        }
                    }
                }
                Err(why) => {
                    let _ = tx.send(Step::Failed(why));
                }
            }
        }));
    }
}

struct Installer {
    job: Job,
    palette: Palette,
    step: Step,
    /// News from the worker, once there is a worker. `None` before setup has
    /// been told to start, and for a window that opened only to report why
    /// there is nothing to install.
    news: Option<Receiver<Step>>,
    /// Held so the thread is joined rather than detached when the window goes.
    worker: Option<std::thread::JoinHandle<()>>,
    /// The installer's log, once there is one worth naming. Held so the failure
    /// screen can offer it.
    log: Option<PathBuf>,
}

impl Page for Installer {
    fn title(&self) -> String {
        "Umber update".to_string()
    }

    fn size(&self) -> [f32; 2] {
        WINDOW
    }

    fn palette(&self) -> Palette {
        self.palette
    }

    fn poll(&mut self) -> Option<Duration> {
        // Nothing to hear from: setup that has not been told to start, or a
        // window that opened only to say why there is nothing to install.
        let news = self.news.as_ref()?;
        loop {
            match news.try_recv() {
                Ok(step) => {
                    if matches!(step, Step::Failed(_)) {
                        self.log = Some(Command::for_package(&self.job.package).log);
                    }
                    self.step = step;
                }
                // The worker has finished and dropped its end. Whatever it last
                // said is the final answer, so stop asking.
                Err(TryRecvError::Disconnected) => return None,
                Err(TryRecvError::Empty) => break,
            }
        }
        // A window that has finished is closing on its own; anything else is
        // still waiting on the worker.
        self.step.holds_work().then_some(TICK)
    }

    fn draw(&mut self, ui: &mut egui::Ui, close: &mut bool) {
        let p = self.palette;
        // Collected here and acted on after the frame, so the step cannot
        // change half way through drawing the window that reads it.
        let mut start = false;
        tabs::dialog_frame(&p).show(ui, |ui| {
            ui.add_space(4.0);
            let heading = match (self.job.setup, self.job.version.as_str()) {
                (true, "") => "Install Umber".to_string(),
                (true, v) => format!("Install Umber {v}"),
                (false, "") => "Updating Umber".to_string(),
                (false, v) => format!("Updating Umber to {v}"),
            };
            ui.label(
                egui::RichText::new(heading)
                    .size(text::HEADING)
                    .color(p.text),
            );
            ui.add_space(10.0);

            match &self.step {
                Step::Failed(why) => {
                    controls::banner(ui, &p, why, |_| {});
                    if let Some(log) = &self.log {
                        ui.add_space(6.0);
                        controls::note(
                            ui,
                            &p,
                            "Windows wrote a log of what it tried. Umber is \
                             still the version you had.",
                        );
                        ui.add_space(4.0);
                        ui.add_space(2.0);
                        ui.label(
                            egui::RichText::new(log.display().to_string())
                                .size(9.5)
                                .color(p.text_dim)
                                .line_height(Some(12.5)),
                        );
                    }
                }
                // Before anything has been done. No bar, because nothing is
                // happening yet and a track sitting at zero would read as an
                // install that had stalled.
                Step::Ready => {
                    controls::note(
                        ui,
                        &p,
                        "Umber will be installed for everyone on this machine,                          so Windows will ask for permission once.",
                    );
                }
                step => {
                    // The bar, and a line saying which step it is on. An empty
                    // track while `msiexec` runs, because nothing reports
                    // progress out of a silent install — see `Step::progress`.
                    widgets::progress_bar(ui, &p, step.progress());
                    ui.add_space(8.0);
                    controls::note(ui, &p, &step.label());
                    if matches!(step, Step::AskingPermission) {
                        ui.add_space(6.0);
                        controls::note(
                            ui,
                            &p,
                            "Windows will ask whether to allow the installation. \
                             Umber installs for everyone on this machine, so it \
                             needs that permission.",
                        );
                    }
                }
            }

            ui.add_space(12.0);
            // Inside a `horizontal`, for the reason every dialog footer here
            // is: a bare right-to-left layout takes the whole remaining height
            // and leaves the buttons floating in the middle of the window.
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Only once there is nothing running. A window that
                    // could be dismissed mid-install would leave `msiexec`
                    // working with nothing on screen to say so — the rule
                    // `Flow::holds_work` already keeps.
                    let done = !self.step.holds_work();
                    if matches!(self.step, Step::Ready) {
                        // Setup, waiting to be told. The emphasised button is
                        // the one that acts; Cancel is beside it, because a
                        // window opened by a double-click has to be refusable
                        // without installing anything.
                        if tabs::button(ui, &p, "Install", true) {
                            start = true;
                        }
                        if tabs::button(ui, &p, "Cancel", false) {
                            *close = true;
                        }
                    } else if done && tabs::button(ui, &p, "Close", true) {
                        *close = true;
                    }
                    if let Some(dir) = self.log.as_deref().and_then(Path::parent)
                        && tabs::button(ui, &p, "Show the log", false)
                    {
                        // The same opener the crash box and the settings
                        // dialog use. Best effort by construction, which is
                        // why the path is printed above it as well.
                        if let Err(e) = crate::autosave::reveal(dir) {
                            log::warn!("could not open {}: {e}", dir.display());
                        }
                    }
                });
            });
        });

        if start {
            self.step = Step::AskingPermission;
            self.begin();
        }
        // Nothing left to do and nothing to read: close on its own rather than
        // asking somebody to dismiss a window that is only saying "done".
        if matches!(self.step, Step::Finished) {
            *close = true;
        }
    }
}

// ---------------------------------------------------------------------------
// The platform
// ---------------------------------------------------------------------------

/// Wait for a process to end, giving up after a while.
///
/// The timeout is what stops the helper hanging for ever behind an Umber that
/// will not close — a quit prompt somebody walked away from. Going ahead anyway
/// is the better failure: Windows Installer will refuse to replace a file in
/// use and say so, which the window then reports, where waiting for ever leaves
/// a window that never changes.
#[cfg(windows)]
fn wait_for(pid: u32) {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };

    const GIVE_UP_MS: u32 = 120_000;
    // SAFETY: `OpenProcess` takes a plain id and returns null on failure, which
    // is the case where the process has already gone — exactly what is being
    // waited for.
    unsafe {
        let handle = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            return;
        }
        let waited = WaitForSingleObject(handle, GIVE_UP_MS);
        if waited != WAIT_OBJECT_0 {
            log::warn!("gave up waiting for Umber (pid {pid}) to close");
        }
        CloseHandle(handle);
    }
}

#[cfg(not(windows))]
fn wait_for(_pid: u32) {}

/// Run the installer, elevated, and wait for it.
///
/// `ShellExecuteExW` with the `runas` verb rather than `Command::spawn`, and
/// that is the whole of why this is not four lines: an MSI installing for the
/// whole machine needs administrator rights, and a plain spawn from an
/// unelevated Umber would have `msiexec` fail with "you must be an
/// administrator" — silently, because `/qn` has no interface to say it in.
/// `runas` is what raises the one consent prompt this update shows.
#[cfg(windows)]
fn install(command: &Command, started: &dyn Fn()) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows_sys::Win32::UI::Shell::{
        SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    let verb = wide("runas");
    let file = wide(&command.program.to_string_lossy());
    let parameters = wide(&command.parameters());

    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    // `NOCLOSEPROCESS` is what hands back a handle to wait on; without it there
    // is no way to know whether the install worked, and the window would say
    // "done" the instant the prompt was answered.
    info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = parameters.as_ptr();
    info.nShow = SW_HIDE;

    // SAFETY: every pointer above outlives the call, and `info` is zeroed
    // before its fields are set so the ones not used are null.
    let launched = unsafe { ShellExecuteExW(&mut info) };
    if launched == 0 {
        // By far the most likely reason, and worth naming rather than reporting
        // a Windows error number: the consent prompt was declined.
        return Err(
            "Windows did not allow the installation to start. It may have been \
             declined at the permission prompt."
                .to_string(),
        );
    }
    if info.hProcess.is_null() {
        return Err("Windows did not report the installer's progress.".to_string());
    }

    // The consent prompt has been answered and Windows Installer is running.
    started();

    // SAFETY: `hProcess` is non-null and owned by this function until closed.
    let code = unsafe {
        let waited = WaitForSingleObject(info.hProcess, u32::MAX);
        let mut code: u32 = 0;
        let read = GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hProcess);
        if waited != WAIT_OBJECT_0 || read == 0 {
            return Err("Umber could not tell whether the installation finished.".to_string());
        }
        code
    };

    match code {
        0 => Ok(()),
        // 3010 is "a restart is required to finish", which `/norestart` asks
        // for rather than performing. The files are in place.
        3010 => Ok(()),
        1602 => Err("The installation was cancelled.".to_string()),
        other => Err(format!(
            "Windows Installer stopped with error {other}. Umber is still the \
             version you had."
        )),
    }
}

#[cfg(not(windows))]
fn install(_command: &Command, _started: &dyn Fn()) -> Result<(), String> {
    Err("Umber only installs a package this way on Windows.".to_string())
}

/// Start the version that was just installed.
///
/// The *installed* path rather than this helper's own, because this helper is a
/// copy in the temporary directory — starting it again would open the updater,
/// not Umber.
fn start_installed(target: Option<&Path>) -> Result<(), String> {
    let exe = target.ok_or_else(|| "Umber was not told where it is installed.".to_string())?;
    std::process::Command::new(exe)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("{e}"))
}
