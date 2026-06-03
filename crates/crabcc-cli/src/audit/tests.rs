#[cfg(test)]
mod tests {
    use crate::audit::aggregate::build_report;
    use crate::audit::model::{AuditReport, SessionFile, WasteFinding};
    use crate::audit::render::render_json;
    use crate::audit::tokens::estimate_tokens;
    use crate::audit::waste::analyze;

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("a"), 1);
        assert_eq!(estimate_tokens("xxxxxxxx"), 2); // 8/4 = 2
    }

    #[test]
    fn test_analyze_duplicate_read() {
        let session = SessionFile {
            path: "test-session".to_string(),
            project: "test-project".to_string(),
            messages: 0,
            total_tokens: 0,
        };
        let raw_lines = vec![
            "line 1".to_string(),
            "line 2".to_string(),
            "line 2".to_string(),
            "line 2".to_string(),
            "line 3".to_string(),
        ];
        let findings = analyze(&session, &raw_lines);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, "duplicate_read");
        // "line 2" appears 3 times, tokens = estimate*(count-1) = 1*(3-1) = 2
        assert_eq!(findings[0].tokens, 2);
    }

    #[test]
    fn test_analyze_huge_tool_output() {
        let session = SessionFile {
            path: "test-session".to_string(),
            project: "test-project".to_string(),
            messages: 0,
            total_tokens: 0,
        };
        let huge_line = "a".repeat(50000);
        let raw_lines = vec![
            "normal line".to_string(),
            huge_line.clone(),
            "another normal line".to_string(),
        ];
        let findings = analyze(&session, &raw_lines);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, "huge_tool_output");
        // 50000 chars / 4 = 12500 estimated tokens (no cap)
        assert_eq!(findings[0].tokens, 12500);
    }

    #[test]
    fn test_build_report_aggregation_and_sorting() {
        let sessions = vec![
            SessionFile {
                path: "session1".to_string(),
                project: "project1".to_string(),
                messages: 0,
                total_tokens: 100,
            },
            SessionFile {
                path: "session2".to_string(),
                project: "project2".to_string(),
                messages: 0,
                total_tokens: 200,
            },
        ];
        let findings = vec![
            WasteFinding {
                session: "session1".to_string(),
                kind: "duplicate_read".to_string(),
                detail: "duplicate line".to_string(),
                tokens: 50,
            },
            WasteFinding {
                session: "session2".to_string(),
                kind: "huge_tool_output".to_string(),
                detail: "huge line".to_string(),
                tokens: 150,
            },
        ];
        let report = build_report(&sessions, findings);
        assert_eq!(report.sessions_scanned, 2);
        assert_eq!(report.total_tokens, 300);
        assert_eq!(report.wasted_tokens, 200);
        // Check that findings are sorted by tokens descending
        assert_eq!(report.findings.len(), 2);
        assert_eq!(report.findings[0].tokens, 150);
        assert_eq!(report.findings[1].tokens, 50);
    }

    #[test]
    fn test_render_json_contains_sessions_scanned() {
        let report = AuditReport {
            sessions_scanned: 5,
            total_tokens: 1000,
            wasted_tokens: 200,
            findings: vec![],
        };
        let json_result = render_json(&report);
        assert!(json_result.is_ok());
        let json_string = json_result.unwrap();
        assert!(json_string.contains("sessions_scanned"));
        assert!(json_string.contains("5"));
    }
}
