use crate::types::CompactOutput;
use anyhow::Result;
use crabcc_memory::Palace;

/// Append a compact run to the history for a given file path.
/// Stored in wing="compact", room="history", source_id="history:{file_path}:saved={tokens_saved}".
/// Multiple calls create separate drawers (Palace deduplicates by content hash,
/// so we include tokens_saved to distinguish different outputs).
pub fn append_history(palace: &Palace, file_path: &str, output: &CompactOutput) -> Result<()> {
    if std::env::var("CRABCC_COMPACT_HOOK").as_deref() != Ok("1") {
        return Ok(());
    }
    let version_key = format!(
        "history:{}:saved={}",
        file_path,
        output.metrics.tokens_saved,
    );
    let body = serde_json::to_string(output)?;
    palace.remember("compact", Some("history"), &version_key, &body)?;
    Ok(())
}

/// Return all history entries for a file path, newest first (by id desc).
pub fn get_history(palace: &Palace, file_path: &str, limit: usize) -> Result<Vec<CompactOutput>> {
    let drawers = palace.list_drawers(Some("compact"), limit * 3)?;
    let prefix = format!("history:{file_path}:");
    let matching: Vec<CompactOutput> = drawers
        .iter()
        .filter(|d| d.source_id.starts_with(&prefix))
        .filter_map(|d| serde_json::from_str(&d.body).ok())
        .take(limit)
        .collect();
    Ok(matching)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CompactMetrics, SmoothnessScore};
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn make_output(tokens_saved: u32) -> CompactOutput {
        CompactOutput {
            compacted_code: format!("code-{tokens_saved}"),
            steps: vec![],
            metrics: CompactMetrics {
                tokens_before: 100,
                tokens_after: 100 - tokens_saved,
                tokens_saved,
                complexity_before: 1.0,
                complexity_after: 1.0,
                process_time_ms: 1,
            },
            smoothness: SmoothnessScore {
                disruption: 0.1,
                readability: 0.9,
                compatibility: 1.0,
                preservation: 1.0,
            },
        }
    }

    #[test]
    fn three_compacts_produce_three_history_entries() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("CRABCC_COMPACT_HOOK", "1") };
        let palace = Palace::ephemeral();
        let file_path = "src/main.rs";

        // Three distinct outputs (different tokens_saved → different body → different SHA256).
        append_history(&palace, file_path, &make_output(5)).unwrap();
        append_history(&palace, file_path, &make_output(10)).unwrap();
        append_history(&palace, file_path, &make_output(20)).unwrap();

        let entries = get_history(&palace, file_path, 10).unwrap();
        assert_eq!(entries.len(), 3, "expected all three history entries");
        // Verify content fidelity; order is not guaranteed, so check as a set.
        let mut saved_values: Vec<u32> = entries.iter().map(|e| e.metrics.tokens_saved).collect();
        saved_values.sort_unstable();
        assert_eq!(saved_values, vec![5, 10, 20]);
        unsafe { std::env::remove_var("CRABCC_COMPACT_HOOK") };
    }

    #[test]
    fn get_history_filters_by_file_path() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("CRABCC_COMPACT_HOOK", "1") };
        let palace = Palace::ephemeral();

        append_history(&palace, "src/lib.rs", &make_output(3)).unwrap();
        append_history(&palace, "src/main.rs", &make_output(7)).unwrap();

        let lib_entries = get_history(&palace, "src/lib.rs", 10).unwrap();
        let main_entries = get_history(&palace, "src/main.rs", 10).unwrap();

        assert_eq!(lib_entries.len(), 1);
        assert_eq!(main_entries.len(), 1);
        assert_eq!(lib_entries[0].metrics.tokens_saved, 3);
        assert_eq!(main_entries[0].metrics.tokens_saved, 7);
        unsafe { std::env::remove_var("CRABCC_COMPACT_HOOK") };
    }

}
