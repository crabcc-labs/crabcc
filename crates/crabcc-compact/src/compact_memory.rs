use crate::types::{CompactContext, CompactDrawerBody, CompactInput, CompactOutput};
use anyhow::Result;
use crabcc_memory::Palace;
use serde::{Deserialize, Serialize};

pub const WING: &str = "compact";

pub fn store(palace: &Palace, session_id: &str, body: &CompactDrawerBody) -> Result<()> {
    if std::env::var("CRABCC_COMPACT_HOOK").as_deref() != Ok("1") {
        return Ok(());
    }
    let json = serde_json::to_string(body)?;
    palace.remember(
        WING,
        Some("enriched"),
        &format!("compact:{session_id}"),
        &json,
    )?;
    Ok(())
}

pub fn store_raw(palace: &Palace, session_id: &str, input: &CompactInput) -> Result<()> {
    if std::env::var("CRABCC_COMPACT_HOOK").as_deref() != Ok("1") {
        return Ok(());
    }
    let summary = format!(
        "compact:raw session={} file_type={} tokens={}",
        session_id,
        input.file_type,
        input.original_code.len() / 4,
    );
    palace.remember(
        WING,
        Some("raw"),
        &format!("compact:raw:{session_id}"),
        &summary,
    )?;
    Ok(())
}

pub fn list(palace: &Palace, limit: usize) -> Result<Vec<CompactDrawerBody>> {
    let drawers = palace.list_drawers(Some(WING), limit)?;
    Ok(drawers
        .iter()
        .filter(|d| d.room.as_deref() == Some("enriched"))
        .filter_map(|d| serde_json::from_str(&d.body).ok())
        .collect())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactStats {
    pub total: usize,
    pub total_tokens_saved: u32,
    pub avg_readability: f64,
}

pub fn compact_stats(palace: &Palace) -> Result<CompactStats> {
    if std::env::var("CRABCC_COMPACT_HOOK").as_deref() != Ok("1") {
        return Ok(CompactStats {
            total: 0,
            total_tokens_saved: 0,
            avg_readability: 0.0,
        });
    }
    // Use a large limit; palace doesn't support unbounded list so use usize::MAX
    let bodies = list(palace, usize::MAX)?;
    let total = bodies.len();
    let total_tokens_saved = bodies.iter().map(|b| b.metrics.tokens_saved).sum();
    let avg_readability = if total == 0 {
        0.0
    } else {
        bodies.iter().map(|b| b.smoothness.readability).sum::<f64>() / total as f64
    };
    Ok(CompactStats {
        total,
        total_tokens_saved,
        avg_readability,
    })
}

pub fn compact_drawer_body_from_output(
    input_original: &str,
    output: &CompactOutput,
    file_type: &str,
    project_scope: Option<String>,
) -> CompactDrawerBody {
    CompactDrawerBody {
        original: input_original.to_string(),
        compacted: output.compacted_code.clone(),
        steps: output.steps.clone(),
        metrics: output.metrics.clone(),
        smoothness: output.smoothness.clone(),
        context: CompactContext {
            file_type: file_type.to_string(),
            project_scope,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CompactContext, CompactMetrics, SmoothnessScore};
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn sample_body(session_id: &str) -> CompactDrawerBody {
        CompactDrawerBody {
            original: "fn foo() {}".to_string(),
            compacted: "fn foo(){}".to_string(),
            steps: vec![],
            metrics: CompactMetrics {
                tokens_before: 10,
                tokens_after: 8,
                tokens_saved: 2,
                complexity_before: 1.0,
                complexity_after: 1.0,
                process_time_ms: 5,
            },
            smoothness: SmoothnessScore {
                disruption: 0.1,
                readability: 0.9,
                compatibility: 1.0,
                preservation: 1.0,
            },
            context: CompactContext {
                file_type: "rust".to_string(),
                project_scope: Some(format!("proj-{session_id}")),
            },
        }
    }

    #[test]
    fn store_list_round_trip() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("CRABCC_COMPACT_HOOK", "1") };
        let palace = Palace::ephemeral();
        let body = sample_body("sess1");

        store(&palace, "sess1", &body).unwrap();

        let results = list(&palace, 100).unwrap();
        assert_eq!(results.len(), 1);
        let got = &results[0];
        assert_eq!(got.original, body.original);
        assert_eq!(got.compacted, body.compacted);
        assert_eq!(got.metrics.tokens_saved, 2);
        assert!((got.smoothness.readability - 0.9).abs() < 1e-9);
        unsafe { std::env::remove_var("CRABCC_COMPACT_HOOK") };
    }

    #[test]
    fn compact_stats_counts_correctly() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("CRABCC_COMPACT_HOOK", "1") };
        let palace = Palace::ephemeral();
        let body = sample_body("sess2");

        store(&palace, "sess2", &body).unwrap();

        let stats = compact_stats(&palace).unwrap();
        assert_eq!(stats.total, 1);
        assert_eq!(stats.total_tokens_saved, 2);
        assert!((stats.avg_readability - 0.9).abs() < 1e-9);
        unsafe { std::env::remove_var("CRABCC_COMPACT_HOOK") };
    }
}
