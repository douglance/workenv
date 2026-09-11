//! Choosing a site from capacity reports.
//!
//! Pure over JSON so the policy can be tested without a backend.
use serde_json::{Map, Value, json};

/// What an environment needs from a site.
#[derive(Clone, Debug)]
pub(super) struct Requirement {
    pub(super) system: String,
    pub(super) cpus: u64,
    pub(super) memory_gb: u64,
    pub(super) disk_gb: u64,
}

/// One site weighed against the requirement.
#[derive(Clone, Debug)]
pub(super) struct Candidate {
    pub(super) site: String,
    pub(super) eligible: bool,
    /// Smallest slack in parts-per-thousand of total; `None` when ineligible.
    pub(super) headroom: Option<u64>,
    pub(super) warm: u64,
    pub(super) report: Value,
}

/// Resources a requirement actually constrains, paired with what it asks for.
fn demands(requirement: &Requirement) -> [(&'static str, u64); 4] {
    [
        ("cpus", requirement.cpus),
        ("memory_gb", requirement.memory_gb),
        ("disk_gb", requirement.disk_gb),
        ("units", 1),
    ]
}

fn amount(section: Option<&Value>, key: &str) -> Option<u64> {
    section
        .and_then(|value| value.get(key))
        .and_then(Value::as_u64)
}

/// Weigh one capacity report against the requirement.
///
/// A resource the site does not report is not scored. That is what lets a Mac
/// mini and a cloud plan, which measure themselves differently, be compared at
/// all — rather than one of them losing on a field the other invented.
pub(super) fn evaluate(site: &str, capacity: &Value, requirement: &Requirement) -> Candidate {
    let systems: Vec<&str> = capacity
        .get("systems")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let warm = capacity.get("warm").and_then(Value::as_u64).unwrap_or(0);

    if !systems.iter().any(|value| *value == requirement.system) {
        return ineligible(
            site,
            capacity,
            warm,
            json!({}),
            wrong_system(&systems, requirement),
        );
    }
    score(site, capacity, warm, requirement)
}

/// Why a site serving other architectures cannot take this environment.
fn wrong_system(systems: &[&str], requirement: &Requirement) -> String {
    let served = if systems.is_empty() {
        "no declared system".to_owned()
    } else {
        systems.join(", ")
    };
    format!("serves {served}, need {}", requirement.system)
}

/// Weigh a system-compatible site against the requirement.
///
/// Headroom is scored in parts-per-thousand of the site's own total, in integer
/// arithmetic. Normalising by total is what lets a Mac mini and a cloud plan be
/// compared at all; staying integral avoids a lossy widening on values that are
/// only ever small counts of cores and gigabytes.
fn score(site: &str, capacity: &Value, warm: u64, requirement: &Requirement) -> Candidate {
    let free = capacity.get("free");
    let total = capacity.get("total");
    let mut shortfall = Map::new();
    let mut worst: Option<u64> = None;
    let mut reasons = Vec::new();
    for (key, needed) in demands(requirement) {
        let Some(have) = amount(free, key) else {
            continue;
        };
        if have < needed {
            shortfall.insert(key.to_owned(), json!(needed - have));
            reasons.push(format!("{key}: have {have}, need {needed}"));
            continue;
        }
        let whole = amount(total, key).unwrap_or(have).max(1);
        let slack = (have - needed).saturating_mul(1000) / whole;
        worst = Some(worst.map_or(slack, |current: u64| current.min(slack)));
    }

    if shortfall.is_empty() {
        return Candidate {
            site: site.to_owned(),
            eligible: true,
            headroom: Some(worst.unwrap_or(0)),
            warm,
            report: report(Verdict {
                site,
                capacity,
                eligible: true,
                warm,
                shortfall: Value::Object(shortfall),
                reason: None,
            }),
        };
    }
    ineligible(
        site,
        capacity,
        warm,
        Value::Object(shortfall),
        reasons.join("; "),
    )
}

fn ineligible(
    site: &str,
    capacity: &Value,
    warm: u64,
    shortfall: Value,
    reason: String,
) -> Candidate {
    Candidate {
        site: site.to_owned(),
        eligible: false,
        headroom: None,
        warm,
        report: report(Verdict {
            site,
            capacity,
            eligible: false,
            warm,
            shortfall,
            reason: Some(reason),
        }),
    }
}

/// Everything a candidate report needs, gathered so the verdict reads as one thing.
struct Verdict<'a> {
    site: &'a str,
    capacity: &'a Value,
    eligible: bool,
    warm: u64,
    shortfall: Value,
    reason: Option<String>,
}

fn report(verdict: Verdict<'_>) -> Value {
    let Verdict {
        site,
        capacity,
        eligible,
        warm,
        shortfall,
        reason,
    } = verdict;
    json!({
        "site": site,
        "eligible": eligible,
        "warm": warm,
        "free": capacity.get("free").cloned().unwrap_or(Value::Null),
        "total": capacity.get("total").cloned().unwrap_or(Value::Null),
        "systems": capacity.get("systems").cloned().unwrap_or(Value::Null),
        "shortfall": shortfall,
        "reason": reason.map_or(Value::Null, Value::String)
    })
}

/// Pick the winner: most normalised headroom, then a warm slot, then declared order.
///
/// Declaration order as the final tiebreak keeps the choice reproducible, so a
/// placement can be explained after the fact rather than merely observed.
pub(super) fn choose(candidates: &[Candidate]) -> Option<&Candidate> {
    candidates
        .iter()
        .filter(|candidate| candidate.eligible)
        .enumerate()
        .max_by(|(left_index, left), (right_index, right)| {
            let left_key = (left.headroom.unwrap_or(0), u64::from(left.warm > 0));
            let right_key = (right.headroom.unwrap_or(0), u64::from(right.warm > 0));
            left_key.cmp(&right_key).then(right_index.cmp(left_index))
        })
        .map(|(_, candidate)| candidate)
}
