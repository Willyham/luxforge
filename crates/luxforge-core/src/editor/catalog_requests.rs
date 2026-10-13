//! Catalog-wide retry identity, independent of photographs and immutable history.
use crate::{Error, JobId};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

/// Canonical identity shared with the bounded owner cache.
#[derive(Clone, Debug)]
pub(crate) struct Key {
    pub scope: String,
    pub request_id: String,
    pub input: [u8; 32],
    pub method: String,
}

/// Larger than the owner cache: the bounded 50,000-item report remains durable after cache eviction.
pub(crate) const MAX_RESULT_BYTES: usize = 256 * 1024 * 1024;

pub(super) const SCHEMA: &str = "
CREATE TABLE catalog_requests (
 scope TEXT NOT NULL, request_id TEXT NOT NULL, method TEXT NOT NULL,
 input_hash BLOB NOT NULL CHECK(length(input_hash)=32),
 job_id TEXT, job_kind TEXT, asset_id TEXT,
 resolved_json TEXT CHECK(resolved_json IS NULL OR length(CAST(resolved_json AS BLOB)) <= 268435456),
 result_json TEXT CHECK(result_json IS NULL OR length(CAST(result_json AS BLOB)) <= 268435456),
 error_json TEXT CHECK(error_json IS NULL OR length(CAST(error_json AS BLOB)) <= 268435456),
 CHECK((job_id IS NULL AND result_json IS NOT NULL AND error_json IS NULL)
       OR (job_id IS NOT NULL AND job_kind IS NOT NULL)),
 CHECK(result_json IS NULL OR error_json IS NULL),
 PRIMARY KEY(scope,request_id)
) WITHOUT ROWID;
CREATE INDEX catalog_requests_by_job ON catalog_requests(job_id);
CREATE TRIGGER catalog_requests_identity_immutable BEFORE UPDATE OF
 scope,request_id,method,input_hash,job_id,job_kind,asset_id,resolved_json ON catalog_requests BEGIN
 SELECT RAISE(ABORT,'catalog request identity is immutable'); END;
CREATE TRIGGER catalog_requests_results_immutable BEFORE UPDATE OF result_json,error_json ON catalog_requests
WHEN OLD.result_json IS NOT NULL OR OLD.error_json IS NOT NULL BEGIN
 SELECT RAISE(ABORT,'catalog request outcomes are immutable'); END;
CREATE TRIGGER catalog_requests_are_permanent BEFORE DELETE ON catalog_requests BEGIN
 SELECT RAISE(ABORT,'catalog requests are permanent'); END;
";

pub(crate) struct Recorded {
    pub job_id: Option<JobId>,
    pub job_kind: Option<String>,
    pub asset_id: Option<crate::AssetId>,
    pub answer: Option<Value>,
    pub error: Option<Error>,
}

/// One indexed bounded result read, with identity matching before callers resolve any targets.
pub(crate) fn lookup(connection: &Connection, key: &Key) -> Result<Option<Recorded>, Error> {
    let row = connection.query_row(
        "SELECT input_hash,job_id,job_kind,asset_id,CASE WHEN input_hash=?3 THEN result_json END,CASE WHEN input_hash=?3 THEN error_json END FROM catalog_requests WHERE scope=?1 AND request_id=?2",
        params![key.scope,key.request_id,key.input.as_slice()],
        |r| Ok((r.get::<_,Vec<u8>>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,Option<String>>(3)?,bounded_text(r,4)?,bounded_text(r,5)?)),
    ).optional()?;
    let Some((input, job_id, job_kind, asset_id, answer, error)) = row else {
        return Ok(None);
    };
    if input != key.input {
        return Err(Error::conflict(
            "request_id was already used with different input",
        ));
    }
    Ok(Some(Recorded {
        job_id: job_id
            .map(|id| serde_json::from_value(Value::String(id)))
            .transpose()
            .map_err(|e| Error::incompatible(e.to_string()))?,
        job_kind,
        asset_id: asset_id
            .map(|id| serde_json::from_value(Value::String(id)))
            .transpose()
            .map_err(|e| Error::incompatible(e.to_string()))?,
        answer: answer
            .map(|v| super::catalog::decode("invalid catalog request answer", v))
            .transpose()?,
        error: error
            .map(|v| {
                super::catalog::decode::<StoredError>("invalid catalog request error", v)
                    .map(StoredError::into_error)
            })
            .transpose()?,
    }))
}

/// Called inside the writer's transaction, including successful no-ops.
pub(crate) fn record(
    connection: &Connection,
    key: Option<&Key>,
    answer: &impl serde::Serialize,
) -> Result<(), Error> {
    let Some(key) = key else { return Ok(()) };
    let result = encode_bounded(answer)?;
    connection.execute("INSERT INTO catalog_requests(scope,request_id,method,input_hash,result_json) VALUES(?1,?2,?3,?4,?5)",params![key.scope,key.request_id,key.method,key.input.as_slice(),result])?;
    Ok(())
}

/// Admission precedes enqueue, so a dead owner's job can never restart against new selection.
pub(crate) fn admit(
    connection: &Connection,
    key: &Key,
    job_id: &JobId,
    job_kind: &str,
    asset_id: Option<&crate::AssetId>,
    resolved: &Value,
) -> Result<(), Error> {
    connection.execute("INSERT INTO catalog_requests(scope,request_id,method,input_hash,job_id,job_kind,asset_id,resolved_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![key.scope,key.request_id,key.method,key.input.as_slice(),job_id.as_str(),job_kind,asset_id.map(|a| a.as_str()),encode_bounded(resolved)?])?;
    Ok(())
}

/// Terminal results are retained for every alias admitted to this job. A successful publication
/// calls this from its mutation transaction; other endings call it before answering job.read.
pub(crate) fn finish(
    connection: &Connection,
    job_id: &JobId,
    result: &Result<Value, Error>,
) -> Result<(), Error> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM catalog_requests WHERE job_id=?1)",
        [job_id.as_str()],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(());
    }
    let mut refusal = None;
    let (answer, error) = match result {
        Ok(answer) => match encode_bounded(answer) {
            Ok(answer) => (Some(answer), None),
            Err(error) => {
                let encoded = encode_bounded(&StoredError::of(&error))?;
                refusal = Some(error);
                (None, Some(encoded))
            }
        },
        Err(error) => (None, Some(encode_bounded(&StoredError::of(error))?)),
    };
    let different: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM catalog_requests WHERE job_id=?1 AND (result_json IS NOT NULL OR error_json IS NOT NULL) AND (result_json IS NOT ?2 OR error_json IS NOT ?3))",params![job_id.as_str(),answer,error],|row|row.get(0))?;
    if different {
        return Err(Error::conflict(
            "the catalog job already has a different immutable outcome",
        ));
    }
    connection.execute("UPDATE catalog_requests SET result_json=?2,error_json=?3 WHERE job_id=?1 AND result_json IS NULL AND error_json IS NULL",params![job_id.as_str(),answer,error])?;
    match refusal {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredError {
    kind: crate::ErrorKind,
    detail: String,
    data: Option<Box<Value>>,
}
impl StoredError {
    fn of(error: &Error) -> Self {
        Self {
            kind: error.kind,
            detail: error.detail.clone(),
            data: error.data.clone(),
        }
    }
    fn into_error(self) -> Error {
        let mut e = Error::new(self.kind, self.detail);
        e.data = self.data;
        e
    }
}

fn bounded_text(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Option<String>> {
    let value = row.get_ref(index)?;
    if value == rusqlite::types::ValueRef::Null {
        return Ok(None);
    }
    let text = value.as_str()?;
    if text.len() > MAX_RESULT_BYTES {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::other(
                "catalog request result exceeds its byte limit",
            )),
        ));
    }
    Ok(Some(text.to_owned()))
}

fn encode_bounded(value: &impl serde::Serialize) -> Result<String, Error> {
    struct Buffer(Vec<u8>);
    impl std::io::Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_RESULT_BYTES.saturating_sub(self.0.len()) {
                return Err(std::io::Error::other(
                    "catalog request result exceeds its byte limit",
                ));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut buffer = Buffer(Vec::new());
    serde_json::to_writer(&mut buffer, value)
        .map_err(|error| Error::resource_limit(error.to_string()))?;
    String::from_utf8(buffer.0).map_err(|error| Error::internal(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn database() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(SCHEMA).unwrap();
        connection
    }
    fn key() -> Key {
        Key {
            scope: "folder".into(),
            request_id: "request".into(),
            method: "folder.create".into(),
            input: [7; 32],
        }
    }

    #[test]
    fn durable_requests_storage_is_independent_of_asset_lifetime_and_refuses_changed_input() {
        let connection = database();
        let key = key();
        record(&connection, Some(&key), &json!({"outcome":"no-op"})).unwrap();
        assert_eq!(
            lookup(&connection, &key).unwrap().unwrap().answer,
            Some(json!({"outcome":"no-op"}))
        );
        let mut other = key.clone();
        other.input[0] = 9;
        assert_eq!(
            lookup(&connection, &other).err().unwrap().kind,
            crate::ErrorKind::Conflict
        );
        assert!(
            connection
                .execute("DELETE FROM catalog_requests", [])
                .is_err()
        );
        assert!(
            connection
                .execute("UPDATE catalog_requests SET result_json='{}'", [])
                .is_err()
        );
        assert!(
            connection
                .prepare("PRAGMA foreign_key_list(catalog_requests)")
                .unwrap()
                .query([])
                .unwrap()
                .next()
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn durable_requests_serialization_failure_rolls_back_mutation_and_request_record_together() {
        struct Refused;
        impl serde::Serialize for Refused {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("injected serialization refusal"))
            }
        }
        let mut connection = database();
        connection
            .execute_batch("CREATE TABLE changed(value INTEGER)")
            .unwrap();
        let key = key();
        let error = super::super::catalog::write(&mut connection, |tx| {
            tx.execute("INSERT INTO changed VALUES(1)", [])?;
            record(tx, Some(&key), &Refused)
        })
        .unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::ResourceLimit);
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM changed", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(lookup(&connection, &key).unwrap().is_none());
    }

    #[test]
    #[luxforge_test_macros::slow]
    fn durable_requests_result_larger_than_cache_is_retained_and_terminal_identity_is_immutable() {
        let connection = database();
        let key = key();
        let answer = json!({"payload":"a".repeat(9*1024*1024)});
        record(&connection, Some(&key), &answer).unwrap();
        assert_eq!(
            lookup(&connection, &key).unwrap().unwrap().answer,
            Some(answer)
        );
        let other = Key {
            scope: "batch".into(),
            ..key
        };
        let job = JobId::new();
        admit(
            &connection,
            &other,
            &job,
            "batch-settings",
            None,
            &json!({"assets":[]}),
        )
        .unwrap();
        finish(&connection, &job, &Ok(json!({"done":[]}))).unwrap();
        assert_eq!(
            finish(&connection, &job, &Err(Error::cancelled("late cancel")))
                .unwrap_err()
                .kind,
            crate::ErrorKind::Conflict
        );
        assert!(
            connection
                .execute(
                    "UPDATE catalog_requests SET job_id='changed' WHERE scope='batch'",
                    []
                )
                .is_err()
        );
        assert!(connection.execute("UPDATE catalog_requests SET error_json='{}',result_json=NULL WHERE scope='batch'",[]).is_err());
    }
}
