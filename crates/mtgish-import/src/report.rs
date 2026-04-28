//! Diagnostic accumulator for unsupported mtgish variants.
//!
//! The translator is a fan-in: many phrasal `Rule` variants collapse onto
//! a smaller set of `AbilityDefinition` shapes. Anything we don't yet
//! handle becomes `Effect::Unimplemented` at runtime — same fallback the
//! native `oracle_nom` parser uses — but we also record it here so the
//! work queue is visible.

use std::collections::BTreeMap;

#[derive(Default)]
pub struct ImportReport {
    /// variant path (e.g. "Rule::TriggerI/Trigger/WhenAPlayerCycles") → cards-affected count
    pub unsupported: BTreeMap<String, UnsupportedStat>,
    pub cards_total: usize,
    pub cards_with_unsupported: usize,
}

#[derive(Default, Debug, serde::Serialize)]
pub struct UnsupportedStat {
    pub count: usize,
    pub example_cards: Vec<String>,
}

impl ImportReport {
    pub fn record(&mut self, path: &str, card: &str) {
        let entry = self.unsupported.entry(path.to_string()).or_default();
        entry.count += 1;
        if entry.example_cards.len() < 3 && !entry.example_cards.iter().any(|c| c == card) {
            entry.example_cards.push(card.to_string());
        }
    }

    pub fn summary_json(&self) -> serde_json::Value {
        let mut ranked: Vec<_> = self
            .unsupported
            .iter()
            .map(|(k, v)| (k.clone(), v.count, v.example_cards.clone()))
            .collect();
        ranked.sort_by_key(|x| std::cmp::Reverse(x.1));
        serde_json::json!({
            "cards_total": self.cards_total,
            "cards_with_unsupported": self.cards_with_unsupported,
            "unsupported_variants": ranked.iter().map(|(path, count, ex)| {
                serde_json::json!({"path": path, "count": count, "examples": ex})
            }).collect::<Vec<_>>(),
        })
    }
}

/// Per-card translation context — accumulates unsupported-variant paths.
pub struct Ctx<'a> {
    pub card_name: String,
    pub report: &'a mut ImportReport,
    saw_unsupported: bool,
}

impl<'a> Ctx<'a> {
    pub fn new(card_name: String, report: &'a mut ImportReport) -> Self {
        Self {
            card_name,
            report,
            saw_unsupported: false,
        }
    }

    /// Record an unsupported variant. Path is a "/"-joined breadcrumb of
    /// nested enum variant names (e.g. "Rule::TriggerI/Trigger/Foo").
    pub fn unsupported(&mut self, path: &str) {
        self.report.record(path, &self.card_name);
        self.saw_unsupported = true;
    }

    pub fn finish(self) -> bool {
        if self.saw_unsupported {
            self.report.cards_with_unsupported += 1;
        }
        self.saw_unsupported
    }
}
