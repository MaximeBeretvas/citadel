# CI Gate & Release Workflows Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A required PR checks gate on `main` and a tag-triggered GitHub Release whose assets `citadel update` can download, for macOS (arm64, x86_64) and Linux (x86_64, arm64).

**Architecture:** Two hand-written GitHub Actions workflows. A repo-file test (same pattern as `tests/container_packaging.rs`) pins the release workflow to the asset contract in `src/update/mod.rs`, so the two cannot drift apart unnoticed.

**Tech Stack:** GitHub Actions (`actions/checkout@v4`, `dtolnay/rust-toolchain@stable`, `Swatinem/rust-cache@v2`, `actions/upload-artifact@v4`, `actions/download-artifact@v4`), `gh` CLI, Rust integration tests.

**Spec:** `Docs/superpowers/specs/2026-11-22-ci-release-workflows-design.md`

## Global Constraints

- Branch `ci/release-workflows` cut from `feat/v3`.
- Archive name: `citadel-vX.Y.Z-<target>.tar.gz`; manifest `checksums.sha256` in `sha256sum` format (`<hex>  <file>`); `citadel` binary at archive root.
- Targets: `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`. No Windows.
- Linux builds on `ubuntu-22.04` / `ubuntu-22.04-arm`; macOS builds on `macos-latest`.
- Only the publish job gets `contents: write`; everything else `contents: read`.
- Windows: docs say "not yet tested"; install via `cargo install --git https://github.com/mberetvas/citadel --locked` or build from source.
- Checks gate before every commit: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`.

## Review Focus

- Tag does not match `Cargo.toml` version → release run must fail before building (guard step; pinned by Task 2 test asserting the guard exists).
- An archive with the binary nested in a directory (`target/.../release/citadel`) → `extract_binary_from_tar_gz` would not find it; Task 2 test asserts `tar … -C target/<target>/release citadel`.
- A target added to `release_target_triple` but missing from the matrix (or vice versa) → Task 2 test asserts all four triples appear in `release.yml`.
- A forked PR having write permission → Task 1 test asserts `ci.yml` declares `contents: read` and no `write`.
- Checksum line format differing from what `verify_sha256_manifest` parses → Task 2 test asserts `sha256sum *.tar.gz > checksums.sha256`.

---

### Task 1: PR checks workflow

**Files:**
- Create: `.github/workflows/ci.yml`
- Create: `tests/ci_workflows.rs` (shared by Task 2)

**Interfaces:**
- Produces: `tests/ci_workflows.rs` with helper `fn read_repo_file(rel: &str) -> String` (copy of the `tests/container_packaging.rs` helper); job id `checks` with matrix `os: [ubuntu-latest, macos-latest]`, so the GitHub check names are `checks (ubuntu-latest)` and `checks (macos-latest)` (used by Task 4).

- [ ] **Step 1: Write the failing test** `ci_workflow_runs_the_checks_gate_on_prs`

```rust
let ci = read_repo_file(".github/workflows/ci.yml");
for needle in ["pull_request", "branches: [main]", "ubuntu-latest", "macos-latest",
               "cargo fmt --check",
               "cargo clippy --all-targets --locked -- -D warnings",
               "cargo test --locked", "contents: read"] {
    assert!(ci.contains(needle), "ci.yml must contain {needle:?}");
}
assert!(!ci.contains("write"), "ci.yml must not grant write permissions");
```

- [ ] **Step 2:** `cargo test --test ci_workflows` → FAIL (file missing).
- [ ] **Step 3:** Write `ci.yml`: triggers `pull_request` + `push: branches: [main]`; top-level `permissions: contents: read`; `concurrency: { group: ci-${{ github.ref }}, cancel-in-progress: true }`; job `checks`, matrix as above, steps checkout → toolchain (components `rustfmt, clippy`) → rust-cache → the three cargo commands.
- [ ] **Step 4:** `cargo test --test ci_workflows` → PASS; run full checks gate → green.
- [ ] **Step 5: Commit** `ci: add PR checks workflow for Linux and macOS`

### Task 2: Release workflow

**Files:**
- Create: `.github/workflows/release.yml`
- Modify: `tests/ci_workflows.rs`

**Interfaces:**
- Consumes: `read_repo_file` from Task 1; asset contract from `src/update/mod.rs:51-62` (triples) and `:253` (archive name).

- [ ] **Step 1: Write the failing test** `release_workflow_matches_update_asset_contract`

```rust
let rel = read_repo_file(".github/workflows/release.yml");
for needle in ["tags: ['v*']",
               "aarch64-apple-darwin", "x86_64-apple-darwin",
               "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu",
               "ubuntu-22.04", "ubuntu-22.04-arm",
               "cargo build --release --locked --target",
               "citadel-$GITHUB_REF_NAME-${{ matrix.target }}.tar.gz",
               "-C target/${{ matrix.target }}/release citadel",
               "sha256sum *.tar.gz > checksums.sha256",
               "gh release create",
               "contents: write"] {
    assert!(rel.contains(needle), "release.yml must contain {needle:?}");
}
assert!(rel.contains("GITHUB_REF_NAME") && rel.contains("cargo metadata"),
        "release.yml must guard tag == v<Cargo.toml version>");
assert!(!rel.contains("windows"), "Windows is out of scope");
```

- [ ] **Step 2:** `cargo test --test ci_workflows` → the new test FAILs.
- [ ] **Step 3:** Write `release.yml`: top-level `permissions: contents: read`. Job `build`: matrix `include` of the 4 `{target, os}` pairs; steps: checkout → toolchain with `targets: ${{ matrix.target }}` → guard (`v=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version')`; fail unless `"v$v" = "$GITHUB_REF_NAME"`) → build → tar → `upload-artifact` (name = target). Job `publish`: `needs: build`, `ubuntu-latest`, job-level `permissions: contents: write`; `download-artifact` with `merge-multiple: true` → sha256sum → `gh release create "$GITHUB_REF_NAME" *.tar.gz checksums.sha256 --generate-notes --repo "$GITHUB_REPOSITORY"` with `GH_TOKEN: ${{ github.token }}`.
- [ ] **Step 4:** `cargo test --test ci_workflows` → PASS; full checks gate → green. If `actionlint` is available, `actionlint` → no output.
- [ ] **Step 5: Commit** `ci: publish GitHub Release binaries on version tags`

### Task 3: Docs (PRD, SPEC, README)

**Files:**
- Modify: `Docs/PRD.md` §5 (new "Distribution" bullet)
- Modify: `Docs/SPEC.md` §6 "Release Artifacts", §8.2, new §10 "CI & Release"
- Modify: `README.md` "Install (Recommended)"

- [ ] **Step 1:** PRD §5: add **Distribution** bullet: prebuilt binaries for macOS (Apple Silicon, Intel) and Linux (x86_64, ARM64) on every GitHub Release; every PR passes an automated checks gate before merge; Windows not yet tested (install via `cargo install` or build from source).
- [ ] **Step 2:** SPEC §6 Release Artifacts: state assets are produced by `.github/workflows/release.yml` on `vX.Y.Z` tags; tag must equal `Cargo.toml` version; Linux built on Ubuntu 22.04 (glibc 2.35+). §8.2: add a line: Windows not yet tested, use `cargo install --git … --locked` or build from source; `citadel update` and `daemon install` are unsupported there.
- [ ] **Step 3:** SPEC new §10 "CI & Release": `ci.yml` (triggers, matrix, the three commands, required checks `checks (ubuntu-latest)` / `checks (macos-latest)` on `main`); `release.yml` (targets table, archive/manifest names, publish job); release process: bump `Cargo.toml` → merge → `git tag vX.Y.Z && git push origin vX.Y.Z`; out of scope list from the design doc.
- [ ] **Step 4:** README: under Install, add "Prebuilt binaries" (download from GitHub Releases, verify against `checksums.sha256`, macOS browser downloads need `xattr -d com.apple.quarantine citadel`) and a "Windows" note (not yet tested; use the `cargo install` command above or build from a checkout).
- [ ] **Step 5:** Full checks gate → green (`tests/container_packaging.rs` reads README/SPEC; must still pass).
- [ ] **Step 6: Commit** `docs: document CI gate, release process, and supported platforms`

### Task 4: Push, open PR, make the gate required

- [ ] **Step 1:** `git push -u origin ci/release-workflows`; `gh pr create --base feat/v3` (body via the `pr` skill).
- [ ] **Step 2:** Wait for both `checks (…)` runs; expected: green. If red, fix the root cause on the branch.
- [ ] **Step 3:** **Ask the user first.** Then, on `main`:
  `gh api -X PUT repos/mberetvas/citadel/branches/main/protection --input -` with
  `{"required_status_checks":{"strict":false,"contexts":["checks (ubuntu-latest)","checks (macos-latest)"]},"enforce_admins":false,"required_pull_request_reviews":null,"restrictions":null}`.
  Verify: `gh api repos/mberetvas/citadel/branches/main/protection/required_status_checks` lists both contexts.
- [ ] **Step 4:** Hand off: the first real release test is the user's first `vX.Y.Z` tag after merge (4 tarballs + `checksums.sha256` on the Release page).
