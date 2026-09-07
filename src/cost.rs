use crate::config::PassConfig;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LoopCosts {
    pub(crate) scalar_iteration: u64,
    pub(crate) vector_iteration: u64,
    pub(crate) setup: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Plan {
    pub(crate) vf: u32,
    pub(crate) scalar_cost: u64,
    pub(crate) vector_cost: u64,
    pub(crate) utilization_x100: u64,
}

pub(crate) fn choose_plan(
    config: PassConfig,
    element_bits: u32,
    estimated_trip_count: u64,
    costs: LoopCosts,
) -> Option<Plan> {
    if element_bits == 0 || estimated_trip_count == 0 || costs.scalar_iteration == 0 {
        return None;
    }

    let natural_vf = PassConfig::vector_bits().checked_div(element_bits)?.max(1);
    let candidate_vf = config.forced_vf.unwrap_or(natural_vf);
    if candidate_vf < 2 || !candidate_vf.is_power_of_two() || candidate_vf > 64 {
        return None;
    }
    if estimated_trip_count < config.minimum_trip_count(candidate_vf) {
        return None;
    }

    let vector_iterations = estimated_trip_count / u64::from(candidate_vf);
    let remainder = estimated_trip_count % u64::from(candidate_vf);
    let scalar_cost = estimated_trip_count.saturating_mul(costs.scalar_iteration);
    let vector_cost = costs
        .setup
        .saturating_add(vector_iterations.saturating_mul(costs.vector_iteration))
        .saturating_add(remainder.saturating_mul(costs.scalar_iteration));
    let profitable = config.forced_vf.is_some()
        || scalar_cost.saturating_mul(100)
            >= vector_cost.saturating_mul(config.required_speedup_x100());
    if !profitable {
        return None;
    }

    Some(Plan {
        vf: candidate_vf,
        scalar_cost,
        vector_cost,
        utilization_x100: vector_iterations
            .saturating_mul(u64::from(candidate_vf))
            .saturating_mul(100)
            / estimated_trip_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Heuristic;

    fn config(heuristic: Heuristic) -> PassConfig {
        PassConfig::new(heuristic, 0, false)
    }

    #[test]
    fn selects_four_lanes_for_f32_on_128_bit_vectors() {
        let plan = choose_plan(
            config(Heuristic::Balanced),
            32,
            64,
            LoopCosts {
                scalar_iteration: 8,
                vector_iteration: 8,
                setup: 6,
            },
        )
        .expect("the representative loop should be profitable");
        assert_eq!(plan.vf, 4);
        assert_eq!(plan.utilization_x100, 100);
        assert!(plan.vector_cost < plan.scalar_cost);
    }

    #[test]
    fn conservative_policy_rejects_short_loops() {
        let plan = choose_plan(
            config(Heuristic::Conservative),
            32,
            8,
            LoopCosts {
                scalar_iteration: 8,
                vector_iteration: 8,
                setup: 6,
            },
        );
        assert_eq!(plan, None);
    }

    #[test]
    fn force_width_still_rejects_invalid_widths() {
        let invalid = PassConfig::new(Heuristic::Aggressive, 3, false);
        assert_eq!(
            choose_plan(
                invalid,
                32,
                64,
                LoopCosts {
                    scalar_iteration: 2,
                    vector_iteration: 2,
                    setup: 0,
                }
            ),
            None
        );
    }

    #[test]
    fn force_width_overrides_profitability_but_not_legality_shape() {
        let forced = PassConfig::new(Heuristic::Balanced, 4, false);
        let plan = choose_plan(
            forced,
            32,
            4,
            LoopCosts {
                scalar_iteration: 1,
                vector_iteration: 100,
                setup: 100,
            },
        )
        .expect("a valid forced width should bypass the profitability filter");
        assert_eq!(plan.vf, 4);
    }
}
