# Mobile

Umber is a desktop application that has been written as though it might one day
not be. `umber-app` builds as a `cdylib`, `android_main` exists, device limits
are `downlevel_defaults`, `suspended()` drops the surface and keeps the editor,
and `resumed` rebuilds storage for every open document because Android's window
dies when the app is backgrounded. None of it has ever been built. None of it
has ever been run. This document is the research that says what building it
would actually cost, in two halves that turn out to be almost unrelated:

- **Android**, where the cost is engineering — a build system, a storage layer,
  a stylus that reaches winit only partly, and a set of desktop assumptions that
  have to be switched off. No account, no fee and no gatekeeper.
- **iPadOS**, where the engineering is *smaller* and the cost is Apple's:
  US$99 a year, for ever, with no free tier that produces a shippable file, and
  a hard technical dependency on a Mac somewhere in the pipeline.

Everything below was checked against the versions this repository actually pins
(`Cargo.lock` at workspace version 0.0.5: winit 0.30.13, android-activity 0.6.1,
ndk 0.9.0, wgpu 29.0.4, rfd 0.17.2, directories 6.0.0) or against a named source
with a date. Anything not checked says so.

**Checked on 3 August 2026.** Apple's and Google's terms move; the crate
versions move faster. Re-read section 6 before quoting a price and section 2
before quoting a tool.

---

## 1. The iPad question, answered first

> *Does iPad support require the same developer account stuff that macOS does?*

**No — it requires strictly more, and the difference is the part that matters.**

macOS has a free tier that produces a file somebody can run. iPadOS does not
have one at all.

### 1.1 What Umber does on macOS today, for comparison

`.github/workflows/release.yml` builds a universal binary on `macos-latest`,
`lipo`s the two slices together and puts it in a `.tar.gz`. There is **no
`codesign` step and no notarisation step anywhere in that workflow.** So the
macOS artefact today is an unsigned Mach-O binary in an archive, and what a user
gets is a Gatekeeper warning on first launch that they clear by hand
(right-click → Open, or `xattr -d com.apple.quarantine`) — a file downloaded
from the internet carries `com.apple.quarantine`, and an app that is both
quarantined and unsigned is blocked on first launch
([HackTricks, Gatekeeper](https://hacktricks.wiki/en/macos-hardening/macos-security-and-privilege-escalation/macos-security-protections/macos-gatekeeper.html),
read 3 Aug 2026).

That is the whole macOS position: **it works, badly, for free.** Making it work
*well* — no warning at all — needs a Developer ID certificate and notarisation,
and Apple's own membership comparison lists "Notarization & Developer ID for Mac
apps" as a Developer Program row with no free equivalent
([Apple, Choosing a Membership](https://developer.apple.com/support/compare-memberships/),
read 3 Aug 2026). So macOS today is the free tier and it ships.

### 1.2 What a free Apple Account gets you on iPadOS

A free Apple Account gets you Xcode, the simulator, and **on-device testing** —
and that is the ceiling. Apple's own comparison table puts every distribution
row in the paid column: *App distribution*, *Ad hoc distribution for testing and
internal use*, *App management, testing, and analytics with App Store Connect*
([Apple, Choosing a Membership](https://developer.apple.com/support/compare-memberships/),
read 3 Aug 2026).

"On-device testing" under a free "Personal Team" means, concretely:

| | Free Apple Account |
|---|---|
| Provisioning profile lifetime | **7 days**, then the app refuses to launch |
| App IDs registered at once | 10, each expiring after 7 days |
| Test devices | 3 per platform, expiring after 7 days |
| Apps signed at once | 3 |
| App Store / TestFlight / ad-hoc | **none** |

(Apple's membership comparison plus the free-tier Personal Team limits it
documents; corroborated by
[myByways, "New limitations imposed on free Apple Developer account"](https://mybyways.com/blog/new-limitations-imposed-on-free-apple-developer-account),
read 3 Aug 2026.)

There is no artefact here. There is no `.ipa` a stranger can install, no link to
put in the README's download table, and no equivalent of "download it and click
through one warning". Every seven days, on every device, somebody has to plug it
into a Mac running Xcode and re-sign it. **That is not a distribution channel;
it is a debugging arrangement.** It is not comparable to macOS's unsigned
tarball, and the temptation to describe it as "the same, just with a warning" is
exactly the claim this project refuses to make elsewhere.

### 1.3 What US$99 a year gets you, and whether review is avoidable

The Apple Developer Program is **99 USD per membership year**
([Apple](https://developer.apple.com/support/compare-memberships/), 3 Aug 2026).
The three routes it opens, and what each costs beyond the fee:

- **App Store.** Full App Review. For a painting application there is nothing
  obviously disqualifying, but review is a human process with a schedule
  somebody else owns, and it recurs on every release. Umber's release process
  is "push a tag"; this bolts a queue onto the end of it.
- **TestFlight.** Up to 10,000 external testers. The first build of each version
  for *external* testers goes through **Beta App Review**; up to 100 *internal*
  testers (App Store Connect users on your team) skip it. **Builds expire 90
  days after upload** — so a release from four months ago simply stops working
  for everybody who has it
  ([TestFlight distribution guide](https://techconcepts.org/blog/testflight-guide),
  read 3 Aug 2026). TestFlight is a beta channel, and it behaves like one: it
  cannot be the way somebody installs a painting application and keeps it.
- **Ad hoc.** Signed for specific devices whose UDIDs are registered in advance
  (100 per device type per membership year). Fine for a handful of testers you
  can email; useless as a public download.
- **Enterprise (Apple Developer Enterprise Program).** Requires a legal entity
  and is contractually for distribution *to your own employees*. Using it to
  ship a free painting application to the public is a straightforward violation
  and gets the certificate revoked. Not an option.
- **EU alternative distribution (DMA).** Web Distribution and alternative
  marketplaces exist, and **every route still requires Notarization by Apple**
  and a paid membership. Apple does not publish the eligibility bar for Web
  Distribution beyond "specific criteria" and "ongoing requirements"
  ([Apple, Update on apps distributed in the European Union](https://developer.apple.com/support/dma-and-apps-in-the-eu/),
  read 3 Aug 2026); the widely reported version of that bar — two continuous
  years of membership and a million first annual installs in the EU — is not
  something Apple states on that page, so this document will not assert it. The
  Core Technology Fee (€0.50 per annual install past the first million) was
  supposed to become a Core Technology Commission on 1 January 2026 and as of
  mid-2026 that transition had not happened
  ([RevenueCat](https://www.revenuecat.com/blog/growth/apple-eu-dma-update-june-2025/),
  read 3 Aug 2026). **The relevant conclusion is not the fee.** It is that the
  DMA changed *who may run a store*, not *whether Apple must sign your binary*.
  There is no unsigned, un-notarised route onto an iPad, in the EU or anywhere.

**So: review is avoidable only by making distribution useless.** App Store means
review; TestFlight means a 90-day fuse and review for external testers; ad-hoc
means a list of UDIDs. There is no combination that produces "a file on the
releases page that an iPad owner can install".

### 1.4 Is a Mac required?

**For iPadOS: yes, somewhere in the pipeline — but it does not have to be
yours.** The whole Apple toolchain (`xcodebuild`, `codesign`, the simulator, the
provisioning machinery) runs only on macOS. The realistic answer is the one
Umber already uses for its macOS build: **GitHub's `macos-latest` runner is a
Mac**, and it can build and sign an iOS app given a certificate and a
provisioning profile stored as base64 secrets
([GitHub Docs, Installing an Apple certificate on macOS runners](https://docs.github.com/actions/deployment/deploying-xcode-applications/installing-an-apple-certificate-on-macos-runners-for-xcode-development),
read 3 Aug 2026). The certificate itself can be obtained without a Mac — a CSR
generated with `openssl`, uploaded to the developer portal — so no physical Mac
need ever be bought. What cannot be avoided is the **US$99 membership**, because
without it there is no certificate to obtain.

**For macOS: no Mac is required today and none is used.** `release.yml` already
builds on a hosted runner and nothing here is signed. That is the existing
answer, and it is the one that makes the iPad comparison sharp: the macOS build
costs nothing and needs nobody's permission; the iPad build costs US$99 a year
and needs Apple's.

### 1.5 The technical side, briefly

The engineering is *not* the obstacle, and it is worth saying so clearly so that
the recommendation is not mistaken for "it would be hard".

- **wgpu on Metal is a first-class backend** and `Gpu::create_instance` takes
  whatever the platform offers. `surface_config` deliberately picks a **non-sRGB**
  surface format; Metal offers `Bgra8Unorm` alongside `Bgra8UnormSrgb`, so the
  existing `find(|f| !f.is_srgb())` succeeds and `composite.wgsl`'s explicit
  encode stays correct. Nothing about the colour-space argument changes.
- **`downlevel_defaults` is already the limit set**, and the comment in
  `gpu.rs` says it exists so a desktop build cannot depend on what an iOS device
  refuses. That bet looks sound: nothing in the pipeline asks for a feature, and
  `required_features` is `Features::empty()`.
- **winit's iOS backend does carry `Force::Calibrated`, with tilt.** This is
  checked in the vendored source, not assumed —
  `winit-0.30.13/src/platform_impl/ios/view.rs:491–503` constructs
  `Force::Calibrated { force, max_possible_force, altitude_angle }` and fills
  `altitude_angle` only when `touch.r#type() == UITouchType::Pencil`. So
  `CLAUDE.md`'s "tilt is a sentence, not a meter … winit carries one only as the
  stylus altitude inside a `Force::Calibrated`, which is iOS's form" is exactly
  right, and an iPad is the one platform where `InputLog`'s tilt row would ever
  show a number.

Three real defects sit in that path, and all three are winit's rather than
Umber's:

1. **`Force::normalized()` divides by `sin(altitude_angle)`** (`event.rs:900`).
   A pencil held nearly flat has an altitude approaching zero, so the quotient
   diverges and `PressureModel::resolve`'s `clamp(0.0, 1.0)` saturates it to
   full pressure. Worse: with a genuine `force` of `0.0` *and* an altitude of
   `0.0`, the expression is `0.0 / 0.0` — NaN — and `f32::clamp` returns NaN
   rather than clamping it. A NaN pressure would reach `radius_at`,
   `coverage_at` and the dab quad. Umber should read `Force::Calibrated`
   directly rather than through `normalized()`, which is what winit's own
   documentation on that enum advises.
2. **Only altitude, never azimuth.** `UITouch.azimuthAngle` is what says which
   *way* the pencil is leaning, and winit does not surface it.
   `InputPoint::tilt` is a `Vec2` and could only ever be given a magnitude.
3. **`UITouchPhase` is matched with a `panic!` on the default arm**
   (`ios/view.rs`, the `_ => panic!("unexpected touch phase: {phase:?}")`).
   `UITouchPhase` gained `.regionEntered`, `.regionMoved` and `.regionExited` in
   iOS 13.4 for indirect pointers — a trackpad or mouse paired with an iPad. On
   the reading of the source this is an abort, not a dropped event. **Unverified
   by anybody here** — nobody has an iPad to provoke it on — but it is the kind
   of thing that would be found in the first ten minutes of a real run.

And two things that are genuinely missing rather than broken:

- **There is no iOS entry point.** `lib.rs` has `android_main` and nothing else.
  The comment in `umber-app/Cargo.toml` — "cdylib is what Android's JNI loader
  and the iOS static link both consume" — is **wrong about iOS**: a static link
  wants `crate-type = ["staticlib"]`. A `cdylib` can be embedded as a framework,
  but that is a different arrangement from the one the comment describes, and
  either way an Xcode project with an `Info.plist`, a launch storyboard and a
  `main` that calls into Rust has to exist. `cargo-mobile2` (0.22.4, released
  29 April 2026, maintained by tauri-apps) is the tool that generates it.
- **The interface is a desktop interface**, exactly as it is on Android. See
  section 5; the argument is the same and the fix is the same.

### 1.6 Recommendation on iPad

**Do not pursue iPad now. Do Android first, and let Android answer the questions
iPad would otherwise have to answer blind.**

The reasoning:

- **The recurring cost has no floor.** US$99 every year, indefinitely, for a
  GPL-3.0 application nobody is charging for. If the fee lapses, every installed
  copy stops being installable and TestFlight builds expire on their own
  90-day clock regardless. Android's cost is US$25 once.
- **There is no honest halfway house.** Umber ships a Flatpak *bundle* and says
  plainly that it is not a Flathub listing; it ships an unsigned macOS tarball
  and lets the user click through Gatekeeper. iPadOS offers no equivalent
  degraded-but-real artefact. The only free thing on offer expires in seven days
  and requires the recipient to own a Mac. Putting "iPad" in the README's
  install table would therefore be a claim this project's own standard forbids.
- **Every interface, storage and stylus question iPad raises, Android raises
  first and cheaper.** Tablet layout, a soft keyboard, a document picker instead
  of paths, an updater that must be off, a stylus arriving as touches — all of
  it is shared. Doing Android first means the iPad work, if it ever happens, is
  a build system and an entry point rather than a rewrite of the interface.
- **Nobody here has an iPad, and nobody here has a Mac.** `docs/pen-platforms.md`
  (a sibling investigation, running concurrently) exists because nobody here has
  a pen either. This project's standing rule is that shipping a binary nobody
  has run is refused. An iPad build would be a binary nobody here can run, sold
  through a store nobody here can test against, at US$99 a year.

**The condition under which this changes:** somebody with an iPad and an Apple
Pencil offering to test it, and a willingness to carry the membership. At that
point the technical work is roughly a week — `cargo-mobile2`, an entry point, a
document picker, the three winit defects above worked around — and the
distribution question becomes "TestFlight for testers, App Store if it ever gets
that far". Until then it is a subscription to a plan.

**Rough cost comparison:**

| | Android | iPadOS |
|---|---|---|
| Account fee | US$25, once | **US$99, every year** |
| Mac required | no | yes (a hosted runner counts) |
| Free artefact strangers can install | **yes** — an APK on the releases page | **no** |
| Store review | only if you use Play | unavoidable for any real distribution |
| Store gate for a new personal account | 12 testers × 14 continuous days | App Review |
| Build tooling maturity | good (`cargo-ndk`, 528k recent downloads) | good (`cargo-mobile2`, active) |
| Stylus data through winit | pressure only | pressure **and** altitude |
| Engineering to a first running build | larger (storage, entry, gradle) | smaller (entry, storage) |

Android wins on everything except the quality of the stylus data — which is a
winit limitation on both, and which is the subject of section 4.

---

## 2. Android: the toolchain

Three candidates. One wins on maintenance alone.

### 2.1 `cargo-apk` — no

Deprecated in favour of xbuild, and the practical evidence is the release
history: **the newest version on crates.io is 0.10.0, published 30 November
2023** ([crates.io API](https://crates.io/api/v1/crates/cargo-apk), read 3 Aug
2026) — two years and eight months old at the time of writing, with the previous
release before that in December 2022. The repository still sees commits, but no
release has followed them.

Two of its gaps are disqualifying rather than merely stale:

- **16 KB page alignment.** Since **1 November 2025**, new apps and updates to
  existing apps on Google Play targeting Android 15 (API 35) or above must
  support 16 KB memory pages on 64-bit devices, and Play blocks the release
  otherwise
  ([Android Developers Blog](https://android-developers.googleblog.com/2025/05/prepare-play-apps-for-devices-with-16kb-page-size.html);
  [developer.android.com, Support 16 KB page sizes](https://developer.android.com/guide/practices/page-sizes),
  read 3 Aug 2026). NDK r28 and above compile 16 KB-aligned by default; anything
  older needs `-Wl,-z,max-page-size=16384 -Wl,-z,common-page-size=16384` passed
  explicitly. A tool released in 2023 knows about neither.
- **Target API level.** From **31 August 2026**, new apps and updates must
  target Android 16 (API 36) or higher to be submitted to Google Play, with an
  extension available to 1 November 2026
  ([Play Console Help, Target API level requirements](https://support.google.com/googleplay/android-developer/answer/11926878),
  read 3 Aug 2026). `cargo-apk` generates the manifest, so it is the thing that
  would have to know.

It is also **NativeActivity-only** — its own README describes it as "Ideal for
apps that provide a `NativeActivity` via our `ndk` crate". That happens to match
what Umber pins today (see 2.4), so it is not the reason to reject it; the dates
are.

### 2.2 `xbuild` — no

`rust-mobile/xbuild`'s **repository description says "(unmaintained)"**
([github.com/rust-mobile/xbuild](https://github.com/rust-mobile/xbuild), read
3 Aug 2026). There is a fork by NiklasEi with activity on it. Adopting an
unmaintained tool, or a personal fork of one, as the thing that stands between a
release script and a published artefact is the opposite of what this project
does everywhere else — `packaging/linux/build-packages.sh` exists precisely so
that packaging is a script somebody can run and debug rather than a black box.

### 2.3 `cargo-ndk` plus a Gradle project — **yes**

**Recommended.** `cargo-ndk` 4.1.2, published 9 August 2025, ~528,000 recent
downloads ([crates.io API](https://crates.io/api/v1/crates/cargo-ndk), read
3 Aug 2026). It does one job — point cargo at the NDK's clang, sysroot and
linker for each ABI, and drop the resulting `.so` into `jniLibs/<abi>/` — and
leaves everything else to Gradle, which is the thing Google actually maintains.

Why the division is right rather than merely popular:

- **Everything on the Google Play side becomes Gradle's problem, and Gradle is
  updated.** Target API level, 16 KB alignment, the AAB format, signing configs,
  `minSdk`, permissions, adaptive icons, the splash screen API, the `.desktop`
  equivalent — all of it is boilerplate that AGP knows and no Rust tool tracks.
  This is the same argument `release.yml` already makes about WiX and about
  `flatpak-builder`: use the platform's own packager.
- **It matches the shape of the existing packaging.** `packaging/windows/umber.wxs`
  is WiX's file. `packaging/linux/io.github.spillebulle.umber.yml` is Flatpak's.
  A `packaging/android/` holding a Gradle project is the same arrangement, and
  `taskbar::APP_ID` — `io.github.spillebulle.umber`, already the Wayland app id,
  the Flatpak id and the desktop entry's filename — is already exactly the shape
  of an Android `applicationId`. **One name across every platform** is a rule
  this codebase already states; Android should join it rather than invent a
  second spelling.
- **It is testable locally.** `packaging/android/gradlew assembleRelease` on any
  machine with a JDK and the SDK, which is the "a release process only a robot
  can run cannot be debugged" rule.

The cost is honest: **a Java/Kotlin build system enters the repository.** That is
a real weight — a `build.gradle.kts`, a `settings.gradle.kts`, a wrapper, an
`AndroidManifest.xml`, and a Gradle version somebody has to keep current. The
alternative is a Rust tool that has not been released in nearly three years.

### 2.4 `android-activity`: which flavour, and is the choice forced?

**It is forced by winit, and Umber has already made it.**

`umber-app/Cargo.toml` line 90 pins `winit = { features =
["android-native-activity"] }`. winit's own manifest maps its two features
straight onto android-activity's:

```toml
android-game-activity   = ["android-activity/game-activity"]
android-native-activity = ["android-activity/native-activity"]
```

and `android-activity/src/lib.rs` has a `compile_error!` for enabling neither or
both. So the whole dependency graph must agree on exactly one, and it is a
single global switch, not a per-crate choice.

The tradeoff, read from the vendored 0.6.1 source rather than from the docs:

| | `native-activity` (pinned) | `game-activity` |
|---|---|---|
| Needs a Java class from AndroidX Games | no | **yes** — so needs Gradle |
| Works with `cargo-apk` | yes | no |
| `AndroidApp::show_soft_input` | works (JNI to `InputMethodManager`) | works |
| `AndroidApp::text_input_state` | **returns an empty state** (`native_activity/mod.rs:435`) | real |
| `AndroidApp::set_text_input_state` | **`// NOP: Unsupported`** (line 444) | real |
| Historical motion samples | available on both, via `Pointer::history()` | same |

**The soft-keyboard consequence is the one that matters, and it lands the same
way whichever flavour is chosen — because winit throws the difference away.**
winit's Android backend (`platform_impl/android/mod.rs`) matches only
`InputEvent::MotionEvent` and `InputEvent::KeyEvent`; `InputEvent::TextEvent` and
`InputEvent::TextAction`, which are how GameActivity delivers composed IME text,
fall into `_ => warn!("Unknown android_activity input event {event:?}")`. So
switching to `game-activity` buys nothing through winit 0.30 today. What does
work on both is winit's `set_ime_allowed`, which calls `show_soft_input` /
`hide_soft_input` (`mod.rs:918–925`), plus whatever hardware-style key events the
soft keyboard emits, which winit *does* translate through
`character_map_and_combine_key` into a `logical_key` and a `text`. That is enough
for a brush name and a filename on most keyboards and is not enough to be relied
on for a proper IME.

**Recommendation: keep `native-activity`.** Not because it is better — it is
worse on paper — but because the one thing GameActivity would buy is discarded by
winit before it reaches Umber, and changing the pin is a change to a global
feature that every crate in the graph must agree on. Revisit it if and when
winit grows a `WindowEvent::Ime` path on Android. Note this in the eventual
Gradle project so nobody later "upgrades" to GameActivity expecting text input to
start working.

**A caveat this document is obliged to state:** `native-activity` plus Gradle is
a slightly unusual pairing (Gradle is normally reached for *because* of
GameActivity), and nobody here has built it. It should work — `NativeActivity`
is a stock Android class and a Gradle project can declare it in the manifest
exactly as `cargo-apk` generates it — but that is a reading of the pieces, not a
build somebody ran.

### 2.5 ABIs

**`arm64-v8a` only, and ship `x86_64` beside it.**

- **`arm64-v8a`** is every tablet made in the last decade and is the only one
  that matters for the stated target.
- **`armeabi-v7a`** is 32-bit ARM. Google Play has required a 64-bit version
  alongside any 32-bit one since 2019 and now requires 64-bit-capable devices to
  be served 64-bit code. A drawing application on a 32-bit tablet is going to run
  out of address space on the first large canvas anyway — a 10000² layer is
  400 MB and `MAX_LAYERS` is 64. **Do not build it.**
- **`x86_64`** is worth building, and the reason is not users: it is **the
  emulator**. Nobody here has an Android tablet either, so the emulator on a
  desktop machine is the *only* way anybody working on this can look at the
  result at all. That makes an x86-64 slice a development requirement rather than
  a distribution one. It roughly doubles the APK, which is why the release should
  build **per-ABI APKs** (or an AAB, which does the split automatically) rather
  than one fat binary — and an emulator slice can be a debug-only build if the
  size is ever a problem.
- **`riscv64-linux-android` is Tier 3** in rustc
  ([Rust platform support: Android](https://doc.rust-lang.org/nightly/rustc/platform-support/android.html),
  read 3 Aug 2026), and the README already refuses RISC-V on desktop for the
  reason that nobody could run it. Same answer.

`aarch64-linux-android` and `x86_64-linux-android` are both **Tier 2** in rustc,
which is the same tier as `aarch64-pc-windows-msvc` — a platform Umber already
ships to.

### 2.6 The NDK, and the 16 KB trap

Rust "supports the most recent Long Term Support (LTS) edition of the Android
NDK" and all API levels the NDK supports
([Rust platform support: Android](https://doc.rust-lang.org/nightly/rustc/platform-support/android.html),
read 3 Aug 2026).

**Use NDK r28 or newer and nothing older.** r28+ links 16 KB-aligned by default;
r27 and below need `-Wl,-z,max-page-size=16384 -Wl,-z,common-page-size=16384`
passed through, and a linker flag that has to be remembered is a linker flag that
will be forgotten. Verify with `llvm-objdump -p` and look for `align 2**14`.

This belongs in CI as an assertion rather than a hope, in the same spirit as
`packaging/check.sh`: a build step that greps `llvm-objdump` output for the
alignment and fails the job otherwise. Play will reject a misaligned upload; a CI
step tells you two years earlier, and for free.

---

## 3. What has to change in the code

Everything in this section was found by reading the tree. None of it has been
compiled for Android, because six other agents were building concurrently and
the brief forbade it. Line numbers are from workspace version 0.0.5.

### 3.1 The updater must be off, and today it would be actively dangerous

This is not a preference; it is a bug waiting in the current source.

`update/install.rs:42` defines:

```rust
pub const CURRENT: Os = if cfg!(target_os = "windows") {
    Os::Windows
} else if cfg!(target_os = "macos") {
    Os::Mac
} else {
    Os::Linux            // ← Android lands here
};
```

`target_os = "android"` is not `"linux"`, so **an Android build would classify
itself as Linux**, and `Arch::CURRENT` on `aarch64` is `Some(Aarch64)`. With no
`/.flatpak-info`, no `$APPIMAGE` and an executable path under `/data/app/...`
that no package database claims, `install::detect` would most likely answer
`InstallKind::Portable` — the arm whose `is_self_updatable()` is `true`. Umber
would then offer to download `umber-0.0.5-aarch64-unknown-linux-gnu.tar.gz` and
**swap a glibc desktop binary over itself**, on a device where the executable is
not even writable.

The fix is the Flatpak's, exactly, and the argument is already written down for
it: *"its sandbox is granted no network — a request could only time out and
report a decision as a failure, and Flatpak's own updater already does the job."*
A Play Store or sideloaded APK is the same case. Concretely:

- Add an arm to `install::Manager` (`Store`, or `PlayStore`) and return it from
  `detect` under `cfg!(target_os = "android")`, ahead of every path-based test —
  the same "before anything that looks at paths" ordering the Flatpak check
  already has and for the same reason.
- `InstallKind::Managed(_)` already makes `is_self_updatable()` false, so `apply`
  and `swap_in` become unreachable by construction rather than by a second gate.
- Extend `Updates::check_unavailable` to answer for it, so `start_if_due` and
  `check` both return early through the one switch that already exists.
- Its sentence should say what is true: the store keeps this copy current, and
  Umber does not check for itself. It must **not** say anything about network
  access unless the manifest actually withholds `android.permission.INTERNET` —
  and the honest thing is to withhold it. A painting application that does not
  reach the network is one whose Play data-safety declaration is a single
  sentence, and `ureq`, `rustls` and `webpki-roots` can then be `cfg`'d out of
  the build entirely.

`Os::CURRENT` should probably gain an explicit `Android` arm regardless, so that
the fall-through never silently classifies a fourth platform.

### 3.2 Storage: three call sites, one answer

Umber finds its data directory in exactly three places:

- `autosave.rs:107` — `internal_dir()`
- `crash/mod.rs:390` — the reports directory
- `prefs.rs:181` — the preferences file

all through `directories::ProjectDirs::from("", "", "Umber")`, plus
`umber_core::preset::UserLibrary::default_dir` for the brush library, which
`autosave.rs`'s own comment says finds it the same way.

**`directories` has no Android support.** It compiles — `dirs-sys` 0.5.0 has an
Android arm at `src/lib.rs:39`, but only to make the `getpwuid_r` fallback return
`None` — so the whole thing reduces to `$XDG_DATA_HOME` or `$HOME/.local/share`.
On an Android app process `HOME` is not a writable per-app directory, and there
is no XDG anything. The result is not a compile error and not a panic: it is
**silent failure to write**, which is the worst shape this bug takes, since the
autosave's whole purpose is to be there when nothing else is.

The answer is already in the tree: `android-activity`'s `AndroidApp` exposes
`internal_data_path()` and `external_data_path()` (`lib.rs:1272`, `:1277`), and
winit hands `AndroidApp` back through
`winit::platform::android::ActiveEventLoopExtAndroid::android_app()`. So the
whole of it is one function — the Android arm of a `data_root()` that the four
call sites go through instead of calling `ProjectDirs` each — plus threading
`AndroidApp` (or the resolved path) in once at start-up. Given that this rule
already has four statements of it, collapsing them to one is worth doing even
without Android.

**`autosave::Reaper`'s containment rules survive untouched, and this is worth
stating because it is the thing that would be easy to get wrong.** The Reaper is
run against the directory an internal copy was just written to, which is always
`internal_dir()`; it never sees a document the user chose. `internal_data_path()`
is a real POSIX path on a real filesystem — `/data/user/0/<applicationId>/files`
— so `canonicalize`, `symlink_metadata`, "the candidate's parent must equal the
root", and "no recursion" all mean exactly what they meant on Linux. **A content
URI never reaches it.** Do not add a "path or URI" abstraction under the Reaper
to make it "Android-ready"; that would loosen the one piece of this codebase that
is deliberately paranoid, for nothing.

### 3.3 Files the user chooses: the Storage Access Framework

`rfd` 0.17.2 has backends for `gtk3`, `xdg_desktop_portal`, `macos`, `win_cid`
and `wasm` (`rfd-0.17.2/src/backend.rs`) and **none for Android**. The trait
implementations are gated on those targets, so the five call sites —
`app.rs:1568`, `:1771`, `:2612` and `brushlib.rs:1287`, `:1662` — would fail to
*compile*, not fail at runtime. `rfd` has to be `cfg`'d out.

Android's answer is the Storage Access Framework: `ACTION_OPEN_DOCUMENT` and
`ACTION_CREATE_DOCUMENT` return a `content://` URI, not a path. There is no
mature Rust crate for this. The realistic design:

- **Do the Intent in Kotlin**, in the Gradle project, where the activity-result
  API lives and where it is ordinary code. Umber already has a Java-shaped
  boundary; adding one small file there is cheaper than a JNI dance across it.
- **Hand Rust a file descriptor, not a URI.** `ContentResolver.openFileDescriptor`
  gives a `ParcelFileDescriptor`; `detachFd()` gives an `int`; `File::from_raw_fd`
  gives a `std::fs::File`. `zip` 8 reads and writes a `Read + Seek` / `Write +
  Seek`, and `docformat::encode` already separates encoding from
  `write_encoded` — the two halves the autosave needed apart. So the ORA reader
  and writer need **no change at all**; only the callers that name a path do.
- **`docformat::write_encoded`'s temp-and-rename does not survive a content
  URI.** There is nowhere to put a temporary neighbour of a `content://`
  document, and no rename. This is a real loss of a real guarantee, and the
  correct response is to say so rather than to pretend: on the SAF path the
  archive is built **entirely in memory** and written to the descriptor in one
  go. That is not atomic — a process killed mid-write truncates the user's file
  — which is a good reason for the **autosave to keep using the internal
  directory and the real filesystem**, where `write_encoded` works exactly as it
  does today. The internal copy is the safety net; the SAF write is the export.
- **`MANAGE_EXTERNAL_STORAGE` is not the shortcut.** It would restore real paths
  and it is a Play-restricted permission granted essentially only to file
  managers and backup tools. Asking for it to avoid writing a document picker is
  the kind of thing that gets an app removed.

**Where the brush library lives is the interesting sub-question.**
`UserLibrary` is *a directory* — `brushes/brushes.ron` plus `brushes/tips/*.png`
— for reasons `CLAUDE.md` argues at length, and none of those reasons is about
paths. Internal storage gives it a real directory with real atomic writes, so it
should live there and **not** go through SAF. The cost is that it is invisible to
the user and disappears on uninstall; the mitigation is an explicit
export/import of the library as one archive, which is a feature that would be
welcome on desktop too.

### 3.4 Desktop assumptions that must be switched off

Each of these is a small `cfg`, and each would otherwise be a visible failure:

- **`autosave::reveal`** spawns `explorer` / `open` / `xdg-open`. None exists on
  Android. The Settings row should not be drawn at all — a button that does
  nothing is the control this project refuses everywhere.
- **The crash reporter's second process cannot work.** `crash::install_hook`
  writes a report and spawns `current_exe()` with `--crash-report <path>`. On
  Android the process is launched by the runtime, `current_exe` is not a thing
  you re-exec, and spawning a second copy of an activity is not how the platform
  works. The design **already names the right fallback** — *"an unwritable
  report, an unspawnable child and a window that will not open all end with the
  process dying exactly as it does today"* — so the Android answer is: write the
  report file (into `internal_data_path()`, where it is genuinely useful) and
  stop there. Note that `android_main` does not currently call `crash::install_hook`
  at all, because it does not go through `run()`. It should, once the spawn is
  gated.
- **`taskbar::claim_identity`** is already a Windows-only no-op elsewhere;
  nothing to do beyond confirming it.
- **`update::sweep_previous_binary`** likewise — and `android_main` does not call
  it, since it bypasses `run()`.
- **`localtime`** uses `libc` under `cfg(unix)`. Android *is* unix and bionic has
  `localtime_r`, so this should work; **unverified**.
- **`keylayout`** is Windows-only and falls back to `us_key_name`, which is
  correct: an Android soft keyboard has no layout to ask about.
- **Multi-window / tabs.** `Session` and the tab strip are Umber's own, not the
  window system's, so several documents open at once works unchanged. Good.

### 3.5 Window insets

winit's Android `Window::inner_size()` returns the `NativeWindow` dimensions —
the whole display. The status bar, the navigation bar and any display cutout are
**not** subtracted. Umber would draw the menu bar under the status bar and the
status bar under the tab strip.

`winit::platform::android::WindowExtAndroid::content_rect()` is the answer, and
it exists in 0.30.13 (`src/platform/android.rs:97–106`). It should feed the same
place `Editor::canvas_pivot` already comes from — the central panel's rect — so
that `Camera`'s pivot and `CompositeParams::pivot` stay the one value they are
required to be. Alternatively, ask for edge-to-edge in the manifest and inset in
egui; either way it is one number threaded to one place, not a layout rewrite.

---

## 4. The stylus

This is the point of a tablet, so it is worth being exact. Everything here is
read from `winit-0.30.13/src/platform_impl/android/mod.rs` and
`android-activity-0.6.1/src/`, not from documentation.

### 4.1 What arrives

`handle_input_event`, at `mod.rs:413–420`, constructs exactly one thing:

```rust
event: event::WindowEvent::Touch(event::Touch {
    device_id,
    phase,
    location,
    id: pointer.pointer_id() as u64,
    force: Some(Force::Normalized(pointer.pressure() as f64)),
}),
```

`pointer.pressure()` is `android-activity`'s wrapper on `AMOTION_EVENT_AXIS_PRESSURE`.
So: **pressure works, and arrives through the touch arm `app.rs:3441` already
handles.** Umber's whole pen path — `gesture::contact`, `drawing_touch`, the
pinch, the hover, `pointer_pressed` — is reached without a line of new code.
That is the payoff of the `gesture.rs` refactor, and it is real.

### 4.2 The `None` / `0.0` ambiguity does not exist on Android

`CLAUDE.md`'s pressure rules are built around winit's Windows path, where
`normalize_pointer_pressure` accepts `1..=1024` and answers `None` for
everything else — so a genuine zero from a lifting pen is indistinguishable from
a mouse with no sensor, and `PressureModel::resolve` settles it with a per-stroke
`sensed` latch.

**Android has no such ambiguity.** `force` is *always* `Some(...)` on that path —
the expression is a literal `Some(Force::Normalized(...))`, with no `Option` in
the chain — so `resolve` takes the `Some(p)` arm on the first sample of every
stroke, sets `sensed`, and the `None` arms are never reached. **The latch is a
no-op on Android and is not harmed by being one.** Nothing needs changing, and
nothing needs a new test; the model is simply already correct here for a
different reason than it is on Windows.

There is a *different* pressure question on Android, and it is worth naming so it
is not mistaken for the first: **a finger is not a pen, and Android reports both
through the same axis.** A device with no pressure sensor reports 1.0 for a
finger; many report something derived from contact area, which is neither 1.0 nor
a pressure. So a finger stroke may come out faint for a reason the artist cannot
see. The fix is not in `PressureModel` — it is to know what kind of pointer it
is, which brings us to:

### 4.3 What does *not* arrive, and it is the interesting half

winit forwards `pressure()` and **nothing else**. Read from the sources:

| Android has | Reachable from `android-activity` | Surfaced by winit 0.30.13 |
|---|---|---|
| `AXIS_PRESSURE` | `Pointer::pressure()` | **yes**, as `Force::Normalized` |
| `AXIS_TILT` (stylus tilt from vertical) | `Pointer::axis_value(Axis::Tilt)` | **no** |
| `AXIS_ORIENTATION` (the direction it leans) | `Pointer::orientation()` | **no** |
| `getToolType()` — finger / stylus / eraser / mouse | `Pointer::tool_type()` | **no** |
| `getButtonState()` — barrel button, eraser end | `MotionEvent::button_state()` | **no** |
| Batched historical samples | `Pointer::history()` | **no** |
| Hover (`ACTION_HOVER_*`) | via `MotionAction` | **no** — falls into `_ => None` |
| Mouse (`ACTION_SCROLL`, button press) | via `MotionAction` | **no** — same arm |

Five of those are worth a sentence each:

- **Tilt.** Android has a genuinely better tilt story than any other platform
  Umber runs on: `AXIS_TILT` is the angle from vertical and `AXIS_ORIENTATION`
  is the direction, which together are exactly the `Vec2` that
  `InputPoint::tilt` was declared to hold and has never been given. winit gives
  neither. (Compare iOS, section 1.5: winit gives altitude but not azimuth. So
  *no* platform hands winit a complete tilt vector today, which is the thing
  `docs/brushes.md` says when it notes there is "no tilt input on any platform
  it runs on".)
- **`ToolType` is the missing piece for palm rejection**, and palm rejection is
  the difference between a tablet you can draw on and one you cannot. Without it
  Umber cannot tell a stylus from the heel of a hand, and `gesture::contact`'s
  "a second contact is a pinch" rule will fire on a resting palm and abandon the
  stroke. This is the single most consequential gap in the table.
- **Buttons.** No barrel button and no eraser-end detection.
- **Historical samples.** Android batches several stylus samples into one
  `MotionEvent` — a 240 Hz pen into a 120 Hz frame is two per event — and winit
  reads only the newest. Umber's `StrokeBuilder::emit_segment` lerps pressure
  between consecutive samples, so the result is smooth rather than steppy, but it
  is smoothed-over data rather than the data. Worth knowing before anyone blames
  the brush engine for a stroke that looks less lively than the same pen in
  Krita.
- **No mouse and no hover at all.** `MotionAction`'s match has
  `_ => None // TODO mouse events`, so an Android tablet with a keyboard case and
  a trackpad — a Galaxy Tab or a Chromebook — produces no `CursorMoved`, no
  `MouseInput`, no `MouseWheel`. Every mouse-driven path in `app.rs` is dead on
  Android. That is survivable (the whole point of `gesture.rs` is that the touch
  arm reaches every gesture) but it should be said out loud, because "it worked
  under a mouse and did nothing under a pen" is a bug this codebase has already
  had three times, and Android is that sentence with the words swapped.

### 4.4 Where this leaves the design, and a note for the supervisor

**All six missing signals are reachable without forking winit**, because
`android-activity`'s `AndroidApp` is available through
`ActiveEventLoopExtAndroid::android_app()` and `AndroidApp::input_events_iter()`
gives the raw `MotionEvent`. A second, Android-only reader could pull `Tilt`,
`Orientation` and `ToolType` alongside winit's touches. That is a genuinely bad
idea to reach for first — two consumers of one input queue, racing, with winit's
`InputStatus` handling in the middle — and it is the shape of thing that ends up
as a second implementation of the pointer path. The better answers, in order:

1. Upstream it. winit's `Touch` would need new fields; that is a winit API
   change, not a patch.
2. Use `enable_motion_axis` (which `AndroidApp` exposes) plus a winit fork, if it
   ever becomes worth carrying one.
3. Live without tilt, which is where every other platform already is.

**For the supervisor, explicitly:** a sibling investigation,
`docs/pen-platforms.md`, is researching pen pressure and tilt on macOS and Linux
and will hit the same winit question from the other side. The two documents
should be reconciled, because the answer is one answer: **winit's `Touch::force`
is the only stylus channel that exists in 0.30.13 on any platform, and tilt
reaches it only as iOS's `altitude_angle` inside `Force::Calibrated`.** If that
document proposes a winit fork, a patch, or an upstream PR for macOS/Linux, the
Android tilt gap and the missing `ToolType` belong in the same change, and this
document's section 4.3 is the list of what Android would want from it. Note also
one small contradiction to settle: `CLAUDE.md` says "Touch screens report real
pressure via winit's `Force`, and so — on Windows — does a pen", which is true
and, read carelessly, sounds like Android is already covered. It is covered for
*pressure* and for nothing else.

---

## 5. The interface on a tablet

The design is a desktop design and its numbers are in `theme::metrics`:
`PANEL` 264, `TOOL_RAIL` 100, `BRUSH_LIBRARY` 780×540, `BRUSH_EDITOR` 560×600,
`MODULE_LIBRARY_WIDTH` 470, `UPDATE_DIALOG_WIDTH` 560, and rows measured in
tens of pixels. Two separate problems, and only one of them is hard.

### 5.1 Density — mostly solved already

An iPad Pro is 2732×2048 at roughly 264 dpi; a good Android tablet is similar.
winit's Android `scale_factor()` is `density / 160.0`
(`platform_impl/android/mod.rs:1095`), so a 2× or 3× tablet reports 2.0 or 3.0
and egui's points already carry it. The panels are **not** going to be 264
physical pixels wide. That half works out of the box.

What is left is that a finger is about 9 mm and a 16-pixel row at 2× is about
1.5 mm. The existing **interface scale preference** (`prefs.rs:78`,
`0.75..=2.0`, applied as `ctx.set_zoom_factor`) is the right control and it
already exists — a tablet should simply **default it higher**, around 1.25–1.5,
rather than 1.0. That is one platform-conditional default, not a redesign, and
it is exactly the kind of change `prefs.rs`'s clamping already tolerates.

The controls that would still be too small are the ones already known to be
small: `PICK_HIT`'s 18 px tick boxes, `SLIDER_KNOB`'s 11, `SCROLL_BAR`'s 6, and
`DROPDOWN`'s 18. Raising the scale raises all of them together, which is the
point of having one number.

### 5.2 Space — the real problem

A landscape tablet is roughly a 16:10 rectangle. Two 264-point sidebars plus a
100-point rail is 628 points of chrome, and on a 1366×1024-point iPad-class
surface that is **46% of the width gone before any canvas is drawn**. In
portrait it is worse than half.

The good news is that the fix already exists and is not a mobile feature:
**`dock.rs` is a model with no drawing in it**, `PanelKind::ALL` is every module,
`DEFAULT_DOCK` is the shipped arrangement, and *"a layout file written before a
module existed does not name it, and an absent panel is a closed one."* So a
tablet arrangement is **a different `DEFAULT_DOCK`**, not different code — one
sidebar rather than two, the rail, and everything else reached from the Window
menu. That change is testable without a window, which is what `dock.rs` is for.

The modals are the part that will not go quietly. `BRUSH_LIBRARY` at 780×540
points is fine on an iPad-class landscape surface and does not fit portrait.
`metrics` already states these in one place, which is the whole reason it exists
— *"use them instead of re-typing 264.0 or 36.0 at the call site"* — so making
the browser's size a function of the available rectangle is a change in one file.
The settings dialog is in better shape: it is deliberately **one size with one
vertical `ScrollArea` and `auto_shrink([false, false])`**, so it degrades by
scrolling rather than by overflowing.

**Do not design a phone interface.** The brief says tablets, and the honest
reason is stronger than the brief: `metrics::PANEL` at 264 points cannot become
a phone control by scaling, and a phone build would be a second interface to
keep in step with the design project's "Umber app" screen. Set a **minimum
short-edge size** in the manifest's supported screens, or simply state in the
listing that Umber is for tablets, and refuse the rest.

### 5.3 One thing that is genuinely missing

**There is no soft-keyboard story.** Section 2.4 has the detail: winit can *show*
the keyboard via `set_ime_allowed`, and what comes back is whatever key events
the IME emits, translated by winit through the device's `KeyCharacterMap`.
`ui::draw` already calls `shortcuts::set_typing(ctx.text_edit_focused())` once
for the whole interface, so the *dispatch* half is correct and needs nothing; a
real `TextEdit` gaining focus would need to trigger `set_ime_allowed(true)`,
which egui-winit may or may not already do on this backend — **unverified**.

Brush names, collection names, layer names, filenames and the shortcut recorder
all want text. Expect this to be the roughest edge in the first build.

---

## 6. CI and release

The rule is stated in `ci.yml`'s own comment and it is not negotiable:
**"`ci.yml`'s matrix must cover every runner `release.yml` builds on"**, or the
pre-tag CI wait that `tools/release.ps1` performs is a gate with a hole in it.
v0.0.5 was tagged green and failed on `windows-11-arm` for exactly this reason.

### 6.1 Shape of the jobs

Android is unlike every other target in one way that makes this easier: **the
build is cross-compilation, so it does not need its own runner architecture.**
`ubuntu-latest` with the Android SDK and NDK builds the `arm64-v8a` slice
perfectly well. So:

- **`ci.yml`** gains one job, not a matrix row. It should do the two things a
  release would do and nothing more: `cargo clippy --target
  aarch64-linux-android -p umber-app` and `cargo build --target
  aarch64-linux-android -p umber-app`. **It cannot run `cargo test`** — the
  tests are host tests, and the GPU tests need a device. A cross-compile that
  compiles is the whole of what CI can honestly assert here, and saying so is
  better than an emulator job that is slow, flaky and still not a tablet.
- **`release.yml`** gains an `android` job alongside `flatpak` and `arch`,
  producing per-ABI APKs (or one AAB) from `packaging/android/`. It fits the
  existing shape exactly: a job of its own because the SDK is a gigabyte the
  build matrix has no use for, which is verbatim the reason the Flatpak job
  gives.
- **`ci.yml`'s `packaging` job** should parse the new files the way it already
  parses `umber.wxs` and the Flatpak manifest — `AndroidManifest.xml` is XML and
  costs milliseconds. And `packaging/check.sh` should learn the Android
  `applicationId`, so that `taskbar::APP_ID` and the Gradle `applicationId`
  cannot drift; that is the same test that already pins the Wayland app id and
  the X11 class against `packaging/`.

### 6.2 Signing

An Android APK **must** be signed to install at all — there is no unsigned
sideload. This is the one genuinely new secret.

- A keystore (`.jks`), base64-encoded, plus the store password, the key alias and
  the key password: four repository secrets, e.g. `ANDROID_KEYSTORE_BASE64`,
  `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS`, `ANDROID_KEY_PASSWORD`.
- **The keystore is unrecoverable and permanent.** An APK signed with a
  different key is a *different application* to Android: it will not upgrade an
  installed copy, it will not upgrade a Play listing, and there is no appeal.
  Back it up somewhere that is not a CI secret store. (Play App Signing exists
  and lets Google hold the app signing key while you hold only an upload key,
  which is a real mitigation and only applies if Play is used at all.)
- The `workflow_dispatch` rehearsal path must not need the secrets — a fork or a
  rehearsal should still build a **debug-signed** APK so the workflow can be
  exercised without them, exactly as `publish` is gated on `refs/tags/v` so a
  dispatch can build everything and publish nothing.

### 6.3 Where the APK goes

**On the releases page, beside the Flatpak bundle, and initially nowhere else.**

The Flatpak precedent is precise and should be followed word for word:
*"This produces the bundle attached to the release. It is **not** a Flathub
submission: Flathub builds from source on its own infrastructure and that is a
separate piece of work."* An APK on the releases page is the same statement. The
README's download table gains an Android row and the "what is not there yet"
section says it is not on Play.

The Play Store is a separate decision with a separate cost, and it is larger than
the money:

- **US$25, once**, non-refundable, plus identity verification with document
  uploads
  ([Play Console registration guides](https://afkarsoftware.com/en/blog-detail/google-play-console-account-2026-one-time-25-fee/),
  read 3 Aug 2026).
- **A personal developer account created after 13 November 2023 cannot publish
  to production until it has run a closed test with at least 12 testers opted in
  continuously for the last 14 days**, followed by a reviewed application form
  ([Play Console Help, App testing requirements for new personal developer accounts](https://support.google.com/googleplay/android-developer/answer/14151465),
  read 3 Aug 2026). For a solo project with no user base, finding twelve people
  with Android tablets willing to keep a test build installed for a fortnight is
  a harder problem than any of the engineering in this document.
- **Target API 36 by 31 August 2026** and **16 KB pages since 1 November 2025**,
  as section 2 covers — both of which are Gradle's problem, but only if Gradle is
  what builds it.

So: **APK first, Play later or never.** The APK is a real artefact somebody can
install today; the Play listing is a project.

---

## 7. What nobody here can check

Stated plainly, because this project's rule is that shipping a binary nobody has
run is refused, and everything below would be exactly that.

**Nobody working on Umber has an Android tablet, an iPad, an Apple Pencil, a
Mac, or a stylus of any kind.** `umber-app/src/inputlog.rs` exists as a whole
module because of the last of those. So:

| Claim | Status |
|---|---|
| The Rust cross-compile to `aarch64-linux-android` succeeds | **untested** — CI can settle it cheaply and should be the first thing built |
| wgpu picks Vulkan and gets a non-sRGB surface format on a real Adreno/Mali | **untested**, reasoned only |
| `downlevel_defaults` is actually satisfied by a real mobile adapter | **untested**; this is the bet the limit set was chosen to make, and it has never been collected on |
| `suspended` / `resumed` genuinely rebuild every document's storage on a real backgrounding | **untested** — this is the path `CLAUDE.md` says is Android's, and it has never run |
| Pressure from a real Android stylus reaches `PressureModel` sensibly | **untested**; the code path is confirmed from source, the *values* are not |
| A finger without a pressure sensor does not paint faint | **untested**, and section 4.2 says why it might |
| The soft keyboard produces usable text through winit | **untested and doubtful** |
| The interface at 1.25 scale on a 2732×2048 tablet is usable | **untested** — an emulator would settle it, which is why x86_64 is worth building |
| `localtime` works against bionic | **untested** |
| Memory: 64 layers of a large canvas on a tablet | **untested**, and a per-app memory limit is a real ceiling a desktop does not have |
| Anything at all about iPadOS | **untested**, and cannot be tested by anyone here at any price under US$99/yr |

**The first milestone should therefore be the smallest one that produces
evidence:** a `cargo build --target aarch64-linux-android -p umber-app` in CI,
which costs one job and settles whether the crate graph even compiles for the
platform. Everything after that needs a device or an emulator, and the emulator
is why `x86_64` is on the ABI list.

---

## 8. Summary of recommendations

**Android — do it, in this order.**

1. **`cargo build --target aarch64-linux-android` in CI first.** One job, no
   Gradle, no device. It either compiles or it names the crates that do not
   (`rfd` will be one).
2. **`cargo-ndk` + a Gradle project in `packaging/android/`.** Not `cargo-apk`
   (last release November 2023), not `xbuild` (repository says unmaintained).
3. **Keep `native-activity`.** GameActivity's one advantage is discarded by
   winit 0.30 before it reaches Umber. Write the reason into the Gradle project.
4. **`arm64-v8a` to ship, `x86_64` for the emulator, nothing else.** NDK r28+;
   assert `align 2**14` in CI.
5. **Turn the updater off properly** — an `install::Manager` arm and a
   `check_unavailable` sentence — before anything else, because the current code
   would classify Android as a portable Linux install and offer to overwrite
   itself with a glibc tarball.
6. **One `data_root()` for the four `ProjectDirs` call sites**, Android-backed by
   `AndroidApp::internal_data_path()`. The autosave, the preferences, the crash
   reports and the brush library all live there and `Reaper` is unchanged.
7. **SAF for the user's own documents only**, through a file descriptor, with the
   loss of `write_encoded`'s atomicity stated rather than hidden.
8. **A tablet `DEFAULT_DOCK` and a higher default interface scale.** Not a
   redesign.
9. **APK on the releases page.** Play is a separate decision; the 12-tester rule
   is its real price.

**iPad — no, not now.** US$99 every year against Android's US$25 once; no free
tier that produces an installable file; App Review or a 90-day TestFlight fuse as
the only distribution; a Mac somewhere in every pipeline. The engineering is the
smaller half and the account is the larger, which is the direct answer to the
question that was asked: **it is not the same as macOS, it is worse, and the
difference is that macOS still has a free path and iPadOS has none.**

Revisit it when somebody with an iPad, a Pencil and a willingness to carry the
membership asks for it — and note that the tilt data waiting on an iPad is the
best of any platform Umber runs on, which is the one genuine reason to want it.
