# Application icon

Luxforge's application icon: light through a six-blade aperture. It is original project art,
licensed like the rest of the project under GPL-3.0-or-later, so the bundle's `LICENSE` covers it
and it needs no third-party notice.

- `luxforge.svg` is the editable source.
- `luxforge.icns` is the macOS icon. `release-build` copies it into
  `Luxforge.app/Contents/Resources` and names it in `CFBundleIconFile`.
- `luxforge-256.png` and `luxforge-512.png` are the Linux icons.

The ICNS and PNGs are generated from the SVG and committed, so packaging needs no rasterizer.
After editing the SVG, regenerate them on macOS with `tools/cargo-cached xtask icon`, which renders
each size with `sips` and assembles the ICNS with `iconutil`, and commit the results.
