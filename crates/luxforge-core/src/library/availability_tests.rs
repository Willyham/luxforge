use super::*;
use crate::EditorService;
use luxforge_testbase::paths::{fixture, temp_dir};
use std::fs;

struct Photograph {
    service: EditorService,
    asset: AssetId,
    row: AssetRowId,
    path: PathBuf,
    original: Vec<u8>,
}

impl Photograph {
    fn new(name: &str) -> Self {
        let dir = temp_dir(&format!("availability-{name}"));
        let path = dir.join("original.jpg");
        let original = fs::read(fixture("s0/srgb.jpg")).unwrap();
        fs::write(&path, &original).unwrap();
        let mut service = EditorService::open(&dir.join("catalog.sqlite")).unwrap();
        let asset = service.import(&path).unwrap().asset.id;
        let row = AssetRowId(
            service
                .connection
                .query_row(
                    "SELECT row_id FROM assets WHERE id = ?1",
                    [asset.as_str()],
                    |row| row.get(0),
                )
                .unwrap(),
        );
        Self {
            service,
            asset,
            row,
            path,
            original,
        }
    }

    fn plan(&self) -> Plan {
        plan(&self.service.connection, &[(self.asset.clone(), self.row)]).unwrap()
    }

    fn mark(&self, state: FileAvailability) {
        observed(&self.service.connection, &self.asset, state, now_ms()).unwrap();
    }

    fn corrupt(&self) {
        let mut bytes = self.original.clone();
        bytes[100] ^= 0xff;
        // Rewriting an existing file preserves its identity and length.
        fs::write(&self.path, bytes).unwrap();
    }

    fn check(&self) -> Observation {
        observe(self.plan(), None, &JobControl::new(), &|_| {})
            .unwrap()
            .rows
            .remove(0)
    }

    fn commit(&mut self, observation: &Observation) -> Option<AvailabilityRow> {
        let tx = self.service.connection.transaction().unwrap();
        let admitted = checked_row(&tx, observation, now_ms(), false).unwrap();
        if let Some((row, _)) = &admitted {
            record(&tx, std::slice::from_ref(row)).unwrap();
        }
        tx.commit().unwrap();
        admitted.map(|(row, _)| row)
    }
}

#[test]
fn availability_known_changed_requires_the_original_bytes_to_recover() {
    let mut photo = Photograph::new("integrity-recovery");
    photo.corrupt();
    photo.mark(FileAvailability::Changed);
    let changed = photo.check();
    assert_eq!(changed.availability, FileAvailability::Changed);
    assert!(changed.verified.is_none());
    assert_eq!(
        photo.commit(&changed).unwrap().availability,
        FileAvailability::Changed
    );

    fs::write(&photo.path, &photo.original).unwrap();
    let recovered = photo.check();
    assert_eq!(recovered.availability, FileAvailability::Available);
    assert!(recovered.verified.is_some());
    assert_eq!(
        photo.commit(&recovered).unwrap().availability,
        FileAvailability::Available
    );
    assert!(!photo.plan().groups[0].subjects[0].fingerprint_mismatch);
    assert_eq!(fs::read(&photo.path).unwrap(), photo.original);
}

#[test]
fn availability_missing_or_offline_does_not_forget_a_known_mismatch() {
    let photo = Photograph::new("integrity-retention");
    photo.corrupt();
    for state in [FileAvailability::Missing, FileAvailability::Offline] {
        photo.mark(FileAvailability::Changed);
        photo.mark(state);
        assert!(photo.plan().groups[0].subjects[0].fingerprint_mismatch);
        assert_eq!(photo.check().availability, FileAvailability::Changed);
    }
}

#[test]
fn availability_healthy_presence_checks_do_not_hash_the_original() {
    let photo = Photograph::new("metadata-only");
    let answer = observe(photo.plan(), None, &JobControl::new(), &|_| {
        panic!("an ordinary presence check must not enter fingerprint verification");
    })
    .unwrap();
    assert_eq!(answer.rows[0].availability, FileAvailability::Available);
    assert!(answer.rows[0].verified.is_none());
}

#[test]
fn availability_recovery_verification_can_be_cancelled_at_a_chunk() {
    let photo = Photograph::new("cancel-recovery");
    photo.mark(FileAvailability::Changed);
    let control = JobControl::new();
    let error = observe(photo.plan(), None, &control, &|phase| {
        if phase == Phase::Hashing {
            control.cancel("cancel fingerprint recovery");
        }
    })
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert!(photo.plan().groups[0].subjects[0].fingerprint_mismatch);
    assert_eq!(fs::read(&photo.path).unwrap(), photo.original);
}

#[test]
fn availability_stale_metadata_cannot_clear_a_newer_integrity_refusal() {
    let mut photo = Photograph::new("stale-presence");
    let answer = photo.check();
    assert_eq!(answer.availability, FileAvailability::Available);
    photo.corrupt();
    photo.mark(FileAvailability::Changed);
    assert!(photo.commit(&answer).is_none());
    assert!(photo.plan().groups[0].subjects[0].fingerprint_mismatch);
}

#[test]
fn availability_verified_bytes_must_still_match_at_commit() {
    let mut photo = Photograph::new("stale-verification");
    photo.mark(FileAvailability::Changed);
    let answer = photo.check();
    assert!(answer.verified.is_some());
    photo.corrupt();
    assert_eq!(
        photo.commit(&answer).unwrap().availability,
        FileAvailability::Changed
    );
}

#[test]
fn availability_an_observation_cannot_follow_a_replaced_locator() {
    let mut photo = Photograph::new("stale-locator");
    let answer = photo.check();
    photo.mark(FileAvailability::Changed);
    photo
        .service
        .connection
        .execute(
            "UPDATE assets SET locator = '/another/original.jpg' WHERE id = ?1",
            [photo.asset.as_str()],
        )
        .unwrap();
    assert!(photo.commit(&answer).is_none());
    assert!(photo.plan().groups[0].subjects[0].fingerprint_mismatch);
}

#[test]
fn availability_old_verification_cannot_clear_a_newer_integrity_refusal() {
    let mut photo = Photograph::new("stale-verified-integrity");
    photo.mark(FileAvailability::Changed);
    let answer = photo.check();
    assert!(answer.verified.is_some());
    // A newer refusal invalidates the proof even without a distinguishable filesystem signature.
    photo.mark(FileAvailability::Changed);
    assert!(photo.commit(&answer).is_none());
    assert!(photo.plan().groups[0].subjects[0].fingerprint_mismatch);
    let fresh = photo.check();
    assert_eq!(
        photo.commit(&fresh).unwrap().availability,
        FileAvailability::Available
    );
}
