//! How this copy of Umber was installed — and therefore whether it may replace
//! itself.
//!
//! Umber ships in nine shapes (see `packaging/` and `.github/workflows/release.yml`),
//! and they divide into two groups that must be treated completely differently:
//!
//! * **The ones Umber owns.** A portable zip or tarball unpacked wherever the
//!   user put it, and an AppImage, which is a single file. Nothing else on the
//!   system has a record of these, so replacing the file *is* the update.
//! * **The ones a package manager owns.** `.deb`, `.rpm`, Arch's
//!   `.pkg.tar.zst`, the Flatpak — and, on Windows, the MSI. Every one of those
//!   has a database entry listing the files it installed and their checksums.
//!   Writing over those files behind the manager's back is wrong three times
//!   over: it is usually not permitted (they live under `/usr`, owned by root),
//!   it makes the manager's record a lie, and the next system upgrade puts the
//!   old version back — silently, months later, which is the worst way for this
//!   to fail.
//!
//! The MSI is the one managed case Umber can still update, because Windows
//! provides the mechanism: hand `msiexec` a newer MSI and it does the upgrade,
//! elevating on its own and keeping its own record straight. What Umber must
//! *not* do is edit files under Program Files itself.
//!
//! Everything here is decided by [`detect`], which is a pure function of a
//! [`Probe`] — the executable's path, the environment and a "does this path
//! exist" predicate. That is what lets the Linux and macOS answers be tested on
//! a Windows machine, which is the only way they get tested at all.

use super::version::Version;
use std::path::{Path, PathBuf};

/// The platforms Umber publishes for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Windows,
    /// Spelt `Mac` rather than `MacOs`, which would repeat the enum's own name.
    Mac,
    Linux,
}

impl Os {
    /// The platform this build runs on.
    pub const CURRENT: Os = if cfg!(target_os = "windows") {
        Os::Windows
    } else if cfg!(target_os = "macos") {
        Os::Mac
    } else {
        Os::Linux
    };
}

/// The architectures the release workflow builds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Aarch64,
}

impl Arch {
    /// The architecture this build runs on, or `None` on one Umber does not
    /// publish for — a source build on a machine with no release asset to
    /// offer it.
    pub const CURRENT: Option<Arch> = if cfg!(target_arch = "x86_64") {
        Some(Arch::X86_64)
    } else if cfg!(target_arch = "aarch64") {
        Some(Arch::Aarch64)
    } else {
        None
    };

    /// Debian's spelling, which is the one in a `.deb` file's name.
    pub fn deb(self) -> &'static str {
        match self {
            Self::X86_64 => "amd64",
            Self::Aarch64 => "arm64",
        }
    }

    /// rpm's spelling, which the Arch package and the AppImage use too.
    pub fn rpm(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
        }
    }
}

/// The package manager that owns an installation.
///
/// The two formats the house archive publishes carry a second fact with them:
/// whether *this machine* has that archive configured. It is the difference
/// between a package manager that can fetch a new Umber and one that cannot,
/// and it changes the whole of what there is to say, so it is part of the
/// answer rather than something the message guesses at later.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Manager {
    Flatpak,
    Dpkg {
        archive: bool,
    },
    Rpm {
        archive: bool,
    },
    Pacman,
    /// A system path with no recognisable manager behind it. Named separately
    /// from "unknown installation" because the answer to the user is different:
    /// this one is certainly not ours to overwrite.
    Unknown,
}

impl Manager {
    /// What to call it in a sentence.
    pub fn label(self) -> &'static str {
        match self {
            Self::Flatpak => "Flatpak",
            Self::Dpkg { .. } => "apt / dpkg",
            Self::Rpm { .. } => "dnf / zypper / rpm",
            Self::Pacman => "pacman",
            Self::Unknown => "a package manager",
        }
    }

    /// What to do about it: a sentence, and a command where there is an honest
    /// one to print.
    ///
    /// **Every arm without the archive names a file rather than an upgrade.**
    /// 0.1.4 printed `sudo apt install --only-upgrade umber`, and that command
    /// cannot ever have worked: the packages come from a release page, apt only
    /// consults archives it has been given, and a machine with none answers
    /// "umber is already the newest version" however old it is. It sounds
    /// authoritative, which is what makes it the worst kind of wrong. The same
    /// was true of `pacman -Syu umber`, for a package in no repository, and of
    /// `flatpak update`, for a bundle with no remote.
    ///
    /// The rpm arms still print no command even with the archive configured,
    /// and for the original reason: Fedora, RHEL and openSUSE drive the same
    /// package format with three different front ends, so naming one would be
    /// wrong on two distributions out of three. "Your usual system update" is
    /// the sentence that is true on all three.
    fn remedy(self, version: &Version, arch: Option<Arch>) -> String {
        // Where the architecture is unknown there is no file name to write, and
        // inventing one would send somebody looking for a file that is not
        // there. See `Arch::CURRENT`: a source build on riscv64 is a real case.
        const BY_HAND: &str =
            "Take the new package from the releases page and install it over this one.";

        match (self, arch) {
            (Self::Dpkg { archive: true }, _) => "This machine has the Spillebulle archive, \
                 so apt already has the new version. Update it with:\n\n    \
                 sudo apt update && sudo apt install --only-upgrade umber"
                .to_string(),

            (Self::Dpkg { archive: false }, Some(arch)) => {
                let file = format!("umber_{version}_{}.deb", arch.deb());
                format!(
                    "This machine does not have the Spillebulle archive, so apt has \
                     nothing newer to offer whatever it says. Take {file} from the \
                     releases page and install it over this one:\n\n    \
                     sudo apt install ./{file}\n\nThat package adds the archive, and \
                     apt keeps Umber up to date from then on."
                )
            }

            (Self::Rpm { archive: true }, _) => "This machine has the Spillebulle archive, \
                 so your usual system update now includes Umber."
                .to_string(),

            (Self::Rpm { archive: false }, Some(arch)) => {
                let file = format!("umber-{version}-1.{}.rpm", arch.rpm());
                format!(
                    "This machine does not have the Spillebulle archive, so there is \
                     nothing newer for your package manager to find. Take {file} from \
                     the releases page and install it over this one. That package adds \
                     the archive, and your usual system update keeps Umber up to date \
                     from then on."
                )
            }

            // The Arch package is built for x86-64 only, and is in no
            // repository: neither the official ones nor the AUR. So there is
            // nothing for `pacman -Syu` to find, and the file is the answer.
            (Self::Pacman, Some(Arch::X86_64)) => {
                let file = format!("umber-bin-{version}-1-x86_64.pkg.tar.zst");
                format!(
                    "The Arch package is in no repository, so pacman has nothing to \
                     upgrade from. Take {file} from the releases page and install it \
                     over this one:\n\n    sudo pacman -U {file}"
                )
            }

            // The bundle is published as a file, not through Flathub, so the
            // installation has no remote behind it and `flatpak update` finds
            // nothing to do. x86-64 only, for the same reason the release is.
            (Self::Flatpak, Some(Arch::X86_64)) => {
                let file = format!("umber-{version}-x86_64.flatpak");
                format!(
                    "This bundle has no remote behind it, so there is nothing for \
                     flatpak to update from. Take {file} from the releases page and \
                     install it over this one:\n\n    flatpak install --user {file}"
                )
            }

            (Self::Unknown, _) => "Update it through your package manager, or take the new \
                 package from the releases page."
                .to_string(),

            _ => BY_HAND.to_string(),
        }
    }
}

/// How this copy got onto the machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallKind {
    /// An archive the user unpacked. The binary sits in a directory Umber owns
    /// and can be replaced in place.
    Portable,
    /// One AppImage file, named by `$APPIMAGE`. Replacing that file is the
    /// whole update.
    AppImage(PathBuf),
    /// Installed by the Windows MSI. Updated by handing `msiexec` a newer one,
    /// never by writing into Program Files.
    Msi,
    /// Owned by a package manager. Umber says so and stays out of the way.
    Managed(Manager),
    /// Umber could not work out where it is — `current_exe` failed, or the
    /// architecture is one no release is built for. Nothing is offered, because
    /// the alternative is guessing about somebody's file system.
    Unknown,
}

impl InstallKind {
    /// Whether Umber may update this installation itself.
    pub fn is_self_updatable(&self) -> bool {
        matches!(self, Self::Portable | Self::AppImage(_) | Self::Msi)
    }

    /// How the About dialog names it.
    pub fn label(&self) -> String {
        match self {
            Self::Portable => "a portable archive".to_string(),
            Self::AppImage(_) => "an AppImage".to_string(),
            Self::Msi => "the Windows installer".to_string(),
            Self::Managed(m) => format!("a package managed by {}", m.label()),
            Self::Unknown => "an unrecognised location".to_string(),
        }
    }

    /// The sentence shown where an update cannot be applied from inside Umber.
    ///
    /// Takes the version being offered and this machine's architecture because
    /// the answer is usually a file to fetch, and a file has a name. Naming it
    /// is the difference between a message somebody can act on and one they
    /// have to go and interpret.
    pub fn cannot_update(&self, version: &Version, arch: Option<Arch>) -> Option<String> {
        match self {
            Self::Portable | Self::AppImage(_) | Self::Msi => None,
            Self::Managed(m) => Some(format!(
                "This copy was installed by {}, which keeps its own record of \
                 every file it owns. Umber will not write over that: the next \
                 system upgrade would put the old version back.\n\n{}",
                m.label(),
                m.remedy(version, arch),
            )),
            Self::Unknown => Some(
                "Umber cannot tell how this copy was installed, so it will not \
                 replace any of its own files. Take the new build from the \
                 releases page."
                    .to_string(),
            ),
        }
    }
}

/// Everything [`detect`] is allowed to look at.
///
/// A struct of injected readings rather than direct calls to `std::env` and
/// `Path::exists`, so the answer for every platform can be tested on one
/// machine. The Linux and macOS branches below have never been *run*; they have
/// only been tested, and this is what makes even that possible.
pub struct Probe<'a> {
    pub os: Os,
    /// Where the running executable is, or `None` if the platform would not say.
    pub exe: Option<PathBuf>,
    pub env: &'a dyn Fn(&str) -> Option<String>,
    pub exists: &'a dyn Fn(&Path) -> bool,
}

impl Probe<'_> {
    /// The probe for the machine this is running on.
    pub fn current() -> Probe<'static> {
        Probe {
            os: Os::CURRENT,
            exe: std::env::current_exe().ok(),
            env: &|key| std::env::var(key).ok(),
            exists: &|path| path.exists(),
        }
    }
}

/// Work out how this copy was installed.
pub fn detect(probe: &Probe<'_>) -> InstallKind {
    // Flatpak first, and before anything that looks at paths. Inside the
    // sandbox the executable is at `/app/bin/umber`, which is a system path
    // like any other and would otherwise be classified by whichever package
    // database happened to be visible. `/.flatpak-info` is the file the
    // runtime itself puts there and is how every application in the sandbox
    // finds out it is in one.
    if (probe.exists)(Path::new("/.flatpak-info")) || (probe.env)("FLATPAK_ID").is_some() {
        return InstallKind::Managed(Manager::Flatpak);
    }

    // An AppImage tells its payload where the image file is. Without that
    // variable there is nothing to replace: the process is running out of a
    // mount point that disappears when it exits.
    if let Some(image) = (probe.env)("APPIMAGE") {
        let path = PathBuf::from(image);
        if (probe.exists)(&path) {
            return InstallKind::AppImage(path);
        }
    }

    // No architecture Umber publishes for means no asset to offer, whatever
    // else is true. A source build on riscv64 is a real case — see the README.
    if Arch::CURRENT.is_none() {
        return InstallKind::Unknown;
    }

    let Some(exe) = probe.exe.as_deref() else {
        return InstallKind::Unknown;
    };
    let Some(dir) = parent_dir(probe.os, exe) else {
        return InstallKind::Unknown;
    };
    let dir = dir.as_path();

    match probe.os {
        Os::Windows => {
            // The MSI installs under Program Files, which is the only signal
            // available without reading the installer database — and reading
            // that would mean a Windows API dependency for one boolean.
            //
            // The failure direction is deliberate. A portable copy unpacked
            // into Program Files is misread as an MSI install, and the worst
            // that produces is a proper MSI installation appearing beside it;
            // the reverse mistake would have Umber writing into a directory
            // the installer owns.
            let program_files = ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"]
                .into_iter()
                .filter_map(|key| (probe.env)(key))
                .any(|root| within_windows_dir(dir, Path::new(&root)));
            if program_files {
                InstallKind::Msi
            } else {
                InstallKind::Portable
            }
        }

        // Nothing Umber ships for macOS is managed: the release carries one
        // tarball holding one universal binary, and there is no `.app` bundle,
        // no pkg and no Homebrew formula. A copy under a system prefix is
        // therefore something the user built and installed themselves, and
        // guessing at their arrangement is worse than saying so.
        Os::Mac => {
            if system_prefix(dir) {
                InstallKind::Unknown
            } else {
                InstallKind::Portable
            }
        }

        Os::Linux => {
            if !system_prefix(dir) {
                return InstallKind::Portable;
            }
            InstallKind::Managed(linux_manager(probe))
        }
    }
}

/// Which manager owns a Linux system install.
///
/// dpkg first and by name: `/var/lib/dpkg/info/umber.list` is dpkg's own record
/// of the files it laid down for this package, so its presence is proof rather
/// than inference. The other two are inferred from the package database being
/// there at all, which is sound in practice — pacman only exists on Arch, and
/// an rpm database only on an rpm distribution — and the cost of being wrong is
/// naming the wrong command in a message, never a file being written.
fn linux_manager(probe: &Probe<'_>) -> Manager {
    let exists = |path: &str| (probe.exists)(Path::new(path));

    // Whether this machine has the house archive. These are the exact files the
    // packages write on install (`packaging/linux/build-packages.sh`), so their
    // presence is not a hint about it: it is the thing itself. A path rather
    // than a call to the package manager, so this stays a pure function of the
    // probe and the message can be tested on a machine that has neither.
    let apt = Manager::Dpkg {
        archive: exists("/etc/apt/sources.list.d/spillebulle.sources"),
    };
    let rpm = Manager::Rpm {
        archive: exists("/etc/yum.repos.d/spillebulle.repo"),
    };

    if exists("/var/lib/dpkg/info/umber.list") {
        apt
    } else if exists("/var/lib/pacman/local") {
        Manager::Pacman
    } else if exists("/var/lib/rpm") || exists("/usr/lib/sysimage/rpm") {
        rpm
    } else if exists("/var/lib/dpkg/status") {
        // dpkg is here but has no record of umber — the tarball unpacked into
        // /usr by hand. Still not ours to overwrite: something else may own
        // those paths, and /usr is root's.
        apt
    } else {
        Manager::Unknown
    }
}

/// Whether a directory is inside a prefix the system owns.
///
/// `/usr/local` is deliberately absent. It is the prefix that exists precisely
/// so that locally installed software has somewhere to live that no package
/// manager touches, so a copy there is the user's own and may be replaced.
fn system_prefix(dir: &Path) -> bool {
    if dir.starts_with("/usr/local") {
        return false;
    }
    [
        "/usr",
        "/bin",
        "/sbin",
        "/opt",
        "/app",
        "/snap",
        "/nix/store",
    ]
    .into_iter()
    .any(|root| dir.starts_with(root))
}

/// Whether a Windows directory is `root` or sits inside it.
///
/// Compared component by component through `Path::starts_with` rather than as
/// text: a textual prefix test says `C:\Program Files Custom` is inside
/// `C:\Program Files`. Both sides are folded to lower case and onto one
/// separator first, because the file system is case-insensitive, accepts either
/// slash, and the environment's spelling of a directory need not match the
/// spelling in the executable's own path.
///
/// Folding is decided by the *probe's* platform rather than by `cfg!(windows)`,
/// so the Windows answers are the same whichever machine the tests run on.
fn within_windows_dir(dir: &Path, root: &Path) -> bool {
    fold_windows(dir).starts_with(fold_windows(root))
}

/// A Windows path as `Path` components the *host* will also split.
///
/// `\` is a separator on Windows and an ordinary character everywhere else, so
/// a `Path` built from `C:\Program Files\Umber` is three components on Windows
/// and one on Linux. Everything here is fed by [`Probe`], whose whole purpose
/// is that one machine can answer for every platform, so the splitting has to
/// come from the probe's OS rather than from the host's `Path` implementation.
/// Lower-cased in the same pass because the file system is case-insensitive
/// and both spellings occur in the wild.
fn fold_windows(path: &Path) -> PathBuf {
    PathBuf::from(path.to_string_lossy().to_lowercase().replace('\\', "/"))
}

/// The directory holding `exe`, split the way `os` splits paths.
///
/// Not `Path::parent`: that splits the way the *running* machine does, so a
/// Windows path tested on Linux has no separators in it at all and comes back
/// as a single component with an empty parent — which classified every Windows
/// installation as portable, including one under Program Files. Windows CI
/// passed and both Unix runners failed, which is exactly the shape of a bug
/// that only a cross-platform test matrix finds.
fn parent_dir(os: Os, exe: &Path) -> Option<PathBuf> {
    match os {
        Os::Windows => {
            let folded = fold_windows(exe);
            folded.parent().map(Path::to_path_buf)
        }
        Os::Mac | Os::Linux => exe.parent().map(Path::to_path_buf),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A probe over two fixed tables, so a whole imaginary machine is three
    /// lines of test.
    fn probe<'a>(
        os: Os,
        exe: Option<&str>,
        env: &'a HashMap<&str, &str>,
        present: &'a [&str],
    ) -> Probe<'a> {
        Probe {
            os,
            exe: exe.map(PathBuf::from),
            env: Box::leak(Box::new(move |key: &str| {
                env.get(key).map(|v| (*v).to_string())
            })),
            exists: Box::leak(Box::new(move |path: &Path| {
                present.iter().any(|p| Path::new(p) == path)
            })),
        }
    }

    fn no_env() -> HashMap<&'static str, &'static str> {
        HashMap::new()
    }

    #[test]
    fn a_flatpak_is_recognised_before_anything_else() {
        // Inside the sandbox the binary is at /app/bin/umber, and a Debian
        // host's dpkg database can be visible through the runtime. Flatpak has
        // to win, or a Flatpak on Ubuntu would be told to run apt.
        let env = no_env();
        let p = probe(
            Os::Linux,
            Some("/app/bin/umber"),
            &env,
            &["/.flatpak-info", "/var/lib/dpkg/info/umber.list"],
        );
        assert_eq!(detect(&p), InstallKind::Managed(Manager::Flatpak));
    }

    #[test]
    fn the_flatpak_id_alone_is_enough() {
        let mut env = no_env();
        env.insert("FLATPAK_ID", "io.github.spillebulle.umber");
        let p = probe(Os::Linux, Some("/app/bin/umber"), &env, &[]);
        assert_eq!(detect(&p), InstallKind::Managed(Manager::Flatpak));
    }

    #[test]
    fn an_appimage_is_the_file_the_variable_names() {
        let mut env = no_env();
        env.insert("APPIMAGE", "/home/a/Apps/Umber-0.0.1-x86_64.AppImage");
        let p = probe(
            Os::Linux,
            // The payload runs from a temporary mount, which is exactly why the
            // image's own path has to come from the environment.
            Some("/tmp/.mount_Umber1a/usr/bin/umber"),
            &env,
            &["/home/a/Apps/Umber-0.0.1-x86_64.AppImage"],
        );
        assert_eq!(
            detect(&p),
            InstallKind::AppImage(PathBuf::from("/home/a/Apps/Umber-0.0.1-x86_64.AppImage")),
        );
    }

    #[test]
    fn an_appimage_whose_file_has_gone_is_not_updated() {
        // The variable is set but names nothing — a moved or deleted image.
        // Writing a new file at that path would leave a copy the user never
        // asked for, and the running one still stale.
        let mut env = no_env();
        env.insert("APPIMAGE", "/home/a/Apps/gone.AppImage");
        let p = probe(Os::Linux, Some("/tmp/.mount_x/usr/bin/umber"), &env, &[]);
        assert_ne!(
            detect(&p),
            InstallKind::AppImage(PathBuf::from("/home/a/Apps/gone.AppImage")),
        );
    }

    #[test]
    fn a_windows_install_under_program_files_is_the_msi() {
        let mut env = no_env();
        env.insert("ProgramFiles", "C:\\Program Files");
        for exe in [
            "C:\\Program Files\\Umber\\umber.exe",
            // The environment's spelling need not match the path's: the file
            // system is case-insensitive and both forms occur in the wild.
            "c:\\program files\\Umber\\umber.exe",
            "C:/Program Files/Umber/umber.exe",
        ] {
            let p = probe(Os::Windows, Some(exe), &env, &[]);
            assert_eq!(detect(&p), InstallKind::Msi, "{exe}");
        }
    }

    #[test]
    fn a_windows_zip_unpacked_anywhere_else_is_portable() {
        let mut env = no_env();
        env.insert("ProgramFiles", "C:\\Program Files");
        for exe in [
            "C:\\Users\\a\\Downloads\\umber\\umber.exe",
            "D:\\portable\\umber\\umber.exe",
            // A textual prefix test would call this one an MSI install.
            "C:\\Program Files Custom\\umber\\umber.exe",
        ] {
            let p = probe(Os::Windows, Some(exe), &env, &[]);
            assert_eq!(detect(&p), InstallKind::Portable, "{exe}");
        }
    }

    #[test]
    fn a_debian_package_is_named_by_dpkgs_own_record() {
        let env = no_env();
        let p = probe(
            Os::Linux,
            Some("/usr/bin/umber"),
            &env,
            &["/var/lib/dpkg/info/umber.list", "/var/lib/dpkg/status"],
        );
        assert_eq!(
            detect(&p),
            InstallKind::Managed(Manager::Dpkg { archive: false }),
        );
    }

    #[test]
    fn an_arch_package_is_recognised_by_pacmans_database() {
        let env = no_env();
        let p = probe(
            Os::Linux,
            Some("/usr/bin/umber"),
            &env,
            &["/var/lib/pacman/local"],
        );
        assert_eq!(detect(&p), InstallKind::Managed(Manager::Pacman));
    }

    #[test]
    fn an_rpm_distribution_is_recognised_by_either_database_location() {
        let env = no_env();
        for present in [
            ["/var/lib/rpm"].as_slice(),
            ["/usr/lib/sysimage/rpm"].as_slice(),
        ] {
            let p = probe(Os::Linux, Some("/usr/bin/umber"), &env, present);
            assert_eq!(
                detect(&p),
                InstallKind::Managed(Manager::Rpm { archive: false }),
                "{present:?}",
            );
        }
    }

    #[test]
    fn the_house_archive_is_read_from_the_file_the_packages_write() {
        // The difference this makes is the difference between "apt has the new
        // version" and "apt will say you are up to date for ever", so it is
        // detected rather than assumed.
        let env = no_env();

        let p = probe(
            Os::Linux,
            Some("/usr/bin/umber"),
            &env,
            &[
                "/var/lib/dpkg/info/umber.list",
                "/etc/apt/sources.list.d/spillebulle.sources",
            ],
        );
        assert_eq!(
            detect(&p),
            InstallKind::Managed(Manager::Dpkg { archive: true })
        );

        let p = probe(
            Os::Linux,
            Some("/usr/bin/umber"),
            &env,
            &["/var/lib/rpm", "/etc/yum.repos.d/spillebulle.repo"],
        );
        assert_eq!(
            detect(&p),
            InstallKind::Managed(Manager::Rpm { archive: true })
        );

        // And one manager's archive is not the other's. A Debian machine with
        // a stray `.repo` file left over from something else has not got apt
        // pointed anywhere.
        let p = probe(
            Os::Linux,
            Some("/usr/bin/umber"),
            &env,
            &[
                "/var/lib/dpkg/info/umber.list",
                "/etc/yum.repos.d/spillebulle.repo",
            ],
        );
        assert_eq!(
            detect(&p),
            InstallKind::Managed(Manager::Dpkg { archive: false })
        );
    }

    #[test]
    fn a_system_install_with_no_database_is_still_not_ours_to_replace() {
        let env = no_env();
        let p = probe(Os::Linux, Some("/opt/umber/umber"), &env, &[]);
        assert_eq!(detect(&p), InstallKind::Managed(Manager::Unknown));
    }

    #[test]
    fn a_linux_tarball_in_a_home_directory_is_portable() {
        let env = no_env();
        for exe in [
            "/home/a/umber-0.0.1/umber",
            // /usr/local exists precisely so locally installed software has
            // somewhere no package manager touches.
            "/usr/local/bin/umber",
        ] {
            let p = probe(Os::Linux, Some(exe), &env, &["/var/lib/dpkg/status"]);
            assert_eq!(detect(&p), InstallKind::Portable, "{exe}");
        }
    }

    #[test]
    fn a_macos_tarball_is_portable_and_a_system_copy_is_not_touched() {
        let env = no_env();
        let p = probe(Os::Mac, Some("/Users/a/Umber/umber"), &env, &[]);
        assert_eq!(detect(&p), InstallKind::Portable);
        // Nothing Umber ships puts a binary here, so this is the user's own
        // arrangement and Umber has no business guessing at it.
        let p = probe(Os::Mac, Some("/usr/local/bin/umber"), &env, &[]);
        assert_eq!(detect(&p), InstallKind::Portable);
        let p = probe(Os::Mac, Some("/opt/homebrew/bin/umber"), &env, &[]);
        assert_eq!(detect(&p), InstallKind::Unknown);
    }

    #[test]
    fn nothing_is_offered_when_the_executables_own_path_is_unknown() {
        let env = no_env();
        let p = probe(Os::Linux, None, &env, &[]);
        assert_eq!(detect(&p), InstallKind::Unknown);
    }

    /// The version an offer would be about, for the message tests below.
    fn offered() -> Version {
        Version {
            major: 0,
            minor: 1,
            patch: 2,
        }
    }

    /// Every manager, in both archive states where it has them.
    fn every_manager() -> Vec<Manager> {
        vec![
            Manager::Flatpak,
            Manager::Dpkg { archive: true },
            Manager::Dpkg { archive: false },
            Manager::Rpm { archive: true },
            Manager::Rpm { archive: false },
            Manager::Pacman,
            Manager::Unknown,
        ]
    }

    #[test]
    fn only_the_three_shapes_umber_owns_may_replace_themselves() {
        assert!(InstallKind::Portable.is_self_updatable());
        assert!(InstallKind::AppImage(PathBuf::from("/x")).is_self_updatable());
        assert!(InstallKind::Msi.is_self_updatable());
        for manager in every_manager() {
            let kind = InstallKind::Managed(manager);
            assert!(!kind.is_self_updatable(), "{manager:?}");
            // And every one of them has something to say instead, on every
            // architecture — including one no release is built for, where the
            // answer is a sentence with no file name in it.
            for arch in [Some(Arch::X86_64), Some(Arch::Aarch64), None] {
                assert!(
                    kind.cannot_update(&offered(), arch).is_some(),
                    "{manager:?} {arch:?}",
                );
            }
        }
        assert!(!InstallKind::Unknown.is_self_updatable());
    }

    #[test]
    fn a_managed_install_names_the_manager_it_belongs_to() {
        let flatpak = InstallKind::Managed(Manager::Flatpak)
            .cannot_update(&offered(), Some(Arch::X86_64))
            .expect("a Flatpak cannot self-update");
        assert!(flatpak.contains("Flatpak"), "{flatpak}");
        let rpm = InstallKind::Managed(Manager::Rpm { archive: false })
            .cannot_update(&offered(), Some(Arch::X86_64))
            .expect("an rpm cannot self-update");
        // Three distributions share the format with three different front ends,
        // so no command is printed — but the format is still named.
        assert!(rpm.contains("rpm"), "{rpm}");
    }

    /// **The regression this whole arrangement exists for.**
    ///
    /// 0.1.4 told every `.deb` user to run `apt install --only-upgrade umber`.
    /// There is no archive for apt to consult unless the machine has been given
    /// one, so that command answered "already the newest version" on every
    /// machine it was ever run on, and did it in the voice of something that
    /// had checked. Nothing here may print an upgrade command to a machine that
    /// has nowhere to upgrade from.
    #[test]
    fn no_upgrade_command_is_printed_to_a_machine_with_no_archive() {
        let never = [
            "apt install --only-upgrade",
            "pacman -Syu",
            "flatpak update",
            "dnf upgrade",
            "zypper update",
        ];
        for manager in every_manager() {
            let archived = matches!(
                manager,
                Manager::Dpkg { archive: true } | Manager::Rpm { archive: true },
            );
            if archived {
                continue;
            }
            for arch in [Some(Arch::X86_64), Some(Arch::Aarch64), None] {
                let message = InstallKind::Managed(manager)
                    .cannot_update(&offered(), arch)
                    .expect("a managed copy says why");
                for command in never {
                    assert!(
                        !message.contains(command),
                        "{manager:?} {arch:?} was told to run `{command}`:\n{message}",
                    );
                }
            }
        }
    }

    #[test]
    fn without_the_archive_the_message_names_the_file_to_fetch() {
        // A name somebody can search the releases page for, spelt exactly as
        // the release workflow spells it. `crates/umber-desktop/tests/release.rs`
        // is the other half of that agreement.
        let cases = [
            (
                Manager::Dpkg { archive: false },
                Arch::X86_64,
                "umber_0.1.2_amd64.deb",
            ),
            (
                Manager::Dpkg { archive: false },
                Arch::Aarch64,
                "umber_0.1.2_arm64.deb",
            ),
            (
                Manager::Rpm { archive: false },
                Arch::X86_64,
                "umber-0.1.2-1.x86_64.rpm",
            ),
            (
                Manager::Pacman,
                Arch::X86_64,
                "umber-bin-0.1.2-1-x86_64.pkg.tar.zst",
            ),
            (Manager::Flatpak, Arch::X86_64, "umber-0.1.2-x86_64.flatpak"),
        ];
        for (manager, arch, file) in cases {
            let message = InstallKind::Managed(manager)
                .cannot_update(&offered(), Some(arch))
                .expect("a managed copy says why");
            assert!(message.contains(file), "{manager:?}:\n{message}");
        }
    }

    #[test]
    fn with_the_archive_apt_is_told_to_do_the_upgrade() {
        let message = InstallKind::Managed(Manager::Dpkg { archive: true })
            .cannot_update(&offered(), Some(Arch::X86_64))
            .expect("a managed copy says why");
        assert!(
            message.contains("sudo apt update && sudo apt install --only-upgrade umber"),
            "{message}",
        );
        // And no file name, because there is nothing to go and fetch.
        assert!(!message.contains(".deb"), "{message}");
    }

    #[test]
    fn an_architecture_with_no_package_gets_a_sentence_rather_than_a_made_up_name() {
        // A source build on riscv64, or the Arch package on ARM, which is not
        // built. Inventing a file name would send somebody looking for a file
        // that has never existed.
        for (manager, arch) in [
            (Manager::Dpkg { archive: false }, None),
            (Manager::Pacman, Some(Arch::Aarch64)),
            (Manager::Flatpak, Some(Arch::Aarch64)),
        ] {
            let message = InstallKind::Managed(manager)
                .cannot_update(&offered(), arch)
                .expect("a managed copy says why");
            assert!(message.contains("releases page"), "{manager:?}:\n{message}");
            for extension in [".deb", ".rpm", ".flatpak", ".pkg.tar.zst"] {
                assert!(
                    !message.contains(extension),
                    "{manager:?} {arch:?} named a {extension} that is not built:\n{message}",
                );
            }
        }
    }
}
