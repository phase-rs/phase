//! Avoid feeding a Chalice of the Void.
//!
//! CR 601.2i: A "whenever a player casts a spell …" ability triggers when the
//! spell is cast. CR 701.6a: countering a spell removes it from the stack — it
//! doesn't resolve and goes to its owner's graveyard, with no cost refund
//! (CR 701.6b). Chalice of the Void's trigger ("Whenever a player casts a spell
//! with mana value equal to the number of charge counters on this artifact,
//! counter that spell") therefore eats any spell whose mana value (CR 202.3)
//! matches the artifact's charge-counter count — including the controller's own.
//!
//! This policy detects that *class* of permanent structurally (not by name) and
//! demotes casting a spell whose mana value the Chalice would match:
//!   - own Chalice: a self-counter is pure tempo and card loss → heavy penalty.
//!   - opponent's Chalice: usually bad, but the spell may still be worth baiting
//!     or simply better than passing → lighter demotion, never a hard veto.
//!
//! The Chalice-class signature is a single `TriggerDefinition` whose `mode` is
//! `SpellCast` (or `SpellCastOrCopy`), whose `execute` effect chain contains
//! `Effect::Counter`, and whose `valid_card` is a `Typed` filter carrying
//! `FilterProp::Cmc { comparator: EQ, value: Ref(CountersOn { scope: Source,
//! counter_type }) }`.
//!
//! Parameterizing on the counter type and its live count covers any spell mana
//! value and any current/future card with this structure — not a single card.

use engine::game::functioning_abilities::{
    active_static_definitions, is_self_referential_prohibition,
};
use engine::game::static_abilities::{check_static_ability, StaticCheckContext};
use engine::types::ability::{
    AbilityDefinition, Comparator, Effect, FilterProp, ObjectScope, QuantityExpr, QuantityRef,
    TargetFilter, TriggerEntry,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::DeckFeatures;

pub struct ChaliceAvoidancePolicy;

/// One live Chalice-class permanent and the mana value it currently counters.
struct ChaliceMatch {
    /// Mana value that this permanent's trigger counters right now — the count
    /// of its charge-class counters (CR 202.3 vs CR 122.1).
    countered_mana_value: u32,
    /// `true` when the AI controls this permanent (self-counter = pure loss).
    own: bool,
}

impl ChaliceAvoidancePolicy {
    pub fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        // Only at cast time — countering happens on cast (CR 601.2i), so the
        // decision is whether to put the spell on the stack at all.
        let GameAction::CastSpell { .. } = ctx.candidate.action else {
            return 0.0;
        };
        let Some(spell) = ctx.source_object() else {
            return 0.0;
        };
        if !spell_can_be_countered(ctx.state, spell.id) {
            return 0.0;
        }
        let spell_mana_value = spell.mana_cost.mana_value();

        // Pick the worst applicable Chalice: an own Chalice that matches is the
        // strongest signal; otherwise an opponent's matching Chalice still
        // demotes. Either way the spell's mana value must equal the count.
        let mut worst = 0.0_f64;
        for chalice in chalice_matches(ctx.state, ctx.ai_player) {
            if chalice.countered_mana_value != spell_mana_value {
                continue;
            }
            let penalty = if chalice.own {
                ctx.penalties().own_chalice_counter_penalty
            } else {
                ctx.penalties().opponent_chalice_counter_penalty
            };
            // Keep the most negative (own Chalice dominates an opponent's).
            worst = worst.min(penalty);
        }
        worst
    }
}

impl TacticalPolicy for ChaliceAvoidancePolicy {
    fn id(&self) -> PolicyId {
        PolicyId::ChaliceAvoidance
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::CastSpell]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        state: &GameState,
        player: PlayerId,
    ) -> Option<f32> {
        // Opt out entirely unless a Chalice-class permanent is on the
        // battlefield — this is a board-state concern, not a deck-archetype one,
        // so the gate is the presence of the matching permanent rather than a
        // commitment score.
        chalice_matches(state, player)
            .next()
            // activation-constant: board-state gate; weight lives in the penalty.
            .map(|_| 1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        PolicyVerdict::Score {
            delta: self.score(ctx),
            reason: PolicyReason::new("chalice_self_counter"),
        }
    }
}

fn spell_can_be_countered(
    state: &GameState,
    spell_id: engine::types::identifiers::ObjectId,
) -> bool {
    let ctx = StaticCheckContext {
        source_id: Some(spell_id),
        target_id: Some(spell_id),
        ..Default::default()
    };
    if check_static_ability(state, StaticMode::CantBeCountered, &ctx) {
        return false;
    }
    state.objects.get(&spell_id).is_none_or(|obj| {
        !active_static_definitions(state, obj).any(|sd| sd.mode == StaticMode::CantBeCountered)
            // CR 113.6g: this policy scores the cast before the card moves to
            // the stack, so predict the spell's own future stack-functioning
            // self-prohibition instead of asking whether it functions in hand.
            && !obj.static_definitions.iter_unchecked().any(|sd| {
                sd.mode == StaticMode::CantBeCountered
                    && is_self_referential_prohibition(sd)
                    && (sd.active_zones.is_empty() || sd.active_zones.contains(&Zone::Stack))
            })
    })
}

/// Iterate every Chalice-class permanent on the battlefield, paired with the
/// mana value it currently counters and whether `viewer` controls it.
fn chalice_matches<'a>(
    state: &'a GameState,
    viewer: PlayerId,
) -> impl Iterator<Item = ChaliceMatch> + 'a {
    state.battlefield.iter().filter_map(move |id| {
        let obj = state.objects.get(id)?;
        let counter_type = chalice_counter_type(obj.trigger_definitions.as_slice())?;
        // CR 122.1: the live count of the matching counter is the mana value the
        // trigger currently counters. Zero counters means it counters mana value
        // 0 (free spells) — still a valid match, so don't filter it out.
        let countered_mana_value = obj.counters.get(&counter_type).copied().unwrap_or(0);
        Some(ChaliceMatch {
            countered_mana_value,
            own: obj.controller == viewer,
        })
    })
}

/// Structurally classify a permanent as Chalice-of-the-Void-class: a spell-cast
/// trigger that counters the cast spell when its mana value equals the count of
/// some counter type on this permanent. Returns that counter type so the caller
/// can read the live count. Covers any card with this shape — not just Chalice.
fn chalice_counter_type(triggers: &[TriggerEntry]) -> Option<CounterType> {
    triggers
        .iter()
        .map(|entry| &entry.definition)
        .find_map(|trigger| {
            if !matches!(
                trigger.mode,
                TriggerMode::SpellCast | TriggerMode::SpellCastOrCopy
            ) {
                return None;
            }
            // CR 701.6a: the trigger must actually counter the spell.
            if !trigger
                .execute
                .as_deref()
                .is_some_and(ability_counters_spell)
            {
                return None;
            }
            // CR 202.3 + CR 122.1: the cast filter must gate on mana value equal to
            // the count of one of this permanent's own counters.
            trigger
                .valid_card
                .as_ref()
                .and_then(filter_counter_type_for_cmc_eq_self_counters)
        })
}

/// True when an ability's effect chain counters a spell/ability (CR 701.6).
fn ability_counters_spell(ability: &AbilityDefinition) -> bool {
    let mut current = Some(ability);
    while let Some(def) = current {
        if matches!(&*def.effect, Effect::Counter { .. }) {
            return true;
        }
        current = def.sub_ability.as_deref();
    }
    false
}

/// If `filter` is a `Typed` filter carrying `Cmc EQ <count of one of this
/// permanent's own counter types>`, return that counter type. The
/// `ObjectScope::Source` constraint ensures the comparison is against *this*
/// permanent's counters (CR 113.7), so the trigger self-references and the
/// counted mana value is the artifact's own charge count.
fn filter_counter_type_for_cmc_eq_self_counters(filter: &TargetFilter) -> Option<CounterType> {
    let TargetFilter::Typed(typed) = filter else {
        return None;
    };
    typed.properties.iter().find_map(|prop| match prop {
        FilterProp::Cmc {
            comparator: Comparator::EQ,
            value:
                QuantityExpr::Ref {
                    qty:
                        QuantityRef::CountersOn {
                            scope: ObjectScope::Source,
                            counter_type: Some(counter_type),
                        },
                },
        } => Some(counter_type.clone()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, StaticDefinition, TriggerDefinition, TypedFilter,
    };
    use engine::types::card_type::CoreType;
    use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
    use engine::types::identifiers::CardId;
    use engine::types::mana::ManaCost;
    use engine::types::statics::StaticMode;
    use engine::types::triggers::TriggerMode;
    use engine::types::zones::Zone;

    const AI: PlayerId = PlayerId(0);
    const OPP: PlayerId = PlayerId(1);

    fn charge() -> CounterType {
        CounterType::Generic("charge".to_string())
    }

    /// Build a Chalice-of-the-Void-class artifact controlled by `owner` with
    /// `charge_count` charge counters.
    fn add_chalice(state: &mut GameState, owner: PlayerId, charge_count: u32) {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            "Chalice of the Void".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.counters.insert(charge(), charge_count);
        let trigger = TriggerDefinition::new(TriggerMode::SpellCast)
            .valid_card(TargetFilter::Typed(TypedFilter {
                type_filters: Vec::new(),
                controller: None,
                properties: vec![FilterProp::Cmc {
                    comparator: Comparator::EQ,
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::CountersOn {
                            scope: ObjectScope::Source,
                            counter_type: Some(charge()),
                        },
                    },
                }],
            }))
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Counter {
                    target: TargetFilter::Any,
                    source_rider: None,
                    countered_spell_zone: None,
                },
            ));
        obj.trigger_definitions.push(trigger);
    }

    /// Build a cast candidate for a spell with the given mana value, owned by AI.
    fn cast_candidate(
        state: &mut GameState,
        mana_value: u32,
    ) -> (AiDecisionContext, CandidateAction) {
        let card_id = CardId(state.next_object_id);
        let spell_id = create_object(state, card_id, AI, "Spell".to_string(), Zone::Hand);
        let obj = state.objects.get_mut(&spell_id).unwrap();
        obj.card_types.core_types.push(CoreType::Instant);
        obj.mana_cost = ManaCost::Cost {
            shards: Vec::new(),
            generic: mana_value,
        };
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id,
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata {
                actor: Some(AI),
                tactical_class: TacticalClass::Spell,
            },
        };
        (decision, candidate)
    }

    fn score(state: &GameState, decision: &AiDecisionContext, candidate: &CandidateAction) -> f64 {
        let config = AiConfig::default();
        let ctx = PolicyContext {
            state,
            decision,
            candidate,
            ai_player: AI,
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        ChaliceAvoidancePolicy.score(&ctx)
    }

    /// Pre-policy baseline: without the gate, the spell is a legal cast that the
    /// AI would happily play. The discriminating signal is that the policy turns
    /// that into a negative score once an own Chalice matches the mana value.
    #[test]
    fn avoids_casting_into_own_chalice() {
        let mut state = GameState::new_two_player(0);
        add_chalice(&mut state, AI, 2);
        let (decision, candidate) = cast_candidate(&mut state, 2);
        let delta = score(&state, &decision, &candidate);
        assert!(
            delta < -5.0,
            "casting MV-2 into own 2-charge Chalice must be strongly demoted, got {delta}"
        );
    }

    /// Self-harden: no Chalice on the board → no penalty (the policy must not
    /// over-fire on ordinary casts).
    #[test]
    fn no_penalty_without_chalice() {
        let mut state = GameState::new_two_player(0);
        let (decision, candidate) = cast_candidate(&mut state, 2);
        assert_eq!(score(&state, &decision, &candidate), 0.0);
    }

    /// Self-harden: a Chalice is out, but the spell's mana value doesn't match
    /// the charge count → not countered, so no penalty.
    #[test]
    fn no_penalty_when_mana_value_differs() {
        let mut state = GameState::new_two_player(0);
        add_chalice(&mut state, AI, 2);
        let (decision, candidate) = cast_candidate(&mut state, 3);
        assert_eq!(score(&state, &decision, &candidate), 0.0);
    }

    /// An opponent's matching Chalice demotes the cast, but less than an own
    /// Chalice — the AI may still want the spell on the stack.
    #[test]
    fn opponent_chalice_demotes_less_than_own() {
        let mut own_state = GameState::new_two_player(0);
        add_chalice(&mut own_state, AI, 1);
        let (own_dec, own_cand) = cast_candidate(&mut own_state, 1);
        let own_delta = score(&own_state, &own_dec, &own_cand);

        let mut opp_state = GameState::new_two_player(0);
        add_chalice(&mut opp_state, OPP, 1);
        let (opp_dec, opp_cand) = cast_candidate(&mut opp_state, 1);
        let opp_delta = score(&opp_state, &opp_dec, &opp_cand);

        assert!(opp_delta < 0.0, "opponent Chalice should still demote");
        assert!(
            own_delta < opp_delta,
            "own Chalice ({own_delta}) must be worse than opponent's ({opp_delta})"
        );
    }

    /// Class coverage: a Chalice with zero charge counters counters free (MV-0)
    /// spells, and the policy must catch that boundary too.
    #[test]
    fn matches_mana_value_zero_chalice() {
        let mut state = GameState::new_two_player(0);
        add_chalice(&mut state, AI, 0);
        let (decision, candidate) = cast_candidate(&mut state, 0);
        assert!(score(&state, &decision, &candidate) < -5.0);
    }

    /// CR 101.2: A spell that can't be countered is not eaten by Chalice, so the
    /// AI must not demote casting it solely because its mana value matches.
    #[test]
    fn uncounterable_spell_is_not_penalized() {
        let mut state = GameState::new_two_player(0);
        add_chalice(&mut state, AI, 2);
        let (decision, candidate) = cast_candidate(&mut state, 2);
        let spell_id = match &candidate.action {
            GameAction::CastSpell { object_id, .. } => *object_id,
            _ => unreachable!("test builds a cast candidate"),
        };
        state
            .objects
            .get_mut(&spell_id)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantBeCountered));

        assert_eq!(score(&state, &decision, &candidate), 0.0);
    }

    /// Activation gates the policy off entirely when no Chalice is present and
    /// on when one is.
    #[test]
    fn activation_gates_on_chalice_presence() {
        let features = DeckFeatures::default();

        let empty = GameState::new_two_player(0);
        assert!(ChaliceAvoidancePolicy
            .activation(&features, &empty, AI)
            .is_none());

        let mut with_chalice = GameState::new_two_player(0);
        add_chalice(&mut with_chalice, AI, 2);
        assert_eq!(
            ChaliceAvoidancePolicy.activation(&features, &with_chalice, AI),
            Some(1.0)
        );
    }

    /// End-to-end: the policy is wired into the default registry and emits a
    /// negative `ChaliceAvoidance` verdict for a self-counter cast.
    #[test]
    fn wired_into_registry() {
        use super::super::registry::PolicyRegistry;
        let mut state = GameState::new_two_player(0);
        add_chalice(&mut state, AI, 2);
        let (decision, candidate) = cast_candidate(&mut state, 2);
        let config = AiConfig::default();
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        let registry = PolicyRegistry::default();
        let fired = registry.verdicts(&ctx).into_iter().any(|(id, v)| {
            matches!(id, PolicyId::ChaliceAvoidance)
                && matches!(v, PolicyVerdict::Score { delta, .. } if delta < 0.0)
        });
        assert!(
            fired,
            "ChaliceAvoidance must fire negatively via the registry"
        );
    }

    /// Build-for-the-class guard: a permanent whose spell-cast trigger does NOT
    /// counter (e.g. it draws) must not be classified as a Chalice.
    #[test]
    fn non_countering_spell_trigger_is_not_chalice() {
        let mut state = GameState::new_two_player(0);
        let card_id = CardId(state.next_object_id);
        let id = create_object(
            &mut state,
            card_id,
            AI,
            "Decoy".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.counters.insert(charge(), 2);
        let trigger = TriggerDefinition::new(TriggerMode::SpellCast)
            .valid_card(TargetFilter::Typed(TypedFilter {
                type_filters: Vec::new(),
                controller: None,
                properties: vec![FilterProp::Cmc {
                    comparator: Comparator::EQ,
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::CountersOn {
                            scope: ObjectScope::Source,
                            counter_type: Some(charge()),
                        },
                    },
                }],
            }))
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            ));
        obj.trigger_definitions.push(trigger);

        let (decision, candidate) = cast_candidate(&mut state, 2);
        assert_eq!(score(&state, &decision, &candidate), 0.0);
    }
}
