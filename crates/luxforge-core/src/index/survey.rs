//! The survey: what the catalog owner needs to answer `volume.list`, `card.list` and
//! `index.folders` at once, learned off the owner. The owner reads the platform's mount table
//! itself, which waits on no file system ([`super::volumes::MountSource::list`]); everything that
//! stats — each mounted volume's identity, its `DCIM` folder, and whether an indexed folder on a
//! volume that is not mounted is there anyway — is learned here, on the index lane's survey thread,
//! and posted back as [`Survey`]s, which the owner keeps as [`Known`].
//!
//! - **Local volumes first.** A network volume's server may not answer, and a stat of anything on
//!   it can wait for as long as it does. So a survey learns the local volumes and the folders not
//!   on a network volume first and posts them, then learns the network volumes and posts again. A
//!   hung network volume holds only the second post: the owner answers from the first.
//! - **What is still mounted is the owner's.** The owner matches what a survey learned against the
//!   mount table it reads at each call ([`Known::now`]), so a volume taken out is gone from the next
//!   answer at once; one mounted since the survey appears once the next survey has learned it.
use super::volumes::{MountTable, Mounted, PlatformMount as Mount};
use crate::catalog_types::VolumeId;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

/// What one post of a survey learned.
#[derive(Debug)]
pub(crate) struct Survey {
    /// The platform's mounts when the survey began, in its order.
    pub listed: Vec<Mount>,
    /// The volumes this post learned: each one's identity, label and `DCIM` folder.
    pub learned: Vec<Mounted>,
    /// The indexed folders this post settles: whether each is there matters only while its volume
    /// is not mounted.
    pub checked: Vec<PathBuf>,
    /// The folders among `checked` whose volume was not mounted and that were there anyway.
    pub present: Vec<PathBuf>,
}

/// Survey the mounted file systems `mounts` and the indexed `folders` (each with the volume it was
/// added on), posting what it learned: the local volumes and the folders not on a network volume
/// first, then, when there are any, the network volumes and the rest. `post` is told whether the
/// post is the survey's last. Off the owner only: every volume is statted.
pub(crate) fn survey(
    mounts: Vec<Mount>,
    folders: &[(PathBuf, VolumeId)],
    now_ms: i64,
    mut post: impl FnMut(Survey, bool),
) {
    let network: Vec<Mount> = mounts
        .iter()
        .filter(|mount| !mount.local && mount.browsable)
        .cloned()
        .collect();
    let on_network = |path: &Path| {
        network
            .iter()
            .any(|mount| path.starts_with(&mount.mount_point))
    };
    let (near, far): (Vec<_>, Vec<_>) = folders.iter().partition(|(path, _)| !on_network(path));
    // The system's own mounts and the startup disk's data volume go with the local ones: the table
    // skips the first without a stat, and needs the second to name the startup disk.
    let local = mounts
        .iter()
        .filter(|mount| mount.local || !mount.browsable)
        .cloned()
        .collect();
    let mut learned = learn(local, now_ms);
    let (checked, present) = look_for(&near, &learned);
    let last = network.is_empty();
    post(
        Survey {
            listed: mounts.clone(),
            learned: learned.clone(),
            checked,
            present,
        },
        last,
    );
    if last {
        return;
    }
    let remote = learn(network, now_ms);
    learned.extend(remote.iter().cloned());
    let (checked, present) = look_for(&far, &learned);
    post(
        Survey {
            listed: mounts,
            learned: remote,
            checked,
            present,
        },
        true,
    );
}

/// The volumes `mounts` hold, each with its identity and its `DCIM` folder.
fn learn(mounts: Vec<Mount>, now_ms: i64) -> Vec<Mounted> {
    let mut table = MountTable::of(mounts, now_ms);
    table.find_cards();
    table.into_volumes()
}

/// Settle `folders` against the volumes `mounted`: every one is checked, and each whose volume is
/// not mounted is looked for (one stat).
fn look_for(folders: &[&(PathBuf, VolumeId)], mounted: &[Mounted]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let present = folders
        .iter()
        .filter(|(_, volume)| !mounted.iter().any(|known| known.volume.id == *volume))
        .filter(|(path, _)| path.symlink_metadata().is_ok())
        .map(|(path, _)| path.clone())
        .collect();
    let checked = folders.iter().map(|(path, _)| path.clone()).collect();
    (checked, present)
}

/// What the owner keeps of the surveys: every volume learned whose mount was still listed by the
/// latest survey, and the indexed folders found there while their volume was not mounted.
#[derive(Debug, Default)]
pub(crate) struct Known {
    volumes: Vec<Mounted>,
    present: HashSet<PathBuf>,
}

impl Known {
    /// Take in one post: its volumes replace what was learned of the same mounts, a volume whose
    /// mount the survey no longer listed is forgotten, and one it listed but has not learned yet is
    /// kept until it does.
    pub(crate) fn merge(&mut self, survey: Survey) {
        let Survey {
            listed,
            learned,
            checked,
            present,
        } = survey;
        let mut volumes = learned;
        for earlier in std::mem::take(&mut self.volumes) {
            if listed.contains(&earlier.source)
                && !volumes.iter().any(|known| known.source == earlier.source)
            {
                volumes.push(earlier);
            }
        }
        self.volumes = volumes;
        for path in &checked {
            self.present.remove(path);
        }
        self.present.extend(present);
    }

    /// The volumes mounted now, as the owner knows them without touching a file system: each one
    /// learned whose mount is in `mounts`, the platform's table read now, in that table's order.
    pub(crate) fn now(&self, mounts: &[Mount]) -> Mounts<'_> {
        Mounts {
            volumes: mounts
                .iter()
                .filter_map(|mount| self.volumes.iter().find(|known| known.source == *mount))
                .collect(),
            present: &self.present,
        }
    }
}

/// The volumes mounted now as the owner knows them ([`Known::now`]).
pub(crate) struct Mounts<'k> {
    volumes: Vec<&'k Mounted>,
    present: &'k HashSet<PathBuf>,
}

impl Mounts<'_> {
    /// The mounted volumes, in the platform's order.
    pub(crate) fn volumes(&self) -> impl Iterator<Item = &Mounted> {
        self.volumes.iter().copied()
    }

    pub(crate) fn is_mounted(&self, id: &VolumeId) -> bool {
        self.get(id).is_some()
    }

    pub(crate) fn get(&self, id: &VolumeId) -> Option<&Mounted> {
        self.volumes().find(|mounted| mounted.volume.id == *id)
    }

    /// Whether the indexed folder at `path`, added on the volume `volume`, is offline: its volume
    /// is not mounted and the last survey did not find it there.
    pub(crate) fn offline(&self, path: &Path, volume: &VolumeId) -> bool {
        !self.is_mounted(volume) && !self.present.contains(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::volumes::tests::mount_at;

    /// Local volumes are posted before network ones; what the owner knows follows the table it
    /// reads now, so a volume taken out is gone at once and one mounted since waits for a survey;
    /// a folder on a volume not mounted is offline unless a survey found it there.
    #[test]
    fn a_survey_posts_local_volumes_first_and_the_owner_follows_its_own_table() {
        let dir = luxforge_testbase::paths::temp_dir("survey")
            .canonicalize()
            .unwrap();
        let card = dir.join("CARD");
        let share = dir.join("share");
        let gone = dir.join("DRIVE");
        std::fs::create_dir_all(card.join("DCIM")).unwrap();
        std::fs::create_dir_all(share.join("DCIM")).unwrap();
        let there = dir.join("elsewhere/photos");
        std::fs::create_dir_all(&there).unwrap();
        let card_mount = mount_at(&card, "CARD", 1, true);
        let mut share_mount = mount_at(&share, "share", 2, false);
        share_mount.local = false;
        let drive = VolumeId::parse(format!("volume-{}", "03".repeat(16))).unwrap();
        let folders = vec![
            (gone.join("trip"), drive.clone()),
            (there.clone(), drive.clone()),
            (share.join("album"), drive.clone()),
        ];
        let mut posts = Vec::new();
        survey(
            vec![card_mount.clone(), share_mount.clone()],
            &folders,
            5,
            |survey, last| posts.push((survey, last)),
        );
        assert_eq!(posts.len(), 2);
        let labels = |survey: &Survey| -> Vec<String> {
            survey
                .learned
                .iter()
                .map(|mounted| mounted.volume.label.clone())
                .collect()
        };
        assert_eq!(labels(&posts[0].0), ["CARD"], "local first");
        assert!(!posts[0].1);
        assert_eq!(posts[0].0.checked, [gone.join("trip"), there.clone()]);
        assert_eq!(posts[0].0.present, std::slice::from_ref(&there));
        assert_eq!(labels(&posts[1].0), ["share"]);
        assert!(posts[1].1, "the last post");
        assert_eq!(posts[1].0.checked, [share.join("album")]);

        let mut known = Known::default();
        let mut posts = posts.into_iter();
        known.merge(posts.next().unwrap().0);
        let table = [card_mount.clone(), share_mount.clone()];
        let now = known.now(&table);
        assert_eq!(now.volumes().count(), 1, "the share is not learned yet");
        let card_volume = now.volumes().next().unwrap();
        assert_eq!(card_volume.card_folder(), Some(card.join("DCIM")));
        assert!(now.offline(&gone.join("trip"), &drive));
        assert!(!now.offline(&there, &drive), "found there");
        known.merge(posts.next().unwrap().0);
        assert_eq!(known.now(&table).volumes().count(), 2);
        let card_id = known
            .now(&table)
            .volumes()
            .next()
            .unwrap()
            .volume
            .id
            .clone();
        // Taken out: gone from the next answer without a survey.
        let now = known.now(std::slice::from_ref(&share_mount));
        assert!(!now.is_mounted(&card_id));
        assert_eq!(now.volumes().count(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
