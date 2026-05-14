use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::identifiers::ObjectId;
use super::layers::Layer;

/// Identifies a single `ContinuousModification` that contributed to an
/// object's current characteristics, for source-attribution display.
///
/// Layer effects originate from one of two places:
/// - A `StaticDefinition` on a permanent in a tracked zone. The source object
///   stays addressable via `state.objects[source]` while its static ability
///   is active. Most statics function only on the battlefield (CR 113.6c-d),
///   but some function from hand, graveyard, exile, or the command zone
///   (CR 113.6 + per-mechanic carve-outs for flashback / dredge / foretell /
///   companion). The FE must therefore search any tracked zone, not just
///   the battlefield, when resolving `source`.
/// - A `TransientContinuousEffect` created by a resolving spell or ability.
///   The originating object usually moves to the graveyard with a new
///   ObjectId after resolution (CR 400.7), so the `TransientContinuousEffect`
///   carries its own snapshotted `source_name` and is addressed by stable
///   `id` here.
///
/// `mod_index` points into the source's `modifications` vector
/// (`StaticDefinition.modifications` / `TransientContinuousEffect.modifications`).
/// Without it, a multi-modification source (Akroma's Memorial: flying + first
/// strike + vigilance + …) would record indistinguishable entries; with it,
/// the FE can look up the exact `ContinuousModification` and render the
/// correct grant/remove verb plus payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum EffectRef {
    /// Runtime-generated effect from a resolved spell or ability.
    /// Resolves via `state.transient_continuous_effects[id].modifications[mod_index]`.
    Transient { id: u64, mod_index: usize },
    /// Intrinsic static ability on a tracked-zone permanent.
    /// Resolves via `state.objects[source].static_definitions[def_index].modifications[mod_index]`.
    Static {
        source: ObjectId,
        def_index: usize,
        mod_index: usize,
    },
}

/// Per-object record of which continuous effects contributed to its current
/// characteristics during the most recent layers pass.
///
/// Rebuilt fresh on every layers evaluation alongside the other derived state
/// (keywords, abilities, P/T). The frontend reads this to render attribution
/// tooltips ("Flying — from Akroma's Memorial") without inferring source from
/// name-diffing. Game logic never reads this; it's display metadata only.
///
/// Excluded from this side-table by design:
/// - CR 613.4c counter-derived P/T modifications (layer 7e) — counters are
///   not applied via `ContinuousModification` instances; counter sourcing
///   belongs to the event log.
/// - CR 122.1b + CR 613.1f keyword-counter promotions (e.g., +1/+1 counters
///   from `KeywordCounter(Flying)`) — applied directly from `obj.counters`
///   without an intermediating `ContinuousModification`; same event-log
///   rationale as P/T.
/// - CR 510 combat-damage assignment rule effects ("assigns no combat
///   damage", "assigns damage equal to toughness") — applied by a separate
///   pipeline (`apply_combat_assignment_rule_effects`) and not yet
///   instrumented. Follow-up.
///
/// Within a single layer's `Vec<EffectRef>`, entries accumulate in
/// CR 613.7 timestamp order because the outer apply loop in
/// `evaluate_layers` iterates effects already sorted by timestamp /
/// dependency order before each call into `apply_continuous_effect`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ObjectAttribution {
    /// Layer-pipeline grants, partitioned by CR 613 layer. BTreeMap gives
    /// deterministic CR-order iteration without sorting at every read.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub by_layer: BTreeMap<Layer, Vec<EffectRef>>,
}

impl ObjectAttribution {
    pub fn is_empty(&self) -> bool {
        self.by_layer.is_empty()
    }

    pub fn record_layer(&mut self, layer: Layer, effect: EffectRef) {
        self.by_layer.entry(layer).or_default().push(effect);
    }
}
