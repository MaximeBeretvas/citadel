# Task V3-04: Self-update command (`citadel update`)

- **Phase:** V3
- **Dependencies:** V3-03
- **Suggested Commit:** `feat(update): add citadel update with SHA-256 verification and atomic swap`

## Goal
Implement `citadel update [--check] [--yes]` to upgrade native Citadel binaries from GitHub Releases with mandatory SHA-256 checksum verification, atomic binary replacement (with rollback on failure), daemon restart when active, and non-mutating Apple `container` upgrade guidance when running inside a container.

## Exact Files
- `Cargo.toml`: add `flate2 = "1"` and `tar = "0.4"`.
- `src/update/mod.rs`:
  - `is_running_in_container() -> bool`: checks `/.dockerenv`, `/run/.containerenv`, or `CITADEL_CONTAINER=1`.
  - `target_triple() -> Result<&'static str>`: maps compile-time `(target_os, target_arch)` to supported GitHub Release targets (`aarch64-apple-darwin`, `x86_64-apple-darwin`, `aarch64-unknown-linux-gnu`, `x86_64-unknown-linux-gnu`).
  - `verify_sha256_manifest(manifest: &str, archive_name: &str, bytes: &[u8]) -> Result<()>`: parses `checksums.sha256` and verifies exact SHA-256 match before extraction.
  - `extract_binary_from_tar_gz(archive_bytes: &[u8], dest: &Path) -> Result<()>`: extracts the `citadel` binary safely without path traversal.
  - `atomic_replace_binary(new_bin: &Path, current_bin: &Path) -> Result<()>`: preserves executable permissions, backs up `current_bin`, renames `new_bin` into place, and restores backup on failure.
  - `run_update(opts)`:
    1. If container -> print current/latest version + exact Apple `container` pull/recreate commands and return `Ok(())`.
    2. Query latest release + update `update-check.json`.
    3. If already up to date -> print `"citadel vX.Y.Z is already up to date"`.
    4. If `--check` -> print available version and exit without modifying binary.
    5. Otherwise download tarball + `checksums.sha256`, verify SHA-256, extract to temp file in `current_bin.parent()`, atomically replace binary, and restart native daemon if `daemon.pid` holds a live process.
- `src/cli/mod.rs`:
  - Add `Commands::Update { #[arg(long)] check: bool, #[arg(short = 'y', long)] yes: bool }`.

## Red-First Test Cases
1. `update::tests::sha256_manifest_accepts_valid_digest_and_rejects_tampered_bytes`.
2. `update::tests::extract_tar_gz_extracts_citadel_binary_and_rejects_missing_binary`.
3. `update::tests::atomic_replace_swaps_binary_and_preserves_exec_mode`: verifies replacement on a tempdir fake binary.
4. `update::tests::container_mode_prints_guidance_without_mutating_binary`.
5. `update::tests::end_to_end_update_against_mock_release_server`: local Axum/loopback server serving release JSON, `.tar.gz`, and `checksums.sha256` upgrades a tempdir binary cleanly and updates `update-check.json`.

## Acceptance Criteria
- A checksum mismatch aborts before extraction and leaves the existing binary untouched.
- `--check` never downloads the archive or mutates the binary.
- Container detection prints Apple `container` commands (never `docker`) and exits 0 without mutating.

## Checks Gate
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
