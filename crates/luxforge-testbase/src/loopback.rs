//! What a test that serves a loopback session does on a host that forbids loopback listening, as
//! some sandboxes do: it fails, unless the run declares that host with [`NO_LOOPBACK`], when it is
//! skipped and says so. A skipped check is never a silent pass.

/// The environment variable a run sets to `1` to declare a host that forbids loopback listening.
pub const NO_LOOPBACK: &str = "LUXFORGE_NO_LOOPBACK";

/// Whether the failure `detail` of listening on loopback is the host forbidding it and the run
/// declares such a host ([`NO_LOOPBACK`]=1), so the caller skips its test: it prints why to
/// standard error and answers `true`. A forbidding host the run did not declare fails the test
/// here; any other failure answers `false`, for the caller to fail on.
pub fn loopback_forbidden(detail: &str) -> bool {
    if !detail.contains("Operation not permitted") && !detail.contains("Permission denied") {
        return false;
    }
    if std::env::var_os(NO_LOOPBACK).is_some_and(|value| value == "1") {
        eprintln!("skipped: this host forbids loopback listening ({detail}); {NO_LOOPBACK}=1");
        return true;
    }
    panic!(
        "this host forbids loopback listening ({detail}); set {NO_LOOPBACK}=1 to declare it and \
         skip the tests that need it"
    );
}
