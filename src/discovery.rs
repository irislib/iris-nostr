//! Discovery helpers over query results; no separate tag database or network.
use nostr::Event;
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet};

/// Needles must be lowercase. Empty alternatives match every event.
pub fn matches_text(event: &Event, needles: &[String]) -> bool {
    needles.is_empty()
        || needles.iter().any(|needle| {
            event.content.to_lowercase().contains(needle)
                || event.tags.iter().any(|tag| {
                    let values = tag.as_slice();
                    matches!(
                        values.first().map(String::as_str),
                        Some("title" | "summary" | "alt" | "t" | "category" | "location")
                    ) && values
                        .get(1)
                        .is_some_and(|value| value.to_lowercase().contains(needle))
                })
        })
}

#[derive(Serialize)]
pub struct TagCount {
    pub tag: String,
    /// Exact observed spellings, suitable for case-sensitive relay filters.
    pub values: Vec<String>,
    pub authors: usize,
    pub events: usize,
}

/// Count t tags in a verified sample, grouping case variants. Full author keys
/// and event IDs prevent repeated tags/posts from inflating distinct authors.
pub fn tag_counts(events: &[Event], min_authors: usize) -> Vec<TagCount> {
    #[derive(Default)]
    struct Counts {
        values: BTreeSet<String>,
        authors: HashSet<nostr::PublicKey>,
        events: usize,
    }
    let mut counts: HashMap<String, Counts> = HashMap::new();
    let mut seen_events = HashSet::new();
    for event in events {
        if !seen_events.insert(event.id) {
            continue;
        }
        let mut seen_tags = HashSet::new();
        for tag in event.tags.iter() {
            let values = tag.as_slice();
            if values.first().is_none_or(|v| v != "t") {
                continue;
            }
            let Some(value) = values.get(1).filter(|v| !v.trim().is_empty()) else {
                continue;
            };
            let normalized = value.to_lowercase();
            let entry = counts.entry(normalized.clone()).or_default();
            entry.values.insert(value.clone());
            entry.authors.insert(event.pubkey);
            if seen_tags.insert(normalized) {
                entry.events += 1;
            }
        }
    }
    let mut result = counts
        .into_iter()
        .filter_map(|(tag, counts)| {
            (counts.authors.len() >= min_authors).then(|| TagCount {
                tag,
                values: counts.values.into_iter().collect(),
                authors: counts.authors.len(),
                events: counts.events,
            })
        })
        .collect::<Vec<_>>();
    result.sort_by(|a, b| {
        b.authors
            .cmp(&a.authors)
            .then_with(|| b.events.cmp(&a.events))
            .then_with(|| a.tag.cmp(&b.tag))
    });
    result
}
