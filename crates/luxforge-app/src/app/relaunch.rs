//! Work that runs once the desktop's ordinary quit path has closed the catalog: a relaunch from a
//! new place after a move into Applications, and the update port's install and relaunch
//! ([design](../../../../docs/design/auto-update.md#commands-and-preferences), "Installing").
//!
//! One action waits at a time, in a process-wide slot any thread may fill: the move question from
//! the update loop, the update port from its own worker. [`request_quit`] then asks the window to
//! close, which runs the same path as its close button: the remembered frame and the last
//! preference write are stored, the live session stops, and the catalog owner closes the catalog
//! and its thread is joined. Only after that join does [`Editor::close`](super::Editor::close)
//! take the action and run it, on the way out, and log what it did.
use super::{message::Message, waker::Signal};
use serde_json::{Value, json};
use std::sync::{Mutex, OnceLock};

/// Something to do once the catalog has closed; it answers what it did, for the log.
pub(crate) type AfterClose = Box<dyn FnOnce() -> Value + Send>;

type Pending = Option<(&'static str, AfterClose)>;

/// The action waiting for the quit path, by name.
#[cfg(not(test))]
static PENDING: Mutex<Pending> = Mutex::new(None);

// Each test's editor updates on the test's own thread, so a test's slot is its thread's: a test
// that closes an editor never takes another test's action.
#[cfg(test)]
thread_local! {
    static PENDING: Mutex<Pending> = const { Mutex::new(None) };
}

fn with_pending<R>(f: impl FnOnce(&mut Pending) -> R) -> Option<R> {
    #[cfg(not(test))]
    return PENDING.lock().ok().map(|mut pending| f(&mut pending));
    #[cfg(test)]
    return PENDING.with(|pending| pending.lock().ok().map(|mut pending| f(&mut pending)));
}

static QUIT: OnceLock<Signal<Message>> = OnceLock::new();

fn quit() -> &'static Signal<Message> {
    QUIT.get_or_init(|| Signal::new(|| Message::Close))
}

/// Run `action` once the quit path has closed the catalog. Answers `false`, keeping the action
/// already waiting, when one is: a second relaunch never replaces the first.
pub(crate) fn after_close(name: &'static str, action: AfterClose) -> bool {
    with_pending(|pending| {
        if pending.is_some() {
            return false;
        }
        *pending = Some((name, action));
        true
    })
    .unwrap_or(false)
}

/// Relaunch `relaunch.app` once the quit path has closed the catalog, then move the original to
/// the Trash or eject its disk image as the move decided: the hook the move question takes, and
/// the one the macOS update port takes for `update.move-to-applications`.
pub(crate) fn relaunch(relaunch: luxforge_install::Relaunch) -> bool {
    after_close(
        "relaunch",
        Box::new(move || match relaunch.start(&luxforge_install::System) {
            Ok(started) => json!({
                "app": relaunch.app,
                "trashed": started.trashed,
                "trash_failed": started.trash_failed,
                "eject": started.eject,
            }),
            Err(error) => json!({"app": relaunch.app, "error": error.to_string()}),
        }),
    )
}

/// Ask the window to close through its ordinary quit path, from any thread. Never blocks.
pub(crate) fn request_quit() {
    quit().post();
}

/// Take the waiting action: [`Editor::close`](super::Editor::close) does, once.
pub(crate) fn take() -> Option<(&'static str, AfterClose)> {
    with_pending(Option::take).flatten()
}

/// Run a taken action, and the log event it answers, once the catalog owner's thread has joined.
pub(crate) fn run(taken: (&'static str, AfterClose)) -> Value {
    let (name, action) = taken;
    json!({"name": name, "detail": action()})
}

/// One `Message::Close` per [`request_quit`]. Not gated: a port may ask at any time, and with
/// nothing asked the blocked stream wakes nothing.
pub(super) fn subscription(_: &super::Editor) -> iced::Subscription<Message> {
    iced::Subscription::run(|| quit().stream())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_to_applications_one_action_waits_for_the_quit_path_and_runs_once() {
        assert!(take().is_none());
        assert!(after_close("first", Box::new(|| json!({"ran": 1}))));
        assert!(
            !after_close("second", Box::new(|| json!({"ran": 2}))),
            "a second action never replaces the first"
        );
        let taken = take().expect("the first waits");
        assert!(take().is_none(), "taken once");
        assert_eq!(run(taken), json!({"name": "first", "detail": {"ran": 1}}));
    }
}
