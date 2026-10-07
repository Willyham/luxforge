//! When the GPU finished a GPU frame, as far as the interface can learn it without waiting.
//!
//! Iced's compositor creates its device with no optional features, so the device the surface
//! receives has no timestamp queries on any adapter. wgpu 27 runs a
//! `Queue::on_submitted_work_done` callback from the maintenance of the first `queue.submit`,
//! `device.poll` or `instance.poll_all` that finds the work complete: wgpu-core's `Queue::submit`
//! ends with a non-blocking `Device::maintain` that fires every completed submission's callbacks,
//! and on Metal the fence's completed value is set by each command buffer's completion handler.
//! Iced submits every frame it draws, and every update draws a frame, so a pass's callback runs on
//! the interface thread at the first frame submitted after the GPU finished it — usually the same
//! frame's own submit, after the rest of the window is encoded — or on the retirement worker's
//! nonblocking poll while something retires. Nothing polls or waits for it.
//!
//! A pass's figure is therefore the time from when the stage began preparing it to when the
//! interface learned it was complete: the preparation, the GPU's execution and any wait for the
//! next submit, which while frames keep coming is at most one frame. The slot keeps the newest one
//! reported. It is evidence, an upper bound on when the GPU finished, and not the status bar's
//! figure: measured on the M4, the wait for the next submit is most of it (the design's "Labels and
//! overlays during motion").
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

/// The newest completed pass a slot has heard of, shared with the callbacks of its passes.
#[derive(Default)]
pub struct PassClock {
    /// The pass and its figure in microseconds.
    done: Mutex<Option<(u64, u64)>>,
}

impl PassClock {
    /// Ask the queue to report pass `pass`, prepared since `started`, once the GPU has finished
    /// every submission so far. Call it right after the pass's own submit.
    pub fn follow(self: &Arc<Self>, queue: &wgpu::Queue, pass: u64, started: Instant) {
        let clock = Arc::clone(self);
        queue.on_submitted_work_done(move || {
            clock.resolve(pass, started.elapsed().as_micros() as u64);
        });
    }

    /// Record pass `pass` complete after `us`, unless a newer pass was reported already.
    fn resolve(&self, pass: u64, us: u64) {
        let mut done = self.done.lock().expect("pass clock lock");
        if done.is_none_or(|(newest, _)| pass > newest) {
            *done = Some((pass, us));
        }
    }

    /// The newest completed pass's figure in microseconds, `None` before any is reported.
    pub fn figure(&self) -> Option<u64> {
        self.done.lock().expect("pass clock lock").map(|(_, us)| us)
    }

    /// The newest completed pass, `None` before any is reported.
    #[cfg(any(test, feature = "qualification"))]
    pub fn newest(&self) -> Option<u64> {
        self.done
            .lock()
            .expect("pass clock lock")
            .map(|(pass, _)| pass)
    }
}
