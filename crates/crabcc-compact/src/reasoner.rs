use crate::{traits::Reasoner, types::*};

pub struct RuleBasedReasoner;

impl Reasoner for RuleBasedReasoner {
    fn analyze(&self, input: &CompactInput) -> anyhow::Result<Vec<TransformStep>> {
        let mut steps = Vec::new();
        steps.extend(crate::rules::redundancy::find(input));
        steps.extend(crate::rules::conditionals::find(input));
        steps.extend(crate::rules::dead_code::find(input));
        steps.extend(crate::rules::expressions::find(input));
        steps.extend(crate::rules::patterns::find(input));
        Ok(steps)
    }
}
