//! Avoid feeding a Chalice-class cast trap.
//!
//! CR 601.2i: A "whenever a player casts a spell …" ability triggers when the
//! spell is cast. CR 701.6a: countering a spell removes it from the stack — it
//! doesn't resolve and goes to its owner's graveyard, with no cost refund
//! (CR 701.6b). A permanent whose spell-cast trigger counters the cast spell
//! therefore eats every spell its gate selects — including, when the trigger
//! carries no caster scope, its own controller's.
//!
//! The class signature is a single `TriggerDefinition` whose `mode` is
//! `SpellCast` (or `SpellCastOrCopy`) and whose `execute` effect chain contains
//! `Effect::Counter`. Which casts it eats is the parameterized axis
//! [`SpellTrapGate`]:
//!   - `ManaValueEqualsCounters` — Chalice of the Void: `valid_card` is a
//!     `Typed` filter carrying `FilterProp::Cmc { comparator: EQ, value:
//!     Ref(CountersOn { scope: Source, counter_type }) }`, so the countered mana
//!     value (CR 202.3) is this permanent's live counter count (CR 122.1).
//!   - `NoManaSpent` — Vexing Bauble, Lavinia, Azorius Renegade: the trigger's
//!     CR 603.4 intervening-`if` `condition` is
//!     `TriggerCondition::ManaSpentCondition` whose text names "no mana was
//!     spent" (CR 601.2h fixes how much was spent at the payment step). Every
//!     free cast (CR 118.9 + CR 107.3b) is eaten, and so is every genuinely
//!     `{0}`-cost cast.
//!
//! The trigger's `valid_target` is its caster scope (CR 603.2): an unscoped
//! trap ("a player") punishes its own controller too, a `Controller`-scoped one
//! punishes only its controller, and an opponent-scoped one (Lavinia) punishes
//! only the other side.
//!
//! Demotion:
//!   - own trap: a self-counter is pure tempo and card loss → heavy penalty.
//!   - opponent's trap: usually bad, but the spell may still be worth baiting
//!     or simply better than passing → lighter demotion, never a hard veto.
//!
//! Parameterizing on the gate, its live inputs and the caster scope covers any
//! current/future card with this structure — not a single card.

use engine::game::functioning_abilities::{
    active_static_definitions, is_self_referential_prohibition,
};
use engine::game::game_object::GameObject;
use engine::game::players::is_opponent;
use engine::game::static_abilities::{check_static_ability, StaticCheckContext};
use engine::types::ability::{
    AbilityDefinition, Comparator, ControllerRef, Effect, FilterProp, ObjectScope, QuantityExpr,
    QuantityRef, TargetFilter, TriggerCondition, TriggerDefinition, TypedFilter,
};
use engine::types::counter::CounterType;
use engine::types::game_state::GameState;
use engine::types::mana::ManaCost;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::cast_facts::{is_cast_family_action, CastCostMode, CastFacts};
use crate::features::DeckFeatures;

pub struct ChaliceAvoidancePolicy;

/// Which casts a Chalice-class trap's spell-cast trigger counters.
///
/// The two shapes are leaf parameterizations of one structure ("whenever a
/// player casts a spell [meeting a condition], counter that spell"): they differ
/// only in where the condition lives on the `TriggerDefinition` and what it
/// reads, so they belong on one axis rather than in two policies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpellTrapGate<'a> {
    /// Chalice of the Void: `valid_card` gates on CR 202.3 mana value equal to
    /// the live count (CR 122.1) of this counter type on the trap itself.
    ManaValueEqualsCounters(&'a CounterType),
    /// Vexing Bauble / Lavinia, Azorius Renegade: the CR 603.4 intervening-`if`
    /// condition is "if no mana was spent to cast it".
    NoManaSpent,
}

/// One live Chalice-class trap on the battlefield, resolved against the AI.
struct ChaliceMatch<'a> {
    /// The cast predicate its spell-cast trigger applies (CR 601.2i).
    gate: SpellTrapGate<'a>,
    /// The trap permanent itself. The `ManaValueEqualsCounters` gate reads its
    /// live counter count from here (CR 122.1) rather than from a snapshot.
    permanent: &'a GameObject,
    /// `true` when the AI controls this permanent (self-counter = pure loss).
    own: bool,
}

impl ChaliceMatch<'_> {
    /// Would this trap counter the cast described by `facts`?
    fn punishes(&self, facts: &CastFacts<'_>) -> bool {
        match self.gate {
            // CR 202.3 + CR 122.1: the countered mana value is the live count of
            // the trap's own charge-class counters. Zero counters means it
            // counters mana value 0 (free spells) — still a valid match, so
            // don't special-case it away.
            SpellTrapGate::ManaValueEqualsCounters(counter_type) => {
                facts.mana_value
                    == self
                        .permanent
                        .counters
                        .get(counter_type)
                        .copied()
                        .unwrap_or(0)
            }
            // Which cost is actually paid decides this, so switch on the cost
            // mode rather than on the printed mana value:
            //   - CR 118.9 + CR 107.3b: "without paying its mana cost" spends no
            //     mana at all, whatever the printed cost says.
            //   - CR 118.9: an alternative cost REPLACES the mana cost, so the
            //     printed mana value says nothing here — a madness `{0}`
            //     (Basking Rootwalla, Blazing Rootwalla, Call to the Netherworld)
            //     spends nothing while a madness `{1}{B}` on a `{0}` card spends
            //     mana.
            //   - CR 601.2h: otherwise the printed cost is what gets paid, so
            //     only a genuinely `{0}` cost spends nothing. An `{X}` cost is
            //     excluded because CR 601.2b announces X before payment and any
            //     X ≥ 1 spends mana.
            //
            // NOT modelled: a cost that CR 601.2f reducers drive down to `{0}`
            // also spends no mana and is countered. Detecting it needs a
            // whole-board cost-reduction and affordability sweep, which is far
            // too expensive for a per-candidate `verdict()`.
            SpellTrapGate::NoManaSpent => match &facts.cost_mode {
                CastCostMode::Free => true,
                CastCostMode::Alternative(cost) => alternative_cost_spends_nothing(cost),
                CastCostMode::Printed => facts.mana_value == 0 && !facts.object.mana_cost.has_x(),
            },
        }
    }
}

/// CR 118.9: does paying this alternative cost genuinely spend no mana?
///
/// `CastCostMode::Alternative` carries the keyword's cost exactly as printed on
/// the object, so the self-referential placeholders are still unresolved here —
/// and `cast_facts` also falls back to `SelfManaCost` when a granted keyword is
/// not on the object. Those resolve to a real cost at payment time, so treating
/// them as `{0}` would invent a penalty; they answer `false`.
fn alternative_cost_spends_nothing(cost: &ManaCost) -> bool {
    match cost {
        ManaCost::NoCost => true,
        // CR 601.2b: X is announced before payment, so an `{X}` alternative cost
        // spends mana whenever X ≥ 1.
        ManaCost::Cost { .. } => cost.mana_value() == 0 && !cost.has_x(),
        ManaCost::SelfManaCost | ManaCost::SelfManaValue | ManaCost::SelfManaCostReduced { .. } => {
            false
        }
    }
}

impl ChaliceAvoidancePolicy {
    pub fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        // Only at cast time — countering happens on cast (CR 601.2i), so the
        // decision is whether to put the spell on the stack at all. The whole
        // cast family counts, and the free members of it are exactly what the
        // `NoManaSpent` class exists to punish.
        if !is_cast_family_action(&ctx.candidate.action) {
            return 0.0;
        }
        // `cast_facts` resolves the cast object for every family member and
        // carries the CR 118.9 cost mode; `source_object()` only knows the
        // literal `CastSpell` arm.
        let Some(facts) = ctx.cast_facts() else {
            return 0.0;
        };
        if !spell_can_be_countered(ctx.state, facts.object.id) {
            return 0.0;
        }

        // Pick the worst applicable trap: an own trap that matches is the
        // strongest signal; otherwise an opponent's matching trap still demotes.
        let mut worst = 0.0_f64;
        for chalice in chalice_matches(ctx.state, ctx.ai_player) {
            if !chalice.punishes(&facts) {
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

/// Iterate every Chalice-class trap on the battlefield whose caster scope
/// covers a cast by `viewer`, paired with the gate it applies and whether
/// `viewer` controls it.
fn chalice_matches<'a>(
    state: &'a GameState,
    viewer: PlayerId,
) -> impl Iterator<Item = ChaliceMatch<'a>> + 'a {
    state.battlefield.iter().filter_map(move |id| {
        let permanent = state.objects.get(id)?;
        let gate = permanent
            .trigger_definitions
            .as_slice()
            .iter()
            .map(|entry| &entry.definition)
            .find_map(|trigger| {
                let gate = spell_trap_gate(trigger)?;
                scope_covers_cast_by(state, trigger, permanent.controller, viewer).then_some(gate)
            })?;
        Some(ChaliceMatch {
            gate,
            permanent,
            own: permanent.controller == viewer,
        })
    })
}

/// Structurally classify one trigger as Chalice-class: a spell-cast trigger that
/// counters the cast spell under one of the [`SpellTrapGate`] conditions.
/// Covers any card with either shape — not just Chalice or Vexing Bauble.
fn spell_trap_gate(trigger: &TriggerDefinition) -> Option<SpellTrapGate<'_>> {
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
    // CR 202.3 + CR 122.1: the cast filter gates on mana value equal to the
    // count of one of this permanent's own counters.
    if let Some(counter_type) = trigger
        .valid_card
        .as_ref()
        .and_then(filter_counter_type_for_cmc_eq_self_counters)
    {
        return Some(SpellTrapGate::ManaValueEqualsCounters(counter_type));
    }
    // CR 603.4: the intervening-`if` condition gates on "no mana was spent".
    // Mirror the engine's own discriminator exactly — `triggers.rs` matches
    // `TriggerCondition::ManaSpentCondition { text }` and tests
    // `text.contains("no mana was spent")`, defaulting every other mana-spent
    // condition to false, so anything else in this slot never fires.
    //
    // Top-level condition only: a `ManaSpentCondition` nested inside an `And` /
    // `Or` composite is deliberately not classified, because the sibling terms
    // would change when the trigger fires and this policy cannot evaluate them
    // card-locally.
    matches!(
        &trigger.condition,
        Some(TriggerCondition::ManaSpentCondition { text }) if text.contains("no mana was spent")
    )
    .then_some(SpellTrapGate::NoManaSpent)
}

/// CR 603.2: a spell-cast trigger fires only for casts by a player its
/// `valid_target` covers — `trigger_matchers::valid_player_matches` checks the
/// CASTING player against that filter, treating a missing filter as "any
/// player". This policy only ever scores the AI's own casts, so the question is
/// whether `caster` (the AI) is in scope for a trap controlled by
/// `source_controller`.
///
/// This is a PARTIAL mirror of `trigger_matchers::player_matches_filter`: it
/// covers only the caster scopes counter-on-cast triggers actually carry in
/// card-data — of the 31 such triggers, 20 have no `valid_target` (Chalice,
/// Vexing Bauble), 9 are `Typed { controller: Opponent }` (Lavinia, Boromir) and
/// 2 are `Controller`. The engine's four remaining explicit arms
/// (`SourceChosenPlayer`, `AttachedTo`, `ParentTargetController`,
/// `PlayerMatching`) need a `TriggerSourceContext` this policy does not have and
/// appear on no card in this class, so they fall to `_` alongside the engine's
/// own fail-OPEN tail: an unrecognised player scope matches every player there,
/// so the trigger would fire and the cast would be countered.
fn scope_covers_cast_by(
    state: &GameState,
    trigger: &TriggerDefinition,
    source_controller: PlayerId,
    caster: PlayerId,
) -> bool {
    // "Whenever a player casts a spell" (Chalice, Vexing Bauble): no caster
    // scope, so the trap is symmetric and eats its own controller's casts too.
    let Some(filter) = trigger.valid_target.as_ref() else {
        return true;
    };
    match filter {
        TargetFilter::Player | TargetFilter::AllPlayers => true,
        TargetFilter::Controller => source_controller == caster,
        // CR 102.3: in team games teammates are not opponents, so route every
        // opponent-scoped filter through the shared team-topology authority.
        TargetFilter::Opponent => is_opponent(state, source_controller, caster),
        TargetFilter::Typed(TypedFilter {
            controller: Some(ControllerRef::You),
            ..
        }) => source_controller == caster,
        TargetFilter::Typed(TypedFilter {
            controller: Some(ControllerRef::Opponent),
            ..
        }) => is_opponent(state, source_controller, caster),
        _ => true,
    }
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
fn filter_counter_type_for_cmc_eq_self_counters(filter: &TargetFilter) -> Option<&CounterType> {
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
        } => Some(counter_type),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cast_facts::cast_facts_for_action;
    use crate::config::AiConfig;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{AbilityDefinition, AbilityKind, StaticDefinition};
    use engine::types::actions::GameAction;
    use engine::types::card_type::CoreType;
    use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::keywords::Keyword;
    use engine::types::mana::ManaCostShard;
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
    fn add_chalice(state: &mut GameState, owner: PlayerId, charge_count: u32) -> ObjectId {
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
        id
    }

    /// Build a Vexing-Bauble-class artifact: "Whenever a player casts a spell,
    /// if no mana was spent to cast it, counter that spell." `caster_scope` is
    /// the trigger's `valid_target` — `None` for the symmetric Bauble wording,
    /// `Typed { controller: Opponent }` for the Lavinia wording.
    fn add_bauble(
        state: &mut GameState,
        owner: PlayerId,
        caster_scope: Option<TargetFilter>,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            "Vexing Bauble".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        let mut trigger =
            TriggerDefinition::new(TriggerMode::SpellCast).execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Counter {
                    target: TargetFilter::TriggeringSource,
                    source_rider: None,
                    countered_spell_zone: None,
                },
            ));
        trigger.condition = Some(TriggerCondition::ManaSpentCondition {
            text: "no mana was spent to cast it".to_string(),
        });
        trigger.valid_target = caster_scope;
        obj.trigger_definitions.push(trigger);
        id
    }

    /// Lavinia's caster scope: "Whenever an opponent casts a spell …".
    fn opponent_scope() -> Option<TargetFilter> {
        Some(TargetFilter::Typed(TypedFilter {
            type_filters: Vec::new(),
            controller: Some(ControllerRef::Opponent),
            properties: Vec::new(),
        }))
    }

    /// Put an AI-owned spell with the given mana cost into hand.
    fn add_spell(state: &mut GameState, mana_cost: ManaCost) -> (ObjectId, CardId) {
        let card_id = CardId(state.next_object_id);
        let spell_id = create_object(state, card_id, AI, "Spell".to_string(), Zone::Hand);
        let obj = state.objects.get_mut(&spell_id).unwrap();
        obj.card_types.core_types.push(CoreType::Instant);
        obj.mana_cost = mana_cost;
        (spell_id, card_id)
    }

    fn generic(mana_value: u32) -> ManaCost {
        ManaCost::Cost {
            shards: Vec::new(),
            generic: mana_value,
        }
    }

    /// Build a cast candidate for a spell with the given mana value, owned by AI.
    fn cast_candidate(
        state: &mut GameState,
        mana_value: u32,
    ) -> (AiDecisionContext, CandidateAction) {
        let (spell_id, card_id) = add_spell(state, generic(mana_value));
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
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Spell),
        };
        (decision, candidate)
    }

    /// Build a `CastSpellForFree` candidate (CR 118.9 + CR 107.3b) — the
    /// permission source is `source_id`.
    fn free_cast_candidate(
        state: &mut GameState,
        mana_cost: ManaCost,
        source_id: ObjectId,
    ) -> (AiDecisionContext, CandidateAction) {
        let (spell_id, card_id) = add_spell(state, mana_cost);
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpellForFree {
                object_id: spell_id,
                card_id,
                source_id,
                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Spell),
        };
        (decision, candidate)
    }

    /// Build a `CastSpellAsMadness` candidate (CR 702.35a) for a card whose
    /// printed cost is `printed` and whose madness cost is `madness_cost` —
    /// `cast_facts` reads the alternative cost off the object's own keyword.
    fn madness_candidate(
        state: &mut GameState,
        printed: ManaCost,
        madness_cost: ManaCost,
    ) -> (AiDecisionContext, CandidateAction) {
        let (spell_id, card_id) = add_spell(state, printed);
        state
            .objects
            .get_mut(&spell_id)
            .unwrap()
            .keywords
            .push(Keyword::Madness(madness_cost));
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpellAsMadness {
                object_id: spell_id,
                card_id,
                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Spell),
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

    /// Like [`score`] but populates `PolicyContext::cast_facts` the way
    /// production does (`search.rs` builds it with `cast_facts_for_action`), so
    /// the non-`CastSpell` members of the cast family carry their CR 118.9 cost
    /// mode. `PolicyContext::cast_facts()`'s lazy fallback only covers the
    /// literal `CastSpell` arm.
    fn score_with_facts(
        state: &GameState,
        decision: &AiDecisionContext,
        candidate: &CandidateAction,
    ) -> f64 {
        let config = AiConfig::default();
        let ctx = PolicyContext {
            state,
            decision,
            candidate,
            ai_player: AI,
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: cast_facts_for_action(state, &candidate.action, AI),
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        ChaliceAvoidancePolicy.score(&ctx)
    }

    /// Read the gate a permanent's spell-cast trigger applies.
    fn gate_of(state: &GameState, id: ObjectId) -> Option<SpellTrapGate<'_>> {
        state
            .objects
            .get(&id)?
            .trigger_definitions
            .as_slice()
            .iter()
            .find_map(|entry| spell_trap_gate(&entry.definition))
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

    /// Shape detection: Vexing Bauble's real parsed trigger (no `valid_card`, a
    /// top-level `ManaSpentCondition` reading "no mana was spent to cast it")
    /// classifies as the `NoManaSpent` gate, while Chalice still classifies as
    /// the counter-count gate. Both arms of the axis, one fixture each.
    #[test]
    fn vexing_bauble_shape_detected_as_no_mana_spent_gate() {
        let mut state = GameState::new_two_player(0);
        let bauble = add_bauble(&mut state, AI, None);
        let chalice = add_chalice(&mut state, AI, 2);

        assert_eq!(gate_of(&state, bauble), Some(SpellTrapGate::NoManaSpent));
        assert_eq!(
            gate_of(&state, chalice),
            Some(SpellTrapGate::ManaValueEqualsCounters(&charge()))
        );
    }

    /// The reported miss: a free cast (CR 118.9 + CR 107.3b) into an own Vexing
    /// Bauble is countered no matter what the card's printed mana value is, so
    /// the AI must not take the free cast.
    #[test]
    fn free_cast_into_bauble_penalised() {
        let mut state = GameState::new_two_player(0);
        let bauble = add_bauble(&mut state, AI, None);
        let (decision, candidate) = free_cast_candidate(&mut state, generic(5), bauble);
        let delta = score_with_facts(&state, &decision, &candidate);
        assert!(
            delta < -5.0,
            "a free cast into an own Bauble must be strongly demoted, got {delta}"
        );
    }

    /// The second half of the class: a plain `CastSpell` of a `{0}` card
    /// (Ornithopter, Lotus Petal) also spends no mana and is countered.
    #[test]
    fn zero_mv_cast_into_bauble_penalised() {
        let mut state = GameState::new_two_player(0);
        add_bauble(&mut state, AI, None);
        let (decision, candidate) = cast_candidate(&mut state, 0);
        let delta = score(&state, &decision, &candidate);
        assert!(
            delta < -5.0,
            "a {{0}} cast into an own Bauble must be strongly demoted, got {delta}"
        );
    }

    /// Self-harden: a normally-paid cast spends mana, so the Bauble's
    /// intervening-`if` is false and the AI must not be discouraged from it.
    /// The MV-0 and free-cast cases above prove this is not vacuous.
    #[test]
    fn paid_cast_into_bauble_not_penalised() {
        let mut state = GameState::new_two_player(0);
        add_bauble(&mut state, AI, None);
        let (decision, candidate) = cast_candidate(&mut state, 3);
        assert_eq!(score(&state, &decision, &candidate), 0.0);
    }

    /// CR 601.2b: X is announced before payment, so a `{X}` cost with MV 0
    /// (Walking Ballista) does spend mana whenever X ≥ 1. Excluded from the
    /// zero-cost arm rather than penalised.
    #[test]
    fn x_cost_cast_into_bauble_not_penalised() {
        let mut state = GameState::new_two_player(0);
        add_bauble(&mut state, AI, None);
        let (spell_id, card_id) = add_spell(
            &mut state,
            ManaCost::Cost {
                shards: vec![ManaCostShard::X, ManaCostShard::X],
                generic: 0,
            },
        );
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
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Spell),
        };
        assert_eq!(score(&state, &decision, &candidate), 0.0);
    }

    /// Caster scope (CR 603.2): Lavinia, Azorius Renegade reads "whenever an
    /// OPPONENT casts a spell", so the AI's own Lavinia never eats the AI's own
    /// free cast — and the policy must not invent a penalty for it.
    #[test]
    fn own_opponent_scoped_trap_does_not_punish_own_free_cast() {
        let mut state = GameState::new_two_player(0);
        let lavinia = add_bauble(&mut state, AI, opponent_scope());
        let (decision, candidate) = free_cast_candidate(&mut state, generic(2), lavinia);
        assert_eq!(score_with_facts(&state, &decision, &candidate), 0.0);
    }

    /// CR 118.9: a madness `{0}` (Basking Rootwalla — card-data confirms
    /// `Keyword::Madness(Cost { shards: [], generic: 0 })`) replaces the printed
    /// mana cost, so the cast spends nothing and the Bauble eats it even though
    /// the card's printed mana value is 2.
    #[test]
    fn madness_zero_cost_cast_into_bauble_penalised() {
        let mut state = GameState::new_two_player(0);
        add_bauble(&mut state, AI, None);
        let (decision, candidate) = madness_candidate(&mut state, generic(2), generic(0));
        let delta = score_with_facts(&state, &decision, &candidate);
        assert!(
            delta < -5.0,
            "a madness {{0}} cast into an own Bauble must be strongly demoted, got {delta}"
        );
    }

    /// The guard for the same arm: a madness cost that DOES spend mana is not
    /// punished, even on a `{0}`-printed card — the alternative cost replaces
    /// the mana cost, so the printed mana value must not be consulted.
    #[test]
    fn madness_paid_cost_cast_into_bauble_not_penalised() {
        let mut state = GameState::new_two_player(0);
        add_bauble(&mut state, AI, None);
        let (decision, candidate) = madness_candidate(&mut state, generic(0), generic(2));
        assert_eq!(score_with_facts(&state, &decision, &candidate), 0.0);
    }

    /// The mirror: an OPPONENT's Lavinia does see the AI's free cast, and
    /// demotes it at the opponent-trap rate.
    #[test]
    fn opponent_scoped_trap_punishes_ai_free_cast() {
        let mut state = GameState::new_two_player(0);
        let lavinia = add_bauble(&mut state, OPP, opponent_scope());
        let (decision, candidate) = free_cast_candidate(&mut state, generic(2), lavinia);
        assert_eq!(
            score_with_facts(&state, &decision, &candidate),
            AiConfig::default()
                .policy_penalties
                .opponent_chalice_counter_penalty
        );
    }
}
