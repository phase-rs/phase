//! Hard-veto gate for casting/activating an {X}-cost spell or ability whose
//! only affordable X is 0 and whose payoff scales with X — a guaranteed no-op.
//!
//! Issue: the AI commits to `GameAction::CastSpell` / `ActivateAbility` at the
//! `WaitingFor::Priority` seam, *before* X is chosen (X is announced downstream
//! at `WaitingFor::ChooseXValue`, CR 601.2b/f). When the only value the AI can
//! afford is X=0, nothing rejects the commitment, so it casts Exsanguinate for 0
//! (draining no life), pumps Helix Pinnacle by 0 counters, wipes its own board
//! to 0/0 with Mirror Entity, etc. `XValuePolicy` cannot help — it only scores
//! the `ChooseX` candidates *after* the cast is already committed.
//!
//! This gate rejects the pre-cast commitment when three things hold:
//! 1. the cost carries an `{X}` mana shard,
//! 2. `casting_costs::max_x_value` (the engine's single X-affordability
//!    authority, CR 601.2b/f) reports the only legal X is 0, and
//! 3. the payoff genuinely scales with X (via `x_reference`, the shared
//!    detector the ramp policy also consumes), with any *fixed* non-X residual
//!    still being trivial (priced by `self_cost::effect_is_trivial`).
//!
//! Rejecting the cast makes the AI `Pass` (always a Priority candidate) and hold
//! the card — it never loses the card, it just waits until X≥1 is affordable.
//! Build for the class, not the card: every rule 1–3 match is gated regardless
//! of card name; fixed-payoff {X} cards (whose payoff does not reference X) are
//! structurally spared.

use engine::game::game_object::GameObject;
use engine::game::max_x_value;
use engine::types::ability::{AbilityCost, AbilityDefinition, AbilityKind, Effect};
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::player::PlayerId;

use crate::ability_chain::collect_chain_effects;
use crate::cast_facts::collect_definition_effects;
use crate::features::DeckFeatures;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use super::self_cost::effect_is_trivial;
use super::x_reference;

pub struct XCastGatePolicy;

impl TacticalPolicy for XCastGatePolicy {
    fn id(&self) -> PolicyId {
        PolicyId::XCastGate
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::CastSpell, DecisionKind::ActivateAbility]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        // A hard-veto backstop for every {X}-cost cast / activation candidate,
        // on or off the AI's turn (instant-speed X). The verdict is a `Reject`,
        // not an activation-scaled delta — mirrors `SelfCostValuePolicy`.
        // activation-constant: unconditional Reject backstop; gating in `verdict`.
        Some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        gate_rejects(ctx).map_or_else(
            || PolicyVerdict::neutral(PolicyReason::new("x_cast_gate_na")),
            PolicyVerdict::reject,
        )
    }
}

/// `Some(&object.mana_cost)` iff the object's printed mana cost carries an `{X}`
/// shard, else `None`.
fn spell_x_manacost(object: &GameObject) -> Option<&ManaCost> {
    mana_cost_has_x(&object.mana_cost).then_some(&object.mana_cost)
}

/// The `{X}`-bearing `ManaCost` inside an activated ability's cost, if any.
/// Exhaustive over the compositional cost shapes: a bare `Mana` cost is
/// X-checked directly; `Composite`/`OneOf` search their sub-costs. Every other
/// cost variant is fail-open (`None`) — a cost shape we do not model simply gets
/// no gate rather than a spurious veto.
fn activated_x_manacost(cost: &AbilityCost) -> Option<&ManaCost> {
    match cost {
        AbilityCost::Mana { cost } => mana_cost_has_x(cost).then_some(cost),
        AbilityCost::Composite { costs } | AbilityCost::OneOf { costs } => {
            costs.iter().find_map(activated_x_manacost)
        }
        _ => None,
    }
}

fn mana_cost_has_x(cost: &ManaCost) -> bool {
    matches!(cost, ManaCost::Cost { shards, .. } if shards.iter().any(|s| matches!(s, ManaCostShard::X)))
}

/// Collect the payoff effects to price at X=0 and whether the spell object
/// itself references X (X-counter-ETB creatures, dynamic-P/T statics).
///
/// For an activated ability the payoff is just its own resolution chain. For a
/// spell (v3 CORRECTION 2) the residual walk takes effects from `CastFacts`'s
/// definition lists — the printed spell abilities, immediate ETB triggers, and
/// immediate ETB replacements — because the boolean `EffectProfile` cannot
/// yield `&Effect`s. Object-level X detection stays independent via
/// `spell_object_references_x`, so X-counter-ETB creatures are gated even when
/// their X reference is a replacement rather than a flat effect.
fn payoff_effects_and_x_ref<'a>(
    ctx: &PolicyContext<'a>,
    object_id: ObjectId,
    ability: &'a AbilityDefinition,
    is_spell: bool,
) -> (Vec<&'a Effect>, bool) {
    if !is_spell {
        return (collect_chain_effects(ability), false);
    }

    let effects = match ctx.cast_facts() {
        Some(facts) => {
            let mut effects: Vec<&Effect> = Vec::new();
            for def in facts.primary_effects {
                effects.extend(collect_definition_effects(def));
            }
            for trigger in facts.immediate_etb_triggers {
                if let Some(exec) = trigger.execute.as_deref() {
                    effects.extend(collect_definition_effects(exec));
                }
            }
            for replacement in facts.immediate_replacements {
                if let Some(exec) = replacement.execute.as_deref() {
                    effects.extend(collect_definition_effects(exec));
                }
            }
            effects
        }
        None => collect_chain_effects(ability),
    };
    let object_level_x = x_reference::spell_object_references_x(ctx.state, object_id);
    (effects, object_level_x)
}

/// The reviewer's unified predicate, made chain-aware (v3 CORRECTION 1): the
/// payoff is a guaranteed no-op at X=0 iff every effect either scales with X (so
/// resolves to 0) or is a trivial fixed residual — and at least one effect
/// actually references X.
///
/// `prev_was_x` tracks *only the immediately preceding* effect: an effect that
/// reads `PreviousEffectAmount` is transitively 0 at X=0 exactly when
/// its predecessor was X-scaled (Exsanguinate's "gain life equal to the life
/// lost this way" after "each opponent loses X life"). A non-X residual resets
/// `prev_was_x`, so `LoseLife{X}, GainLife{Fixed 5}, Draw{PreviousEffectAmount}`
/// does NOT treat the draw as X-scaled (the 5 came from the fixed gain).
fn no_op_at_x_zero(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    ability: &AbilityDefinition,
    effects: &[&Effect],
    object_level_x: bool,
) -> bool {
    let mut references_any_x = object_level_x;
    let mut prev_was_x = object_level_x;
    for effect in effects {
        if x_reference::effect_references_x(effect) {
            references_any_x = true;
            prev_was_x = true;
            continue;
        }
        if prev_was_x && x_reference::effect_references_previous_amount(effect) {
            // Chain-relative to an X-scaled predecessor → also 0 at X=0.
            references_any_x = true;
            prev_was_x = true;
            continue;
        }
        if !effect_is_trivial(state, ai_player, source_id, ability, effect) {
            // A fixed, meaningful non-X residual is worth casting at X=0.
            return false;
        }
        prev_was_x = false;
    }
    references_any_x
}

fn gate_rejects(ctx: &PolicyContext<'_>) -> Option<PolicyReason> {
    // Perf: the board-wide `max_x_value` affordability sweep below only needs to
    // be correct at the committed decision. In beam/rollout lookahead an X=0 cast
    // is already dominated by its resulting-state eval, so scoring it neutral there
    // costs nothing and removes the per-node sweep that regressed large boards.
    if !ctx.at_root() {
        return None;
    }

    let state = ctx.state;
    let (ability, object_id, x_manacost, is_spell): (
        &AbilityDefinition,
        ObjectId,
        &ManaCost,
        bool,
    ) = match &ctx.candidate.action {
        GameAction::CastSpell { object_id, .. } => {
            let object = state.objects.get(object_id)?;
            // Conservative modal/multi-Spell-ability guard: a card with more
            // than one castable spell ability, or a modal `ChooseOneOf`
            // chain, may have a non-no-op mode at X=0 we do not analyse —
            // don't gate (miss a veto rather than suppress a real cast).
            let mut spell_abilities = object
                .abilities
                .iter()
                .filter(|a| a.kind == AbilityKind::Spell);
            let ability = spell_abilities.next()?;
            if spell_abilities.next().is_some() {
                return None;
            }
            if collect_definition_effects(ability)
                .iter()
                .any(|e| matches!(e, Effect::ChooseOneOf { .. }))
            {
                return None;
            }
            let x_manacost = spell_x_manacost(object)?;
            (ability, *object_id, x_manacost, true)
        }
        GameAction::ActivateAbility {
            source_id,
            ability_index,
        } => {
            let object = state.objects.get(source_id)?;
            let ability = object.abilities.get(*ability_index)?;
            let x_manacost = ability.cost.as_ref().and_then(activated_x_manacost)?;
            (ability, *source_id, x_manacost, false)
        }
        _ => return None,
    };

    // Card-local payoff walk FIRST (no board sweep): if the payoff is not a
    // guaranteed no-op at X=0 (a fixed meaningful residual, or it does not scale
    // with X), the gate can never fire — skip the affordability sweep entirely.
    // AND is commutative, so ordering the cheap card-local predicate before the
    // board-wide sweep cannot change any verdict; it only spares the sweep.
    let (effects, object_level_x) = payoff_effects_and_x_ref(ctx, object_id, ability, is_spell);
    if !no_op_at_x_zero(
        state,
        ctx.ai_player,
        object_id,
        ability,
        &effects,
        object_level_x,
    ) {
        return None;
    }

    // Board-wide affordability sweep LAST, only for genuine no-op-at-X=0 payoffs
    // at the root. `max_x_value` already caps X to what the caster can legally
    // pay (CR 601.2b/f), so max ≥ 1 means the ramp path (`XValuePolicy`) handles
    // it — don't gate.
    if max_x_value(state, ctx.ai_player, x_manacost, Some(object_id)) != 0 {
        return None;
    }

    Some(PolicyReason::new("x_cast_zero_no_op").with_fact("max_x", 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cast_facts::cast_facts_for_object;
    use crate::config::AiConfig;
    use crate::context::AiContext;
    use crate::policies::context::SearchDepth;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        Comparator, ContinuousModification, Duration, FilterProp, QuantityExpr, QuantityRef,
        ReplacementDefinition, StaticDefinition, TargetFilter, TypedFilter,
    };
    use engine::types::card_type::CoreType;
    use engine::types::counter::CounterType;
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId, TrackedSetId};
    use engine::types::keywords::Keyword;
    use engine::types::mana::{ManaCost, ManaPipId, ManaType, ManaUnit};
    use engine::types::replacements::ReplacementEvent;
    use engine::types::zones::Zone;
    use std::sync::Arc;

    const AI: PlayerId = PlayerId(0);
    const OPP: PlayerId = PlayerId(1);

    // --- fixture builders -------------------------------------------------

    fn x_expr() -> QuantityExpr {
        QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_string(),
            },
        }
    }

    fn x_only_cost() -> AbilityCost {
        AbilityCost::Mana {
            cost: ManaCost::Cost {
                shards: vec![ManaCostShard::X],
                generic: 0,
            },
        }
    }

    fn x_bb_manacost() -> ManaCost {
        ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Black, ManaCostShard::Black],
            generic: 0,
        }
    }

    fn colorless_unit() -> ManaUnit {
        ManaUnit {
            color: ManaType::Colorless,
            source_id: ObjectId(0),
            pip_id: ManaPipId(0),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: Vec::new(),
            grants: Vec::new(),
            expiry: None,
        }
    }

    fn add_pool(state: &mut GameState, player: PlayerId, count: usize) {
        for _ in 0..count {
            state.players[player.0 as usize]
                .mana_pool
                .add(colorless_unit());
        }
    }

    fn base_state() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        state.priority_player = AI;
        state
    }

    /// Battlefield object carrying a single activated ability.
    fn activated_source(state: &mut GameState, name: &str, ability: AbilityDefinition) -> ObjectId {
        let id = create_object(
            state,
            CardId(next_id()),
            AI,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        *Arc::make_mut(&mut obj.abilities) = vec![ability];
        id
    }

    /// Hand object representing an {X}-cost spell with the given spell ability.
    fn spell_source(
        state: &mut GameState,
        name: &str,
        ability: AbilityDefinition,
        mana_cost: ManaCost,
    ) -> (ObjectId, CardId) {
        let card_id = CardId(next_id());
        let id = create_object(state, card_id, AI, name.to_string(), Zone::Hand);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.mana_cost = mana_cost;
        *Arc::make_mut(&mut obj.abilities) = vec![ability];
        (id, card_id)
    }

    fn activated(effect: Effect, cost: AbilityCost) -> AbilityDefinition {
        let mut ability = AbilityDefinition::new(AbilityKind::Activated, effect);
        ability.cost = Some(cost);
        ability
    }

    fn spell(effect: Effect) -> AbilityDefinition {
        AbilityDefinition::new(AbilityKind::Spell, effect)
    }

    fn next_id() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(5000);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }

    // --- Exsanguinate real AST: LoseLife{X} + GainLife{PreviousEffectAmount} --

    fn exsanguinate_ability() -> AbilityDefinition {
        let mut ability = spell(Effect::LoseLife {
            amount: x_expr(),
            target: None,
        });
        ability.sub_ability = Some(Box::new(spell(Effect::GainLife {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::PreviousEffectAmount {
                    channel: engine::types::ability::DamageChannel::Total,
                },
            },
            player: TargetFilter::Controller,
        })));
        ability
    }

    fn helix_ability() -> AbilityDefinition {
        activated(
            Effect::PutCounter {
                counter_type: CounterType::Generic("tower".to_string()),
                count: x_expr(),
                target: TargetFilter::SelfRef,
            },
            x_only_cost(),
        )
    }

    // Mirror Entity: {X}: creatures you control become X/X (dynamic P/T via
    // CostXPaid) and gain all creature types.
    fn mirror_entity_ability() -> AbilityDefinition {
        let cost_x_paid = || QuantityExpr::Ref {
            qty: QuantityRef::CostXPaid,
        };
        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::Typed(TypedFilter::creature()))
            .modifications(vec![
                ContinuousModification::SetPowerDynamic {
                    value: cost_x_paid(),
                },
                ContinuousModification::SetToughnessDynamic {
                    value: cost_x_paid(),
                },
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Changeling,
                },
            ]);
        activated(
            Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            },
            x_only_cost(),
        )
    }

    // Day of Black Sun: GenericEffect granting RemoveAllAbilities over an
    // X-filtered subject (mana value X or less), then Destroy those creatures.
    fn day_of_black_sun_ability() -> AbilityDefinition {
        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::Typed(TypedFilter::creature().properties(
                vec![FilterProp::Cmc {
                    comparator: Comparator::LE,
                    value: x_expr(),
                }],
            )))
            .modifications(vec![ContinuousModification::RemoveAllAbilities]);
        let mut ability = spell(Effect::GenericEffect {
            static_abilities: vec![static_def],
            duration: Some(Duration::UntilEndOfTurn),
            target: None,
        });
        ability.sub_ability = Some(Box::new(spell(Effect::Destroy {
            target: TargetFilter::TrackedSet {
                id: TrackedSetId(0),
            },
            cant_regenerate: false,
        })));
        ability
    }

    /// Hangarback-shape ETB-X-counter creature: `{X}{X}` Artifact Creature that
    /// "enters with X +1/+1 counters on it", modeled (per the
    /// `spell_object_references_x` doc) as a `Moved`→`Battlefield` `SelfRef`
    /// self-replacement whose execute puts X `+1/+1` counters. The X payoff
    /// lives OUTSIDE the resolving spell ability, so it is recognized via
    /// `object_level_x` (the spell-object scan), the branch no other cast test
    /// reaches.
    ///
    /// Real Hangarback stamps this count as `CostXPaid`; here it is `Variable X`
    /// (`x_expr()`), the on-cast reference the shared detector recognizes. A
    /// permanent spell carries no meaningful spell-resolution effect, but the
    /// gate needs one `AbilityKind::Spell` ability to engage, so a `NoOp` spell
    /// ability stands in for "this permanent resolves and enters".
    fn etb_x_counter_creature(state: &mut GameState) -> (ObjectId, CardId) {
        let card_id = CardId(next_id());
        let id = create_object(
            state,
            card_id,
            AI,
            "Hangarback Walker".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::X],
            generic: 0,
        };
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.card_types.core_types.push(CoreType::Creature);
        *Arc::make_mut(&mut obj.abilities) = vec![spell(Effect::NoOp)];
        obj.replacement_definitions.push(
            ReplacementDefinition::new(ReplacementEvent::Moved)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::PutCounter {
                        counter_type: CounterType::Plus1Plus1,
                        count: x_expr(),
                        target: TargetFilter::SelfRef,
                    },
                ))
                .valid_card(TargetFilter::SelfRef)
                .destination_zone(Zone::Battlefield),
        );
        (id, card_id)
    }

    // --- context / verdict helpers ---------------------------------------

    fn verdict_for_activate(state: &GameState, source_id: ObjectId) -> PolicyVerdict {
        let candidate = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id,
                ability_index: 0,
            },
            metadata: ActionMetadata {
                actor: Some(AI),
                tactical_class: TacticalClass::Ability,
            },
        };
        verdict_for(state, candidate, SearchDepth::Root)
    }

    fn verdict_for_cast(state: &GameState, object_id: ObjectId, card_id: CardId) -> PolicyVerdict {
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id,
                card_id,
                targets: Vec::new(),
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            },
            metadata: ActionMetadata {
                actor: Some(AI),
                tactical_class: TacticalClass::Spell,
            },
        };
        verdict_for(state, candidate, SearchDepth::Root)
    }

    /// Like [`verdict_for_cast`] but populates the `PolicyContext::cast_facts`
    /// field with the real [`cast_facts_for_object`] (the constructor
    /// production uses), so the gate walks the `Some(facts)` branch of
    /// `payoff_effects_and_x_ref` from a genuinely populated `CastFacts` rather
    /// than the lazily-rebuilt path or the `None` fallback.
    fn verdict_for_cast_with_facts(
        state: &GameState,
        object_id: ObjectId,
        card_id: CardId,
    ) -> PolicyVerdict {
        let facts = cast_facts_for_object(state.objects.get(&object_id).expect("cast object"));
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id,
                card_id,
                targets: Vec::new(),
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            },
            metadata: ActionMetadata {
                actor: Some(AI),
                tactical_class: TacticalClass::Spell,
            },
        };
        let config = AiConfig::default();
        let context = AiContext::empty(&config.weights);
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        };
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: Some(facts),
            search_depth: SearchDepth::Root,
        };
        XCastGatePolicy.verdict(&ctx)
    }

    /// Activation verdict at a caller-chosen search depth — lets a test drive the
    /// identical fixture at both `Root` and `Lookahead` to prove the depth guard
    /// is the sole cause of a verdict flip.
    fn verdict_for_activate_at(
        state: &GameState,
        source_id: ObjectId,
        search_depth: SearchDepth,
    ) -> PolicyVerdict {
        let candidate = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id,
                ability_index: 0,
            },
            metadata: ActionMetadata {
                actor: Some(AI),
                tactical_class: TacticalClass::Ability,
            },
        };
        verdict_for(state, candidate, search_depth)
    }

    fn verdict_for(
        state: &GameState,
        candidate: CandidateAction,
        search_depth: SearchDepth,
    ) -> PolicyVerdict {
        let config = AiConfig::default();
        let context = AiContext::empty(&config.weights);
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        };
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth,
        };
        XCastGatePolicy.verdict(&ctx)
    }

    fn assert_reject(verdict: &PolicyVerdict, kind: &str) {
        match verdict {
            PolicyVerdict::Reject { reason } => assert_eq!(reason.kind, kind, "reject kind"),
            PolicyVerdict::Score { delta, reason } => panic!(
                "expected reject {kind}, got Score {{ delta: {delta}, kind: {} }}",
                reason.kind
            ),
        }
    }

    fn assert_not_reject(verdict: &PolicyVerdict) {
        assert!(
            matches!(verdict, PolicyVerdict::Score { .. }),
            "expected a non-reject (neutral) verdict, got {verdict:?}"
        );
    }

    // --- Helix Pinnacle: detector-based gate (HIGH) -----------------------

    #[test]
    fn helix_pinnacle_max_x_zero_rejected() {
        // {X}: put X tower counters on ~, with no mana → max X = 0. Discriminating:
        // the reason is `x_cast_zero_no_op`, driven by the PutCounter-X detector.
        // A `benefit_is_trivial`-delegating gate would NOT reject (Helix's
        // beneficial self-counter is non-trivial), so a Reject here proves the
        // detector-based gate, not a triviality delegation.
        let mut state = base_state();
        let source = activated_source(&mut state, "Helix Pinnacle", helix_ability());
        assert_reject(&verdict_for_activate(&state, source), "x_cast_zero_no_op");
    }

    #[test]
    fn helix_pinnacle_lookahead_returns_neutral() {
        // The fix, discriminating: the IDENTICAL zero-mana Helix fixture that
        // `helix_pinnacle_max_x_zero_rejected` rejects at `Root` must return a
        // neutral (non-reject) verdict at `Lookahead`. Same inputs, opposite
        // verdicts, differing ONLY in `search_depth` — so the depth guard is the
        // sole cause. Reverting the `if !ctx.at_root() { return None; }` first line
        // of `gate_rejects` makes this reject (the board sweep runs, max_x=0, the
        // PutCounter-X payoff is a no-op) → `assert_not_reject` fails.
        //
        // Non-vacuous: the paired Root-reject test proves the fixture reaches the
        // real gate arm (genuine {X} no-op payoff), so neutrality here is caused by
        // depth, not by an upstream short-circuit (non-{X} cost, wrong action, etc.).
        let mut state = base_state();
        let source = activated_source(&mut state, "Helix Pinnacle", helix_ability());
        assert_not_reject(&verdict_for_activate_at(
            &state,
            source,
            SearchDepth::Lookahead,
        ));
    }

    #[test]
    fn helix_pinnacle_max_x_one_not_rejected() {
        // Reach-guard: one mana available → max X = 1 → the ramp handles it, the
        // gate stands down (proves the gate keys on live affordability, not on
        // {X}-presence).
        let mut state = base_state();
        let source = activated_source(&mut state, "Helix Pinnacle", helix_ability());
        add_pool(&mut state, AI, 1);
        assert_not_reject(&verdict_for_activate(&state, source));
    }

    // --- Exsanguinate: chain-relative residual, THREE states (v3) ---------

    #[test]
    fn exsanguinate_max_x_zero_rejected_healthy() {
        let mut state = base_state();
        let (obj, card) = spell_source(
            &mut state,
            "Exsanguinate",
            exsanguinate_ability(),
            x_bb_manacost(),
        );
        assert_reject(&verdict_for_cast(&state, obj, card), "x_cast_zero_no_op");
    }

    #[test]
    fn exsanguinate_max_x_zero_rejected_life_critical() {
        // Without the chain-aware handling, a low-life AI treats
        // GainLife{PreviousEffectAmount} as a non-trivial residual (self_cost's
        // GainLife arm is non-trivial when life-critical) and does NOT gate. The
        // chain-relative branch keeps it gated.
        let mut state = base_state();
        state.players[AI.0 as usize].life = 3; // ai_life_critical (<= 5)
        let (obj, card) = spell_source(
            &mut state,
            "Exsanguinate",
            exsanguinate_ability(),
            x_bb_manacost(),
        );
        assert_reject(&verdict_for_cast(&state, obj, card), "x_cast_zero_no_op");
    }

    #[test]
    fn exsanguinate_max_x_zero_rejected_stale_last_effect_amount() {
        // A stale `last_effect_amount` above the lifegain ceiling would make the
        // GainLife residual resolve non-trivially through effect_is_trivial. The
        // chain-relative branch short-circuits that path, so it stays gated.
        let mut state = base_state();
        state.last_effect_amount = Some(10); // > TRIVIAL_LIFEGAIN_CEILING
        let (obj, card) = spell_source(
            &mut state,
            "Exsanguinate",
            exsanguinate_ability(),
            x_bb_manacost(),
        );
        assert_reject(&verdict_for_cast(&state, obj, card), "x_cast_zero_no_op");
    }

    #[test]
    fn exsanguinate_max_x_one_not_rejected() {
        // Reach-guard: {X}{B}{B} with three mana → max X = 1 → not gated.
        let mut state = base_state();
        let (obj, card) = spell_source(
            &mut state,
            "Exsanguinate",
            exsanguinate_ability(),
            x_bb_manacost(),
        );
        add_pool(&mut state, AI, 3);
        assert_not_reject(&verdict_for_cast(&state, obj, card));
    }

    // --- Interleaved-chain negative: prev_was_x resets (guards over-gating) --

    #[test]
    fn interleaved_fixed_gain_then_prev_amount_not_gated() {
        // LoseLife{X}, GainLife{Fixed 5}, Draw{PreviousEffectAmount}: the draw's
        // PreviousEffectAmount is 5 (from the fixed gain), NOT X. The fixed
        // GainLife{5} is a meaningful non-X residual (> the lifegain ceiling), so
        // the gate stands down. Reverting the immediate-predecessor reset (treating
        // "any prior X" as latching) would wrongly gate this.
        let mut state = base_state();
        let mut ability = spell(Effect::LoseLife {
            amount: x_expr(),
            target: None,
        });
        let mut draw = spell(Effect::Draw {
            count: QuantityExpr::Ref {
                qty: QuantityRef::PreviousEffectAmount {
                    channel: engine::types::ability::DamageChannel::Total,
                },
            },
            target: TargetFilter::Controller,
        });
        // (draw is the tail; gain is the middle)
        draw.sub_ability = None;
        let mut gain = spell(Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 5 },
            player: TargetFilter::Controller,
        });
        gain.sub_ability = Some(Box::new(draw));
        ability.sub_ability = Some(Box::new(gain));
        let (obj, card) = spell_source(&mut state, "Interleaved", ability, x_bb_manacost());
        assert_not_reject(&verdict_for_cast(&state, obj, card));
    }

    // --- Fixed-payoff {X}: references_any_x requirement (MEDIUM-HIGH) ------

    #[test]
    fn fixed_generic_keyword_grant_not_gated() {
        // {X} ability whose only payoff is a FIXED non-X keyword grant (GenericEffect
        // AddKeyword). effect_is_trivial treats it as trivial, but references_any_x is
        // false → the gate stands down. Reverting the references_any_x requirement
        // (gate whenever all effects are trivial) would REJECT this.
        let mut state = base_state();
        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Flying,
            }]);
        let ability = activated(
            Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            },
            x_only_cost(),
        );
        let source = activated_source(&mut state, "Fixed Grant", ability);
        assert_not_reject(&verdict_for_activate(&state, source));
    }

    #[test]
    fn x_referencing_rider_now_gated() {
        // Positive reach-guard for the row above: with the rider's count referencing
        // X (Draw{X} standing in for Token{Variable X} to avoid a 16-field Token
        // literal — same references_any_x flip via the count-X arm), the gate now
        // rejects. Proves the fixed-payoff stand-down is not vacuous.
        let mut state = base_state();
        let ability = activated(
            Effect::Draw {
                count: x_expr(),
                target: TargetFilter::Controller,
            },
            x_only_cost(),
        );
        let source = activated_source(&mut state, "X Draw", ability);
        assert_reject(&verdict_for_activate(&state, source), "x_cast_zero_no_op");
    }

    // --- RC1: Mirror Entity (CostXPaid) + Day of Black Sun (filter-CMC-X) --

    #[test]
    fn mirror_entity_max_x_zero_rejected() {
        // Reaches the gate only via RC1: the GenericEffect-static arm of
        // effect_references_x + expr_references_chosen_x recognising CostXPaid in
        // SetPowerDynamic. Reverting either arm makes references_any_x false → not
        // gated → this Reject fails. Gating avoids wiping the AI's board to 0/0.
        let mut state = base_state();
        let source = activated_source(&mut state, "Mirror Entity", mirror_entity_ability());
        assert_reject(&verdict_for_activate(&state, source), "x_cast_zero_no_op");
    }

    #[test]
    fn mirror_entity_max_x_one_not_rejected() {
        let mut state = base_state();
        let source = activated_source(&mut state, "Mirror Entity", mirror_entity_ability());
        add_pool(&mut state, AI, 1);
        assert_not_reject(&verdict_for_activate(&state, source));
    }

    #[test]
    fn day_of_black_sun_max_x_zero_rejected() {
        // Reaches the gate only via RC1: the GenericEffect-static arm +
        // target_filter_references_x recognising FilterProp::Cmc{LE, X} in the
        // granted static's AFFECTED subject filter. At X=0 the symmetric −X/−X wipe
        // (here modeled as RemoveAllAbilities + Destroy of the empty X-filtered set)
        // is a no-op. Reverting the affected-filter walk makes references_any_x false.
        let mut state = base_state();
        let (obj, card) = spell_source(
            &mut state,
            "Day of Black Sun",
            day_of_black_sun_ability(),
            x_bb_manacost(),
        );
        assert_reject(&verdict_for_cast(&state, obj, card), "x_cast_zero_no_op");
    }

    // --- Non-{X} cost: cheap short-circuit, never gated -------------------

    #[test]
    fn non_x_cost_activation_not_gated() {
        // A fixed-cost activated ability (no {X} shard) short-circuits to None
        // before the affordability sweep — the gate only ever fires on {X} costs.
        let mut state = base_state();
        let ability = activated(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            AbilityCost::Mana {
                cost: ManaCost::generic(2),
            },
        );
        let source = activated_source(&mut state, "Fixed Draw", ability);
        assert_not_reject(&verdict_for_activate(&state, source));
    }

    // --- Multi-Spell-ability guard: conservative stand-down ---------------

    #[test]
    fn multi_spell_ability_object_not_gated() {
        // Two Spell abilities on one object → we cannot know which resolves; the
        // conservative guard declines to gate even at max X = 0.
        let mut state = base_state();
        let card_id = CardId(next_id());
        let id = create_object(&mut state, card_id, AI, "Twin".to_string(), Zone::Hand);
        {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.mana_cost = x_bb_manacost();
            *Arc::make_mut(&mut obj.abilities) = vec![
                spell(Effect::LoseLife {
                    amount: x_expr(),
                    target: None,
                }),
                spell(Effect::GainLife {
                    amount: x_expr(),
                    player: TargetFilter::Controller,
                }),
            ];
        }
        assert_not_reject(&verdict_for_cast(&state, id, card_id));
        let _ = OPP;
    }

    // --- ETB-X-counter creature: object_level_x + Some(cast_facts) branch ---

    #[test]
    fn etb_x_counter_creature_max_x_zero_rejected_via_object_level_x() {
        // A Hangarback-shape {X}{X} creature that "enters with X +1/+1 counters"
        // via a Moved→Battlefield SelfRef replacement. At max X = 0 it would
        // enter as a 0/0 and die immediately — a guaranteed no-op → Reject.
        //
        // Unlike every other cast test (whose X payoff is a flat spell-ability
        // effect), this fixture's X reference lives on the spell OBJECT's
        // replacement, so the rejection is driven by `object_level_x`
        // (`spell_object_references_x`) inside the `Some(cast_facts)` arm of
        // `payoff_effects_and_x_ref` — the previously untested production path.
        let mut state = base_state();
        let (obj, card) = etb_x_counter_creature(&mut state);

        // Verify by construction that this drives the Some(facts) branch and
        // object_level_x, not the None fallback:
        // - the ETB replacement is an immediate self-replacement, so the real
        //   CastFacts (the constructor production uses) carries it, and the
        //   Some(facts) arm sources effects from `immediate_replacements`;
        // - `spell_object_references_x` sees the object-level X, so
        //   `object_level_x` is true.
        let facts = cast_facts_for_object(state.objects.get(&obj).unwrap());
        assert_eq!(
            facts.immediate_replacements.len(),
            1,
            "the ETB replacement must be an immediate self-replacement so the \
             Some(cast_facts) branch sources it"
        );
        assert!(
            x_reference::spell_object_references_x(&state, obj),
            "object_level_x must be true — the X lives on the spell object's \
             replacement, not its spell-ability effect"
        );

        // Revert-failing assertion: if the gate's spell-object X handling
        // (Some(cast_facts) branch + object_level_x) is reverted, the NoOp
        // spell ability carries no X and this flips to a non-reject.
        assert_reject(
            &verdict_for_cast_with_facts(&state, obj, card),
            "x_cast_zero_no_op",
        );
    }

    #[test]
    fn etb_x_counter_creature_max_x_one_not_rejected() {
        // Reach-guard for the row above: two mana → max X = 1 → the creature
        // enters as a 1/1, a real body, so the gate stands down (proves the
        // X=0 rejection is not vacuous). `max_x_value != 0` short-circuits
        // before the payoff walk even runs.
        let mut state = base_state();
        let (obj, card) = etb_x_counter_creature(&mut state);
        add_pool(&mut state, AI, 2);
        assert_not_reject(&verdict_for_cast_with_facts(&state, obj, card));
    }
}
