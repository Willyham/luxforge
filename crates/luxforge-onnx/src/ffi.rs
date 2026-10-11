//! The two ONNX Runtime calls `ort` does not wrap, each the only `unsafe` in the crate: the linked
//! library's version string, and the record of which execution provider each part of a session's
//! graph was assigned to (C API 24, ONNX Runtime 1.24 and later), which a placement is read from.
use ort::{AsPointer, session::Session, sys};
use std::ffi::{CStr, c_char};

/// The session option that has ONNX Runtime record its execution-provider assignment.
pub(crate) const RECORD_ASSIGNMENT: &str = "session.record_ep_graph_assignment_info";

/// The linked ONNX Runtime's version, `1.28.0`; [`crate::PINNED_VERSION`] if the library does not
/// say.
#[allow(unsafe_code)]
pub(crate) fn version() -> String {
    // SAFETY: `OrtGetApiBase` is the statically linked library's entry point; it takes nothing and
    // answers a pointer to a static table, which is checked for null before it is read.
    let base = unsafe { sys::OrtGetApiBase() };
    if base.is_null() {
        return crate::PINNED_VERSION.into();
    }
    // SAFETY: `base` is the non-null static table above; `GetVersionString` answers a static,
    // NUL-terminated UTF-8 string the caller must not free, checked for null before it is read.
    let version = unsafe { ((*base).GetVersionString)() };
    if version.is_null() {
        return crate::PINNED_VERSION.into();
    }
    // SAFETY: `version` is the non-null static NUL-terminated string above, never freed.
    unsafe { CStr::from_ptr(version) }
        .to_string_lossy()
        .into_owned()
}

/// One part of a session's graph and the execution provider it was assigned to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Assigned {
    pub provider: String,
    pub nodes: usize,
}

/// The parts of `session`'s graph, each with its execution provider and node count, as ONNX Runtime
/// recorded them when the session was created with [`RECORD_ASSIGNMENT`] on.
#[allow(unsafe_code)]
pub(crate) fn assignment(session: &Session) -> Result<Vec<Assigned>, String> {
    let api = ort::api();
    let mut subgraphs: *const *const sys::OrtEpAssignedSubgraph = std::ptr::null();
    let mut count = 0_usize;
    // SAFETY: `session.ptr()` is the live session this borrow keeps alive; the call writes a
    // pointer to an array the session owns (valid while it lives, never freed by the caller) and
    // its length into the two locals.
    status(unsafe {
        (api.Session_GetEpGraphAssignmentInfo)(session.ptr(), &mut subgraphs, &mut count)
    })?;
    if count == 0 {
        return Ok(Vec::new());
    }
    if subgraphs.is_null() {
        return Err("ONNX Runtime answered a null assignment".into());
    }
    // SAFETY: `subgraphs` is the non-null array of `count` subgraph pointers the session owns,
    // valid while `session` is borrowed.
    let subgraphs = unsafe { std::slice::from_raw_parts(subgraphs, count) };
    let mut assigned = Vec::with_capacity(count);
    for &subgraph in subgraphs {
        let mut name: *const c_char = std::ptr::null();
        // SAFETY: `subgraph` is one element of the session-owned array above; the call writes a
        // pointer to a NUL-terminated name the subgraph owns.
        status(unsafe { (api.EpAssignedSubgraph_GetEpName)(subgraph, &mut name) })?;
        if name.is_null() {
            return Err("ONNX Runtime answered a null provider name".into());
        }
        // SAFETY: `name` is the non-null NUL-terminated string the subgraph owns, read before the
        // session can be dropped.
        let provider = unsafe { CStr::from_ptr(name) }
            .to_string_lossy()
            .into_owned();
        let mut nodes: *const *const sys::OrtEpAssignedNode = std::ptr::null();
        let mut node_count = 0_usize;
        // SAFETY: as above; the call writes a pointer to the subgraph's own node array and its
        // length, which only the length of is read.
        status(unsafe {
            (api.EpAssignedSubgraph_GetNodes)(subgraph, &mut nodes, &mut node_count)
        })?;
        assigned.push(Assigned {
            provider,
            nodes: node_count,
        });
    }
    Ok(assigned)
}

/// `Ok` for a null status; otherwise its message, the status released.
#[allow(unsafe_code)]
fn status(status: sys::OrtStatusPtr) -> Result<(), String> {
    let pointer = status.0;
    if pointer.is_null() {
        return Ok(());
    }
    let api = ort::api();
    // SAFETY: `pointer` is the non-null status the call above answered, owned by the caller; its
    // message is copied before the status is released, exactly once.
    let message = unsafe {
        let text = (api.GetErrorMessage)(pointer);
        let message = if text.is_null() {
            "ONNX Runtime failed without a message".to_owned()
        } else {
            CStr::from_ptr(text).to_string_lossy().into_owned()
        };
        (api.ReleaseStatus)(pointer);
        message
    };
    Err(message)
}
