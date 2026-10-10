//! Normalisation of an accepted batch into NDJSON rows for the private collector: one row per event
//! and one profile row per batch. Nothing but validated batch content and the server's receive time
//! enters a row; no connection, header or body metadata is available here to leak.

use crate::wire::{Batch, EventFields, profile_columns};
use serde::Serialize;
use serde::ser::{SerializeMap, Serializer};
use serde_json::Value;

/// One event, as a collector row. Absent event fields are `null`.
#[derive(Debug, Serialize)]
pub struct EventRow<'a> {
    pub received_at: &'a str,
    pub installation_id: &'a str,
    pub session_id: &'a str,
    pub session_start: &'a str,
    pub batch_seq: u32,
    pub seq: u32,
    pub t: u32,
    pub name: &'static str,
    pub release: &'a str,
    #[serde(flatten)]
    pub fields: &'a EventFields,
}

/// The batch's release, sequence number, dropped-event count and device profile, flattened to columns
/// in a fixed order.
pub struct ProfileRow<'a> {
    pub received_at: &'a str,
    pub batch: &'a Batch,
}

impl Serialize for ProfileRow<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let batch = self.batch;
        let columns = profile_columns();
        let mut map = serializer.serialize_map(Some(6 + columns.len()))?;
        map.serialize_entry("received_at", self.received_at)?;
        map.serialize_entry("installation_id", &batch.installation_id)?;
        map.serialize_entry("session_id", &batch.session_id)?;
        map.serialize_entry("release", &batch.release)?;
        map.serialize_entry("batch_seq", &batch.batch_seq)?;
        map.serialize_entry("dropped", &batch.dropped)?;
        for column in &columns {
            map.serialize_entry(
                column,
                batch.device.columns.get(column).unwrap_or(&Value::Null),
            )?;
        }
        map.end()
    }
}

/// The two NDJSON bodies one accepted batch produces.
#[derive(Debug, PartialEq, Eq)]
pub struct Rows {
    /// One line per event.
    pub events: Vec<u8>,
    /// Exactly one line.
    pub profile: Vec<u8>,
}

impl Rows {
    pub fn len(&self) -> usize {
        self.events.len() + self.profile.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty() && self.profile.is_empty()
    }
}

pub fn normalize(batch: &Batch, unix_secs: u64) -> Rows {
    let received_at = utc(unix_secs);
    let mut events = Vec::new();
    for event in &batch.events {
        let row = EventRow {
            received_at: &received_at,
            installation_id: &batch.installation_id,
            session_id: &batch.session_id,
            session_start: &batch.session_start,
            batch_seq: batch.batch_seq,
            seq: event.seq,
            t: event.t,
            name: event.name,
            release: &batch.release,
            fields: &event.fields,
        };
        serde_json::to_writer(&mut events, &row).expect("rows serialize");
        events.push(b'\n');
    }
    let mut profile = Vec::new();
    serde_json::to_writer(
        &mut profile,
        &ProfileRow {
            received_at: &received_at,
            batch,
        },
    )
    .expect("rows serialize");
    profile.push(b'\n');
    Rows { events, profile }
}

/// Unix seconds as `YYYY-MM-DDTHH:MM:SSZ` (UTC), the form `session_start` also has.
pub fn utc(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let rest = unix_secs % 86_400;
    // Civil date from a day count (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::utc;

    #[test]
    fn unix_seconds_become_utc_dates() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(utc(1_760_018_592), "2025-10-09T14:03:12Z");
        assert_eq!(utc(1_791_554_592), "2026-10-09T14:03:12Z");
        assert_eq!(utc(4_102_444_799), "2099-12-31T23:59:59Z");
        assert_eq!(utc(4_107_542_400), "2100-03-01T00:00:00Z");
    }
}
