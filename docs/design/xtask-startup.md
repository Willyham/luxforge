# Fast development-tool startup

## Behavior and scope

`cargo xtask` uses a small `xtask-cli` package for repository validation, formatting,
Clippy, tests, builds and dependency audits. It has no image, native codec or workspace
library dependency. These commands call the existing validators and parallel test
runner. The full `xtask` package depends on that shared tooling library, so checks
and advisory policy have one implementation and their tests run once.

Photo, evidence, packaging and verification commands delegate to the existing `xtask`
package through Cargo. Arguments remain OS strings, the debug/release profile is
preserved and the child's exit status reaches the caller. Explicit
`cargo run --release --locked --package xtask -- ...` commands remain available.
The whole workspace's Clippy and test commands still include both packages and all
acceptance tests; no feature flag removes tests from ordinary verification.

## Constraints and acceptance

- Keep source validation, exact advisory versions, UTC expiry and task checks intact.
- Keep compiler-wrapper and package-environment handling for nested Cargo commands.
- Reject unknown commands before building photo tooling. Forward heavy-command
  arguments without shell interpolation; preserve incomplete-verification exit codes.
- Enforce the lightweight package's dependency boundary through the repository rules.
- Prove the resolved dependency graph excludes photo/native libraries, compare fresh
  builds with incremental compilation and sccache disabled, and distinguish local
  build measurements from hosted CI savings.
- Run focused tests and a delegated command without an app launch, then the final
  quick tier. Update the development guide, architecture, feature status and user guide.

There are no open product decisions. Release profiles, test scheduling, editor behavior
and renderer settings are outside this change.

## Measured startup scope

On the owner's M4, Rust 1.94.0, one fresh dev build per variant with registry sources
already present, separate empty target directories, incremental compilation disabled
and sccache bypassed:

| Tool entry point | Fresh compilation |
| --- | --- |
| Full photo/evidence `xtask` | 72.44 s |
| Lightweight `xtask-cli` | 4.06 s |

The local entry-point build was about 18 times faster (94% less wall time). External
locked dependency versions and checks are unchanged. This is a diagnostic sample of
tooling compilation, not a full-suite, image-processing or hosted CI speed claim;
whole-workspace checks still compile and test the photo and evidence tools. Hosted
startup and warm-cache savings remain unmeasured.
