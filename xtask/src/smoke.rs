//! The smoke runner: every scenario is one row of [`SCENARIOS`], and every row runs through
//! [`launch_all`] unless its shape needs a function of its own. A row names the scenario, its
//! launches — each with the [`Plan`] its script and frame count are derived from — the scenario's
//! own checks, what it opens and the window it opens at. `smoke --list`, [`dispatch`], the replay
//! and `verify`'s rendered tier all read the table, so a new scenario is one row.
use crate::{
    basic_smoke as basic, capabilities_smoke as capabilities, controls_smoke as controls,
    crop_smoke as crop, curve_smoke as curve, detail_smoke as detail,
    develop_picks_smoke as develop_picks, export_smoke as export, filmstrip_smoke as filmstrip,
    gallery_smoke as gallery, gpu_preview_smoke as gpu_preview,
    gpu_preview_zoom_smoke as gpu_preview_zoom, histogram_smoke as histogram,
    information_smoke as information, lens_smoke as lens, look_smoke as look, loupe_smoke as loupe,
    mask_brush_smoke as mask_brush, mask_combine_smoke as mask_combine,
    mask_interactions_smoke as mask_interactions, mask_panel_smoke as mask_panel,
    mask_range_smoke as mask_range, mask_smoke as mask, minify_smoke as minify,
    mixer_smoke as mixer, no_gpu_render_smoke as no_gpu_render, performance_smoke as performance,
    presence_smoke as presence, presets_smoke as presets, raw_panel_smoke as raw_panel,
    resolve_missing_smoke as resolve_missing,
    scenario::{Checked, Checks, Fixture, Launch, Plan, Run, Step, launch::Guard},
    select_smoke as select, settings_smoke as settings, theme_smoke as theme,
    viewport_smoke as viewport, vignette_smoke as vignette, visibility_smoke as visibility,
    workspace_smoke as workspace, zoom_smoke as zoom, *,
};
use std::{
    borrow::Borrow,
    process::ExitStatus,
    time::{Duration, Instant},
};

/// The window the design's layout constants are written against.
pub const PANELLED: [&str; 2] = ["1440", "900"];
const ORIENTATION_6: &str = "fixtures/s0/orientation-6.jpg";
const ORIENTATION_1: &str = "fixtures/s0/orientation-1.jpg";
const INVALID: &str = "fixtures/s0/invalid.jpg";

/// What a scenario opens.
#[derive(Clone, Copy, Debug)]
pub enum Source {
    /// These fixtures, in order, relative to the checkout; none opens nothing.
    Fixtures(&'static [&'static str]),
    /// These fixtures, unless `--source` names a photograph to open instead.
    Default(&'static [&'static str]),
    /// Only the photograph `--source` names: no checkout holds one, so the scenario is outside
    /// `rendered`. A `listed` one must also be given the RAW manifest that lists the photograph, as
    /// `--manifest`: the run keeps a copy of it as [`MANIFEST`], which its checks read, and which a
    /// replay reads again.
    Supplied { listed: bool },
}

/// Where a run whose source is `listed` keeps its copy of the RAW manifest.
pub const MANIFEST: &str = "manifest.json";

/// Waits for a launched editor in place of the ordinary wait, and returns what it recorded; see
/// [`crate::scenario::launch::Watcher`].
pub type Watch = fn(&mut Guard, Instant, Duration) -> Result<(Option<ExitStatus>, Value)>;

/// One editor launch of a scenario.
#[derive(Clone, Copy)]
pub struct LaunchSpec {
    /// Its evidence directory, `app` for a scenario's one launch, with its log beside it.
    pub name: &'static str,
    /// The file its script is kept in.
    pub script: &'static str,
    /// Every frame it captures and what each must show, over the sources the run opens.
    pub plan: fn(&[PathBuf]) -> Plan,
    /// The earlier launch whose catalog it reopens.
    pub catalog: Option<&'static str>,
    /// Built-in modules registered as unavailable.
    pub disable: &'static [&'static str],
    /// Draw the photograph at Fit through the GPU preview stage's identity program.
    pub gpu_identity: bool,
    /// Refuse the editor's GPU stage, as a machine whose adapter cannot run it does.
    pub no_gpu_render: bool,
    pub developer: bool,
    /// A watcher to wait with, and the file what it records is kept in.
    pub watch: Option<(&'static str, Watch)>,
    /// A process deadline of its own in place of the run's.
    pub deadline: Option<Duration>,
}

/// A scenario's one launch, as the rows spell it with `..APP`.
pub const APP: LaunchSpec = LaunchSpec {
    name: "app",
    script: "script.json",
    plan: |_| Plan::default(),
    catalog: None,
    disable: &[],
    gpu_identity: false,
    no_gpu_render: false,
    developer: false,
    watch: None,
    deadline: None,
};

/// The scenario's own checks, over every launch's evidence once its plan has held, in launch order.
pub type Verify = fn(&mut Run, &[Checked]) -> Result;

/// A scenario's own run, over the row and the sources it opens.
pub type Own = fn(Run, &'static Scenario, Vec<PathBuf>) -> Result;

/// One row of the table.
pub struct Scenario {
    pub name: &'static str,
    /// What it proves, in a line, for `smoke --list`.
    pub about: &'static str,
    pub launches: &'static [LaunchSpec],
    pub verify: Verify,
    pub source: Source,
    pub window: Option<[&'static str; 2]>,
    /// The paragraph `reproduce.md` gives it, when its launches need saying more about.
    pub note: Option<&'static str>,
    /// A scenario whose shape a list of launches cannot say runs its own function instead, through
    /// the same library and with the same launches and checks: `zoom` makes its launch once per
    /// photograph, each a whole run of its own, and `capabilities` starts a proof endpoint in this
    /// process for its launch to talk to and scans everything the run wrote for its secret.
    pub own: Option<Own>,
}

impl Scenario {
    /// Whether `verify --tier rendered` runs it: everything a checkout can open.
    pub fn rendered(&self) -> bool {
        !matches!(self.source, Source::Supplied { .. })
            && (self.name != visibility::SCENARIO || cfg!(target_os = "macos"))
    }

    /// Whether `--source` may replace what it opens.
    pub fn takes_source(&self) -> bool {
        matches!(self.source, Source::Default(_) | Source::Supplied { .. })
    }

    /// Whether it needs `--manifest`.
    pub fn listed(&self) -> bool {
        matches!(self.source, Source::Supplied { listed: true })
    }

    /// What it opens: `given` when `--source` named one, else its own fixtures.
    fn sources(&self, root: &Path, given: Option<Vec<PathBuf>>) -> Result<Vec<PathBuf>> {
        match (self.source, given) {
            (_, Some(given)) => {
                ensure(
                    self.takes_source(),
                    format!("--source is only for {}", sourced().join(" and ")),
                )?;
                Ok(given)
            }
            (Source::Fixtures(fixtures) | Source::Default(fixtures), None) => {
                Ok(fixtures.iter().map(|fixture| root.join(fixture)).collect())
            }
            (Source::Supplied { .. }, None) => {
                Err(format!("The {} scenario needs --source RAW_FILE", self.name).into())
            }
        }
    }
}

/// The scenarios `--source` may be given to.
fn sourced() -> Vec<&'static str> {
    SCENARIOS
        .iter()
        .filter(|scenario| scenario.takes_source())
        .map(|scenario| scenario.name)
        .collect()
}

/// Every scenario, in the order `verify --tier rendered` runs them.
pub static SCENARIOS: &[Scenario] = &[
    Scenario {
        name: visibility::SCENARIO,
        about: "Native minimize/hide/restore pauses presentation sampling and preserves fresh history",
        launches: &[LaunchSpec {
            plan: visibility::plan,
            watch: Some((visibility::READINGS, visibility::watch)),
            deadline: Some(Duration::from_secs(420)),
            ..APP
        }],
        verify: visibility::verify,
        source: Source::Default(&[visibility::FIXTURE]),
        window: Some(PANELLED),
        note: Some(
            "Actual AppKit visibility transitions on a transparent background-only window; no foreground activation. Captures are renderer readbacks. Native visibility qualification is macOS-only.",
        ),
        own: None,
    },
    Scenario {
        name: "detail",
        about: "Detail controls and correlated rendered presentation",
        launches: &[LaunchSpec {
            plan: detail::plan,
            deadline: Some(Duration::from_secs(310)),
            ..APP
        }],
        verify: detail::verify,
        source: Source::Fixtures(&[detail::FIXTURE]),
        window: Some(PANELLED),
        note: None,
        own: None,
    },
    Scenario {
        name: "detail-fit",
        about: "Detail controls and correlated rendered presentation",
        launches: &[LaunchSpec {
            plan: detail::fit_plan,
            deadline: Some(Duration::from_secs(310)),
            ..APP
        }],
        verify: detail::verify_fit,
        source: Source::Fixtures(&[detail::FIXTURE]),
        window: Some(PANELLED),
        note: None,
        own: None,
    },
    Scenario {
        name: "detail-zoom",
        about: "Detail controls and correlated rendered presentation",
        launches: &[LaunchSpec {
            plan: detail::zoom_plan,
            deadline: Some(Duration::from_secs(310)),
            ..APP
        }],
        verify: detail::verify_zoom,
        source: Source::Fixtures(&[detail::FIXTURE]),
        window: Some(PANELLED),
        note: None,
        own: None,
    },
    Scenario {
        name: "raw-detail",
        about: "Detail controls and correlated rendered presentation",
        launches: &[LaunchSpec {
            plan: detail::raw_plan,
            deadline: Some(Duration::from_secs(310)),
            ..APP
        }],
        verify: detail::verify_raw,
        source: Source::Supplied { listed: true },
        window: Some(PANELLED),
        note: None,
        own: None,
    },
    Scenario {
        name: "empty",
        about: "The editor with nothing open",
        launches: &[LaunchSpec {
            plan: |_| Plan::new(vec![Step::opened("empty")]),
            ..APP
        }],
        verify: plain,
        source: Source::Fixtures(&[]),
        window: None,
        note: None,
        own: None,
    },
    Scenario {
        name: "load",
        about: "One JPEG opened at Fit",
        launches: &[LaunchSpec { plan: opens, ..APP }],
        verify: plain,
        source: Source::Fixtures(&[ORIENTATION_6]),
        window: None,
        note: None,
        own: None,
    },
    Scenario {
        name: "replacement",
        about: "A JPEG replaced by an invalid file, which fails with the photograph kept",
        launches: &[LaunchSpec {
            plan: |_| {
                Plan::new(vec![
                    Step::opened("opened").label("Original"),
                    // The failed replacement keeps the photograph and its history on screen.
                    Step::opened("replaced")
                        .refused("invalid-input")
                        .label("Original"),
                ])
            },
            ..APP
        }],
        verify: plain,
        source: Source::Fixtures(&[ORIENTATION_6, INVALID]),
        window: None,
        note: None,
        own: None,
    },
    Scenario {
        name: "invalid",
        about: "An invalid file refused on open",
        launches: &[LaunchSpec {
            plan: |_| Plan::new(vec![Step::opened("refused").refused("invalid-input")]),
            ..APP
        }],
        verify: plain,
        source: Source::Fixtures(&[INVALID]),
        window: None,
        note: None,
        own: None,
    },
    Scenario {
        name: "repeated",
        about: "One JPEG opened eight times, each open displayed",
        launches: &[LaunchSpec { plan: opens, ..APP }],
        verify: plain,
        source: Source::Fixtures(&[ORIENTATION_6; 8]),
        window: None,
        note: None,
        own: None,
    },
    Scenario {
        name: "alternating",
        about: "Two orientations opened in turn, four times each",
        launches: &[LaunchSpec { plan: opens, ..APP }],
        verify: plain,
        source: Source::Fixtures(&[
            ORIENTATION_6,
            ORIENTATION_1,
            ORIENTATION_6,
            ORIENTATION_1,
            ORIENTATION_6,
            ORIENTATION_1,
            ORIENTATION_6,
            ORIENTATION_1,
        ]),
        window: None,
        note: None,
        own: None,
    },
    Scenario {
        name: "large24",
        about: "The generated 24 MP JPEG at Fit, shown as its proxy",
        launches: &[LaunchSpec { plan: opens, ..APP }],
        verify: plain,
        source: Source::Fixtures(&["fixtures/generated/24mp.jpg"]),
        window: None,
        note: None,
        own: None,
    },
    Scenario {
        name: "large60",
        about: "The generated 60 MP JPEG at Fit, shown as its proxy",
        launches: &[LaunchSpec { plan: opens, ..APP }],
        verify: plain,
        source: Source::Fixtures(&["fixtures/generated/60mp.jpg"]),
        window: None,
        note: None,
        own: None,
    },
    Scenario {
        name: GPU_IDENTITY,
        about: "One JPEG at Fit drawn by the GPU preview stage's identity program, with its path, budget and label",
        launches: &[LaunchSpec {
            plan: gpu_identity_plan,
            gpu_identity: true,
            ..APP
        }],
        verify: gpu_identity,
        source: Source::Fixtures(&[ORIENTATION_6]),
        window: None,
        note: Some(
            "The launch passes `--evidence-gpu-identity`, the evidence run's test hook: the editor \
             holds each Fit frame it presents as an rgba16float boundary and draws the photograph \
             through the photo surface's GPU stage with the identity program, the frame itself \
             staying the fallback.",
        ),
        own: None,
    },
    Scenario {
        name: gpu_preview::SCENARIO,
        about: "Gestures at Fit drawn on the GPU over a held boundary with no preview job per tick: a Basic drag, a gradient move, a brush stroke, a Texture and a Clarity drag and a Basic drag under Presence, each correlated with the CPU frame of its settings",
        launches: &[LaunchSpec {
            plan: gpu_preview::plan,
            // Its Presence drags wait for Presence's sequences to compile.
            deadline: Some(Duration::from_secs(120)),
            ..APP
        }],
        verify: gpu_preview::verify,
        source: Source::Fixtures(&[gpu_preview::FIXTURE]),
        window: Some(PANELLED),
        note: Some(
            "Each gesture draws from a boundary derived on the GPU from the source the photograph's \
             own job handed the surface; a scripted wait lets the surface evaluate the plan and \
             the sequence compile, and the gesture's later ticks are drawn on the GPU with no \
             preview job. The checks read the tick and job events of each step and compare each \
             GPU frame with the CPU frame of the same settings.",
        ),
        own: None,
    },
    Scenario {
        name: gpu_preview_zoom::SCENARIO,
        about: "Basic drags at 100% and 200% drawn on the GPU over the visible region at full scale with no preview job per tick, correlated with the CPU frame of their settings, a drag at 800% panned past its region, Presence drags and Basic drags under Presence at 100%, and drags at 50% and 33% drawn on the GPU over the whole stage at its displayed size",
        launches: &[
            LaunchSpec {
                plan: gpu_preview_zoom::plan,
                // Its Presence drags at 100% wait for their boundaries and sequences.
                deadline: Some(Duration::from_secs(150)),
                ..APP
            },
            // The first launch's script holds the evidence's 64 steps, so the drags below 100%
            // are their own short launch.
            LaunchSpec {
                name: "below",
                script: "script-below.json",
                plan: gpu_preview_zoom::below_plan,
                ..APP
            },
        ],
        verify: gpu_preview_zoom::verify,
        source: Source::Fixtures(&[gpu_preview_zoom::FIXTURE]),
        window: Some(PANELLED),
        note: Some(
            "Each drag draws from a boundary derived on the GPU from the source the surface holds, \
             a window of it cut at full scale over the region the view shows at 100% and above and \
             the source reduced to the displayed-size proxy below 100%; a scripted wait lets the \
             surface evaluate the plan and the sequence compile, and the drag's later ticks are \
             drawn on the GPU with no preview job of any kind. The checks read \
             each step's tick and job events, the visible region and the plan's region recorded \
             with each frame, or below 100% the proxy the boundary holds against the view's bounds \
             and the CPU frame, and compare each GPU frame with the CPU frame its release commits.",
        ),
        own: None,
    },
    Scenario {
        name: no_gpu_render::SCENARIO,
        about: "The editor launched with --no-gpu-render: the session names the reference renderer for no-adapter, every frame of an open, a Basic drag and its release at Fit and at 100% is drawn on the CPU path with no plan handed to the surface, each drag on its display-size proxy and each release sharp, the status bar says the reference renderer draws, at rest too, and an export is the reference renderer's for refused",
        launches: &[LaunchSpec {
            plan: no_gpu_render::plan,
            no_gpu_render: true,
            ..APP
        }],
        verify: no_gpu_render::verify,
        source: Source::Fixtures(&[no_gpu_render::FIXTURE]),
        window: Some(PANELLED),
        note: Some(
            "The launch passes `--no-gpu-render`, which refuses the photo surface's GPU stage before \
             the window opens, as a machine whose adapter cannot run it does: the stage's \
             capability check answers unavailable, the desktop hands it no plan, and every frame \
             is the reference renderer's. The window itself is still drawn by the adapter each \
             frame identifies.",
        ),
        own: None,
    },
    Scenario {
        name: zoom::SCENARIO,
        about: "Percentage and pinch zooms, pans and idle frames over the 24 MP and 60 MP JPEGs, one run each",
        launches: &[LaunchSpec {
            plan: zoom::plan,
            ..APP
        }],
        verify: zoom::verify,
        source: Source::Fixtures(&["fixtures/generated/24mp.jpg", "fixtures/generated/60mp.jpg"]),
        window: Some(PANELLED),
        note: None,
        own: Some(zoom::run),
    },
    Scenario {
        name: minify::SCENARIO,
        about: "Before/After at Fit over the generated zone plate: After's exact raster drawn below its size shows no replica rings, and its mip levels are in the photo slots",
        launches: &[LaunchSpec {
            plan: minify::plan,
            ..APP
        }],
        verify: minify::verify,
        source: Source::Fixtures(&[minify::FIXTURE]),
        window: Some(PANELLED),
        note: None,
        own: None,
    },
    Scenario {
        name: minify::TILED,
        about: "Before/After at Fit over the generated 60 MP JPEG, held in tiles that cannot have mip levels: After draws the display reduction, as the photograph was drawn before the comparison",
        launches: &[LaunchSpec {
            plan: minify::tiled_plan,
            ..APP
        }],
        verify: minify::verify_tiled,
        source: Source::Fixtures(&[minify::TILED_FIXTURE]),
        window: Some(PANELLED),
        note: None,
        own: None,
    },
    Scenario {
        name: viewport::REGION,
        about: "Masked, cropped 100% viewport draft on the GPU's region with its mask coverage, overlays, release, history and GPU draws",
        launches: &[LaunchSpec {
            plan: viewport::region_plan,
            ..APP
        }],
        verify: viewport::verify_region,
        source: Source::Fixtures(&["fixtures/generated/24mp.jpg"]),
        window: Some(PANELLED),
        note: None,
        own: None,
    },
    Scenario {
        name: viewport::IDLE,
        about: "100% to Fit without an evidence tick or frame capture during the idle interval",
        launches: &[LaunchSpec {
            plan: viewport::idle_plan,
            ..APP
        }],
        verify: viewport::verify_idle,
        source: Source::Fixtures(&["fixtures/generated/24mp.jpg"]),
        window: Some(PANELLED),
        note: None,
        own: None,
    },
    Scenario {
        name: "crop",
        about: "Crop actions, a draft cancelled and one applied",
        launches: &[LaunchSpec {
            plan: crop::plan,
            ..APP
        }],
        verify: crop::verify,
        source: Source::Fixtures(&[ORIENTATION_1]),
        window: Some(["1280", "800"]),
        note: None,
        own: None,
    },
    Scenario {
        name: "crop-draft",
        about: "The crop draft's gestures and controls at Fit and 100%",
        launches: &[LaunchSpec {
            plan: crop::draft_plan,
            ..APP
        }],
        verify: crop::verify_draft,
        source: Source::Fixtures(&[ORIENTATION_1]),
        window: Some(["1280", "800"]),
        note: None,
        own: None,
    },
    Scenario {
        name: "workspace",
        about: "Panels, canvas mode, thirds, a historical preview, an agent's conflicting commit and the palette",
        launches: &[LaunchSpec {
            plan: workspace::plan,
            ..APP
        }],
        verify: workspace::verify,
        source: Source::Fixtures(&[ORIENTATION_1]),
        window: Some(PANELLED),
        note: None,
        own: None,
    },
    Scenario {
        name: "information",
        about: "Information key and palette toggle, cached capture metadata, current/history/live crop dimensions, zoom and hidden panels, without photo uploads",
        launches: &[LaunchSpec {
            plan: information::plan,
            ..APP
        }],
        verify: information::verify,
        source: Source::Fixtures(&[information::FIXTURE]),
        window: Some(PANELLED),
        note: None,
        own: None,
    },
    Scenario {
        name: "basic",
        about: "The Exposure slider's whole gesture: draft, commit, typed value, undo, reset and an agent's conflicting commit",
        launches: &[LaunchSpec {
            plan: basic::plan,
            ..APP
        }],
        verify: basic::verify,
        source: Source::Fixtures(&[ORIENTATION_1]),
        window: Some(PANELLED),
        note: None,
        own: None,
    },
    Scenario {
        name: "basic-panel",
        about: "The whole Basic section, historical values, a group reset and the neutral picker",
        launches: &[LaunchSpec {
            plan: basic::panel_plan,
            ..APP
        }],
        verify: basic::verify_panel,
        source: Source::Fixtures(&["fixtures/s0/greyscale.jpg"]),
        window: Some(PANELLED),
        note: None,
        own: None,
    },
    Scenario {
        name: "basic-crop",
        about: "Basic composed with a crop and a straighten",
        launches: &[LaunchSpec {
            plan: histogram::crop_plan,
            ..APP
        }],
        verify: histogram::verify_crop,
        source: Source::Fixtures(&[ORIENTATION_1]),
        window: Some(PANELLED),
        note: None,
        own: None,
    },
    Scenario {
        name: "basic-restart",
        about: "A Basic edit committed in one launch and reopened in the next",
        launches: &[
            LaunchSpec {
                name: "launch1",
                script: "script1.json",
                plan: basic::restart_first,
                ..APP
            },
            LaunchSpec {
                name: "launch2",
                plan: basic::restart_second,
                catalog: Some("launch1"),
                ..APP
            },
        ],
        verify: basic::verify_restart,
        source: Source::Fixtures(&[ORIENTATION_1]),
        window: Some(PANELLED),
        note: Some(basic::RESTART_NOTE),
        own: None,
    },
    Scenario {
        name: "histogram",
        about: "The histogram, its clipping overlays, a render.sample of the edited pixel and a drafted frame",
        launches: &[LaunchSpec {
            plan: histogram::plan,
            // The scenario commits an `edit.set-pixel`, and the pixel proof is a test module.
            developer: true,
            ..APP
        }],
        verify: histogram::verify,
        source: Source::Fixtures(&[ORIENTATION_1]),
        window: Some(PANELLED),
        note: None,
        own: None,
    },
    Scenario {
        name: "presence",
        about: "Texture, Clarity and Dehaze over a generated gradient, edge, texture and flat field",
        launches: &[LaunchSpec {
            plan: presence::plan,
            ..APP
        }],
        verify: presence::verify,
        source: Source::Fixtures(&[presence::FIXTURE]),
        window: Some(PANELLED),
        note: None,
        own: None,
    },
    Scenario {
        name: "lens-perspective",
        about: "Tone curve and Detail with Lens/Perspective, warped masks, crop and exact native Undo pixels",
        launches: &[LaunchSpec {
            plan: lens::plan,
            deadline: Some(Duration::from_secs(310)),
            ..APP
        }],
        verify: lens::verify,
        source: Source::Fixtures(&[lens::FIXTURE]),
        window: Some(PANELLED),
        note: Some(
            "Exercises the generated metadata grid through the descriptor-backed profile list and real desktop messages. This is rendering evidence; photographic qualification uses supplied photographs.",
        ),
        own: None,
    },
    Scenario {
        name: "mixer",
        about: "The Colour mixer over a generated hue wheel",
        launches: &[LaunchSpec {
            plan: mixer::plan,
            ..APP
        }],
        verify: mixer::verify,
        source: Source::Fixtures(&[mixer::FIXTURE]),
        window: Some(PANELLED),
        note: None,
        own: None,
    },
    Scenario {
        name: "curve",
        about: "The Tone curve over a generated grey ramp and colour patches",
        launches: &[LaunchSpec {
            plan: curve::plan,
            ..APP
        }],
        verify: curve::verify,
        source: Source::Fixtures(&[curve::FIXTURE]),
        window: Some(PANELLED),
        note: None,
        own: None,
    },
    Scenario {
        name: "vignette",
        about: "Vignette amount, roundness and feather, a post-crop recentre and the reset",
        launches: &[LaunchSpec {
            plan: vignette::plan,
            ..APP
        }],
        verify: vignette::verify,
        source: Source::Fixtures(&[vignette::FIXTURE]),
        window: Some(PANELLED),
        note: None,
        own: None,
    },
    Scenario {
        name: "copy-settings",
        about: "Copy and paste, chooser, undo, previous, filmstrip and Select batch confirmation and reports",
        launches: &[],
        verify: crate::copy_settings_smoke::verify,
        source: Source::Default(&[]),
        window: Some(PANELLED),
        note: None,
        own: Some(crate::copy_settings_smoke::run),
    },
    Scenario {
        name: "presets",
        about: "Presets imported, applied, undone, created, listed and deleted",
        launches: &[LaunchSpec {
            plan: presets::plan,
            ..APP
        }],
        verify: presets::verify,
        source: Source::Fixtures(&[presets::FIXTURE]),
        window: Some(PANELLED),
        note: None,
        own: None,
    },
    Scenario {
        name: "export",
        about: "The Export menu, two GPU exports and a reference one written and read back, the GPU's within the display limit of the reference's, one GPU export repeated byte for byte, and a refused one",
        launches: &[LaunchSpec {
            plan: export::plan,
            ..APP
        }],
        verify: export::verify,
        source: Source::Fixtures(&[export::FIXTURE]),
        window: Some(PANELLED),
        note: None,
        own: None,
    },
    Scenario {
        name: mask::SCENARIO,
        about: "A linear mask drawn and edited through, then reopened in a second launch",
        launches: &[
            LaunchSpec {
                name: "launch1",
                script: "script1.json",
                plan: mask::launch1,
                ..APP
            },
            LaunchSpec {
                name: "launch2",
                script: "script2.json",
                plan: mask::launch2,
                catalog: Some("launch1"),
                ..APP
            },
        ],
        verify: mask::verify,
        source: Source::Fixtures(&[mask::FIXTURE]),
        window: Some(PANELLED),
        note: Some(mask::NOTE),
        own: None,
    },
    Scenario {
        name: mask_combine::SCENARIO,
        about: "Four radial components in three modes in one mask, its coverage read off the overlay",
        launches: &[LaunchSpec {
            name: "launch",
            script: "script.json",
            plan: mask_combine::plan,
            ..APP
        }],
        verify: mask_combine::verify,
        source: Source::Fixtures(&[mask_combine::FIXTURE]),
        window: Some(PANELLED),
        note: Some(mask_combine::NOTE),
        own: None,
    },
    Scenario {
        name: mask_brush::SCENARIO,
        about: "Brush strokes painted, erased and deleted, a subtracting brush, and painting over the edge, at 100% and under a rotated crop",
        launches: &[
            LaunchSpec {
                name: "launch1",
                script: "script1.json",
                plan: mask_brush::plan1,
                ..APP
            },
            LaunchSpec {
                name: "launch2",
                script: "script2.json",
                plan: mask_brush::plan2,
                catalog: Some("launch1"),
                ..APP
            },
            LaunchSpec {
                name: "launch3",
                script: "script3.json",
                plan: mask_brush::plan3,
                catalog: Some("launch1"),
                ..APP
            },
        ],
        verify: mask_brush::verify,
        source: Source::Fixtures(&[mask_brush::FIXTURE]),
        window: Some(PANELLED),
        note: Some(mask_brush::NOTE),
        own: None,
    },
    Scenario {
        name: mask_range::SCENARIO,
        about: "Luminance and colour range selections typed, picked and combined, then their limits",
        launches: &[
            LaunchSpec {
                name: "launch1",
                script: "script1.json",
                plan: mask_range::launch1_plan,
                ..APP
            },
            LaunchSpec {
                name: "launch2",
                script: "script2.json",
                plan: mask_range::launch2_plan,
                catalog: Some("launch1"),
                ..APP
            },
        ],
        verify: mask_range::verify,
        source: Source::Fixtures(&[mask_range::FIXTURE]),
        window: Some(PANELLED),
        note: Some(mask_range::NOTE),
        own: None,
    },
    Scenario {
        name: mask_panel::SCENARIO,
        about: "The Masks panel's states over the mask-mode board's three masks, ending on its radial drag",
        launches: &[LaunchSpec {
            name: "launch",
            script: "script.json",
            plan: mask_panel::plan,
            ..APP
        }],
        verify: mask_panel::verify,
        source: Source::Fixtures(&[mask_panel::FIXTURE]),
        window: Some(PANELLED),
        note: Some(mask_panel::NOTE),
        own: None,
    },
    Scenario {
        name: mask_interactions::SCENARIO,
        about: "Unplaced creation, live coverage, explicit hiding and coherent brush selection",
        launches: &[LaunchSpec {
            name: "launch",
            script: "script.json",
            plan: mask_interactions::plan,
            ..APP
        }],
        verify: mask_interactions::verify,
        source: Source::Fixtures(&[mask_interactions::FIXTURE]),
        window: Some(PANELLED),
        note: Some(mask_interactions::NOTE),
        own: None,
    },
    Scenario {
        name: performance::SCENARIO,
        about: "The Performance section over a 60 MP heavy edit and its reference export, listed running and finished, its memory held to the runner's own readings on idle frames",
        launches: &[LaunchSpec {
            plan: performance::plan,
            watch: Some((performance::READINGS, performance::watch)),
            // This functional scenario waits for a 60 MP reference export of the heavy stack and fixed
            // sampling windows. Let the editor's 60-second script deadline report a failure
            // before the parent reaps it; latency budgets belong to the quiet-host timing tier.
            deadline: Some(Duration::from_secs(75)),
            ..APP
        }],
        verify: performance::verify,
        source: Source::Default(&[performance::FIXTURE]),
        window: Some(PANELLED),
        note: None,
        own: None,
    },
    Scenario {
        name: "settings",
        about: "The Settings sheet's Experiments tab: every flag kind changed, refused, reset and read back; the General rows: the canvas background drawn, the interface size scaling the title bar, the mask overlay colour, the lens switch and a catalog folder's relaunch note",
        launches: &[LaunchSpec {
            plan: settings::plan,
            // The proof flags, which the choice and number controls are checked on, are listed
            // only by a host that serves the test modules.
            developer: true,
            ..APP
        }],
        verify: settings::verify,
        source: Source::Fixtures(&[ORIENTATION_1]),
        window: Some(PANELLED),
        note: None,
        // The catalog step names a folder inside the launch's own evidence directory.
        own: Some(settings::run),
    },
    Scenario {
        name: theme::SCENARIO,
        about: "UI themes end to end: the synthetic Omarchy set imported through the Appearance tab, each theme's outcome listed; Luxforge Dark, Nord and both imports drawn and sampled against their tokens, the surround neutral, the photograph's pixels and an export's bytes the same under every theme, Grey kept under a light theme, and a second client switching back",
        launches: &[LaunchSpec {
            plan: theme::plan,
            ..APP
        }],
        verify: theme::verify,
        source: Source::Fixtures(&[theme::FIXTURE]),
        window: Some(PANELLED),
        note: None,
        own: None,
    },
    Scenario {
        name: "gallery",
        about: "All 129 widget gallery states across nineteen pages",
        launches: &[LaunchSpec {
            plan: gallery::plan,
            developer: true,
            ..APP
        }],
        verify: gallery::verify,
        source: Source::Fixtures(&[ORIENTATION_1]),
        window: Some(gallery::WINDOW),
        note: None,
        own: None,
    },
    Scenario {
        name: "controls",
        about: "The developer proof's generated controls and identity layer",
        launches: &[LaunchSpec {
            plan: controls::plan,
            developer: true,
            ..APP
        }],
        verify: controls::verify,
        source: Source::Fixtures(&[ORIENTATION_1]),
        window: Some(PANELLED),
        note: None,
        own: None,
    },
    Scenario {
        name: "capabilities",
        about: "Module capabilities against a loopback proof endpoint in this process",
        // Its one launch's plan names the endpoint and the sentinel its own run makes.
        launches: &[],
        verify: capabilities::verify,
        source: Source::Fixtures(&[ORIENTATION_1]),
        window: Some(PANELLED),
        note: Some(capabilities::NOTE),
        own: Some(capabilities::run),
    },
    Scenario {
        name: select::SCENARIO,
        about: "The Select workspace over a generated catalog: G, an event's grouped grid, arrow selection, the Group chip, an agent's pick read again, a first look's progress sheet, Continue in background and Cancel, a folder of real images, and back to Develop",
        launches: &[],
        verify: select::verify,
        source: Source::Fixtures(&[]),
        window: Some(PANELLED),
        note: Some(select::NOTE),
        own: Some(select::run),
    },
    Scenario {
        name: resolve_missing::SCENARIO,
        about: "Missing originals over reorganized originals on a disk image: the groups and their reasons, a search's per-row results, Choose…, Stop search changing nothing, Relink of exactly the verified pairs, and Locate…",
        launches: &[],
        verify: resolve_missing::verify,
        source: Source::Fixtures(&[]),
        window: Some(PANELLED),
        note: Some(resolve_missing::NOTE),
        own: Some(resolve_missing::run),
    },
    Scenario {
        name: develop_picks::SCENARIO,
        about: "Picks on a camera card developed through Develop N's confirmation: an existing folder chosen, a name typed, Escape changing nothing, two picks' copies in an indexed folder used, Develop opened on what it developed and the next photograph's cached preview drawn in the frame after the key",
        launches: &[],
        verify: develop_picks::verify,
        source: Source::Fixtures(&[]),
        window: Some(PANELLED),
        note: Some(develop_picks::NOTE),
        own: Some(develop_picks::run),
    },
    Scenario {
        name: filmstrip::SCENARIO,
        about: "Develop's filmstrip over a catalog view of JPEG photographs and, with --source, a RAW one: each move draws the photograph's own cached preview in the frame after the key, then its render",
        launches: &[],
        verify: filmstrip::verify,
        source: Source::Default(&[]),
        window: Some(PANELLED),
        note: Some(filmstrip::NOTE),
        own: Some(filmstrip::run),
    },
    Scenario {
        name: loupe::SCENARIO,
        about: "The Select loupe over a folder of generated images: a burst stepped, jumped and left for the moments either side, a bracket, the 100% focus check, compare, P on the bracket and P on the burst moving on to the next moment, each picture its own frame's",
        launches: &[],
        verify: loupe::verify,
        source: Source::Fixtures(&[]),
        window: Some(PANELLED),
        note: Some(loupe::NOTE),
        own: Some(loupe::run),
    },
    Scenario {
        name: "unavailable",
        about: "A committed crop reopened with the crop module disabled: reported, never omitted",
        launches: &[
            LaunchSpec {
                name: "launch1",
                script: "script1.json",
                plan: workspace::unavailable_first,
                ..APP
            },
            LaunchSpec {
                name: "launch2",
                plan: workspace::unavailable_second,
                catalog: Some("launch1"),
                disable: &["luxforge.crop"],
                ..APP
            },
        ],
        verify: workspace::verify_unavailable,
        source: Source::Fixtures(&[ORIENTATION_1]),
        window: Some(PANELLED),
        note: Some(workspace::UNAVAILABLE_NOTE),
        own: None,
    },
    Scenario {
        name: raw_panel::SCENARIO,
        about: "The RAW section, double-click resets and a crop over a supplied RAW file",
        launches: &[LaunchSpec {
            plan: raw_panel::plan,
            ..APP
        }],
        verify: raw_panel::verify,
        source: Source::Supplied { listed: false },
        window: Some(PANELLED),
        note: None,
        own: None,
    },
    Scenario {
        name: look::SCENARIO,
        about: "The RAW look over a supplied RAW file: Standard in the new photograph's Original, Neutral and Standard through the Look section above Basic, an Amount drag, the reset and a double-click, Neutral through the API",
        launches: &[LaunchSpec {
            plan: look::plan,
            ..APP
        }],
        verify: look::verify,
        source: Source::Supplied { listed: false },
        window: Some(PANELLED),
        note: None,
        own: None,
    },
    Scenario {
        name: raw_editor::SCENARIO,
        about: "RAW exposure, gains, white balance, neutral pick, geometry, undo and history over a manifest-listed RAW file, then a reopen",
        launches: &[
            LaunchSpec {
                name: raw_editor::EDIT,
                plan: raw_editor::edit_plan,
                deadline: Some(raw_editor::DEADLINE),
                ..APP
            },
            LaunchSpec {
                name: raw_editor::REOPEN,
                plan: raw_editor::reopen_plan,
                catalog: Some(raw_editor::EDIT),
                deadline: Some(raw_editor::DEADLINE),
                ..APP
            },
        ],
        verify: raw_editor::verify,
        source: Source::Supplied { listed: true },
        window: None,
        note: Some(raw_editor::NOTE),
        own: Some(raw_editor::run),
    },
];

/// The row named `name`.
pub fn find(name: &str) -> Result<&'static Scenario> {
    SCENARIOS
        .iter()
        .find(|scenario| scenario.name == name)
        .ok_or_else(|| format!("Unknown smoke scenario {name:?}; `smoke --list` names them").into())
}

/// `smoke --list`: every row, in table order.
pub fn list(root: &Path) -> String {
    let mut text = String::new();
    for scenario in SCENARIOS {
        let sources = scenario.sources(root, None).unwrap_or_default();
        let launches = if scenario.launches.is_empty() {
            "its own".to_owned()
        } else {
            scenario
                .launches
                .iter()
                .map(|launch| {
                    let frames = (launch.plan)(&sources).len();
                    format!(
                        "{} ({frames} frame{})",
                        launch.name,
                        if frames == 1 { "" } else { "s" }
                    )
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        let source = match scenario.source {
            Source::Fixtures([]) => "nothing".to_owned(),
            Source::Fixtures(fixtures) => fixtures.join(" "),
            Source::Default(fixtures) => format!("{} or --source", fixtures.join(" ")),
            Source::Supplied { listed: false } => "--source RAW (not in rendered)".to_owned(),
            Source::Supplied { listed: true } => {
                "--source RAW and a --manifest listing it (not in rendered)".to_owned()
            }
        };
        let window = scenario
            .window
            .map_or("default".to_owned(), |[width, height]| {
                format!("{width}x{height}")
            });
        text.push_str(&format!(
            "{}\n    {}\n    launches: {launches}; opens: {source}; window: {window}\n",
            scenario.name, scenario.about
        ));
    }
    text
}

fn execute(run: Run, scenario: &'static Scenario, sources: Vec<PathBuf>) -> Result {
    match scenario.own {
        Some(own) => own(run, scenario, sources),
        None => launch_all(run, scenario, sources),
    }
}

/// Run one scenario into `out`, over its own fixtures or, for a scenario that takes one, the
/// photograph `--source` names, with the RAW manifest `--manifest` names when its source is listed.
pub fn dispatch(
    root: &Path,
    out: &Path,
    name: &str,
    bin: &Path,
    timeout: Duration,
    source: Option<Vec<PathBuf>>,
    manifest: Option<&Path>,
) -> Result {
    let scenario = find(name)?;
    let sources = scenario.sources(root, source)?;
    ensure(
        scenario.listed() == manifest.is_some(),
        if scenario.listed() {
            format!("The {name} scenario needs --manifest FILE listing its source")
        } else {
            format!("--manifest is only for {}", listed().join(" and "))
        },
    )?;
    if let Some(manifest) = manifest {
        ensure(
            manifest.is_file(),
            format!("{} is missing", manifest.display()),
        )?;
    }
    let run = Run::start(root, out, name, bin, timeout)?;
    if let Some(manifest) = manifest {
        fs::copy(manifest, run.out().join(MANIFEST))?;
    }
    execute(run, scenario, sources)
}

/// The scenarios `--manifest` must be given to.
fn listed() -> Vec<&'static str> {
    SCENARIOS
        .iter()
        .filter(|scenario| scenario.listed())
        .map(|scenario| scenario.name)
        .collect()
}

/// Rerun a recorded run's checks without launching: `recorded` is copied into `out` and the
/// scenario's own code runs over the copy, each launch being the one recorded there. `source`
/// names a `--source` run's source again.
pub fn verify_only(
    root: &Path,
    recorded: &Path,
    out: &Path,
    name: &str,
    source: Option<Vec<PathBuf>>,
) -> Result {
    let scenario = find(name)?;
    let sources = scenario.sources(root, source)?;
    execute(Run::replay(root, recorded, out, name)?, scenario, sources)
}

/// Every launch of `scenario` over `sources`, in order, each checked against its plan as soon as
/// it exits, then the scenario's own checks over all of them.
pub fn launch_all(run: Run, scenario: &Scenario, sources: Vec<PathBuf>) -> Result {
    launch_planned(run, scenario, sources, |_, sources| {
        Ok(scenario
            .launches
            .iter()
            .map(|spec| (spec.plan)(sources))
            .collect())
    })
}

/// [`launch_all`] with each launch's plan from `plans`, which is given the run once its sources
/// are hashed: a row whose plans read more than its sources, as `raw-editor`'s read its source's
/// manifest entry, plans in its own function and launches through this one.
pub fn launch_planned(
    mut run: Run,
    scenario: &Scenario,
    sources: Vec<PathBuf>,
    plans: impl FnOnce(&mut Run, &[PathBuf]) -> Result<Vec<Plan>>,
) -> Result {
    if let Some(note) = scenario.note {
        run.note(note);
    }
    run.check(|run| {
        for source in &sources {
            ensure(
                source.is_file(),
                if source.starts_with(run.root().join("fixtures/generated")) {
                    format!(
                        "{} is missing; run `cargo xtask generate-fixtures --output fixtures/generated`",
                        source.display()
                    )
                } else {
                    format!("{} is missing", source.display())
                },
            )?;
        }
        run.hash(&sources)?;
        let plans = plans(run, &sources)?;
        ensure(
            plans.len() == scenario.launches.len(),
            "A plan for every launch",
        )?;
        let mut checked = Vec::with_capacity(scenario.launches.len());
        for (spec, plan) in scenario.launches.iter().zip(plans) {
            if let Some(earlier) = spec.catalog {
                let catalog = run.out().join(earlier).join("catalog.sqlite");
                ensure(catalog.is_file(), format!("Launch {earlier} wrote no catalog"))?;
            }
            let evidence = run
                .launch(launch_of(scenario, spec, &plan, &sources, run.out()))?
                .dir;
            checked.push(plan.check(&evidence)?);
        }
        (scenario.verify)(run, &checked)?;
        run.sources_unchanged()?;
        // The adapter that drew the run, identified as wgpu describes it: its device type tells a
        // software rasterizer (`Cpu`, lavapipe on Linux CI) from a GPU, so the run says which it
        // ran on. Each launch's own is in `launches`.
        let backend = checked
            .last()
            .and_then(|launch| launch.frames.last())
            .map_or(Value::Null, |frame| frame.state()["backend"].clone());
        ensure(
            backend["device_type"]
                .as_str()
                .is_some_and(|kind| !kind.is_empty()),
            format!("The run recorded no identified adapter: {backend}"),
        )?;
        run.record("backend", backend);
        Ok(())
    })
}

/// The launch `spec` makes over `sources` in a run whose output directory is `out`.
fn launch_of(
    scenario: &Scenario,
    spec: &LaunchSpec,
    plan: &Plan,
    sources: &[PathBuf],
    out: &Path,
) -> Launch {
    let mut launch = if spec.name == APP.name {
        Launch::app()
    } else {
        Launch::named(spec.name)
    };
    if let Some(earlier) = spec.catalog {
        launch = launch.catalog(&out.join(earlier).join("catalog.sqlite"));
    }
    for module in spec.disable {
        launch = launch.disable(module);
    }
    if spec.gpu_identity {
        launch = launch.gpu_identity();
    }
    if spec.no_gpu_render {
        launch = launch.no_gpu_render();
    }
    if spec.developer {
        launch = launch.developer();
    }
    launch = launch.open_all(sources);
    if plan.scripted() {
        launch = launch.script(spec.script, plan.script());
    }
    if let Some(window) = scenario.window {
        launch = launch.window(window);
    }
    if let Some((file, watch)) = spec.watch {
        launch = launch.watch(Box::new(watch)).keep(file);
    }
    if let Some(deadline) = spec.deadline {
        launch = launch.deadline(deadline);
    }
    launch
}

/// A launch that opens each source in turn, one frame per open.
fn opens(sources: &[PathBuf]) -> Plan {
    Plan::new(
        (1..=sources.len())
            .map(|number| Step::opened(format!("open-{number}")).label("Original"))
            .collect(),
    )
}

/// The checks of the scenarios that only open files: each frame is the open it follows, ready or
/// failed as its plan says, the photograph the fixture at its own orientation and size, displayed
/// and drawn from a new upload or explicitly reused current pixels; the photo-sized ones report
/// the proxy's own render time.
fn plain(run: &mut Run, launches: &[Checked]) -> Result {
    plain_checks(run.scenario(), &launches[0])
}

fn plain_checks(scenario: &str, launch: &Checked) -> Result {
    let empty = scenario == "empty";
    for (index, frame) in launch.frames.iter().enumerate() {
        let state = frame.state();
        let generation = if empty { 0 } else { index + 1 };
        let orientation =
            if scenario.starts_with("large") || (scenario == "alternating" && index % 2 == 1) {
                1
            } else {
                6
            };
        ensure(
            state["requested_generation"] == generation,
            "Wrong requested generation",
        )?;
        if matches!(scenario, "empty" | "invalid") {
            ensure(
                state["phase"] == if empty { "empty" } else { "error" }
                    && state["displayed_generation"] == 0,
                "Wrong empty/error state",
            )?;
            let colors: std::collections::BTreeSet<_> = frame
                .image()?
                .pixels()
                .map(|p| p.0)
                .take(20_000_000)
                .collect();
            ensure(colors.len() > 10, "Blank empty UI")?;
        } else {
            let displayed = if matches!(scenario, "repeated" | "alternating") {
                generation
            } else {
                1
            };
            ensure(
                state["displayed_generation"] == displayed,
                "Stale displayed image",
            )?;
            let dims = match scenario {
                "large24" => [6000, 4000],
                "large60" => [10000, 6000],
                _ if orientation == 1 => [480, 320],
                _ => [320, 480],
            };
            ensure(
                state["source_dimensions"] == json!(dims),
                "Wrong dimensions",
            )?;
            let failed = scenario == "replacement" && index == 1;
            ensure(
                state["phase"] == if failed { "error" } else { "ready" },
                "Wrong phase",
            )?;
            frame.fixture(Fixture {
                aspect: match scenario {
                    "large24" => Some(1.5),
                    "large60" => Some(5.0 / 3.0),
                    _ => None,
                },
                ..Fixture::fit(orientation)
            })?;
            let uploaded = launch.events.iter().any(|e| {
                e["event"] == "render_ready" && e["generation"] == state["displayed_generation"]
            });
            let reused = scenario == "repeated"
                && index > 0
                && launch.events.iter().any(|e| {
                    e["event"] == "preview_pixels_reused"
                        && e["generation"] == state["displayed_generation"]
                        && e["detail"]["generation"] == state["surface"]["generation"]
                        && e["detail"]["identity"]["entry_id"]
                            == state["histogram"]["identity"]["entry"]
                });
            ensure(uploaded || reused, "Missing current photograph readiness")?;
            if reused {
                let first = launch.frames[0].state();
                let (gpu, first_gpu) = (&state["surface"]["gpu"], &first["surface"]["gpu"]);
                // The unchanged picture is drawn: where the GPU draws it at rest, the view plan
                // over the boundary the first open drew, ready and with no CPU frame under it;
                // otherwise the CPU frame of the unchanged version.
                let drawn = if gpu["drawing_path"] == json!("gpu") {
                    !gpu["drawn_gpu_boundary"].is_null()
                        && gpu["drawn_gpu_boundary"] == first_gpu["drawn_gpu_boundary"]
                        && gpu["gpu_ready_boundary"] == gpu["drawn_gpu_boundary"]
                        && gpu["drawn_full_version"].is_null()
                } else {
                    gpu["drawn_full_version"] == state["surface"]["version"]
                };
                ensure(
                    state["surface"]["version"] == first["surface"]["version"]
                        && gpu["upload_bytes"] == first_gpu["upload_bytes"]
                        && drawn
                        && state["surface"]["gpu"]["drawn_photo_blank"] == json!(false)
                        && state["surface"]["gpu"]["drawn_stale_photo"] == json!(false),
                    "Reopening reused pixels without the unchanged current photograph drawn",
                )?;
            }
        }
    }
    if scenario.starts_with("large") {
        // A photo-sized source at Fit is drawn at rest by the GPU, the stack at full resolution in
        // tiles reduced to the view, and the status bar names the GPU's render. A capture can land
        // before its tiles are in, while the photograph is still the reference's exact frame
        // reduced to the view, whose own render time the bar then gives and calls exact, or says
        // "Rendering…" while a refit or the reference frame runs; the figure behind it is still
        // recorded.
        let record = expect_render_times(&launch.events, &launch.frames)?;
        ensure(
            launch.frames.iter().all(|frame| {
                let state = frame.state();
                let bar = &state["status_bar"];
                if gpu_at_rest(state) {
                    bar["render"].as_str().is_some_and(names_gpu_render)
                } else {
                    bar["render"].as_str().is_some_and(|text| {
                        (text.starts_with("Exact render") && state["reference"]["reduced"] == true)
                            || text == "Rendering\u{2026}"
                    })
                }
            }),
            "A photo-sized frame at Fit reports neither the GPU's render nor the reference's reduction",
        )?;
        let pictures: Vec<Value> = launch
            .frames
            .iter()
            .map(|frame| {
                let state = frame.state();
                json!({"frame": frame["file"], "picture": state["surface"]["gpu"]["picture"],
                    "render": state["status_bar"]["render"], "rest": state["surface"]["gpu"]["rest"]})
            })
            .collect();
        Checks::new().write(
            &launch.evidence,
            scenario,
            json!({"render_times": record, "pictures": pictures}),
        )?;
    }
    Ok(())
}

/// The scenario that draws its photograph through the GPU preview stage.
const GPU_IDENTITY: &str = "gpu-identity";

/// The GPU-preview budget the editor records, its recorded default.
const GPU_PREVIEW_BUDGET: u64 = 2 * 1024 * 1024 * 1024;

/// The open's frame of each source.
fn gpu_identity_plan(sources: &[PathBuf]) -> Plan {
    Plan::new(
        (1..=sources.len())
            .map(|number| Step::opened(format!("open-{number}")).label("Original"))
            .collect(),
    )
}

/// `load`'s checks — the fixture at its orientation, size and colours, placed at Fit — over frames
/// the GPU stage drew: the identity program over a boundary held from the frame on screen, so the
/// fixture's own colours are the stage's output. Each such frame's state records the hook, the GPU
/// drawing path with no fallback, the boundary of the frame on screen with the CPU frame itself not
/// drawn, at least one encoded pass, GPU-preview figures within the recorded budget, and the status
/// bar's "GPU preview · N ms" with the frame's own figure.
fn gpu_identity(run: &mut Run, launches: &[Checked]) -> Result {
    let launch = &launches[0];
    // Every frame is the one fixture, opened once, at its orientation, size and colours at Fit,
    // whichever path drew it.
    for frame in &launch.frames {
        let state = frame.state();
        ensure(
            state["requested_generation"] == json!(1)
                && state["displayed_generation"] == json!(1)
                && state["phase"] == json!("ready")
                && state["source_dimensions"] == json!([320, 480]),
            format!(
                "{} is not the one open, ready at 320 × 480: requested {}, displayed {}, {} {}",
                frame["file"],
                state["requested_generation"],
                state["displayed_generation"],
                state["phase"],
                state["source_dimensions"]
            ),
        )?;
        frame.fixture(Fixture::fit(6))?;
    }
    let mut checks = Checks::new();
    for frame in &launch.frames {
        let surface = &frame.state()["surface"];
        let gpu = &surface["gpu"];
        let bar = &frame.state()["status_bar"];
        let figure = |name: &str| gpu[name].as_u64().unwrap_or(0);
        checks.note(
            frame,
            "the photograph drawn by the GPU stage",
            json!({
                "gpu_identity": gpu["gpu_identity"],
                "drawing_path": gpu["drawing_path"],
                "gpu_fallback": gpu["gpu_fallback"],
                "plan_fallback": gpu["plan_fallback"],
                "drawn_gpu_boundary": gpu["drawn_gpu_boundary"],
                "surface_version": surface["version"],
                "drawn_full_version": gpu["drawn_full_version"],
                "gpu_preview_passes": gpu["gpu_preview_passes"],
                "gpu_preview_budget_bytes": gpu["gpu_preview_budget_bytes"],
                "gpu_preview_in_use_bytes": gpu["gpu_preview_in_use_bytes"],
                "gpu_preview_scratch_bytes": gpu["gpu_preview_scratch_bytes"],
                "gpu_preview_peak_bytes": gpu["gpu_preview_peak_bytes"],
                "gpu_preview_frame_us": gpu["gpu_preview_frame_us"],
                "gpu_preview_done_us": gpu["gpu_preview_done_us"],
                "full_resident_bytes": gpu["full_resident_bytes"],
                "render": bar["render"],
                "gpu_ms": bar["gpu_ms"],
            }),
        );
        // The status bar names the GPU frame, with that frame's own figure.
        let ms = bar["gpu_ms"].as_f64().unwrap_or(f64::NAN);
        ensure(
            ms.is_finite()
                && (0.0..RENDER_MS_BOUND).contains(&ms)
                && gpu["gpu_preview_frame_us"].as_f64().map(|us| us / 1000.0) == Some(ms)
                && bar["render"] == json!(frame_gpu_text(ms, frame.state()))
                && gpu["plan_fallback"].is_null(),
            format!(
                "{}: the status bar says {} for a GPU frame of {} µs (status figure {})",
                frame["file"], bar["render"], gpu["gpu_preview_frame_us"], bar["gpu_ms"]
            ),
        )?;
        ensure(
            gpu["gpu_identity"] == json!(true)
                && gpu["drawing_path"] == json!("gpu")
                && gpu["gpu_fallback"].is_null(),
            format!(
                "The GPU identity frame was not drawn by the GPU stage: path {}, fallback {}",
                gpu["drawing_path"], gpu["gpu_fallback"]
            ),
        )?;
        ensure(
            gpu["drawn_gpu_boundary"] == surface["version"] && gpu["drawn_full_version"].is_null(),
            format!(
                "The GPU stage drew boundary {} over surface version {}, with CPU frame {} drawn",
                gpu["drawn_gpu_boundary"], surface["version"], gpu["drawn_full_version"]
            ),
        )?;
        let (budget, in_use, peak) = (
            figure("gpu_preview_budget_bytes"),
            figure("gpu_preview_in_use_bytes"),
            figure("gpu_preview_peak_bytes"),
        );
        // The slots' shared scratch pools are part of what is in use.
        let scratch = gpu["gpu_preview_scratch_bytes"].as_u64();
        ensure(
            budget == GPU_PREVIEW_BUDGET
                && in_use > 0
                && in_use <= peak
                && peak <= budget
                && scratch.is_some_and(|scratch| scratch <= in_use)
                && figure("gpu_preview_passes") > 0,
            format!(
                "GPU-preview figures out of bounds: {in_use} in use, {scratch:?} scratch, {peak} \
                 peak, {budget} budget, {} passes",
                gpu["gpu_preview_passes"]
            ),
        )?;
    }
    checks.write(
        &launch.evidence,
        run.scenario(),
        json!({"budget_bytes": GPU_PREVIEW_BUDGET}),
    )
}

/// Check an `empty` launch's evidence made elsewhere, as `measure` does for its empty-shell
/// launches.
pub fn check_empty(evidence: &Path) -> Result {
    let scenario = find("empty")?;
    let launch = (scenario.launches[0].plan)(&[]).check(evidence)?;
    plain_checks(scenario.name, &launch)
}

/// The longest plausible render of one preview phase on the fixtures a scenario opens, in
/// milliseconds. A release render of the 60 MP fixture's exact phase is well under a second; the
/// bound exists to catch a figure that is not a render time at all, such as the time since the last
/// request, which grows with the length of the run.
pub const RENDER_MS_BOUND: f64 = 5000.0;

/// The status bar's wording of one frame's render time, exactly as the editor's
/// `state::status::RenderTime` formats it, so a captured frame's text is checked against its own
/// figure rather than against a copy of the text. `approximate` is a frame that approximates a
/// drafted RAW white balance, which reads as an approximate render.
pub fn render_text(ms: f64, approximate: bool) -> String {
    let kind = if approximate { "Approximate" } else { "Exact" };
    format!("{kind} render \u{b7} {}", render_figure(ms))
}

/// The status bar's wording of a GPU frame's figure, exactly as the editor's
/// `state::status::gpu_text` formats it: "GPU render" for the committed stack at rest, "GPU
/// preview" for a gesture's frame.
pub fn gpu_text(ms: f64, at_rest: bool) -> String {
    let kind = if at_rest { "GPU render" } else { "GPU preview" };
    format!("{kind} \u{b7} {}", render_figure(ms))
}

/// The status bar's wording of a GPU frame in a captured frame's `state`: [`gpu_text`] for its
/// picture, after "Software " while the session's renderer is the GPU on a software adapter
/// (`--software-adapter`), as the editor's status bar says it.
pub fn frame_gpu_text(ms: f64, state: &Value) -> String {
    let text = gpu_text(ms, gpu_at_rest(state));
    if state["renderer"]["software"] == json!(true) {
        format!("Software {text}")
    } else {
        text
    }
}

/// Whether a render slot's `text` names the GPU's picture at rest, on any adapter.
pub fn names_gpu_render(text: &str) -> bool {
    text.trim_start_matches("Software ")
        .starts_with("GPU render")
}

/// Whether a captured frame's photograph is the GPU's picture of the committed stack at rest,
/// as its `state.surface.gpu.picture` names it: its picture at rest in tiles or its view plan.
pub fn gpu_at_rest(state: &Value) -> bool {
    matches!(
        state["surface"]["gpu"]["picture"].as_str(),
        Some("rest" | "view")
    )
}

/// A figure as the status bar's render slot gives it.
fn render_figure(ms: f64) -> String {
    if ms < 0.5 {
        "<1 ms".to_owned()
    } else if ms.round() >= 1000.0 {
        format!("{:.1} s", ms / 1000.0)
    } else {
        format!("{} ms", ms.round() as i64)
    }
}

/// Every presented frame's render time, from its `preview_displayed` event: the preview worker's
/// own time for the phase on screen. Each must be a finite number of milliseconds in
/// `0..RENDER_MS_BOUND`, and there must be at least one. Then, for every captured frame, the status
/// bar either says the renderer is busy or states a figure that one of those events carried, in
/// exactly the editor's wording, with the correlated proxy flag exactly when the frame on screen is
/// the proxy and the approximate flag exactly when it approximates a drafted RAW white balance, and
/// `Approximate render` exactly when either is set. Returns the evidence record.
pub fn expect_render_times<F: Borrow<Value>>(events: &[Value], frames: &[F]) -> Result<Value> {
    let mut displayed = Vec::new();
    for event in events.iter().filter(|e| e["event"] == "preview_displayed") {
        let detail = &event["detail"];
        // A committed stack the GPU presents with no CPU render has no worker time of its own:
        // its frames name the interface thread's time to draw it, checked below.
        if detail["path"] == "gpu" && detail["render_ms"].is_null() {
            displayed.push(json!({"generation":detail["generation"],"path":"gpu"}));
            continue;
        }
        let ms = detail["render_ms"]
            .as_f64()
            .ok_or_else(|| format!("A preview_displayed event carries no render_ms: {detail}"))?;
        ensure(
            ms.is_finite() && (0.0..RENDER_MS_BOUND).contains(&ms),
            format!(
                "Generation {} reports a render of {ms} ms, outside 0..{RENDER_MS_BOUND}",
                detail["generation"]
            ),
        )?;
        displayed.push(json!({"generation":detail["generation"],"reduced":detail["reduced"],"reason":detail["reason"],"render_ms":ms}));
    }
    ensure(
        !displayed.is_empty(),
        "No preview_displayed event: nothing reported a render time",
    )?;
    let figures: Vec<f64> = displayed
        .iter()
        .filter_map(|d| d["render_ms"].as_f64())
        .collect();
    let mut shown = Vec::new();
    for frame in frames {
        let frame: &Value = frame.borrow();
        let bar = &frame["state"]["status_bar"];
        let text = bar["render"]
            .as_str()
            .ok_or("A frame records no status bar render text")?;
        if text == "Rendering\u{2026}" {
            shown.push(json!({"frame":frame["file"],"render":text}));
            continue;
        }
        // A frame the GPU preview drew names the interface thread's time to prepare it, not a
        // render's: a drag drawn on the GPU from its first tick.
        if let Some(gpu_ms) = bar["gpu_ms"].as_f64() {
            ensure(
                text == frame_gpu_text(gpu_ms, &frame["state"]),
                format!(
                    "{}: the status bar says {text:?} for a GPU frame of {gpu_ms} ms",
                    frame["file"]
                ),
            )?;
            shown.push(json!({"frame":frame["file"],"render":text,"gpu_ms":gpu_ms}));
            continue;
        }
        let Some(ms) = bar["render_ms"].as_f64() else {
            ensure(
                text == "Idle",
                format!(
                    "{}: the status bar says {text:?} with no render time",
                    frame["file"]
                ),
            )?;
            continue;
        };
        ensure(
            figures.contains(&ms),
            format!(
                "{}: the status bar's {ms} ms is no presented frame's own render time",
                frame["file"]
            ),
        )?;
        let approximate = bar["render_approximate"] == json!(true);
        ensure(
            approximate == (frame["state"]["approximate_white_balance"] == json!(true)),
            format!(
                "{}: the status bar's approximate label disagrees with the frame on screen",
                frame["file"]
            ),
        )?;
        ensure(
            text == render_text(ms, approximate),
            format!(
                "{}: the status bar says {text:?} for {ms} ms",
                frame["file"]
            ),
        )?;
        shown.push(
            json!({"frame":frame["file"],"render":text,"render_ms":ms,"approximate":approximate}),
        );
    }
    Ok(json!({"bound_ms":RENDER_MS_BOUND,"preview_displayed":displayed,"status_bar":shown}))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The render-time check accepts a presented frame's own figure in the editor's wording and
    /// refuses what the status bar used to show: a figure that is the time since the last request,
    /// a missing one, and a status bar that states a figure no frame reported.
    #[test]
    fn render_times_must_be_each_frames_own_and_plausible() {
        let displayed = |ms: Value| json!({"event":"preview_displayed","detail":{"generation":2,"reduced":true,"render_ms":ms}});
        let frame = |render: &str, ms: f64, approximate: bool| json!({"file":"frame-1.png","state":{"approximate_white_balance":approximate,"status_bar":{"render":render,"render_ms":ms,"render_approximate":approximate}}});
        assert_eq!(render_text(12.4, false), "Exact render \u{b7} 12 ms");
        assert_eq!(render_text(0.3, false), "Exact render \u{b7} <1 ms");
        assert_eq!(render_text(1234.0, false), "Exact render \u{b7} 1.2 s");
        assert_eq!(render_text(9.2, true), "Approximate render \u{b7} 9 ms");
        assert_eq!(render_text(140.0, true), "Approximate render \u{b7} 140 ms");
        assert_eq!(gpu_text(2.4, false), "GPU preview \u{b7} 2 ms");
        assert_eq!(gpu_text(0.2, false), "GPU preview \u{b7} <1 ms");
        assert_eq!(gpu_text(12.4, true), "GPU render \u{b7} 12 ms");
        let software = json!({"renderer": {"record": "gpu", "reason": null, "software": true},
            "surface": {"gpu": {"picture": "rest"}}});
        assert_eq!(
            frame_gpu_text(12.4, &software),
            "Software GPU render \u{b7} 12 ms"
        );
        assert_eq!(
            frame_gpu_text(2.4, &json!({"renderer": {"record": "gpu", "reason": null}})),
            "GPU preview \u{b7} 2 ms"
        );
        assert!(names_gpu_render("Software GPU render \u{b7} 12 ms"));
        assert!(names_gpu_render("GPU render \u{b7} 12 ms"));
        assert!(!names_gpu_render("Software GPU preview \u{b7} 12 ms"));
        let good = expect_render_times(
            &[displayed(json!(12.4))],
            &[frame("Exact render \u{b7} 12 ms", 12.4, false)],
        );
        assert!(good.is_ok(), "{good:?}");
        // The old figure: half a million milliseconds since the open.
        let none: [Value; 0] = [];
        assert!(expect_render_times(&[displayed(json!(500_000.0))], &none).is_err());
        assert!(expect_render_times(&[displayed(Value::Null)], &none).is_err());
        assert!(
            expect_render_times(&[], &none).is_err(),
            "nothing was presented"
        );
        // A status bar stating a figure no presented frame carried.
        assert!(
            expect_render_times(
                &[displayed(json!(12.4))],
                &[frame("Exact render \u{b7} 90 ms", 90.0, false)]
            )
            .is_err()
        );
        // The approximate label must match the frame on screen.
        assert!(
            expect_render_times(
                &[displayed(json!(12.4))],
                &[frame("Exact render \u{b7} 12 ms", 12.4, false)
                    .as_object()
                    .map(|object| {
                        let mut object = object.clone();
                        object["state"]["approximate_white_balance"] = json!(true);
                        Value::Object(object)
                    })
                    .unwrap()]
            )
            .is_err()
        );
    }

    #[test]
    fn missing_binary_retains_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("evidence ü space");
        assert!(
            dispatch(
                &root().unwrap(),
                &out,
                "load",
                &tmp.path().join("absent"),
                Duration::from_millis(100),
                None,
                None,
            )
            .is_err()
        );
        assert_eq!(
            read_json(&out.join("result.json")).unwrap()["status"],
            "failed"
        );
        assert!(out.join("reproduce.md").is_file());
        assert!(!out.join("app/frame-1.png").exists());
    }

    /// `--manifest` goes to exactly the scenarios whose source is listed, and a run refused for it
    /// writes nothing.
    #[test]
    fn a_manifest_is_given_exactly_to_a_listed_scenario() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, out) = (root().unwrap(), tmp.path().join("out"));
        let manifest = tmp.path().join("manifest.json");
        fs::write(&manifest, "{}").unwrap();
        let bin = tmp.path().join("absent");
        let refusal = |name: &str, source: Option<Vec<PathBuf>>, manifest: Option<&Path>| {
            dispatch(&root, &out, name, &bin, Duration::ZERO, source, manifest)
                .unwrap_err()
                .to_string()
        };
        assert_eq!(
            refusal("load", None, Some(&manifest)),
            "--manifest is only for raw-detail and raw-editor"
        );
        let photo = Some(vec![tmp.path().join("photo.NEF")]);
        assert!(refusal("raw-editor", photo.clone(), None).contains("needs --manifest"));
        assert!(
            refusal("raw-editor", photo, Some(&tmp.path().join("gone.json"))).contains("missing")
        );
        assert!(!out.exists());
    }

    #[test]
    fn missing_evidence_and_stale_generation_fail() {
        let tmp = tempfile::tempdir().unwrap();
        let load = || (find("load").unwrap().launches[0].plan)(&[PathBuf::from("x.jpg")]);
        assert!(load().check(tmp.path()).is_err());
        fs::write(tmp.path().join("events.jsonl"),"{\"event\":\"startup\",\"run_id\":\"current\"}\n{\"event\":\"shutdown\",\"run_id\":\"current\"}\n").unwrap();
        image::RgbImage::new(4, 4)
            .save(tmp.path().join("frame-1.png"))
            .unwrap();
        let mut app = json!({"status":"captured","run_id":"current","had_input_errors":false,"frames":[{"file":"frame-1.png","capture_provenance":"window-renderer-readback","state":{"run_id":"old","requested_generation":1,"backend":{"backend":"metal","adapter":"a"},"stack":{"label":"Original"}}}]});
        let write = |app: &Value| {
            write_json(&tmp.path().join("result.json"), app).unwrap();
            write_json(&tmp.path().join("state-1.json"), &app["frames"][0]).unwrap();
        };
        write(&app);
        assert!(
            load()
                .check(tmp.path())
                .unwrap_err()
                .to_string()
                .contains("run identity")
        );
        app["frames"][0]["state"]["run_id"] = json!("current");
        app["frames"][0]["state"]["requested_generation"] = json!(0);
        write(&app);
        let checked = load().check(tmp.path()).unwrap();
        assert!(
            plain_checks("load", &checked)
                .unwrap_err()
                .to_string()
                .contains("generation")
        );
    }

    /// Every row's name is unique, every plan it can make is well formed, and every launch it
    /// reopens a catalog from comes before it.
    #[test]
    fn every_row_is_well_formed() {
        let root = root().unwrap();
        let mut names = std::collections::BTreeSet::new();
        for scenario in SCENARIOS {
            assert!(
                names.insert(scenario.name),
                "{} is listed twice",
                scenario.name
            );
            let sources = scenario
                .sources(&root, None)
                .unwrap_or_else(|_| vec![PathBuf::from("/raw/photo.nef")]);
            for (index, spec) in scenario.launches.iter().enumerate() {
                let plan = (spec.plan)(&sources);
                assert!(
                    plan.validate().is_ok(),
                    "{}: {:?}",
                    scenario.name,
                    plan.validate()
                );
                assert!(!plan.is_empty(), "{} plans no frame", scenario.name);
                if let Some(earlier) = spec.catalog {
                    assert!(
                        scenario.launches[..index]
                            .iter()
                            .any(|launch| launch.name == earlier),
                        "{} reopens {earlier}'s catalog before it runs",
                        scenario.name
                    );
                }
            }
            assert!(
                !scenario.launches.is_empty() || scenario.own.is_some(),
                "{} launches nothing",
                scenario.name
            );
        }
        assert!(find("raw-panel").is_ok_and(|raw| !raw.rendered()));
        assert!(find("look").is_ok_and(|look| !look.rendered()));
        // The filmstrip takes a RAW source, and runs without one, its RAW steps pending.
        assert!(find("filmstrip").is_ok_and(|filmstrip| filmstrip.rendered()));
        assert_eq!(
            sourced(),
            [
                "visibility-monitoring",
                "raw-detail",
                "copy-settings",
                "performance",
                "filmstrip",
                "raw-panel",
                "look",
                "raw-editor"
            ]
        );
        assert_eq!(listed(), ["raw-detail", "raw-editor"]);
        assert!(find("nothing").is_err());
    }

    /// Every launch's script, written as a launch writes it, and its argument list, into
    /// `$SCRIPT_DUMP/<scenario>/`: the proof that a change to the plans or to the launch envelope
    /// left the script each launch runs and the arguments it passes byte for byte the same.
    /// `performance` is written again over a RAW source as `performance-raw`.
    #[test]
    #[ignore]
    fn dump_scripts() {
        let dir =
            PathBuf::from(std::env::var("SCRIPT_DUMP").expect("SCRIPT_DUMP names a directory"));
        let root = root().unwrap();
        let put = |scenario: &str, file: &str, script: Value| {
            let dir = dir.join(scenario);
            fs::create_dir_all(&dir).unwrap();
            write_json(&dir.join(file), &script).unwrap();
        };
        for scenario in SCENARIOS {
            let sources = scenario
                .sources(&root, None)
                .unwrap_or_else(|_| vec![PathBuf::from("/raw/photo.nef")]);
            for spec in scenario.launches {
                let plan = (spec.plan)(&sources);
                if plan.scripted() {
                    put(scenario.name, spec.script, plan.kept());
                }
                // The editor's arguments, as the launch passes them in a run written to `/out`.
                let out = Path::new("/out");
                put(
                    scenario.name,
                    &format!("{}-arguments.json", spec.name),
                    json!(launch_of(scenario, spec, &plan, &sources, out).command(out)),
                );
            }
        }
        let raw = (find("performance").unwrap().launches[0].plan)(&[PathBuf::from("/x/photo.NEF")]);
        put("performance-raw", "script.json", raw.kept());
        // `capabilities` plans with its run's own endpoint and sentinels: here, fixed ones.
        let capabilities = capabilities::plan("http://127.0.0.1:1", "sentinel-KEY", "wrong-KEY");
        put("capabilities", "sent.json", capabilities.script());
        put("capabilities", "script.json", capabilities.kept());
    }

    /// The proof that each scenario is checked against its own plan: over recorded runs, in
    /// `$SMOKE_RECORDED/smoke-<scenario>/run`, a replay with the plan's last step removed fails on the
    /// frame count, and one with its first expected label changed fails on that step. Writes a
    /// line per scenario to `$SMOKE_MUTATIONS`; `$SMOKE_ONLY` names the scenarios to take, space
    /// separated, when not every recorded one.
    #[test]
    #[ignore]
    fn mutations() {
        use crate::scenario::plan::mutation::{self, Mutation};
        let recorded = PathBuf::from(std::env::var("SMOKE_RECORDED").expect("SMOKE_RECORDED"));
        // `cargo test` runs in the package directory, so a relative path would silently find no run.
        assert!(
            recorded.is_absolute() && recorded.is_dir(),
            "SMOKE_RECORDED must be an absolute path to a verify or smoke output: {}",
            recorded.display()
        );
        let only = std::env::var("SMOKE_ONLY").unwrap_or_default();
        let root = root().unwrap();
        let mut lines = Vec::new();
        let mut failures = Vec::new();
        for scenario in SCENARIOS {
            let run = recorded
                .join(format!("smoke-{}", scenario.name))
                .join("run");
            if !run.is_dir()
                || (!only.is_empty() && !only.split(' ').any(|name| name == scenario.name))
            {
                continue;
            }
            // A supplied source is given again: the one the recorded run's first launch opened.
            let source = matches!(scenario.source, Source::Supplied { .. }).then(|| {
                let recorded = read_json(&run.join("result.json")).unwrap();
                let command = recorded["launches"][0]["command"].as_array().unwrap();
                command
                    .windows(2)
                    .filter(|pair| pair[0] == "--open")
                    .map(|pair| PathBuf::from(pair[1].as_str().unwrap()))
                    .collect::<Vec<_>>()
            });
            let replay = |mutated: Option<Mutation>| {
                let out = tempfile::tempdir().unwrap();
                mutation::set(mutated);
                let outcome = verify_only(
                    &root,
                    &run,
                    &out.path().join("replay"),
                    scenario.name,
                    source.clone(),
                );
                let changed = mutation::changed();
                mutation::set(None);
                (outcome.map_err(|error| error.to_string()), changed)
            };
            let (plain, _) = replay(None);
            let (dropped, _) = replay(Some(Mutation::DropLast));
            let (relabelled, changed) = replay(Some(Mutation::Relabel));
            let count = dropped.as_ref().err().is_some_and(|error| {
                error.contains("the plan has") && error.contains("frames, but the launch captured")
            });
            let label = match changed.first() {
                None => "no label planned".to_owned(),
                Some(step) => {
                    let named = relabelled.as_ref().err().is_some_and(|error| {
                        error.contains(&format!("Step {step:?}"))
                            && error.contains("expected \"A label no step commits\"")
                    });
                    if !named {
                        failures.push(format!("{}: relabel {relabelled:?}", scenario.name));
                    }
                    format!(
                        "step {step:?} {}",
                        if named { "fails" } else { "DOES NOT FAIL" }
                    )
                }
            };
            if plain.is_err() || !count {
                failures.push(format!(
                    "{}: replay {plain:?}, drop {dropped:?}",
                    scenario.name
                ));
            }
            let line = format!(
                "{}: replay {}; last step removed: {}; label changed at {label}",
                scenario.name,
                if plain.is_ok() { "passes" } else { "FAILS" },
                if count {
                    "fails on the frame count"
                } else {
                    "DOES NOT FAIL on the count"
                },
            );
            println!("{line}");
            lines.push(line);
        }
        if let Ok(path) = std::env::var("SMOKE_MUTATIONS") {
            fs::write(path, lines.join("\n") + "\n").unwrap();
        }
        assert!(
            !lines.is_empty(),
            "no recorded run of {} under {}",
            if only.is_empty() {
                "any scenario"
            } else {
                &only
            },
            recorded.display()
        );
        assert!(failures.is_empty(), "{failures:#?}");
    }

    /// The development guide's scenario commands name only scenarios the table has, and its
    /// scenario list is the table's own, by pointing at `smoke --list` rather than restating it.
    #[test]
    fn the_docs_name_only_table_scenarios() {
        let guide =
            fs::read_to_string(root().unwrap().join("docs/engineering/development.md")).unwrap();
        let mut named = 0;
        for (index, _) in guide.match_indices("--scenario ") {
            let name: String = guide[index + "--scenario ".len()..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect();
            if name == "NAME" {
                continue;
            }
            assert!(
                find(&name).is_ok(),
                "development.md names {name:?}, which is no scenario"
            );
            named += 1;
        }
        assert!(named > 10);
        assert!(guide.contains("smoke --list"));
    }
}
