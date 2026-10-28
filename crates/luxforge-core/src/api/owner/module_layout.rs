//! `module.list` and the module layout methods (`docs/design/module-manager.md`, Descriptor and
//! API). The catalog owner answers them from its registry and the person's `module_layout` in
//! `preferences.json`, through one bounded read or one locked read-modify-write each. The layout
//! is presentation only: none of these reads or writes a catalog, plans, renders or changes the
//! registry, so `schema.list` and every render are the same whatever it holds. Each write that
//! changes the stored layout announces one `module.layout` event under its request and answers
//! the new layout.
use super::{Call, Owner};
use crate::{
    AssetId, EditorService, Error, Toolset,
    api::{
        Origin, announce_once, methods,
        params::{NoParams, host_params},
    },
    module_layout::{Arrangement, LAYOUT_EVENT, MAX_LAYOUT_ID, StoredLayout},
};
use serde_json::{Value, json};

host_params! {
    /// `module.list`.
    pub(in crate::api) struct ModuleList {
        asset_id: Option<AssetId> = asset().notes("keep only the modules that apply to this asset's source kind; default every module"),
    }
}

host_params! {
    /// `module.enable` and `module.disable`.
    pub(in crate::api) struct LayoutModule {
        module: String = string(MAX_LAYOUT_ID).notes("a registered module's id, as module.list answers it"),
    }
}

host_params! {
    /// `module.order.set`.
    pub(in crate::api) struct LayoutOrder {
        toolset: Toolset = enumeration(Toolset::ORDERABLE.map(Toolset::as_str)),
        order: Vec<String> = json("[module id]: every module with a placement in toolset, each exactly once, in the order to draw them; module.layout.read answers the current order and the default"),
    }
}

host_params! {
    /// `module.layout.reset`.
    pub(in crate::api) struct LayoutReset {
        toolset: Option<Toolset> = enumeration(Toolset::ORDERABLE.map(Toolset::as_str)).notes("restore only this toolset's order; absent restores every order and turns every module on"),
    }
}

/// The stored layout, or the defaults when nothing is stored.
fn stored(owner: &Owner) -> Result<Option<StoredLayout>, Error> {
    Ok(owner.host.preferences.read()?.module_layout)
}

/// `module.list`: the registered modules that apply to the named photograph's kind, or every one,
/// each with its origin, placements and switch, and the host's descriptors beside them. Discovery
/// never fails on the layout: preferences that cannot be read list every module on, and
/// `module.layout.read` reports why.
pub(in crate::api) fn list(
    owner: &mut Owner,
    _: &Call<'_>,
    params: ModuleList,
) -> Result<Value, Error> {
    let stored = stored(owner).ok().flatten();
    listing(&owner.service, stored.as_ref(), params.asset_id.as_ref())
}

/// `module.list`'s answer from `service`'s registry and the stored layout.
pub(in crate::api) fn listing(
    service: &EditorService,
    stored: Option<&StoredLayout>,
    asset_id: Option<&AssetId>,
) -> Result<Value, Error> {
    let kind = asset_id
        .map(|id| service.state(id))
        .transpose()?
        .map(|state| state.asset.source.tag());
    let registry = service.registry();
    let arrangement = Arrangement::new(registry.descriptors());
    let modules = arrangement.listing(stored, methods::listed_modules(registry, kind));
    // The host's own descriptors beside them, in the same shape, so an agent discovers a `mask.*`
    // command, its parameters and its controls exactly as it discovers a module action.
    Ok(json!({"modules": modules, "host": registry.host_descriptors()}))
}

/// `module.layout.read`.
pub(in crate::api) fn read(owner: &mut Owner, _: &Call<'_>, _: NoParams) -> Result<Value, Error> {
    let stored = stored(owner)?;
    let arrangement = Arrangement::new(owner.service.registry().descriptors());
    methods::value(arrangement.view(stored.as_ref()))
}

/// One locked write of the layout through `change`, announced when it changed the stored layout,
/// answered as `module.layout.read` is.
fn write(
    owner: &mut Owner,
    call: &Call<'_>,
    change: impl FnOnce(&Arrangement<'_>, &mut StoredLayout) -> Result<bool, Error>,
) -> Result<Value, Error> {
    let arrangement = Arrangement::new(owner.service.registry().descriptors());
    let published = owner
        .host
        .preferences
        .update_module_layout_published(|stored| {
            let changed = change(&arrangement, stored)?;
            Ok((changed, arrangement.view(Some(stored))))
        })?;
    if published.value.0 {
        let origin = Origin::new(LAYOUT_EVENT, &call.origin.request_id);
        announce_once(&mut owner.announced, &origin);
    }
    let (_, view) = published.finish()?;
    methods::value(view)
}

/// `module.enable`.
pub(in crate::api) fn enable(
    owner: &mut Owner,
    call: &Call<'_>,
    params: LayoutModule,
) -> Result<Value, Error> {
    write(owner, call, |arrangement, stored| {
        arrangement.set_enabled(stored, &params.module, true)
    })
}

/// `module.disable`.
pub(in crate::api) fn disable(
    owner: &mut Owner,
    call: &Call<'_>,
    params: LayoutModule,
) -> Result<Value, Error> {
    write(owner, call, |arrangement, stored| {
        arrangement.set_enabled(stored, &params.module, false)
    })
}

/// `module.order.set`.
pub(in crate::api) fn order_set(
    owner: &mut Owner,
    call: &Call<'_>,
    params: LayoutOrder,
) -> Result<Value, Error> {
    write(owner, call, |arrangement, stored| {
        arrangement.set_order(stored, params.toolset, &params.order)
    })
}

/// `module.layout.reset`.
pub(in crate::api) fn reset(
    owner: &mut Owner,
    call: &Call<'_>,
    params: LayoutReset,
) -> Result<Value, Error> {
    write(owner, call, |arrangement, stored| {
        arrangement.reset(stored, params.toolset)
    })
}
