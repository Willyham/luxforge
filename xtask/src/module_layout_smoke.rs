//! The `module-layout` smoke scenario: the person's module layout arranged through the desktop's
//! own gestures, on the real editor in the background.
//!
//! The module manager experiment (`modules.manager`) is turned on first, as another client turns it
//! on. The photograph with a Vignette layer (`orientation-1.jpg`, Amount -60) is opened, then another
//! photograph (`orientation-6.jpg`), on which a band's menu moves Vignette to the top of the panel
//! and turns it off. Reopening the first photograph draws its Off · edit kept band at Vignette's
//! place. Customize panel… opens Settings › Modules at Vignette, the Tools group moves Crop ahead of
//! Corrections, and Reset all puts everything back.
//!
//! Every gesture's frame records the layout the desktop draws by and the owner's
//! `module.layout.read` and `module.list` read beside it, which must agree, and the layout must
//! show what the gesture did. The layout is presentation only, so the photograph a frame draws
//! must be the same bytes whatever the layout: the SHA-256 of each photograph's on-screen pixels is
//! the same in every frame that shows it with nothing drawn over it.
use crate::{
    scenario::{Checked, Checks, Frame, Plan, Run, Step, plan::only},
    *,
};
use luxforge_core::VIGNETTE_EFFECT;
use luxforge_evidence::{
    self as script, ModuleCustomize, ModuleMove, ModuleSwitch, ModuleTabMove, ModulesStep,
};
use std::collections::BTreeMap;

pub const SCENARIO: &str = "module-layout";
/// The photograph given a Vignette layer, opened first.
pub const FIXTURE: &str = "fixtures/s0/orientation-1.jpg";
/// The photograph without one, on which the layout is arranged.
const OTHER: &str = "fixtures/s0/orientation-6.jpg";
const VIGNETTE: &str = "luxforge.vignette";
const BASIC: &str = "luxforge.basic";
const CROP: &str = "luxforge.crop";
const CORRECTIONS: &str = "luxforge.corrections";
const AMOUNT: f64 = -60.0;

fn modules(name: &str, step: ModulesStep) -> Step {
    Step::new(name, script::Step::Modules(step)).commits(0)
}

/// Every frame, in order: the open, then one per step.
pub fn plan(_: &[PathBuf]) -> Plan {
    Plan::new(vec![
        Step::opened("opened").no_layer(VIGNETTE_EFFECT),
        // 1: the module manager experiment on, as another client turns it on: its gestures are
        // offered only then.
        Step::new(
            "manager-on",
            script::Step::agent(
                "flags.set",
                json!({"flag": "modules.manager", "value": true}),
            ),
        )
        .commits(0),
        // 2: the first photograph's Vignette layer, through the API, with every module on.
        Step::new(
            "vignette-layer",
            script::Step::call("edit.set-vignette", json!({"amount": AMOUNT})),
        )
        .commits(1)
        .payload(VIGNETTE_EFFECT, json!({"amount": AMOUNT})),
        // 3: the other photograph, without one. An open's history is the photograph's, so it
        // expects no count of commits against the frame before.
        Step::new("other-photo", script::Step::Open { path: OTHER.into() }),
        // 4: Basic collapsed, so every band and the footer are on screen.
        Step::new("basic-collapsed", script::Step::section(BASIC, false))
            .commits(0)
            .collapsed(BASIC),
        // 5: Vignette's band menu, opened by a secondary click.
        modules("band-menu", ModulesStep::BandMenu(VIGNETTE.into())),
        // 6: its Move to top.
        modules(
            "panel-moved",
            ModulesStep::Move(ModuleMove {
                module: VIGNETTE.into(),
                to: "top".into(),
            }),
        ),
        // 7: its Turn off Vignette.
        modules(
            "vignette-off",
            ModulesStep::Enabled(ModuleSwitch {
                module: VIGNETTE.into(),
                on: false,
            }),
        ),
        // 8: the photograph with the Vignette layer, opened again while Vignette is off.
        Step::new(
            "kept-band",
            script::Step::Open {
                path: FIXTURE.into(),
            },
        )
        .payload(VIGNETTE_EFFECT, json!({"amount": AMOUNT})),
        // 9: its kept band expanded: the values, read-only, and Enable Vignette.
        Step::new("kept-expanded", script::Step::section(VIGNETTE, true))
            .commits(0)
            .expanded(VIGNETTE),
        // 10: the band menu's Customize panel… opens Settings › Modules at Vignette.
        modules(
            "customize",
            ModulesStep::Customize(ModuleCustomize {
                toolset: "adjustments".into(),
                module: Some(VIGNETTE.into()),
            }),
        ),
        // 11: the Tools group's Crop row, moved up with ⌥↑, ahead of Corrections.
        modules(
            "strip-moved",
            ModulesStep::TabMove(ModuleTabMove {
                toolset: "tools".into(),
                module: CROP.into(),
                to: "up".into(),
            }),
        ),
        // 12: the sheet closed: the strip in its new order, the kept band at the top.
        Step::new("settings-closed", script::Step::settings(false)).commits(0),
        // 13: the strip's ⋯ menu: Mask, then Crop ahead of Corrections.
        modules("strip-menu", ModulesStep::StripMenu),
        // 14: its Customize tools… opens Settings › Modules at the Tools group.
        modules(
            "customize-tools",
            ModulesStep::Customize(ModuleCustomize {
                toolset: "tools".into(),
                module: None,
            }),
        ),
        // 15: Reset all: every module on, every order the default.
        modules("reset", ModulesStep::ResetAll),
        // 16: the sheet closed: the default panel and strip, Vignette's own band with its layer.
        Step::new("after-reset", script::Step::settings(false)).commits(0),
    ])
}

/// The adjustments or tools order of a `module.layout.read` answer.
fn order<'a>(layout: &'a Value, toolset: &str) -> Vec<&'a str> {
    layout["toolsets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| entry["toolset"] == toolset)
        .and_then(|entry| entry["order"].as_array())
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

fn default_order<'a>(layout: &'a Value, toolset: &str) -> Vec<&'a str> {
    layout["toolsets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| entry["toolset"] == toolset)
        .and_then(|entry| entry["default"].as_array())
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

fn disabled(layout: &Value) -> Vec<&str> {
    layout["disabled"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

/// What a frame records of the layout the workspace draws by.
fn drawn(frame: &Frame) -> &Value {
    &frame.state()["module_layout"]
}

/// The tools panel's bands, in drawn order.
fn bands(frame: &Frame) -> Vec<&str> {
    drawn(frame)["bands"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|band| band["module"].as_str())
        .collect()
}

fn band<'a>(frame: &'a Frame, module: &str) -> Option<&'a Value> {
    drawn(frame)["bands"]
        .as_array()?
        .iter()
        .find(|band| band["module"] == module)
}

fn strip(frame: &Frame) -> Vec<&str> {
    drawn(frame)["strip"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

fn position(list: &[&str], id: &str) -> Option<usize> {
    list.iter().position(|item| *item == id)
}

/// The owner's reads a `modules` step recorded beside its frame.
fn owner(frame: &Frame) -> Result<(&Value, &Value)> {
    let owner = &frame["step"]["owner"];
    ensure(
        owner.is_object(),
        format!(
            "The step {} records no owner reads",
            frame["step"]
                .to_string()
                .chars()
                .take(120)
                .collect::<String>()
        ),
    )?;
    Ok((&owner["module_layout_read"], &owner["module_list"]))
}

/// A `module.list` entry's switch.
fn listed_enabled(list: &Value, module: &str) -> Option<bool> {
    list.as_array()?
        .iter()
        .find(|entry| entry["id"] == module)?["enabled"]
        .as_bool()
}

/// The SHA-256 of the photograph's on-screen pixels, inside the canvas.
fn render_hash(frame: &Frame) -> Result<String> {
    let [left, top, right, bottom] = frame.visible_photo()?;
    let image = frame.image()?;
    let mut hasher = Sha256::new();
    hasher.update(format!("{left},{top},{right},{bottom};").as_bytes());
    for y in top..bottom {
        for x in left..right {
            hasher.update(image.get_pixel(x, y).0);
        }
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// The photograph's history entry on screen: one per photograph here, since no layout step
/// commits anything.
fn photo(frame: &Frame) -> Option<&str> {
    frame.state()["stack"]["entry"].as_str()
}

/// Nothing is drawn over the photograph: no Settings sheet and no menu.
fn unobstructed(frame: &Frame) -> bool {
    frame.state()["settings"]["open"].is_null() && drawn(frame)["menu"].is_null()
}

/// What each frame shows of the layout, correlated with the owner's reads, and the photograph's
/// bytes the same in every unobstructed frame of it.
pub fn verify(_: &mut Run, launches: &[Checked]) -> Result {
    let launch = only(launches)?;
    let mut checks = Checks::new();

    // Every `modules` step's frame: the desktop draws by exactly the layout the owner answers.
    for (name, frame) in launch.names().iter().zip(&launch.frames) {
        if frame["step"]["owner"].is_null() {
            continue;
        }
        let (read, _) = owner(frame)?;
        ensure(
            &drawn(frame)["layout"] == read,
            format!(
                "{name}: the desktop draws by {} but the owner holds {read}",
                drawn(frame)["layout"]
            ),
        )?;
        ensure(
            drawn(frame)["pending"] == false,
            format!("{name}: a layout write is still in flight"),
        )?;
    }

    let opened = launch.at("vignette-layer")?;
    let default_panel = bands(opened);
    let default_strip = strip(opened);
    ensure(
        position(&default_strip, CORRECTIONS) < position(&default_strip, CROP),
        format!("The default strip does not put Corrections before Crop: {default_strip:?}"),
    )?;
    ensure(
        band(opened, VIGNETTE).is_some_and(|band| band["kept"].is_null()),
        "Vignette's band is not drawn as an ordinary band while it is on",
    )?;

    let menu = launch.at("band-menu")?;
    let items = &drawn(menu)["band_menu"];
    ensure(
        items["module"] == VIGNETTE && items["turn_off"] == "Turn off Vignette",
        format!("Vignette's band menu offers no Turn off Vignette: {items}"),
    )?;
    checks.note(menu, "Vignette's band menu", items.clone());

    let moved = launch.at("panel-moved")?;
    let (read, _) = owner(moved)?;
    ensure(
        order(read, "adjustments").first() == Some(&VIGNETTE)
            && bands(moved).first() == Some(&VIGNETTE),
        format!(
            "Move to top did not put Vignette first: panel {:?}, owner {:?}",
            bands(moved),
            order(read, "adjustments")
        ),
    )?;
    checks.note(
        moved,
        "Vignette moved to the top of the panel",
        json!({"bands": bands(moved), "owner_order": order(read, "adjustments")}),
    );

    let off = launch.at("vignette-off")?;
    let (read, list) = owner(off)?;
    ensure(
        disabled(read) == [VIGNETTE] && listed_enabled(list, VIGNETTE) == Some(false),
        format!("Vignette is not off in the owner's layout: {read}"),
    )?;
    ensure(
        band(off, VIGNETTE).is_none(),
        "A photograph without a Vignette layer still draws Vignette's band while it is off",
    )?;
    ensure(
        drawn(off)["off_footer"] == "1 module off: Vignette",
        format!("The footer reads {}", drawn(off)["off_footer"]),
    )?;
    checks.note(
        off,
        "Vignette off: no band, the footer names it",
        json!({"bands": bands(off), "footer": drawn(off)["off_footer"], "disabled": disabled(read)}),
    );

    let kept = launch.at("kept-band")?;
    let kept_band = band(kept, VIGNETTE).ok_or("The reopened photograph draws no Vignette band")?;
    ensure(
        kept_band["kept"].is_array() && bands(kept).first() == Some(&VIGNETTE),
        format!(
            "Vignette's band is not kept at its place while off: {kept_band} in {:?}",
            bands(kept)
        ),
    )?;
    let expanded = launch.at("kept-expanded")?;
    ensure(
        band(expanded, VIGNETTE)
            .is_some_and(|band| band["kept"].is_array() && band["expanded"] == true),
        "The kept band did not expand",
    )?;
    checks.note(
        expanded,
        "the Off · edit kept band, expanded, with its values",
        band(expanded, VIGNETTE).cloned().unwrap_or_default(),
    );

    let customize = launch.at("customize")?;
    let tab = &customize.state()["settings"];
    ensure(
        tab["open"] == "modules",
        format!("Customize panel… opened Settings at {}", tab["open"]),
    )?;
    let selected: Vec<&Value> = tab["modules"]["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|group| group["rows"].as_array().into_iter().flatten())
        .filter(|row| row["selected"] == true)
        .collect();
    ensure(
        selected.len() == 1
            && selected[0]["module"] == VIGNETTE
            && selected[0]["control"]["switch"] == false,
        format!("Settings › Modules does not select Vignette, off: {selected:?}"),
    )?;
    checks.note(
        customize,
        "Settings › Modules at Vignette",
        json!({"detail": tab["modules"]["detail"], "row": selected[0]}),
    );

    let strip_moved = launch.at("strip-moved")?;
    let (read, _) = owner(strip_moved)?;
    ensure(
        order(read, "tools") == [CROP, CORRECTIONS],
        format!("The Tools group's move left {:?}", order(read, "tools")),
    )?;
    let closed = launch.at("settings-closed")?;
    ensure(
        position(&strip(closed), CROP) < position(&strip(closed), CORRECTIONS),
        format!("The strip does not draw Crop first: {:?}", strip(closed)),
    )?;
    checks.note(
        closed,
        "the strip in the person's order",
        json!({"strip": strip(closed)}),
    );

    let strip_menu = launch.at("strip-menu")?;
    let listed: Vec<&str> = drawn(strip_menu)["strip_menu"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tool| tool["module"].as_str())
        .collect();
    ensure(
        listed == [CROP, CORRECTIONS],
        format!("The strip's ⋯ menu lists {listed:?}"),
    )?;
    let tools_tab = launch.at("customize-tools")?;
    ensure(
        tools_tab.state()["settings"]["open"] == "modules",
        "Customize tools… did not open Settings › Modules",
    )?;

    let reset = launch.at("reset")?;
    let (read, list) = owner(reset)?;
    ensure(
        disabled(read).is_empty()
            && order(read, "adjustments") == default_order(read, "adjustments")
            && order(read, "tools") == default_order(read, "tools")
            && listed_enabled(list, VIGNETTE) == Some(true),
        format!("Reset all left {read}"),
    )?;
    let after = launch.at("after-reset")?;
    ensure(
        bands(after) == default_panel && strip(after) == default_strip,
        format!(
            "After Reset all the panel draws {:?} and the strip {:?}, not {default_panel:?} and \
             {default_strip:?}",
            bands(after),
            strip(after)
        ),
    )?;
    ensure(
        band(after, VIGNETTE).is_some_and(|band| band["kept"].is_null()),
        "Vignette's band is still kept after Reset all",
    )?;

    // The render: each photograph's on-screen bytes in every frame that shows it unobstructed,
    // after its Vignette layer: the frames before it show its Original.
    let mut renders: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for (name, frame) in launch.names().iter().zip(&launch.frames) {
        if ["opened", "manager-on"].contains(&name.as_str()) || !unobstructed(frame) {
            continue;
        }
        let Some(asset) = photo(frame) else {
            continue;
        };
        renders
            .entry(asset.to_owned())
            .or_default()
            .push((name.clone(), render_hash(frame)?));
    }
    ensure(
        renders.len() == 2,
        format!("Expected two photographs' renders, found {}", renders.len()),
    )?;
    for (asset, hashes) in &renders {
        ensure(
            hashes.len() >= 2,
            format!("{asset} is shown unobstructed in fewer than two frames"),
        )?;
        let first = &hashes[0].1;
        ensure(
            hashes.iter().all(|(_, hash)| hash == first),
            format!("{asset}'s on-screen render changed with the layout: {hashes:?}"),
        )?;
    }
    let vignetted = photo(opened).ok_or("The first photograph is not named")?;
    let kept_frames: Vec<&str> = renders[vignetted]
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    ensure(
        ["vignette-layer", "kept-band", "after-reset"]
            .iter()
            .all(|name| kept_frames.contains(name)),
        format!(
            "The Vignette photograph's render is not compared across its layouts: {kept_frames:?}"
        ),
    )?;

    checks.write(
        &launch.evidence,
        "module-layout",
        json!({
            "renders": renders,
            "scope": "SHA-256 of the photograph's on-screen pixels, read back from the window's \
                      renderer, in every frame showing it with no sheet or menu over it; the \
                      layout each frame draws by against the owner's module.layout.read and \
                      module.list read beside it",
        }),
    )
}
