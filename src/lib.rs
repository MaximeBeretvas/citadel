pub mod cli;
pub mod config;
pub mod daemon;
pub mod db;
pub mod error;
pub mod ingest;
pub mod mcp;
pub mod skill;
pub mod ui;

use std::io::IsTerminal;

use tracing_subscriber::EnvFilter;

/// Initialize structured tracing strictly on `stderr` so `stdout` remains clean for MCP `stdio`.
pub fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_tracing_is_idempotent() {
        init_tracing();
        init_tracing();
    }
}
