//! Constructors for writing a script in code, which is how xtask builds every scenario's: each
//! step's payload converts into its [`Step`], and the common shapes have a short constructor. A
//! step built here is checked by [`Step::validate`] like one read from disk.
use crate::*;

macro_rules! step_from {
    ($($payload:ident => $variant:ident),* $(,)?) => {
        $(impl From<$payload> for Step {
            fn from(step: $payload) -> Self {
                Self::$variant(step)
            }
        })*
    };
}

step_from! {
    DraftStep => Draft,
    SliderStep => Slider,
    DoubleClickStep => DoubleClick,
    ControlsStep => Controls,
    PickerStep => Picker,
    CurveStep => Curve,
    GroupStep => Group,
    TabStep => Tab,
    SectionStep => Section,
    FieldStep => Field,
    ResetStep => Reset,
    PickStep => Pick,
    SliderDraftStep => SliderDraft,
    ViewStep => View,
    WorkspaceStep => Workspace,
    PreviewStep => Preview,
    PaletteStep => Palette,
    PresetCreateStep => PresetCreate,
    CapabilityStep => Capability,
    MaskStep => Mask,
}

impl Step {
    /// One owner request without parameters.
    pub fn api(method: impl Into<String>) -> Self {
        Self::Api {
            method: method.into(),
            params: Map::new(),
        }
    }

    /// One owner request with these parameters, which must be a JSON object.
    pub fn call(method: impl Into<String>, params: Value) -> Self {
        let Value::Object(params) = params else {
            panic!("api params are an object, not {params}");
        };
        Self::Api {
            method: method.into(),
            params,
        }
    }

    /// A tools-panel section expanded or collapsed.
    pub fn section(module: impl Into<String>, expanded: bool) -> Self {
        Self::Section(SectionStep {
            module: module.into(),
            expanded,
        })
    }

    pub fn wait(ms: u64) -> Self {
        Self::Wait { ms }
    }

    /// The tools panel scrolled to this fraction of its range.
    pub fn tools_scroll(fraction: f64) -> Self {
        Self::ToolsScroll(fraction)
    }

    pub fn gallery(page: Option<usize>) -> Self {
        Self::Gallery { page }
    }

    pub fn hover(x: u32, y: u32) -> Self {
        Self::Hover { x, y }
    }

    pub fn pan(x: f32, y: f32) -> Self {
        Self::Pan { x, y }
    }

    pub fn performance(expanded: bool) -> Self {
        Self::Performance { expanded }
    }

    pub fn preset(name: impl Into<String>) -> Self {
        Self::Preset(PresetPick {
            name: name.into(),
            group: None,
        })
    }

    pub fn preset_import(path: impl Into<String>) -> Self {
        Self::PresetImport { path: path.into() }
    }

    pub fn pick(x: u32, y: u32) -> Self {
        Self::Pick(PickStep { x, y })
    }

    pub fn reset(module: impl Into<String>, group: Option<&str>) -> Self {
        Self::Reset(ResetStep {
            module: module.into(),
            group: group.map(str::to_owned),
        })
    }

    /// A typed value in one generated field, with Enter pressed in it when `submit`.
    pub fn field(
        action: impl Into<String>,
        parameter: impl Into<String>,
        text: impl Into<String>,
        submit: bool,
    ) -> Self {
        Self::Field(FieldStep {
            action: action.into(),
            parameter: parameter.into(),
            text: text.into(),
            submit,
        })
    }
}

impl SliderStep {
    /// A gesture through `values`, left open; [`SliderStep::release`] or [`SliderStep::cancel`]
    /// ends it.
    pub fn new(
        action: impl Into<String>,
        parameter: impl Into<String>,
        values: impl Into<Vec<f64>>,
    ) -> Self {
        Self {
            action: action.into(),
            parameter: parameter.into(),
            values: values.into(),
            end: SliderEnd::Open,
            interval_ms: None,
        }
    }

    pub fn release(self) -> Self {
        Self {
            end: SliderEnd::Release,
            ..self
        }
    }

    pub fn cancel(self) -> Self {
        Self {
            end: SliderEnd::Cancel,
            ..self
        }
    }

    /// One value per this many milliseconds of the step's own timer.
    pub fn paced(self, interval_ms: u64) -> Self {
        Self {
            interval_ms: Some(interval_ms),
            ..self
        }
    }
}

impl WorkspaceStep {
    pub fn state_panel(self, open: bool) -> Self {
        Self {
            state_panel: Some(open),
            ..self
        }
    }

    pub fn tools_panel(self, open: bool) -> Self {
        Self {
            tools_panel: Some(open),
            ..self
        }
    }

    pub fn mode(self, mode: impl Into<String>) -> Self {
        Self {
            mode: Some(mode.into()),
            ..self
        }
    }

    pub fn thirds(self, on: bool) -> Self {
        Self {
            thirds: Some(on),
            ..self
        }
    }

    pub fn clip_shadows(self, on: bool) -> Self {
        Self {
            clip_shadows: Some(on),
            ..self
        }
    }

    pub fn clip_highlights(self, on: bool) -> Self {
        Self {
            clip_highlights: Some(on),
            ..self
        }
    }

    pub fn mask_overlay(self, overlay: impl Into<String>) -> Self {
        Self {
            mask_overlay: Some(overlay.into()),
            ..self
        }
    }

    pub fn mask_overlay_colour(self, colour: impl Into<String>) -> Self {
        Self {
            mask_overlay_colour: Some(colour.into()),
            ..self
        }
    }
}

impl CapabilityStep {
    /// One gesture on `module`'s section, whose frame waits for the jobs it starts.
    pub fn new(module: impl Into<String>, action: CapabilityAction) -> Self {
        Self {
            module: module.into(),
            action,
            wait: true,
        }
    }

    /// Capture while a job the step started is still running.
    pub fn no_wait(self) -> Self {
        Self {
            wait: false,
            ..self
        }
    }
}

impl From<usize> for Reference {
    fn from(index: usize) -> Self {
        Self::Index(index)
    }
}

impl MaskRow {
    pub fn new(component: impl Into<Reference>, edit: RowStep) -> Self {
        Self {
            component: component.into(),
            edit,
        }
    }
}
