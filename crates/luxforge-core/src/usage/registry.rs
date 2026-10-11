//! The checked-in event and field registry: every event name, enum value and numeric range a
//! statistics batch may carry (`docs/design/usage-statistics-wire.md`).
//!
//! Every string an upload contains originates in one of these static enums, so there is no
//! `record(name, JSON)` and no arbitrary label: an unknown module, command or action has no upload
//! representation until it is deliberately mapped here. The `tool` and `feature` enums are also
//! checked in as `registry.json`, which the ingress validates against its own copy of; a test holds
//! the two equal.

/// A closed set of wire strings: the enum, its values in declaration order, and both directions of
/// the mapping. Parsing is exact (no case folding, no trimming).
macro_rules! wire_enum {
    ($(#[$meta:meta])* $vis:vis enum $name:ident { $($(#[$vmeta:meta])* $variant:ident => $wire:literal,)+ }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        $vis enum $name {
            $($(#[$vmeta])* $variant,)+
        }

        impl $name {
            /// Every value, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];

            /// The value's wire string.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire,)+
                }
            }

            /// The value a wire string names, exactly.
            #[allow(clippy::should_implement_trait)]
            pub fn parse(wire: &str) -> Option<Self> {
                match wire {
                    $($wire => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}
pub(crate) use wire_enum;

/// The registry's `tool` and `feature` enums as checked in, which the ingress keeps a copy of.
pub const REGISTRY_JSON: &str = include_str!("registry.json");

/// The most events one batch, and one bank, holds.
pub const MAX_EVENTS: usize = 512;
/// The most bytes one serialized batch body holds.
pub const MAX_BODY_BYTES: usize = 65_536;
/// The most bytes the serialized `device` object holds.
pub const MAX_DEVICE_BYTES: usize = 2_048;
/// Durations are clipped to 24 hours.
pub const MAX_DURATION_MS: u32 = 86_400_000;
/// Percentages of one core (or of the GPU) are clipped to 1,024 cores' worth.
pub const MAX_PERCENT: u32 = 102_400;
/// Memory is clipped to 16 TiB, in MiB.
pub const MAX_MEMORY_MIB: u32 = 16_777_216;
/// The most photographs one `images.added` or export `count` reports: a million, a wire range (not
/// a parallel threshold, which `luxforge-raw`'s limits own).
pub const MAX_COUNT: u32 = 10u32.pow(6);
/// The schema this encoder writes and the validator accepts; no other.
pub const SCHEMA: u32 = 1;

wire_enum! {
    /// A built-in adjustment tool a recipe transaction changed (`tool.commit`): one per shipped
    /// product module that edits a photograph. A module's identity maps through
    /// [`Tool::from_module_id`]; developer proofs, Presets (whose application commits each group it
    /// changes under that group's own tool) and host features (masks, history) are not tools.
    pub enum Tool {
        /// Basic: white balance, tone and colour. On a RAW photo's global target its white balance
        /// is the RAW development's (`luxforge.raw`), which Basic presents.
        Basic => "basic",
        /// Corrections: the Clone and Heal operations of the repair stage, and a frozen patch
        /// accepted from a task's candidate.
        Corrections => "corrections",
        /// Crop, transform, straighten, including Rotate, Mirror and Flip.
        Crop => "crop",
        /// The tone curve.
        Curve => "curve",
        /// Sharpening and noise reduction.
        Detail => "detail",
        /// Lens correction.
        Lens => "lens",
        /// A RAW photo's Look.
        Look => "look",
        /// The colour mixer and grading.
        Mixer => "mixer",
        /// Perspective (keystone) correction.
        Perspective => "perspective",
        /// Texture, Clarity and Dehaze.
        Presence => "presence",
        /// The post-crop vignette.
        Vignette => "vignette",
    }
}

impl Tool {
    /// The tool a built-in module's identity belongs to, or `None` for anything that is not a
    /// shipped adjustment tool: an external or developer module, Presets, or an unknown identity.
    pub fn from_module_id(module: &str) -> Option<Self> {
        Some(match module {
            "luxforge.basic" => Self::Basic,
            raw if raw == crate::modules::RAW_MODULE => Self::Basic,
            "luxforge.corrections" => Self::Corrections,
            "luxforge.crop" => Self::Crop,
            "luxforge.curve" => Self::Curve,
            "luxforge.detail" => Self::Detail,
            "luxforge.lens" => Self::Lens,
            "luxforge.look" => Self::Look,
            "luxforge.mixer" => Self::Mixer,
            "luxforge.perspective" => Self::Perspective,
            "luxforge.presence" => Self::Presence,
            "luxforge.vignette" => Self::Vignette,
            _ => return None,
        })
    }
}

wire_enum! {
    /// A completed deliberate operation (`interaction`), across navigation, zoom, compare, clipping,
    /// information, history, versions, presets, copy/paste, masks, catalog organization, Locate,
    /// HDR merges, the development set, preferences and export. Only shipped operations have a value; a continuous gesture is one
    /// event at its completion. Adjustment commits are [`Tool`]s, not features.
    pub enum Feature {
        /// Select: a deliberate change of the view's source, filter, sort or grouping.
        BrowseView => "browse-view",
        /// The clipping overlay (`J`) or a clipping triangle turned on or off.
        ClippingOverlay => "clipping-overlay",
        CollectionAdd => "collection-add",
        CollectionCreate => "collection-create",
        CollectionDelete => "collection-delete",
        CollectionMove => "collection-move",
        CollectionRemove => "collection-remove",
        CollectionRename => "collection-rename",
        CollectionSmartCreate => "collection-smart-create",
        CollectionSmartUpdate => "collection-smart-update",
        /// Select's loupe: compare the moment.
        CompareMoment => "compare-moment",
        /// Develop: the Original hold.
        CompareOriginal => "compare-original",
        /// Develop: the Before/After slider entered, moved or left.
        CompareSlider => "compare-slider",
        /// Developing picks (`pick.develop`).
        DevelopPicks => "develop-picks",
        /// Photographs added to the development set (`development-set.add`, Add to Develop).
        DevelopmentSetAdd => "development-set-add",
        /// Photographs taken out of the development set (`development-set.remove`).
        DevelopmentSetRemove => "development-set-remove",
        /// An export of the displayed photograph started.
        Export => "export",
        /// A batch export of selected photographs started.
        ExportBatch => "export-batch",
        /// Select's 100% focus check turned on or off.
        FocusCheck => "focus-check",
        FolderCreate => "folder-create",
        FolderDelete => "folder-delete",
        FolderMerge => "folder-merge",
        FolderMove => "folder-move",
        FolderRename => "folder-rename",
        /// An HDR merge started (`merge.create`) or a recorded one merged again (`merge.rerun`).
        HdrMerge => "hdr-merge",
        /// A history entry previewed.
        HistoryPreview => "history-preview",
        HistoryRedo => "history-redo",
        HistoryRestore => "history-restore",
        HistoryUndo => "history-undo",
        IndexedFolderAdd => "indexed-folder-add",
        IndexedFolderRemove => "indexed-folder-remove",
        /// The Info panel shown or hidden.
        InformationPanel => "information-panel",
        LibraryRedo => "library-redo",
        LibraryUndo => "library-undo",
        /// One missing original located (`source.locate`).
        LocateOriginal => "locate-original",
        /// Select's loupe and grid toggled.
        LoupeToggle => "loupe-toggle",
        MaskComponentAdd => "mask-component-add",
        MaskComponentDelete => "mask-component-delete",
        MaskCreate => "mask-create",
        MaskDelete => "mask-delete",
        MaskDuplicate => "mask-duplicate",
        /// A completed change to a mask or one of its components: geometry, a brush stroke, a range,
        /// amount, inversion or mode.
        MaskEdit => "mask-edit",
        MaskRename => "mask-rename",
        MaskReorder => "mask-reorder",
        /// Remove merge: merged photographs moved to Removed with their stacks (`merge.remove`).
        MergeRemove => "merge-remove",
        /// Put back merge (`merge.restore`).
        MergeRestore => "merge-restore",
        /// The previous or next photograph or frame.
        NavigatePhoto => "navigate-photo",
        /// Missing originals searched for (`source.find`).
        OriginalsFind => "originals-find",
        /// Found originals relinked (`source.relink`).
        OriginalsRelink => "originals-relink",
        /// A completed pan gesture.
        Pan => "pan",
        /// A side panel or the filmstrip shown or hidden.
        PanelToggle => "panel-toggle",
        PhotoMove => "photo-move",
        PhotoRemove => "photo-remove",
        PhotoRestore => "photo-restore",
        PhotoSendBack => "photo-send-back",
        PickSet => "pick-set",
        /// A deliberate change in Settings, including the theme.
        PreferencesChange => "preferences-change",
        PresetApply => "preset-apply",
        PresetCreate => "preset-create",
        PresetDelete => "preset-delete",
        PresetExport => "preset-export",
        PresetImport => "preset-import",
        PresetUpdate => "preset-update",
        /// Empty Removed.
        RemovedEmpty => "removed-empty",
        SettingsCopy => "settings-copy",
        /// Paste settings onto the open photograph or a selection.
        SettingsPaste => "settings-paste",
        /// Paste from previous.
        SettingsPastePrevious => "settings-paste-previous",
        ThemeDelete => "theme-delete",
        ThemeExport => "theme-export",
        ThemeImport => "theme-import",
        VersionCreate => "version-create",
        VersionDelete => "version-delete",
        VersionSelect => "version-select",
        /// A zoom stop, Fit, 100% or a typed zoom.
        Zoom => "zoom",
    }
}

wire_enum! {
    /// Why a session began (`session.start`).
    pub enum Trigger {
        Launch => "launch",
        OptIn => "opt-in",
    }
}

wire_enum! {
    /// The window became visible or hidden (`app.presented`).
    pub enum Presented {
        Visible => "visible",
        Hidden => "hidden",
    }
}

wire_enum! {
    /// A minute without deliberate interaction, or its end (`app.idle`).
    pub enum Idle {
        Idle => "idle",
        Active => "active",
    }
}

wire_enum! {
    /// A deliberate transition between workspaces (`workspace.enter`).
    pub enum Workspace {
        Select => "select",
        Develop => "develop",
    }
}

wire_enum! {
    /// A fixed operation whose terminal outcome `operation.end` reports.
    pub enum Operation {
        SourcePreparation => "source-preparation",
        CatalogScan => "catalog-scan",
        Export => "export",
        AutoTone => "auto-tone",
    }
}

wire_enum! {
    /// An operation's terminal outcome.
    pub enum Outcome {
        Completed => "completed",
        Cancelled => "cancelled",
        Refused => "refused",
        Failed => "failed",
    }
}

wire_enum! {
    /// The path that drew a settled preview, and the device profile's renderer.
    pub enum Renderer {
        Gpu => "gpu",
        Reference => "reference",
    }
}

wire_enum! {
    /// What a process memory figure measures, as the platform's resource report names it.
    pub enum MemoryKind {
        Footprint => "footprint",
        Resident => "resident",
        Private => "private",
    }
}

wire_enum! {
    /// Every event name, in the wire format's order.
    pub enum EventName {
        SessionStart => "session.start",
        AppPresented => "app.presented",
        AppIdle => "app.idle",
        WorkspaceEnter => "workspace.enter",
        ImageOpen => "image.open",
        Interaction => "interaction",
        ToolCommit => "tool.commit",
        ImagesAdded => "images.added",
        OperationEnd => "operation.end",
        PreviewSettled => "preview.settled",
        ProcessSample => "process.sample",
    }
}

impl Operation {
    /// Whether this operation's `operation.end` may carry `image`.
    pub const fn may_carry_image(self) -> bool {
        !matches!(self, Self::CatalogScan)
    }

    /// Whether this operation's `operation.end` may carry `count`.
    pub const fn may_carry_count(self) -> bool {
        matches!(self, Self::Export)
    }
}

impl From<luxforge_process::MemoryKind> for MemoryKind {
    fn from(kind: luxforge_process::MemoryKind) -> Self {
        match kind {
            luxforge_process::MemoryKind::Footprint => Self::Footprint,
            luxforge_process::MemoryKind::Resident => Self::Resident,
            luxforge_process::MemoryKind::Private => Self::Private,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn checked_in(key: &str) -> Vec<String> {
        let registry: Value = serde_json::from_str(REGISTRY_JSON).expect("registry.json parses");
        let object = registry.as_object().expect("an object");
        let mut keys: Vec<&String> = object.keys().collect();
        keys.sort();
        assert_eq!(
            keys,
            ["feature", "tool"],
            "registry.json holds tool and feature only"
        );
        object[key]
            .as_array()
            .expect("an array")
            .iter()
            .map(|value| value.as_str().expect("a string").to_owned())
            .collect()
    }

    fn assert_sorted_kebab(values: &[String]) {
        let mut sorted = values.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(values, sorted, "sorted and distinct");
        for value in values {
            assert!(
                value
                    .split('-')
                    .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_lowercase())),
                "{value} is kebab-case"
            );
        }
    }

    #[test]
    fn the_rust_tool_and_feature_enums_equal_registry_json() {
        let tools = checked_in("tool");
        let features = checked_in("feature");
        assert_sorted_kebab(&tools);
        assert_sorted_kebab(&features);
        let rust_tools: Vec<String> = Tool::ALL.iter().map(|t| t.as_str().to_owned()).collect();
        let rust_features: Vec<String> =
            Feature::ALL.iter().map(|f| f.as_str().to_owned()).collect();
        assert_eq!(rust_tools, tools);
        assert_eq!(rust_features, features);
    }

    #[test]
    fn every_wire_string_parses_back_to_its_value_and_nothing_else_parses() {
        for tool in Tool::ALL {
            assert_eq!(Tool::parse(tool.as_str()), Some(*tool));
        }
        for feature in Feature::ALL {
            assert_eq!(Feature::parse(feature.as_str()), Some(*feature));
        }
        for name in EventName::ALL {
            assert_eq!(EventName::parse(name.as_str()), Some(*name));
        }
        for unknown in [
            "",
            "Basic",
            "basic ",
            "luxforge.basic",
            "pixel",
            "presets",
            "heal",
        ] {
            assert_eq!(Tool::parse(unknown), None, "{unknown:?}");
        }
        assert_eq!(Feature::parse("ZOOM"), None);
    }

    #[test]
    fn every_shipped_adjustment_module_maps_to_a_tool_and_nothing_else_does() {
        let registry = crate::ModuleRegistry::builtin();
        let mut mapped = Vec::new();
        for descriptor in registry.descriptors() {
            match Tool::from_module_id(&descriptor.id) {
                Some(tool) => mapped.push(tool),
                None => assert_eq!(
                    descriptor.id, "luxforge.presets",
                    "only Presets, which commits under each changed group's own tool, is not one"
                ),
            }
        }
        mapped.sort();
        mapped.dedup();
        assert_eq!(mapped, Tool::ALL, "every tool is a shipped module's");
        assert_eq!(
            Tool::from_module_id("luxforge.pixel"),
            None,
            "a developer proof"
        );
        assert_eq!(Tool::from_module_id("example.external"), None);
    }
}
