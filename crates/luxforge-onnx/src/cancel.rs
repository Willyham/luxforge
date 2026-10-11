//! Cancelling a model load mid-way. A first load on Core ML compiles the model and can take seconds
//! (tens of seconds for a large encoder), longer than a person waits after cancelling, so a load is
//! watched: one scoped thread, alive only while the load runs, polls the job's cancellation and
//! sets ONNX Runtime's load-cancellation flag when it fires. Runs are not watched; they are checked
//! before and after.
use luxforge_core::Cancel;
use ort::session::builder::LoadCanceler;
use std::{
    sync::mpsc::{self, RecvTimeoutError},
    time::Duration,
};

/// How often a watched load looks at its cancellation.
const POLL: Duration = Duration::from_millis(20);

/// Run `load`, cancelling it through `canceler` as soon as `cancel` fires.
pub(crate) fn watching<T>(cancel: &Cancel, canceler: LoadCanceler, load: impl FnOnce() -> T) -> T {
    watching_with(
        cancel,
        move || {
            let _ = canceler.cancel();
        },
        load,
    )
}

/// Run `load`, calling `cancelled` once, from the watcher, as soon as `cancel` fires while it runs.
pub(crate) fn watching_with<T>(
    cancel: &Cancel,
    cancelled: impl FnOnce() + Send,
    load: impl FnOnce() -> T,
) -> T {
    let (done, finished) = mpsc::channel::<()>();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            loop {
                match finished.recv_timeout(POLL) {
                    Err(RecvTimeoutError::Timeout) => {
                        if cancel.check().is_err() {
                            cancelled();
                            return;
                        }
                    }
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        });
        let answer = load();
        drop(done);
        answer
    })
}
