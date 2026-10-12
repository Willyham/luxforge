//! The bridge to Sparkle 2 on macOS (`docs/design/auto-update.md#macos-sparkle`).
//!
//! A release bundle embeds `Contents/Frameworks/Sparkle.framework`. [`Sparkle::start`] loads it at
//! run time through `NSBundle`, so no build links it: a development build, a test bundle and
//! `cargo test` never need it, and a bundle without it is told so ([`framework_present`]) before
//! anything is loaded.
//!
//! Sparkle is driven on AppKit's main thread, which is the desktop's UI thread. Every [`Command`]
//! is queued onto the main dispatch queue and returns at once, so no caller ever waits on Sparkle,
//! and Sparkle does its own network and disk work off that thread. The updater runs with a user
//! driver and a delegate implemented here: no Sparkle window is ever shown, and every event Sparkle
//! raises is handed to the [`Listener`] as an [`Event`] of plain values, on the main thread. The
//! listener must only record it and wake whoever waits, never block.
//!
//! Sparkle's reply blocks are held here, one of each kind, and answered by [`Command::Answer`]: the
//! offer of an update found ([`Reply::Found`]) and the readiness of a downloaded one
//! ([`Reply::Ready`]). A relaunch the delegate postponed is resumed by [`Command::Resume`].
//!
//! The decisions are not made here. Which channel is followed, whether a relaunch must wait for the
//! catalog to close and what to answer are the listener's: the desktop's port.

#[cfg(target_os = "macos")]
mod macos;

use std::path::{Path, PathBuf};

/// Where a bundle embeds Sparkle: `<bundle>/Contents/Frameworks/Sparkle.framework`.
pub fn framework_path(bundle: &Path) -> PathBuf {
    bundle.join("Contents/Frameworks/Sparkle.framework")
}

/// Whether `bundle` embeds the framework, by its loadable binary, without loading it.
pub fn framework_present(bundle: &Path) -> bool {
    framework_path(bundle).join("Sparkle").is_file()
}

/// The app bundle an executable runs from: `<bundle>/Contents/MacOS/<executable>`. `None` for an
/// executable outside a bundle, such as one `cargo` built.
pub fn bundle_of(executable: &Path) -> Option<PathBuf> {
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app")
        .then(|| bundle.to_path_buf())
}

/// How far an update Sparkle offers has come, as `SPUUserUpdateState` tells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Found and not downloaded yet.
    NotDownloaded,
    /// Downloaded, not yet handed to the installer.
    Downloaded,
    /// Handed to the installer, which installs it whenever this app terminates, unless the offer
    /// is answered with [`Choice::Skip`].
    Installing,
}

/// One appcast item Sparkle found newer than this bundle, read from its `SUAppcastItem`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// `sparkle:shortVersionString` (`displayVersionString`).
    pub version: String,
    /// `sparkle:version` (`versionString`), the build number.
    pub build: String,
    /// The item's `<link>` (`infoURL`), the release page.
    pub link: Option<String>,
    /// `luxforge:catalogFormat`, as the item's properties hold it.
    pub catalog_format: Option<String>,
    /// `sparkle:channel`; none for the default channel.
    pub channel: Option<String>,
    /// The enclosure's length.
    pub length: u64,
}

/// The kind of check Sparkle starts (`SPUUpdateCheck`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckKind {
    /// A check the person asked for: `update.check`.
    Asked,
    /// Sparkle's own scheduled check, or the launch's.
    Background,
    /// A probe for information only, which offers nothing.
    Information,
}

/// What Sparkle reports, on the main thread.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The updater could not be created or started; it does nothing more.
    Failed(String),
    /// A check started.
    CheckStarted(CheckKind),
    /// A check found `item` ([`Reply::Found`] is held until answered).
    Found { item: Item, stage: Stage },
    /// A check found nothing newer.
    NotFound,
    /// A check, download, extraction or installation failed.
    Error(String),
    /// The update cycle a check started ended, with its error if it failed.
    CycleFinished { error: Option<String> },
    /// The download started.
    DownloadStarted,
    /// The download's stated length.
    DownloadLength(u64),
    /// More bytes arrived: the count of this delivery, not a total.
    DownloadReceived(u64),
    /// The archive is downloaded and being verified and extracted.
    Extracting,
    /// The update is ready to install ([`Reply::Ready`] is held until answered).
    Ready,
    /// The relaunch is postponed until [`Command::Resume`], as the listener asked.
    RelaunchPostponed,
    /// The installer took the update and the app is asked to terminate, if it has not already.
    Installing { terminated: bool },
    /// Sparkle's update session ended: any held reply is gone.
    Dismissed,
    /// A command could not be carried out, such as a check while a session runs.
    Refused(String),
}

/// An answer to a held reply (`SPUUserUpdateChoice`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// Download, extract or install. On [`Reply::Ready`], install and relaunch now.
    Install,
    /// Not now; an installing update still installs when the app terminates.
    Dismiss,
    /// On [`Reply::Ready`] or an installing offer, cancel the installation. Sparkle also records a
    /// skip on an offer, which [`Command::Configure`] clears.
    Skip,
}

/// Which held reply an answer is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply {
    /// `showUpdateFoundWithAppcastItem:state:reply:`.
    Found,
    /// `showReadyToInstallAndRelaunch:`.
    Ready,
}

/// What the bridge can be asked, on any thread; each is queued onto the main thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Apply the person's choice of automatic checks. The first creates and starts the updater.
    /// Sparkle never downloads on its own (`automaticallyDownloadsUpdates` is set off every time),
    /// and the skip Sparkle records is cleared, so the person's preferences, held by Luxforge, are
    /// the only ones that apply.
    Configure { automatic_checks: bool },
    /// Check now, as the person asked (`checkForUpdates`).
    Check,
    /// Check in the background (`checkForUpdatesInBackground`), as Sparkle's schedule does.
    CheckInBackground,
    /// [`Self::CheckInBackground`] after this many seconds, unless a session is running then.
    CheckInBackgroundAfter(u64),
    /// Answer a held reply. Nothing happens when none is held.
    Answer(Reply, Choice),
    /// Cancel a running download.
    CancelDownload,
    /// Resume the relaunch the delegate postponed.
    Resume,
}

/// Receives Sparkle's events and decides what the delegate asks, on the main thread. Every method
/// must return at once: record, wake whoever waits, and never block on the network or a lock held
/// across work.
pub trait Listener: Send + Sync + 'static {
    /// What Sparkle reports.
    fn event(&self, event: Event);
    /// The channels besides the default one to follow (`allowedChannelsForUpdater:`).
    fn allowed_channels(&self) -> Vec<String>;
    /// Whether the relaunch must wait for [`Command::Resume`]
    /// (`updater:shouldPostponeRelaunchForUpdate:untilInvokingBlock:`).
    fn postpone_relaunch(&self) -> bool;
}

/// Something that carries commands to Sparkle: [`Sparkle`] itself, or a test's fake.
pub trait Driver: Send + Sync + 'static {
    fn send(&self, command: Command);
}

/// The running bridge. Cloning it is cheap; every clone sends to the same updater.
#[derive(Clone, Debug)]
pub struct Sparkle {
    _private: (),
}

impl Sparkle {
    /// Load the framework `bundle` embeds and prepare its updater, which is created on the main
    /// thread at the first [`Command::Configure`]. Fails when the framework is absent, cannot be
    /// loaded or lacks Sparkle 2's classes. The listener is held for the life of the process.
    pub fn start(bundle: &Path, listener: std::sync::Arc<dyn Listener>) -> Result<Self, String> {
        #[cfg(target_os = "macos")]
        {
            macos::start(bundle, listener)?;
            Ok(Self { _private: () })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (bundle, listener);
            Err("Sparkle runs on macOS only".into())
        }
    }
}

impl Driver for Sparkle {
    fn send(&self, command: Command) {
        #[cfg(target_os = "macos")]
        macos::send(command);
        #[cfg(not(target_os = "macos"))]
        let _ = command;
    }
}

/// Whether the calling thread is AppKit's main thread, which must never wait on Sparkle: Sparkle's
/// own work runs there.
pub fn on_main_thread() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::on_main_thread()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// A value of the running bundle's `Info.plist`, as text: a string as it is, and a boolean or a
/// number in decimal (`1` for true). `None` outside a bundle, for a missing key or another type.
pub fn info_value(key: &str) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        macos::info_value(key)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = key;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bundle_is_found_from_its_executable_only() {
        assert_eq!(
            bundle_of(Path::new("/Applications/Luxforge.app/Contents/MacOS/luxforge")),
            Some(PathBuf::from("/Applications/Luxforge.app"))
        );
        for outside in [
            "/Users/someone/luxforge/target/release/luxforge",
            "/Applications/Luxforge.app/Contents/Helpers/luxforge-ctl",
            "/tmp/Luxforge/Contents/MacOS/luxforge",
            "luxforge",
        ] {
            assert_eq!(bundle_of(Path::new(outside)), None, "{outside}");
        }
    }

    #[test]
    fn the_framework_is_present_only_with_its_binary() {
        let root = std::env::temp_dir().join(format!("luxforge-sparkle-{}", std::process::id()));
        let bundle = root.join("Luxforge.app");
        assert!(!framework_present(&bundle));
        std::fs::create_dir_all(framework_path(&bundle)).unwrap();
        assert!(!framework_present(&bundle));
        std::fs::write(framework_path(&bundle).join("Sparkle"), b"").unwrap();
        assert!(framework_present(&bundle));
        // A development bundle without the framework cannot start the bridge.
        assert!(Sparkle::start(&root.join("Other.app"), std::sync::Arc::new(Silent)).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    struct Silent;

    impl Listener for Silent {
        fn event(&self, _: Event) {}
        fn allowed_channels(&self) -> Vec<String> {
            Vec::new()
        }
        fn postpone_relaunch(&self) -> bool {
            false
        }
    }
}
