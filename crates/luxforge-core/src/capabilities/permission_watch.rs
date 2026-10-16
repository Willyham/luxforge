//! Live permission snapshots and a lazy observer of the shared, small configuration documents.
//! Native callbacks only enqueue hints. The observer reads on hints, cancels controls directly
//! (including sockets), retains invalidated identities, and wakes the owner's existing channel.
use super::{
    grants::{Grant, GrantScope, GrantsStore},
    settings::SettingsStore,
};
use crate::{
    Error, JobId, ModuleDescriptor,
    jobs::{JobControl, PERMISSION_REVOKED},
};
use luxforge_watch::{WatchEvent, WatchRoot, Watcher};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{SyncSender, sync_channel},
    },
    thread::{self, JoinHandle},
};

type Wake = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone)]
pub(crate) struct PermissionGuard {
    grants: GrantsStore,
    settings: SettingsStore,
    descriptor: ModuleDescriptor,
    expected: Vec<Grant>,
    destinations: HashMap<String, super::endpoint::Endpoint>,
    control: Arc<JobControl>,
    ready: Arc<AtomicBool>,
}

impl PermissionGuard {
    pub(crate) fn new(
        grants: GrantsStore,
        settings: SettingsStore,
        descriptor: ModuleDescriptor,
        expected: Vec<Grant>,
        control: Arc<JobControl>,
    ) -> Self {
        Self {
            grants,
            settings,
            descriptor,
            expected,
            destinations: HashMap::new(),
            control,
            ready: Arc::new(AtomicBool::new(true)),
        }
    }

    pub(crate) fn pin_endpoint(
        mut self,
        profile: &str,
        endpoint: &super::endpoint::Endpoint,
    ) -> Self {
        self.destinations
            .insert(profile.to_owned(), endpoint.clone());
        self
    }

    /// Revalidate the exact admitted identities and captured profile origins; a renewed grant
    /// never authorizes an older job. Missing, unreadable or incompatible documents fail closed.
    pub(crate) fn validate(&self) -> Result<(), Error> {
        while !self.ready.load(Ordering::Acquire) {
            let observed = self.control.wait_generation();
            self.control.checkpoint()?;
            if self.ready.load(Ordering::Acquire) {
                break;
            }
            self.control
                .wait_for_change(observed, std::time::Duration::from_millis(50));
        }
        self.check_snapshot()
    }

    fn check_snapshot(&self) -> Result<(), Error> {
        self.control.checkpoint()?;
        let current = self.grants.list(Some(&self.descriptor.id));
        let valid = current.and_then(|current| {
            for expected in &self.expected {
                if !current.grants.iter().any(|grant| {
                    grant.grant_id == expected.grant_id
                        && grant.is_live()
                        && grant.module_id == expected.module_id
                        && grant.capability == expected.capability
                        && grant.scope == expected.scope
                }) {
                    return Ok(false);
                }
                if let GrantScope::Remote(scope) = &expected.scope {
                    let destination = self
                        .settings
                        .profile_destination(&self.descriptor, &scope.profile_id)?;
                    if !destination.as_ref().is_some_and(|(adapter, endpoint)| {
                        *adapter == scope.adapter
                            && endpoint.origin() == scope.origin
                            && self.destinations.get(&scope.profile_id) == Some(endpoint)
                    }) {
                        return Ok(false);
                    }
                }
            }
            Ok(true)
        });
        if !matches!(valid, Ok(true)) {
            self.control.cancel(PERMISSION_REVOKED);
            return Err(self.control.cancelled_error());
        }
        self.control.checkpoint()
    }
}

#[derive(Default)]
struct Shared {
    active: HashMap<JobId, PermissionGuard>,
    invalidated: HashSet<JobId>,
    wake: Option<Wake>,
    failed: bool,
}

fn invalidate(shared: &Arc<Mutex<Shared>>, fail_closed: bool) {
    // File reads and native calls never hold the registration lock.
    let active: Vec<_> = shared
        .lock()
        .expect("permission observer")
        .active
        .iter()
        .map(|(id, guard)| (id.clone(), guard.clone()))
        .collect();
    let mut invalid = Vec::new();
    for (id, guard) in active {
        if fail_closed {
            guard.control.cancel(PERMISSION_REVOKED);
        }
        if fail_closed || guard.check_snapshot().is_err() {
            invalid.push(id);
        }
    }
    if invalid.is_empty() {
        return;
    }
    let wake = {
        let mut shared = shared.lock().expect("permission observer");
        for id in invalid {
            if shared.active.contains_key(&id) {
                shared.invalidated.insert(id);
            }
        }
        shared.wake.clone()
    };
    if let Some(wake) = wake {
        wake();
    }
}

fn ready(shared: &Arc<Mutex<Shared>>) {
    let active: Vec<_> = shared
        .lock()
        .expect("permission observer")
        .active
        .values()
        .cloned()
        .collect();
    for guard in active {
        guard.ready.store(true, Ordering::Release);
        guard.control.wake_waiters();
    }
}

fn failed(shared: &Arc<Mutex<Shared>>) {
    shared.lock().expect("permission observer").failed = true;
    invalidate(shared, true);
    ready(shared);
}

enum Message {
    Changed(WatchEvent),
    Refresh,
    Stop,
}
impl From<WatchEvent> for Message {
    fn from(event: WatchEvent) -> Self {
        Self::Changed(event)
    }
}
struct Running {
    send: SyncSender<Message>,
    thread: JoinHandle<()>,
}

pub(crate) struct PermissionObserver {
    root: Option<PathBuf>,
    shared: Arc<Mutex<Shared>>,
    running: Option<Running>,
    native: bool,
}

impl PermissionObserver {
    pub(crate) fn new(root: Option<PathBuf>) -> Self {
        Self {
            root,
            shared: Arc::default(),
            running: None,
            native: !cfg!(test),
        }
    }
    pub(crate) fn set_wake(&self, wake: Wake) {
        self.shared.lock().expect("permission observer").wake = Some(wake);
    }
    pub(crate) fn register(&mut self, id: JobId, guard: PermissionGuard) -> Result<(), Error> {
        guard.check_snapshot()?;
        let mut held = self.shared.lock().expect("permission observer");
        if held.active.len() >= 2 * (crate::jobs::LANE_QUEUE + 1) {
            return Err(Error::resource_limit(
                "permission observer registrations are full",
            ));
        }
        if held.failed {
            guard.control.cancel(PERMISSION_REVOKED);
            return Err(guard.control.cancelled_error());
        }
        if self.native && self.running.is_none() {
            guard.ready.store(false, Ordering::Release);
        }
        held.active.insert(id, guard);
        drop(held);
        if self.native && self.running.is_none() {
            self.start();
        }
        if let Some(running) = &self.running {
            let _ = running.send.try_send(Message::Refresh);
        }
        // Reconcile after registration even when native observation is driven deterministically.
        invalidate(&self.shared, false);
        Ok(())
    }
    fn start(&mut self) {
        let Some(root) = self.root.clone() else {
            failed(&self.shared);
            return;
        };
        let shared = self.shared.clone();
        let (send, receive) = sync_channel(32);
        let events = send.clone();
        let worker_shared = shared.clone();
        let worker = thread::Builder::new()
            .name("luxforge-permissions".into())
            .spawn(move || {
                let mut watcher = match Watcher::start(events) {
                    Ok(watcher) => watcher,
                    Err(_) => {
                        failed(&worker_shared);
                        return;
                    }
                };
                if watcher
                    .add_root(WatchRoot {
                        id: 1,
                        path: root.clone(),
                        resume: None,
                    })
                    .is_err()
                {
                    failed(&worker_shared);
                    return;
                }
                invalidate(&worker_shared, false);
                ready(&worker_shared);
                while let Ok(message) = receive.recv() {
                    match message {
                        Message::Stop => break,
                        Message::Refresh => invalidate(&worker_shared, false),
                        Message::Changed(WatchEvent::Unwatched { .. }) => failed(&worker_shared),
                        Message::Changed(WatchEvent::Changed { paths, .. }) => {
                            if paths.iter().any(|path| {
                                path == &root
                                    || path.file_name().is_some_and(|name| {
                                        name == super::grants::GRANTS_FILE
                                            || name == super::settings::SETTINGS_FILE
                                    })
                            }) {
                                invalidate(&worker_shared, false);
                            }
                        }
                        Message::Changed(WatchEvent::Rescan { reason, .. }) => {
                            if reason == luxforge_watch::RescanReason::RootChanged || !root.is_dir()
                            {
                                failed(&worker_shared);
                            } else {
                                invalidate(&worker_shared, false);
                            }
                        }
                        Message::Changed(WatchEvent::CaughtUp { .. } | WatchEvent::Volume(_)) => {}
                    }
                }
                // Watcher's drop stops callbacks and joins the native service before returning.
            });
        match worker {
            Ok(thread) => self.running = Some(Running { send, thread }),
            Err(_) => failed(&shared),
        }
    }
    pub(crate) fn remove(&mut self, id: &JobId) {
        let empty = {
            let mut shared = self.shared.lock().expect("permission observer");
            shared.active.remove(id);
            shared.invalidated.remove(id);
            shared.active.is_empty()
        };
        if empty {
            self.stop();
            self.shared.lock().expect("permission observer").failed = false;
        }
    }
    pub(crate) fn validate_job(&self, id: &JobId) -> Result<(), Error> {
        let guard = self
            .shared
            .lock()
            .expect("permission observer")
            .active
            .get(id)
            .cloned();
        if let Some(guard) = guard {
            guard.check_snapshot()?;
        }
        Ok(())
    }
    pub(crate) fn cancel_matching(&self, matches: impl Fn(&Grant) -> bool) {
        let selected: Vec<_> = self
            .shared
            .lock()
            .expect("permission observer")
            .active
            .iter()
            .filter(|(_, guard)| guard.expected.iter().any(&matches))
            .map(|(id, guard)| (id.clone(), guard.control.clone()))
            .collect();
        let wake = {
            let mut shared = self.shared.lock().expect("permission observer");
            for (id, control) in selected {
                control.cancel(PERMISSION_REVOKED);
                shared.invalidated.insert(id);
            }
            shared.wake.clone()
        };
        if let Some(wake) = wake {
            wake();
        }
    }

    pub(crate) fn take_invalidated(&self) -> Vec<JobId> {
        self.shared
            .lock()
            .expect("permission observer")
            .invalidated
            .drain()
            .collect()
    }
    pub(crate) fn reconcile(&self) {
        invalidate(&self.shared, false);
    }
    fn stop(&mut self) {
        if let Some(running) = self.running.take() {
            let _ = running.send.send(Message::Stop);
            let _ = running.thread.join();
        }
    }
}
impl Drop for PermissionObserver {
    fn drop(&mut self) {
        invalidate(&self.shared, true);
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AssetId, Mutation,
        capabilities::{
            descriptor::DataClass,
            grants::{DownloadScope, GRANTS_FILE, NewGrant, RemoteScope},
            secrets::MemorySecretStore,
            testing::{ADAPTER, MODULE, capability_descriptor, temp},
        },
    };
    use luxforge_test_macros::slow;
    use serde_json::json;
    use std::{
        fs,
        io::{BufRead, BufReader, Read, Write},
        net::{TcpListener, TcpStream},
        process::{Command, Stdio},
        time::Duration,
    };

    struct Fixture {
        root: PathBuf,
        grants: GrantsStore,
        settings: SettingsStore,
        descriptor: ModuleDescriptor,
    }
    impl Fixture {
        fn new(name: &str) -> Self {
            let root = temp(name);
            Self {
                grants: GrantsStore::new(&root),
                settings: SettingsStore::new(&root),
                descriptor: capability_descriptor(),
                root,
            }
        }
        fn grant(&self, scope: GrantScope) -> Grant {
            self.grants
                .grant(NewGrant {
                    module_id: MODULE,
                    capability: "test",
                    scope,
                    actor: "test",
                    request_id: "test",
                })
                .unwrap()
                .grant
        }
        fn guard(&self, expected: Grant, control: Arc<JobControl>) -> PermissionGuard {
            let profile = match &expected.scope {
                GrantScope::Remote(scope) => Some(scope.profile_id.clone()),
                _ => None,
            };
            let mut guard = PermissionGuard::new(
                self.grants.clone(),
                self.settings.clone(),
                self.descriptor.clone(),
                vec![expected],
                control,
            );
            if let Some(profile) = profile {
                let (_, endpoint) = self
                    .settings
                    .profile_destination(&self.descriptor, &profile)
                    .unwrap()
                    .unwrap();
                guard = guard.pin_endpoint(&profile, &endpoint);
            }
            guard
        }
        fn remote(&self) -> Grant {
            let created = self
                .settings
                .create_profile(&self.descriptor, ADAPTER, "provider", &mutation(0))
                .unwrap();
            let profile = created.result.profile_id.unwrap();
            self.settings
                .set(
                    &self.descriptor,
                    Some(&profile),
                    json!({"endpoint": "http://127.0.0.1:9/send"})
                        .as_object()
                        .unwrap(),
                    &mutation(1),
                )
                .unwrap();
            self.grant(GrantScope::Remote(RemoteScope {
                profile_id: profile,
                adapter: ADAPTER.into(),
                origin: "http://127.0.0.1:9".into(),
                data: DataClass::SampleGrid8,
                asset_id: AssetId::new(),
            }))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    fn mutation(revision: u64) -> Mutation {
        Mutation {
            expected_revision: revision,
            request_id: uuid::Uuid::new_v4().to_string(),
            actor: "test".into(),
        }
    }
    fn download() -> GrantScope {
        GrantScope::Download(DownloadScope {
            resource: "palette".into(),
            version: "1".into(),
            origin: "https://example.com".into(),
        })
    }

    #[test]
    fn current_grant_identity_is_rechecked_before_work_even_after_renewal() {
        let fixture = Fixture::new("permission-current");
        let grant = fixture.grant(download());
        let control = JobControl::new();
        let guard = fixture.guard(grant.clone(), control.clone());
        guard.validate().unwrap();
        // A second store represents another owner/process of the same documents.
        let other = GrantsStore::new(&fixture.root);
        other.revoke(&grant.grant_id, "another owner").unwrap();
        let renewed = fixture.grant(download());
        assert_ne!(renewed.grant_id, grant.grant_id);
        assert_eq!(guard.validate().unwrap_err().detail, PERMISSION_REVOKED);
        assert!(control.is_cancelled());
    }

    #[test]
    fn profile_changes_cancel_captured_work_even_if_the_grants_file_cannot_be_written() {
        for change in ["endpoint", "same-origin", "remove", "reset"] {
            let fixture = Fixture::new(&format!("permission-profile-{change}"));
            let grant = fixture.remote();
            let GrantScope::Remote(scope) = &grant.scope else {
                unreachable!()
            };
            let control = JobControl::new();
            let guard = fixture.guard(grant.clone(), control.clone());
            guard.validate().unwrap();
            let secrets = MemorySecretStore::new();
            match change {
                "same-origin" => {
                    fixture
                        .settings
                        .set(
                            &fixture.descriptor,
                            Some(&scope.profile_id),
                            json!({"endpoint": "http://127.0.0.1:9/new-path"})
                                .as_object()
                                .unwrap(),
                            &mutation(2),
                        )
                        .unwrap();
                }
                "endpoint" => {
                    fixture
                        .settings
                        .set(
                            &fixture.descriptor,
                            Some(&scope.profile_id),
                            json!({"endpoint": "http://127.0.0.1:10/send"})
                                .as_object()
                                .unwrap(),
                            &mutation(2),
                        )
                        .unwrap();
                }
                "remove" => {
                    fixture
                        .settings
                        .remove_profile(
                            &fixture.descriptor,
                            &secrets,
                            &scope.profile_id,
                            &mutation(2),
                        )
                        .unwrap();
                }
                _ => {
                    fixture
                        .settings
                        .reset(&fixture.descriptor, &secrets, &mutation(2))
                        .unwrap();
                }
            }
            // The old live grant was not writable/revoked, but cannot authorize the captured profile.
            assert!(fixture.grants.list(None).unwrap().grants[0].is_live());
            assert!(guard.validate().is_err(), "{change}");
            assert!(control.is_cancelled(), "{change}");
        }
    }

    #[test]
    fn deterministic_rescan_cancels_only_invalid_work_and_retains_wakes() {
        let fixture = Fixture::new("permission-rescan");
        let revoked = fixture.grant(download());
        let other_scope = GrantScope::Download(DownloadScope {
            resource: "other".into(),
            ..match download() {
                GrantScope::Download(scope) => scope,
                _ => unreachable!(),
            }
        });
        let valid = fixture.grant(other_scope);
        let mut observer = PermissionObserver::new(Some(fixture.root.clone()));
        let a = JobId::new();
        let b = JobId::new();
        let unaffected = JobId::new();
        let running = JobControl::new();
        let queued = JobControl::new();
        let allowed = JobControl::new();
        observer
            .register(a.clone(), fixture.guard(revoked.clone(), running.clone()))
            .unwrap();
        observer
            .register(b.clone(), fixture.guard(revoked.clone(), queued.clone()))
            .unwrap();
        observer
            .register(unaffected.clone(), fixture.guard(valid, allowed.clone()))
            .unwrap();
        assert!(
            observer.running.is_none(),
            "deterministic tests start no native watcher"
        );
        GrantsStore::new(&fixture.root)
            .revoke(&revoked.grant_id, "other owner")
            .unwrap();
        observer.reconcile();
        assert!(running.is_cancelled() && queued.is_cancelled());
        assert!(!allowed.is_cancelled());
        let invalid: HashSet<_> = observer.take_invalidated().into_iter().collect();
        assert_eq!(invalid, HashSet::from([a.clone(), b.clone()]));
        observer.remove(&a);
        observer.remove(&b);
        observer.remove(&unaffected);
        assert!(observer.shared.lock().unwrap().active.is_empty());
    }

    #[test]
    fn deletion_invalid_json_and_watch_failure_fail_closed() {
        for failure in ["delete", "json", "watch"] {
            let fixture = Fixture::new(&format!("permission-{failure}"));
            let grant = fixture.grant(download());
            let control = JobControl::new();
            let mut observer = PermissionObserver::new(Some(fixture.root.clone()));
            observer
                .register(JobId::new(), fixture.guard(grant, control.clone()))
                .unwrap();
            match failure {
                "delete" => fs::remove_file(fixture.root.join(GRANTS_FILE)).unwrap(),
                "json" => fs::write(fixture.root.join(GRANTS_FILE), b"invalid").unwrap(),
                _ => {}
            }
            if failure == "watch" {
                failed(&observer.shared);
            } else {
                observer.reconcile();
            }
            assert!(control.is_cancelled(), "{failure}");
            assert_eq!(observer.take_invalidated().len(), 1);
        }
    }

    #[test]
    fn lost_native_coverage_refuses_new_jobs_until_the_last_registration_retires() {
        let fixture = Fixture::new("permission-coverage-lost");
        let grant = fixture.grant(download());
        let mut observer = PermissionObserver::new(Some(fixture.root.clone()));
        let first = JobId::new();
        observer
            .register(
                first.clone(),
                fixture.guard(grant.clone(), JobControl::new()),
            )
            .unwrap();
        failed(&observer.shared);
        let replacement = JobControl::new();
        assert!(
            observer
                .register(
                    JobId::new(),
                    fixture.guard(grant.clone(), replacement.clone())
                )
                .is_err()
        );
        assert!(replacement.is_cancelled());
        observer.remove(&first);
        observer
            .register(JobId::new(), fixture.guard(grant, JobControl::new()))
            .unwrap();
    }

    // Invoked only by the native test's child. Ordinary test enumeration does no work here.
    #[test]
    fn permission_child() {
        let Ok(root) = std::env::var("LUXFORGE_PERMISSION_CHILD_ROOT") else {
            return;
        };
        let id = std::env::var("LUXFORGE_PERMISSION_CHILD_GRANT").unwrap();
        let mut trigger = String::new();
        std::io::stdin().read_line(&mut trigger).unwrap();
        GrantsStore::new(root)
            .revoke(&id, "separate process")
            .unwrap();
        println!("permission-revoked");
        std::io::stdout().flush().unwrap();
    }

    #[test]
    #[slow]
    fn native_watch_separate_process_revocation_cancels_controls_and_shuts_the_socket() {
        let fixture = Fixture::new("permission-native-process");
        let grant = fixture.grant(download());
        let control = JobControl::new();
        let queued = JobControl::new();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let socket = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        assert!(control.hold_connection(&socket).unwrap());
        let mut observer = PermissionObserver::new(Some(fixture.root.clone()));
        observer.native = true;
        let (wake, awakened) = sync_channel(4);
        observer.set_wake(Arc::new(move || {
            let _ = wake.try_send(());
        }));
        let running_guard = fixture.guard(grant.clone(), control.clone());
        observer
            .register(JobId::new(), running_guard.clone())
            .unwrap();
        observer
            .register(JobId::new(), fixture.guard(grant.clone(), queued.clone()))
            .unwrap();
        // Worker validation waits for native registration and its immediate reconciliation.
        running_guard.validate().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "capabilities::permission_watch::tests::permission_child",
                "--nocapture",
            ])
            .env("LUXFORGE_PERMISSION_CHILD_ROOT", &fixture.root)
            .env("LUXFORGE_PERMISSION_CHILD_GRANT", &grant.grant_id)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"revoke\n").unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        loop {
            line.clear();
            assert!(output.read_line(&mut line).unwrap() > 0);
            if line.contains("permission-revoked") {
                break;
            }
        }
        assert!(child.wait().unwrap().success());
        awakened
            .recv_timeout(Duration::from_secs(10))
            .expect("native notification");
        assert!(control.is_cancelled() && queued.is_cancelled());
        assert_eq!(observer.take_invalidated().len(), 2);
        let mut byte = [0u8];
        assert_eq!(
            peer.read(&mut byte).unwrap(),
            0,
            "revocation shuts down the held socket"
        );
        assert!(control.checkpoint().is_err());
    }
}
