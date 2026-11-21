pub mod service;

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use notify::RecursiveMode;
use notify_debouncer_mini::{new_debouncer, DebounceEventResult};
use rusqlite::Connection;
use tokio::sync::{mpsc, Mutex, Semaphore};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::config::VaultConfig;
use crate::db::open_rw_db;
use crate::error::{Result, VaultError};
use crate::ingest::{local::canonicalize_local_dir, map_source_record, SourceRecord};
use crate::sources::SourceLifecycle;

pub use service::{
    generate_launchd_plist, generate_systemd_unit, install_service, uninstall_service,
    LAUNCHD_LABEL, SYSTEMD_SERVICE_NAME,
};

/// Default web scheduler tick interval (60 seconds).
pub const DEFAULT_SCHEDULER_TICK: Duration = Duration::from_secs(60);
/// Default maximum concurrent web source syncs.
pub const DEFAULT_MAX_CONCURRENT_SYNCS: usize = 2;
/// Default maximum random jitter (in seconds) before starting a scheduled web sync.
pub const DEFAULT_MAX_JITTER_SECS: u64 = 30;
/// Default file watcher debounce window (2 seconds).
pub const DEFAULT_WATCHER_DEBOUNCE: Duration = Duration::from_secs(2);
/// Interval at which the watcher refreshes the list of registered local sources.
const WATCHER_SOURCE_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// Current liveness state of the background daemon for a given vault directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonStatus {
    /// A daemon holds an exclusive lock on `daemon.pid` with the recorded PID.
    Running(u32),
    /// No `daemon.pid` file exists.
    Stopped,
    /// `daemon.pid` exists on disk, but no live process holds its advisory lock.
    Stale,
}

/// Exclusive advisory file lock guarding `daemon.pid` for single-instance enforcement.
#[derive(Debug)]
pub struct PidLock {
    path: PathBuf,
    _file: File,
}

impl PidLock {
    /// Acquire an exclusive advisory lock on `path` and record the current process PID.
    pub fn acquire(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;

        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                let existing_pid = read_pid_from_path(path);
                let detail = match existing_pid {
                    Some(pid) => format!(
                        "daemon is already running (pid {pid}, lockfile {})",
                        path.display()
                    ),
                    None => format!("daemon is already running (lockfile {})", path.display()),
                };
                return Err(VaultError::AlreadyRunning(detail));
            }
            Err(TryLockError::Error(err)) => return Err(VaultError::Io(err)),
        }

        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        writeln!(file, "{}", std::process::id())?;
        file.flush()?;
        let _ = file.sync_data();

        Ok(Self {
            path: path.to_path_buf(),
            _file: file,
        })
    }

    /// Path of the locked PID file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PidLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn read_pid_from_file(file: &mut File) -> Option<u32> {
    let mut contents = String::new();
    let _ = file.seek(SeekFrom::Start(0));
    file.read_to_string(&mut contents).ok()?;
    contents.trim().parse::<u32>().ok()
}

fn read_pid_from_path(path: &Path) -> Option<u32> {
    let contents = fs::read_to_string(path).ok()?;
    contents.trim().parse::<u32>().ok()
}

/// Inspect the status of the background daemon for `config`.
pub fn daemon_status(config: &VaultConfig) -> Result<DaemonStatus> {
    let pid_path = config.pid_path();
    if !pid_path.exists() {
        return Ok(DaemonStatus::Stopped);
    }

    let mut file = match OpenOptions::new().read(true).write(true).open(&pid_path) {
        Ok(f) => f,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DaemonStatus::Stopped);
        }
        Err(err) => return Err(VaultError::Io(err)),
    };

    match file.try_lock() {
        Ok(()) => Ok(DaemonStatus::Stale),
        Err(TryLockError::WouldBlock) => {
            let pid = read_pid_from_file(&mut file).unwrap_or(0);
            Ok(DaemonStatus::Running(pid))
        }
        Err(TryLockError::Error(err)) => Err(VaultError::Io(err)),
    }
}

/// Start the daemon either in the foreground (blocking until `SIGINT`/`SIGTERM`) or detached.
///
/// Returns `Ok(Some(child_pid))` when detached, or `Ok(None)` after clean foreground shutdown.
pub fn start_daemon(config: &VaultConfig, foreground: bool) -> Result<Option<u32>> {
    config.ensure_data_dir()?;

    if foreground {
        let _lock = PidLock::acquire(&config.pid_path())?;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|err| VaultError::Other(format!("failed to build tokio runtime: {err}")))?;
        rt.block_on(run_daemon_foreground(config.clone()))?;
        return Ok(None);
    }

    if let DaemonStatus::Running(pid) = daemon_status(config)? {
        return Err(VaultError::AlreadyRunning(format!(
            "daemon is already running (pid {pid})"
        )));
    }

    let exe = std::env::current_exe()?;
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(config.log_path())?;
    let log_err = log_file.try_clone()?;

    let child = Command::new(exe)
        .arg("--data-dir")
        .arg(&config.data_dir)
        .arg("daemon")
        .arg("start")
        .arg("--foreground")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_err))
        .spawn()?;

    Ok(Some(child.id()))
}

/// Stop a running daemon cleanly via `SIGTERM`, or clean up a stale `daemon.pid`.
///
/// Returns `Ok(Some(pid))` if a running daemon was stopped, or `Ok(None)` if none was running.
pub fn stop_daemon(config: &VaultConfig) -> Result<Option<u32>> {
    match daemon_status(config)? {
        DaemonStatus::Stopped => Ok(None),
        DaemonStatus::Stale => {
            let _ = fs::remove_file(config.pid_path());
            Ok(None)
        }
        DaemonStatus::Running(pid) => {
            if pid == 0 {
                return Err(VaultError::Other(
                    "daemon lock is held, but daemon.pid does not contain a valid PID".into(),
                ));
            }

            send_sigterm(pid)?;

            for _ in 0..50 {
                if !matches!(daemon_status(config)?, DaemonStatus::Running(_)) {
                    return Ok(Some(pid));
                }
                std::thread::sleep(Duration::from_millis(100));
            }

            Err(VaultError::Other(format!(
                "sent SIGTERM to daemon (pid {pid}), but it did not exit within 5 seconds"
            )))
        }
    }
}

#[cfg(unix)]
fn send_sigterm(pid: u32) -> Result<()> {
    let status = Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status()?;
    if !status.success() {
        return Err(VaultError::Other(format!(
            "failed to send SIGTERM to pid {pid} (exit status {status})"
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn send_sigterm(pid: u32) -> Result<()> {
    Err(VaultError::Other(format!(
        "stopping daemon pid {pid} via signal is not supported on this platform"
    )))
}

/// Run the scheduler and local folder watcher concurrently until `SIGINT` or `SIGTERM`.
pub async fn run_daemon_foreground(config: VaultConfig) -> Result<()> {
    let cancel = CancellationToken::new();
    let signal_cancel = cancel.clone();

    let signal_task = tokio::spawn(async move {
        wait_for_shutdown_signal().await;
        tracing::info!("shutdown signal received; stopping daemon tasks");
        signal_cancel.cancel();
    });

    let scheduler_cfg = config.clone();
    let scheduler_cancel = cancel.clone();
    let watcher_cfg = config;
    let watcher_cancel = cancel.clone();

    let (sched_res, watch_res) = tokio::join!(
        run_scheduler(scheduler_cfg, scheduler_cancel),
        run_watcher(watcher_cfg, watcher_cancel),
    );

    cancel.cancel();
    signal_task.abort();

    sched_res?;
    watch_res?;
    Ok(())
}

pub(crate) async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut sigterm) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = sigterm.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Select all `web` sources that are due for refresh at the current wall-clock time.
pub fn select_due_web_sources(conn: &Connection) -> Result<Vec<SourceRecord>> {
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    select_due_web_sources_at(conn, now_secs)
}

/// Select all `web` sources that are due for refresh at `now_epoch_secs` (UTC Unix seconds).
pub fn select_due_web_sources_at(
    conn: &Connection,
    now_epoch_secs: i64,
) -> Result<Vec<SourceRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, kind, origin, tags, include_patterns, exclude_patterns,
                max_pages, max_depth, render_js, refresh_interval_secs,
                status, last_synced_at, last_error, created_at
         FROM sources
         WHERE kind = 'web'
           AND status != 'syncing'
           AND refresh_interval_secs > 0
           AND (
               last_synced_at IS NULL
               OR unixepoch(last_synced_at) + refresh_interval_secs <= ?1
           )
         ORDER BY COALESCE(unixepoch(last_synced_at), 0) ASC, id ASC",
    )?;
    let rows = stmt.query_map([now_epoch_secs], map_source_record)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Run the background web refresh scheduler with default cadence (60s tick, concurrency 2, 0–30s jitter).
pub async fn run_scheduler(config: VaultConfig, cancel: CancellationToken) -> Result<()> {
    run_scheduler_with_options(
        config,
        DEFAULT_SCHEDULER_TICK,
        DEFAULT_MAX_CONCURRENT_SYNCS,
        DEFAULT_MAX_JITTER_SECS,
        cancel,
    )
    .await
}

/// Run the web refresh scheduler with configurable tick interval, concurrency bound, and jitter.
pub async fn run_scheduler_with_options(
    config: VaultConfig,
    tick_interval: Duration,
    max_concurrency: usize,
    max_jitter_secs: u64,
    cancel: CancellationToken,
) -> Result<()> {
    let semaphore = Arc::new(Semaphore::new(max_concurrency.max(1)));
    let in_flight: Arc<Mutex<HashSet<i64>>> = Arc::new(Mutex::new(HashSet::new()));
    let mut tasks: JoinSet<()> = JoinSet::new();

    let mut ticker = tokio::time::interval(tick_interval.max(Duration::from_millis(10)));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                break;
            }
            _ = ticker.tick() => {
                // Reap any finished sync tasks to keep memory bounded.
                while tasks.try_join_next().is_some() {}

                let due_sources = match load_due_web_sources(&config) {
                    Ok(sources) => sources,
                    Err(err) => {
                        tracing::warn!(error = %err, "scheduler failed to query due web sources");
                        continue;
                    }
                };

                for source in due_sources {
                    if cancel.is_cancelled() {
                        break;
                    }
                    {
                        let mut active = in_flight.lock().await;
                        if !active.insert(source.id) {
                            continue;
                        }
                    }

                    let sem = Arc::clone(&semaphore);
                    let active_set = Arc::clone(&in_flight);
                    let lifecycle = SourceLifecycle::new(config.clone());
                    let task_cancel = cancel.clone();

                    tasks.spawn(async move {
                        let permit = tokio::select! {
                            _ = task_cancel.cancelled() => None,
                            acquired = sem.acquire_owned() => acquired.ok(),
                        };

                        if let Some(_permit) = permit {
                            let jitter = compute_jitter(max_jitter_secs, source.id);
                            let proceed = if jitter.is_zero() {
                                !task_cancel.is_cancelled()
                            } else {
                                tokio::select! {
                                    _ = task_cancel.cancelled() => false,
                                    _ = tokio::time::sleep(jitter) => true,
                                }
                            };

                            if proceed {
                                tracing::info!(source = %source.name, "starting scheduled web sync");
                                match lifecycle.sync(&source.name, false).await {
                                    Ok(summary) => {
                                        tracing::info!(
                                            source = %summary.source_name,
                                            added = summary.pages_added,
                                            updated = summary.pages_updated,
                                            unchanged = summary.pages_unchanged,
                                            stale = summary.pages_stale,
                                            failed = summary.pages_failed,
                                            "scheduled web sync complete"
                                        );
                                    }
                                    Err(err) => {
                                        tracing::warn!(
                                            source = %source.name,
                                            error = %err,
                                            "scheduled web sync failed"
                                        );
                                    }
                                }
                            }
                        }

                        let mut active = active_set.lock().await;
                        active.remove(&source.id);
                    });
                }
            }
        }
    }

    // Drain in-flight tasks so any active SQLite transactions finish cleanly.
    while tasks.join_next().await.is_some() {}
    Ok(())
}

fn load_due_web_sources(config: &VaultConfig) -> Result<Vec<SourceRecord>> {
    let db_path = config.db_path();
    if !db_path.exists() {
        return Ok(Vec::new());
    }
    let conn = open_rw_db(&db_path)?;
    select_due_web_sources(&conn)
}

fn compute_jitter(max_jitter_secs: u64, source_id: i64) -> Duration {
    if max_jitter_secs == 0 {
        return Duration::ZERO;
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::from(d.subsec_nanos()))
        .unwrap_or(0);
    let mix = nanos ^ (source_id.unsigned_abs().wrapping_mul(2_654_435_761));
    Duration::from_secs(mix % (max_jitter_secs + 1))
}

/// Run the local Markdown folder watcher with the default 2-second debounce window.
pub async fn run_watcher(config: VaultConfig, cancel: CancellationToken) -> Result<()> {
    run_watcher_with_debounce(config, DEFAULT_WATCHER_DEBOUNCE, cancel).await
}

/// Run the local Markdown folder watcher with a custom debounce duration.
pub async fn run_watcher_with_debounce(
    config: VaultConfig,
    debounce: Duration,
    cancel: CancellationToken,
) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<DebounceEventResult>();
    let mut debouncer = new_debouncer(debounce, move |res: DebounceEventResult| {
        let _ = tx.send(res);
    })
    .map_err(|err| VaultError::Other(format!("failed to initialize file watcher: {err}")))?;

    let lifecycle = SourceLifecycle::new(config.clone());
    let mut watched_roots: HashMap<PathBuf, SourceRecord> = HashMap::new();
    refresh_watched_local_sources(&config, &mut debouncer, &mut watched_roots);

    let mut refresh_ticker = tokio::time::interval(WATCHER_SOURCE_REFRESH_INTERVAL);
    refresh_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Consume the immediate initial tick since we already loaded sources above.
    refresh_ticker.tick().await;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                break;
            }
            _ = refresh_ticker.tick() => {
                refresh_watched_local_sources(&config, &mut debouncer, &mut watched_roots);
            }
            maybe_res = rx.recv() => {
                let Some(event_res) = maybe_res else {
                    break;
                };
                match event_res {
                    Ok(events) => {
                        let affected_ids = match_affected_sources(&events, &watched_roots);
                        if affected_ids.is_empty() {
                            continue;
                        }
                        for source_id in affected_ids {
                            let Some(source) = watched_roots
                                .values()
                                .find(|s| s.id == source_id)
                                .cloned()
                            else {
                                continue;
                            };
                            match lifecycle.sync(&source.name, false).await {
                                Ok(summary) => {
                                    tracing::info!(
                                        source = %summary.source_name,
                                        added = summary.pages_added,
                                        updated = summary.pages_updated,
                                        unchanged = summary.pages_unchanged,
                                        stale = summary.pages_stale,
                                        "watcher re-indexed local source"
                                    );
                                }
                                Err(err) => {
                                    tracing::warn!(error = %err, "watcher local sync failed");
                                }
                            }
                        }
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "file watcher reported error");
                    }
                }
            }
        }
    }

    drop(debouncer);
    Ok(())
}

fn load_local_sources(config: &VaultConfig) -> Result<Vec<SourceRecord>> {
    let db_path = config.db_path();
    if !db_path.exists() {
        return Ok(Vec::new());
    }
    let conn = open_rw_db(&db_path)?;
    let mut stmt = conn.prepare(
        "SELECT id, name, kind, origin, tags, include_patterns, exclude_patterns,
                max_pages, max_depth, render_js, refresh_interval_secs,
                status, last_synced_at, last_error, created_at
         FROM sources
         WHERE kind = 'local'
         ORDER BY id ASC",
    )?;
    let rows = stmt.query_map([], map_source_record)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn refresh_watched_local_sources(
    config: &VaultConfig,
    debouncer: &mut notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>,
    watched_roots: &mut HashMap<PathBuf, SourceRecord>,
) {
    let sources = match load_local_sources(config) {
        Ok(list) => list,
        Err(err) => {
            tracing::warn!(error = %err, "watcher failed to load local sources");
            return;
        }
    };

    let mut desired: HashMap<PathBuf, SourceRecord> = HashMap::new();
    for source in sources {
        match canonicalize_local_dir(Path::new(&source.origin)) {
            Ok(canon) => {
                desired.insert(canon, source);
            }
            Err(err) => {
                tracing::warn!(
                    source = %source.name,
                    origin = %source.origin,
                    error = %err,
                    "skipping unavailable local source directory in watcher"
                );
            }
        }
    }

    // Unwatch directories that are no longer registered.
    let stale_roots: Vec<PathBuf> = watched_roots
        .keys()
        .filter(|path| !desired.contains_key(*path))
        .cloned()
        .collect();
    for path in stale_roots {
        let _ = debouncer.watcher().unwatch(&path);
        watched_roots.remove(&path);
    }

    // Watch newly registered directories and keep SourceRecord metadata fresh.
    for (path, source) in desired {
        if !watched_roots.contains_key(&path) {
            if let Err(err) = debouncer.watcher().watch(&path, RecursiveMode::Recursive) {
                tracing::warn!(
                    source = %source.name,
                    path = %path.display(),
                    error = %err,
                    "failed to register directory with file watcher"
                );
                continue;
            }
            tracing::debug!(source = %source.name, path = %path.display(), "watching local source");
        }
        watched_roots.insert(path, source);
    }
}

fn match_affected_sources(
    events: &[notify_debouncer_mini::DebouncedEvent],
    watched_roots: &HashMap<PathBuf, SourceRecord>,
) -> Vec<i64> {
    let mut affected: HashSet<i64> = HashSet::new();

    for event in events {
        let candidate = canonicalize_event_path(&event.path);
        for (root, source) in watched_roots {
            let rel = match candidate.strip_prefix(root) {
                Ok(rel) => rel,
                Err(_) => match event.path.strip_prefix(root) {
                    Ok(rel) => rel,
                    Err(_) => continue,
                },
            };

            if is_ignored_relative_path(rel) || !is_markdown_path(rel) {
                continue;
            }

            affected.insert(source.id);
        }
    }

    let mut ids: Vec<i64> = affected.into_iter().collect();
    ids.sort_unstable();
    ids
}

fn canonicalize_event_path(path: &Path) -> PathBuf {
    if let Ok(canon) = fs::canonicalize(path) {
        return canon;
    }
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
        if let Ok(canon_parent) = fs::canonicalize(parent) {
            return canon_parent.join(name);
        }
    }
    path.to_path_buf()
}

fn is_ignored_relative_path(rel: &Path) -> bool {
    for component in rel.components() {
        if let Component::Normal(part) = component {
            let s = part.to_string_lossy();
            if s.starts_with('.') || s == "node_modules" {
                return true;
            }
        }
    }
    false
}

fn is_markdown_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_rw_db;
    use crate::error::VaultError;
    use crate::sources::{AddSourceParams, SourceLifecycle};
    use tempfile::tempdir;

    #[test]
    fn selects_only_due_web_sources() {
        let dir = tempdir().expect("tempdir");
        let conn = open_rw_db(&dir.path().join("vault.db")).expect("open_rw_db");

        // Reference time: 2026-11-21T12:00:00Z
        let now_epoch: i64 = conn
            .query_row("SELECT unixepoch('2026-11-21T12:00:00Z')", [], |r| r.get(0))
            .expect("unixepoch");

        // 1. Overdue web source (last synced 2h ago, interval 1h) -> DUE
        conn.execute(
            "INSERT INTO sources (name, kind, origin, refresh_interval_secs, last_synced_at)
             VALUES ('overdue-web', 'web', 'https://overdue.example.com', 3600, '2026-11-21T10:00:00Z')",
            [],
        )
        .expect("insert overdue web");

        // 2. Never-synced web source with interval > 0 -> DUE
        conn.execute(
            "INSERT INTO sources (name, kind, origin, refresh_interval_secs, last_synced_at)
             VALUES ('never-synced-web', 'web', 'https://never.example.com', 86400, NULL)",
            [],
        )
        .expect("insert never-synced web");

        // 3. Recently synced web source (last synced 10m ago, interval 1h) -> SKIP
        conn.execute(
            "INSERT INTO sources (name, kind, origin, refresh_interval_secs, last_synced_at)
             VALUES ('recent-web', 'web', 'https://recent.example.com', 3600, '2026-11-21T11:50:00Z')",
            [],
        )
        .expect("insert recent web");

        // 4. Disabled web source (refresh_interval_secs = 0) -> SKIP
        conn.execute(
            "INSERT INTO sources (name, kind, origin, refresh_interval_secs, last_synced_at)
             VALUES ('disabled-web', 'web', 'https://disabled.example.com', 0, NULL)",
            [],
        )
        .expect("insert disabled web");

        // 5. Local source (even if overdue / never synced) -> SKIP
        conn.execute(
            "INSERT INTO sources (name, kind, origin, refresh_interval_secs, last_synced_at)
             VALUES ('local-docs', 'local', '/tmp/local-docs', 3600, NULL)",
            [],
        )
        .expect("insert local");

        // 6. Currently syncing web source (even if last_synced_at IS NULL) -> SKIP
        conn.execute(
            "INSERT INTO sources (name, kind, origin, refresh_interval_secs, status, last_synced_at)
             VALUES ('syncing-web', 'web', 'https://syncing.example.com', 86400, 'syncing', NULL)",
            [],
        )
        .expect("insert syncing web");

        let due = select_due_web_sources_at(&conn, now_epoch).expect("select_due_web_sources_at");
        let mut names: Vec<String> = due.into_iter().map(|s| s.name).collect();
        names.sort();
        assert_eq!(names, vec!["never-synced-web", "overdue-web"]);
    }

    #[test]
    fn pid_lock_prevents_second_instance() {
        let dir = tempdir().expect("tempdir");
        let config = VaultConfig::new(dir.path().join("nested"));
        let pid_path = config.pid_path();

        assert_eq!(
            daemon_status(&config).expect("stopped status"),
            DaemonStatus::Stopped
        );

        let lock = PidLock::acquire(&pid_path).expect("first lock should succeed");
        assert!(pid_path.exists());
        let recorded = std::fs::read_to_string(&pid_path).expect("read pid file");
        assert_eq!(recorded.trim(), std::process::id().to_string());
        assert_eq!(
            daemon_status(&config).expect("running status"),
            DaemonStatus::Running(std::process::id())
        );

        let err = PidLock::acquire(&pid_path).expect_err("second lock must fail");
        assert!(
            matches!(err, VaultError::AlreadyRunning(ref msg) if msg.contains(&std::process::id().to_string())),
            "expected AlreadyRunning mentioning PID, got: {err:?}"
        );

        drop(lock);
        assert!(!pid_path.exists(), "daemon.pid should be removed on drop");
        assert_eq!(
            daemon_status(&config).expect("stopped after drop"),
            DaemonStatus::Stopped
        );

        // Write a stale unlocked pid file and verify DaemonStatus::Stale
        std::fs::write(&pid_path, "999999\n").expect("write stale pid");
        assert_eq!(
            daemon_status(&config).expect("stale status"),
            DaemonStatus::Stale
        );

        let reacquired = PidLock::acquire(&pid_path).expect("lock should succeed over stale file");
        drop(reacquired);
    }

    #[tokio::test]
    async fn local_watcher_reindexes_modified_markdown_file() {
        let vault_dir = tempdir().expect("vault tempdir");
        let docs_dir = tempdir().expect("docs tempdir");
        let md_path = docs_dir.path().join("guide.md");
        std::fs::write(&md_path, "# Guide\n\nInitial version.\n").expect("write initial md");

        let config = VaultConfig::new(vault_dir.path());
        SourceLifecycle::new(config.clone())
            .add(
                AddSourceParams {
                    origin: docs_dir.path().to_string_lossy().into_owned(),
                    name: Some("watched-docs".to_string()),
                    ..Default::default()
                },
                true,
            )
            .await
            .expect("add should succeed");

        let conn = open_rw_db(&config.db_path()).expect("open_rw_db");
        let initial_hash: String = conn
            .query_row(
                "SELECT content_hash FROM pages WHERE uri = 'guide.md'",
                [],
                |r| r.get(0),
            )
            .expect("initial hash");
        drop(conn);

        let cancel = CancellationToken::new();
        let watcher_handle = tokio::spawn(run_watcher_with_debounce(
            config.clone(),
            Duration::from_millis(150),
            cancel.clone(),
        ));

        // Give watcher a brief moment to register the directory.
        tokio::time::sleep(Duration::from_millis(200)).await;

        std::fs::write(
            &md_path,
            "# Guide\n\nUpdated version with new watcher content.\n",
        )
        .expect("update md");

        // Poll up to 4 seconds for the debounced re-index to complete.
        let mut updated_hash = initial_hash.clone();
        let mut revision_count: i64 = 0;
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let conn = open_rw_db(&config.db_path()).expect("open_rw_db");
            updated_hash = conn
                .query_row(
                    "SELECT content_hash FROM pages WHERE uri = 'guide.md'",
                    [],
                    |r| r.get(0),
                )
                .expect("query updated hash");
            revision_count = conn
                .query_row("SELECT COUNT(*) FROM page_revisions", [], |r| r.get(0))
                .expect("query revisions");
            if updated_hash != initial_hash && revision_count > 0 {
                break;
            }
        }

        cancel.cancel();
        watcher_handle
            .await
            .expect("watcher task join")
            .expect("watcher clean shutdown");

        assert_ne!(
            updated_hash, initial_hash,
            "content_hash should change after file edit"
        );
        assert_eq!(
            revision_count, 1,
            "page_revisions should record the change diff"
        );
    }
}
