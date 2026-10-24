# Screenshots

These are native window-renderer captures from the release editor on macOS, each referencing its photographs without modifying them.

## The README's images

The README shows the [website](https://luxforge.app)'s captures of Luxforge 0.1.0 (the `v0.1.0` tag), copied from the website's `site/img` as it ships them: sRGB WebP without metadata. Each comes from a hidden 1440 × 900 background launch with a fresh catalog and no `script_step_failed` event in its log, at 2×. The landscape is the [fixture](../design/develop-workspace/html/sapa.jpg); the other photographs are the maintainer's own and are not in the repository.

- `develop.webp` (2880 × 1800): `captures/develop.json` on the landscape fixture, its last frame at rev 3.
- `agent.webp` (1440 × 900): `captures/agent.json`, which only waits, on a Nikon Z 6 RAW, while Claude Code was asked to "make the photo I have open in Luxforge look vintage" and sent the requests in [`captures/agent-transcript.txt`](captures/agent-transcript.txt) through `luxforge-ctl`; frame 30, at rev 6. `agent-before.webp` and `agent-after.webp` (560 × 373) are the photograph from frame 1 and from that frame.
- `select.webp` (1440 × 900): the maintainer's own window screenshot of Select over a 792-photograph folder, cropped to 16:10, with six sidebar folder names replaced and the Info panel's preview filled with the selected photograph's embedded preview.
- `masking.webp` (1440 × 900): `cargo xtask smoke --scenario mask-panel`'s `radial-dragged` frame, with the masks renamed Upper slopes, Paddies and Lower slopes for that run.
- `remove.webp` (1440 × 900): `captures/remove.json` on a Nikon Z 6 RAW with the `ai.generative` flag on and its models installed; frame 10, after Accept, at rev 2. `remove-before.webp` and `remove-after.webp` (420 × 280) are crops of the road before the stroke and after.

The full commands, crops, encoder settings and hashes are in the website repository's `site/img/README.md`. Refresh these by copying that repository's images again, not by re-encoding here.

## The OmaStore listing

[`omastore.toml`](../../omastore.toml) lists the PNGs. The Develop, Select and Corrections shots use the maintainer's own photographs; the masking and colour grading shots use the landscape fixture.

- `develop.png`: a Nikon Z6 RAW with the Standard look, its automatic lens profile, and Basic, Presence, Tone curve and vignette edits.
- `select.png`: the Select workspace over a folder of five photographs, two of them picked, ungrouped.
- `masking.png`: the `mask-panel` scenario's `radial-dragged` frame. Face combines an additive radial, a subtracting brush and an intersecting luminance range; local Exposure and Clarity use the ordinary module controls.
- `grading.png`: the colour mixer's Grading tab with shadow and highlight tints.
- `corrections.png`: a DJI Air 2S DNG in the Corrections mode with six Heal spots, each with an automatic source.

To refresh them, use new evidence directories and absolute paths to the photographs:

```sh
cargo xtask develop --background --hidden-window --window-size 1440 900 \
  --open /ABSOLUTE/PATH/TO/RAW --evidence-dir artifacts/readme-develop \
  --evidence-script docs/screenshots/develop.json
sed "s#PHOTO_FOLDER#/ABSOLUTE/PATH/TO/FOLDER#" docs/screenshots/select.json > artifacts/select.json
cargo xtask develop --background --hidden-window --window-size 1440 900 \
  --evidence-dir artifacts/readme-select --evidence-script artifacts/select.json
cargo xtask smoke --scenario mask-panel --output artifacts/readme-masking
cargo xtask develop --background --hidden-window --window-size 1440 900 \
  --open "$PWD/docs/design/develop-workspace/html/sapa.jpg" --evidence-dir artifacts/readme-grading \
  --evidence-script docs/screenshots/grading.json
cargo xtask develop --background --hidden-window --window-size 1440 900 \
  --open /ABSOLUTE/PATH/TO/AIR2S.DNG --evidence-dir artifacts/readme-corrections \
  --evidence-script docs/screenshots/corrections.json
```

The Corrections script's spot positions are for the maintainer's Air 2S photograph; move them for another. Inspect each run's last frame alongside its correlated state and event logs (no `script_step_failed` events) before copying it here; for masking, copy the `radial-dragged` frame named in `mask-panel-checks.json`. The smoke scenario checks every frame's state and provenance against its plan. Keep full evidence under `artifacts/`; only the selected PNGs belong here.
