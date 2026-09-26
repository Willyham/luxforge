//! The state correlated with every logged event and captured frame: what the screen shows, read
//! from the editor and the derived models, never including a source path.
use super::{Editor, sync::module_summary};
use crate::{state, state::tools, view};
use serde_json::{Value, json};

impl Editor {
    /// The state correlated with every event and captured frame; never includes source paths.
    pub(crate) fn snapshot(&self) -> Value {
        fn summarize_controls(
            controls: &[tools::ControlModel],
            curves: &mut Vec<Value>,
            pickers: &mut Vec<Value>,
            samples: &std::collections::BTreeMap<(String, String), tools::CurveSamples>,
            entry: Option<&luxforge_core::EntryId>,
        ) {
            for control in state::control_tree::walk(controls) {
                match control {
                    tools::ControlModel::Curve(curve) => {
                        let parameter = &curve.channels[curve.selected_channel].parameter;
                        let sampled = samples.get(&(curve.action.clone(), parameter.clone()));
                        curves.push(json!({"action":curve.action,"parameter":parameter,
                            "channel":curve.selected_channel,"selected_point":curve.selected_point,
                            "point_count":curve.points.len(),"sample_count":curve.sampled.len(),
                            "sample_version":sampled.map(|sample| sample.version),
                            "sample_source":sampled.map(|sample| &sample.source),
                            "sample_source_entry":sampled.map(|sample| &sample.entry),
                            "sample_asset":sampled.map(|sample| &sample.asset),
                            "display_entry":entry,"dragging":curve.dragging}));
                    }
                    tools::ControlModel::Color(color) => {
                        pickers.push(json!({"action":color.action,"parameter":color.parameter,
                            "open":color.picker_open,"dragging":color.dragging,"rgb":color.rgb}));
                    }
                    _ => {}
                }
            }
        }
        let gallery = self
            .gallery_page()
            .and_then(view::gallery_page_info)
            .map(|info| {
                json!({"page":info.page,"count":info.count,
                "title":info.title,"state_count":info.state_count})
            });
        let tools_scroll = self
            .evidence
            .as_ref()
            .and_then(|evidence| evidence.tools_scroll);
        let curve_channels: Vec<Value> = self
            .controls_ui
            .curve_channels
            .iter()
            .map(|((action, parameter), channel)| {
                json!({"action":action,
                "parameter":parameter,"channel":channel})
            })
            .collect();
        let curve_points: Vec<Value> = self
            .controls_ui
            .curve_points
            .iter()
            .map(|((action, parameter), point)| {
                json!({"action":action,
                "parameter":parameter,"point":point})
            })
            .collect();
        let picker_open: Vec<Value> = self
            .controls_ui
            .color_open
            .iter()
            .map(|((action, parameter), open)| {
                json!({"action":action,
                "parameter":parameter,"open":open})
            })
            .collect();
        let entry = self.displayed_entry();
        let mut curves = Vec::new();
        let mut pickers = Vec::new();
        for section in self.workspace.tools.all() {
            summarize_controls(
                &section.controls,
                &mut curves,
                &mut pickers,
                &self.controls_ui.curve_samples,
                entry.as_ref(),
            );
        }
        json!({"run_id":self.run_id,"mode":if self.evidence.is_some() {"evidence"} else {"editor"},"selection":self.session.preview.selection,"orientation":self.activity.orientation,"phase":self.activity.phase,"requested_generation":self.activity.requested,"displayed_generation":self.activity.displayed,"displayed_draft_revision":self.displayed_draft_revision,"source_dimensions":self.activity.source_dimensions,"preview_dimensions":self.activity.preview_dimensions,"backend":self.activity.backend,"status":self.status,"error_code":self.activity.error_code,"modules":module_summary(&self.modules),"controls":self.fields.summary(),"control_ui":{"group_expanded":self.controls_ui.group_expanded,"selected_tab":self.controls_ui.selected_tab,"curve_channels":curve_channels,"curve_points":curve_points,"picker_open":picker_open,"curves":curves,"pickers":pickers},"gallery":gallery,"tools_scroll":tools_scroll,"crop":self.crop_summary(),"masks":self.workspace.masks.summary(),"mask_draft":self.mask_draft_summary(),"last_mask_request":self.last_mask_request.as_ref().map(|(method, params)| json!({"method":method,"params":params})),"draft":self.draft_summary(),"stack":self.stack_summary(),"workspace":serde_json::to_value(&self.session.workspace).unwrap_or(Value::Null),"developer":self.developer,"expanded":self.workspace.expanded(),"pickers":self.workspace.pickers(),"notices":self.notice_titles(),"compare":self.compare_return.is_some(),"render_error":self.render_error_summary(),"palette":{"open":self.palette_open,"query":self.palette_query},"presets":self.presets_summary(),"histogram":self.histogram_summary(),"readout":self.readout_summary(),"status_bar":self.status_bar_summary(),"proxy":self.proxy_summary(),"approximate_white_balance":self.presented_approximate_white_balance,"surface":self.surface_summary(),"active":self.workspace.active(),"scratch":self.scratch_summary(),"capabilities":state::capabilities::summary(&self.capabilities,&self.modules,self.state.as_ref()),"performance":self.performance_summary()})
    }

    /// The Presets section as the frame drew it: its rows, the create form and whether the section
    /// is expanded. `null` when no module declares a `presets` control.
    pub(super) fn presets_summary(&self) -> Value {
        self.workspace
            .tools
            .all()
            .find_map(|section| {
                section
                    .presets()
                    .map(|presets| presets.summary(section.expanded))
            })
            .unwrap_or(Value::Null)
    }

    /// The owner's colour scratch budget, which every preview and analysis render shares, as it
    /// stands when the frame is captured, with the high-water mark the renders behind that frame
    /// actually reached. A pass releases its reservation as soon as its chunk is done, so `in_use`
    /// here is normally zero; `peak` is the figure a resource measurement wants.
    pub(super) fn scratch_summary(&self) -> Value {
        let context = self.owner.render_context();
        let budget = context.scratch();
        json!({
            "target_bytes": budget.target(),
            "in_use_bytes": budget.in_use(),
            "peak_bytes": budget.peak(),
        })
    }

    /// The core draft this client holds, as `session.state` reports it. The desktop adopts every
    /// draft response into its own copy of the session, so a captured frame carries the identity,
    /// the fields, both revisions and the conflict state the gesture was evaluated against. An open
    /// gesture whose session copy has not caught up reports what the desktop itself knows, so a
    /// frame never shows "no draft" while one is plainly on screen.
    pub(super) fn draft_summary(&self) -> Value {
        match (&self.session.draft, self.slider_gesture()) {
            (Some(draft), _) => json!({
                "draft_id": draft.draft_id.as_str(),
                "action": draft.action,
                "fields": draft.fields,
                "base_revision": draft.base_revision,
                "draft_revision": draft.draft_revision,
                "conflicted": draft.conflicted,
            }),
            (None, Some(slider)) => self
                .core_gesture()
                .map(|gesture| gesture.draft.summary(&slider.action))
                .unwrap_or(Value::Null),
            (None, None) => Value::Null,
        }
    }

    /// The open mask gesture as a captured frame reports it: its shape, with the revision its core
    /// draft is based on and whether that draft is conflicted.
    pub(crate) fn mask_draft_summary(&self) -> Value {
        let Some(gesture) = self.core_gesture() else {
            return Value::Null;
        };
        let Some(mask) = gesture.mask() else {
            return Value::Null;
        };
        let mut summary = mask.shape.summary();
        if let Some(object) = summary.as_object_mut() {
            object.insert("base_revision".into(), json!(gesture.draft.base_revision));
            object.insert("conflicted".into(), json!(gesture.draft.conflicted));
        }
        summary
    }

    /// The histogram inspector as a captured frame reports it: its status, the render identity the
    /// counts belong to, all ten endpoint counters and the count one full-height bin stands for, so
    /// a frame's plot can be checked against an independent reduction of the same fixture. Beside
    /// them, where the inspector's words are drawn: `notice` is the text drawn inside the plot's own
    /// area (null while there is a report), and `tooltips` what the two triangles state on hover.
    /// The plot itself states nothing, so the summary carries no caption.
    pub(super) fn histogram_summary(&self) -> Value {
        let model = &self.workspace.histogram;
        let counters = &model.counters;
        let identity = match &model.identity {
            Some(identity) => {
                json!({"entry":identity.entry,"draft_revision":identity.draft_revision,"generation":identity.generation,"width":identity.width,"height":identity.height,"domain":luxforge_core::analysis::AnalysisDomain.as_str()})
            }
            None => Value::Null,
        };
        let tooltips =
            json!({"shadow":model.shadow_tooltip(),"highlight":model.highlight_tooltip()});
        json!({"status":model.status.as_str(),"stale":model.stale,"notice":model.notice(),"tooltips":tooltips,"identity":identity,"plotted_max":model.plotted_max,"reason":model.reason,"counters":{"r0":counters.r0,"g0":counters.g0,"b0":counters.b0,"r255":counters.r255,"g255":counters.g255,"b255":counters.b255,"any_shadow":counters.any_shadow,"any_highlight":counters.any_highlight,"all_shadow":counters.all_shadow,"all_highlight":counters.all_highlight,"both":counters.both},"overlay":self.overlay_summary()})
    }

    /// The clipping overlay a captured frame was drawn with: its cell grid, which flags it covers
    /// and whether its pixels are on the GPU for the displayed generation.
    pub(super) fn overlay_summary(&self) -> Value {
        match &self.overlay_request {
            Some(request) => {
                json!({"cells":[request.cells_w,request.cells_h],"shadows":request.shadows,"highlights":request.highlights,"generation":request.generation,"approximate":request.approximate,"drawn":self.overlay_surface().is_some()})
            }
            None => Value::Null,
        }
    }

    /// The photograph's surface as a captured frame reports it: the view it is drawn at, the preview
    /// generation whose raster it holds, that raster's size and version, how many rasters the
    /// surface has written into its texture and how many times the view has been built. Two frames
    /// with the same version and the same write count prove nothing was written between them,
    /// however often the view was rebuilt meanwhile.
    pub(super) fn surface_summary(&self) -> Value {
        json!({
            "view": serde_json::to_value(&self.session.preview.view).unwrap_or(Value::Null),
            "generation": self.presented_generation,
            "raster": self.presenter.photo().map(|photo| {
                let (width, height) = photo.size();
                json!([width, height])
            }),
            "version": self.presenter.photo().map(luxforge_ui::Frame::version),
            "texture_writes": luxforge_ui::photo_surface::texture_writes(),
            "views": self.loop_timing.get().views,
        })
    }

    /// The display proxy as a captured frame reports it: the bounds the next job will offer, what
    /// the core did with the last one, and whether the texture on screen is a proxy.
    ///
    /// `eligible` is whether the newest job took the proxy path at all, and is `null` until one
    /// has reported either way. `declined` names why it did not — an ineligible layer, a stage
    /// already inside the bounds, or a failure building or rendering the proxy — so a stack that
    /// took the exact path says so rather than being silently identical to one that did not.
    pub(super) fn proxy_summary(&self) -> Value {
        let bounds = self.proxy_bounds();
        json!({
            "eligible": match (&self.proxy_declined, self.proxy_frame.is_some()) {
                (Some(_), _) => Some(false),
                (None, true) => Some(true),
                (None, false) => None,
            },
            "declined": self.proxy_declined,
            "approximate": self
                .presented_proxy_frame()
                .map(|frame| frame.approximation.is_approximate()),
            "approximate_reason": self
                .presented_proxy_frame()
                .and_then(|frame| frame.approximation.reason()),
            "dimensions": self
                .presented_proxy_frame()
                .map(|frame| json!([frame.dimensions.0, frame.dimensions.1])),
            "bounds": bounds.map(|bounds| json!({"width":bounds.width,"height":bounds.height})),
            "presented": self.presented_proxy,
        })
    }

    /// The pointer readout, when one has been sampled: the three output codes and their pixel.
    pub(super) fn readout_summary(&self) -> Value {
        match &self.readout {
            Some(readout) => {
                json!({"x":readout.x,"y":readout.y,"rgba":readout.rgba,"text":state::histogram::readout_text(readout)})
            }
            None => Value::Null,
        }
    }

    /// The status bar as the captured frame drew it: the pointer readout's slot (null when empty)
    /// and the renderer's figure for the picture on screen.
    pub(super) fn status_bar_summary(&self) -> Value {
        let model = &self.workspace.status;
        json!({"readout":model.readout,"render":model.render,"render_ms":self.activity.render.map(|time| time.ms),"render_proxy":self.activity.render.map(|time| time.proxy),"render_approximate":self.activity.render.map(|time| time.approximate)})
    }

    /// The notices the captured frame drew, by title, so a frame's chrome is observable.
    pub(super) fn notice_titles(&self) -> Value {
        Value::Array(
            self.workspace
                .canvas
                .notices
                .iter()
                .map(|notice| Value::from(notice.title.clone()))
                .collect(),
        )
    }

    /// The failure the notices were derived from, as its code and detail.
    pub(super) fn render_error_summary(&self) -> Value {
        match &self.render_error {
            Some((kind, detail)) => json!({"code":kind.code(),"detail":detail}),
            None => Value::Null,
        }
    }

    /// The committed stack the captured frame belongs to: the revision, the current entry and every
    /// layer's identity, effect and payload, so evidence can prove that an edit updated one layer in
    /// place instead of appending another.
    pub(super) fn stack_summary(&self) -> Value {
        match &self.state {
            Some(state) => {
                let layers: Vec<Value> = state
                    .current_entry
                    .snapshot
                    .recipe
                    .layers
                    .iter()
                    .map(|layer| {
                        json!({"id":layer.id.as_str(),"effect":layer.effect_id,"payload":layer.payload,"mask":layer.mask.as_ref().map(luxforge_core::MaskId::as_str),"artifacts":layer.artifacts})
                    })
                    .collect();
                let displayed = self.rendered_entry.as_ref().map(|entry| json!({
                    "entry": entry.id.as_str(),
                    "snapshot": entry.snapshot.id.as_str(),
                    "dimensions": self.dimensions,
                    "layers": entry.snapshot.recipe.layers.iter().map(|layer| json!({"id":layer.id.as_str(),"effect":layer.effect_id,"payload":layer.payload,"mask":layer.mask.as_ref().map(luxforge_core::MaskId::as_str),"artifacts":layer.artifacts})).collect::<Vec<_>>(),
                }));
                json!({"revision":state.revision,"entry":state.current_entry.id.as_str(),"label":state.current_entry.label,"layers":layers,"displayed":displayed})
            }
            None => Value::Null,
        }
    }

    /// The crop draft as a captured frame reports it, so a rendered frame correlates with the
    /// rectangle, angle and output size that produced it.
    pub(super) fn crop_summary(&self) -> Value {
        match self
            .core_gesture()
            .and_then(|gesture| Some((gesture.crop()?, &gesture.draft)))
            .filter(|(crop, _)| crop.frame.is_some())
        {
            Some((crop, draft)) => {
                let mut summary = crop.summary(draft);
                if let Some(object) = summary.as_object_mut() {
                    object.insert("drafting".into(), Value::from(true));
                    object.insert("guide".into(), Value::from(self.crop_guide));
                    object.insert("option".into(), Value::from(self.crop_option));
                    object.insert("space".into(), Value::from(self.crop_space));
                    object.insert(
                        "paused".into(),
                        Value::from(!self.session.preview.can_edit()),
                    );
                    object.insert(
                        "input_stage_loaded".into(),
                        Value::from(self.presenter.stage().is_some()),
                    );
                    object.insert("section".into(), self.crop_section_summary());
                }
                summary
            }
            None => {
                json!({"drafting":false,"pending":self.crop_pending().is_some(),"section":self.crop_section_summary()})
            }
        }
    }

    /// What the crop section shows, exactly as its model derived it for the frame on screen: the
    /// chosen ratio chip, the lock, the angle's box and rail, and whether its controls act. A
    /// capture of the section is checked against these.
    pub(super) fn crop_section_summary(&self) -> Value {
        self.workspace
            .tools
            .all()
            .flat_map(|section| section.controls.iter())
            .find_map(|control| match control {
                state::tools::ControlModel::CropFrame(model) => Some(model),
                _ => None,
            })
            .map_or(Value::Null, |model| {
                json!({
                    "drafting": model.drafting,
                    "pending": model.pending,
                    "enabled": model.enabled,
                    "chosen": model.presets.iter().find(|chip| chip.chosen).map(|chip| chip.label.clone()),
                    "locked": model.locked,
                    "can_swap": model.can_swap,
                    "angle": model.angle,
                    "rail": model.angle_rail.as_ref().map(|rail| rail.value),
                    "guide": model.guide,
                })
            })
    }
}
