# Installing a Luxforge {{version}} build from source

Build the executables first (`BUILD.md`). Luxforge finds its Lensfun profile index beside the editor executable, so an installation is a directory holding the executables, that index, the licence and the notices. Nothing needs root, and no installer changes your shell or `PATH`.

Your originals, catalogs, settings and caches are never inside the installation: replacing or removing it keeps them.

## Linux

```sh
mkdir -p ~/.local/opt/Luxforge/lensfun
cp target/release/luxforge target/release/luxforge-json target/release/luxforge-ctl ~/.local/opt/Luxforge/
cp crates/luxforge-core/data/lensfun/index.json crates/luxforge-core/data/lensfun/provenance.json \
  crates/luxforge-core/data/lensfun/LICENSE-CC-BY-SA-3.0.txt crates/luxforge-core/data/lensfun/ATTRIBUTION.md \
  ~/.local/opt/Luxforge/lensfun/
cp LICENSE ~/.local/opt/Luxforge/
cp -R notices/linux-x86_64 ~/.local/opt/Luxforge/notices
~/.local/opt/Luxforge/luxforge
```

On ARM64 Linux, copy `notices/linux-arm64` instead of `notices/linux-x86_64`. The command lines run from the same directory, for example `~/.local/opt/Luxforge/luxforge-json --version`. The editor needs a Vulkan driver, the X11 or Wayland libraries and an XDG desktop portal with a file-chooser backend.

## macOS

The release's `Luxforge.app` is made by `cargo xtask release-build` from the Git repository at the release tag, which also embeds the pinned Sparkle updater it downloads. To run what you built from this archive, use the same layout in a directory of your own:

```sh
mkdir -p ~/Applications/Luxforge/lensfun
cp target/release/luxforge target/release/luxforge-json target/release/luxforge-ctl ~/Applications/Luxforge/
cp crates/luxforge-core/data/lensfun/index.json crates/luxforge-core/data/lensfun/provenance.json \
  crates/luxforge-core/data/lensfun/LICENSE-CC-BY-SA-3.0.txt crates/luxforge-core/data/lensfun/ATTRIBUTION.md \
  ~/Applications/Luxforge/lensfun/
cp LICENSE ~/Applications/Luxforge/
cp -R notices/macos-arm64 ~/Applications/Luxforge/notices
~/Applications/Luxforge/luxforge
```

A build of your own is not signed or notarized by the Luxforge project, and it has no updater: automatic updates need the Sparkle framework inside an app bundle.

## Removing it

Delete the installation directory. Catalogs and originals stay where you keep them.
