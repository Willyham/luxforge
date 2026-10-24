# Application icon

Luxforge's application icon: the [Midtone](../brand/README.md) mark in the house mix on a graphite
tile. It is original project art, licensed like the rest of the project under GPL-3.0-or-later, so
the bundle's `LICENSE` covers it and it needs no third-party notice.

- `luxforge.svg` is the source. `luxforge-32.svg` and `luxforge-16.svg` are its pixel cuts for the
  32 and 16 pixel sizes, where the scaled drawing loses the mark. All three are written by
  [`packaging/brand/generate.py`](../brand/generate.py); edit the generator, not the SVGs.
- `luxforge.icns` is the macOS icon. `release-build` copies it into
  `Luxforge.app/Contents/Resources` and names it in `CFBundleIconFile`.
- `luxforge-256.png` and `luxforge-512.png` are the Linux icons, and `luxforge.svg` is also its scalable icon. The Linux payload carries all three, and `desktop-integration.sh install` puts them in the hicolor icon theme.

The ICNS and PNGs are generated from the SVGs and committed, so packaging needs no rasterizer.
After regenerating the SVGs, regenerate them on macOS with `tools/cargo-cached xtask icon`, which
renders each size with `sips`, from a pixel cut where there is one, and assembles the ICNS with
`iconutil`, and commit the results.
