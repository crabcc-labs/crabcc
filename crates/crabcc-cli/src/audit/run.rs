use crate::audit::{aggregate, discover, parse, render, waste};
use anyhow::Result;
use std::path::Path;

pub fn run_audit(path: Option<&Path>, json: bool) -> Result<()> {
    let root = match path {
        Some(p) => p.to_path_buf(),
        None => {
            let home = std::env::var("HOME")?;
            Path::new(&home).join(".claude/projects")
        }
    };

    let log_paths = discover::find_session_logs(&root)?;
    let mut sessions = Vec::new();
    let mut all_findings = Vec::new();

    for log_path in log_paths {
        let content = std::fs::read_to_string(&log_path)?;
        let lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
        let session = parse::parse_session(&log_path)?;
        let findings = waste::analyze(&session, &lines);

        all_findings.extend(findings);
        sessions.push(session);
    }

    let report = aggregate::build_report(&sessions, all_findings);

    if json {
        println!("{}", render::render_json(&report)?);
    } else {
        println!("{}", render::render_human(&report));
    }

    Ok(())
}
