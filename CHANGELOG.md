# Changelog

Each release's notes include its section here. A pre-release such as `v0.2.0-rc.1` uses the section of its version, `0.2.0`.

## 0.2.0

### Agents

- **`luxforge-mcp`**, an MCP server for the editor, ships beside `luxforge-ctl` in the macOS app and the Linux payload. It drives a running desktop's live session, or a catalog of its own with `--headless --catalog PATH`, and refuses a catalog a desktop has open.
- `luxforge-mcp --tools` offers one typed tool per method. Each tool's input schema is generated from the parameter vocabulary, and its annotations say whether it reads or changes. The server tells clients when the list changes.
- `luxforge-mcp config CLIENT` prints the configuration for Claude Code, Claude Desktop, Codex, Cursor or generic MCP JSON. It never writes another application's files.
- **Settings › Agents** shows this build's `luxforge-mcp` and each client's configuration, with Copy. It lists the connected clients, and Disconnect ends a connection.
- `schema.list` marks each method's authority, effects and images. Live clients are named and listed through `session.clients`.

### AI tools

- **Setup with one consent.** Each AI tool declares the models it needs. The desktop shows a setup line per tool and one setup sheet that asks once for everything missing. Settings › Models groups models by the tools that use them.
- **Corrections › Object** (behind the `ai.analysis` experiment, off by default) picks objects to remove. Hover outlines an object and a click selects it, with up to eight objects per removal. Paint shadow brushes in an object's shadow so it goes too. Generate fills the joined region.
- **Masks › Object…** (behind the same experiment) adds a picked object to a mask as a model component, with Recompute and Keep on a stale component.
- Generative fill is always available; it no longer needs a feature flag. While a generation is running, a notice pinned to the tools panel says the editor may be slow.
- Fixes from qualification on the M4: a small object keeps its removal region, and a point on a small object picks the object, not what surrounds it. A selection too large to fill is refused in a plain status line. Status lines and hover outlines no longer lag behind the selected tool and object.
- A Remove stroke is drawn at the brush's current size while you paint it, not at the size of the selection already painted.

### Workspace

- Luxforge launches into Develop where you left it: the same catalog folder and photograph. If the folder is gone, the photograph opens with its own folder.
- Select's Info panel shows the selected photograph's preview, in a box that does not shift when the preview arrives.
- Select's title bar has the Settings gear, and Cmd+, shows Settings in Select too.
- With no Develop folder chosen and no photograph open, `D` in Select develops the picks in view, as **Develop N** does.
- All 22 of Omarchy's built-in themes are built in, not only six of them.
- Range masks are always available. Feature flags are reduced to developer mode and `ai.analysis`.
- Updates follow the default and pre-release channels without a flag, and `update.status` no longer reports `prerelease_channel`.

### Downloads

- **ARM64 Linux.** Each release adds `luxforge-VERSION-linux-arm64.tar.gz`, built on Ubuntu 24.04 for ARM64 like the x86-64 tarball, for ARM64 machines and virtual machines such as Linux on an Apple silicon Mac, with its own update feed. Like the x86-64 build, it has no native GPU, Wayland, X11 or installation qualification yet.

### Brand

- A new app icon, the Midtone mark on a graphite tile, with drawn cuts at 16 and 32 pixels.
- The empty Develop canvas shows the Midtone lockup, with a short animation as it appears.

### Security and dependencies

- Tokio 1.53.3, for RUSTSEC-2026-0386.

## 0.1.0

- The first pre-release build candidate.
