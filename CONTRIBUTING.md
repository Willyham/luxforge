# Contributing

Read [AGENTS.md](AGENTS.md) first. It holds the pillars, workflow and engineering rules for every change.

## Layout

- `crates/lightwell-core`: images, recipes, rendering, SQLite catalog and history, preview scheduling, JSON API.
- `crates/lightwell-app`: Iced desktop adapter plus the desktop and headless `lightwell-json` binaries.
- `xtask`: all development, check, evidence and packaging commands.
- `docs/`: product, design, spec, engineering and research documentation.
- `tasks/`: JSON task plans ([index](tasks/README.md)).
- `fixtures/`: small synthetic inputs with a hash manifest ([details](fixtures/README.md)). Generated large workloads live in ignored `fixtures/generated/`.
- `probes/raw`: isolated RAW decoder experiments, not the maintained application.
- Ignored: `artifacts/` (per-run evidence), `dist/`, `private/` and `fixtures/jpg/` (local originals; never force-add).

## Before handing off

```sh
cargo xtask check
```

This runs repository checks (Markdown links, task-plan schemas and DAGs), formatting, Clippy and tests. Run it once the change is complete; while working, run the tests for the code you are changing (`cargo test -p CRATE FILTER`). UI or image changes also need a real rendered check; see [development](docs/engineering/development.md). Put run evidence in a fresh `artifacts/<run-id>/`. Never commit private paths, machine identifiers, screenshots, logs, build outputs or camera originals. Only synthetic or licensed inputs may enter CI.

Keep documentation focused on current behavior and outstanding work. Update [feature status](docs/features.md) and the [user guide](docs/user-guide.md) when behavior or scope changes. Preserve task IDs and truthful statuses when editing a plan.
