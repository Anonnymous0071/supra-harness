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

#[cfg(test)]
mod tests {
    use super::*;
    use supra_config::ConfigLayer;

    fn config_with_limit(limit: usize) -> Config {
        use std::path::PathBuf;
        let text = format!("[cohort]\nlimit = {limit}\n");
        let layer = ConfigLayer::parse(&text, supra_config::ConfigSource::User, PathBuf::from("test"))
            .expect("valid fixture");
        layer.validate(supra_config::ConfigSource::User).expect("valid fixture");
        supra_config::resolve(&[(supra_config::ConfigSource::User, layer)])
    }

    #[test]
    fn a_minimal_task_estimates_e0() {
        assert_eq!(estimate_tier(), supra_types::Tier::E0);
    }

    #[test]
    fn admission_never_leaves_a_tier_gap() {
        for requested in [
            supra_types::Tier::E0,
            supra_types::Tier::E1,
            supra_types::Tier::E2,
            supra_types::Tier::E3,
            supra_types::Tier::E4,
            supra_types::Tier::E5,
        ] {
            for limit in 1..=80usize {
                let config = config_with_limit(limit);
                let plan = plan_turn(&config, requested);
                if let Some((tier, k)) = plan.admitted {
                    assert_eq!(
                        supra_types::Tier::containing(k),
                        Some(tier),
                        "limit {limit} requested {requested:?} admitted {tier:?} at k={k}"
                    );
                }
            }
        }
    }

    #[test]
    fn describe_names_the_admitted_tier_not_the_request() {
        let config = config_with_limit(6);
        let plan = plan_turn(&config, supra_types::Tier::E3);
        assert_eq!(plan.admitted, Some((supra_types::Tier::E2, 5)), "limit 6 reduces E3 to E2/k=5");
        let text = describe(&plan);
        assert!(text.contains("E2"), "admitted tier: {text}");
        assert!(text.contains("k=5"), "{text}");
    }
}
