//! The `luxforge-ctl` command line: `status`, `schema [METHOD]`, `call METHOD` and `batch`, each over
//! one connection to a live session. Every method and its parameters come from the session's own
//! `schema.list`, read once per connection as its handshake; nothing here keeps a second list of
//! them.
use super::{
    CONNECTION_LOST, EXIT_FAILURE, JOB_RELEASED, LiveClient, OUTCOME_UNKNOWN, PROTOCOL_ERROR,
    USAGE, exit_status, failure, find_session,
};
use crate::Paths;
use luxforge_core::{
    ApiFailure, Envelope, MAX_EVENT_WAIT_MS, MAX_REQUEST_BYTES, Mutation, MutationRequest,
    RevisionOf,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{
    ffi::OsString,
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
};

/// The actor every mutation this client builds records.
pub const ACTOR: &str = "luxforge-ctl";

/// The longest `job.wait` hold one request asks for, the longest the session allows; the wait
/// continues with another until the job ends.
const WAIT_HOLD_MS: i64 = MAX_EVENT_WAIT_MS;

const HELP: &str = "luxforge-ctl [--catalog PATH] status
luxforge-ctl [--catalog PATH] schema [METHOD]
luxforge-ctl [--catalog PATH] call METHOD [--params JSON | --params @FILE | --params -] [--asset ID | --expected-revision N] [--wait]
luxforge-ctl [--catalog PATH] batch < REQUESTS.jsonl

Operates the catalog the Luxforge desktop has open, through its live session, as one more API
client with edit authority. It never starts Luxforge or another catalog owner.

status                  {session, catalog}: session.state and catalog.info.
schema [METHOD]         schema.list, or that method's entry in its methods.
call METHOD             sends METHOD with the --params object ({} without it) and prints its
                        result. A mutating method's envelope is built from schema.list, with new
                        request identities and the actor luxforge-ctl; params must not carry one.
                        A session_scoped method, such as draft.*, is refused: use batch.
batch                   reads one request per line from standard input, {method, params?, asset?,
                        expected_revision?, wait?, id?}, sends each in order over one connection,
                        and prints one line per request: {id?, result, warning?} or {id?, error,
                        job? | call?}. A failed request does not stop the batch; a lost
                        connection does. Its session lasts until the input ends, so drafts and
                        selections carry from one request to the next.
--catalog PATH          the catalog whose live session to attach to; without it, the one running
                        Luxforge's, from the registry of running live sessions.
--params JSON|@FILE|-   the parameters as inline JSON, a UTF-8 JSON file, or standard input.
--asset ID              for a method whose revision_of is asset: reads asset.state for ID and
                        sends that asset with its current revision. A competing edit in between is
                        a conflict, never retried.
--expected-revision N   for any other method with a revision envelope, such as
                        module.settings.set.
--wait                  when the result names a job, follows it with job.wait over the same
                        connection until it ends, and prints {call, job}. Without --wait, a job
                        that belongs to the clients that requested it (job.read's ownership:
                        clients, such as source.prepare's) stops if it is still running when
                        luxforge-ctl exits, and a warning on standard error says so. Interrupting
                        a --wait stops such a job too; a job with ownership catalog, such as an
                        export, runs on whatever its client does.
--help, --version       print this help or the version, without connecting.

Results are JSON on standard output. Failures are {\"error\": {code, message, job_id?, data?}} on
standard error; a warning is {\"warning\": {code, message, job_id, data}} there. Exit status: 0
success; 1 a failure the session answered, or another failure; 2 usage, a command line or batch
line that cannot be read or a request this client refuses to send; 3 no session to speak to
(no-session, stale-session, invalid-session, unsupported-protocol); 4 outcome-unknown, a mutation
sent whose answer was lost. A batch exits with the highest status of its requests.";

/// What the command line asks for: help or the version, answered without connecting, or a command
/// to run against the live session.
#[derive(Debug, PartialEq)]
enum Parsed {
    Help,
    Version,
    Run(Invocation),
}

#[derive(Debug, PartialEq)]
enum Command {
    Status,
    Schema(Option<String>),
    Call(Call),
    Batch,
}

#[derive(Debug, PartialEq)]
struct Call {
    method: String,
    params: Params,
    target: Target,
    wait: bool,
}

/// Where a call's parameters come from.
#[derive(Debug, PartialEq)]
enum Params {
    Empty,
    Inline(String),
    File(PathBuf),
    Stdin,
}

/// The revision a mutation is sent against: none given, an asset's current one, or a number.
#[derive(Debug, PartialEq)]
enum Target {
    None,
    Asset(String),
    Revision(u64),
}

#[derive(Debug, PartialEq)]
struct Invocation {
    catalog: Option<PathBuf>,
    command: Command,
}

/// One request, from `call`'s command line or one line of a `batch`, with its parameters read.
struct Request {
    method: String,
    params: Map<String, Value>,
    target: Target,
    wait: bool,
}

/// One line of a `batch`'s input.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchLine {
    /// Echoed on the line answering it, so a caller can match them up; read from the line before
    /// it is parsed, so a refused line is answered with it too.
    #[serde(default, rename = "id")]
    _id: Option<serde::de::IgnoredAny>,
    method: String,
    #[serde(default)]
    params: Option<Map<String, Value>>,
    #[serde(default)]
    asset: Option<String>,
    #[serde(default)]
    expected_revision: Option<u64>,
    #[serde(default)]
    wait: bool,
}

/// Which command a request is sent for: it says whether a session-scoped method may be sent and
/// when a job that belongs to its requesters is released.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Call,
    Batch,
}

fn usage(message: impl Into<String>) -> ApiFailure {
    failure(USAGE, message, None)
}

fn validation(message: impl Into<String>) -> ApiFailure {
    failure("validation", message, None)
}

/// Read the arguments, after the program's name. Nothing is read or connected to here.
fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Parsed, ApiFailure> {
    let mut args = args.into_iter();
    let mut catalog = None;
    let mut params = None;
    let mut target = Target::None;
    let mut wait = false;
    let mut words = Vec::new();
    let text = |value: Option<OsString>, flag: &str| {
        value
            .ok_or_else(|| usage(format!("{flag} requires a value")))?
            .into_string()
            .map_err(|_| usage(format!("{flag} requires UTF-8 text")))
    };
    while let Some(argument) = args.next() {
        let flag = argument.to_str();
        match flag {
            Some("--help" | "-h") => return Ok(Parsed::Help),
            Some("--version") => return Ok(Parsed::Version),
            Some("--catalog") => {
                if catalog.is_some() {
                    return Err(usage("--catalog is given twice"));
                }
                catalog = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| usage("--catalog requires a path"))?,
                ));
            }
            Some("--params") => {
                if params.is_some() {
                    return Err(usage("--params is given twice"));
                }
                let value = text(args.next(), "--params")?;
                params = Some(match value.as_str() {
                    "-" => Params::Stdin,
                    _ => match value.strip_prefix('@') {
                        Some(file) => Params::File(file.into()),
                        None => Params::Inline(value),
                    },
                });
            }
            Some("--asset") => {
                if target != Target::None {
                    return Err(usage(
                        "--asset and --expected-revision are given together or twice",
                    ));
                }
                target = Target::Asset(text(args.next(), "--asset")?);
            }
            Some("--expected-revision") => {
                if target != Target::None {
                    return Err(usage(
                        "--asset and --expected-revision are given together or twice",
                    ));
                }
                let value = text(args.next(), "--expected-revision")?;
                target = Target::Revision(value.parse().map_err(|_| {
                    usage("--expected-revision requires a non-negative whole number")
                })?);
            }
            Some("--wait") => {
                if wait {
                    return Err(usage("--wait is given twice"));
                }
                wait = true;
            }
            Some(word) if !word.starts_with('-') => words.push(word.to_owned()),
            _ => {
                return Err(usage(format!(
                    "unknown argument {}; use --help",
                    argument.to_string_lossy()
                )));
            }
        }
    }
    let mut words = words.into_iter();
    let command = match words.next().as_deref() {
        Some("status") => Command::Status,
        Some("schema") => Command::Schema(words.next()),
        Some("call") => Command::Call(Call {
            method: words
                .next()
                .ok_or_else(|| usage("call requires a METHOD"))?,
            params: params.take().unwrap_or(Params::Empty),
            target: std::mem::replace(&mut target, Target::None),
            wait: std::mem::take(&mut wait),
        }),
        Some("batch") => Command::Batch,
        Some(other) => return Err(usage(format!("unknown command {other}; use --help"))),
        None => {
            return Err(usage(
                "a command is required: status, schema, call or batch; use --help",
            ));
        }
    };
    if let Some(extra) = words.next() {
        return Err(usage(format!("unexpected argument {extra}; use --help")));
    }
    if params.is_some() || target != Target::None || wait {
        return Err(usage(
            "--params, --asset, --expected-revision and --wait apply only to call",
        ));
    }
    Ok(Parsed::Run(Invocation { catalog, command }))
}

/// Read a call's parameters, a JSON object, from where the command line says: at most a request's
/// length, since a longer one could never be sent.
fn read_params(params: &Params, stdin: &mut dyn Read) -> Result<Map<String, Value>, ApiFailure> {
    let bounded = |reader: &mut dyn Read, what: &str| -> Result<String, ApiFailure> {
        let mut text = String::new();
        reader
            .take(MAX_REQUEST_BYTES as u64 + 1)
            .read_to_string(&mut text)
            .map_err(|error| usage(format!("cannot read the parameters from {what}: {error}")))?;
        if text.len() > MAX_REQUEST_BYTES {
            return Err(usage(format!(
                "the parameters in {what} are longer than a request's {MAX_REQUEST_BYTES} bytes"
            )));
        }
        Ok(text)
    };
    let text = match params {
        Params::Empty => return Ok(Map::new()),
        Params::Inline(text) => text.clone(),
        Params::Stdin => bounded(stdin, "standard input")?,
        Params::File(path) => {
            let mut file = std::fs::File::open(path).map_err(|error| {
                usage(format!("cannot read --params @{}: {error}", path.display()))
            })?;
            bounded(&mut file, &path.display().to_string())?
        }
    };
    match serde_json::from_str(&text) {
        Ok(Value::Object(object)) => Ok(object),
        Ok(_) => Err(validation("params must be a JSON object")),
        Err(error) => Err(usage(format!("--params is not JSON: {error}"))),
    }
}

/// How a method's request is built, from its `schema.list` entry: the envelope it carries and
/// where that envelope's revision comes from, and whether its effect lives only in the caller's
/// session.
struct Shape {
    envelope: Build,
    session_scoped: bool,
}

/// The envelope a request is sent with.
#[derive(Clone, Copy, PartialEq)]
enum Build {
    /// None: the method reads, or changes only the caller's session.
    None,
    /// `{request_id, actor}`.
    Request,
    /// `{expected_revision, request_id, actor}` at the current revision of the asset `--asset`
    /// names: the method's `revision_of` is `asset`.
    AssetRevision,
    /// `{expected_revision, request_id, actor}` at the revision `--expected-revision` gives: any
    /// other `revision_of`.
    GivenRevision,
}

fn shape(method: &str, entry: &Value) -> Result<Shape, ApiFailure> {
    let unreadable = |what: String| {
        failure(
            PROTOCOL_ERROR,
            format!("{method}'s schema entry {what}, which this luxforge-ctl cannot build"),
            None,
        )
    };
    let named = entry
        .get("mutation")
        .map(|name| name.as_str().unwrap_or("?"));
    let envelope = Envelope::named(named)
        .ok_or_else(|| unreadable(format!("names the envelope {}", entry["mutation"])))?;
    let revision_of = match entry.get("revision_of") {
        None => None,
        Some(of) => Some(
            RevisionOf::deserialize(of)
                .map_err(|_| unreadable(format!("names the revision of {of}")))?,
        ),
    };
    let envelope = match (envelope, revision_of) {
        (Envelope::None, None) => Build::None,
        (Envelope::Request, None) => Build::Request,
        (Envelope::Revision, Some(RevisionOf::Asset)) => Build::AssetRevision,
        (Envelope::Revision, Some(_)) => Build::GivenRevision,
        _ => {
            return Err(unreadable(
                "pairs its envelope and revision_of differently".into(),
            ));
        }
    };
    Ok(Shape {
        envelope,
        session_scoped: entry.get("session_scoped") == Some(&Value::Bool(true)),
    })
}

/// A new envelope with new request identities and [`ACTOR`]: a revision envelope at
/// `expected_revision` when there is one, else a request envelope.
fn mutation(expected_revision: Option<u64>) -> Result<Value, ApiFailure> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let actor = ACTOR.to_owned();
    let built = match expected_revision {
        Some(expected_revision) => serde_json::to_value(Mutation {
            expected_revision,
            request_id,
            actor,
        }),
        None => serde_json::to_value(MutationRequest { request_id, actor }),
    };
    built.map_err(|error| failure(super::INTERNAL, error.to_string(), None))
}

/// Build the parameters a request sends: the caller's, with the envelope `schema.list` declares for
/// the method. A revision of an asset is read from `asset.state` over the same connection just
/// before the request.
fn prepare(
    client: &mut LiveClient,
    request: &Request,
    shape: &Shape,
) -> Result<Map<String, Value>, ApiFailure> {
    let method = &request.method;
    let mut params = request.params.clone();
    let refuse_target = |envelope: &str| match &request.target {
        Target::None => Ok(()),
        Target::Asset(_) => Err(validation(format!(
            "--asset applies only to a method whose revision_of is asset; {method} {envelope}"
        ))),
        Target::Revision(_) => Err(validation(format!(
            "--expected-revision applies only to a method with a revision envelope whose \
             revision_of is not asset; {method} {envelope}"
        ))),
    };
    if shape.envelope != Build::None && params.contains_key("mutation") {
        return Err(validation(format!(
            "luxforge-ctl builds {method}'s mutation envelope; remove mutation from --params"
        )));
    }
    let envelope = match shape.envelope {
        Build::None => {
            refuse_target("does not mutate")?;
            return Ok(params);
        }
        Build::Request => {
            refuse_target("has no revision")?;
            mutation(None)?
        }
        Build::AssetRevision => {
            let Target::Asset(asset) = &request.target else {
                return Err(validation(format!(
                    "{method} changes an asset: name it with --asset ID"
                )));
            };
            if params.contains_key("asset_id") {
                return Err(validation(format!(
                    "name {method}'s asset with --asset only; remove asset_id from --params"
                )));
            }
            let state = client.call("asset.state", json!({"asset_id": asset}))?;
            let revision = state["revision"]
                .as_u64()
                .ok_or_else(|| failure(PROTOCOL_ERROR, "asset.state answered no revision", None))?;
            params.insert("asset_id".into(), asset.as_str().into());
            mutation(Some(revision))?
        }
        Build::GivenRevision => {
            let Target::Revision(revision) = request.target else {
                return Err(validation(format!(
                    "{method} has a revision but no asset: give it with --expected-revision N"
                )));
            };
            mutation(Some(revision))?
        }
    };
    params.insert("mutation".into(), envelope);
    Ok(params)
}

/// What a request came to: a result, with a warning beside it, or a failure, with anything the
/// failure should carry beside it.
enum Outcome {
    Printed(Value, Option<ApiFailure>),
    Failed(ApiFailure, Option<(&'static str, Value)>),
}

impl Outcome {
    fn status(&self) -> i32 {
        match self {
            Self::Printed(..) => 0,
            Self::Failed(error, _) => exit_status(&error.code),
        }
    }

    /// Whether the connection can carry no further request after this one.
    fn ends_connection(&self) -> bool {
        matches!(self, Self::Failed(error, _)
            if error.code == CONNECTION_LOST || error.code == OUTCOME_UNKNOWN)
    }
}

/// Follow `job_id` with `job.wait` until its status is neither queued nor running, holding each
/// wait at most [`WAIT_HOLD_MS`], and answer its last record.
fn wait_for_job(client: &mut LiveClient, job_id: &str) -> Result<Value, ApiFailure> {
    let mut after: Option<Value> = None;
    loop {
        let mut params = json!({"job_id": job_id, "timeout_ms": WAIT_HOLD_MS});
        if let Some(after) = after.take() {
            params["after"] = after;
        }
        let mut answer = client.call("job.wait", params).map_err(|mut error| {
            if error.code == CONNECTION_LOST {
                error.job_id = Some(job_id.into());
                error.message.push_str(
                    "; a job with ownership clients stops if no other client wants it, and one \
                     with ownership catalog runs on",
                );
            }
            error
        })?;
        let job = answer["job"].take();
        if !matches!(job["status"].as_str(), Some("queued" | "running")) {
            return Ok(job);
        }
        after = Some(answer["change"].take());
    }
}

/// The warning for a job a request started and does not wait for, when `job.read` says it belongs
/// to the clients that requested it: it stops, if still running, once this client's connection
/// closes, and no later client can read it. Nothing when the job belongs to the catalog, or when
/// it cannot be read.
fn released(client: &mut LiveClient, job_id: &str, mode: Mode) -> Option<ApiFailure> {
    let record = client.call("job.read", json!({"job_id": job_id})).ok()?;
    if record["ownership"] != "clients" {
        return None;
    }
    let (when, instead) = match mode {
        Mode::Call => ("luxforge-ctl exits", "pass --wait"),
        Mode::Batch => ("this batch ends", "give the request wait: true"),
    };
    Some(ApiFailure {
        code: JOB_RELEASED.into(),
        message: format!(
            "job {job_id} belongs to the clients that requested it: if it is still running when \
             {when}, it stops, and no later client can read it; {instead} to follow it to its end"
        ),
        job_id: Some(job_id.into()),
        data: Some(json!({
            "kind": record["kind"],
            "status": record["status"],
            "ownership": record["ownership"],
        })),
    })
}

/// Send one request: its method checked against `schema`, its envelope built, and with `wait` the
/// job its result names followed to its end.
fn send(client: &mut LiveClient, schema: &Value, request: &Request, mode: Mode) -> Outcome {
    let method = &request.method;
    let Some(entry) = schema["methods"].get(method) else {
        return Outcome::Failed(
            validation(format!(
                "unknown method {method}; luxforge-ctl schema lists them"
            )),
            None,
        );
    };
    let shape = match shape(method, entry) {
        Ok(shape) => shape,
        Err(error) => return Outcome::Failed(error, None),
    };
    if shape.session_scoped && mode == Mode::Call {
        return Outcome::Failed(
            usage(format!(
                "{method} is session_scoped: it acts only on this connection's session, which \
                 ends when luxforge-ctl exits; send it with the requests that depend on it \
                 through luxforge-ctl batch"
            )),
            None,
        );
    }
    let params = match prepare(client, request, &shape) {
        Ok(params) => params,
        Err(error) => return Outcome::Failed(error, None),
    };
    let request_id = params
        .get("mutation")
        .map(|mutation| mutation["request_id"].clone());
    let result = match client.call(method, Value::Object(params)) {
        Ok(result) => result,
        Err(mut error) => {
            if error.code == CONNECTION_LOST
                && let Some(request_id) = request_id
            {
                error.code = OUTCOME_UNKNOWN.into();
                error.message.push_str(
                    "; the mutation may or may not have been applied, and was not sent again",
                );
                error.data = Some(json!({"method": method, "request_id": request_id}));
            }
            return Outcome::Failed(error, None);
        }
    };
    let Some(job_id) = result["job_id"].as_str().map(str::to_owned) else {
        return Outcome::Printed(result, None);
    };
    if !request.wait {
        let warning = released(client, &job_id, mode);
        return Outcome::Printed(result, warning);
    }
    match wait_for_job(client, &job_id) {
        Ok(job) if job["status"] == "ready" => {
            Outcome::Printed(json!({"call": result, "job": job}), None)
        }
        Ok(job) => {
            let status = job["status"].as_str().unwrap_or("ended").to_owned();
            let error = &job["error"];
            let failure = ApiFailure {
                code: error["code"].as_str().unwrap_or(&status).into(),
                message: error["message"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("job {job_id} is {status}")),
                job_id: Some(job_id),
                data: error.get("data").cloned(),
            };
            Outcome::Failed(failure, Some(("job", job)))
        }
        Err(error) => Outcome::Failed(error, Some(("call", result))),
    }
}

/// Connect to the session the invocation names, shaking hands: the client and the session's
/// `schema.list`.
fn connect(
    invocation: &Invocation,
    paths: Option<&Paths>,
) -> Result<(LiveClient, Value), ApiFailure> {
    let session = find_session(invocation.catalog.as_deref(), paths)?;
    LiveClient::connect(&session)
}

fn execute(
    invocation: &Invocation,
    stdin: &mut dyn Read,
    paths: Option<&Paths>,
) -> Result<Outcome, ApiFailure> {
    // Parameters are read and checked before anything connects.
    let request = match &invocation.command {
        Command::Call(call) => Some(Request {
            method: call.method.clone(),
            params: read_params(&call.params, stdin)?,
            target: match &call.target {
                Target::None => Target::None,
                Target::Asset(asset) => Target::Asset(asset.clone()),
                Target::Revision(revision) => Target::Revision(*revision),
            },
            wait: call.wait,
        }),
        Command::Status | Command::Schema(_) | Command::Batch => None,
    };
    let (mut client, mut schema) = connect(invocation, paths)?;
    Ok(match (&invocation.command, request) {
        (_, Some(request)) => send(&mut client, &schema, &request, Mode::Call),
        (Command::Status, _) => {
            let session = client.call("session.state", json!({}))?;
            let catalog = client.call("catalog.info", json!({}))?;
            Outcome::Printed(json!({"session": session, "catalog": catalog}), None)
        }
        (Command::Schema(None), _) => Outcome::Printed(schema, None),
        (Command::Schema(Some(method)), _) => match schema["methods"].get_mut(method) {
            Some(entry) => Outcome::Printed(entry.take(), None),
            None => Outcome::Failed(validation(format!("unknown method {method}")), None),
        },
        (Command::Call(_) | Command::Batch, None) => {
            unreachable!("a call has its request, and a batch runs elsewhere")
        }
    })
}

/// One line of standard input, at most [`MAX_REQUEST_BYTES`] long without its newline.
enum Line {
    End,
    Text(Vec<u8>),
    TooLong,
}

/// Read the next line of `reader` within [`MAX_REQUEST_BYTES`]; the rest of a longer line is read
/// and dropped, so memory stays bounded and the next line is read whole.
fn next_line(reader: &mut dyn BufRead) -> std::io::Result<Line> {
    let mut line = Vec::new();
    let read = (&mut *reader)
        .take(MAX_REQUEST_BYTES as u64 + 1)
        .read_until(b'\n', &mut line)?;
    if read == 0 {
        return Ok(Line::End);
    }
    if line.last() == Some(&b'\n') {
        line.pop();
        return Ok(Line::Text(line));
    }
    if line.len() <= MAX_REQUEST_BYTES {
        return Ok(Line::Text(line));
    }
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            break;
        }
        if let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
            reader.consume(end + 1);
            break;
        }
        let length = buffer.len();
        reader.consume(length);
    }
    Ok(Line::TooLong)
}

/// Read one batch line as a request: the line's `id`, if it has one, and the request or why it
/// cannot be sent.
fn batch_request(line: &[u8]) -> (Option<Value>, Result<Request, ApiFailure>) {
    let value: Value = match serde_json::from_slice(line) {
        Ok(value) => value,
        Err(error) => return (None, Err(usage(format!("the line is not JSON: {error}")))),
    };
    let id = value.get("id").cloned();
    let parsed = match BatchLine::deserialize(value) {
        Ok(parsed) => parsed,
        Err(error) => {
            return (
                id,
                Err(usage(format!("the line is not a batch request: {error}"))),
            );
        }
    };
    let target = match (parsed.asset, parsed.expected_revision) {
        (Some(_), Some(_)) => {
            return (id, Err(usage("give asset or expected_revision, not both")));
        }
        (Some(asset), None) => Target::Asset(asset),
        (None, Some(revision)) => Target::Revision(revision),
        (None, None) => Target::None,
    };
    let request = Request {
        method: parsed.method,
        params: parsed.params.unwrap_or_default(),
        target,
        wait: parsed.wait,
    };
    (id, Ok(request))
}

/// Run a batch: one connection and one handshake, then each line of `stdin` sent in order and
/// answered with one line on `stdout`. A failed request does not stop it; a lost connection does,
/// since nothing more can be sent. Answers the highest exit status of its requests.
fn batch(
    invocation: &Invocation,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    paths: Option<&Paths>,
) -> i32 {
    let (mut client, schema) = match connect(invocation, paths) {
        Ok(connected) => connected,
        Err(error) => return report(stdout, stderr, Outcome::Failed(error, None)),
    };
    let mut input = BufReader::new(stdin);
    let mut worst = 0;
    loop {
        let (id, outcome) = match next_line(&mut input) {
            Ok(Line::End) => break,
            Ok(Line::Text(line)) if line.iter().all(u8::is_ascii_whitespace) => continue,
            Ok(Line::Text(line)) => match batch_request(&line) {
                (id, Ok(request)) => (id, send(&mut client, &schema, &request, Mode::Batch)),
                (id, Err(error)) => (id, Outcome::Failed(error, None)),
            },
            Ok(Line::TooLong) => (
                None,
                Outcome::Failed(
                    usage(format!(
                        "the line is longer than a request's {MAX_REQUEST_BYTES} bytes"
                    )),
                    None,
                ),
            ),
            Err(error) => (
                None,
                Outcome::Failed(usage(format!("cannot read standard input: {error}")), None),
            ),
        };
        worst = worst.max(outcome.status());
        let stop = outcome.ends_connection();
        let mut line = Map::new();
        if let Some(id) = id {
            line.insert("id".into(), id);
        }
        match outcome {
            Outcome::Printed(result, warning) => {
                line.insert("result".into(), result);
                if let Some(warning) = warning {
                    line.insert("warning".into(), json!(warning));
                }
            }
            Outcome::Failed(error, beside) => {
                line.insert("error".into(), json!(error));
                if let Some((name, value)) = beside {
                    line.insert(name.into(), value);
                }
            }
        }
        if writeln!(stdout, "{}", Value::Object(line)).is_err() {
            return worst.max(EXIT_FAILURE);
        }
        if stop {
            break;
        }
    }
    worst
}

/// Print what a single command came to: its result on `stdout`, with a warning beside it on
/// `stderr`, or its failure on `stderr`. Answers the exit status.
fn report(stdout: &mut dyn Write, stderr: &mut dyn Write, outcome: Outcome) -> i32 {
    let status = outcome.status();
    match outcome {
        Outcome::Printed(result, warning) => {
            if let Some(warning) = warning {
                let _ = writeln!(stderr, "{}", json!({"warning": warning}));
            }
            match writeln!(stdout, "{result}") {
                Ok(()) => status,
                Err(_) => EXIT_FAILURE,
            }
        }
        Outcome::Failed(error, beside) => {
            let mut report = json!({"error": error});
            if let Some((name, value)) = beside {
                report[name] = value;
            }
            let _ = writeln!(stderr, "{report}");
            status
        }
    }
}

/// Run `luxforge-ctl` with `args`, after the program's name, and answer its exit status. Results go
/// to `stdout` as JSON lines, failures and warnings to `stderr`; `paths` is where the desktop's
/// configuration is, for the registry of running live sessions.
pub fn run(
    args: impl IntoIterator<Item = OsString>,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    paths: Option<&Paths>,
) -> i32 {
    let invocation = match parse(args) {
        Ok(Parsed::Help) => {
            let _ = writeln!(stdout, "{HELP}");
            return 0;
        }
        Ok(Parsed::Version) => {
            let _ = writeln!(stdout, "luxforge-ctl {}", env!("CARGO_PKG_VERSION"));
            return 0;
        }
        Ok(Parsed::Run(invocation)) => invocation,
        Err(error) => return report(stdout, stderr, Outcome::Failed(error, None)),
    };
    if invocation.command == Command::Batch {
        return batch(&invocation, stdin, stdout, stderr, paths);
    }
    let outcome =
        execute(&invocation, stdin, paths).unwrap_or_else(|error| Outcome::Failed(error, None));
    report(stdout, stderr, outcome)
}
