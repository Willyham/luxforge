# Bundled Omarchy themes

Each `<slug>/colors.toml` here is Omarchy's own `themes/<slug>/colors.toml`, one for each of Omarchy's 22 built-in themes, copied unmodified from [Omarchy](https://github.com/omacom/omarchy) at commit `035ce29f03bdd97a09af80ef5f2d22d7a98930d6` (branch `quattro`, 2026-10-04). Omarchy is distributed under the MIT licence in `LICENSE` beside this file, copied unmodified from the same commit. `luxforge_core::theme::omarchy` embeds the files and a test pins each one's SHA-256 to that commit's bytes; changing a file, the set or the commit is a change of its own.

Each palette is Omarchy's rendering of an upstream colour scheme. For the sixteen added after the first six, the upstream named here is the editor plugin or extension that Omarchy's own theme folder at that commit names for the theme (in its Neovim or VS Code file):

| File | Upstream scheme |
| --- | --- |
| `tokyo-night/colors.toml` | [Tokyo Night](https://github.com/enkia/tokyo-night-vscode-theme) |
| `catppuccin/colors.toml` | [Catppuccin](https://github.com/catppuccin/catppuccin), Mocha |
| `catppuccin-latte/colors.toml` | [Catppuccin](https://github.com/catppuccin/catppuccin), Latte |
| `gruvbox/colors.toml` | [Gruvbox Material](https://github.com/sainnhe/gruvbox-material), a variant of [Gruvbox](https://github.com/morhetz/gruvbox) |
| `nord/colors.toml` | [Nord](https://github.com/nordtheme/nord) |
| `everforest/colors.toml` | [Everforest](https://github.com/sainnhe/everforest) |
| `flexoki-light/colors.toml` | [Flexoki](https://github.com/kepano/flexoki-neovim), light |
| `hackerman/colors.toml` | [hackerman.nvim](https://github.com/bjarneo/hackerman.nvim) |
| `kanagawa/colors.toml` | [Kanagawa](https://github.com/rebelot/kanagawa.nvim) |
| `last-horizon/colors.toml` | [Ship at Sea](https://marketplace.visualstudio.com/items?itemName=rikkarth.ship-at-sea) (`rikkarth.ship-at-sea`) |
| `lumon/colors.toml` | [lumon.nvim](https://github.com/omacom-io/lumon.nvim) |
| `matte-black/colors.toml` | [matteblack.nvim](https://github.com/tahayvr/matteblack.nvim) |
| `osaka-jade/colors.toml` | [bamboo.nvim](https://github.com/ribru17/bamboo.nvim) |
| `retro-82/colors.toml` | [retro-82.nvim](https://github.com/OldJobobo/retro-82.nvim) |
| `rose-pine/colors.toml` | [Rosé Pine](https://github.com/rose-pine/neovim), Dawn |
| `solitude/colors.toml` | [Ashen](https://github.com/ficcdaf/ashen.nvim) |

The other six, `ethereal`, `lupine`, `miasma`, `ristretto`, `vantablack` and `white`, name no editor plugin or extension in their theme folders at that commit, so no upstream is recorded for them.

These records are provenance, not a completed manual licence review.
