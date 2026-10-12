//! klein installed exactly as `luxforge.ai` declares it, over a loopback HTTP server: every pinned
//! file, the project's removal conditioning release asset included, is fetched through the real
//! HTTP transport, hashed whole against its pin, checked for its format by the module and
//! published by the host's install job; installing runs this runtime's `prepare` (the Metal
//! kernels' warm-up). Then both `local-runtime` slots load on this runtime: removal's with the
//! generator, Replace's with the text encoder and the generator as its companion.
//!
//! The declared URLs are the publishers' and the planned release's, which this test never
//! contacts: its transport answers each declared URL from the loopback server, which serves the
//! files of `LUXFORGE_GENERATIVE_MODELS` (default: the main checkout's
//! `private/models/generative/`). Ignored, since it needs those files (about 5.2 GB, copied into
//! a temporary data root): `cargo test -p luxforge-diffusion --release --test loopback_install --
//! --ignored --nocapture`.
use luxforge_core::{
    ApiRequest, ClientAuthority, ClientId, Error, HostConfig, ModuleRegistry, OwnerHandle,
    capabilities::{
        endpoint::{EndpointClass, parse_endpoint},
        secrets::MemorySecretStore,
        transport::{SendOptions, Transport, TransportRequest, TransportResponse},
    },
};
use luxforge_testbase::{TestServer, respond};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

const AI: &str = "luxforge.ai";
const GENERATOR: &str = "generative-klein-4b";
const ENCODER: &str = "generative-klein-text-encoder";
const CONDITIONING_ASSET: &str = "klein-4b-removal-conditioning-ba26d15a.lfsdcond";

fn models() -> PathBuf {
    if let Some(dir) = std::env::var_os("LUXFORGE_GENERATIVE_MODELS") {
        return dir.into();
    }
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("git");
    PathBuf::from(String::from_utf8(out.stdout).unwrap().trim())
        .parent()
        .unwrap()
        .join("private/models/generative")
}

/// The local file each declared download is: the publishers' file names as the models directory
/// holds them, and the release asset by its name.
fn local_file(models: &Path, url: &str) -> PathBuf {
    let name = url.rsplit('/').next().unwrap();
    let local = match name {
        "flux-2-klein-4b-Q4_0.gguf" | "Qwen3-4B-Q4_K_M.gguf" => name,
        "diffusion_pytorch_model.safetensors" => "flux2-klein-4b-vae.safetensors",
        "tokenizer.json" => "qwen3-tokenizer.json",
        CONDITIONING_ASSET => CONDITIONING_ASSET,
        lora if lora.ends_with("_comfy_converted.safetensors") => {
            "klein-object-remove-lora-comfy.safetensors"
        }
        other => panic!("no local file for {other}"),
    };
    models.join(local)
}

/// Answers each declared URL from the loopback server, through the real HTTP transport, and
/// reports the declared URL as the one that answered; refuses every other request.
#[derive(Debug)]
struct Loopback {
    inner: luxforge_net::HttpTransport,
    served: BTreeMap<String, String>,
}

impl Transport for Loopback {
    fn send(
        &self,
        request: &TransportRequest,
        options: SendOptions<'_>,
        sink: &mut dyn Write,
    ) -> Result<TransportResponse, Error> {
        let declared = request.endpoint.url.clone();
        let local = self.served.get(declared.as_str()).ok_or_else(|| {
            Error::validation(format!("the test serves no {declared}"))
        })?;
        let rewritten = TransportRequest {
            method: request.method,
            endpoint: parse_endpoint(local, &[EndpointClass::Loopback])?,
            headers: request.headers.clone(),
            body: request.body.clone(),
        };
        let mut response = self.inner.send(&rewritten, options, sink)?;
        response.final_url = declared;
        Ok(response)
    }
}

struct Client {
    owner: OwnerHandle,
    client: ClientId,
    next: u64,
}

impl Client {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, Value> {
        self.next += 1;
        let response = self
            .owner
            .call(
                self.client,
                ApiRequest {
                    id: format!("request-{}", self.next),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .unwrap();
        match response.error {
            Some(error) => Err(serde_json::to_value(error).unwrap()),
            None => Ok(response.result.unwrap_or(Value::Null)),
        }
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        self.call(method, params)
            .unwrap_or_else(|error| panic!("{method}: {error}"))
    }

    fn mutation(&self) -> Value {
        json!({"request_id": format!("mutation-{}", self.next + 1), "actor": "loopback-test"})
    }

    /// Wait for a job to end; answers its record.
    fn finished(&mut self, job: &Value) -> Value {
        let deadline = Instant::now() + Duration::from_secs(1800);
        loop {
            let record = self.ok("job.read", json!({"job_id": job}));
            if !matches!(record["status"].as_str(), Some("queued" | "running")) {
                return record;
            }
            assert!(Instant::now() < deadline, "the job never ended: {record}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn install(&mut self, resource: &Value) -> (Value, Duration) {
        let id = resource["id"].as_str().unwrap();
        let refused = self
            .call(
                "module.resource.install",
                json!({"module_id": AI, "resource_id": id, "mutation": self.mutation()}),
            )
            .unwrap_err();
        assert_eq!(refused["code"], "consent-required", "{refused}");
        let disclosure = refused["data"]["consent"]["disclosure"].clone();
        let consent = &refused["data"]["consent"];
        assert_eq!(consent["scope"]["version"], resource["version"], "{consent}");
        let grant = json!({
            "module_id": AI, "capability": consent["capability"],
            "scope": consent["scope"], "mutation": self.mutation(),
        });
        self.ok("module.permission.grant", grant);
        let started = Instant::now();
        let queued = self.ok(
            "module.resource.install",
            json!({"module_id": AI, "resource_id": id, "mutation": self.mutation()}),
        );
        let record = self.finished(&queued["job_id"]);
        (json!({"disclosure": disclosure, "record": record}), started.elapsed())
    }
}

#[test]
#[ignore = "needs the pinned klein files"]
fn klein_installs_over_loopback_and_both_slots_load() {
    let models = models();
    let registry = ModuleRegistry::builtin();
    let descriptor = registry
        .descriptors()
        .into_iter()
        .find(|descriptor| descriptor.id == AI)
        .unwrap()
        .clone();
    let declared: Vec<String> = descriptor
        .resources
        .iter()
        .flat_map(|resource| {
            std::iter::once(resource.url.clone())
                .chain(resource.files.iter().map(|file| file.url.clone()))
        })
        .collect();
    assert_eq!(declared.len(), 6, "{declared:?}");
    assert!(declared.iter().any(|url| url.ends_with(CONDITIONING_ASSET)));

    // The loopback server streams each file from disk under a path of its own.
    let files: BTreeMap<String, PathBuf> = declared
        .iter()
        .enumerate()
        .map(|(index, url)| (format!("/{index}"), local_file(&models, url)))
        .collect();
    for path in files.values() {
        assert!(path.is_file(), "missing {}", path.display());
    }
    let serving = files.clone();
    let server = TestServer::http(move |request, out| {
        let Some(path) = serving.get(&request.path) else {
            respond(out, "404 Not Found", "", b"");
            return;
        };
        let mut file = fs::File::open(path).unwrap();
        let length = file.metadata().unwrap().len();
        let head =
            format!("HTTP/1.1 200 OK\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n");
        let _ = out.write_all(head.as_bytes());
        let _ = std::io::copy(&mut file, out);
        let _ = out.flush();
    })
    .unwrap();
    let served = declared
        .iter()
        .enumerate()
        .map(|(index, url)| (url.clone(), server.url(&format!("/{index}"))))
        .collect();

    let root = std::env::temp_dir().join(format!("luxforge-klein-loopback-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let host = HostConfig {
        config_dir: Some(root.join("config")),
        resource_dir: Some(root.join("resources")),
        secrets: Arc::new(MemorySecretStore::new()),
        transport: Arc::new(Loopback {
            inner: luxforge_net::HttpTransport::system(),
            served,
        }),
        machine_memory_bytes: luxforge_core::capabilities::host::installed_memory_bytes(),
        inference: Some(luxforge_diffusion::DiffusionRuntime::shared()),
        ..HostConfig::unconfigured()
    };
    let (owner, join) =
        OwnerHandle::start_with_host(&root.join("catalog.sqlite"), Arc::new(registry), host)
            .unwrap();
    let client = owner.register_with(ClientAuthority::Permissions);
    let mut client = Client {
        owner: owner.clone(),
        client,
        next: 0,
    };

    let resources: Vec<Value> = descriptor
        .resources
        .iter()
        .map(|resource| serde_json::to_value(resource).unwrap())
        .collect();
    for resource in &resources {
        let (installed, took) = client.install(resource);
        let record = &installed["record"];
        assert_eq!(record["status"], "ready", "{record}");
        let result = &record["result"];
        assert!(result.get("prepare_error").is_none(), "{result}");
        let total: u64 = resource["bytes"].as_u64().unwrap()
            + resource["files"]
                .as_array()
                .unwrap()
                .iter()
                .map(|file| file["bytes"].as_u64().unwrap())
                .sum::<u64>();
        assert_eq!(result["bytes"], total);
        eprintln!(
            "{} installed over loopback: {total} bytes in {:.1} s; disclosure {}",
            resource["id"],
            took.as_secs_f64(),
            installed["disclosure"]
        );
    }
    let generator = &resources[0];
    let conditioning = generator["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["url"].as_str().unwrap().ends_with(CONDITIONING_ASSET))
        .unwrap();
    assert_eq!(
        conditioning["sha256"],
        "ba26d15a8b280abf7f436cbd817d2caaba412b9e186aa6a2163cf6a73c199141"
    );
    let version_dir = root
        .join("resources")
        .join(AI)
        .join(GENERATOR)
        .join(generator["version"].as_str().unwrap());
    assert_eq!(
        fs::read(version_dir.join("removal-conditioning.lfsdcond")).unwrap(),
        fs::read(models.join(CONDITIONING_ASSET)).unwrap()
    );

    // Both slots load on this runtime: Replace's with the encoder and the generator beside it.
    for resource in [GENERATOR, ENCODER] {
        let started = Instant::now();
        let load = client.ok(
            "module.runtime.load",
            json!({"module_id": AI, "resource_id": resource}),
        );
        if let Some(job) = load.get("job_id") {
            let record = client.finished(job);
            assert_eq!(record["status"], "ready", "{record}");
        }
        eprintln!("{resource} loaded in {:.1} s", started.elapsed().as_secs_f64());
    }
    let status = client.ok("module.status", json!({"module_id": AI}));
    let held: Vec<(Value, Value)> = status["runtime"]["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| (model["resource_id"].clone(), model["state"].clone()))
        .collect();
    assert_eq!(
        held,
        [
            (json!(GENERATOR), json!("loaded")),
            (json!(ENCODER), json!("loaded"))
        ],
        "{status}"
    );
    eprintln!("runtime: {}", status["runtime"]);

    owner.stop();
    join.join().unwrap();
    drop(server);
    let _ = fs::remove_dir_all(&root);
}
