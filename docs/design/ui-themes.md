# UI themes

Status: **implemented.** The owner asked on 2026-10-04 for UI themes, ideally importing the themes [Omarchy](https://github.com/omacom/omarchy) ships. On 2026-10-05 the owner accepted four things ([decisions](../decisions.md#ui-themes)):

- themes, light ones included, in place of "Dark theme only; a light theme is not planned" ([product decisions](../decisions.md#develop-workspace), 2026-09-20);
- a surround held neutral;
- each theme's own accent;
- a few bundled Omarchy themes.

The visual language's first principle, "the photograph is the only colour on screen" ([Develop workspace](develop-workspace.md#principles)), now holds for the default theme alone. The remaining questions run on the [recorded defaults](#proposals-with-recorded-defaults) below.

## Where it lives

- **The model**, in `luxforge_core::theme`: roles, the derivation, the chroma bound, the contrast floors and their moves, the report and the Luxforge theme document. CIEDE2000 joins the core's other colour equations in `colour.rs`.
- **The Omarchy reader**, `theme::omarchy`, and the six bundled palettes beside it under `theme/omarchy/bundled/`.
- **The library**, `theme::library`, and the `theme.*` methods in `api/owner/themes.rs`.
- **The runtime theme**, `luxforge_ui::Theme` and its `Palette`, in `crates/luxforge-ui/src/theme/`. `theme.rs` keeps the sizes, the fonts and the fixed colours.
- **The desktop's theme**: the mapping from the core's tokens, the launch read, the change and the fallback in `app/themes.rs`; the folder import in `app/theme_folder.rs`; the Appearance tab's plain data in `state/themes.rs`.

The [preferences](preferences.md) the owner decided on 2026-10-04 meet themes in two places. The **canvas background** fills the canvas around the photograph: Theme, the theme's surround and the default; Dark, `#19191b`; Black; or an 18% grey, `#777777`. The **interface size** scales everything Iced draws, whatever the theme.

## What a theme is

A theme is a named set of colours for the interface, chosen by the person and kept outside every catalog. The default theme, **Luxforge Dark**, is the [visual language](develop-workspace.md#visual-language) exactly. Six popular Omarchy themes are built in beside it ([bundled themes](#bundled-themes)). Any other theme comes from an import, of an Omarchy theme folder or of a Luxforge theme document, and is stored as a Luxforge theme, so once imported it no longer depends on the folder it came from. The desktop draws it; every client can read it; nothing else in Luxforge reads it.

## Rules

- **A theme never changes a photograph.** It changes no recipe, history entry, render, histogram, sample or export byte. No catalog or recipe names it, and switching theme reads no asset, starts no preview job and writes no catalog. This is the [flags'](settings-and-flags.md#rules) rule, applied to the person's appearance.
- **Only colours.** Type, sizes, spacing, radii and the panel density are the visual language's in every theme and keep their tests, and the interface size preference scales them alike whatever the theme. The density acceptance (the histogram, Basic and every section header on screen at 1440 × 900 at 100%) holds unchanged.
- **The photograph's surround stays neutral.** A theme's surround is held to at most 0.010 OKLCh chroma, at the theme's own lightness. Luxforge Dark's canvas, `#19191b`, carries 0.004, and none of its surfaces reaches 0.010. A tinted surround shifts how the photograph's colours are seen, which is why colour-critical viewing asks for a neutral one. A theme chooses how dark or light the surround is, never its hue.
  - The histogram plot always draws the theme's surround.
  - Around the photograph, the canvas background preference decides. Its Dark, Black and Grey choices stay the fixed greys they are in every theme.
  - A fourth choice, **Theme**, the default, draws the theme's surround. For Luxforge Dark, Theme and Dark are the same `#19191b`, so nothing on screen changes for a person who never chose.
- **Colours whose meaning is their colour are never themed.** These are:
  - the clipping red, blue and both-endpoint magenta of the triangles and overlays;
  - the mask overlay's green and white;
  - the histogram's channel fills;
  - the canvas background's Dark, Black and Grey;
  - the status bar's green dot for a connected agent;
  - every colour rail a module declares, which keeps its own stops;
  - colour swatches and the colour picker;
  - the guides, handles and labels the crop, mask and compare canvases draw over the photograph.

  A declared rail is laid over the panel at 85%, so its backdrop is held to the surround's chroma bound too, and a tinted panel cannot tint a temperature or hue rail. The clipping and mask colours keep the measured distances between them that the visual language chose.
- **Every theme is legible.** Each ink meets a contrast floor against every surface it is drawn on ([legibility](#legibility)), checked whenever a theme is stored. Luxforge Dark meets every floor.
- **Nothing is discarded silently.** An import's report says what it mapped, derived, adjusted and ignored. A stored theme this build cannot read is kept, listed with its reason and never rewritten. An active theme that is missing or unreadable leaves Luxforge Dark on screen with the reason in the status bar, and the stored choice is not changed.
- **Every theme operation is programmable.** The Appearance tab reads and writes through the [methods](#methods) any client uses, as the Experiments tab does through `flags.*`.

## The theme model

### Roles

A theme declares roles. Every colour the widgets draw is a role, a token derived from roles, or one of the fixed colours above.

| Role | Luxforge Dark | Drawn as |
| --- | --- | --- |
| `background` | `#202023` | Panel: the side panels, the status bar and the Settings sheet; the origin of the surface ramp |
| `surround` | `#19191b` | Canvas: behind the histogram plot, and around the photograph when the canvas background is Theme; held near-neutral |
| `surface` | `#232326` | Bar: the title bar, floating strips, notices and module bands |
| `control` | `#2c2c31` | Buttons, chips and text fields |
| `text` | `#e8e8ea` | Primary text |
| `text_secondary` | `#a8a8ae` | Secondary text |
| `text_tertiary` | `#77777f` | Tertiary text and disabled controls |
| `accent` | `#e2b46a` | The current entry, the active mode, the edited dot, a dragged thumb, Apply and the render bar |
| `accent_ink` | `#1a1408` | Text and icons on the accent |
| `error` | `#e5534b` | Invalid values, unavailable reasons and failure outlines: the clipping red, moved in lightness only where a theme's surfaces need it |
| `mode` | `dark` | The native window's appearance |

A theme must give `background`, `text` and `accent`. `mode` follows the background's lightness when a theme does not state it (dark below 0.5 OKLab L), and every other role derives from those three.

### Derived tokens

The remaining tokens derive from the roles, each by one rule: either a mix of two roles in encoded sRGB at a fixed weight, the way the references' composites were sampled, or a role moved in OKLab lightness. They are:

- the label, faint and bright text tiers;
- hover and selected fills, and the current row's fill;
- rules and band borders, and the 6% border;
- the slider's rail, fill, zero tick, thumb and ring;
- the tab and segment tracks;
- menu surfaces and separators;
- the notice outlines;
- the mode strip's icon ink.

The weights are fitted to Luxforge Dark so that a derived theme keeps its hierarchy. A mix is per channel in encoded sRGB, rounded to the nearest code. The rules (`luxforge_core::theme`):

- **Surfaces.** The canvas, bar and control sit −0.031, +0.013 and +0.050 from the panel in OKLab lightness, keeping its hue and chroma. When a theme gives no surround it sits below the background in both modes, as Omarchy's own ramp does. The bar and the control step toward the text: lighter in a dark theme and darker in a light one.
- **Text tiers.** The text mixed toward the control by 16%, 33.5%, 59% and 77.5% gives the label, secondary, tertiary and faint tiers, within two codes of Luxforge Dark's, which carry the control's cool tint. A mix toward the background matched only within five codes in blue.
- **Inks of the text and the poles.** The bright, current-row and thumb inks are the text mixed 36%, 45.5% and 18% toward white in a dark theme (toward black when the text is darker than the background). The mode strip's fixed glyph is the text 42% toward the control; the chip label and the strip icon are the secondary tier 12.5% and 26.5% toward the text; the title bar's identity is the secondary tier 24% toward the control.
- **Rules and fills over the panel.** The text at 4.5%, 7.5%, 7.5%, 8%, 8.5%, 10% and 12.5% over the panel gives the row hover, the current row, the band border, the icon hover, the rule, the chrome border and the strip rule; over the canvas, at 4% and 5.5%, the thumbnail and histogram outlines. The secondary tier at 15.5%, 17%, 20% and 23% over the panel gives the sparkline area, the menu item hover, the menu separator and the menu border. The control at 63% and 80% over the panel gives the tab track and the menu surface; the tertiary tier at 19%, 20% and 61% over the control gives the rail, the selected tab and the zero tick, and the rail fill is the secondary tier 9.5% toward the tertiary. The thumb's ring is the panel 46% toward black (toward white in a light theme).
- **The accent and error.** The accent over the panel at 12%, 15%, 15.5% and 17% gives the fills of the mask row, the selected icon button, the selected chip and the selected mode-strip tool, and at 32% over the canvas the revealed row. The accent at 34.5% and the error at 40% over the bar give the warning and failure outlines. The accent mixed 89.5% toward black gives the accent ink.
- **Alpha.** The border is white at 6% alpha in a dark theme and black at 6% in a light one; the scrim is black at 35% in both.
- **Neutral.** The rail backdrop is the panel held to the chroma bound.

Derived from all ten of Luxforge Dark's roles, 31 of the other 39 tokens are exact, and none is more than two codes off in any channel: the label tier and the revealed row by two, and the faint tier, the strip icon, the failure outline, the zero tick, the selected icon button and the selected mode-strip tool by one. Derived from its three required roles alone, no token is more than four codes off (the zero tick); the derived canvas and control are one and two codes off, and the secondary and tertiary tiers and the accent ink three. A core test records each token's fit.

A derived text tier stops short of its fitted weight, in steps of 0.5%, where it would miss its floor, so derived inks meet the floors by construction. Besides the error ink, only a theme's own colours are ever moved, in OKLab lightness steps of 0.001, with chroma reduced at the gamut's edge; the report names each move. The accent ink mixes toward black, or toward white when the accent is dark by the WCAG luminance threshold of 0.179, which Omarchy's own guidance for applications uses. Where that mix misses its floor, the ink is pure black or white.

A theme may also set any token explicitly, which wins over its derivation. Luxforge Dark sets every token, so it draws exactly what the visual language specifies, and the visual language's table is Luxforge Dark's document. The derivation is deterministic and has golden tests.

### Tokens

The token names are the API's, and the widget crate's palette has a field of the same name for each. The core and the widget crate cannot see each other, so each keeps the list, and a desktop test holds the two lists equal. A value is `#rrggbb`, or `#rrggbbaa` for the two tokens drawn with alpha. The first ten are the colour roles.

| Token | Luxforge Dark | Replaced the constant | Drawn as |
| --- | --- | --- | --- |
| `surround` | `#19191b` | `CANVAS` | Behind the histogram plot; around the photograph when the canvas background is Theme |
| `background` | `#202023` | `PANEL` | Side panels, status bar, Settings sheet |
| `surface` | `#232326` | `BAR` | Title bar, floating strips, notices, module bands |
| `control` | `#2c2c31` | `CONTROL` | Buttons, chips, text fields |
| `text` | `#e8e8ea` | `TEXT_PRIMARY` | Primary text |
| `text_secondary` | `#a8a8ae` | `TEXT_SECONDARY` | Secondary text |
| `text_tertiary` | `#77777f` | `TEXT_TERTIARY` | Tertiary text, disabled controls |
| `accent` | `#e2b46a` | `ACCENT` | State marks |
| `accent_ink` | `#1a1408` | `PRIMARY_INK` | Ink on the accent |
| `error` | `#e5534b` | `CLIPPING_HIGHLIGHT` where it marks an invalid value or failure | Invalid values, unavailable reasons |
| `text_label` | `#c9c9ce` | `TEXT_LABEL` | Slider and field labels |
| `text_faint` | `#55555c` | `TEXT_FAINT`, `CLIP_TRIANGLE_REST` | A finished job's duration; an empty clipping triangle |
| `text_bright` | `#f0f0f2` | `TEXT_BRIGHT` | A notice's title, the file name |
| `text_identity` | `#8a8a90` | `TEXT_IDENTITY` | The title bar's dimensions and format |
| `text_current_row` | `#f2f2f4` | `TEXT_CURRENT_ROW` | The current history row's label |
| `chip_label` | `#b0b0b6` | `CHIP_LABEL` | An unselected chip's label |
| `strip_icon` | `#b9b9bf` | `STRIP_ICON` | A mode-strip tool's icon at rest |
| `mode_fixed_ink` | `#99999c` | `MODE_FIXED_INK` | A fixed mode control's glyph |
| `border` | `#ffffff0f` | `BORDER` (6% white) | Outlines |
| `scrim` | `#00000059` | The palette's and the Settings sheet's 35% black | Behind a modal sheet |
| `rule` | `#313134` | `RULE` | A group header's rule |
| `band_border` | `#2f2f32` | `BAND_BORDER`, `DIVIDER` | Module band borders, the shell's dividers |
| `chrome_border` | `#343437` | `CHROME_BORDER` | Floating chrome's outline |
| `strip_rule` | `#39393c` | `STRIP_RULE`, `NOTICE_BORDER` | The mode strip's rule, a neutral notice's outline |
| `notice_warning_border` | `#65553d` | `NOTICE_WARNING_BORDER` | A notice that needs a decision |
| `notice_error_border` | `#713634` | `NOTICE_ERROR_BORDER` | A notice that reports a failure |
| `histogram_border` | `#242426` | `HISTOGRAM_BORDER` | The histogram plot's outline |
| `thumbnail_border` | `#212123` | `THUMBNAIL_BORDER` | A coverage thumbnail's outline |
| `rail` | `#3a3a40` | `RAIL` | The empty slider rail |
| `rail_fill` | `#a3a3aa` | `RAIL_FILL` | The rail's fill |
| `rail_backdrop` | `#202023` | `PANEL` under a declared rail | The panel held to the surround's chroma bound, under a declared rail at 85% |
| `zero_tick` | `#5a5a62` | `ZERO_TICK` | The zero tick |
| `thumb` | `#ececee` | `THUMB` | The resting handle |
| `thumb_outline` | `#111113` | `THUMB_OUTLINE` | The handle's ring |
| `sparkline_area` | `#353539` | `SPARKLINE_AREA` | The area under a sparkline |
| `tab_track` | `#28282c` | `TAB_TRACK`, `SEGMENT_TRACK` | Tab and segment tracks |
| `tab_selected` | `#3b3b41` | `TAB_SELECTED`, `SEGMENT_SELECTED` | The selected tab or segment |
| `row_hover` | `#29292c` | `ROW_HOVER` | A row under the pointer |
| `list_row_current` | `#2f2f32` | `LIST_ROW_CURRENT` | The current row |
| `icon_hover` | `#303033` | `ICON_HOVER` | An icon button under the pointer |
| `selected_fill` | `#3e372e` | `SELECTED_FILL` | A selected chip |
| `icon_selected_fill` | `#3e372f` | `ICON_SELECTED_FILL` | A selected icon button |
| `strip_selected` | `#413a30` | `STRIP_SELECTED` | A selected mode-strip tool |
| `mask_row_selected` | `#37322c` | `MASK_ROW_SELECTED` | The open mask's row |
| `revealed_row` | `#5b4932` | `REVEALED_ROW` | A row the command palette has revealed |
| `menu_surface` | `#2a2a2e` | `MENU_SURFACE` | A menu |
| `menu_border` | `#3f3f43` | `MENU_BORDER` | A menu's outline |
| `menu_item_hover` | `#37373b` | `MENU_ITEM_HOVER` | A menu item under the pointer |
| `menu_separator` | `#3b3b3f` | `MENU_SEPARATOR` | A menu's separator |

The fixed colours keep their constants and are not tokens: `CANVAS` as the canvas background's Dark choice, `CANVAS_BLACK` and `CANVAS_GREY`; the clipping colours, `CLIPPING_BOTH` and the overlay palette; the mask overlay tints; the histogram channels; `TEMPERATURE_RAIL`, `TINT_RAIL` and every declared rail; `GUIDE`, `RENDER_BAR_TRACK` and the compare and mask canvases' `PHOTO_HANDLE`, `PHOTO_HANDLE_OUTLINE`, `PHOTO_GRIP`, `PHOTO_LABEL` and `PHOTO_ANCHOR`, drawn over the photograph; `AGENT_CONNECTED`; `SWATCH_OUTLINE`; the coverage thumbnail's black `THUMBNAIL_BACKGROUND`, which is the mask's own zero; the mask glyphs' `MASK_GLYPH_OUTLINE` and `MASK_GLYPH_PHOTO`; and the black `CHROME_SHADOW` and `MENU_SHADOW`.

Luxforge Dark's palette in the widget crate is built from these values exactly, alpha included, so it draws what the constants drew. The desktop draws that palette whenever Luxforge Dark is active, and builds any other theme's palette from the core's resolved tokens. A desktop test holds the core's Luxforge Dark within one code per channel of the widget crate's.

### Legibility

| Ink | Floor | Against | Luxforge Dark |
| --- | --- | --- | --- |
| Primary text | 7:1 | Surround, background, surface and control | 11.4 to 14.3 |
| Labels and secondary text | 4.5:1 | The same | Labels 8.4 to 10.6, secondary 5.9 to 7.4 |
| Tertiary text | 3:1 | The same | 3.1 to 4.0 |
| Accent and error | 3:1 | The same | Accent 7.3 to 9.2, error 3.8 to 4.7 |
| Accent ink | 4.5:1 | The accent | 9.6 |

The floors are WCAG 2's contrast ratios:

- 7:1 is its enhanced level for body text.
- 4.5:1 is its minimum.
- 3:1 is its floor for large text and interface marks. The accent and error inks are held to it because they mark state beside a label rather than carry body text.

The faint tier (a finished job's duration beside its label) is unchecked, as it is in Luxforge Dark, where it reads 1.9 to 2.4:1.

An imported theme's own ink (its text or accent) that misses its floor is moved in OKLab lightness, keeping its hue and chroma, just far enough to meet it. The report names the ink, the floor and both colours. A Luxforge theme document that misses a floor is refused by name, because its author chose each value.

### The accent

The accent marks state: the current history entry, the active mode, a module's edited dot, a dragged thumb, Apply and the render bar. The visual language reserves red and blue for clipping, and keeps the mask overlay's green and white apart from both. Luxforge Dark's amber is 27 to 50 ΔE00 from all four, but an imported accent can sit close to one. Among Omarchy's built-in themes, nine accents are under 15 ΔE00 from a reserved colour. Two of them are under 10: Tokyo Night's blue, Omarchy's default theme, at 8.7 from the shadow-clipping blue, and Nord's at 9.7.

Every theme keeps its own accent (owner, 2026-10-05), so an imported theme looks like itself. Its report notes an accent under 15 ΔE00 from a reserved colour. The clipping and mask colours stay recognisable by where they draw: in the histogram's corners, and as overlays on the photograph's own pixels, where the accent never draws.

## Omarchy themes

### What Omarchy ships

[Omarchy](https://github.com/omacom/omarchy) (MIT; formerly `basecamp/omarchy`) is an Arch Linux and Hyprland setup whose themes are folders. These facts were checked on 2026-10-04 against its default branch, `quattro` at `035ce29f03`, and its latest release, v4.0.4 of 2026-09-15; the two have identical theme files.

- **A theme is a folder** named by its slug. Its palette is `colors.toml`, from which every themed application's colours have been rendered since v3.3.0. The folder also holds files Luxforge has no use for: wallpapers under `backgrounds/`, an icon theme name, preview images and a few hand-written per-application files.
- **Where themes live.** Omarchy 4 ships 22 built-in themes in its repository's `themes/` directory, installed at `/usr/share/omarchy/themes`. Themes a person installs live in `~/.config/omarchy/themes`; `omarchy theme install <git-url>` clones a repository there. Omarchy 3 kept its built-ins in `~/.local/share/omarchy/themes`.
- **The palette has had three forms.**
  - Omarchy 4 uses semantic keys: `mode`, `accent`, `selection`, `muted`, the background ramp (`background`, `dark_background`, `darker_background`, `lighter_background`), four foregrounds and fourteen named colours.
  - Omarchy 3.3 to 3.8 used a terminal form: `accent`, `cursor`, `foreground`, `background`, `selection_foreground`, `selection_background` and `color0` to `color15`.
  - Before 3.3 there was no palette file. Omarchy still derives one from a theme's `alacritty.toml`, taking the terminal's normal blue as the accent.

  Omarchy's resolver, `bin/omarchy-theme-color`, reads both `colors.toml` forms; `bin/omarchy-theme-colors-from-alacritty` first writes a `colors.toml` for a theme with only `alacritty.toml`. The resolver also takes short and ANSI aliases, and fills missing keys by fixed mixes. It has no fallback for the accent.
- **Light or dark** comes from the first of these that is present: the `mode` key; the legacy `theme_type` key; a `light.mode` file beside the palette; the background's brightness, light when its three 8-bit channels sum to more than 382.
- **The format carries no version.** Keys are added over time. Named colours do not always hold the colour they name: Matte Black's `yellow` is a red and its `green` an amber, and Lumon's `red` is a blue.
- **Community themes.** omarchy.org lists 146. Of the 145 reachable, 108 ship `colors.toml` (every one with an `accent`) and 37 ship only `alacritty.toml`. About half carry no licence.

### What Luxforge reads

Luxforge reads three files, as data, and runs nothing a theme ships:

- `colors.toml`, in any of Omarchy's forms;
- `alacritty.toml`, when there is no `colors.toml`;
- the `light.mode` marker.

It resolves them as Omarchy's resolver does at the pinned commit (aliases, ANSI fallbacks, the mode's precedence, and the fallback mixes with Omarchy's rounding), so a theme reads the same in Luxforge as in Omarchy. The port is `luxforge_core::theme::omarchy`, with tests that hold it to the values Omarchy's resolver gives for its 22 built-in themes and for synthetic themes in each form. Where it differs from Omarchy, it is stricter or says so:

- It parses strict TOML, where Omarchy reads `key = value` lines.
- Every colour a `colors.toml` gives a key the resolver reads must be `#rrggbb`. Hyprland's gradient strings, and keys the reader does not know, are listed in the report and not used. An empty value is no value, as in Omarchy, and is listed.
- In `alacritty.toml`, a colour Omarchy reads that is not hex, such as `CellForeground`, is skipped as Omarchy skips it, so its fallback applies, and listed. The other keys under `colors` are listed; the rest of the file is the terminal's configuration and is not.
- A fallback mix of a colour the theme does not have leaves that key out. Omarchy's awk would mix an empty string, into a colour that depends on the awk.

Wallpapers, the icon theme and per-application files are never read.

Six resolved values become roles. Luxforge derives everything else by its own rules, because Omarchy's named colours cannot be relied on for meaning.

| Omarchy | Luxforge role | Note |
| --- | --- | --- |
| `mode`, by Omarchy's precedence | `mode` | |
| `background` | `background` | The panels |
| `dark_background` | `surround` | Held to the chroma bound. When a theme leaves it out, Omarchy's resolver makes it the background mixed 25% with black |
| `lighter_background` | `control` | Omarchy's raised surface: lighter in a dark theme, darker in a light one. Derived instead when it equals the background, as it does for an Omarchy 3 palette |
| `foreground` | `text` | |
| `accent` | `accent` | The terminal's normal blue for an `alacritty.toml` theme, as in Omarchy. A `colors.toml` without an accent is refused by name, since Omarchy's resolver gives it none; so is an `alacritty.toml` missing any of its eight normal colours, for which Omarchy writes no palette |

The theme's name is its folder's slug title-cased, as Omarchy lists it, with a cloned repository's `omarchy-` prefix and `-theme` suffix removed. The client sends the slug as `folder`, since the core never sees the folder itself. The person can give another name on import; with neither a name nor a folder that gives one, the import is refused. The mapping is `omarchy_roles` in `luxforge_core::theme`, and the theme resolves by the import rules. The report's `omarchy` section records:

- which form the palette was in (`omarchy4`, `omarchy3` or `alacritty`), and how the mode was decided (`mode-key`, `theme-type-key`, `light-mode-file` or `background-brightness`);
- each role, the key it came from and the value Omarchy's resolver gives that key, before any move;
- each role left to derive, with its reason: the control, `equals-background`;
- every key and value the reader does not use, with its reason: `unknown-key`, `gradient`, `replaced`, `empty` or `not-a-colour`. The palette's other colours are never read, so they are not listed.

The rest of the report is the model's: each ink moved, with its floor and both colours; each derived tier that stopped short; the surround before and after; and the accent note. A refusal names the file and key, with the reader's coded error (such as `{code: "missing-key", file: "colors.toml", key: "accent"}`) as its data.

### What the 22 built-in themes become

These figures are the implementation's, run on Omarchy's 22 built-in palettes at `035ce29f03bdd97a09af80ef5f2d22d7a98930d6`. The test `the_22_built_in_omarchy_themes_become_the_recorded_figures` pins them, and every one of the 22 meets every floor and the chroma bound after its reported moves.

| Effect | Themes |
| --- | --- |
| Surround neutralised: `dark_background` above 0.010 chroma | 12 of 22: Catppuccin, Ethereal, Everforest, Flexoki Light, Hackerman, Kanagawa, Lumon, Nord, Osaka Jade, Retro 82, Rose Pine and Tokyo Night. Most of all Retro 82's navy `#031222` (0.040), which becomes `#0e1215`; least Rose Pine's `#ede7e1` (0.0104) |
| Own text moved to its floor | Tokyo Night (0.55 ΔE00), Everforest (2.20), Gruvbox (2.26), Catppuccin Latte (3.41), Rose Pine (3.83) |
| Own accent moved to its floor | White (1.93 ΔE00), Rose Pine (2.11) |
| A derived text tier stops short of Luxforge Dark's weight | 16 of 22: the tertiary tier in all 16, and the secondary in 8 of them (Catppuccin Latte, Everforest, Gruvbox, Miasma, Nord, Osaka Jade, Rose Pine and Tokyo Night), because their text-to-background contrast is lower than Luxforge Dark's. Ethereal, Hackerman, Last Horizon, Lumon, Retro 82 and Vantablack keep both weights |
| Error ink moved in lightness | Catppuccin Latte (1.92 ΔE00), Everforest (0.79), Flexoki Light (0.96), Nord (2.79), White (10.91) |
| Accent under 15 ΔE00 from a reserved colour | 9 of 22: Tokyo Night 8.74, Nord 9.72, Ethereal 11.03, Catppuccin Latte 12.55, Catppuccin 12.85 and Lupine 13.44 from the shadow-clipping blue; Hackerman 10.59 from the mask green; Ristretto 13.90 from the clipping red; Kanagawa 14.87 from the mask white |
| Control derived, because `lighter_background` equals the background | Last Horizon, Solitude |

They differ from the earlier prototype's figures (Python, not the implementation) in three places. Rose Pine's surround, at 0.0104, is neutralised, where the prototype counted 11 themes. A derived tier stops short in 16 themes rather than 18, and the secondary tier in 8 rather than 9. And Catppuccin Latte's accent is 12.55 ΔE00 from the blue rather than 12.6.

### Bundled themes

The owner decided on 2026-10-05 to bundle a few popular Omarchy themes, beside imports from a folder. The recorded default is six:

| Theme | Mode | Why |
| --- | --- | --- |
| Tokyo Night | Dark | Omarchy's default theme |
| Catppuccin | Dark | The most-starred scheme on GitHub of those behind Omarchy's built-in themes (19.8k stars on 2026-10-05) |
| Catppuccin Latte | Light | The light theme of that same scheme |
| Gruvbox | Dark | Gruvbox has 15.8k stars; Omarchy's palette is the Gruvbox Material variant |
| Nord | Dark | 6.9k stars |
| Everforest | Dark | 4.2k stars, and a teal accent apart from its text |

Kanagawa (6.4k stars) is left out because its accent is its text colour, so its state marks would not stand out against its labels. The six together are about 3.6 KB.

- **The files** are Omarchy's own `colors.toml` files, vendored verbatim from the pinned commit beside the core's theme module, never edited. Omarchy's MIT licence and a notice naming each palette's upstream scheme sit beside them, and a test pins each file's SHA-256 to that commit. Changing the set, or the commit, is a deliberate change of its own.
- **The same reader.** A bundled theme is read by the same reader and mapping as an import, once, when the library is first used, so the bundled files are also the reader's real-file tests. Its report is part of what `theme.read` answers, and its `source` is the vendored `colors.toml`.
- **Built in.** Bundled themes are listed after Luxforge Dark, in the order above, with ids of the form `omarchy.tokyo-night` and an origin of `{kind: "omarchy", folder, form: "omarchy4", commit}` naming the pinned commit. They are never stored and cannot be deleted, and their names are taken: an import under the same name, ignoring case, is a conflict, so importing the same folder again is listed as one. `preferences.set {theme: "omarchy.nord"}` chooses one like any other theme.
- **What they become:**
  - Tokyo Night, Catppuccin, Nord and Everforest have their surround neutralised; Gruvbox and Catppuccin Latte keep theirs.
  - Tokyo Night, Everforest, Gruvbox and Catppuccin Latte have their text moved to its floor, by 0.55 to 3.41 ΔE00. No bundled theme's accent is moved.
  - Catppuccin Latte, Everforest and Nord have the error ink moved in lightness.
  - All six have the tertiary tier stop short, and all but Catppuccin the secondary too.
  - Tokyo Night, Catppuccin, Nord and Catppuccin Latte keep accents under 15 ΔE00 from a reserved colour, as decided.
  - Each takes its control from `lighter_background`.

## Library and API

### Storage

Themes are the person's, outside any catalog, like flags and preferences. They live in one document, `themes.json`, in the application configuration directory beside `preferences.json`, written through the same bounded, locked, format-marked atomic writer. It holds at most 128 themes, each at most 8 KiB including the text of the files it was imported from, in at most 1 MiB. An evidence run keeps the document inside its evidence directory, so no automated launch reads or writes the person's themes. Luxforge Dark and the bundled themes are built in and never stored.

The active theme is a preference: `theme` in `preferences.json`, absent for Luxforge Dark. `Preferences` refuses unknown fields, so an older build refuses a file that names a theme and keeps it unchanged. Every shape change is handled that way under the current-shapes rule.

A record is `{id, name, mode, roles, tokens, origin, report, source, actor, created_ms, updated_ms}`:

- `tokens` holds only the tokens the theme sets explicitly; the rest are derived when the record is read.
- `origin` is `{kind: "built-in"}` for Luxforge Dark, `{kind: "luxforge"}`, or `{kind: "omarchy", folder, form}`, which for a bundled theme also names the commit its file came from.
- `source` keeps the text of each file the import read verbatim, as presets keep theirs, so a later reader can map them again without the folder. An `alacritty.toml` beside a `colors.toml` is not read, so not kept. Only `theme.read` returns it.
- `report` keeps the import's report. `theme.read` answers the report this build's rules give, with the stored `omarchy` section, which resolving the roles again cannot give.

Names are trimmed and non-empty, have no control characters and at most 64 characters, and are unique ignoring case across Unicode. A duplicate is a `conflict`, and nothing is renamed or replaced automatically. The built-in themes' names are taken.

### Methods

Every method is a host method listed by `schema.list`. The mutating methods take the `{request_id, actor}` mutation envelope and are deduplicated, as the preset library's are. Each emits an event named after the method unless it is a no-op or a retry.

| Method | Mutates | Parameters | Returns |
| --- | --- | --- | --- |
| `theme.list` | no | none | `{themes, active, unrecognized}`. Each theme gives its id, name, mode, origin, whether it is built in, five swatches (surround, background, surface, text, accent) and its report's counts. `unrecognized` lists the stored records this build cannot read, each with its reason |
| `theme.read` | no | `theme_id` | `{theme}`: the record with every resolved token, the full report and the source |
| `theme.inspect` | no | `format`, and `files` or `content`; optional `folder` and `name` | `{theme, report}` as an import would create them; nothing is stored |
| `theme.import` | yes | `format`, `files` or `content`, `mutation`; optional `folder` and `name` | `{theme, report, deduplicated}` |
| `theme.export` | no | `theme_id` | `{file_name, content}`, a Luxforge theme document |
| `theme.delete` | yes | `theme_id`, `mutation` | `{outcome, deleted, deduplicated}`. A built-in theme and the active theme are refused by name |

`preferences.read` also answers `theme`. `preferences.set {theme}` takes a theme id, or `null` for Luxforge Dark, and refuses an id the library does not hold. It announces its event whenever the theme changes, as it does for the fields a General row shows. The canvas background gains its proposed `theme` choice the same way, checked and announced as its other choices are.

`format` is `omarchy` or `luxforge`. The client reads the files and sends their text, as it does for `preset.import`, because the core reads no user-chosen file ([module capabilities](../decisions.md#module-capabilities)). For Omarchy, `files` maps each file name the reader takes to its text, each at most 64 KiB. A name the reader does not take is refused by name rather than ignored. `folder` is the theme folder's own name, not a path, at most 128 bytes; it names the theme unless `name` does and is kept in the origin, and `format: luxforge` refuses it. A Luxforge theme document carries its format marker; a document of another shape or marker is refused without being read further.

### Events and other clients

A client of the desktop's live session that imports, deletes or chooses a theme changes the desktop's window at once. The event sync runs whether or not a photograph is open. It reads the preferences again on a `preferences.set` event and draws the theme they name, and it reads the library again on a `theme.*` event, whether or not the Settings sheet is open.

A separate `luxforge-json` process runs its own catalog owner over the same configuration directory and logs its events in its own log. What it stores therefore reaches the desktop only when the desktop next launches or opens Settings.

## Desktop

### A runtime theme

The widget crate has its own Iced theme type, `luxforge_ui::Theme`. It holds the resolved palette behind an `Arc` with a generation number no other theme in the process has, and implements Iced 0.14's `theme::Base` and the style catalogs of the widgets the desktop uses: text, container (which the tooltip uses), button, scrollable, text input, slider, checkbox, pick list with its menu, and progress bar. Every view and widget is a `luxforge_ui::Element` of that theme.

- Iced hands the theme to every style function and every canvas `draw`, and each reads its colours from it. No global holds a palette. A widget model that names a colour before any theme is known holds an `Ink`, a token or a fixed colour, resolved when it draws.
- The application builder takes the theme from the editor's state, which Iced reads again after every update, so a change redraws the window once.
- Iced's own defaults for what it styles itself (the scrollbars, the checkbox, the text input's selection, the pick list's menu, the window's clear colour) come from an `iced::Theme` built from the roles and held inside the Luxforge theme.
- Every canvas cache keys on the theme's generation: the icons, histogram, curve editor, slider, range slider, notched slider, disclosure heading, colour swatch, coverage thumbnail, colour picker and sparkline. Each rebuilds once when the theme changes.
- The desktop draws the widget crate's own Luxforge Dark palette, built from the visual language's values with their exact alpha, whenever Luxforge Dark is active. Any other theme's palette is the core's resolved tokens, copied by name.
- The canvas fill keeps its view-model value (`CanvasFill` in `state/canvas.rs`), whose Theme choice the view maps to the theme's surround; Dark, Black and Grey keep their constants.
- `state/` holds only the active theme's identity and the Appearance tab's rows as plain data, as its layer rule requires. The theme value itself lives in `app/`.

### Appearance tab

The Settings sheet gains a third tab, **Appearance**, between General and Experiments. It lists Luxforge Dark first, then the bundled themes, then the imported themes by name. Each row shows:

- five swatches: surround, background, surface, text and accent;
- the name;
- Dark or Light;
- its origin: Built-in (the bundled Omarchy themes too, since they ship with Luxforge), `Omarchy · <folder>` or Imported file;
- an **Adjusted** badge when the import moved one of the theme's own inks;
- a check, on the active row.

Clicking a row applies it at once through `preferences.set {theme}`, and the sheet and the workspace behind it redraw in the new theme. **Import Omarchy theme…** takes a theme folder or a folder of theme folders ([importing a folder](#importing-a-folder)). **Import theme file…** takes a Luxforge document. A row's menu, from its More button or a right-click, holds Export…, Copy import report and Delete. Built-in rows have no Delete, and Delete on the active row is refused with its reason, in the tab and the status bar. Stored themes this build cannot read are listed at the end with their reasons, and kept.

The palette gains **Settings · Appearance** and one **Theme: \<name\>** entry per theme. Which tab is open stays this desktop's own view state, as it is for the other tabs.

### Importing a folder

A folder that holds a file the reader takes is one theme. A folder whose subfolders hold them is a set, each subfolder one theme. Examples of sets are a clone of Omarchy's repository, whose `themes` directory brings all 22 built-in themes at once, and Omarchy's own theme directories on Linux. There the picker opens at `~/.config/omarchy/themes`, `/usr/share/omarchy/themes` or `~/.local/share/omarchy/themes`, whichever exists first.

The desktop reads only the files the reader takes: each at most 64 KiB, from at most 256 subfolders. It reads them on a task rather than on the update loop ([performance rule 12](../engineering/performance-rules.md#rules)). Each theme is one `theme.import` with its own report. A file that cannot be read fails its own theme only, with the reason. A name the library already holds is listed as a conflict, never renamed or replaced; importing Omarchy's whole `themes` directory therefore imports 16 themes and lists the six bundled themes as already built in. The tab then shows a summary line, the themes imported, those already built in or already imported, and each failure with its folder and reason, until the next import or delete; the status bar shows the summary, and its copy button copies every answer and report.

### Launch, changes and fallback

- **Launch.** The desktop reads the active theme before its first frame, with the preferences it already reads synchronously there (the remembered workspace, the canvas background and the interface size among them). It therefore never draws Luxforge Dark first and then changes. The extra cost is one read of `themes.json`, which `measure`'s launch-to-first-frame records.
- **A change** by this desktop or by a client of its session redraws the window in the next update. It reads no asset, history page, preview or upload, and each canvas cache rebuilds once.
- **A missing or unreadable active theme** leaves Luxforge Dark on screen. The status bar names the theme by its id and gives the reason, and the stored choice stays until the person chooses another.
- **Native appearance.** iced_winit sets the window's appearance from the theme's mode when the window opens and whenever the mode changes (`synchronize` in `window/state.rs`). A light theme therefore turns the macOS window Aqua with no code of Luxforge's. The native file dialogs open without a parent window, so they presumably follow the system's appearance rather than the window's; that is not verified.

## Verification

- **Core.** Unit tests cover:
  - the roles and their derivation, with golden tokens for a dark and a light synthetic theme;
  - the surround and rail-backdrop bound, and each contrast floor and move;
  - Luxforge Dark's resolved tokens, which equal the visual language's table;
  - the Omarchy reader on fixtures in Omarchy's layout: each palette form, each way of deciding the mode, aliases, gradient values, unknown keys, and malformed, missing and oversized files;
  - the store's round trip, and an unreadable record kept and listed;
  - every refusal by name, events, deduplication and `schema.list`.
- **Resolver parity.** The reader resolves each of the 22 built-in palettes, and synthetic Omarchy 3, short-name and `alacritty.toml` themes, to the values Omarchy's resolver gives at the pinned commit.
- **Bundled themes.** Each bundled file's SHA-256 equals the pinned commit's. Each bundled theme resolves to Omarchy's values and meets every floor after its reported moves. None of them can be deleted, and their names are taken.
- **Widgets.** Luxforge Dark's values keep their tests. A test theme of distinct colours draws each region in its own token, and every canvas cache rebuilds when the generation changes.
- **Desktop.** Tests cover:
  - the Appearance model and rows, and the palette entries;
  - request parity: each Appearance action builds the request an independent JSON client sends;
  - adoption of another client's change with no photograph open;
  - the fallback, and the launch read;
  - a theme change, counted as sending no `asset.state`, `history.list` or preview job.
- **Pixel identity.** The release build before the runtime theme and the build after it captured the gallery's 104 states and the `workspace` scenario's frames under Luxforge Dark. The gallery's 14 board pages decode to identical pixels. Every other frame is identical outside the Performance section's live figures (memory, CPU, GPU, sparkline ends and job times), which differ in the same frames and regions between two runs of the earlier build.
- **The `theme` smoke scenario**, in the `rendered` tier, at 1440 × 900 on `orientation-1.jpg`:
  - It imports a set of synthetic Omarchy themes (`fixtures/themes/omarchy/`) through the Appearance tab's own messages: a dark one and a light one imported, one named like a bundled theme listed as built in, and one without an accent failed with the reader's reason. Importing the dark one again is listed as already imported.
  - It switches to Nord, the dark import and the light import, sets the canvas background to Grey and back, and exports. A second client then chooses Luxforge Dark with `preferences.set`, and it exports again.
  - Each frame records the drawn theme, its surround, background, surface, control and text tokens, and the reports. The runner checks that each frame draws the theme expected with no problem pending, and that the tokens equal the core's resolution or the import's answer.
  - In the pixels, exactly: the title bar's dominant colour is the surface, the status bar's and the state panel's the background, a control holds the control token, and the canvas beside the photograph the surround, or `#777777` at Grey under the light theme. The surround carries at most 0.010 OKLCh chroma.
  - The photograph, found by the surround each frame records, has the same rectangle and pixels in every frame where the sheet is closed, and the two exports have the same SHA-256.
- **Review captures.** The workspace, the Appearance tab and the gallery's 14 pages were captured in Luxforge Dark, each bundled theme and each of Omarchy's other 16 built-in themes, imported from Omarchy at the pinned commit. They are evidence for the owner's review, not committed boards. No region was left in Luxforge Dark's colours, every surround is neutral, and no text was found illegible. Two things to review: White's accent is a grey, so its state marks barely stand out; and the fixed histogram channel fills read pastel over a light surround.
- **Timing** against the build before the runtime theme ([performance](../specs/performance.md#ui-themes-measured-on-the-m4)): the 24 MP drag's input to presented frame and the workspace derivation per message are unchanged within the host's spread. Launch to first frame is about 23 ms later at p50, all of it before the process starts, in copying a 1.4 MB larger executable into the background bundle; the application's own startup is unchanged.

## Performance rules checklist

- **Original reads and decodes:** none.
- **Full-frame allocations:** none. A theme is a few kilobytes, and the library is bounded at 1 MiB.
- **Point queries:** none.
- **Owner thread:** each `theme.*` method reads or rewrites one bounded document. An import parses at most 64 KiB per file and derives about seventy colours. No frame work.
- **Desktop messages:** a theme change sends no `asset.state`, `history.list`, preview job or upload. It redraws once, and each canvas cache rebuilds once. The event sync reads the preferences and the library only on their own events.
- **Timers, polls and subscriptions:** none added. The event sync's existing wake also runs with no photograph open.
- **Repeated work and caches:** no new cache. Each existing canvas cache's key gains the theme's generation.
- **Timing:** the style functions sit on the per-message view path, so the drag latency and the workspace derivation are measured once, before and after.
- **Exactness:** no effect is added. The theme scenario checks that the photograph's pixels and an export's bytes are the same under every theme.

## Decisions

Decided by the owner on 2026-10-05 and recorded in [decisions](../decisions.md#ui-themes):

| Question | Decided |
| --- | --- |
| Themes, and light ones | Themes replace "dark only", light ones included. Luxforge Dark stays the default |
| Tinted chrome | Allowed in any theme but Luxforge Dark, which keeps the principle that the photograph is the only colour on screen |
| The surround | Near-neutral in every theme (at most 0.010 OKLCh chroma), at the theme's lightness |
| An accent near a reserved colour | Each theme keeps its own accent; the report notes one under 15 ΔE00 from a reserved colour |
| Bundled themes | A few popular Omarchy themes, beside imports from a folder |

### Proposals with recorded defaults

Luxforge runs on these until the owner revises them.

| Question | Recorded default | Alternatives |
| --- | --- | --- |
| Which themes are bundled | Tokyo Night, Catppuccin, Catppuccin Latte, Gruvbox, Nord and Everforest ([bundled themes](#bundled-themes)) | Another set, such as all 22 of Omarchy's built-in themes, or a second light theme (Rose Pine or Flexoki Light) |
| Contrast | 7:1 for primary text; 4.5:1 for labels and secondary text; 3:1 for tertiary text, the accent and the error ink. An imported theme's own ink is moved to its floor and reported (6 of the 22). A Luxforge document that misses a floor is refused | Refuse an import that misses a floor; or accept it and only warn |
| Omarchy forms | Omarchy 4 and Omarchy 3 palettes, `light.mode`, and `alacritty.toml` where there is no palette (37 of the 145 reachable curated community themes) | The Omarchy 4 palette only |
| The canvas background | A fourth choice, Theme, the theme's surround, as the new default; Dark, Black and Grey stay fixed greys | No Theme choice: themes never set the canvas around the photograph, only the histogram plot |
| Where | A Settings › Appearance tab for the theme list. The canvas background and interface size stay General rows, as decided on 2026-10-04 | A Theme row in General, with the library managed elsewhere |
| Following Omarchy's current theme on Linux | Later | In this plan: read `~/.local/state/omarchy/current/theme/colors.toml` (in Omarchy 3, `~/.config/omarchy/current/theme`) at launch and whenever the window gains focus, with no watcher thread or poll |
| Following the system's light or dark appearance | Later | In this plan, with a theme chosen for each |

## Recorded engineering defaults

These are the implementation choices Luxforge is built on; the owner can revise any of them.

| Choice | Default | Why |
| --- | --- | --- |
| Theme value | Luxforge's own Iced theme type, read by every style function and `draw` | Iced already passes it everywhere: no global, and no new parameter on every view function |
| Where the model lives | `luxforge-core`, beside `flags` and `preferences`, using the core's colour equations (the sRGB transfer, Rec. 709 luminance and Oklab in `colour.rs`) | Every client reads themes through the command service, and each equation is written once |
| Colour difference | CIEDE2000, written once in `colour.rs` and tested against `luxforge-reference`'s independent implementation | The accent note and the GPU preview limits use the same measure |
| TOML | `toml_edit` 0.25.15, already in the lockfile through `proc-macro-crate`, pinned | Omarchy's palette and `alacritty.toml` are TOML, and every curated `colors.toml` parses as strict TOML. This adds no package to the lockfile, though the crate enters the shipped binary, so its notice is kept |
| Omarchy resolution | Ported from `bin/omarchy-theme-color` at a pinned commit, with parity tests | A theme reads the same in Luxforge as in Omarchy |
| Bundled files | Omarchy's `colors.toml` files vendored verbatim from the pinned commit beside the core's theme module, with Omarchy's MIT licence and a notice naming each upstream scheme; a test pins their SHA-256s | One reader for bundled and imported themes, and no edited copy of anyone's palette |
| Storage | One `themes.json` beside `preferences.json`: at most 128 themes of 8 KiB, in 1 MiB | The person's, outside any catalog, isolated in evidence runs, with one atomic write per change |
| Deleting the active theme | Refused | Luxforge never leaves a stored choice pointing at nothing by its own hand |
| Event sync | Runs with no photograph open, for preference and theme events | A change from another client reaches the window wherever the person is |

## Later

- Following Omarchy's current theme on Linux, if the owner wants it (see the [recorded defaults](#proposals-with-recorded-defaults)). Omarchy 4 rebuilds `~/.local/state/omarchy/current/theme/` on every switch, by removing and renaming the directory, and writes `theme.name` beside it. Omarchy's own guidance for applications is to read that directory's `colors.toml` and never write Omarchy's configuration.
- Following the system's light or dark appearance, with a theme chosen for each (Iced 0.14 has `system::theme_changes`).
- A Lightroom-style choice of surround grey, independent of the theme.
- Installing a theme from a repository URL, as `omarchy theme install` does. That would be the editor's first download a person starts outside module resources.
- A theme editor.
