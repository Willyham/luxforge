//! Shared decoded-preview mechanics. Grid/loupe owners keep their wanted sets, roles, retry,
//! ordering, byte/count ceilings and distinct Latest cancellation policies.
use iced::widget::image::Handle;
use luxforge_core::{Cancel, DecodedPreview, ErrorKind, decode_preview};
use std::{path::Path, sync::mpsc::SyncSender};

pub(super) trait SourceKey {
    fn source_key(&self) -> &str;
}

impl SourceKey for String {
    fn source_key(&self) -> &str {
        self
    }
}

/// One image handle and its charge, with only the owning controller's presentation metadata.
pub(super) struct Held<M> {
    pub(super) handle: Handle,
    pub(super) side: u32,
    pub(super) bytes: usize,
    pub(super) metadata: M,
}

impl<M: SourceKey> Held<M> {
    pub(super) fn new(metadata: M, side: u32, preview: DecodedPreview) -> Self {
        Self {
            bytes: preview.rgba.len(),
            handle: image_handle(preview),
            side,
            metadata,
        }
    }

    pub(super) fn key(&self) -> &str {
        self.metadata.source_key()
    }

    pub(super) fn matches(&self, key: &str, side: u32) -> bool {
        self.key() == key && self.side >= side
    }
}

/// Result acceptance follows source identity rather than the worker plan number: both controllers
/// intentionally retain a finished decode when a newer plan still wants that source.
pub(super) enum Adoption {
    Stale,
    Failed,
    Reused,
    Ready(DecodedPreview),
}

pub(super) fn accept<M: SourceKey>(
    source: Option<&str>,
    held: Option<&Held<M>>,
    key: &str,
    side: u32,
    result: Result<DecodedPreview, String>,
) -> Adoption {
    if source != Some(key) {
        return Adoption::Stale;
    }
    match result {
        Err(_) => Adoption::Failed,
        Ok(_) if held.is_some_and(|held| held.matches(key, side)) => Adoption::Reused,
        Ok(preview) => Adoption::Ready(preview),
    }
}

pub(crate) struct Decoded<D> {
    pub(crate) decode: D,
    pub(crate) result: Result<DecodedPreview, String>,
}

impl<D> Decoded<D> {
    pub(super) fn handoff(self, sender: &SyncSender<Self>, wake: fn()) -> bool {
        if sender.send(self).is_err() {
            return false;
        }
        wake();
        true
    }
}

/// Decode on the caller's worker with its cancellation policy. Cancellation publishes no failed
/// source; every other error is delivered for the owner's existing retry policy.
pub(super) fn decode(
    path: &Path,
    side: u32,
    cancel: &Cancel,
) -> Option<Result<DecodedPreview, String>> {
    match decode_preview(path, side, cancel) {
        Err(error) if error.kind == ErrorKind::Cancelled => None,
        result => Some(result.map_err(|error| error.to_string())),
    }
}

/// Called only on accepted results, once. Both caches and the loupe inset use the same handle home.
pub(super) fn image_handle(preview: DecodedPreview) -> Handle {
    let DecodedPreview {
        width,
        height,
        rgba,
    } = preview;
    Handle::from_rgba(width, height, rgba)
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Usage {
    pub(super) bytes: usize,
    pub(super) handles: usize,
}

#[derive(Clone, Copy)]
pub(super) struct Limits {
    pub(super) bytes: usize,
    pub(super) handles: usize,
}

impl Usage {
    pub(super) fn replace(&mut self, old: Option<usize>, new: usize) {
        self.bytes = self.bytes - old.unwrap_or(0) + new;
        self.handles += usize::from(old.is_none());
    }

    pub(super) fn remove(&mut self, bytes: usize) {
        self.bytes -= bytes;
        self.handles -= 1;
    }

    fn replacing(self, incoming: usize, replaced: Option<usize>) -> Self {
        Self {
            bytes: self.bytes - replaced.unwrap_or(0) + incoming,
            handles: self.handles + usize::from(replaced.is_none()),
        }
    }

    fn fits(self, limits: Limits) -> bool {
        self.bytes <= limits.bytes && self.handles <= limits.handles
    }

    pub(super) fn fits_replacing(
        self,
        incoming: usize,
        replaced: Option<usize>,
        limits: Limits,
    ) -> bool {
        self.replacing(incoming, replaced).fits(limits)
    }

    /// Candidates are already ordered and protected by the controller. Clone only selected keys,
    /// and evict nothing when all eligible candidates still cannot make the result fit.
    pub(super) fn evictions<'a, K: Clone + 'a>(
        self,
        incoming: usize,
        replaced: Option<usize>,
        limits: Limits,
        candidates: impl Iterator<Item = (&'a K, usize)> + Clone,
    ) -> Option<Vec<K>> {
        let mut usage = self.replacing(incoming, replaced);
        let mut count = 0;
        for (_, bytes) in candidates.clone() {
            if usage.fits(limits) {
                break;
            }
            usage.remove(bytes);
            count += 1;
        }
        usage
            .fits(limits)
            .then(|| candidates.take(count).map(|(key, _)| key.clone()).collect())
    }
}

pub(super) fn may_evict<K: PartialEq>(
    held: &K,
    incoming: &K,
    shown: bool,
    wanted: bool,
    incoming_shown: bool,
) -> bool {
    held != incoming && !shown && (incoming_shown || !wanted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixels() -> DecodedPreview {
        DecodedPreview {
            width: 2,
            height: 2,
            rgba: vec![128; 16],
        }
    }

    #[test]
    fn decoded_handles_reject_stale_results_and_reuse_a_sufficient_handle() {
        let held = Held::new("first".to_owned(), 40, pixels());
        let id = held.handle.id();
        assert!(matches!(
            accept(
                Some("second"),
                Some(&held),
                "first",
                40,
                Err("old decode failed".into())
            ),
            Adoption::Stale
        ));
        assert!(matches!(
            accept(Some("first"), Some(&held), "first", 20, Ok(pixels())),
            Adoption::Reused
        ));
        assert_eq!(held.handle.id(), id);
        assert!(matches!(
            accept(Some("first"), Some(&held), "first", 80, Ok(pixels())),
            Adoption::Ready(_)
        ));
        assert!(matches!(
            accept(
                Some("first"),
                Some(&held),
                "first",
                20,
                Err("decode failed".into())
            ),
            Adoption::Failed
        ));
        assert!(matches!(
            accept::<String>(None, None, "first", 20, Ok(pixels())),
            Adoption::Stale
        ));
    }

    #[test]
    fn decoded_handles_charge_replacements_and_evict_atomically_with_both_caps() {
        let mut usage = Usage::default();
        usage.replace(None, 80);
        usage.replace(None, 20);
        usage.replace(Some(80), 40);
        assert_eq!((usage.bytes, usage.handles), (60, 2));
        let keys = [("old", 40), ("margin", 20)];
        let candidates = || keys.iter().map(|(key, bytes)| (key, *bytes));
        let limits = Limits {
            bytes: 100,
            handles: 2,
        };
        assert_eq!(
            usage.evictions(30, None, limits, candidates()),
            Some(vec!["old"])
        );
        assert_eq!(
            usage.evictions(110, None, limits, candidates()),
            None,
            "an oversize result evicts nothing"
        );
        assert_eq!((usage.bytes, usage.handles), (60, 2));
        assert!(usage.fits_replacing(50, Some(40), limits));
        usage.remove(40);
        assert_eq!((usage.bytes, usage.handles), (20, 1));
        assert!(!may_evict(&"shown", &"incoming", true, false, true));
        assert!(!may_evict(&"incoming", &"incoming", false, false, true));
        assert!(!may_evict(&"margin", &"incoming", false, true, false));
        assert!(may_evict(&"margin", &"incoming", false, true, true));
    }

    #[test]
    fn decoded_handles_cancellation_publishes_no_failure_and_handoff_wakes_once() {
        let cancel = Cancel::new();
        cancel.cancel();
        assert!(decode(Path::new("not-read-when-cancelled.jpg"), 40, &cancel).is_none());
        static WAKES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        fn wake() {
            WAKES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        assert!(
            Decoded {
                decode: 7,
                result: Ok(pixels())
            }
            .handoff(&sender, wake)
        );
        assert_eq!(receiver.recv().unwrap().decode, 7);
        drop(receiver);
        assert!(
            !Decoded {
                decode: 8,
                result: Ok(pixels())
            }
            .handoff(&sender, wake)
        );
        assert_eq!(WAKES.load(std::sync::atomic::Ordering::Relaxed), 1);
    }
}
