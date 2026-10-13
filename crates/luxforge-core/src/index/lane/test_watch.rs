//! A fixture-owned notification source. Only startup is synthesized; a test reports every later
//! change explicitly through the real lane's input channel. The roots keep the resume cursors the
//! coordinator actually asked for, so reopening is checked without inventing a replay history.
use super::Input;
use luxforge_watch::{RescanReason, Resume, WatchEvent, WatchRoot};
use std::{
    io,
    sync::{Arc, Mutex, mpsc::SyncSender},
};

#[derive(Clone, Default)]
pub(crate) struct InjectedWatcher {
    roots: Arc<Mutex<Vec<WatchRoot>>>,
}

impl InjectedWatcher {
    pub(crate) fn cursor(&self) -> Resume {
        Resume {
            volume: [1; 16],
            event_id: 10,
        }
    }

    pub(crate) fn roots(&self) -> Vec<WatchRoot> {
        self.roots.lock().unwrap().clone()
    }

    pub(crate) fn clear(&self) {
        self.roots.lock().unwrap().clear();
    }

    pub(super) fn add_root(&self, inputs: &SyncSender<Input>, root: WatchRoot) -> io::Result<()> {
        if !root.path.metadata()?.is_dir() {
            return Err(io::ErrorKind::NotADirectory.into());
        }
        self.roots.lock().unwrap().push(root.clone());
        let resume = root
            .resume
            .filter(|cursor| cursor.volume == self.cursor().volume);
        let mut events = Vec::new();
        if resume.is_none() {
            events.push(WatchEvent::Rescan {
                root: root.id,
                subtree: root.path,
                reason: RescanReason::NoReplay,
            });
        }
        events.push(WatchEvent::CaughtUp {
            root: root.id,
            cursor: Some(resume.unwrap_or_else(|| self.cursor())),
        });
        for event in events {
            inputs
                .try_send(Input::Watch(event))
                .map_err(|error| io::Error::other(format!("injected watch startup: {error}")))?;
        }
        Ok(())
    }
}
