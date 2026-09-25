# Bundled UI font

Every piece of workspace text is set in Inter. The two static instances below are compiled into the binary with `include_bytes!` from `assets/fonts/inter-4.1/` (see `theme::FONT_FILES`) and registered with Iced once at startup, so the font works offline and on every platform without a system install. No other font is bundled; icons are vector paths, not a font.

| Component | Exact upstream | Bundled files | License / notices |
| --- | --- | --- | --- |
| Inter 4.1 | Release [`v4.1`](https://github.com/rsms/inter/releases/tag/v4.1), tag commit `e3a3d4c57d5ecc01453a575621882a384c1995a3`; asset `https://github.com/rsms/inter/releases/download/v4.1/Inter-4.1.zip`, SHA-256 `9883fdd4a49d4fb66bd8177ba6625ef9a64aa45899767dde3d36aa425756b11e` | `extras/ttf/Inter-Regular.ttf` and `extras/ttf/Inter-SemiBold.ttf` from that archive, unmodified; its `LICENSE.txt` | SIL Open Font License 1.1; preserve `LICENSE.txt` beside the files |

| File | SHA-256 |
| --- | --- |
| `assets/fonts/inter-4.1/Inter-Regular.ttf` | `40d692fce188e4471e2b3cba937be967878f631ad3ebbbdcd587687c7ebe0c82` |
| `assets/fonts/inter-4.1/Inter-SemiBold.ttf` | `78a843fade9d4612a5567302fb595b56976eb5fcebf4fea5a5912d638bafcde3` |
| `assets/fonts/inter-4.1/LICENSE.txt` | `262481e844521b326f5ecd053e59b98c8b2da78c8ee1bdbb6e8174305e54935a` |

Both files report `Version 4.001;git-9221beed3` in their name tables, weight classes 400 and 600, and no `fvar` table (static, not variable); the weight classes and the static shape are checked by a test in `src/theme.rs`. The OFL permits bundling and embedding the fonts in software; the font files are not sold on their own and are not renamed or modified. Changing a bundled file must be reviewed as a dependency update. These hashes are provenance evidence, not a completed manual license or asset review.
