//! Installing a Windows package without showing Windows' installer.
//!
//! An update on the MSI installation used to hand `msiexec` the package and get
//! out of the way, which meant the artist watched a Windows installer they had
//! not asked for and then had to start Umber again themselves. This is the
//! replacement: a process of Umber's own, drawing Umber's own window, running
//! `msiexec` silently underneath and starting the new build when it is done.
//!
//! ## Why it has to be a second process at all
//!
//! **A running executable cannot be replaced.** Umber's own `umber.exe` is the
//! file the package is about to overwrite, so Umber has to be gone before the
//! installer's execute sequence reaches it — which is exactly why the old
//! design exited. Something else therefore has to hold the window and start the
//! new copy, and that something cannot be Umber.
//!
//! Nor can it be the package: the MSI's "Start Umber" action is published on
//! the exit dialog's Finish button (`packaging/windows/umber.wxs`), so a silent
//! install has no UI sequence to fire it and nothing would relaunch anything.
//!
//! So: the same executable again, with [`FLAG`], exactly as the crash reporter
//! is the same executable with `--crash-report`. It shares `shell`, `theme` and
//! `widgets`, so the box is Umber's rather than a second interface.
//!
//! **From a copy in the temporary directory**, and that is not tidiness. The
//! helper would otherwise *be* a file inside the installation the package is
//! replacing, and the installer would find it in use — scheduling a reboot or
//! killing it mid-window. [`stage_helper`] is the copy.
//!
//! ## What cannot be hidden, and is not
//!
//! The **UAC consent prompt**. Umber installs per-machine, so the package needs
//! elevation, and a consent dialog that an application could suppress would not
//! be a security feature. It is asked for once, by [`elevated_msiexec`], and
//! the window says so before it happens rather than letting it arrive unexplained.
//!
//! ## What is testable here
//!
//! Everything except the two calls that touch Windows. The command line is a
//! pure function of the arguments ([`parse`]), the `msiexec` invocation is a
//! pure function of the paths ([`Command::for_package`]), and the stages the
//! window shows are a plain enum. That is the same division `install::detect`
//! keeps, and for the same reason: nobody can cut a release to test against, so
//! the parts that can be checked without one must be.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The flag that turns this executable into the update installer.
pub const FLAG: &str = "--install-update";

/// The flag that turns it into the *first-run* installer.
///
/// `umber-setup.exe` carries the package on its own end — see
/// [`super::payload`] — so this needs no arguments at all, which matters
/// because it is the one flag a person may type or a shortcut may carry.
pub const SETUP_FLAG: &str = "--install";

/// What the helper was asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    /// The `.msi` to install.
    pub package: PathBuf,
    /// The process to wait for before touching anything, if one was named.
    ///
    /// Umber's own id. The helper is spawned *before* Umber exits — there is no
    /// other moment it could be — so it has to wait, or the installer meets the
    /// running executable it is replacing.
    pub parent: Option<u32>,
    /// The version being installed, for the window to name. Free text out of
    /// the release API, so it is only ever displayed.
    pub version: String,
    /// Whether this is the first-run installer rather than an update.
    ///
    /// The two do the same thing to the machine and differ in what the window
    /// says and in one behaviour: an update was already asked for and gets on
    /// with it, where setup was double-clicked by somebody who has not yet
    /// agreed to anything and waits to be told to start.
    pub setup: bool,
    /// The installed `umber.exe` to start when the package is in place.
    ///
    /// Carried rather than worked out here, and that is the point: this helper
    /// runs from a **copy in the temporary directory** — see [`stage_helper`] —
    /// so its own `current_exe` is the updater, not Umber. Starting that would
    /// reopen the updater.
    pub target: Option<PathBuf>,
}

/// Read the command line.
///
/// A pure function of the arguments, like [`crate::crash::parse_args`] and
/// [`super::install::detect`]. Returns `None` for a command line that is not
/// this — including [`FLAG`] with nothing usable after it, because a helper
/// with no package to install has nothing to do and being Umber is the better
/// answer than being a window that reports its own arguments.
pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Option<Job> {
    let mut args = args.into_iter().skip(1);
    while let Some(arg) = args.next() {
        // The first-run installer. No arguments: the package is on the end of
        // this file and the window asks before it does anything.
        if arg == SETUP_FLAG {
            return Some(Job {
                package: PathBuf::new(),
                parent: None,
                version: String::new(),
                setup: true,
                target: None,
            });
        }
        if arg != FLAG {
            continue;
        }
        let package = args.next()?;
        if package.is_empty() {
            return None;
        }
        // The two after it are optional and positional. A missing parent means
        // "do not wait", which is right for a helper started by hand.
        let parent = args.next().and_then(|v| v.parse::<u32>().ok());
        let version = args.next().unwrap_or_default();
        let target = args.next().filter(|v| !v.is_empty()).map(PathBuf::from);
        return Some(Job {
            package: PathBuf::from(package),
            parent,
            version,
            setup: false,
            target,
        });
    }
    None
}

/// The `msiexec` invocation for a package.
///
/// A type rather than a bare `Vec` so the arguments can be read in a test
/// without running anything, which is the only way any of this is checked on a
/// machine that cannot install an MSI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub program: OsString,
    pub args: Vec<OsString>,
    /// Where the installer's own log goes, so a failure has something to show.
    pub log: PathBuf,
}

impl Command {
    /// Install `package`, silently.
    ///
    /// * `/i` — install, and because `packaging/windows/umber.wxs` keeps one
    ///   `UpgradeCode` for ever, that replaces the installed version rather
    ///   than installing beside it.
    /// * `/qn` — no interface at all. This is the whole point: the artist asked
    ///   Umber for an update, not for a Windows installer.
    /// * `/norestart` — never reboot the machine on its own. A painting
    ///   application that restarts somebody's computer to finish updating
    ///   itself would be indefensible. If files really are in use the install
    ///   fails and is reported, which is recoverable; a reboot is not.
    /// * `/l*v` — a verbose log beside the package. Nothing reads it, and that
    ///   is the point: when this fails on a machine nobody here owns, the log
    ///   is the only thing that can say why.
    pub fn for_package(package: &Path) -> Self {
        let log = package.with_extension("log");
        Self {
            program: OsString::from("msiexec"),
            args: vec![
                OsString::from("/i"),
                package.as_os_str().to_owned(),
                OsString::from("/qn"),
                OsString::from("/norestart"),
                OsString::from("/l*v"),
                log.as_os_str().to_owned(),
            ],
            log,
        }
    }

    /// The arguments as one string, for `ShellExecuteExW`, which takes the
    /// parameters as a single line rather than as a vector.
    ///
    /// Each is quoted, because a package under `C:\Users\Someone Else\...` has
    /// a space in it and an unquoted path would be read as two arguments — an
    /// installer that reports a missing file on exactly the machines whose
    /// owner has a space in their name.
    pub fn parameters(&self) -> String {
        self.args
            .iter()
            .map(|a| format!("\"{}\"", a.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// How far the install has got, for the window to draw.
///
/// A short list on purpose. Windows Installer reports nothing useful to a
/// caller running it silently — there is no progress channel out of `/qn` — so
/// what is honest here is *which step*, never a percentage. That is the rule
/// `Stage::progress` already follows for `HandingOver`: a bar that animates
/// over something it cannot see is the control this project refuses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// In place, and the artist will have to start it themselves.
    ///
    /// A **success**, and keeping it out of [`Step::Failed`] is the point. The
    /// package went on exactly as asked; all that is missing is the relaunch,
    /// which for a first install is a nicety. Reporting that as a failed
    /// installation would send somebody looking for a problem that is not
    /// there — the same rule `relaunch` follows, where an update that could not
    /// restart leaves the old Umber running rather than claiming it failed.
    Installed,
    /// Waiting to be told to start.
    ///
    /// The first-run installer alone. An update was asked for and carries on;
    /// setup was double-clicked, and putting files on somebody's machine
    /// because they opened a window would be the wrong way round.
    Ready,
    /// Waiting for Umber to close. Fast, and the one step that could hang, so
    /// it is named rather than folded into the next.
    WaitingForUmber,
    /// The consent prompt is up, or about to be.
    AskingPermission,
    /// `msiexec` is running.
    Installing,
    /// Done, and the new Umber is being started.
    Starting,
    /// Done, and this window is about to close.
    Finished,
    /// It did not work. The window says so, names the log and offers to open
    /// the folder — rather than closing and leaving somebody with the version
    /// they had and no idea why.
    Failed(String),
}

impl Step {
    /// The line under the bar.
    pub fn label(&self) -> String {
        match self {
            Self::Ready => "Umber will be installed for everyone on this machine.".to_string(),
            Self::Installed => "Umber is installed. Start it from the Start menu.".to_string(),
            Self::WaitingForUmber => "Waiting for Umber to close...".to_string(),
            Self::AskingPermission => "Asking Windows for permission to install...".to_string(),
            Self::Installing => "Installing...".to_string(),
            Self::Starting => "Starting the new version...".to_string(),
            Self::Finished => "Done.".to_string(),
            Self::Failed(why) => why.clone(),
        }
    }

    /// Whether the window is still working, and therefore may not be dismissed.
    ///
    /// The same rule `Flow::holds_work` follows in the update dialog: a window
    /// that vanished mid-install would leave `msiexec` running with nothing on
    /// screen to say so.
    pub fn holds_work(&self) -> bool {
        !matches!(
            self,
            Self::Ready | Self::Installed | Self::Finished | Self::Failed(_)
        )
    }

    /// How far along the bar sits, where that can be said at all.
    ///
    /// `None` while `msiexec` runs, because it reports nothing and an animation
    /// invented here would be a lie about somebody's installation. The bar
    /// draws an empty track, exactly as `Stage::HandingOver`'s does.
    pub fn progress(&self) -> Option<f32> {
        match self {
            Self::Ready => Some(0.0),
            Self::Installed => Some(1.0),
            Self::WaitingForUmber => Some(0.1),
            Self::AskingPermission => Some(0.25),
            Self::Installing => None,
            Self::Starting => Some(0.95),
            Self::Finished => Some(1.0),
            Self::Failed(_) => None,
        }
    }
}

/// The folder name the package installs into, under Program Files.
///
/// Pinned against `packaging/windows/umber.wxs` by a test rather than merely
/// matching it today: the setup window starts Umber from this path when the
/// package has gone in, and a rename over there would otherwise turn a
/// successful install into a window that could not find what it had installed.
/// Same arrangement `taskbar`'s names keep against `packaging/`.
pub const INSTALL_FOLDER: &str = "Umber";

/// Where the package puts `umber.exe`, given what Windows says Program Files
/// is.
///
/// Injected rather than read from the environment here, for the reason
/// [`super::install::detect`] takes a `Probe`: it is the only way the answer is
/// testable on a machine that is not Windows.
///
/// **Only for the first install.** An update knows the path already — it is the
/// Umber that spawned the helper — and carries it in [`Job::target`]. This is
/// the case where there was no Umber to ask.
pub fn installed_path(program_files: Option<&str>) -> Option<PathBuf> {
    let root = program_files?;
    if root.is_empty() {
        return None;
    }
    Some(PathBuf::from(root).join(INSTALL_FOLDER).join("umber.exe"))
}

/// Put a copy of this executable somewhere the installer will not be replacing
/// it, and answer where it went.
///
/// The helper must not run from inside the installation being upgraded: a
/// running file is in use, and Windows Installer meeting one either schedules a
/// reboot or has Restart Manager kill it — with the update half done and the
/// window that was explaining it gone.
pub fn stage_helper(dir: &Path) -> Result<PathBuf, String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("Umber could not find its own program file: {e}"))?;
    let name = if cfg!(windows) {
        "umber-updater.exe"
    } else {
        "umber-updater"
    };
    let to = dir.join(name);
    // Removed first rather than overwritten: a copy left by an update that was
    // abandoned may still be running, and on Windows the write would fail with
    // a sharing violation that reads like a permissions problem.
    let _ = std::fs::remove_file(&to);
    std::fs::copy(&exe, &to)
        .map_err(|e| format!("Umber could not stage the updater at {}: {e}", to.display()))?;
    Ok(to)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    /// An ordinary launch is not the installer, and neither is one carrying
    /// somebody else's argument.
    #[test]
    fn an_ordinary_command_line_is_not_an_install() {
        assert_eq!(parse(args(&["umber"])), None);
        assert_eq!(parse(args(&[])), None);
        assert_eq!(parse(args(&["umber", "picture.ora"])), None);
        // The crash reporter's flag must fall through to the crash reporter,
        // not be swallowed here.
        assert_eq!(parse(args(&["umber", "--crash-report", "r.json"])), None);
    }

    /// **The first-run installer needs no arguments**, because the package is
    /// on the end of the file it is reading from. That is what lets it be
    /// double-clicked.
    #[test]
    fn setup_is_its_own_flag_and_carries_nothing() {
        let job = parse(args(&["umber-setup.exe", SETUP_FLAG])).expect("setup");
        assert!(job.setup);
        assert_eq!(job.parent, None);
        assert_eq!(job.target, None);

        // And it is not the update flag: an update names a package and waits
        // for the process that spawned it, and confusing the two would install
        // nothing or install it twice.
        let update = parse(args(&["umber", FLAG, "p.msi"])).expect("update");
        assert!(!update.setup);
    }

    #[test]
    fn the_flag_carries_a_package_a_parent_and_a_version() {
        let job = parse(args(&[
            "umber",
            FLAG,
            "C:\\Temp\\umber-0.0.8-x64.msi",
            "4321",
            "0.0.8",
        ]))
        .expect("an install");
        assert_eq!(job.package, PathBuf::from("C:\\Temp\\umber-0.0.8-x64.msi"));
        assert_eq!(job.parent, Some(4321));
        assert_eq!(job.version, "0.0.8");
    }

    /// **The flag with nothing usable after it is not an install.** A window
    /// that opened with no package to put in place could only report its own
    /// arguments, where starting Umber is something somebody can use.
    #[test]
    fn a_flag_with_no_package_is_refused() {
        assert_eq!(parse(args(&["umber", FLAG])), None);
        assert_eq!(parse(args(&["umber", FLAG, ""])), None);

        // The trailing two are optional: started by hand, with no parent to
        // wait for and no version to name.
        let job = parse(args(&["umber", FLAG, "p.msi"])).expect("an install");
        assert_eq!(job.parent, None);
        assert_eq!(job.version, "");
        assert_eq!(job.target, None);
        assert!(!job.setup);

        // And a parent that is not a number is "do not wait" rather than a
        // refusal, for the reason `crash::parse_args` ignores what it cannot
        // read: this is a command line, and refusing to run over one bad word
        // is worse than running.
        let job = parse(args(&["umber", FLAG, "p.msi", "later"])).expect("an install");
        assert_eq!(job.parent, None);
    }

    /// **Silent, never rebooting, and logged.** Each of these is load-bearing
    /// and each would be invisible if it regressed: `/qn` is the whole feature,
    /// `/norestart` is the difference between a failed update and somebody's
    /// machine restarting under them, and the log is the only evidence
    /// available when this fails on hardware nobody here owns.
    #[test]
    fn the_install_is_silent_and_never_reboots() {
        let cmd = Command::for_package(Path::new("C:\\Temp\\umber-0.0.8-x64.msi"));
        let flat: Vec<String> = cmd
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(flat.contains(&"/qn".to_string()), "{flat:?}");
        assert!(flat.contains(&"/norestart".to_string()), "{flat:?}");
        assert!(flat.contains(&"/i".to_string()), "{flat:?}");
        assert_eq!(cmd.log, PathBuf::from("C:\\Temp\\umber-0.0.8-x64.log"));
        // No interactive level may creep back in beside the silent one.
        for loud in ["/qb", "/qf", "/qr", "/passive"] {
            assert!(!flat.iter().any(|a| a == loud), "{loud} in {flat:?}");
        }
    }

    /// A path with a space in it is one argument, not two.
    #[test]
    fn every_parameter_is_quoted() {
        let cmd = Command::for_package(Path::new("C:\\Users\\Some One\\umber.msi"));
        let line = cmd.parameters();
        assert!(
            line.contains("\"C:\\Users\\Some One\\umber.msi\""),
            "{line}"
        );
        assert!(line.starts_with("\"/i\""), "{line}");
    }

    /// **A successful install is never reported as a failure**, however the
    /// relaunch went. `Installed` is the arm that says so.
    #[test]
    fn being_unable_to_start_umber_is_not_a_failed_install() {
        assert!(!Step::Installed.holds_work());
        assert_eq!(Step::Installed.progress(), Some(1.0));
        let said = Step::Installed.label().to_lowercase();
        for word in ["fail", "could not", "error", "sorry", "problem"] {
            assert!(!said.contains(word), "{said:?} reads as a failure");
        }
    }

    /// Where the first install starts Umber from, and it must be where the
    /// package actually put it.
    #[test]
    fn the_installed_path_is_program_files_and_the_packages_own_folder() {
        assert_eq!(
            installed_path(Some(r"C:\Program Files")),
            Some(PathBuf::from(r"C:\Program Files\Umber\umber.exe"))
        );
        // Nothing to build a path from is "do not guess", which the window
        // reads as `Installed` rather than as a failure.
        assert_eq!(installed_path(None), None);
        assert_eq!(installed_path(Some("")), None);
    }

    /// **The folder is the package's**, and this is what stops the two drifting:
    /// renaming it in the `.wxs` without changing it here would leave a
    /// successful install unable to find what it had just installed.
    #[test]
    fn the_install_folder_is_the_one_the_package_uses() {
        let wxs = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../packaging/windows/umber.wxs"),
        )
        .expect("the packaging file");
        let wanted = format!("<Directory Id=\"INSTALLFOLDER\" Name=\"{INSTALL_FOLDER}\" />");
        assert!(
            wxs.contains(&wanted),
            "umber.wxs does not install into {INSTALL_FOLDER}; expected {wanted}"
        );
    }

    /// The window may not be dismissed while `msiexec` is running, and must be
    /// dismissible the moment it is not.
    #[test]
    fn a_step_that_is_working_holds_the_window() {
        for step in [
            Step::WaitingForUmber,
            Step::AskingPermission,
            Step::Installing,
            Step::Starting,
        ] {
            assert!(step.holds_work(), "{step:?}");
        }
        assert!(!Step::Finished.holds_work());
        assert!(!Step::Failed("no".into()).holds_work());
        // Setup before it has been told to start is not working either: the
        // window has a Cancel that must not be refused.
        assert!(!Step::Ready.holds_work());
    }

    /// **The bar is empty while Windows is installing**, because nothing
    /// reports progress out of a silent `msiexec` and a bar that moved anyway
    /// would be inventing it.
    #[test]
    fn nothing_claims_progress_it_cannot_see() {
        assert_eq!(Step::Installing.progress(), None);
        assert_eq!(Step::Failed("x".into()).progress(), None);
        assert_eq!(Step::Finished.progress(), Some(1.0));
    }

    /// No step may claim the download or the package was checked in a way it
    /// was not — `update`'s standing rule, applied to this window's wording
    /// too. Umber does not sign its releases.
    #[test]
    fn no_step_calls_anything_verified() {
        for step in [
            Step::Ready,
            Step::Installed,
            Step::WaitingForUmber,
            Step::AskingPermission,
            Step::Installing,
            Step::Starting,
            Step::Finished,
        ] {
            let said = step.label().to_lowercase();
            for word in ["verif", "authentic", "secure", "signed", "signature"] {
                assert!(!said.contains(word), "{said:?} contains {word}");
            }
        }
    }
}
