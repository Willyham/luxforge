//! Device-scoped retirement. The caller retains its resources and independent charges through
//! a neutral completion callback; this is the existing single worker, idle-blocking and polling
//! nonblocking maintenance every 2 ms only while submissions remain outstanding.
use std::sync::{
    Arc, Mutex,
    mpsc::{self, Sender},
};

/// Start the one retirement lane of a device. Returning its sender retains the lane; it exits
/// once the last sender is dropped and every queued submission has completed or failed.
pub fn start<T: Send + 'static>(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    finish: Arc<dyn Fn(T, bool) + Send + Sync>,
) -> Sender<T> {
    let (sender, receiver) = mpsc::channel();
    let device = device.clone();
    let queue = queue.clone();
    std::thread::spawn(move || worker(device, queue, receiver, finish));
    sender
}

fn worker<T: Send + 'static>(
    device: wgpu::Device,
    queue: wgpu::Queue,
    receiver: std::sync::mpsc::Receiver<T>,
    finish: Arc<dyn Fn(T, bool) + Send + Sync>,
) {
    let mut pending: Vec<Arc<Mutex<Option<T>>>> = Vec::new();
    loop {
        // Sleep indefinitely when idle. While one or two resources retire, poll maintenance at
        // bounded intervals; wgpu invokes completion callbacks only during submit or poll.
        let incoming = if pending.is_empty() {
            match receiver.recv() {
                Ok(retired) => Some(retired),
                Err(_) => break,
            }
        } else {
            match receiver.recv_timeout(std::time::Duration::from_millis(2)) {
                Ok(retired) => Some(retired),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    None
                }
            }
        };
        if let Some(retired) = incoming {
            // `prepare` removed this picture before the current draw is encoded. A previous draw
            // was already submitted; this empty submit also flushes any pending write_texture
            // staging before the completion callback's fence is registered.
            queue.submit(None);
            let slot = Arc::new(Mutex::new(Some(retired)));
            let callback_slot = Arc::clone(&slot);
            let callback_finish = Arc::clone(&finish);
            queue.on_submitted_work_done(move || {
                if let Some(retired) = callback_slot.lock().expect("retirement slot lock").take() {
                    callback_finish(retired, false);
                }
            });
            pending.push(slot);
        }
        if !pending.is_empty() {
            if device.poll(wgpu::PollType::Poll).is_err() {
                for slot in &pending {
                    if let Some(retired) = slot.lock().expect("retirement slot lock").take() {
                        finish(retired, true);
                    }
                }
            }
            pending.retain(|slot| slot.lock().expect("retirement slot lock").is_some());
        }
    }
}
