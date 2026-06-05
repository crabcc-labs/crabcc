use crate::{traits::SmoothnessEvaluator, types::*};
use std::collections::HashSet;

pub struct DiffBasedScorer;

impl SmoothnessEvaluator for DiffBasedScorer {
    fn score(&self, original: &str, compacted: &str, _steps: &[TransformStep]) -> SmoothnessScore {
        let orig_lines: Vec<&str> = original.lines().collect();
        let comp_lines: Vec<&str> = compacted.lines().collect();

        let orig_set: HashSet<&str> = orig_lines.iter().copied().collect();

        // disruption: 1 - (common_lines / max_lines)
        let common_lines = comp_lines.iter().filter(|l| orig_set.contains(*l)).count();
        let max_lines = orig_lines.len().max(comp_lines.len());
        let disruption = if max_lines == 0 {
            0.0
        } else {
            1.0 - (common_lines as f64 / max_lines as f64)
        };

        // readability: avg_line_len_score + ident_score / 2
        let non_empty: Vec<&str> = comp_lines.iter().copied().filter(|l| !l.is_empty()).collect();
        let avg_line_len = if non_empty.is_empty() {
            0.0
        } else {
            non_empty.iter().map(|l| l.len()).sum::<usize>() as f64 / non_empty.len() as f64
        };
        let avg_line_len_score = (1.0 - (avg_line_len - 40.0).max(0.0) / 60.0).clamp(0.0, 1.0);

        let words: Vec<&str> = compacted.split_whitespace().collect();
        let avg_ident_len = if words.is_empty() {
            0.0
        } else {
            words.iter().map(|w| w.len()).sum::<usize>() as f64 / words.len() as f64
        };
        let ident_score = if avg_ident_len < 25.0 { 1.0 } else { 0.5 };
        let readability = (avg_line_len_score + ident_score) / 2.0;

        // compatibility: same non-empty line count -> 1.0, else 0.8
        let orig_nonempty = orig_lines.iter().filter(|l| !l.is_empty()).count();
        let comp_nonempty = comp_lines.iter().filter(|l| !l.is_empty()).count();
        let compatibility = if orig_nonempty == comp_nonempty { 1.0 } else { 0.8 };

        // preservation: lines in compacted that appear in original / original line count
        let preservation = if orig_lines.is_empty() {
            1.0
        } else {
            common_lines as f64 / orig_lines.len() as f64
        };

        SmoothnessScore {
            disruption,
            readability,
            compatibility,
            preservation,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scorer() -> DiffBasedScorer {
        DiffBasedScorer
    }

    #[test]
    fn identical_input_scores() {
        let code = "fn foo() {\n    let x = 1;\n    x\n}";
        let s = scorer().score(code, code, &[]);
        assert_eq!(s.disruption, 0.0);
        assert_eq!(s.preservation, 1.0);
        assert_eq!(s.compatibility, 1.0);
        // readability depends on line lengths; just check it's in [0,1]
        assert!((0.0..=1.0).contains(&s.readability));
    }

    #[test]
    fn whitespace_removal_high_preservation() {
        let original = "fn foo() {\n    let x = 1;\n\n    x\n}";
        // remove blank lines only
        let compacted = "fn foo() {\n    let x = 1;\n    x\n}";
        let s = scorer().score(original, compacted, &[]);
        // all compacted lines are in original -> preservation = count/orig_total
        // orig has 5 lines (one blank), compacted has 4; common = 4
        assert!(s.preservation >= 0.8, "preservation={}", s.preservation);
        // disruption should be low since most lines are shared
        assert!(s.disruption <= 0.2, "disruption={}", s.disruption);
    }

    #[test]
    fn random_rewrite_higher_disruption() {
        let original = "fn foo() {\n    let x = 1;\n    x\n}";
        let blank_removed = "fn foo() {\n    let x = 1;\n    x\n}"; // same
        let rewrite = "struct Bar { y: i32 }\nimpl Bar { fn baz() {} }";

        let s_similar = scorer().score(original, blank_removed, &[]);
        let s_random = scorer().score(original, rewrite, &[]);
        assert!(
            s_similar.disruption <= s_random.disruption,
            "similar disruption {} should be <= random disruption {}",
            s_similar.disruption,
            s_random.disruption
        );
    }

    #[test]
    fn empty_strings() {
        let s = scorer().score("", "", &[]);
        assert_eq!(s.disruption, 0.0);
        assert_eq!(s.preservation, 1.0);
        assert_eq!(s.compatibility, 1.0);
    }

    #[test]
    fn disruption_range() {
        let original = "a\nb\nc\nd";
        let compacted = "x\ny\nz\nw";
        let s = scorer().score(original, compacted, &[]);
        assert!((0.0..=1.0).contains(&s.disruption));
        assert!((0.0..=1.0).contains(&s.preservation));
        assert!((0.0..=1.0).contains(&s.readability));
        assert!((0.0..=1.0).contains(&s.compatibility));
    }
}
