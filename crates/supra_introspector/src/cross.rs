use supra_types::AgentId;

use crate::finding::{Finding, Kind};

/// One peer's answer to one shared question.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    /// Which peer answered.
    pub agent: AgentId,
    /// The answer's text, canonical already.
    pub text: String,
}

/// Compare peer answers to one question and report divergences.
///
/// A cross-agent finding names the minority side(s), not a verdict about
/// who is right: agreement is evidence and divergence is a question, and
/// the blackboard's quorum - not this check - decides.
#[must_use]
pub fn cross_check(question: &str, answers: &[Answer]) -> Vec<Finding> {
    if answers.len() < 2 {
        return Vec::new();
    }
    let mut groups: Vec<(String, usize)> = Vec::new();
    for answer in answers {
        match groups.iter_mut().find(|(text, _)| *text == answer.text) {
            Some((_, count)) => *count += 1,
            None => groups.push((answer.text.clone(), 1)),
        }
    }
    groups.sort_by_key(|(_, count)| *count);
    let majority_count = groups[groups.len() - 1].1;
    if groups.len() == 1 || majority_count == answers.len() {
        return Vec::new();
    }
    let minority: Vec<&str> = groups[..groups.len() - 1].iter().map(|(text, _)| text.as_str()).collect();
    vec![Finding::new(
        Kind::CrossAgent,
        "peers",
        format!(
            "peers diverge on {question:?}: {} of {} agree, {} differ",
            majority_count,
            answers.len(),
            minority.len()
        ),
        None,
        None,
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answers(texts: &[&str]) -> Vec<Answer> {
        texts.iter().map(|text| Answer { agent: AgentId::generate(), text: (*text).to_owned() }).collect()
    }

    #[test]
    fn unanimous_answers_report_nothing() {
        let found = cross_check("best fix", &answers(&["a", "a", "a"]));
        assert!(found.is_empty());
    }

    #[test]
    fn a_single_answer_reports_nothing() {
        assert!(cross_check("q", &answers(&["only"])).is_empty());
        assert!(cross_check("q", &[]).is_empty());
    }

    #[test]
    fn divergence_names_the_minority_count() {
        let found = cross_check("best fix", &answers(&["a", "a", "b"]));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, Kind::CrossAgent);
        assert!(found[0].summary.contains("2 of 3 agree"), "{}", found[0].summary);
        assert!(found[0].summary.contains("1 differ"), "{}", found[0].summary);
    }

    #[test]
    fn a_three_way_split_reports_two_minority_sides() {
        let found = cross_check("q", &answers(&["a", "b", "c"]));
        assert_eq!(found.len(), 1);
        assert!(found[0].summary.contains("2 differ"), "{}", found[0].summary);
    }

    #[test]
    fn findings_carry_cross_agent_evidence() {
        let found = cross_check("q", &answers(&["a", "b"]));
        assert_eq!(found[0].evidence_ref(), "peers#peers");
    }
}
