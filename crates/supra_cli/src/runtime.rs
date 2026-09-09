#![deny(missing_docs)]
#![allow(dead_code, reason = "turn driver grows stepwise through T30")]

use supra_config::Config;

pub(crate) struct Plan {
    pub(crate) tier: supra_types::Tier,
    pub(crate) limit: usize,
    pub(crate) admitted: Option<(supra_types::Tier, usize)>,
}

pub(crate) fn plan_turn(config: &Config, requested: supra_types::Tier) -> Plan {
    let limit = config.cohort_limit();
    let admitted = supra_types::admit(requested, limit);
    Plan { tier: requested, limit, admitted }
}

pub(crate) fn describe(plan: &Plan) -> String {
    match plan.admitted {
        Some((admitted_tier, k)) => {
            format!("tier {} admitted at k={} (limit {})", tier_label(admitted_tier), k, plan.limit)
        }
        None => format!("tier {} has no room under limit {}", tier_label(plan.tier), plan.limit),
    }
}

fn tier_label(tier: supra_types::Tier) -> &'static str {
    match tier {
        supra_types::Tier::E0 => "E0",
        supra_types::Tier::E1 => "E1",
        supra_types::Tier::E2 => "E2",
        supra_types::Tier::E3 => "E3",
        supra_types::Tier::E4 => "E4",
        supra_types::Tier::E5 => "E5",
    }
}

pub(crate) fn estimate_tier() -> supra_types::Tier {
    let signals = supra_cohort::Signals::minimal();
    let areas = supra_cohort::AreaFlags::none();
    supra_cohort::estimate(&signals, &areas)
}
