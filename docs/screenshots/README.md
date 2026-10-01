# README screenshots

These are native window-renderer captures from the release editor on macOS. They use the existing [landscape fixture](../design/develop-workspace/html/sapa.jpg), referenced without modifying it.

- `develop.png`: Basic, Presence and vignette edits, with the histogram, history and Performance visible.
- `masking.png`: the `mask-panel` scenario's `radial-dragged` frame. Face combines an additive radial, a subtracting brush and an intersecting luminance range; local Exposure and Clarity use the ordinary module controls.

To refresh them, use new evidence directories:

```sh
cargo xtask develop --background --hidden-window \
  --window-size 1440 900 \
  --open docs/design/develop-workspace/html/sapa.jpg \
  --evidence-dir artifacts/readme-develop \
  --evidence-script docs/screenshots/develop.json
cargo xtask smoke --scenario mask-panel --output artifacts/readme-masking
```

Inspect the captures alongside their correlated state and event logs before copying `frame-4.png` from the first run and the `radial-dragged` frame from the second. The smoke scenario checks every frame's state and provenance against its plan. Keep full evidence under `artifacts/`; only the selected PNGs belong here.
