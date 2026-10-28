# Module manager screens

Screen notes for the [module manager board](board.html). The design and its recorded defaults are in [module manager](../module-manager.md).

- **Phase 1** turns built-in modules off and on and reorders the tools panel's sections and the mode strip's tools.
- **Phase 2** installs an external adjustment section from a package file, and in developer mode loads one from a folder.

Phase 2 boards are drawn only to show that phase 1 leaves room for them. Every external module named on the board is invented.

## Terms

- **Toolset**: where a module's UI lives. *Adjustments* is the tools panel's sections, *Tools* the mode strip's takeovers, *Corrections* the Corrections band's kinds, and *Masking* the New mask and Add component kinds. Only Adjustments and Tools reorder. Corrections and mask kinds follow their module's switch and keep a fixed order.
- **Placement**: one module's row in one toolset.
- **Home**: the placement that holds a module's one switch. Crop's home is Adjustments, and the strip's R follows it. Corrections' home is Tools (Q), and Heal and Clone follow it.
- **Off**: hidden from the workspace (section, kinds, strip entry, palette actions, shortcuts) but still registered, so photos that use it render and export unchanged. Off means the same for a built-in and an external module.
- **Unavailable**: the delivered red state, in which rendering refuses. *Not installed* and *Didn't load* are kinds of it.
- **Locked**: can't be turned off, though it can be moved. Locked are Crop, transform, straighten; Basic; the RAW development (processing only, no section); and the host's Mask, which also stays first in the strip.

## Board A: Settings › Modules (phase 1)

- **The tab.** A Modules tab sits after Appearance, with a four-square icon, in every build. It opens from Cmd+, (then the tab), from the palette (*Settings · Modules*), and from Customize in Develop, which selects the module it came from.
- **Groups and rows.** Groups run Adjustments, Corrections, Tools, Masking, then Processing only. A row has a grip (absent where the order is fixed), the title, an optional qualifier ("RAW photos only"), a letter for a strip tool, and a switch or a lock. An off row dims and keeps its place.
- **The detail pane.** It shows title and origin badge, the hint, where the module lives, where it renders (the seven stages, its own underlined, with "Panel order doesn't change this"), what Off does to its photos' edits (nothing), and its API.
- **Drag.** Press the grip and the lifted row follows the pointer, a ghost holds its old place, and an accent line marks where it lands, as in the mask list. A row stays inside its group. Release writes one `module.order.set`; Escape, or leaving the list, cancels.
- **Keyboard.** ↑ ↓ select. ⌥↑ ⌥↓ move one place. ⌥⇧↑ ⌥⇧↓ move to the group's ends. Space toggles. Tab reaches the detail pane. Pressing a locked row's switch shows the reason in the detail pane.
- **Undo.** Preferences have no history entries. Each change shows a one-step **Undo** toast, which the next change replaces; ⌘Z does the same while it shows.
- **Reset.** **Reset order** works per toolset. **Reset all** restores every order and turns every module on, with no confirmation since no photo changes. Writes happen at once, and other clients' changes arrive through the `module.layout` event.

## Board B: Develop with a customised panel (phase 1)

- Sections draw in the person's order, and history records nothing. The status bar reads "Moved Tone curve above Presence in your panel · rendering unchanged".
- A secondary click on a band opens Move up (⌥↑), Move down (⌥↓), Move to top, Move to bottom, Turn off *Title* (absent when locked), Customize panel… and Copy as JSON request. The panel takes no drags: a band press still expands or collapses it.
- A footer after the last section names the modules that are off and offers Customize…; with every module on there is no footer. The palette has *Customize panel…*, *Turn off Title* and *Enable Title*.

## Board C: a photo with edits from a module that is off (phase 1)

- A photo with a non-neutral layer from an off module shows that module's band at its place in the order. Its title is in secondary text, with the accent dot and the hint "Off · edit kept" in grey: the picture is whole. It starts collapsed.
- Expanded, it says that the edit is kept and renders and exports. It lists the values read-only from the `recipe.describe` row, then **Enable Vignette** (`module.enable`, for every photo), with no controls or reset.
- History, undo, redo and restore are unaffected, and a neutral layer brings no band back.
- Board C · 2 fixes the band states. *Off* is grey. *Unavailable* (delivered), *Not installed* and *Didn't load* are red and come with the Preview is stale notice.

## Board D: the mode strip's ⋯ menu (phase 1)

- The ⋯ sits at the strip's end, after a rule, and the same menu opens on a secondary click on the strip. It lists every strip tool with a check for off and on, a lock for Mask and Crop, and the letter, then Customize tools… (Settings › Modules at Tools, where tools are reordered). Off tools stay listed, unchecked.
- **Letters**: a tool's letter is its module's declaration. Moving a tool never changes it. An off tool gives up its letter and palette entry until enabled.
- **Collisions**: the first claimant keeps the letter, built-ins first. A later one gets none rather than a substitute, and its tooltip says whose the letter is. This is a rule for later kinds of module: phase 2's adjustment sections declare no canvas.

## Board E: install from a package file (phase 2)

- **Install module…** in the tab's header opens the system file picker, which the board doesn't draw. `module.package.inspect {path}` reads the manifest for the one modal step, the review sheet.
- **The review sheet.** It names module and version and says the module comes from its author and that **Luxforge hasn't reviewed it**. It lists what the module adds (a section in Adjustments, last) and where it renders, then its program: a GPU program the host validates against the module's CPU reference, or a CPU reference only. Network and Files show as None. Then come the manifest's author, SPDX licence, source repository, version and interface, and the file.
- **Refusals.** A manifest that can't be read, or that declares anything but an adjustment section, refuses with the reason and offers no Install.
- **Install** (`module.install {path}`) takes effect on the next launch. The row joins Adjustments at once with the Community badge and **Relaunch to apply**. A line under the header counts the pending changes and offers **Relaunch**, which reopens the same catalog and photo. Closing the sheet keeps the changes pending.
- After the relaunch the row is ordinary: grip, switch, and a Community badge it keeps for good. The module's band carries the same badge.
- A later community library would add a Browse view to this tab. Nothing plans it, so it isn't drawn.

## Board F: installed module states (phase 2)

- **Badges.** Origin badges are grey: *Community* (installed from a file) and *From folder* (developer). State badges are *Relaunch to apply* (accent) and *Didn't load* (red).
- **Didn't load.** It quotes the loader's reason from `module.status` word for word and keeps the module's place and its photos' edits. It offers Install module… (a version built for this Luxforge), Copy error and Uninstall….
- **From folder.** **Load from folder…** (`module.folder.load {path}`) appears only in developer mode. The row shows the path; changes load at the next launch, since a reload is a relaunch. **Remove** sends `module.folder.remove {module}`.
- **Update.** Choosing a newer package file with the same id opens the review sheet as **Replace … with …?**. If the manifest says the new version can't read the formats existing edits use, the sheet says so in red. **Replace anyway** is then red and not the default, and the photos keep their edits. There are no automatic updates and no store of earlier versions.
- **Uninstall…** says the photos that use the module keep their edits but can't be previewed with them or exported, without counting them. Other catalogs aren't checked. `module.uninstall {module}` takes effect on relaunch.
- **After uninstalling.** The Preview is stale notice names the module and offers Install module…. Its band reads *Not installed*, and Export is refused with the reason in the status bar.

## Programmatic equivalents

| Gesture | Method |
| --- | --- |
| Read toolsets, orders, on and off, locks, origins | `module.layout.read`; `module.list` gains `toolset`, `enabled`, `locked`, `origin` and `placements` |
| Switch, Turn off *Title*, ⋯ check, Enable *Title* | `module.enable {module}`, `module.disable {module}`; a locked module refuses with its reason |
| Drag, ⌥↑ ⌥↓, Move up, down, to top or bottom | `module.order.set {toolset, order}`, the whole order of `adjustments` or `tools` |
| Reset order, Reset all, Undo toast | `module.layout.reset {toolset?}`; Undo sends the inverse `module.order.set`, `module.enable` or `module.disable` |
| Customize…, Settings · Modules | Desktop view state, like the sheet's tab: no method |
| Install module…, the review sheet, Replace | `module.package.inspect {path}`, then `module.install {path}` |
| Uninstall… | `module.uninstall {module}` |
| Load from folder…, Remove | `module.folder.load {path}`, `module.folder.remove {module}`, developer mode only |
| Didn't load, Copy error, Relaunch to apply | `module.status` reports the load state and reason |
| Relaunch | Desktop only: it reopens the app |

Changes announce `module.layout` or `module.installed`, so open sheets, panels and agents follow them. An off module's actions stay callable through the API, and `schema.list` is unchanged.

## Recorded defaults

Recorded in [module manager](../module-manager.md):

- The words are **Off** and **Enable**.
- Locked: Crop, transform, straighten; Basic; RAW development; and the host's Mask.
- One switch per module, at its home placement. No drag in the tools panel itself. Masking is one locked summary row.
- No photo count in phase 1, and none in the phase 2 uninstall confirmation, which states the consequence for every photo that uses the module.
- Install, update, uninstall and folder load take effect on the next launch, with **Relaunch to apply** and **Relaunch**.
- Phase 2 modules get no capabilities. The review lists Network none, Files none, the program, and the manifest's author, licence, source repository and version.
- Trust marks are Built in, Community and From folder. Every external module says it is not reviewed by Luxforge.
