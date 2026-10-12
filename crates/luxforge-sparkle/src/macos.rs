//! The Objective-C side: the framework loaded through `NSBundle`, one `SPUUpdater`, and the user
//! driver and delegate it calls, all on AppKit's main thread.
//!
//! Nothing here is linked against Sparkle. Its classes are looked up by name once the framework is
//! loaded, and every message to them is sent through `msg_send!` with the signature Sparkle 2.10's
//! public headers declare (`SPUUpdater.h`, `SPUUserDriver.h`, `SPUUpdaterDelegate.h`,
//! `SUAppcastItem.h`, `SPUUserUpdateState.h`).
use crate::{
    CheckKind, Choice, Command, Event, Item, Listener, Reply, Stage, framework_path,
    framework_present,
};
use block2::{Block, RcBlock};
use dispatch2::{DispatchQueue, DispatchTime};
use objc2::{
    MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::{Allocated, Retained},
    runtime::{AnyClass, AnyObject, NSObject, NSObjectProtocol},
};
use objc2_foundation::{
    NSBundle, NSDictionary, NSError, NSInteger, NSNumber, NSSet, NSString, NSURL, NSUserDefaults,
};
use std::{
    cell::RefCell,
    path::Path,
    sync::{Arc, OnceLock},
    time::Duration,
};

/// The listener, set once per process by [`start`].
static LISTENER: OnceLock<Arc<dyn Listener>> = OnceLock::new();

/// The skip Sparkle records when an offer is answered with Skip (`SUConstants.m`). Luxforge holds
/// the person's skip itself, so these are cleared.
const SPARKLE_SKIP_KEYS: [&str; 3] = [
    "SUSkippedVersion",
    "SUSkippedMajorVersion",
    "SUSkippedMajorSubreleaseVersion",
];
/// `SUNoUpdateError` and `SUInstallationCanceledError` (`SUErrors.h`): not failures.
const NOT_FAILURES: [NSInteger; 2] = [1001, 4007];

/// What the main thread holds: the updater and the reply blocks Sparkle is waiting on.
#[derive(Default)]
struct Native {
    updater: Option<Retained<AnyObject>>,
    driver: Option<Retained<UserDriver>>,
    delegate: Option<Retained<UpdaterDelegate>>,
    failed: bool,
    automatic_checks: bool,
    found: Option<RcBlock<dyn Fn(NSInteger)>>,
    ready: Option<RcBlock<dyn Fn(NSInteger)>>,
    cancel_download: Option<RcBlock<dyn Fn()>>,
    postponed: Option<RcBlock<dyn Fn()>>,
}

thread_local! {
    // AppKit's main thread only: every access below holds a `MainThreadMarker`.
    static NATIVE: RefCell<Native> = RefCell::new(Native::default());
}

fn emit(event: Event) {
    if let Some(listener) = LISTENER.get() {
        listener.event(event);
    }
}

pub(crate) fn on_main_thread() -> bool {
    MainThreadMarker::new().is_some()
}

pub(crate) fn start(bundle: &Path, listener: Arc<dyn Listener>) -> Result<(), String> {
    if !framework_present(bundle) {
        return Err(format!(
            "{} has no Sparkle.framework",
            bundle.to_string_lossy()
        ));
    }
    let path = framework_path(bundle);
    let path = NSString::from_str(path.to_str().ok_or("The framework path is not UTF-8")?);
    let framework =
        NSBundle::bundleWithPath(&path).ok_or("Sparkle.framework is not a loadable bundle")?;
    // SAFETY: loading a bundle runs its initializers. This is the framework the release build
    // embedded and signed with the app, at the bundle's own path; Sparkle 2 has no initializer
    // with side effects beyond registering its classes.
    unsafe { framework.loadAndReturnError() }.map_err(|error| {
        format!(
            "Sparkle.framework could not be loaded: {}",
            describe(&error)
        )
    })?;
    for class in [
        c"SPUUpdater",
        c"SUUpdatePermissionResponse",
        c"SUAppcastItem",
    ] {
        if AnyClass::get(class).is_none() {
            return Err(format!(
                "Sparkle.framework has no {} class",
                class.to_string_lossy()
            ));
        }
    }
    LISTENER
        .set(listener)
        .map_err(|_| "Sparkle was started already".to_owned())
}

pub(crate) fn send(command: Command) {
    if LISTENER.get().is_none() {
        return;
    }
    if let Command::CheckInBackgroundAfter(seconds) = command {
        let when = DispatchTime::NOW.time(Duration::from_secs(seconds).as_nanos() as i64);
        let _ = DispatchQueue::main().after(when, || run(Command::CheckInBackground, true));
        return;
    }
    DispatchQueue::main().exec_async(move || run(command, false));
}

/// Carry out `command` on the main thread. A delayed background check runs only when no session
/// is in progress then.
fn run(command: Command, only_if_idle: bool) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    match command {
        Command::Configure { automatic_checks } => configure(mtm, automatic_checks),
        Command::Check | Command::CheckInBackground => {
            let Some(updater) = NATIVE.with(|native| native.borrow().updater.clone()) else {
                emit(Event::Refused("the updater is not running".into()));
                return;
            };
            // SAFETY: `SPUUpdater`'s `canCheckForUpdates` and `sessionInProgress` are BOOL
            // properties, read on the main thread.
            let (can, busy): (bool, bool) = unsafe {
                (
                    msg_send![&*updater, canCheckForUpdates],
                    msg_send![&*updater, sessionInProgress],
                )
            };
            if only_if_idle && busy {
                return;
            }
            if !can {
                emit(Event::Refused("an update session is running".into()));
                return;
            }
            // SAFETY: both are `- (void)` methods of `SPUUpdater`, called on the main thread.
            unsafe {
                if command == Command::Check {
                    let _: () = msg_send![&*updater, checkForUpdates];
                } else {
                    let _: () = msg_send![&*updater, checkForUpdatesInBackground];
                }
            }
        }
        Command::CheckInBackgroundAfter(_) => {}
        Command::Answer(reply, choice) => {
            let block = NATIVE.with(|native| {
                let mut native = native.borrow_mut();
                match reply {
                    Reply::Found => native.found.take(),
                    Reply::Ready => native.ready.take(),
                }
            });
            if let Some(block) = block {
                // `SPUUserUpdateChoice`: Skip 0, Install 1, Dismiss 2.
                let choice: NSInteger = match choice {
                    Choice::Skip => 0,
                    Choice::Install => 1,
                    Choice::Dismiss => 2,
                };
                block.call((choice,));
            }
        }
        Command::CancelDownload => {
            if let Some(cancel) = NATIVE.with(|native| native.borrow_mut().cancel_download.take()) {
                cancel.call(());
            }
        }
        Command::Resume => {
            if let Some(resume) = NATIVE.with(|native| native.borrow_mut().postponed.take()) {
                resume.call(());
            }
        }
    }
}

/// Apply the automatic-check choice, creating and starting the updater the first time.
fn configure(mtm: MainThreadMarker, automatic_checks: bool) {
    let (updater, failed) = NATIVE.with(|native| {
        let mut native = native.borrow_mut();
        native.automatic_checks = automatic_checks;
        (native.updater.clone(), native.failed)
    });
    if failed {
        return;
    }
    let defaults = NSUserDefaults::standardUserDefaults();
    for key in SPARKLE_SKIP_KEYS {
        defaults.removeObjectForKey(&NSString::from_str(key));
    }
    let (updater, fresh) = match updater {
        Some(updater) => (updater, false),
        None => match create(mtm) {
            Ok(updater) => (updater, true),
            Err(error) => {
                NATIVE.with(|native| native.borrow_mut().failed = true);
                emit(Event::Failed(error));
                return;
            }
        },
    };
    // SAFETY: BOOL properties of `SPUUpdater`, set on the main thread. Sparkle never downloads on
    // its own: an automatic installation is the port's decision, item by item.
    unsafe {
        let _: () = msg_send![&*updater, setAutomaticallyChecksForUpdates: automatic_checks];
        let _: () = msg_send![&*updater, setAutomaticallyDownloadsUpdates: false];
    }
    if fresh {
        // SAFETY: `- (BOOL)startUpdater:(NSError **)error`, once, on the main thread.
        let started: Result<(), Retained<NSError>> =
            unsafe { msg_send![&*updater, startUpdater: _] };
        if let Err(error) = started {
            NATIVE.with(|native| {
                let mut native = native.borrow_mut();
                native.failed = true;
                native.updater = None;
            });
            emit(Event::Failed(format!(
                "Sparkle could not start: {}",
                describe(&error)
            )));
        }
    }
}

/// Create the updater over the main bundle, with this module's user driver and delegate.
fn create(mtm: MainThreadMarker) -> Result<Retained<AnyObject>, String> {
    let class = AnyClass::get(c"SPUUpdater").ok_or("Sparkle has no SPUUpdater")?;
    let host = NSBundle::mainBundle();
    let driver = UserDriver::new(mtm);
    let delegate = UpdaterDelegate::new(mtm);
    // SAFETY: `- initWithHostBundle:applicationBundle:userDriver:delegate:` takes two bundles, an
    // object implementing `SPUUserDriver` (every required method is defined below) and an
    // optional `SPUUpdaterDelegate`, which it holds weakly: both are retained in `NATIVE`.
    let updater: Option<Retained<AnyObject>> = unsafe {
        let allocated: Allocated<AnyObject> = msg_send![class, alloc];
        msg_send![
            allocated,
            initWithHostBundle: &*host,
            applicationBundle: &*host,
            userDriver: &*driver,
            delegate: &*delegate
        ]
    };
    let updater = updater.ok_or("SPUUpdater could not be created")?;
    NATIVE.with(|native| {
        let mut native = native.borrow_mut();
        native.updater = Some(updater.clone());
        native.driver = Some(driver);
        native.delegate = Some(delegate);
    });
    Ok(updater)
}

fn describe(error: &NSError) -> String {
    error.localizedDescription().to_string()
}

fn text(object: Option<Retained<NSString>>) -> Option<String> {
    object
        .map(|text| text.to_string())
        .filter(|text| !text.is_empty())
}

/// The plain values of an `SUAppcastItem`.
fn item(item: &AnyObject) -> Item {
    // SAFETY: these are `SUAppcastItem`'s declared readonly properties: two non-null `NSString`s,
    // nullable `NSURL` and `NSString`, `uint64_t` and a non-null `NSDictionary`.
    unsafe {
        let version: Option<Retained<NSString>> = msg_send![item, displayVersionString];
        let build: Option<Retained<NSString>> = msg_send![item, versionString];
        let link: Option<Retained<NSURL>> = msg_send![item, infoURL];
        let channel: Option<Retained<NSString>> = msg_send![item, channel];
        let length: u64 = msg_send![item, contentLength];
        let properties: Option<Retained<NSDictionary>> = msg_send![item, propertiesDictionary];
        let catalog_format = properties.and_then(|properties| {
            let key = NSString::from_str("luxforge:catalogFormat");
            let value: Option<Retained<AnyObject>> = msg_send![&*properties, objectForKey: &*key];
            value.and_then(|value| value.downcast::<NSString>().ok())
        });
        Item {
            version: text(version).unwrap_or_default(),
            build: text(build).unwrap_or_default(),
            link: link.and_then(|link| text(link.absoluteString())),
            catalog_format: text(catalog_format),
            channel: text(channel),
            length,
        }
    }
}

fn hold(
    slot: impl FnOnce(&mut Native) -> &mut Option<RcBlock<dyn Fn(NSInteger)>>,
    block: &Block<dyn Fn(NSInteger)>,
) {
    let copied = block.copy();
    NATIVE.with(|native| *slot(&mut native.borrow_mut()) = Some(copied));
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. The class holds no ivars; Sparkle calls
    // every `SPUUserDriver` method on the main thread.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "LuxforgeSparkleUserDriver"]
    struct UserDriver;

    unsafe impl NSObjectProtocol for UserDriver {}

    impl UserDriver {
        // SAFETY (every method below): each selector and signature is `SPUUserDriver`'s, with
        // objects as `AnyObject`, blocks as `Block`, `BOOL` as `bool` and `uint64_t` as `u64`.
        #[unsafe(method(showUpdatePermissionRequest:reply:))]
        fn permission(&self, _request: &AnyObject, reply: &Block<dyn Fn(*mut AnyObject)>) {
            // Never asked: the updater is configured before it starts. Answer with the person's
            // choice of automatic checks and no system profile.
            let automatic = NATIVE.with(|native| native.borrow().automatic_checks);
            if let Some(class) = AnyClass::get(c"SUUpdatePermissionResponse") {
                let response: Option<Retained<AnyObject>> = unsafe {
                    let allocated: Allocated<AnyObject> = msg_send![class, alloc];
                    msg_send![
                        allocated,
                        initWithAutomaticUpdateChecks: automatic,
                        sendSystemProfile: false
                    ]
                };
                if let Some(response) = response {
                    reply.call((Retained::as_ptr(&response).cast_mut(),));
                }
            }
        }

        #[unsafe(method(showUserInitiatedUpdateCheckWithCancellation:))]
        fn user_check(&self, _cancellation: &Block<dyn Fn()>) {}

        #[unsafe(method(showUpdateFoundWithAppcastItem:state:reply:))]
        fn found(&self, appcast_item: &AnyObject, state: &AnyObject, reply: &Block<dyn Fn(NSInteger)>) {
            // `SPUUserUpdateStage`: NotDownloaded 0, Downloaded 1, Installing 2.
            let stage: NSInteger = unsafe { msg_send![state, stage] };
            let stage = match stage {
                1 => Stage::Downloaded,
                2 => Stage::Installing,
                _ => Stage::NotDownloaded,
            };
            hold(|native| &mut native.found, reply);
            emit(Event::Found { item: item(appcast_item), stage });
        }

        #[unsafe(method(showUpdateReleaseNotesWithDownloadData:))]
        fn notes(&self, _data: &AnyObject) {}

        #[unsafe(method(showUpdateReleaseNotesFailedToDownloadWithError:))]
        fn notes_failed(&self, _error: &NSError) {}

        #[unsafe(method(showUpdateNotFoundWithError:acknowledgement:))]
        fn not_found(&self, _error: &NSError, acknowledgement: &Block<dyn Fn()>) {
            // The delegate's `updaterDidNotFindUpdate:error:` reports it.
            acknowledgement.call(());
        }

        #[unsafe(method(showUpdaterError:acknowledgement:))]
        fn error(&self, error: &NSError, acknowledgement: &Block<dyn Fn()>) {
            emit(Event::Error(describe(error)));
            acknowledgement.call(());
        }

        #[unsafe(method(showDownloadInitiatedWithCancellation:))]
        fn download(&self, cancellation: &Block<dyn Fn()>) {
            let copied = cancellation.copy();
            NATIVE.with(|native| native.borrow_mut().cancel_download = Some(copied));
            emit(Event::DownloadStarted);
        }

        #[unsafe(method(showDownloadDidReceiveExpectedContentLength:))]
        fn download_length(&self, length: u64) {
            emit(Event::DownloadLength(length));
        }

        #[unsafe(method(showDownloadDidReceiveDataOfLength:))]
        fn download_data(&self, length: u64) {
            emit(Event::DownloadReceived(length));
        }

        #[unsafe(method(showDownloadDidStartExtractingUpdate))]
        fn extracting(&self) {
            NATIVE.with(|native| native.borrow_mut().cancel_download = None);
            emit(Event::Extracting);
        }

        #[unsafe(method(showExtractionReceivedProgress:))]
        fn extraction_progress(&self, _progress: f64) {}

        #[unsafe(method(showReadyToInstallAndRelaunch:))]
        fn ready(&self, reply: &Block<dyn Fn(NSInteger)>) {
            hold(|native| &mut native.ready, reply);
            emit(Event::Ready);
        }

        #[unsafe(method(showInstallingUpdateWithApplicationTerminated:retryTerminatingApplication:))]
        fn installing(&self, terminated: bool, _retry: &Block<dyn Fn()>) {
            emit(Event::Installing { terminated });
        }

        #[unsafe(method(showUpdateInstalledAndRelaunched:acknowledgement:))]
        fn installed(&self, _relaunched: bool, acknowledgement: &Block<dyn Fn()>) {
            acknowledgement.call(());
        }

        #[unsafe(method(dismissUpdateInstallation))]
        fn dismiss(&self) {
            NATIVE.with(|native| {
                let mut native = native.borrow_mut();
                native.found = None;
                native.ready = None;
                native.cancel_download = None;
            });
            emit(Event::Dismissed);
        }

        #[unsafe(method(showUpdateInFocus))]
        fn focus(&self) {}
    }
);

impl UserDriver {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        // SAFETY: NSObject's initializer is appropriate for this ivar-less subclass.
        unsafe { msg_send![Self::alloc(mtm), init] }
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. The class holds no ivars; Sparkle calls
    // its delegate on the main thread.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "LuxforgeSparkleUpdaterDelegate"]
    struct UpdaterDelegate;

    unsafe impl NSObjectProtocol for UpdaterDelegate {}

    impl UpdaterDelegate {
        // SAFETY (every method below): each selector and signature is `SPUUpdaterDelegate`'s.
        #[unsafe(method(updater:mayPerformUpdateCheck:error:))]
        fn may_check(&self, _updater: &AnyObject, check: NSInteger, _error: *mut *mut NSError) -> bool {
            emit(Event::CheckStarted(match check {
                0 => CheckKind::Asked,
                1 => CheckKind::Background,
                _ => CheckKind::Information,
            }));
            true
        }

        #[unsafe(method_id(allowedChannelsForUpdater:))]
        fn channels(&self, _updater: &AnyObject) -> Retained<NSSet<NSString>> {
            let channels: Vec<Retained<NSString>> = LISTENER
                .get()
                .map(|listener| listener.allowed_channels())
                .unwrap_or_default()
                .iter()
                .map(|channel| NSString::from_str(channel))
                .collect();
            NSSet::from_retained_slice(&channels)
        }

        #[unsafe(method(updaterShouldPromptForPermissionToCheckForUpdates:))]
        fn prompt(&self, _updater: &AnyObject) -> bool {
            false
        }

        #[unsafe(method(updater:shouldDownloadReleaseNotesForUpdate:))]
        fn release_notes(&self, _updater: &AnyObject, _item: &AnyObject) -> bool {
            false
        }

        #[unsafe(method(updaterDidNotFindUpdate:error:))]
        fn not_found(&self, _updater: &AnyObject, _error: &NSError) {
            emit(Event::NotFound);
        }

        #[unsafe(method(updater:didAbortWithError:))]
        fn aborted(&self, _updater: &AnyObject, error: &NSError) {
            if !NOT_FAILURES.contains(&error.code()) {
                emit(Event::Error(describe(error)));
            }
        }

        #[unsafe(method(updater:didFinishUpdateCycleForUpdateCheck:error:))]
        fn finished(&self, _updater: &AnyObject, _check: NSInteger, error: Option<&NSError>) {
            let error = error
                .filter(|error| !NOT_FAILURES.contains(&error.code()))
                .map(describe);
            emit(Event::CycleFinished { error });
        }

        #[unsafe(method(updater:shouldPostponeRelaunchForUpdate:untilInvokingBlock:))]
        fn postpone(&self, _updater: &AnyObject, _item: &AnyObject, handler: &Block<dyn Fn()>) -> bool {
            let postpone = LISTENER
                .get()
                .is_some_and(|listener| listener.postpone_relaunch());
            if postpone {
                let copied = handler.copy();
                NATIVE.with(|native| native.borrow_mut().postponed = Some(copied));
                emit(Event::RelaunchPostponed);
            }
            postpone
        }

        #[unsafe(method(updater:willInstallUpdateOnQuit:immediateInstallationBlock:))]
        fn install_on_quit(&self, _updater: &AnyObject, _item: &AnyObject, _install: &Block<dyn Fn()>) -> bool {
            // Sparkle's own automatic downloads are off, so this is not reached; were it, Sparkle
            // keeps its schedule and the port decides at quit.
            false
        }
    }
);

impl UpdaterDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        // SAFETY: NSObject's initializer is appropriate for this ivar-less subclass.
        unsafe { msg_send![Self::alloc(mtm), init] }
    }
}

pub(crate) fn info_value(key: &str) -> Option<String> {
    let value = NSBundle::mainBundle().objectForInfoDictionaryKey(&NSString::from_str(key))?;
    let value = match value.downcast::<NSString>() {
        Ok(text) => return Some(text.to_string()),
        Err(value) => value,
    };
    // A plist boolean is a `CFBoolean`, whose `stringValue` is `1` or `0`.
    let number = value.downcast::<NSNumber>().ok()?;
    Some(number.stringValue().to_string())
}
