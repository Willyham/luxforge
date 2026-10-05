//! The theme methods (`docs/design/ui-themes.md`, Library and API). The catalog owner answers
//! them from the capability host's theme library, `themes.json` beside the preferences, through
//! one bounded read or one locked read-modify-write each. None of them reads or writes a catalog,
//! so none changes an asset's revision. `theme.import` and `theme.delete` carry the `request`
//! envelope, and the owner's request table answers their retries.
use super::{Call, Owner};
use crate::{
    Error, MutationOutcome,
    api::{
        announce_once, methods,
        params::{NoParams, host_params},
    },
    theme::{
        MAX_THEME_FILE_BYTES, MAX_THEME_FOLDER, MAX_THEME_ID, MAX_THEME_NAME, ThemeFormat,
        ThemeInput,
    },
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// The `folder` parameter of `theme.inspect` and `theme.import`.
const FOLDER_NOTES: &str = "format omarchy: the theme folder's own name, its slug, not a path; \
    names the theme as Omarchy lists it unless name is given, and is kept in its origin; a \
    request with neither folder nor name is refused";

host_params! {
    /// `theme.read` and `theme.export`.
    pub(in crate::api) struct ThemeParams {
        theme_id: String = string(MAX_THEME_ID).notes("a theme id theme.list answers"),
    }
}

host_params! {
    /// `theme.inspect`.
    pub(in crate::api) struct ThemeInspect {
        format: ThemeFormat = enumeration(ThemeFormat::ALL.map(ThemeFormat::as_str)),
        content: Option<String> = text(MAX_THEME_FILE_BYTES).notes("format luxforge: the Luxforge theme document's text"),
        files: Option<BTreeMap<String, String>> = json("format omarchy: {file name: text} for the theme folder's colors.toml, alacritty.toml and light.mode, each at most 64 KiB"),
        folder: Option<String> = string(MAX_THEME_FOLDER).notes(FOLDER_NOTES),
        name: Option<String> = string(MAX_THEME_NAME).notes("overrides the theme's own name; non-empty after trimming"),
    }
}

host_params! {
    /// `theme.import`.
    pub(in crate::api) struct ThemeImport {
        format: ThemeFormat = enumeration(ThemeFormat::ALL.map(ThemeFormat::as_str)),
        mutation: MutationRequest,
        content: Option<String> = text(MAX_THEME_FILE_BYTES).notes("format luxforge: the Luxforge theme document's text"),
        files: Option<BTreeMap<String, String>> = json("format omarchy: {file name: text} for the theme folder's colors.toml, alacritty.toml and light.mode, each at most 64 KiB"),
        folder: Option<String> = string(MAX_THEME_FOLDER).notes(FOLDER_NOTES),
        name: Option<String> = string(MAX_THEME_NAME).notes("overrides the theme's own name; non-empty after trimming"),
    }
}

host_params! {
    /// `theme.delete`.
    pub(in crate::api) struct ThemeDelete {
        theme_id: String = string(MAX_THEME_ID).notes("a stored theme's id"),
        mutation: MutationRequest,
    }
}

/// `theme.list`: every theme, Luxforge Dark first, the active theme's id and the stored records
/// this build cannot read.
pub(in crate::api) fn list(owner: &mut Owner, _: &Call<'_>, _: NoParams) -> Result<Value, Error> {
    let listing = owner.host.themes.list()?;
    let preferences = owner.host.preferences.read()?;
    Ok(json!({
        "themes": listing.themes.iter().map(|theme| theme.summary()).collect::<Vec<_>>(),
        "active": preferences.theme(),
        "unrecognized": listing.unrecognized,
    }))
}

/// `theme.read`: one theme with every resolved token, its full report and its source.
pub(in crate::api) fn read(
    owner: &mut Owner,
    _: &Call<'_>,
    params: ThemeParams,
) -> Result<Value, Error> {
    let theme = owner.host.themes.theme(&params.theme_id)?;
    Ok(json!({"theme": theme.record(true)}))
}

/// `theme.inspect`: what an import would store, with nothing stored.
pub(in crate::api) fn inspect(
    owner: &mut Owner,
    _: &Call<'_>,
    params: ThemeInspect,
) -> Result<Value, Error> {
    let theme = owner.host.themes.inspect(ThemeInput {
        format: params.format,
        content: params.content.as_deref(),
        files: params.files.as_ref(),
        folder: params.folder.as_deref(),
        name: params.name.as_deref(),
    })?;
    Ok(json!({"theme": theme.record(false), "report": theme.resolved.report}))
}

/// `theme.import`: one stored theme, announced.
pub(in crate::api) fn import(
    owner: &mut Owner,
    call: &Call<'_>,
    params: ThemeImport,
) -> Result<Value, Error> {
    let theme = owner.host.themes.import(
        ThemeInput {
            format: params.format,
            content: params.content.as_deref(),
            files: params.files.as_ref(),
            folder: params.folder.as_deref(),
            name: params.name.as_deref(),
        },
        &params.mutation.actor,
    )?;
    announce_once(&mut owner.announced, &call.origin);
    Ok(json!({"theme": theme.record(false), "report": theme.resolved.report}))
}

/// `theme.export`: a Luxforge theme document, for a built-in theme too.
pub(in crate::api) fn export(
    owner: &mut Owner,
    _: &Call<'_>,
    params: ThemeParams,
) -> Result<Value, Error> {
    methods::value(owner.host.themes.export(&params.theme_id)?)
}

/// `theme.delete`: refused for a built-in theme and for the active one, which the preferences
/// name; announced when the theme was there.
pub(in crate::api) fn delete(
    owner: &mut Owner,
    call: &Call<'_>,
    params: ThemeDelete,
) -> Result<Value, Error> {
    let preferences = owner.host.preferences.read()?;
    let outcome = owner
        .host
        .themes
        .delete(&params.theme_id, preferences.theme())?;
    if outcome == MutationOutcome::Applied {
        announce_once(&mut owner.announced, &call.origin);
    }
    Ok(json!({"outcome": outcome, "deleted": outcome == MutationOutcome::Applied}))
}
