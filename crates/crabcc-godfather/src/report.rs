//! Crash report assembly + GitHub-issue surfacing.
//!
//! Two surfaces:
//!
//!   * [`build_report`] returns a markdown string the user can copy
//!     into a bug template. Fully read-only — every input comes from
//!     `_crab_*` rows the user already opted into via telemetry.
//!   * [`open_gh_issue`] shells out to `gh issue create` with the
//!     report pre-filled and a sensible `--title`. Best-effort:
//!     missing `gh` / network errors return a typed error so the
//!     dashboard can render "copy to clipboard" as the fallback.

use anyhow::{anyhow, Context, Result};
use std::fmt::Write as _;
use std::process::Command;

use crate::event::{self, Severity};
use crate::godfather::Godfather;
use crate::session;

/// Build a single markdown blob covering: install fingerprint, host
/// info, the failing session, the most recent N events at warn+
/// severity, the resource-sample summary, and the trailing log.
pub fn build_report(godfather: &Godfather, crash_id: i64) -> Result<String> {
    let conn = godfather.conn();

    let crash = conn
        .query_row(
            "SELECT session_id, ts, exit_code, exit_signal, log_tail
         FROM _crab_crash WHERE id = ?1",
            rusqlite::params![crash_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i32>>(2)?,
                    row.get::<_, Option<i32>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .with_context(|| format!("crash {crash_id} not found"))?;
    let (session_id, ts, exit_code, exit_signal, log_tail) = crash;

    let session = session::get(conn, &session_id)?
        .ok_or_else(|| anyhow!("session {session_id} not found"))?;
    let host = godfather.host_info()?;
    let install_version = godfather.metadata("install_version")?;
    let install_source = godfather.metadata("install_source")?;
    let install_time = godfather.metadata("install_time")?;

    let recent_events = event::list_recent(conn, 20, Some(Severity::Warn))?;

    // Resource summary — peak rss / mean cpu / sample count.
    let summary = conn
        .query_row(
            "SELECT COUNT(*), MAX(rss_mb), AVG(cpu_pct)
             FROM _crab_resource_sample WHERE session_id = ?1",
            rusqlite::params![session_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?.unwrap_or_default(),
                    row.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                ))
            },
        )
        .unwrap_or((0, 0, 0.0));

    let mut s = String::new();
    s.push_str("# crabcc crash report\n\n");
    write!(
        s,
        "**Session**: `{}` · **App**: `{}` · **Version**: `{}` · **PID**: `{}`\n\n",
        session.id, session.app, session.version, session.pid
    )
    .unwrap();
    write!(
        s,
        "**Crash time** (unix): `{}`  ·  **Exit code**: `{}`  ·  **Signal**: `{}`\n\n",
        ts,
        exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "—".into()),
        exit_signal
            .map(|sig| sig.to_string())
            .unwrap_or_else(|| "—".into()),
    )
    .unwrap();

    s.push_str("## Install\n\n");
    writeln!(
        s,
        "- **Version**: `{}`",
        install_version.as_deref().unwrap_or("unknown")
    )
    .unwrap();
    writeln!(
        s,
        "- **Source**: `{}`",
        install_source.as_deref().unwrap_or("unknown")
    )
    .unwrap();
    write!(
        s,
        "- **Installed at** (unix): `{}`\n\n",
        install_time.as_deref().unwrap_or("unknown")
    )
    .unwrap();

    s.push_str("## Host (PII-clean)\n\n");
    if let Some(h) = host {
        writeln!(
            s,
            "- **OS**: `{}` `{}` · **arch**: `{}`",
            h.os, h.os_version, h.arch
        )
        .unwrap();
        writeln!(
            s,
            "- **CPU**: `{}` cores · **RAM**: `{}` MB",
            h.cpu_count, h.total_memory_mb
        )
        .unwrap();
        write!(
            s,
            "- **hostname-hash**: `{}` · **machine-id-hash**: `{}`\n\n",
            h.hostname_hash, h.machine_id_hash
        )
        .unwrap();
    } else {
        s.push_str("- (host info not yet recorded)\n\n");
    }

    s.push_str("## Resource summary\n\n");
    write!(
        s,
        "- Samples: `{}` · Peak RSS: `{}` MB · Mean CPU: `{:.1}%`\n\n",
        summary.0, summary.1, summary.2
    )
    .unwrap();

    s.push_str("## Recent events (warn+)\n\n");
    if recent_events.is_empty() {
        s.push_str("- (none)\n\n");
    } else {
        for ev in &recent_events {
            writeln!(
                s,
                "- `{}` `{}` `{}/{}` — {}",
                ev.ts,
                ev.severity.as_str(),
                ev.source,
                ev.category,
                ev.message
            )
            .unwrap();
        }
        s.push('\n');
    }

    s.push_str("## Log tail\n\n");
    s.push_str("```\n");
    s.push_str(log_tail.as_deref().unwrap_or("(no log captured)"));
    s.push_str("\n```\n");

    Ok(s)
}

/// Shell out to `gh issue create` with the report. Returns the
/// created issue URL on success. Errors when:
///   * `gh` isn't on PATH
///   * the user isn't authenticated (`gh auth status`)
///   * the network is unreachable
///
/// On any error the dashboard should fall back to "copy report to
/// clipboard" rather than retry — `gh` already retries internally.
pub fn open_gh_issue(godfather: &Godfather, crash_id: i64, repo: &str) -> Result<String> {
    let body = build_report(godfather, crash_id)?;
    let title = format!("Crash report — godfather#{crash_id}");

    let out = Command::new("gh")
        .args([
            "issue",
            "create",
            "--repo",
            repo,
            "--title",
            &title,
            "--body-file",
            "-",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| "spawn `gh issue create` — is gh on PATH?")?;

    {
        use std::io::Write;
        let mut stdin = out
            .stdin
            .as_ref()
            .ok_or_else(|| anyhow!("piped stdin missing"))?;
        stdin.write_all(body.as_bytes())?;
        stdin.flush()?;
    }

    let result = out.wait_with_output()?;
    if !result.status.success() {
        return Err(anyhow!(
            "gh issue create failed: {}",
            String::from_utf8_lossy(&result.stderr)
        ));
    }

    let url = String::from_utf8_lossy(&result.stdout).trim().to_string();
    // Persist the URL on the crash row so the dashboard can show
    // "issue → URL" without re-shelling.
    godfather.conn().execute(
        "UPDATE _crab_crash SET gh_issue_url = ?1 WHERE id = ?2",
        rusqlite::params![url, crash_id],
    )?;
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::godfather::InstallSource;
    use tempfile::tempdir;

    #[test]
    fn build_report_renders_required_sections() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("_internal.db");
        let g = Godfather::open_at(&path).unwrap();
        g.record_install_once("3.0.0", InstallSource::Source)
            .unwrap();
        g.record_host_info().unwrap();

        let sid = g.record_session_start("viz", "3.0.0", 12345).unwrap();
        g.record_event(
            Some(&sid),
            Severity::Warn,
            "viz",
            "lifecycle",
            "early signal",
            None,
        )
        .unwrap();
        g.record_resource_sample(&sid, 256, 12.5, 1024).unwrap();
        let cid = g
            .record_crash(&sid, Some(139), Some(11), Some("…tail…"))
            .unwrap();

        let md = build_report(&g, cid).unwrap();
        assert!(md.contains("# crabcc crash report"));
        assert!(md.contains("## Install"));
        assert!(md.contains("## Host"));
        assert!(md.contains("## Resource summary"));
        assert!(md.contains("## Recent events"));
        assert!(md.contains("## Log tail"));
        assert!(md.contains("`139`")); // exit code
        assert!(md.contains("…tail…"));
    }

    #[test]
    fn build_report_errors_on_missing_crash() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("_internal.db");
        let g = Godfather::open_at(&path).unwrap();
        let err = build_report(&g, 9999).unwrap_err();
        assert!(err.to_string().contains("crash 9999 not found"));
    }
}
