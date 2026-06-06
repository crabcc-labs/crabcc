use super::budget::Budget;

pub fn pick_ratio(budget: &Budget) -> f32 {
    if budget.pressure() >= 0.8 { 0.3 } else { 0.5 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_pressure_returns_moderate_ratio() {
        let b = Budget::new();
        assert!((pick_ratio(&b) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn high_pressure_returns_aggressive_ratio() {
        let mut b = Budget::new();
        b.record_compress(90_000, 45_000);
        assert!((pick_ratio(&b) - 0.3).abs() < 1e-6);
    }
}
