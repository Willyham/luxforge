# README screenshots

These are native window-renderer captures from the release editor on macOS, each referencing its photographs without modifying them. The Develop, Select and Corrections shots use the maintainer's own photographs, which are not in the repository; the masking and colour grading shots use the [landscape fixture](../design/develop-workspace/html/sapa.jpg).

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
