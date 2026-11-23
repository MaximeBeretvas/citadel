# Task V4-02: `chrome` on by default, with a lite opt-out

- **Phase:** V4
- **Dependencies:** V4-01
- **Suggested Commit:** `feat(build): enable chrome by default with make install-lite/build-lite opt-out`

## Goal
Every default build (cargo install, release tarballs, container image) can render JavaScript;
a lite build without the browser stack is one Makefile target away.

## Exact Files
- `Cargo.toml`: `default = ["chrome"]`.
- `Makefile`: `install` / `install-lite` (`cargo install --path . --locked --force
  [--no-default-features]`); `build` defaults to `FEATURES ?= chrome` and always passes
  `--build-arg FEATURES=`; `build-lite` = `build` with empty `FEATURES`.
- `Dockerfile`: `ARG FEATURES="chrome"`; `cargo build --no-default-features --features "$FEATURES"`.
- `src/update/mod.rs`: `feature_loss_warning` → warn lite builds that the release adds JS rendering.
- `tests/container_packaging.rs`: assert the new defaults.
- `README.md`, `Docs/SPEC.md`, `AGENTS.md`: default includes chrome; lite opt-out; Chrome only
  needed for `render_js` Sources.

## Red-First Test Cases
1. `update::tests::updating_a_lite_build_says_the_release_adds_js_rendering`: warning only when
   built without `chrome`.
2. `container_packaging`: Dockerfile defaults `FEATURES="chrome"` and uses `--no-default-features`;
   Makefile has `install-lite` and `build-lite`.

## Acceptance Criteria
- `cargo build` includes `chrome`; `make install-lite` produces a binary without it.
- `make build` and `make build-lite` both build with Apple `container`.

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --no-default-features
container build -t citadel:check .
```
