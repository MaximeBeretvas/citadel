# CI gate & release workflows — design

> Size: small change. Status: awaiting approval.

## Intent

- **PR gate:** every PR shows whether it is safe to merge, and a red PR cannot be merged into `main`.
- **Releases:** pushing a version tag publishes a GitHub Release in the exact shape `citadel update` downloads (`src/update/mod.rs:51-62, 253`). Today the repo has no tags or releases, so `citadel update` has nothing to fetch.
- **Binaries:** prebuilt for macOS (Apple Silicon + Intel) and Linux (x86_64 + ARM64).

## Goals / non-goals

- Goals: the two workflows, a required check on `main`, docs (PRD, SPEC, README) matching.
- Non-goals (Windows, deferred to future work on a Windows machine): Windows CI, Windows release binary, Windows self-update and daemon. Docs say Windows is **not yet tested** and point Windows users to `cargo install --git …` or building from source.
- Also out (YAGNI): container build in CI (no Apple `container` on GitHub runners; `docker` is ruled out), `--features chrome` builds, `cargo audit`/Dependabot, coverage, macOS signing/notarization, automatic versioning.
- Anti-goal: release files whose name or checksum format differs from what `citadel update` parses. That would be a release that looks fine but breaks every update.

## Approaches

| | Approach | Trade-off |
|---|---|---|
| **A (picked)** | Hand-written GitHub Actions: `ci.yml` + tag-triggered `release.yml` | You bump the version and push the tag by hand; about 80 lines of YAML you own, and the file names already match `citadel update` |
| B | `cargo-dist` | Its file names and checksum format differ, so the update code you just shipped would need rewriting; generated YAML |
| C | A + `release-please` | Releases without remembering to tag, but one more bot; can be added later on top of A without changes |

## Design

**`.github/workflows/ci.yml`**: runs on `pull_request` and on `push` to `main`
- Matrix `ubuntu-latest`, `macos-latest`; each runs `cargo fmt --check`, `cargo clippy --all-targets --locked -- -D warnings`, `cargo test --locked` (same gate as AGENTS.md).
- `permissions: contents: read`; a new push to the same PR cancels the older run; `Swatinem/rust-cache`.

**Required check on `main`**: after the first green run, make both `checks (…)` jobs required status checks on `main` via `gh api` (branch protection). This changes a GitHub setting, so I confirm with you before running it.

**`.github/workflows/release.yml`**: runs on `push` of tags `v*`
- Guard step: the tag must equal `v` + `Cargo.toml` version, or the run fails.
- Build matrix (`cargo build --release --locked --target …`):
  - `aarch64-apple-darwin` on `macos-latest`
  - `x86_64-apple-darwin` on `macos-latest` (cross-compiled; GitHub's Intel macOS runners are being retired)
  - `x86_64-unknown-linux-gnu` on `ubuntu-22.04`
  - `aarch64-unknown-linux-gnu` on `ubuntu-22.04-arm`
  - (Linux builds use 22.04 so the binary also runs on older distros with glibc 2.35+.)
- Each target is packed as `citadel-vX.Y.Z-<target>.tar.gz`, with the `citadel` binary at the root of the archive.
- Publish job (the only one with `contents: write`): `sha256sum *.tar.gz > checksums.sha256`, then `gh release create <tag> … --generate-notes`.

**Release process (manual)**: bump `version` in `Cargo.toml`, merge to `main`, `git tag vX.Y.Z && git push origin vX.Y.Z`.

**Docs**
- PRD: a "Distribution" non-functional requirement covering the prebuilt targets, Windows not yet tested, and the merge gate.
- SPEC: Release Artifacts lists the 4 targets and links the workflow. New "CI & Release" section covering the gate, release steps, and Windows/cargo fallback. Native install notes Windows.
- README: Windows note under Install; macOS note that a browser-downloaded binary is quarantined by Gatekeeper (`xattr -d com.apple.quarantine citadel`), while `citadel update` and `cargo install` are not.

## Verification

- `ci.yml`: this PR itself runs it; both jobs must be green.
- `release.yml`: it cannot run before a tag exists. After merge, the first tag you push (e.g. `v0.1.0`) is the real test: check the Release has 4 tarballs plus `checksums.sha256`, and `citadel update --check` from a build of an older version sees it.
- Lint the YAML with `actionlint` if it is available.

## Left to the builder

Branch: `ci/release-workflows`, cut from `feat/v3` (the SPEC update section lives there); PR base `feat/v3`, or `main` once PR #1 is merged. Exact wording in the docs.
