//! One runtime made of several: what [`crate::HostConfig::inference`] holds when a host serves more
//! than one runtime, as a developer run serves the deterministic fake beside ONNX Runtime.
//!
//! Every backend name the composite offers belongs to exactly one member, under the member's own
//! name or under another the composite gives it: the fake's `cpu` is offered as `fake`, so a model
//! asking for `cpu` reaches ONNX Runtime and one asking for `fake` reaches the fake. An open picks
//! the first of the capability's backends the composite offers ([`OpenOptions::choose`]) and hands
//! the member that one backend, under the member's own name. A name two members would both offer
//! is refused when the composite is built, never resolved by order at an open.
use super::{Generator, InferenceRuntime, ModelFile, OpenOptions, RuntimeIdentity, Session};
use crate::{Cancel, Error};
use std::{path::PathBuf, sync::Arc};

/// One backend name the composite offers: the member that serves it and the member's own name.
#[derive(Clone)]
struct Route {
    offered: String,
    runtime: Arc<dyn InferenceRuntime>,
    member: String,
}

/// Several runtimes behind one, dispatched by backend name. See the [module documentation](self).
#[derive(Clone, Default)]
pub struct CompositeInference {
    routes: Vec<Route>,
    members: Vec<Arc<dyn InferenceRuntime>>,
}

impl CompositeInference {
    pub fn new() -> Self {
        Self::default()
    }

    /// Serve every backend `runtime` offers under its own name, after the backends already served.
    /// Refused with `conflict` when one of them is already served.
    pub fn with(self, runtime: Arc<dyn InferenceRuntime>) -> Result<Self, Error> {
        let names = runtime.backends();
        names.into_iter().try_fold(self, |composite, name| {
            composite.route(name.clone(), runtime.clone(), name)
        })
    }

    /// Serve `runtime`'s backend `member` under the name `offered`, after the backends already
    /// served. Refused with `validation` when `runtime` does not offer `member`, and with
    /// `conflict` when `offered` is already served.
    pub fn with_alias(
        self,
        offered: impl Into<String>,
        runtime: Arc<dyn InferenceRuntime>,
        member: impl Into<String>,
    ) -> Result<Self, Error> {
        let member = member.into();
        if !runtime.backends().contains(&member) {
            return Err(Error::validation(format!(
                "runtime {} does not offer backend {member}",
                runtime.identity().name
            )));
        }
        self.route(offered.into(), runtime, member)
    }

    /// The composite as a host is handed it.
    pub fn shared(self) -> Arc<dyn InferenceRuntime> {
        Arc::new(self)
    }

    fn route(
        mut self,
        offered: String,
        runtime: Arc<dyn InferenceRuntime>,
        member: String,
    ) -> Result<Self, Error> {
        if let Some(route) = self.routes.iter().find(|route| route.offered == offered) {
            return Err(Error::new(
                crate::ErrorKind::Conflict,
                format!(
                    "backend {offered} is already served by {}",
                    route.runtime.identity().name
                ),
            ));
        }
        if !self
            .members
            .iter()
            .any(|known| Arc::ptr_eq(known, &runtime))
        {
            self.members.push(runtime.clone());
        }
        self.routes.push(Route {
            offered,
            runtime,
            member,
        });
        Ok(self)
    }

    /// The member and the options it is handed for the first of `options.backends` served here.
    fn dispatch(&self, options: &OpenOptions) -> Result<(&Route, OpenOptions), Error> {
        let chosen = options.choose(&self.backends())?;
        let route = self
            .routes
            .iter()
            .find(|route| route.offered == chosen)
            .ok_or_else(|| Error::internal(format!("backend {chosen} has no route")))?;
        Ok((
            route,
            OpenOptions {
                backends: vec![route.member.clone()],
                precision: options.precision,
            },
        ))
    }
}

impl InferenceRuntime for CompositeInference {
    /// The members' names and versions, joined with ` + ` in the order they were added. Which one
    /// ran a model is the backend its placement names.
    fn identity(&self) -> RuntimeIdentity {
        let identities: Vec<_> = self
            .members
            .iter()
            .map(|member| member.identity())
            .collect();
        RuntimeIdentity {
            name: identities
                .iter()
                .map(|identity| identity.name.as_str())
                .collect::<Vec<_>>()
                .join(" + "),
            version: identities
                .iter()
                .map(|identity| identity.version.as_str())
                .collect::<Vec<_>>()
                .join(" + "),
        }
    }

    fn backends(&self) -> Vec<String> {
        self.routes
            .iter()
            .map(|route| route.offered.clone())
            .collect()
    }

    /// Every member prepares the model, and the files they wrote are answered together.
    fn prepare(&self, model: &ModelFile, cancel: &Cancel) -> Result<Vec<PathBuf>, Error> {
        let mut prepared = Vec::new();
        for member in &self.members {
            prepared.extend(member.prepare(model, cancel)?);
        }
        Ok(prepared)
    }

    /// The members serving one of `backends` prepare the model, each once with its own names for
    /// them, and the files they wrote are answered together.
    fn prepare_for(
        &self,
        model: &ModelFile,
        backends: &[String],
        cancel: &Cancel,
    ) -> Result<Vec<PathBuf>, Error> {
        let mut prepared = Vec::new();
        for member in &self.members {
            let names: Vec<String> = self
                .routes
                .iter()
                .filter(|route| {
                    Arc::ptr_eq(&route.runtime, member) && backends.contains(&route.offered)
                })
                .map(|route| route.member.clone())
                .collect();
            if !names.is_empty() {
                prepared.extend(member.prepare_for(model, &names, cancel)?);
            }
        }
        Ok(prepared)
    }

    fn forget(&self, sha256: &str) -> usize {
        self.members
            .iter()
            .map(|member| member.forget(sha256))
            .sum()
    }

    fn open_session(
        &self,
        model: &ModelFile,
        options: &OpenOptions,
        cancel: &Cancel,
    ) -> Result<Box<dyn Session>, Error> {
        let (route, options) = self.dispatch(options)?;
        route.runtime.open_session(model, &options, cancel)
    }

    fn open_generator(
        &self,
        model: &ModelFile,
        options: &OpenOptions,
        cancel: &Cancel,
    ) -> Result<Box<dyn Generator>, Error> {
        let (route, options) = self.dispatch(options)?;
        route.runtime.open_generator(model, &options, cancel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ErrorKind,
        inference::{
            ModelIdentity, Precision, Tensor, TensorData,
            fake::{FAKE_BACKEND, FAKE_INPUT, FakeInference},
        },
    };

    /// A runtime that offers named backends and opens nothing, so a test sees which member an open
    /// reached and with which backend.
    struct Named {
        name: &'static str,
        backends: Vec<String>,
    }

    impl InferenceRuntime for Named {
        fn identity(&self) -> RuntimeIdentity {
            RuntimeIdentity {
                name: self.name.into(),
                version: "9".into(),
            }
        }

        fn backends(&self) -> Vec<String> {
            self.backends.clone()
        }

        fn open_session(
            &self,
            _model: &ModelFile,
            options: &OpenOptions,
            _cancel: &Cancel,
        ) -> Result<Box<dyn Session>, Error> {
            Err(Error::render(format!(
                "{} asked for {}",
                self.name,
                options.backends.join(",")
            )))
        }
    }

    fn named(name: &'static str, backends: &[&str]) -> Arc<dyn InferenceRuntime> {
        Arc::new(Named {
            name,
            backends: backends.iter().map(|backend| (*backend).into()).collect(),
        })
    }

    fn options(backends: &[&str]) -> OpenOptions {
        OpenOptions {
            backends: backends.iter().map(|backend| (*backend).into()).collect(),
            precision: Precision::Fp32,
        }
    }

    fn model(dir: &std::path::Path) -> ModelFile {
        let path = dir.join("model.bin");
        std::fs::write(&path, b"model").unwrap();
        ModelFile::new(
            ModelIdentity {
                id: "segmenter".into(),
                version: "1".into(),
                sha256: "ab".repeat(32),
            },
            path,
        )
    }

    fn refusal(result: Result<Box<dyn Session>, Error>) -> Error {
        match result {
            Ok(_) => panic!("the open succeeded"),
            Err(error) => error,
        }
    }

    #[test]
    fn each_backend_reaches_the_member_that_serves_it_under_the_members_own_name() {
        let dir = luxforge_testbase::paths::temp_dir("composite-dispatch");
        let model = model(&dir);
        let fake = FakeInference::shared();
        let composite = CompositeInference::new()
            .with(named("onnx", &["coreml", "cpu"]))
            .unwrap()
            .with_alias("fake", fake, FAKE_BACKEND)
            .unwrap();
        assert_eq!(composite.backends(), ["coreml", "cpu", "fake"]);
        assert_eq!(composite.identity().name, "onnx + luxforge-fake");
        assert_eq!(composite.identity().version, "9 + 1");
        let cancel = Cancel::never();
        // `cpu` is the native runtime's, though the fake's own backend is also called `cpu`.
        let error = refusal(composite.open_session(&model, &options(&["cpu"]), &cancel));
        assert_eq!(error.detail, "onnx asked for cpu");
        let error = refusal(composite.open_session(
            &model,
            &options(&["directml", "coreml", "cpu"]),
            &cancel,
        ));
        assert_eq!(error.detail, "onnx asked for coreml");
        // `fake` reaches the fake under its own name, and it runs.
        let session = composite
            .open_session(&model, &options(&["fake", "cpu"]), &cancel)
            .unwrap();
        assert_eq!(session.placement().backend, FAKE_BACKEND);
        let input = Tensor::new(
            FAKE_INPUT,
            vec![1, 1, 1, 2],
            TensorData::F32(vec![0.0, 1.0]),
        )
        .unwrap();
        assert_eq!(session.run(&[input], &cancel).unwrap().len(), 1);
        // The generator dispatches the same way.
        assert!(
            composite
                .open_generator(&model, &options(&["fake"]), &cancel)
                .is_ok()
        );
        let error = refusal(composite.open_session(&model, &options(&["tpu"]), &cancel));
        assert_eq!(error.kind, ErrorKind::NotReady);
        assert!(
            error.detail.contains("coreml, cpu, fake"),
            "{}",
            error.detail
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_backend_two_members_would_serve_is_refused_when_the_composite_is_built() {
        let composite = CompositeInference::new()
            .with(named("onnx", &["coreml", "cpu"]))
            .unwrap();
        let error = match composite.clone().with(FakeInference::shared()) {
            Ok(_) => panic!("two members served cpu"),
            Err(error) => error,
        };
        assert_eq!(error.kind, ErrorKind::Conflict);
        assert_eq!(error.detail, "backend cpu is already served by onnx");
        let error =
            match composite
                .clone()
                .with_alias("coreml", FakeInference::shared(), FAKE_BACKEND)
            {
                Ok(_) => panic!("two members served coreml"),
                Err(error) => error,
            };
        assert_eq!(error.kind, ErrorKind::Conflict);
        let error = match composite.with_alias("fake", FakeInference::shared(), "gpu") {
            Ok(_) => panic!("an alias of a backend the member does not offer"),
            Err(error) => error,
        };
        assert_eq!(error.kind, ErrorKind::Validation);
    }

    #[test]
    fn prepare_asks_every_member_once() {
        let dir = luxforge_testbase::paths::temp_dir("composite-prepare");
        let model = model(&dir);
        let onnx = named("onnx", &["coreml", "cpu"]);
        let composite = CompositeInference::new()
            .with(onnx.clone())
            .unwrap()
            .with_alias("fake", FakeInference::shared(), FAKE_BACKEND)
            .unwrap();
        assert_eq!(composite.members.len(), 2);
        assert!(
            composite
                .prepare(&model, &Cancel::never())
                .unwrap()
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A member that answers one derived path per prepare, so a test sees which members were asked.
    struct Deriving {
        backends: Vec<String>,
        asked: std::sync::Mutex<Vec<Vec<String>>>,
    }

    impl InferenceRuntime for Deriving {
        fn identity(&self) -> RuntimeIdentity {
            RuntimeIdentity {
                name: "deriving".into(),
                version: "1".into(),
            }
        }
        fn backends(&self) -> Vec<String> {
            self.backends.clone()
        }
        fn prepare(&self, model: &ModelFile, _: &Cancel) -> Result<Vec<PathBuf>, Error> {
            Ok(vec![model.path().with_extension("derived")])
        }
        fn prepare_for(
            &self,
            model: &ModelFile,
            backends: &[String],
            cancel: &Cancel,
        ) -> Result<Vec<PathBuf>, Error> {
            self.asked.lock().unwrap().push(backends.to_vec());
            self.prepare(model, cancel)
        }
        fn open_session(
            &self,
            _: &ModelFile,
            _: &OpenOptions,
            _: &Cancel,
        ) -> Result<Box<dyn Session>, Error> {
            Err(Error::validation("opens nothing"))
        }
    }

    #[test]
    fn prepare_for_asks_only_the_members_serving_the_named_backends() {
        let dir = luxforge_testbase::paths::temp_dir("composite-prepare-for");
        let model = model(&dir);
        let onnx = Arc::new(Deriving {
            backends: vec!["coreml".into(), "cpu".into()],
            asked: Default::default(),
        });
        let composite = CompositeInference::new()
            .with(onnx.clone())
            .unwrap()
            .with_alias("fake", FakeInference::shared(), FAKE_BACKEND)
            .unwrap();
        let fake_only = composite
            .prepare_for(&model, &["fake".into()], &Cancel::never())
            .unwrap();
        assert!(fake_only.is_empty());
        assert!(onnx.asked.lock().unwrap().is_empty());
        let both = composite
            .prepare_for(
                &model,
                &["gpu".into(), "cpu".into(), "fake".into()],
                &Cancel::never(),
            )
            .unwrap();
        assert_eq!(both, [model.path().with_extension("derived")]);
        assert_eq!(*onnx.asked.lock().unwrap(), [vec!["cpu".to_owned()]]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
