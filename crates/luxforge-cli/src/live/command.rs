//! The `luxforge-ctl` command line: `status`, `schema [METHOD]` and `call METHOD`, over one
//! connection to the selected catalog's live session. Every method and its parameters come from
//! the session's own `schema.list`; nothing here keeps a second list of them.
use super::{
    CONNECTION_LOST, LiveClient, LiveSession, OUTCOME_UNKNOWN, REQUEST_LIMIT, USAGE, failure,
    target_catalog,
};
use crate::Paths;
use luxforge_core::ApiFailure;
use serde_json::{Map, Value, json};
use std::{
    ffi::OsString,
    io::{Read, Write},
    path::PathBuf,
};

/// The actor every mutation this client builds records.
pub const ACTOR: &str = "luxforge-ctl";

/// The longest `job.wait` hold one request asks for; the wait continues with another until the job
/// ends.
const WAIT_HOLD_MS: u64 = 30_000;

const HELP: &str = "luxforge-ctl [--catalog PATH] status
luxforge-ctl [--catalog PATH] schema [METHOD]
luxforge-ctl [--catalog PATH] call METHOD [--params JSON | --params @FILE | --params -] [--asset ID | --expected-revision N] [--wait]

Operates the catalog the Luxforge desktop has open, through its live session, as one more API
client with edit authority. It never starts Luxforge or another catalog owner.

status                  {session, catalog}: session.state and catalog.info.
schema [METHOD]         schema.list, or that method's entry in its methods.
call METHOD             sends METHOD with the --params object ({} without it) and prints its
                        result. A mutating method's envelope is built from schema.list, with new
                        request identities and the actor luxforge-ctl; params must not carry one.
--catalog PATH          the catalog to attach to; without it, the catalog the desktop opens on an
                        ordinary launch: the stored catalog location, else the default.
--params JSON|@FILE|-   the parameters as inline JSON, a UTF-8 JSON file, or standard input.
--asset ID              for a method that changes an asset's revision: reads asset.state for ID
                        and sends that asset with its current revision. A competing edit in between
                        is a conflict, never retried.
--expected-revision N   for a method with a revision but no asset, such as module.settings.set.
--wait                  when the result names a job, follows it with job.wait over the same
                        connection until it ends, and prints {call, job}. Interrupting stops
                        waiting; it does not cancel the job.
--help, --version       print this help or the version, without connecting.

Results are JSON on standard output. Failures are {\"error\": {code, message, data?}} on standard
error, with exit status 1.";

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
        Some(other) => return Err(usage(format!("unknown command {other}; use --help"))),
        None => {
            return Err(usage(
                "a command is required: status, schema or call; use --help",
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
            .take(REQUEST_LIMIT as u64 + 1)
            .read_to_string(&mut text)
            .map_err(|error| usage(format!("cannot read the parameters from {what}: {error}")))?;
        if text.len() > REQUEST_LIMIT {
            return Err(usage(format!(
                "the parameters in {what} are longer than a request's {REQUEST_LIMIT} bytes"
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

/// How a method's mutation envelope is built, from its `schema.list` entry.
#[derive(Debug, PartialEq)]
enum Envelope {
    /// The method reads, or changes only the caller's own session; parameters pass unchanged.
    None,
    /// `{request_id, actor}`.
    Request,
    /// `{expected_revision, request_id, actor}`, the revision read for `--asset` when the method
    /// requires an `asset_id`, else `--expected-revision`.
    Revision { asset: bool },
}

fn envelope(method: &str, entry: &Value) -> Result<Envelope, ApiFailure> {
    let requires = |field: &str| {
        entry["required"]
            .as_array()
            .is_some_and(|required| required.iter().any(|name| name == field))
    };
    match entry.get("mutation").and_then(Value::as_str) {
        None => Ok(Envelope::None),
        Some("request") => Ok(Envelope::Request),
        Some("revision") => Ok(Envelope::Revision {
            asset: requires("asset_id"),
        }),
        Some(other) => Err(failure(
            "protocol",
            format!("{method} has the envelope {other}, which this luxforge-ctl cannot build"),
            None,
        )),
    }
}

fn mutation(expected_revision: Option<u64>) -> Value {
    let mut mutation = json!({
        "request_id": uuid::Uuid::new_v4().to_string(),
        "actor": ACTOR,
    });
    if let Some(revision) = expected_revision {
        mutation["expected_revision"] = revision.into();
    }
    mutation
}

/// Build the parameters `call` sends: the caller's, with the envelope `schema.list` declares for the
/// method. `--asset` reads the asset's revision over the same connection just before the call.
fn prepare(
    client: &mut LiveClient,
    call: &Call,
    envelope: &Envelope,
    mut params: Map<String, Value>,
) -> Result<Map<String, Value>, ApiFailure> {
    let method = &call.method;
    let refuse_target = |envelope: &str| match &call.target {
        Target::None => Ok(()),
        Target::Asset(_) => Err(validation(format!(
            "--asset applies only to a method that changes an asset's revision; {method} \
             {envelope}"
        ))),
        Target::Revision(_) => Err(validation(format!(
            "--expected-revision applies only to a method with a revision and no asset; \
             {method} {envelope}"
        ))),
    };
    if *envelope != Envelope::None && params.contains_key("mutation") {
        return Err(validation(format!(
            "luxforge-ctl builds {method}'s mutation envelope; remove mutation from --params"
        )));
    }
    match envelope {
        Envelope::None => refuse_target("does not mutate")?,
        Envelope::Request => {
            refuse_target("has no revision")?;
            params.insert("mutation".into(), mutation(None));
        }
        Envelope::Revision { asset: true } => {
            let Target::Asset(asset) = &call.target else {
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
                .ok_or_else(|| failure("protocol", "asset.state answered no revision", None))?;
            params.insert("asset_id".into(), asset.as_str().into());
            params.insert("mutation".into(), mutation(Some(revision)));
        }
        Envelope::Revision { asset: false } => {
            let Target::Revision(revision) = call.target else {
                return Err(validation(format!(
                    "{method} has a revision but no asset: give it with --expected-revision N"
                )));
            };
            params.insert("mutation".into(), mutation(Some(revision)));
        }
    }
    Ok(params)
}

/// What a command printed: a result for standard output, or a failure for standard error, with
/// anything the failure should carry beside it.
enum Outcome {
    Printed(Value),
    Failed(ApiFailure, Option<(String, Value)>),
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
                error.message.push_str("; the job was not cancelled");
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

/// Send a `call`: its method checked against `schema.list`, its envelope built, and with `--wait` the
/// job its result names followed to its end.
fn send(client: &mut LiveClient, call: &Call, params: Map<String, Value>) -> Outcome {
    let schema = match client.call("schema.list", json!({})) {
        Ok(schema) => schema,
        Err(error) => return Outcome::Failed(error, None),
    };
    let Some(entry) = schema["methods"].get(&call.method) else {
        return Outcome::Failed(
            validation(format!(
                "unknown method {}; luxforge-ctl schema lists them",
                call.method
            )),
            None,
        );
    };
    let envelope = match envelope(&call.method, entry) {
        Ok(envelope) => envelope,
        Err(error) => return Outcome::Failed(error, None),
    };
    let params = match prepare(client, call, &envelope, params) {
        Ok(params) => params,
        Err(error) => return Outcome::Failed(error, None),
    };
    let request_id = params
        .get("mutation")
        .map(|mutation| mutation["request_id"].clone());
    let result = match client.call(&call.method, Value::Object(params)) {
        Ok(result) => result,
        Err(mut error) => {
            if error.code == CONNECTION_LOST
                && let Some(request_id) = request_id
            {
                error.code = OUTCOME_UNKNOWN.into();
                error.message.push_str(
                    "; the mutation may or may not have been applied, and was not sent again",
                );
                error.data = Some(json!({"method": call.method, "request_id": request_id}));
            }
            return Outcome::Failed(error, None);
        }
    };
    let job_id = result["job_id"].as_str().map(str::to_owned);
    let (true, Some(job_id)) = (call.wait, job_id) else {
        return Outcome::Printed(result);
    };
    match wait_for_job(client, &job_id) {
        Ok(job) if job["status"] == "ready" => {
            Outcome::Printed(json!({"call": result, "job": job}))
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
            Outcome::Failed(failure, Some(("job".into(), job)))
        }
        Err(error) => Outcome::Failed(error, Some(("call".into(), result))),
    }
}

fn execute(
    invocation: &Invocation,
    stdin: &mut dyn Read,
    paths: Option<&Paths>,
) -> Result<Outcome, ApiFailure> {
    // Parameters are read and checked before anything connects.
    let params = match &invocation.command {
        Command::Call(call) => read_params(&call.params, stdin)?,
        Command::Status | Command::Schema(_) => Map::new(),
    };
    let catalog = target_catalog(invocation.catalog.clone(), paths)?;
    let session = LiveSession::discover(&catalog)?;
    let mut client = LiveClient::connect(&session)?;
    Ok(match &invocation.command {
        Command::Status => {
            let session = client.call("session.state", json!({}))?;
            let catalog = client.call("catalog.info", json!({}))?;
            Outcome::Printed(json!({"session": session, "catalog": catalog}))
        }
        Command::Schema(None) => Outcome::Printed(client.call("schema.list", json!({}))?),
        Command::Schema(Some(method)) => {
            let mut schema = client.call("schema.list", json!({}))?;
            match schema["methods"].get_mut(method) {
                Some(entry) => Outcome::Printed(entry.take()),
                None => Outcome::Failed(validation(format!("unknown method {method}")), None),
            }
        }
        Command::Call(call) => send(&mut client, call, params),
    })
}

/// Run `luxforge-ctl` with `args`, after the program's name, and answer its exit status. Results go
/// to `stdout` as one JSON line, failures to `stderr` as `{"error": …}`; `paths` is where the
/// desktop's configuration is, for the catalog an ordinary launch opens.
pub fn run(
    args: impl IntoIterator<Item = OsString>,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    paths: Option<&Paths>,
) -> i32 {
    let outcome = match parse(args) {
        Ok(Parsed::Help) => {
            let _ = writeln!(stdout, "{HELP}");
            return 0;
        }
        Ok(Parsed::Version) => {
            let _ = writeln!(stdout, "luxforge-ctl {}", env!("CARGO_PKG_VERSION"));
            return 0;
        }
        Ok(Parsed::Run(invocation)) => {
            execute(&invocation, stdin, paths).unwrap_or_else(|error| Outcome::Failed(error, None))
        }
        Err(error) => Outcome::Failed(error, None),
    };
    match outcome {
        Outcome::Printed(result) => match writeln!(stdout, "{result}") {
            Ok(()) => 0,
            Err(_) => 1,
        },
        Outcome::Failed(error, beside) => {
            let mut report = json!({"error": error});
            if let Some((name, value)) = beside {
                report[name] = value;
            }
            let _ = writeln!(stderr, "{report}");
            1
        }
    }
}
