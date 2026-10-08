//! `luxforge-json` across its process boundary, one module per slice of what the headless binary
//! does, each test starting processes of its own: the process itself (`process`), developing picks
//! (`develop`), Auto tone (`auto_tone`), the preset methods over its pipe (`presets`), and the opt-in authentic RAW
//! journeys (`raw`), which are ignored
//! unless the owner's RAW files are supplied. Narrow a run with the module path, for example
//! `cargo test -p luxforge-cli --test json_cli presets::`.

mod auto_tone;
mod develop;
mod lens;
mod presets;
mod process;
mod raw;
