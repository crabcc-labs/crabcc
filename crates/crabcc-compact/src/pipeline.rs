use crate::{
    compact_config::CompactConfig,
    coder::RuleBasedCoder,
    reasoner::RuleBasedReasoner,
    smoothness::DiffBasedScorer,
    tokens::count_tokens,
    traits::*,
    types::*,
};
use crabcc_memory::Palace;

pub fn run_compact(input: CompactInput, config: &CompactConfig) -> anyhow::Result<CompactOutput> {
    let tokens_before = count_tokens(&input.original_code);
    let t0 = std::time::Instant::now();

    // 1. Run reasoner
    let all_steps = RuleBasedReasoner.analyze(&input)?;

    // 2. Filter by config thresholds
    let steps: Vec<TransformStep> = all_steps
        .into_iter()
        .filter(|s| s.tokens_saved >= config.min_token_savings)
        .collect();

    // 3. Apply transforms
    let compacted_code = if steps.is_empty() {
        input.original_code.clone()
    } else {
        RuleBasedCoder.apply(&input.original_code, &steps)?
    };

    // 4. Score smoothness
    let smoothness = DiffBasedScorer.score(&input.original_code, &compacted_code, &steps);

    // 5. Reject if too disruptive
    if smoothness.disruption > config.max_disruption && !steps.is_empty() {
        let tokens_after = tokens_before;
        return Ok(CompactOutput {
            compacted_code: input.original_code,
            steps: vec![],
            metrics: CompactMetrics {
                tokens_before,
                tokens_after,
                tokens_saved: 0,
                complexity_before: 1.0,
                complexity_after: 1.0,
                process_time_ms: t0.elapsed().as_millis() as u64,
            },
            smoothness,
        });
    }

    let tokens_after = count_tokens(&compacted_code);
    Ok(CompactOutput {
        compacted_code,
        steps,
        metrics: CompactMetrics {
            tokens_before,
            tokens_after,
            tokens_saved: tokens_before.saturating_sub(tokens_after),
            complexity_before: 1.0,
            complexity_after: 1.0,
            process_time_ms: t0.elapsed().as_millis() as u64,
        },
        smoothness,
    })
}

pub fn run_compact_with_history(
    input: CompactInput,
    config: &CompactConfig,
    palace: &Palace,
) -> anyhow::Result<CompactOutput> {
    let output = run_compact(input.clone(), config)?;
    if output.metrics.tokens_saved > 0 {
        let _ = crate::history::append_history(palace, &input.session_id, &output);
        // best-effort: ignore history errors
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> CompactConfig {
        CompactConfig::default()
    }

    fn make_input(code: &str) -> CompactInput {
        CompactInput {
            session_id: "test".to_string(),
            original_code: code.to_string(),
            file_type: "rust".to_string(),
            project_scope: None,
        }
    }

    #[test]
    fn empty_input_returns_empty_output() {
        let input = make_input("");
        let output = run_compact(input, &default_config()).unwrap();
        assert!(output.compacted_code.is_empty() || output.compacted_code == "");
    }

    #[test]
    fn fixture_with_blank_lines_compacted() {
        // Use repeated non-empty lines so the redundancy rule fires and produces
        // measurable token savings (count_tokens uses word count, so truly empty
        // lines contribute zero tokens and cannot show savings).
        let code = "fn foo() {\n    let x = 1;\n    let x = 1;\n    let x = 1;\n    let x = 1;\n    x\n}\n";
        let input = make_input(code);
        let output = run_compact(input, &default_config()).unwrap();
        assert!(
            output.metrics.tokens_saved > 0,
            "expected tokens_saved > 0, got {}",
            output.metrics.tokens_saved
        );
    }

    #[test]
    fn high_disruption_returns_original() {
        let code = "fn foo() {\n\n\n\n    let x = 1;\n\n\n\n    x\n\n\n\n}\n";
        let input = make_input(code);
        let mut config = default_config();
        config.max_disruption = 0.0;
        let output = run_compact(input.clone(), &config).unwrap();
        assert_eq!(output.compacted_code, input.original_code);
    }
}
