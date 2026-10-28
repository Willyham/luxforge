//! The fixed pixel patches the harness and `cargo xtask module-check` run a module's reference
//! over: the host's own probe patches (`luxforge_gpu_types::probe`), so a module is checked over
//! the pixels the host probes its program on.
pub use luxforge_gpu_types::probe::{Patch, SIDE, SPLIT, patches};
