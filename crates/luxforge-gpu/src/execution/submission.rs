//! Waiting for a submission without the backend's own millisecond sleep.
use std::time::Duration;

/// Wait until the device has done `submission` (or all its work, given `None`), as
/// `PollType::Wait` with no timeout does, and with what that poll answers. wgpu's Metal backend
/// waits by looking at the command buffer and then sleeping a whole millisecond before it looks
/// again, which macOS stretches to about 1.26 ms; a tile is mostly done well inside that. This
/// looks without the backend's sleep (a zero timeout looks once and runs the device's maintenance),
/// resting 20 µs, then twice as long each time up to 1 ms. Blocks its caller; never the interface
/// thread's.
pub(crate) fn wait_for_submission(
    device: &wgpu::Device,
    submission: Option<wgpu::SubmissionIndex>,
) -> Result<wgpu::PollStatus, wgpu::PollError> {
    let mut rest = Duration::from_micros(20);
    loop {
        match device.poll(wgpu::PollType::Wait {
            submission_index: submission.clone(),
            timeout: Some(Duration::ZERO),
        }) {
            Err(wgpu::PollError::Timeout) => {
                std::thread::sleep(rest);
                rest = (rest * 2).min(Duration::from_millis(1));
            }
            answer => return answer,
        }
    }
}
