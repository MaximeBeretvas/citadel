//! Report model: rows grouped by section, rendered as text or JSON.

use std::io::Write;

use serde::Serialize;

/// Outcome of a single check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Ok,
    Info,
    Warn,
    Fail,
}

impl CheckStatus {
    fn marker(self) -> &'static str {
        match self {
            Self::Ok => "✓",
            Self::Info => "·",
            Self::Warn => "!",
            Self::Fail => "✗",
        }
    }
}

/// Report section a check belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Section {
    Version,
    Running,
    Setup,
    Data,
}

impl Section {
    const ALL: [Section; 4] = [Self::Version, Self::Running, Self::Setup, Self::Data];

    fn title(self) -> &'static str {
        match self {
            Self::Version => "Version",
            Self::Running => "Running",
            Self::Setup => "Setup",
            Self::Data => "Data",
        }
    }
}

/// One diagnostic row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub section: Section,
    pub name: String,
    pub status: CheckStatus,
    pub detail: String,
    pub hint: Option<String>,
}

impl Check {
    fn new(status: CheckStatus, section: Section, name: &str, detail: impl Into<String>) -> Self {
        Self {
            section,
            name: name.to_string(),
            status,
            detail: detail.into(),
            hint: None,
        }
    }

    pub fn ok(section: Section, name: &str, detail: impl Into<String>) -> Self {
        Self::new(CheckStatus::Ok, section, name, detail)
    }

    pub fn info(section: Section, name: &str, detail: impl Into<String>) -> Self {
        Self::new(CheckStatus::Info, section, name, detail)
    }

    pub fn warn(section: Section, name: &str, detail: impl Into<String>) -> Self {
        Self::new(CheckStatus::Warn, section, name, detail)
    }

    pub fn fail(section: Section, name: &str, detail: impl Into<String>) -> Self {
        Self::new(CheckStatus::Fail, section, name, detail)
    }

    /// Attach a suggested next step.
    pub fn with_hint(mut self, hint: &str) -> Self {
        self.hint = Some(hint.to_string());
        self
    }
}

/// All checks of one doctor run.
#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub checks: Vec<Check>,
}

impl Report {
    pub fn fail_count(&self) -> usize {
        self.checks
            .iter()
            .filter(|check| check.status == CheckStatus::Fail)
            .count()
    }

    pub fn render_text<W: Write>(&self, out: &mut W) -> std::io::Result<()> {
        writeln!(out, "citadel doctor")?;
        for section in Section::ALL {
            let mut rows = self
                .checks
                .iter()
                .filter(|c| c.section == section)
                .peekable();
            if rows.peek().is_none() {
                continue;
            }
            writeln!(out, "  {}", section.title())?;
            for check in rows {
                writeln!(out, "    {} {}", check.status.marker(), check.detail)?;
                if let Some(hint) = &check.hint {
                    writeln!(out, "      → {hint}")?;
                }
            }
        }
        Ok(())
    }

    pub fn render_json<W: Write>(&self, out: &mut W) -> std::io::Result<()> {
        serde_json::to_writer(&mut *out, self).map_err(std::io::Error::other)?;
        writeln!(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_groups_rows_by_section_with_markers_and_hints() {
        let report = Report {
            checks: vec![
                Check::warn(Section::Running, "daemon", "daemon stale")
                    .with_hint("citadel daemon start"),
                Check::ok(Section::Version, "cli", "citadel 0.3.0"),
            ],
        };
        let mut out = Vec::new();
        report.render_text(&mut out).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "citadel doctor\n  Version\n    ✓ citadel 0.3.0\n  Running\n    ! daemon stale\n      → citadel daemon start\n"
        );
    }

    #[test]
    fn json_uses_lowercase_status_and_section() {
        let report = Report {
            checks: vec![Check::warn(Section::Running, "daemon", "stale")],
        };
        let mut out = Vec::new();
        report.render_json(&mut out).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(parsed["checks"][0]["status"], "warn");
        assert_eq!(parsed["checks"][0]["section"], "running");
        assert!(parsed["checks"][0]["hint"].is_null());
    }

    #[test]
    fn fail_count_counts_only_fail() {
        let report = Report {
            checks: vec![
                Check::fail(Section::Data, "a", "x"),
                Check::warn(Section::Data, "b", "y"),
                Check::fail(Section::Setup, "c", "z"),
                Check::ok(Section::Setup, "d", "w"),
            ],
        };
        assert_eq!(report.fail_count(), 2);
    }
}
