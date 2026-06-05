use crate::{
    compact_config::CompactConfig,
    coder::RuleBasedCoder,
    reasoner::RuleBasedReasoner,
    smoothness::DiffBasedScorer,
    tokens::count_tokens,
    traits::*,
    types::*,
};

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
        // Re-score against unchanged code so smoothness is consistent with zero steps.
        let identity_smoothness =
            DiffBasedScorer.score(&input.original_code, &input.original_code, &[]);
        return Ok(CompactOutput {
            compacted_code: input.original_code,
            steps: vec![],
            metrics: CompactMetrics {
                tokens_before,
                tokens_after: tokens_before,
                tokens_saved: 0,
                complexity_before: 1.0,
                complexity_after: 1.0,
                process_time_ms: t0.elapsed().as_millis() as u64,
            },
            smoothness: identity_smoothness,
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
        // Use whitespace-only lines (spaces) so patterns.rs::find() generates steps.
        // With max_disruption=0.0 and at least one step, the disruption guard fires.
        let code = "fn foo() {\n    \n    let x = 1;\n    \n    x\n}\n";
        let input = make_input(code);
        let mut config = default_config();
        config.max_disruption = 0.0;
        let output = run_compact(input.clone(), &config).unwrap();
        assert_eq!(output.compacted_code, input.original_code);
        // Steps must be empty: the disruption-rejection path was taken, not the no-steps path.
        assert!(output.steps.is_empty(), "disruption-rejected output must have no steps");
        // Smoothness must be consistent with no transformation (disruption=0).
        assert_eq!(output.smoothness.disruption, 0.0);
    }
}
