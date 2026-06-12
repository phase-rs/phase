//! DecisionTrace capture + aggregation for `--suite` runs.
//!
//! The tactical search emits `tracing::debug!` events on the
//! `phase_ai::decision_trace` target with fields `ai_player`, `top_policies`,
//! `rejects` (see `crate::search::emit_trace_for_candidate`). When the target
//! is disabled (the default), emission is gated by `tracing::event_enabled!`
//! and costs nothing — the policy registry isn't even consulted.
//!
//! `--suite` opts into collection by installing a `CaptureLayer` with an
//! env filter that enables the target at DEBUG. Between matchups, the
//! accumulated events are drained, parsed, and aggregated into
//! `PolicyAttribution` per player, which is then attached to each
//! `MatchupResult` in the JSON report.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tracing::field::{Field, Visit};
use tracing::Subscriber;
use tracing_subscriber::layer::{Context, Layer};

/// A single decision-trace event parsed into structured fields. One event
/// corresponds to one tactical decision by one player.
#[derive(Debug, Clone, Default)]
pub struct RawEvent {
    pub ai_player: u32,
    /// Debug-formatted `Vec<String>` rendered by the tracing macro — each
    /// entry is `"PolicyId:kind=±N.NNN[(\"key\",N),...]"`.
    pub top_policies: String,
    /// Debug-formatted `Vec<String>` — each entry is
    /// `"PolicyId:kind[(\"key\",N),...]"`.
    pub rejects: String,
}

/// Per-player aggregation of trace events within a single matchup run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PolicyAttribution {
    /// `PolicyId` debug-name → reject count across all games.
    pub rejects: HashMap<String, u64>,
    /// Top-N policies by mean `|delta|`.
    pub top_scores: Vec<ScoreEntry>,
    /// Total decision-trace events captured — 0 means tracing was off.
    pub decisions: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreEntry {
    pub policy_id: String,
    pub kind: String,
    pub mean_delta: f64,
    pub occurrences: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MatchupAttribution {
    pub p0: PolicyAttribution,
    pub p1: PolicyAttribution,
}

/// `tracing_subscriber::Layer` that captures `phase_ai::decision_trace`
/// events into a shared buffer. The same layer is installed once for the
/// whole suite run; between matchups, `drain()` returns all accumulated
/// events and clears the buffer.
#[derive(Clone, Default)]
pub struct CaptureLayer {
    entries: Arc<Mutex<Vec<RawEvent>>>,
}

impl CaptureLayer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn drain(&self) -> Vec<RawEvent> {
        let mut guard = self.entries.lock().expect("capture mutex poisoned");
        std::mem::take(&mut *guard)
    }
}

struct EventVisitor {
    ai_player: u32,
    top_policies: String,
    rejects: String,
    combat_kind: String,
}

impl Visit for EventVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "combat_kind" {
            self.combat_kind = value.to_string();
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "top_policies" => self.top_policies = format!("{value:?}"),
            "rejects" => self.rejects = format!("{value:?}"),
            "combat_kind" => self.combat_kind = format!("{value:?}"),
            _ => {}
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == "ai_player" {
            self.ai_player = value as u32;
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        if field.name() == "ai_player" && value >= 0 {
            self.ai_player = value as u32;
        }
    }
}

impl<S> Layer<S> for CaptureLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        if event.metadata().target() != "phase_ai::decision_trace" {
            return;
        }
        let mut visitor = EventVisitor {
            ai_player: u32::MAX,
            top_policies: String::new(),
            rejects: String::new(),
            combat_kind: String::new(),
        };
        event.record(&mut visitor);
        if !visitor.combat_kind.is_empty() {
            // Combat AI emits on the shared decision_trace target for raw-log
            // observability, but suite policy attribution should count only
            // tactical policy-registry decisions.
            return;
        }
        if visitor.ai_player == u32::MAX {
            // Event lacks ai_player (e.g. mulligan decisions emit their own
            // format). Skip — we can't attribute it to a player.
            return;
        }
        self.entries
            .lock()
            .expect("capture mutex poisoned")
            .push(RawEvent {
                ai_player: visitor.ai_player,
                top_policies: visitor.top_policies,
                rejects: visitor.rejects,
            });
    }
}

/// Aggregate a batch of raw events into a `MatchupAttribution`. Events are
/// partitioned by `ai_player`, parsed, and reduced into per-policy counts
/// and mean deltas.
pub fn aggregate_events(events: &[RawEvent]) -> MatchupAttribution {
    let mut p0 = PolicyAttribution::default();
    let mut p1 = PolicyAttribution::default();
    let mut p0_scores: HashMap<(String, String), (f64, u64)> = HashMap::new();
    let mut p1_scores: HashMap<(String, String), (f64, u64)> = HashMap::new();

    for event in events {
        let (attribution, scores_acc) = match event.ai_player {
            0 => (&mut p0, &mut p0_scores),
            1 => (&mut p1, &mut p1_scores),
            _ => continue,
        };
        attribution.decisions += 1;

        for reject in parse_entry_list(&event.rejects) {
            if let Some((policy_id, _kind)) = parse_reject_entry(&reject) {
                *attribution.rejects.entry(policy_id).or_insert(0) += 1;
            }
        }

        for score in parse_entry_list(&event.top_policies) {
            if let Some((policy_id, kind, delta)) = parse_score_entry(&score) {
                let key = (policy_id, kind);
                let slot = scores_acc.entry(key).or_insert((0.0, 0));
                slot.0 += delta;
                slot.1 += 1;
            }
        }
    }

    p0.top_scores = collect_top_scores(p0_scores, 3);
    p1.top_scores = collect_top_scores(p1_scores, 3);

    MatchupAttribution { p0, p1 }
}

fn collect_top_scores(
    scores_acc: HashMap<(String, String), (f64, u64)>,
    top_n: usize,
) -> Vec<ScoreEntry> {
    let mut entries: Vec<ScoreEntry> = scores_acc
        .into_iter()
        .map(|((policy_id, kind), (sum, count))| {
            let mean_delta = if count > 0 { sum / count as f64 } else { 0.0 };
            ScoreEntry {
                policy_id,
                kind,
                mean_delta,
                occurrences: count,
            }
        })
        .collect();
    entries.sort_by(|a, b| {
        b.mean_delta
            .abs()
            .partial_cmp(&a.mean_delta.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    entries.truncate(top_n);
    entries
}

/// The `top_policies` / `rejects` fields arrive as a Debug-formatted
/// `Vec<String>`: `"[\"A\", \"B\"]"`. Strip the outer brackets, split on
/// `", "`, and trim surrounding quotes.
fn parse_entry_list(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    let Some(inner) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) else {
        return Vec::new();
    };
    if inner.trim().is_empty() {
        return Vec::new();
    }
    inner
        .split("\", \"")
        .map(|s| {
            s.trim_start_matches('"')
                .trim_end_matches('"')
                .trim_start_matches("\\\"")
                .trim_end_matches("\\\"")
                .to_string()
        })
        .collect()
}

/// Parse a score-line token like `"LandfallTiming:fetch_before_payoff=-0.800[(\"count\", 1)]"`
/// into (policy_id, kind, delta). Facts are ignored for aggregation.
fn parse_score_entry(s: &str) -> Option<(String, String, f64)> {
    let (policy_id, rest) = s.split_once(':')?;
    let (kind, rest) = rest.split_once('=')?;
    let (delta_str, _facts) = rest.split_once('[').unwrap_or((rest, ""));
    let delta: f64 = delta_str.trim().parse().ok()?;
    Some((policy_id.to_string(), kind.to_string(), delta))
}

/// Parse a reject-line token like `"AntiSelfHarm:self_damage[(\"hp\", 5)]"`
/// into (policy_id, kind). Facts are ignored.
fn parse_reject_entry(s: &str) -> Option<(String, String)> {
    let (policy_id, rest) = s.split_once(':')?;
    let (kind, _facts) = rest.split_once('[').unwrap_or((rest, ""));
    Some((policy_id.to_string(), kind.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_score_extracts_fields() {
        let s = "LandfallTiming:fetch_before_payoff=-0.800[(\"count\", 1)]";
        let (id, kind, delta) = parse_score_entry(s).unwrap();
        assert_eq!(id, "LandfallTiming");
        assert_eq!(kind, "fetch_before_payoff");
        assert!((delta - -0.800).abs() < 1e-9);
    }

    #[test]
    fn parse_reject_extracts_fields() {
        let s = "AntiSelfHarm:self_damage[(\"hp\", 5)]";
        let (id, kind) = parse_reject_entry(s).unwrap();
        assert_eq!(id, "AntiSelfHarm");
        assert_eq!(kind, "self_damage");
    }

    #[test]
    fn parse_reject_without_facts() {
        let s = "AntiSelfHarm:self_damage";
        let (id, kind) = parse_reject_entry(s).unwrap();
        assert_eq!(id, "AntiSelfHarm");
        assert_eq!(kind, "self_damage");
    }

    #[test]
    fn parse_score_without_facts() {
        let s = "TempoCurve:push=+0.500";
        let (id, kind, delta) = parse_score_entry(s).unwrap();
        assert_eq!(id, "TempoCurve");
        assert_eq!(kind, "push");
        assert!((delta - 0.5).abs() < 1e-9);
    }

    #[test]
    fn parse_entry_list_unwraps_vec_debug() {
        // This mirrors the Debug output of `Vec<String>` — tracing's `?`
        // formatter emits `["a", "b"]` where each String is quoted.
        let raw = r#"["LandfallTiming:fetch=-0.8", "TempoCurve:push=+0.5"]"#;
        let items = parse_entry_list(raw);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], "LandfallTiming:fetch=-0.8");
        assert_eq!(items[1], "TempoCurve:push=+0.5");
    }

    #[test]
    fn parse_entry_list_empty_is_empty() {
        assert!(parse_entry_list("[]").is_empty());
    }

    #[test]
    fn aggregator_groups_by_player() {
        let events = vec![
            RawEvent {
                ai_player: 0,
                top_policies: r#"["TempoCurve:push=+0.500"]"#.to_string(),
                rejects: "[]".to_string(),
            },
            RawEvent {
                ai_player: 1,
                top_policies: r#"["TempoCurve:push=+0.400"]"#.to_string(),
                rejects: "[]".to_string(),
            },
        ];
        let att = aggregate_events(&events);
        assert_eq!(att.p0.decisions, 1);
        assert_eq!(att.p1.decisions, 1);
        assert_eq!(att.p0.top_scores.len(), 1);
        assert_eq!(att.p1.top_scores.len(), 1);
        assert!((att.p0.top_scores[0].mean_delta - 0.5).abs() < 1e-9);
    }

    #[test]
    fn aggregator_rejects_counted_per_policy() {
        let events = vec![
            RawEvent {
                ai_player: 0,
                top_policies: "[]".to_string(),
                rejects: r#"["AntiSelfHarm:self_damage", "AntiSelfHarm:self_damage"]"#.to_string(),
            },
            RawEvent {
                ai_player: 0,
                top_policies: "[]".to_string(),
                rejects: r#"["LethalityAwareness:no_lethal"]"#.to_string(),
            },
        ];
        let att = aggregate_events(&events);
        assert_eq!(att.p0.rejects.get("AntiSelfHarm").copied(), Some(2));
        assert_eq!(att.p0.rejects.get("LethalityAwareness").copied(), Some(1));
    }

    #[test]
    fn aggregator_tops_by_abs_mean_delta() {
        // Three policies with different mean |delta|: 0.9, 0.3, 0.6.
        let events = vec![RawEvent {
            ai_player: 0,
            top_policies: r#"["A:x=+0.900", "B:x=-0.300", "C:x=+0.600"]"#.to_string(),
            rejects: "[]".to_string(),
        }];
        let att = aggregate_events(&events);
        assert_eq!(att.p0.top_scores[0].policy_id, "A");
        assert_eq!(att.p0.top_scores[1].policy_id, "C");
        assert_eq!(att.p0.top_scores[2].policy_id, "B");
    }

    #[test]
    fn zero_events_yields_empty_attribution() {
        let att = aggregate_events(&[]);
        assert_eq!(att.p0.decisions, 0);
        assert!(att.p0.rejects.is_empty());
        assert!(att.p0.top_scores.is_empty());
    }
}
