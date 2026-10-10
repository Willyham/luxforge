//! The pure numerical library of merges: one module per merge kind, each owning its suitability
//! validation, exposure or coordinate resolution and pixel equations (`docs/design/hdr-merge.md`).
//! Nothing here reads a file, touches the catalog or schedules work: the merge job composes these
//! functions with the shared verified source development, a bounded worker, durable derived
//! storage and the command service. Only `hdr` is a merge kind.

pub mod hdr;
