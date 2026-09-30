//! Sending events into the caller's bounded channel without ever waiting on it. A platform
//! callback hands its events here; what the channel refuses is folded into what each root is owed
//! (one rescan of the whole root, the `CaughtUp` after it) and into the mount table the receiver
//! has not yet been told, and sent again by [`Delivery::retry`].
#![forbid(unsafe_code)]
use crate::{Mount, RescanReason, Resume, VolumeEvent, WatchEvent, mounts::mount_changes};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, PoisonError},
};

/// What became of one event handed to the sink.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sent {
    Delivered,
    /// The channel is full; nothing was sent.
    Full,
    /// The receiver is gone; nothing will be read again.
    Closed,
}

/// Tries to send one event, never waiting.
pub(crate) type Sink = Box<dyn Fn(WatchEvent) -> Sent + Send + Sync>;

/// The sink and what the receiver is owed.
pub(crate) struct Delivery {
    sink: Sink,
    state: Mutex<State>,
}

struct State {
    /// The roots whose events the channel refused, and what each is owed instead.
    owed: BTreeMap<u64, Owed>,
    /// The mount table as the receiver knows it: the table at the start, changed by every volume
    /// event delivered since.
    volumes: Vec<Mount>,
    /// Whether the table changed in a way the receiver has not been told.
    volumes_owed: bool,
}

/// What a root is owed in place of the events the channel refused, sent in this order.
struct Owed {
    /// The root's path: the subtree its rescan names.
    subtree: PathBuf,
    rescan: Option<RescanReason>,
    unwatched: Option<(PathBuf, String)>,
    caught_up: Option<Option<Resume>>,
}

impl Owed {
    fn new(subtree: &Path) -> Self {
        Self {
            subtree: subtree.to_path_buf(),
            rescan: None,
            unwatched: None,
            caught_up: None,
        }
    }

    /// Fold an event the root cannot be sent yet into what it is owed. A change or a rescan of any
    /// subtree becomes the rescan of the whole root, which covers it; a later `CaughtUp` replaces
    /// an earlier one, since it follows the rescan either way.
    fn absorb(&mut self, event: WatchEvent) {
        match event {
            WatchEvent::Changed { .. } => {
                self.rescan.get_or_insert(RescanReason::Overflow);
            }
            WatchEvent::Rescan { reason, .. } => {
                self.rescan.get_or_insert(reason);
            }
            WatchEvent::CaughtUp { cursor, .. } => self.caught_up = Some(cursor),
            WatchEvent::Unwatched { subtree, error, .. } => {
                self.unwatched.get_or_insert((subtree, error));
            }
            WatchEvent::Volume(_) => {}
        }
    }

    /// The next event owed to `root`, if any.
    fn next(&self, root: u64) -> Option<WatchEvent> {
        if let Some(reason) = self.rescan {
            Some(WatchEvent::Rescan {
                root,
                subtree: self.subtree.clone(),
                reason,
            })
        } else if let Some((subtree, error)) = &self.unwatched {
            Some(WatchEvent::Unwatched {
                root,
                subtree: subtree.clone(),
                error: error.clone(),
            })
        } else {
            self.caught_up
                .map(|cursor| WatchEvent::CaughtUp { root, cursor })
        }
    }

    /// Mark the event [`Self::next`] answered as sent.
    fn sent(&mut self) {
        if self.rescan.take().is_none() && self.unwatched.take().is_none() {
            self.caught_up = None;
        }
    }

    fn is_empty(&self) -> bool {
        self.rescan.is_none() && self.unwatched.is_none() && self.caught_up.is_none()
    }
}

impl Delivery {
    /// A delivery whose receiver knows `mounts` as the mount table.
    pub(crate) fn new(sink: Sink, mounts: Vec<Mount>) -> Self {
        Self {
            sink,
            state: Mutex::new(State {
                owed: BTreeMap::new(),
                volumes: mounts,
                volumes_owed: false,
            }),
        }
    }

    /// A panic while the lock was held leaves nothing half-written that matters here, so a
    /// poisoned lock is used as it is.
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Send `events` about `root`, whose path is `path`, in order. While the root is owed
    /// anything, its events are folded into what it is owed instead. Answers whether anything is
    /// owed now, to any root or about the volumes, so the caller can arm a retry.
    pub(crate) fn root(&self, root: u64, path: &Path, events: Vec<WatchEvent>) -> bool {
        let mut state = self.state();
        for event in events {
            if let Some(owed) = state.owed.get_mut(&root) {
                owed.absorb(event);
                continue;
            }
            let kept = event.clone();
            if (self.sink)(event) == Sent::Full {
                let mut owed = Owed::new(path);
                owed.absorb(kept);
                state.owed.insert(root, owed);
            }
        }
        state.is_owed()
    }

    /// Forget what `root` is owed, when it is no longer watched.
    pub(crate) fn forget(&self, root: u64) {
        self.state().owed.remove(&root);
    }

    /// Tell the receiver how the mount table `now` differs from the one it knows. Answers whether
    /// anything is owed now.
    pub(crate) fn volumes(&self, now: &[Mount]) -> bool {
        let mut state = self.state();
        state.volumes_owed = false;
        for change in mount_changes(&state.volumes, now) {
            match (self.sink)(WatchEvent::Volume(change.clone())) {
                Sent::Full => {
                    state.volumes_owed = true;
                    break;
                }
                Sent::Delivered | Sent::Closed => apply(&mut state.volumes, change),
            }
        }
        state.is_owed()
    }

    /// The mount table changed but could not be read: tell the receiver when it next can be.
    pub(crate) fn owe_volumes(&self) {
        self.state().volumes_owed = true;
    }

    /// Whether the receiver has not been told of a change to the mount table.
    pub(crate) fn volumes_owed(&self) -> bool {
        self.state().volumes_owed
    }

    /// Whether anything is owed, which sets how long a watcher thread may block.
    #[cfg_attr(
        target_os = "macos",
        allow(dead_code, reason = "macOS arms a timer instead of blocking a thread")
    )]
    pub(crate) fn owed(&self) -> bool {
        self.state().is_owed()
    }

    /// Send what the roots are owed, in root order, until the channel is full again. The mount
    /// table's difference is sent again by [`Self::volumes`] with the table read then. Answers
    /// whether anything is still owed.
    pub(crate) fn retry(&self) -> bool {
        let mut state = self.state();
        let roots: Vec<u64> = state.owed.keys().copied().collect();
        'roots: for root in roots {
            let owed = state.owed.get_mut(&root).expect("an owed root");
            while let Some(event) = owed.next(root) {
                if (self.sink)(event) == Sent::Full {
                    break 'roots;
                }
                owed.sent();
            }
            if owed.is_empty() {
                state.owed.remove(&root);
            }
        }
        state.is_owed()
    }
}

impl State {
    fn is_owed(&self) -> bool {
        self.volumes_owed || !self.owed.is_empty()
    }
}

/// The mount table `known` after the receiver was told `change`.
fn apply(known: &mut Vec<Mount>, change: VolumeEvent) {
    match change {
        VolumeEvent::Unmounted { mount_point } => {
            known.retain(|mount| mount.mount_point != mount_point);
        }
        VolumeEvent::Mounted { mount } => {
            known.retain(|known| known.mount_point != mount.mount_point);
            known.push(mount);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{Receiver, TrySendError, sync_channel};

    fn delivery(capacity: usize, mounts: Vec<Mount>) -> (Delivery, Receiver<WatchEvent>) {
        let (events, receiver) = sync_channel(capacity);
        let sink: Sink = Box::new(move |event| match events.try_send(event) {
            Ok(()) => Sent::Delivered,
            Err(TrySendError::Full(_)) => Sent::Full,
            Err(TrySendError::Disconnected(_)) => Sent::Closed,
        });
        (Delivery::new(sink, mounts), receiver)
    }

    fn changed(root: u64, path: &str) -> WatchEvent {
        WatchEvent::Changed {
            root,
            paths: vec![PathBuf::from(path)],
            cursor: None,
        }
    }

    fn mount(point: &str, name: &str) -> Mount {
        Mount {
            mount_point: PathBuf::from(point),
            name: Some(name.to_owned()),
            uuid: None,
            file_system: "msdos".to_owned(),
            removable: true,
            local: true,
            browsable: true,
            root: false,
        }
    }

    fn drain(receiver: &Receiver<WatchEvent>) -> Vec<WatchEvent> {
        receiver.try_iter().collect()
    }

    #[test]
    fn what_a_full_channel_refuses_becomes_one_rescan_then_the_caught_up_it_was_owed() {
        let (delivery, receiver) = delivery(1, Vec::new());
        let root = Path::new("/photos");
        let cursor = Some(Resume {
            volume: [1; 16],
            event_id: 9,
        });
        assert!(!delivery.root(1, root, vec![changed(1, "/photos/a")]));
        // The channel is full: the next change is refused, and every later event for the root is
        // withheld, while another root is still sent to once there is room.
        assert!(delivery.root(
            1,
            root,
            vec![
                changed(1, "/photos/b"),
                WatchEvent::CaughtUp { root: 1, cursor },
                changed(1, "/photos/c"),
            ]
        ));
        assert!(delivery.owed());
        assert_eq!(drain(&receiver), [changed(1, "/photos/a")]);
        assert!(delivery.root(2, Path::new("/cards"), vec![changed(2, "/cards/x")]));
        assert_eq!(drain(&receiver), [changed(2, "/cards/x")]);
        // Retrying sends the rescan, then stops at the full channel; the next retry sends the
        // caught-up and owes nothing more.
        assert!(delivery.retry());
        assert_eq!(
            drain(&receiver),
            [WatchEvent::Rescan {
                root: 1,
                subtree: root.to_path_buf(),
                reason: RescanReason::Overflow
            }]
        );
        assert!(!delivery.retry());
        assert_eq!(drain(&receiver), [WatchEvent::CaughtUp { root: 1, cursor }]);
        assert!(!delivery.owed());
        // Once nothing is owed, the root's events are sent as they come.
        assert!(!delivery.root(1, root, vec![changed(1, "/photos/d")]));
        assert_eq!(drain(&receiver), [changed(1, "/photos/d")]);
    }

    #[test]
    fn a_refused_rescan_keeps_its_reason_and_a_refused_unwatched_its_error() {
        let (delivery, receiver) = delivery(1, Vec::new());
        let root = Path::new("/photos");
        delivery.root(1, root, vec![changed(1, "/photos/a")]);
        delivery.root(
            1,
            root,
            vec![
                WatchEvent::Rescan {
                    root: 1,
                    subtree: PathBuf::from("/photos/2024"),
                    reason: RescanReason::Dropped,
                },
                WatchEvent::Unwatched {
                    root: 1,
                    subtree: PathBuf::from("/photos/2025"),
                    error: "the watch limit is reached".into(),
                },
            ],
        );
        let mut seen = drain(&receiver);
        while delivery.retry() {
            seen.extend(drain(&receiver));
        }
        seen.extend(drain(&receiver));
        assert_eq!(
            seen,
            [
                changed(1, "/photos/a"),
                WatchEvent::Rescan {
                    root: 1,
                    subtree: root.to_path_buf(),
                    reason: RescanReason::Dropped
                },
                WatchEvent::Unwatched {
                    root: 1,
                    subtree: PathBuf::from("/photos/2025"),
                    error: "the watch limit is reached".into()
                }
            ]
        );
    }

    #[test]
    fn a_forgotten_root_is_owed_nothing_and_a_closed_channel_nothing_at_all() {
        let (delivery, receiver) = delivery(1, Vec::new());
        delivery.root(1, Path::new("/a"), vec![changed(1, "/a/1")]);
        assert!(delivery.root(1, Path::new("/a"), vec![changed(1, "/a/2")]));
        delivery.forget(1);
        assert!(!delivery.owed());
        drop(receiver);
        assert!(!delivery.root(1, Path::new("/a"), vec![changed(1, "/a/3")]));
        assert!(!delivery.volumes(&[mount("/Volumes/CARD", "CARD")]));
    }

    #[test]
    fn the_mount_table_is_told_as_its_difference_and_sent_again_from_the_table_then() {
        let internal = mount("/", "Macintosh HD");
        let (delivery, receiver) = delivery(1, vec![internal.clone()]);
        let card = mount("/Volumes/CARD", "CARD");
        let other = mount("/Volumes/OTHER", "OTHER");
        // The same table tells nothing.
        assert!(!delivery.volumes(std::slice::from_ref(&internal)));
        assert!(drain(&receiver).is_empty());
        // Two mounts at once: the second is refused and owed.
        assert!(delivery.volumes(&[internal.clone(), card.clone(), other.clone()]));
        assert!(delivery.volumes_owed());
        assert_eq!(
            drain(&receiver),
            [WatchEvent::Volume(VolumeEvent::Mounted {
                mount: card.clone()
            })]
        );
        // By the retry the card was taken out again: the receiver hears of that and of the other
        // volume, from the table then.
        assert!(delivery.volumes(&[internal.clone(), other.clone()]));
        let mut told = drain(&receiver);
        assert!(!delivery.volumes(&[internal.clone(), other.clone()]));
        told.extend(drain(&receiver));
        assert!(
            told.contains(&WatchEvent::Volume(VolumeEvent::Unmounted {
                mount_point: card.mount_point.clone()
            })) && told.contains(&WatchEvent::Volume(VolumeEvent::Mounted { mount: other })),
            "{told:?}"
        );
        assert!(!delivery.owed());
    }
}
