# `citadel doctor` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a read-only `citadel doctor [--json]` command that reports, in one go, the version, running and setup status of Citadel.

**Architecture:** A new `src/doctor/` module. Each area (version, running, setup) has *gather* functions that read the world (files, `ps`, HTTP, child processes) into plain fact structs, and pure *check* functions that turn facts into `Check` rows. Tests drive the check functions with hand-built facts. `run_doctor` wires gather to check. The daemon gains a sibling `daemon.json` (pid, version, exe, exe mtime, start time) so doctor can compare versions exactly.

**Tech Stack:** Rust, clap, serde/serde_json, toml_edit, reqwest (already deps). No new dependency.

**Spec:** `Docs/superpowers/specs/2026-11-25-doctor-command-design.md`

## Global Constraints

- Doctor is read-only: it never creates the data dir, `vault.db`, `update-check.json` or any config file, never starts or stops processes, never spawns the background update refresh, and never prints the "Update available" notice.
- Exit code `0` when no check has status `fail` (warnings allowed); `1` otherwise (via `anyhow::bail!`, like `sync --all`).
- Markers: `✓` ok, `·` info, `!` warn, `✗` fail. Every `warn`/`fail` row carries a one-line `hint`.
- Timeouts: live release check 3 s; serve probe 1 s; `<other citadel> --version` 2 s.
- `CITADEL_NO_UPDATE_CHECK=1`: skip the live release check, use the cache only.
- `daemon.pid` format is unchanged (one integer). Old binaries must keep working.
- AGENTS.md: `thiserror`/`Result` in library code, `anyhow` only in CLI, no `unwrap`/`expect` outside `#[cfg(test)]`; `cargo build --no-default-features` must compile and pass tests.
- Service, process and launchctl/systemctl checks: macOS and Linux only; elsewhere one `info` row "not supported on this platform".
- No history (sync runs, `daemon.log`) anywhere in doctor.

## Review Focus

1. **A config file exists but has no `citadel` entry** (e.g. `~/.claude.json` with other servers, or an empty file): report "not configured" (`info`), never `fail`. → Task 5 test `mcp_entry_absent_in_valid_config_is_none`.
2. **`citadel` on PATH is a symlink to the binary running doctor** (`~/.local/bin/citadel` → release binary): canonicalize both paths before comparing, so it is "same binary", not a version probe or a warning. → Task 5 test `stdio_command_symlinked_to_current_exe_is_same`.
3. **Fresh machine, no data dir yet:** doctor must not create it, and exits 0. → Task 6 integration test `doctor_on_fresh_home_creates_nothing`.
4. **GitHub unreachable or returns 403 (rate limit):** fall back to cache, never `fail`, finish within the 3 s budget. → Task 2 test `latest_release_falls_back_to_cache_on_http_error`.
5. **`daemon.json` left by a crashed daemon whose pid now belongs to a new daemon started by an older binary:** the pid in `daemon.json` does not match the lock holder → treat as absent ("started by an older citadel"), not as the new daemon's version. → Task 2 test `daemon_info_with_other_pid_is_ignored`.

---

### Task 1: Daemon writes `daemon.json`

**Files:**
- Create: `src/daemon/info.rs`
- Modify: `src/daemon/mod.rs` (declare `pub mod info;`; write the info in `run_daemon_foreground`'s caller path right after `PidLock::acquire` in `start_daemon` foreground branch; remove it in `stop_daemon`'s `Stale` arm)
- Modify: `src/config.rs` (add `daemon_info_path()`)
- Test: inline `#[cfg(test)]` in `src/daemon/info.rs`

**Interfaces:**
- Produces:
  - `VaultConfig::daemon_info_path(&self) -> PathBuf` → `<data_dir>/daemon.json`
  - `#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] pub struct DaemonInfo { pub pid: u32, pub version: String, pub exe: PathBuf, pub exe_mtime_secs: Option<i64>, pub started_at: String }` (`started_at` ISO-8601 via `update::check::format_iso8601_utc`)
  - `impl DaemonInfo { pub fn for_current_process() -> Result<Self> }` (pid = `std::process::id()`, version = `CARGO_PKG_VERSION`, exe = `std::env::current_exe()`, mtime of that file in unix secs)
  - `pub fn read_daemon_info(path: &Path) -> Option<DaemonInfo>` (missing or unparsable → `None`)
  - `pub struct DaemonInfoGuard` — `pub fn write(path: &Path, info: &DaemonInfo) -> Result<Self>` writes atomically (temp file + rename); `Drop` removes the file.

- [ ] **Step 1: Write failing tests** in `src/daemon/info.rs`:
  - `daemon_info_round_trips`: write a `DaemonInfo{pid:42, version:"9.9.9", exe:"/x/citadel", exe_mtime_secs:Some(100), started_at:"2026-11-25T00:00:00Z"}` via `DaemonInfoGuard::write` to a tempdir; `read_daemon_info` returns an equal value.
  - `guard_drop_removes_file`: after dropping the guard, `!path.exists()`.
  - `read_daemon_info_returns_none_for_garbage`: file containing `"37556\n"` → `None`.
  - `pid_file_format_is_unchanged`: acquire `PidLock` in a tempdir, then `fs::read_to_string(pid_path).trim() == std::process::id().to_string()` (guards the old-binary compatibility).
- [ ] **Step 2:** `cargo test daemon::info` → FAIL (module missing).
- [ ] **Step 3: Implement** the interfaces above. In `start_daemon`'s foreground branch, after `let _lock = PidLock::acquire(...)`, add `let _info = DaemonInfoGuard::write(&config.daemon_info_path(), &DaemonInfo::for_current_process()?)?;` so both drop together. In `stop_daemon`'s `Stale` arm also `let _ = fs::remove_file(config.daemon_info_path());`. Do **not** touch `serve` (it takes no lock; out of scope).
- [ ] **Step 4:** `cargo test daemon` → PASS.
- [ ] **Step 5: Commit** `feat(daemon): record version and binary in daemon.json`

---

### Task 2: Report model and Version checks

**Files:**
- Create: `src/doctor/mod.rs` (`pub mod report; pub mod version;` — later tasks add `running`, `setup`), `src/doctor/report.rs`, `src/doctor/version.rs`
- Modify: `src/lib.rs` (`pub mod doctor;`)
- Modify: `src/update/check.rs` only if needed to reuse `fetch_release` (it is `pub(crate)`, which is enough)

**Interfaces:**
- Consumes: `DaemonInfo`, `read_daemon_info` (Task 1); `crate::daemon::{DaemonStatus, daemon_status}`; `crate::update::check::{fetch_release, read_cached_status, is_newer_version}`.
- Produces (`report.rs`):
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)] #[serde(rename_all = "lowercase")] pub enum CheckStatus { Ok, Info, Warn, Fail }`
  - `#[derive(... Serialize)] #[serde(rename_all = "lowercase")] pub enum Section { Version, Running, Setup, Data }`
  - `#[derive(Debug, Clone, PartialEq, Eq, Serialize)] pub struct Check { pub section: Section, pub name: String, pub status: CheckStatus, pub detail: String, pub hint: Option<String> }` with constructors `Check::ok/info/warn/fail(section, name, detail)` and `.with_hint(&str)`.
  - `#[derive(Debug, Default, Serialize)] pub struct Report { pub checks: Vec<Check> }` with `fail_count(&self) -> usize`, `render_text<W: Write>(&self, out: &mut W) -> std::io::Result<()>`, `render_json<W: Write>(&self, out: &mut W) -> std::io::Result<()>`.
- Produces (`version.rs`):
  - `pub enum LatestRelease { Live(String), Cached { version: String, checked_at: String }, Unknown }`
  - `pub async fn latest_release(cache_path: &Path, api_url: &str, current: &str, live: bool) -> LatestRelease` (live check wrapped in `tokio::time::timeout(3s)`; on any error or `live == false` use `read_cached_status`; doctor never writes the cache)
  - `pub fn cli_version_check(current: &str, latest: &LatestRelease) -> Check`
  - `pub struct DaemonFacts { pub status: DaemonStatus, pub info: Option<DaemonInfo>, pub exe_mtime_now: Option<i64>, pub service_installed: bool }` — `info` is already filtered: `None` unless `info.pid` equals the `Running(pid)` pid.
  - `pub fn gather_daemon_facts(config: &VaultConfig, service_installed: bool) -> Result<DaemonFacts>`
  - `pub fn daemon_version_checks(current: &str, facts: &DaemonFacts) -> Vec<Check>` (empty unless `Running`)
  - `pub fn restart_hint(service_installed: bool) -> String`: no service → `citadel daemon stop && citadel daemon start`; macOS service → `launchctl kickstart -k gui/$(id -u)/com.citadel.daemon`; Linux service → `systemctl --user restart citadel.service`.

Text layout (exact): a first line `citadel doctor`, then for each section in order Version, Running, Setup, Data that has rows: two-space-indented title (`Version`), each row four-space-indented `<marker> <detail>`, and when `hint` is set a six-space-indented `→ <hint>`. JSON: `{"checks":[{"section":"version","name":…,"status":"ok","detail":…,"hint":null}]}`.

- [ ] **Step 1: Write failing tests** (`report.rs`):
  - `text_groups_rows_by_section_with_markers_and_hints`: report with a `Running` warn row (hint `"citadel daemon start"`) and a `Version` ok row renders exactly
    `"citadel doctor\n  Version\n    ✓ citadel 0.3.0\n  Running\n    ! daemon stale\n      → citadel daemon start\n"`.
  - `json_uses_lowercase_status_and_section`: parsed JSON `checks[0]["status"] == "warn"`, `["section"] == "running"`.
  - `fail_count_counts_only_fail`.
- [ ] **Step 2: Write failing tests** (`version.rs`):
  - `cli_up_to_date_is_ok`: `("0.3.0", Live("0.3.0"))` → `Ok`, detail contains `"latest: 0.3.0"`.
  - `cli_behind_is_warn_with_update_hint`: `Live("0.4.0")` → `Warn`, hint `Some("citadel update")`.
  - `cli_cached_says_cached`: `Cached{version:"0.3.0", checked_at:"2026-11-24T10:00:00Z"}` → detail contains `"cached, checked 2026-11-24"`.
  - `cli_unknown_is_info`: `Unknown` → `Info`, detail contains `"latest version unknown (offline)"`.
  - `latest_release_falls_back_to_cache_on_http_error` (Review Focus 4, `#[tokio::test]`): axum route returning 403; tempdir cache with `0.3.0`; result is `Cached{..}`; elapsed < 3.5 s; cache file content unchanged.
  - `daemon_same_version_same_binary_is_ok`.
  - `daemon_other_version_warns_with_restart_hint`: info `version:"0.2.0"`, `service_installed:false` → `Warn`, hint `"citadel daemon stop && citadel daemon start"`.
  - `daemon_binary_replaced_warns`: same version, `exe_mtime_secs: Some(100)`, `exe_mtime_now: Some(200)` → `Warn`, detail contains `"binary replaced since the daemon started"`.
  - `daemon_binary_missing_warns`: `exe_mtime_now: None` → detail contains `"binary no longer exists"`.
  - `daemon_without_info_warns_older_citadel`: `Running(5)`, `info: None` → detail contains `"started by an older citadel"`.
  - `daemon_info_with_other_pid_is_ignored` (Review Focus 5): tempdir with a held `PidLock` (pid = this process) and a `daemon.json` with `pid: 1` → `gather_daemon_facts(..).info == None`.
- [ ] **Step 3:** `cargo test doctor::` → FAIL.
- [ ] **Step 4: Implement** the interfaces above.
- [ ] **Step 5:** `cargo test doctor::` → PASS.
- [ ] **Step 6: Commit** `feat(doctor): add report model and version checks`

---

### Task 3: Running checks — daemon state and OS service

**Files:**
- Create: `src/doctor/running.rs`
- Modify: `src/daemon/service.rs` (add `pub fn service_file_path() -> Result<Option<PathBuf>>`: macOS → `launchd_plist_path()`, Linux → `systemd_unit_path()`, else `Ok(None)`)

**Interfaces:**
- Consumes: `Check`, `Section` (Task 2); `DaemonStatus`; `LAUNCHD_LABEL`, `SYSTEMD_SERVICE_NAME`.
- Produces:
  - `pub fn daemon_running_check(status: DaemonStatus, pid_path: &Path) -> Check` — `Running(pid)` ok `"daemon running (pid N)"`; `Stopped` info + hint `citadel daemon start`; `Stale` warn `"stale daemon.pid at <path>; no daemon holds it"` + hint `citadel daemon start`.
  - `pub struct ServiceFile { pub path: PathBuf, pub binary: Option<PathBuf>, pub data_dir: Option<PathBuf> }`
  - `pub fn parse_launchd_plist(text: &str) -> (Option<PathBuf>, Option<PathBuf>)` — first `<string>` after `<key>ProgramArguments</key>`, and the `<string>` after `<key>CITADEL_HOME</key>`; XML-unescape `&amp; &lt; &gt; &quot; &apos;`.
  - `pub fn parse_systemd_unit(text: &str) -> (Option<PathBuf>, Option<PathBuf>)` — first token of `ExecStart=` (double-quoted form supported), and `CITADEL_HOME` from `Environment="CITADEL_HOME=…"`.
  - `pub enum ServiceState { Unsupported, NotInstalled, Installed { file: ServiceFile, loaded: bool } }`
  - `pub fn gather_service_state() -> ServiceState` — loaded = `launchctl list com.citadel.daemon` exits 0 (macOS) / `systemctl --user is-active citadel.service` prints `active` (Linux); a failed spawn counts as not loaded.
  - `pub fn service_checks(state: &ServiceState, cli_data_dir: &Path, cli_exe: &Path) -> Vec<Check>` — compare paths after `fs::canonicalize` with fallback to the raw path.

- [ ] **Step 1: Write failing tests:**
  - `plist_round_trips_through_generator`: `parse_launchd_plist(&generate_launchd_plist(Path::new("/a & b/citadel"), Path::new("/data")))` == `(Some("/a & b/citadel"), Some("/data"))`.
  - `unit_round_trips_through_generator`: same with `generate_systemd_unit(Path::new("/opt/my citadel/citadel"), Path::new("/data"))`.
  - `daemon_running_check_maps_each_status` (three asserts on status and hint).
  - `service_not_installed_is_info_without_fail`.
  - `service_installed_not_loaded_warns`.
  - `service_other_data_dir_warns`: detail contains `"service uses data dir /other; this CLI uses /data"`.
  - `service_other_binary_warns_with_install_hint`: hint `citadel daemon install`.
- [ ] **Step 2:** `cargo test doctor::running` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4:** `cargo test doctor::running` → PASS.
- [ ] **Step 5: Commit** `feat(doctor): check daemon state and OS service`

---

### Task 4: Running checks — processes and serve endpoint

**Files:**
- Modify: `src/doctor/running.rs`
- Modify: `src/doctor/version.rs` (add the serve version row)

**Interfaces:**
- Produces:
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum ProcessKind { Daemon, Serve }`; `pub struct CitadelProcess { pub pid: u32, pub kind: ProcessKind }`
  - `pub fn parse_ps_output(text: &str) -> Vec<CitadelProcess>` — input is `ps -axo pid=,command=`; a line counts only when argv[0]'s file name is exactly `citadel` and the remaining args contain `daemon start` (Daemon) or `serve` (Serve). Wrapper lines (e.g. `…/disclaimer --pgroup -- /…/citadel mcp`) and `citadel mcp` are ignored.
  - `pub fn gather_processes() -> Vec<CitadelProcess>` (runs `ps`; failure → empty vec)
  - `pub fn process_check(procs: &[CitadelProcess]) -> Check` — `"N daemon process(es), M serve process(es)"`; >1 daemon → warn `"may be different data dirs"`; daemon ≥1 and serve ≥1 → warn `"if they share a vault, both run scheduled Syncs"` with hint `stop one of them (citadel daemon stop, or stop the serve process)`.
  - `pub enum ServeProbe { NotAnswering, Answering { version: Option<String> } }`
  - `pub async fn probe_serve(url: &str) -> ServeProbe` — POST JSON-RPC `initialize` (`protocolVersion "2024-11-05"`, `clientInfo {name:"citadel-doctor"}`), same headers as `setup::answers_mcp_ping`, 1 s timeout; version from `result.serverInfo.version`.
  - `pub fn serve_check(url: &str, probe: &ServeProbe, http_entry_points_here: bool) -> Check` — answering → ok; not answering → info, or warn when an HTTP MCP entry uses this URL (hint `start it with make run or citadel serve, or re-run citadel setup --transport stdio`).
  - In `version.rs`: `pub fn serve_version_check(current: &str, probe: &ServeProbe) -> Option<Check>` — `None` unless answering; version differs → warn, hint `rebuild and restart the serve process or container (make build && make run)`.

- [ ] **Step 1: Write failing tests:**
  - `ps_parser_finds_daemon_and_serve_only`: input with `37556 /Users/x/.cargo/bin/citadel daemon start --foreground`, `16826 /Users/x/.cargo/bin/citadel mcp`, `16824 /App/disclaimer --pgroup -- /Users/x/.cargo/bin/citadel mcp`, `900 citadel serve --bind 0.0.0.0:4317`, `5 /usr/bin/vim citadel` → `[Daemon 37556, Serve 900]`.
  - `two_daemons_warn_may_differ`; `daemon_and_serve_warn_both_sync`; `one_daemon_is_ok`.
  - `probe_serve_reads_version_from_real_router` (`#[tokio::test]`): spawn the real MCP HTTP router the way `setup` tests do (`spawn_router` in `src/setup/mod.rs` tests) and assert `Answering{version: Some(env!("CARGO_PKG_VERSION"))}`.
  - `probe_serve_closed_port_is_not_answering`.
  - `serve_not_answering_warns_only_when_an_entry_uses_it`.
  - `serve_other_version_warns`.
- [ ] **Step 2:** `cargo test doctor::` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4:** `cargo test doctor::` → PASS.
- [ ] **Step 5: Commit** `feat(doctor): check citadel processes and the serve endpoint`

---

### Task 5: Setup checks — skill and MCP entries

**Files:**
- Create: `src/doctor/setup.rs`
- Modify: `src/setup/agents.rs` (make `skill_targets` and `mcp_targets` `pub(crate)`; no behavior change)

**Interfaces:**
- Consumes: `AgentKind`, `SetupScope`, `ActionKind`, `skill_targets`, `mcp_targets`; `crate::skill::ROUTER_SKILL_MD`; `ServeProbe`, `probe_serve` (Task 4).
- Produces:
  - `pub struct SetupLocation { pub agent: AgentKind, pub scope: SetupScope, pub skill_path: PathBuf, pub mcp_path: PathBuf, pub mcp_is_toml: bool }`
  - `pub fn setup_locations(cwd: &Path, home: Option<&Path>) -> Vec<SetupLocation>` — six entries (3 agents × 2 scopes; global ones only when `home` is `Some`). `skill_path` = the **last** skill target (the symlink where one exists, else the `SKILL.md`); `mcp_path` = the `MergeJsonMcp`/`MergeTomlMcp` target.
  - `pub enum SkillState { Missing, Present { path: PathBuf, link_target: Option<PathBuf>, outdated: bool }, Dangling { path: PathBuf, link_target: PathBuf } }`
  - `pub fn inspect_skill(path: &Path) -> SkillState` — for a directory symlink, read `<target>/SKILL.md`; `outdated` = content ≠ `ROUTER_SKILL_MD`.
  - `#[derive(Debug, Clone, PartialEq, Eq)] pub enum McpEntry { Stdio { command: String, args: Vec<String> }, Http { url: String } }`
  - `pub fn read_mcp_entry(path: &Path, toml: bool) -> std::result::Result<Option<McpEntry>, String>` — missing file or empty file or no `citadel` entry → `Ok(None)`; unparsable → `Err(parse message)`. JSON: `mcpServers.citadel` with `command`/`args` or `url`; TOML: `[mcp_servers.citadel]` same keys.
  - `pub enum StdioResolution { NotFound, SameAsDoctor, Other { path: PathBuf, version: Option<String> } }`
  - `pub fn resolve_stdio_command(command: &str, path_env: Option<&OsStr>, current_exe: &Path) -> StdioResolution` — absolute command used as-is, else search `PATH`; compare after `fs::canonicalize`; for `Other`, run `<path> --version` with a 2 s timeout and parse `citadel X.Y.Z`.
  - `pub fn setup_checks(loc: &SetupLocation, skill: &SkillState, mcp: &Result<Option<McpEntry>, String>, stdio: Option<&StdioResolution>, http: Option<&ServeProbe>, current: &str) -> Vec<Check>` — nothing configured → one info row `"<Agent> <scope> not configured"`; dangling → fail (hint `citadel setup --only skill --force`); outdated → warn (same hint); parse error → fail (hint `fix or remove the citadel entry in <path>`); `NotFound` → fail (hint `install citadel on PATH or re-run citadel setup`); `Other` with different version → warn; HTTP not answering → warn. Detail strings follow the spec's report sketch (`<Agent>  <scope>  skill  <path> → <target>`, `<Agent>  <scope>  MCP  <path>  (stdio: citadel mcp)`).
  - `pub fn local_scope_note(cwd: &Path) -> Check` — info row `"local scope checked in <cwd>"`.

- [ ] **Step 1: Write failing tests** (all in tempdirs):
  - `locations_cover_three_agents_two_scopes`: 6 locations; Claude global `skill_path` ends with `.claude/skills/citadel`, `mcp_path` ends with `.claude.json`; Codex `mcp_is_toml`.
  - `skill_symlink_to_current_skill_is_present_not_outdated`; `skill_with_old_content_is_outdated`; `dangling_symlink_is_dangling`.
  - `mcp_entry_absent_in_valid_config_is_none` (Review Focus 1): `.claude.json` = `{"mcpServers":{"other":{"command":"x"}}}` → `Ok(None)`; empty file → `Ok(None)`.
  - `mcp_stdio_json_and_http_json_parse`; `mcp_toml_parse`; `mcp_broken_json_is_err`.
  - `stdio_command_symlinked_to_current_exe_is_same` (Review Focus 2): tempdir `bin/citadel` symlink → a temp file used as `current_exe`; `PATH` = that `bin` → `SameAsDoctor`.
  - `stdio_command_missing_is_not_found`.
  - `unconfigured_location_is_single_info_row`; `broken_config_fails`; `missing_command_fails`; `outdated_skill_warns`.
- [ ] **Step 2:** `cargo test doctor::setup` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4:** `cargo test doctor::setup && cargo test setup` → PASS (existing setup tests untouched).
- [ ] **Step 5: Commit** `feat(doctor): report skill and MCP setup per agent`

---

### Task 6: `run_doctor`, CLI wiring and docs

**Files:**
- Modify: `src/doctor/mod.rs` (add `run_doctor`)
- Modify: `src/cli/mod.rs` (`Commands::Doctor`, dispatch, exclude from update notice)
- Create: `tests/doctor.rs`
- Modify: `Docs/SPEC.md` (§1 Data & Config Paths: add `daemon.json`; §6 command list: `doctor [--json]` plus a short `### Health Report (citadel doctor)` subsection summarizing the checks and exit code), `README.md` (Core CLI Commands table row), `Docs/AtomicTasks/V8/README.md` + `Docs/AtomicTasks/V8/01-doctor-command.md` (follow the V7 format; link the design doc)

**Interfaces:**
- Consumes: everything above.
- Produces:
  - `pub struct DoctorOpts { pub cwd: PathBuf, pub home: Option<PathBuf>, pub current_exe: PathBuf, pub releases_api_url: String, pub serve_url: String, pub live_release_check: bool, pub in_container: bool }`
  - `pub async fn run_doctor(config: &VaultConfig, opts: &DoctorOpts) -> Report` — order: Version (CLI, daemon, serve), Running (daemon, service, processes, serve), Setup (local scope note, then every location), Data (`data dir <path>` info, plus `"(not created yet)"` when absent). When `in_container`: skip daemon/service/process rows and add info `"running in a container: serve is the daemon"`. Each distinct HTTP URL is probed once.
  - CLI: `Doctor { #[arg(long)] json: bool }` with doc comment `Check whether citadel is up to date, running and set up.`; dispatch builds `DoctorOpts` (`releases_api_url = DEFAULT_RELEASES_API_URL`, `serve_url = setup::DEFAULT_HTTP_MCP_URL`, `live_release_check = !config.no_update_check`, `in_container = update::is_running_in_container()`), runs it via `block_on`, renders text or JSON to `out`, then `anyhow::bail!("{n} doctor check(s) failed")` when `fail_count() > 0`. Add `Commands::Doctor { .. }` to the `should_check_update` exclusion list.

- [ ] **Step 1: Write failing integration tests** in `tests/doctor.rs` (run `env!("CARGO_BIN_EXE_citadel")` with `HOME` = temp home, `CITADEL_HOME` = `<tmp>/data` (not created), `CITADEL_NO_UPDATE_CHECK=1`, cwd = temp project dir):
  - `doctor_on_fresh_home_creates_nothing` (Review Focus 3): `doctor --json` exits 0; stdout parses as JSON with a non-empty `checks` array; `<tmp>/data` still does not exist; no `.mcp.json` created in cwd.
  - `doctor_fails_on_broken_mcp_config`: cwd contains `.mcp.json` = `{not json`; `doctor` exits 1; stdout contains `✗`; stderr contains `doctor check(s) failed`.
  - `doctor_text_has_all_sections`: stdout contains `Version`, `Running`, `Setup`, `Data`.
  - `doctor_prints_no_update_notice`: stderr does not contain `Update available`.
- [ ] **Step 2:** `cargo test --test doctor` → FAIL.
- [ ] **Step 3: Implement** `run_doctor` and the CLI wiring.
- [ ] **Step 4:** `cargo test --test doctor` → PASS.
- [ ] **Step 5: Update docs** listed under Files.
- [ ] **Step 6: Full gate:**
  ```bash
  cargo fmt --check
  cargo clippy --all-targets -- -D warnings
  cargo test
  cargo build --no-default-features && cargo test --no-default-features
  ```
  Expected: all green (the known flaky dropped-sync test may fail ~2/5 on main; re-run before debugging).
- [ ] **Step 7: Manual check:** `touch src/main.rs && cargo build && ./target/debug/citadel doctor` against the real machine; paste the output in the HumanAudit handoff.
- [ ] **Step 8: Commit** `feat(cli): add citadel doctor command`
