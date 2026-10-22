# The Select introduction

Status: implemented on the owner's [decisions](#decisions). The `select-introduction` smoke scenario is recorded under Xvfb on Linux only; its native M4 capture remains open.

The first time a person sees Select, a sheet explains how Select and the catalog work and why they work that way. Luxforge does not import: you browse photographs where they are, pick the few worth developing, and only those join the catalog ([the catalog](catalog.md#what-it-is)). Someone coming from Lightroom expects the opposite, and without an explanation an empty catalog and an Events list read as missing features rather than a different model. The sheet says it once, briefly, and stays one click away afterwards.

## Board

[select-introduction/board.html](select-introduction/board.html), the design's mockup, holds three boards at 1440 × 900 pt in Luxforge Dark: the sheet over an empty first-run Select, the title bar's **?** button with its tooltip, and the footer once something is indexed. Open it in a browser.

## What it says

One sheet, read top to bottom in under a minute. No pages, no coach marks pointing at controls.

- **Eyebrow:** HOW LUXFORGE WORKS
- **Title:** Select, then develop
- **Lead:** Luxforge doesn't import. You browse your photographs **where they already are**, pick the few worth developing, and **only those join your catalog**.
- **Three steps**, side by side, each with a small drawing, a sentence or two, and where to find it:

| Step | Drawing | Text | Where |
| --- | --- | --- | --- |
| 1 **Browse** | A card and a folder flowing into three day rows | Open a camera card or any folder. Luxforge groups it by day and camera, and keeps each burst, bracket and panorama together as one moment. | Cards and On disk, on the left |
| 2 **Pick** | A moment of four frames, one picked with the accent check | Step through each moment, check focus at 100%, and pick the frames worth developing. Picks are kept as you go, even across a restart. | `Space` look · `Z` 100% · `P` pick · `↓` next moment |
| 3 **Develop** | A catalog folder `2026 › Konstanz · Sep 2026 › 12 Sep` with **Develop 18** | Develop brings your picks into the catalog, filed by year, place and day, and opens them for editing. | Develop N, top right · `⌘↵` |

- **Coming from Lightroom?** Two short columns. *Import, then cull*: every frame you shoot goes into the catalog; flags and stars are how you find the keepers; the catalog, previews and backups grow with every shutter press. *Select, then develop*, in the accent: choosing is its own fast step, one moment at a time; the catalog holds only what you develop, so 20 picks from a 1,000-frame trip add 20; the rest stay on disk, organized again whenever you come back.
- **Footer:** "Your files are never moved, copied or changed." on the left; **Add a folder…** and **Start browsing** on the right.

The words follow the catalog design's [vocabulary](catalog.md#the-words) and claim nothing it does not: no "import", "shoot" or "library", and catalog folders are never called folders on disk. The keys are drawn with the platform's chord labels (`Ctrl+Return` off macOS). The sheet's words, keys included, are constants beside its state, and a keymap test presses each key it names and checks it does what the sheet says, so a rebound key fails a test rather than leaving the sheet saying the old one.

## Layout and look

- A sheet at most 760 pt wide, centred over Select on `sheet_surface`, with the theme's scrim over the whole window, title bar included, as Settings and the merge sheets have. Select stays visible behind it, so the regions the steps name are recognisable. Its height follows its content, and it scrolls inside the window when the window is shorter.
- It is a guide sheet, the layout the range masks' help sheet and the AI editing setup sheet share: one head, step strip, card and footer, drawn once in `luxforge-ui` from one set of theme measurements.
- Head: 32 pt side padding, the eyebrow in the accent at the 10.5 pt capitalised label size, the title at 22 pt semibold, the lead at 13 pt in secondary text, at most 620 pt wide, with its two emphasised phrases semibold in primary text, as the board's `.lead strong` draws them. The ✕ sits 14 pt from the corner.
- Steps: three equal cards on the panel colour, each with a 112 pt drawing area on the canvas colour above its text, a `›` between them. The cards end level whatever their text, and where a step is done and its keys sit at the foot of its card. The step number sits in an accent-tinted circle. Key hints use the keycap style the palette draws, every cap and word 4 pt apart, wrapping in a narrow card.
- Comparison: two equal cards; ours carries the accent border at about 40% and a 5% accent wash, and the accent check for its bullets, Lightroom's a tertiary dash.
- Footer: the panel colour over a rule, the promise with a small shield glyph, the buttons right-aligned, **Start browsing** the accent primary with its `↵`.
- Drawings are composed from theme tokens with the ordinary widgets, never shipped as images, so every theme, light ones included, keeps their hierarchy. The burst's frames are four small fixed sky-water-grass gradients, like the clipping colours fixed across themes.
- It scales with the interface size. When the window is narrower than the sheet plus 32 pt each side, 824 layout points (a 1440 pt window at 150% is 960), the steps stack in one column.

## Behaviour

- **When it appears.** The first time Select is shown in an interactive or developer launch while the person's preferences do not record it as seen. That is the first launch with a new catalog, which opens in Select, and equally the first `G` of someone who has been using Develop. It appears once the tool modules are ready and no other sheet (a merge, Forget, export, Develop's or Copy Settings' confirmation) is open; the sources, counts and events keep loading behind it. With the preference already set it is not shown, and the launch logs `select_introduction_not_shown`.
- **Closing it.** **Start browsing**, Return, Escape or the ✕ close it. A press on the scrim does nothing, so a stray click does not dismiss text being read. **Add a folder…** closes it and opens the native folder dialog, exactly as the title bar's Add a folder… does. It is shown only while there are no indexed folders and no mounted card (`index.folders` and `card.list` both read and empty), when it is the obvious next step. Until both have answered it is not offered, so it never appears and then goes as they arrive.
- **Remembering.** Closing it, by any of those, sets the `select_introduction_seen` preference. The sheet is not shown again unless the person asks for it. Quitting with it open stores nothing, so it appears at the next launch: it was never read. Leaving Select while it is up, as Develop N's photographs landing, `D`'s catalog folder read or a photograph opened from elsewhere can, takes it down with it: it is drawn only over Select and holds no key in Develop. A first showing taken down this way stores nothing, since the person never closed it, and appears again the next time Select is shown in that launch; one the **?** brought back is simply gone. A preferences file that cannot be written does not stop it closing; it simply appears again next launch, as the preferences design's other failures do.
- **While it is open** it is modal: Return and Escape go to it, ahead of every other sheet's and Select's keys, and every other key is held, Settings' `Cmd+,` included. There is no keyboard focus to move between its buttons. Nothing in the workspace is paused, cancelled or changed by it. The log records `select_introduction_shown` (`reopened`), `select_introduction_closed` (`how`, and `stored` for the closing that set the preference) and `select_introduction_left` (`first`: whether leaving Select took down the first showing).
- **Reopening it.** A **?** icon button in Select's title bar, between Redo and Develop N, tooltip "How Select works", shows the same sheet (without Add a folder… once something is indexed). It does not change the preference, works in any launch, automated ones included, and closing it again stores nothing. Develop has no counterpart.
- **Other notices.** The update offer, the move to Applications question and the usage statistics notice wait until the sheet closes, whether it is the first showing or one the **?** brought back, so a first launch shows one thing at a time; the update offer is not drawn while it is open. The usage notice's durable claim waits too, so quitting with the sheet open never spends the one statistics prompt unseen ([consent and first launch](usage-statistics.md#consent-and-first-launch)). The card notice keeps its place over the top of the centre, under the scrim.

## Programmability

Whether the sheet is up is the desktop's own view state, like which workspace is shown: `session.state` does not carry it, and evidence records it with every captured frame.

What it leaves behind is an ordinary preference, so agents and scripts can read and change it through the existing methods ([preferences](preferences.md)):

| Field | Value | Default | Shown | Takes effect |
| --- | --- | --- | --- | --- |
| `select_introduction_seen` | boolean, or `null` | `null`: not seen | remembered | next launch |

`preferences.set {"select_introduction_seen": null}` (or `false`) brings the sheet back at the desktop's next launch, the first time Select is shown: the running desktop decides the first time Select is shown in the launch, from the preferences it read at launch, and does not re-read a preference that announces nothing. `true` marks it read without showing it, for a scripted setup. Like other remembered state it announces nothing in the event log.

## Automated launches

Hidden, background, evidence and test launches never show it, whatever the preference says, so no existing scenario or test changes and nothing waits on an unexpected sheet. An evidence run shows it only when its launch passes `--evidence-select-introduction` (which needs `--evidence-dir`), and keeps the preference inside its evidence directory, as every evidence run does. In an evidence run it appears once the opened frame is captured, so that frame is Select without it. The evidence script's Select step `introduction` presses the **?**.

## Acceptance

- State tests (`luxforge-app`): shown on the first Select of an eligible launch with the preference unset; not shown with it set; not shown in hidden, background, evidence or test launches; every way of closing sets it once; quitting with it open sets nothing; leaving Select while it is up takes it down, sets nothing and holds no key in Develop, and the first showing comes again with Select; **?** shows it without touching the preference; Add a folder… present exactly while nothing is indexed and no card is mounted; the other notices and the usage claim wait while it is open.
- Keymap tests: Return and Escape close it; Select's keys (`P`, the arrows, `Cmd+Z`) do nothing while it is open; Escape closes it before anything else that takes Escape.
- Core tests: `select_introduction_seen` reads its default, sets, resets with `null` and refuses a non-boolean by name.
- The rendered `select-introduction` smoke scenario over a new catalog: Select without the sheet in the opened frame, then the sheet with correlated state, its words, its steps' keys and both buttons checked, drawn over the middle of the window; Return closes it and the stored preference reads `true`; **?** reopens it, captured at 150% interface size and in Catppuccin Latte; Escape closes it storing nothing; the log shows and closes it in that order. That a later Select in the same launch shows no sheet is a state test.

## Performance rules checklist

- **Original reads and decodes.** None. The drawings are widgets and theme tokens, with no image asset or preview.
- **Full-frame allocations.** None.
- **Point queries.** None.
- **Owner thread.** One `preferences.set` when the sheet is first closed: a locked read-modify-write of the bounded `preferences.json`. Whether it was seen comes from the preferences the desktop already reads at launch.
- **Desktop messages.** Showing, closing and reopening the sheet change only view state, and each draws one frame. No `asset.state`, `history.list`, preview job or upload is added. The check that decides the first showing is one comparison of fields the desktop already holds, run after each message until it decides.
- **Timers, polls and subscriptions.** None.
- **Repeated work and caches.** None. The sheet's model is derived with the workspace from constants and the window's width.
- **Timing.** No interactive path changes cost, so no `editor-performance` run is needed.
- **Exactness.** Nothing rendered or exported changes.

## Decisions

Settled by the owner:

1. **One sheet, not a paged tour or coach marks.** It is read in one go and stays correct when the layout changes; coach marks anchored to controls break whenever Select's regions move.
2. **It names Lightroom.** "Coming from Lightroom?" is what a newcomer is actually asking. The alternative is "If you've used other photo editors", which is vaguer.
3. **The other startup notices wait for it**, the usage claim included, as [behaviour](#behaviour) describes.
4. **Reopened from a ? button in Select's title bar.** There is no Help menu to put it in.

Delegated to the implementer and settled in the build: the drawings' composition, spacing within the stated sizes, the stacking breakpoint above, and the theme's own scrim opacity.
