# Settings and feature flags

Status: implemented, on the four [decisions](#decisions) below.

Luxforge has a small, typed **feature flag** registry that gates features and experiments, two API methods that read and change it, and a **Settings** surface in the desktop whose **General** tab holds the person's preferences and whose **Experiments** tab lists every flag with its control. Developer mode is the first flag, and AI object picking is the experiment a live flag gates. Verified on the M4 Mac by the `settings` smoke scenario, with the `quick` and `rendered` tiers passing.

## Scope

[Optional usage statistics](usage-statistics.md) add a **Privacy** tab, listed only in a launch eligible to report. It is not a feature flag. Visible debug, unrevisioned and Developer launches are eligible for consented test-tenant reporting and disclose its complete URL. Ordinary revisioned release launches remain ineligible while production activation is off; evidence runs need explicit test eligibility (`--usage-test-receiver`). The switch drives the desktop's statistics supervisor, which an eligible launch starts ([implemented integration](usage-statistics.md#implemented-integration)); turning it on starts a session and turning it off cancels any upload in flight. A **Models** tab, after Appearance, is listed only when a registered module holds a model ([module capabilities](module-capabilities.md#desktop)).

In scope: the flag registry, its storage beside the user preferences, `flags.list` and `flags.set`, launch-time resolution, developer mode as a flag, the Settings surface with its Experiments tab, the palette and keyboard routes to it, evidence steps and a rendered smoke scenario.

Out of scope: remote or percentage rollouts, per-catalog or per-photo flags, flags in `luxforge-json`'s own launch, and the GPU renderer, which is the renderer of record and neither a flag nor a preference ([GPU-first](gpu-first.md)). The person's preferences are designed in [preferences](preferences.md).

## Rules

- **A flag never changes pixels.** A flag gates interface, behaviour and execution paths whose output is byte-identical. An experiment that changes what a recipe renders or exports is a module whose layers the recipe records, so the same recipe always renders the same image whatever flags are on. Review enforces this; the registry's flag descriptions say what each flag changes.
- **Flags are the person's, outside any catalog.** Values are stored in the host's `preferences.json` under the application configuration root, through the same bounded, locked, format-marked atomic writer as the Performance preference. An evidence run keeps them inside its evidence directory, so no automated launch reads or writes the person's flags.
- **Only explicit choices are stored.** A flag nobody set has no stored value and follows its default, so a changed default reaches everyone who never chose. Resetting a flag removes its stored value.
- **Nothing is discarded silently.** A stored value for a flag this build does not know is kept, untouched, and listed as unrecognized. A stored value that does not fit its flag's current kind is reported on that flag with the reason, the flag follows its default, and the stored value is replaced only by an explicit `flags.set`. A malformed or unsupported preferences file is refused without rewriting it.
- **Every flag is programmable.** The Settings surface reads `flags.list` and writes `flags.set`, the same methods any client uses.

## Registry

`luxforge_core::flags`. A flag is a `FlagSpec`: a stable `id` (lowercase, dotted), a short `title`, a one-sentence `description` of what it changes, a `kind` and when it `applies`.

| Kind | Value | Declares |
| --- | --- | --- |
| `toggle` | `true` or `false` | the default |
| `choice` | one option's value | the options, each `{value, label}`, and the default |
| `number` | a finite number | `min`, `max`, `step` and the default |

`applies` is `live`, read by its consumer each time it is needed, or `launch`, read once as a binary starts. A launch flag's change takes effect the next time the desktop launches.

The flags:

| Flag | Kind | Applies | Default | Listed | What it changes |
| --- | --- | --- | --- | --- | --- |
| `developer` | toggle | launch | on in debug builds, off in optimized builds | always | The desktop serves the test modules (the pixel and controls proofs) and shows the Developer button and its components gallery |
| `ai.analysis` | toggle | live | off | always | AI object picking: whether the analysis tasks, `query.pick-object` and `task.select-object` are offered, whether their models load and are listed in Settings › Models, and whether the Corrections band offers Object and the mask menus offer Object… ([gated object picking](#gated-object-picking)); never how a stored selection renders |
| `proof.choice` | choice: `first`, `second`, `third` | live | `first` | developer runs only | Nothing; a fixture for the choice control |
| `proof.number` | number: 0 to 100, step 5 | live | 50 | developer runs only | Nothing; a fixture for the number control |

### Gated object picking

`ai.analysis` gates analysing and picking, not having a selection. The analysis tasks, `query.pick-object` and `task.select-object` declare it, and the segmenter's resources name it, so the desktop hides them from the descriptor alone. Off, the Corrections band lists Heal, Clone and Remove only (a band on Object returns to Remove), the New mask and Add component menus have no Select group (an open Masks › Object sub-state is left as Escape leaves it), Remove never picks, and Settings › Models lists none of the analysis models; the owner refuses the tasks, the query and `module.runtime.load` of the analysis models as `not-ready` naming the flag, and still lists them and installs or removes them for any client. A stored `model` selection renders whatever the flag ([AI editing](ai-editing.md#analysis-and-the-per-photo-cache)).

The two proof flags follow the proof modules' rule: they exist only when the running host serves the test modules, and change nothing, so the choice and number controls have a real flag to render and test. A stored proof value is kept, and listed as unrecognized, by a host that does not list them.

## API

`flags.list {}` returns `{flags, unrecognized}`. Each flag is `{id, title, description, kind, applies, default, value, stored}` with `options` for a choice and `min`, `max` and `step` for a number. `value` is what a live flag reads now and a launch flag's next launch reads; `stored` says whether the person chose it. A launch flag on a host the desktop started also carries `active`, the value this launch resolved, and `override` when the command line forced it (`"--developer"`); a live flag carries both only when its host forced it, as a test host forces an experiment on for its life whatever is stored. A flag whose stored value does not fit carries `error` and follows its default. `unrecognized` lists the identities of stored values no listed flag claims. The read creates no file and changes nothing.

`flags.set {flag, value}` stores `value` for `flag`, checked against the flag's kind; `null` or an absent `value` removes the stored value, so the flag follows its default. Setting the current stored value again is accepted and writes nothing. The answer is the same as `flags.list`, so a client adopts one answer. An unknown flag, a value of the wrong kind, an option the choice does not offer or a number outside its range or off its step is refused by name. A change is announced in the event log under `flags.set`, so another client reads the flags again. It changes no recipe, history or render, and needs a configured application preference directory.

## Launch

The desktop reads its launch flags once, before it assembles the module registry: the stored value, or the default, then the command line. `--developer` forces developer mode on for that launch whatever is stored; there is no switch to force it off. A preferences file that cannot be read leaves every flag at its default for the launch, and the status bar says why, as the Performance preference's failure does. The desktop also reads `flags.list` once as it starts, off the update loop, for what a live flag gates outside the sheet.

`luxforge-json` keeps its own `--developer` and reads no flag at launch: what `schema.list` lists through it never depends on the person's settings or on how the binary was built. Its `flags.list` lists the same flags without `active`.

## Settings surface

A modal sheet over the dimmed workspace, 720 by 480 points, centred: a header with **Settings** and a close button, a 160 pt tab rail on the leading side and the selected tab's content. The tabs are **General**, with a gear icon, **Appearance**, with a drop icon, **Experiments**, with a beaker icon, and, in a launch eligible for usage statistics, **Privacy**, with a lock icon.

- **Opening.** A gear button at the title bar's trailing edge, after the panel toggles, and Cmd+, on macOS and Ctrl+, elsewhere, open it at General; the palette's **Settings · General**, **Settings · Appearance** and **Settings · Experiments** entries open it at their tab, with **Settings · Privacy** where the tab is listed, and a search for "settings" or the tab's name finds each. Every build shows it. Opening it closes the palette and any title bar menu.
- **General.** The person's preferences, which are not experiments and so have no Reset, one row each with its title, description and control: Auto collapse history ([auto-collapse](versions-and-lineage.md#auto-collapse)), Correct lens distortion on new RAW photos, Mask overlay colour, Canvas background, Interface size and Catalog ([preferences](preferences.md)). A change shows at once and is written through the desktop's one preference writer ([desktop writes](preferences.md#desktop-writes)); a refused write says why in the status bar and reads back what is stored. The sheet shrinks to fit a window too small for it at the chosen interface size.
- **Closing.** Escape, Cmd+, again, the Done button or a click on the backdrop. Nothing in the sheet needs confirming: each change is written as it is made.
- **Appearance.** The theme list, its imports, exports and deletes, through `theme.*` and `preferences.set {theme}` ([UI themes](ui-themes.md#appearance-tab)).
- **Experiments.** A one-line explanation ("Experiments gate unfinished features. They never change how a photo renders or exports."), then one row per flag: the title and description, and on the trailing side a switch, a segmented control for a choice, or a number field with its range beside it, which sends on Enter and refuses a value off its range or step in the status bar. A stored flag shows Reset. A change shows at once, before its answer lands. A launch flag says "Applies on next launch", and "Relaunch to apply" when its next value differs from the active one; a forced flag says "On for this launch: --developer". A flag with an error shows it under the row. Unrecognized stored values are listed at the end, with a note that they are kept.
- **Privacy.** The **Share usage and performance statistics** switch beside the short disclosure the startup notice uses, and a collapsed **What is sent** list naming the installation, session and image IDs, the device fields and the events a report can hold ([usage statistics](usage-statistics.md#settings-and-programmatic-parity)). The switch shows the effective state: turning it off shows at once, turning it on only once `usage.set` has saved it. Writes go one at a time through `usage.set`, never `preferences.set`. A failed write is said under the switch; a failed opt-out says statistics are off for this session but the stored choice did not change. An unreadable record or a changed disclosure is explained under the switch. There is no send button, delivery status or connectivity warning. Opening the sheet reads `usage.read`, and another client's `usage.*` change reaches an open tab through the event sync.
- **What it reads.** Opening the sheet reads `preferences.read` and `flags.list` in one task; each change sends `flags.set` or `preferences.set` and adopts its answer. Flag writes go one at a time in the order made; preference writes merge while one is in flight ([desktop writes](preferences.md#desktop-writes)); closing the window waits for the last of either. Another client's `flags.set` or `preferences.set` reaches an open sheet through the event sync, which reads both again, and a closed one reads the flags alone for a `flags.set`, so Remove's picker follows a live flag; `preferences.set` announces an event only when it changes something a General row shows, or the theme. The event sync runs whether or not a photograph is open, and reads the theme library again on a `theme.*` event.
- **Drafts.** The sheet changes no recipe, so opening it neither needs nor displaces a draft; while it is open the workspace takes no shortcut but Escape and Cmd+,.

Which tab is open, and whether the sheet is, is this desktop's own view state, like the gallery page; `workspace.set` and `session.state` do not carry it.

## Evidence and verification

- Core unit tests: storage round trip, reset, refusal of every bad value by name, unrecognized and mismatched stored values kept untouched, a malformed file refused without rewriting, launch resolution with and without `--developer`, proof flags listed only to a developer registry.
- `schema.list` lists both methods; an owner test checks that `flags.set` announces its event and changes no asset revision.
- Desktop unit tests: the Experiments model for every kind, the launch notices, the write queue and adoption, the palette and keyboard routes, Escape, and close waiting for a write; the General switch's model, its coalesced writes, the owner applying it to the next edit and its value kept across launches.
- Evidence steps `{"settings": {"open": true|false}}` (with `"tab"` one of `general`, `appearance`, `experiments`, `privacy`), `{"flag": {"id": …, "value": …}}` and `{"usage": …}` (`"notice"`, `{"answer": "share"|"decline"}`, `{"share": bool}`, `{"details": bool}`), which drive the sheet's and the notice's own messages; the frame's state records the sheet, the flags it shows and, under `usage`, the notice and the Privacy tab as drawn with the stored and effective consent, the destination and `usage.read`'s delivery counts, never an identifier. The `usage` step's `advance` and `await` forms, for delivery, are in [evidence scripts](../engineering/development.md#evidence-scripts).
- Desktop usage tests: the notice claimed once after the workspace is usable and held back by a native dialog, a module consent notice or the sheet; Escape and No thanks writing nothing; Share and the switch through `usage.set` against a real owner with test eligibility, once over retained preferences; another client's opt-out followed; a failed opt-out write; and neither surface in an ineligible launch.
- The `settings` smoke scenario opens the sheet at General from the palette in a developer launch and checks Auto collapse history is drawn on, then changes the proof choice and number through their controls, has a number off its step refused, resets the choice, turns `developer` off for the next launch while `--developer` holds this one, closes the sheet with Escape and opens it again over what the host stored, checking the drawn sheet's pixels against the workspace behind it. The launch has usage statistics test eligibility with a loopback receiver: once the opens are captured the notice is claimed, drawn and answered Share statistics, and the Privacy tab is opened, its list expanded and its switch turned off and on, each checked against the stored consent. The `usage` smoke scenario carries the notice and the switch across three launches and through delivery ([the background journey](usage-statistics.md#the-background-journey)).

## Performance rules checklist

- **Original reads and decodes:** none added.
- **Full-frame allocations:** none. A listing is a few hundred bytes; `preferences.json` is bounded at 16 KiB.
- **Point queries:** none added.
- **Owner thread:** `flags.list` and `preferences.read` read one small file; `flags.set` and `preferences.set` are each one locked read-modify-write of it, `flags.set` written only when the value changed. An analysis task, `query.pick-object`, `task.select-object` or a load of an analysis model reads it once to check `ai.analysis`; no other request does. No frame work.
- **Desktop messages:** opening the sheet reads `flags.list` once; each change sends one `flags.set` and adopts its answer. No `asset.state`, `history.list`, preview job or upload is added. A closed sheet derives nothing. The launch reads `flags.list` once, and a `flags.set` event reads it again, with the preferences too while the sheet is open; the desktop's own writes are skipped by the event sync.
- **Timers, polls and subscriptions:** none added.
- **Repeated work and caches:** none added; the launch reads its flags once, before the registry is assembled.
- **Timing:** no interactive path is involved, so no `editor-performance` run is needed.
- **Exactness:** flags change no pixels by rule; no effect is added.

## Decisions

- A **Settings surface with an Experiments tab**.
- **Always visible**, in every build.
- **Developer mode is the first flag**, with `--developer` kept as a launch override.
- **A flag never changes how a photo renders or exports.**

Recorded defaults, proposals the owner can revise: the gear button and Cmd+, as the routes in; launch flags apply on the next launch rather than restarting anything; only explicit choices are stored; `luxforge-json` reads no flag at launch; the two developer-only proof flags.
