use crabcc_compact::{compact_memory, compact_config::CompactConfig, pipeline::run_compact, types::*};
use crabcc_memory::Palace;
use std::sync::Mutex;

// Tests that mutate CRABCC_COMPACT_HOOK must hold this lock to avoid races
// between parallel test threads (env is process-global state).
static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn pipeline_produces_output_for_blank_heavy_code() {
    let input = CompactInput {
        session_id: "integration-test-1".to_string(),
        original_code: "fn main() {\n\n\n    let x = 1;\n\n\n    println!(\"{}\", x);\n\n\n}\n"
            .to_string(),
        file_type: "rust".to_string(),
        project_scope: None,
    };
    let config = CompactConfig::default();
    let output = run_compact(input, &config).unwrap();
    assert!(!output.compacted_code.is_empty());
    assert!(output.metrics.tokens_before > 0);
}

#[test]
fn compact_memory_round_trip() {
    let palace = Palace::ephemeral();
    let body = CompactDrawerBody {
        original: "fn main() {}".to_string(),
        compacted: "fn main(){}".to_string(),
        steps: vec![],
        metrics: CompactMetrics {
            tokens_before: 5,
            tokens_after: 4,
            tokens_saved: 1,
            complexity_before: 1.0,
            complexity_after: 1.0,
            process_time_ms: 1,
        },
        smoothness: SmoothnessScore {
            disruption: 0.1,
            readability: 0.9,
            compatibility: 1.0,
            preservation: 0.9,
        },
        context: CompactContext {
            file_type: "rust".to_string(),
            project_scope: None,
        },
    };
    compact_memory::store(&palace, "test-session", &body).unwrap();
    let listed = compact_memory::list(&palace, 10).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].original, "fn main() {}");
}

#[test]
fn compact_stats_aggregates_savings() {
    let _guard = ENV_LOCK.lock().unwrap();
    // compact_stats is gated behind CRABCC_COMPACT_HOOK=1
    unsafe { std::env::set_var("CRABCC_COMPACT_HOOK", "1") };
    let palace = Palace::ephemeral();
    // Store 2 compact outputs with known savings
    for i in 0..2u32 {
        let body = CompactDrawerBody {
            original: format!("code_{i}"),
            compacted: format!("c_{i}"),
            steps: vec![],
            metrics: CompactMetrics {
                tokens_before: 10,
                tokens_after: 5,
                tokens_saved: 5 + i,
                complexity_before: 1.0,
                complexity_after: 1.0,
                process_time_ms: 1,
            },
            smoothness: SmoothnessScore {
                disruption: 0.1,
                readability: 0.9,
                compatibility: 1.0,
                preservation: 0.9,
            },
            context: CompactContext {
                file_type: "rust".to_string(),
                project_scope: None,
            },
        };
        compact_memory::store(&palace, &format!("session-{i}"), &body).unwrap();
    }
    let stats = compact_memory::compact_stats(&palace).unwrap();
    unsafe { std::env::remove_var("CRABCC_COMPACT_HOOK") };
    assert_eq!(stats.total, 2);
    assert!(stats.total_tokens_saved >= 10);
}

#[test]
fn sessions_mine_stores_compact_entries() {
    use crabcc_memory::mine::sessions::{mine_sessions, MineSessionsOpts};
    use std::io::Write;
    use tempfile::NamedTempFile;

    let _guard = ENV_LOCK.lock().unwrap();
    unsafe { std::env::set_var("CRABCC_COMPACT_HOOK", "1") };

    let mut f = NamedTempFile::with_suffix(".jsonl").unwrap();
    let entry = serde_json::json!({
        "type": "compact",
        "session_id": "test-mine-session",
        "original_code": "fn main() {}"
    });
    writeln!(f, "{}", entry).unwrap();

    let palace = Palace::ephemeral();
    let report = mine_sessions(&palace, f.path(), &MineSessionsOpts::default()).unwrap();

    unsafe { std::env::remove_var("CRABCC_COMPACT_HOOK") };

    assert!(report.inserted >= 1, "should have stored compact entry");
    let drawers = palace.list_drawers(Some("compact"), 10).unwrap();
    assert!(!drawers.is_empty(), "compact wing should have entries");
}
