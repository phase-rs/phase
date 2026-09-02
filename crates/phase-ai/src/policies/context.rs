use engine::ai_support::{AiDecisionContext, CandidateAction};
use engine::game::ability_utils::modal_spell_mode_ability_refs;
use engine::game::game_object::GameObject;
use engine::game::players::is_opponent;
use engine::game::targeting::find_legal_targets;
use engine::types::ability::{AbilityDefinition, Effect, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

use crate::cast_facts::{
    cast_facts_for_action, collect_definition_effects, collect_definition_effects_with,
    effect_profile_for_action, effective_activated_ability, CastFacts, EffectProfile, ModeWalk,
};
use crate::config::{AiConfig, PolicyPenalties};
use crate::eval::{strategic_intent, StrategicIntent};
#[cfg(test)]
use engine::types::game_state::CastPaymentMode;

/// Position of the node being scored within the current AI decision's search
/// tree. `Root` is the node the AI will actually commit an action at
/// (`score_candidates_core`); `Lookahead` is any hypothetical node inside beam
/// alpha-beta or rollout. Expensive policies (board-wide affordability sweeps,
/// `find_legal_targets`, `SimulationFilter` clones) should run their full
/// analysis only at `Root` via [`PolicyContext::at_root`] and return neutral in
/// lookahead, where the resulting-state eval already accounts for the action.
/// Mirrors the `deadline`/projection-budget self-gating precedent, but is a
/// per-node field (not an `AiContext` value) because depth varies per node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDepth {
    Root,
    Lookahead,
}

pub struct PolicyContext<'a> {
    pub state: &'a GameState,
    pub decision: &'a AiDecisionContext,
    pub candidate: &'a CandidateAction,
    pub ai_player: PlayerId,
    pub config: &'a AiConfig,
    pub context: &'a crate::context::AiContext,
    pub cast_facts: Option<CastFacts<'a>>,
    pub search_depth: SearchDepth,
}

/// Batch-constant scoring inputs for [`super::registry::PolicyRegistry::priors`] —
/// every value that stays fixed across all candidates in a single `priors`
/// call, as opposed to `candidates` itself (what's being scored). Grouping
/// these keeps `priors` under clippy's argument-count limit; every field
/// flows unchanged into the per-candidate [`PolicyContext`] built inside the
/// scoring loop. `search_depth` stays a distinct field here (not folded into
/// `AiContext`) for the same reason it's distinct on `PolicyContext`: it
/// varies per search node, unlike the ambient `AiContext`.
pub struct PriorsEnv<'a> {
    pub state: &'a GameState,
    pub decision: &'a AiDecisionContext,
    pub ai_player: PlayerId,
    pub config: &'a AiConfig,
    pub context: &'a crate::context::AiContext,
    pub search_depth: SearchDepth,
}

impl<'a> PolicyContext<'a> {
    pub fn strategic_intent(&self) -> StrategicIntent {
        strategic_intent(self.state, self.ai_player)
    }

    pub fn penalties(&self) -> &PolicyPenalties {
        &self.config.policy_penalties
    }

    /// True when the top-level wall-clock deadline has already elapsed.
    /// Policies doing non-essential expensive work (opponent-turn
    /// projections, deep synergy sweeps) should short-circuit via this
    /// rather than threading the raw `Deadline` everywhere.
    pub fn deadline_expired(&self) -> bool {
        self.context.deadline.expired()
    }

    /// True when an uncached multi-turn projection is affordable given the
    /// remaining wall-clock budget. The threshold is
    /// `SearchConfig::projection_min_budget_ms` (tunable per difficulty);
    /// policies that project should gate their work behind this helper so
    /// the tightest-budget path (Medium, 1500ms) doesn't pay the ~1.5s
    /// simulation cost and blow its own budget.
    ///
    /// The `remaining().is_none_or(..)` resolving to `true` on a
    /// `Deadline::none()` deadline is deliberate and load-bearing: measurement
    /// runs have no wall clock and MUST still take projections, so `cargo
    /// ai-gate` measures the same policy production runs. Changing it to
    /// `is_some_and` would pin `velocity_score` to 0.0 for every uncached
    /// projection in the gate — a far larger baseline move than any wall-clock
    /// fix, measuring a policy that never ships.
    ///
    /// One production path also reaches here with a never-overwritten
    /// `Deadline::none()`: `search::emit_decision_trace` builds its `AiContext`
    /// via `build_ai_context_with_session` (which initializes `deadline` to
    /// `none()`) and never routes through `PlannerServices::with_deadline`. That
    /// path is diagnostic (gated on `phase_ai::decision_trace` DEBUG) and feeds
    /// the duel suite's attribution mode, so a flip would also silently change
    /// trace output.
    pub fn can_afford_projection(&self) -> bool {
        if self.context.deadline.expired() {
            return false;
        }
        let floor = self.config.search.projection_min_budget_ms;
        if floor == 0 {
            return true;
        }
        self.context
            .deadline
            .remaining()
            .is_none_or(|r| r.as_millis() >= floor)
    }

    /// True when this is the node the AI will commit an action at. Policies whose
    /// only correctness role is stopping a *committed* action (and whose analysis
    /// is board-wide/expensive) should gate that work behind this and return
    /// neutral otherwise — the lookahead eval already dominates no-op lines.
    pub fn at_root(&self) -> bool {
        matches!(self.search_depth, SearchDepth::Root)
    }

    pub fn source_object(&self) -> Option<&'a GameObject> {
        match &self.candidate.action {
            GameAction::CastSpell { card_id, .. } => self
                .state
                .objects
                .values()
                .find(|object| object.card_id == *card_id),
            GameAction::ActivateAbility { source_id, .. } => self.state.objects.get(source_id),
            // During target selection, the source is in the pending cast or trigger.
            GameAction::ChooseTarget { .. } | GameAction::SelectTargets { .. } => {
                match &self.decision.waiting_for {
                    WaitingFor::TargetSelection { pending_cast, .. } => {
                        self.state.objects.get(&pending_cast.object_id)
                    }
                    WaitingFor::MultiTargetSelection {
                        pending_ability, ..
                    } => self.state.objects.get(&pending_ability.source_id),
                    WaitingFor::TriggerTargetSelection { source_id, .. } => {
                        source_id.as_ref().and_then(|id| self.state.objects.get(id))
                    }
                    _ => None,
                }
            }
            // CR 700.2a / CR 700.2b: mode selection is a step of casting the
            // spell or putting the ability on the stack, so the source is the
            // object being cast (a modal spell) or the ability's own source (a
            // modal activated/triggered ability).
            GameAction::SelectModes { .. } => match &self.decision.waiting_for {
                WaitingFor::ModeChoice { pending_cast, .. } => {
                    self.state.objects.get(&pending_cast.object_id)
                }
                WaitingFor::AbilityModeChoice { source_id, .. } => {
                    self.state.objects.get(source_id)
                }
                _ => None,
            },
            _ => None,
        }
    }

    pub fn effects(&self) -> Vec<&'a Effect> {
        // If we're casting/activating, get effects from the source object
        match &self.candidate.action {
            GameAction::CastSpell { .. } => {
                return self
                    .source_object()
                    .into_iter()
                    .flat_map(|object| object.abilities.iter().flat_map(collect_definition_effects))
                    .collect();
            }
            // CR 700.2a: an activation announces the ability; if it is modal the
            // mode is chosen at the separate `AbilityModeChoice` prompt, which
            // the `SelectModes` arm below scores on its own. Reading every
            // printed mode here would price one activation as the CONJUNCTION of
            // all its modes — an Umezawa's Jitte activation would carry a
            // combat trick and a no-opposing-creature whiff even when the
            // intended mode is "gain 2 life". Hence `RootOnly`; the `CastSpell`
            // arm keeps `All` (CR 601.2b: at cast-commit no mode is chosen yet,
            // which is what `cast_facts` already reports to cast-time policies).
            GameAction::ActivateAbility {
                ability_index,
                source_id,
            } => {
                return self
                    .state
                    .objects
                    .get(source_id)
                    .and_then(|object| object.abilities.get(*ability_index))
                    .map(|ability| collect_definition_effects_with(ability, ModeWalk::RootOnly))
                    .unwrap_or_default();
            }
            // CR 601.2b + CR 700.2a: the mode IS chosen here, so report exactly
            // the branches this candidate commits to. Reporting every printed
            // mode instead would make every `SelectModes` candidate for one
            // card carry identical effects, and no policy could tell a
            // harmful mode from a beneficial one.
            //
            // This arm is plumbing: it makes modes visible to EVERY policy that
            // reads `effects()` at a `ModeChoice` / `AbilityModeChoice` prompt,
            // where the whole set previously came back empty and every mode
            // scored identically. `anti_self_harm::score_selected_modes` is the
            // one consumer written against it so far; the rest of the policy
            // set simply stops being blind here.
            GameAction::SelectModes { indices } => {
                return selected_mode_abilities(self.state, &self.decision.waiting_for, indices)
                    .into_iter()
                    .flat_map(collect_definition_effects)
                    .collect();
            }
            _ => {}
        }

        // During target selection, extract effects from the pending cast/ability/trigger
        match &self.decision.waiting_for {
            WaitingFor::TargetSelection { pending_cast, .. } => {
                collect_ability_effects(&pending_cast.ability)
            }
            WaitingFor::MultiTargetSelection {
                pending_ability, ..
            } => collect_ability_effects(pending_ability),
            WaitingFor::TriggerTargetSelection { .. } => self
                .state
                .pending_trigger
                .as_ref()
                .map(|t| collect_ability_effects(&t.ability))
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    pub fn cast_facts(&self) -> Option<CastFacts<'a>> {
        self.cast_facts
            .clone()
            .or_else(|| match &self.candidate.action {
                GameAction::CastSpell { .. } => {
                    cast_facts_for_action(self.state, &self.candidate.action, self.ai_player)
                }
                _ => None,
            })
    }

    /// Exact activated ability represented by this candidate, including
    /// runtime-granted abilities in the engine's production index space.
    pub fn effective_activated_ability(&self) -> Option<AbilityDefinition> {
        effective_activated_ability(self.state, &self.candidate.action)
    }

    /// Effect-level profile for both spells and activated abilities.
    /// For spells, delegates to CastFacts (includes ETB/replacement effects).
    /// For activated abilities, scans the specific ability's effect chain.
    pub fn effect_profile(&self) -> Option<EffectProfile> {
        if let Some(facts) = &self.cast_facts {
            return Some(facts.profile.clone());
        }
        effect_profile_for_action(self.state, &self.candidate.action, self.ai_player)
    }

    /// CR 702.11 / 702.16 / 702.18: True when `filter` has at least one legal
    /// opponent-controlled creature target, per the engine's targeting legality.
    pub(crate) fn has_legal_opponent_creature_target(
        &self,
        filter: &TargetFilter,
        source_id: ObjectId,
        mut is_relevant: impl FnMut(ObjectId) -> bool,
    ) -> bool {
        find_legal_targets(self.state, filter, self.ai_player, source_id)
            .into_iter()
            .any(|target| match target {
                TargetRef::Object(id) => self.state.objects.get(&id).is_some_and(|object| {
                    is_opponent(self.state, self.ai_player, object.controller)
                        && object.card_types.core_types.contains(&CoreType::Creature)
                        && is_relevant(id)
                }),
                TargetRef::Player(_) => false,
            })
    }

    /// Does the pending spell carry an inherently-mass effect (`DestroyAll`,
    /// CR 701.8) with a non-empty OPPONENT population under the resolver's
    /// NON-targeted semantics (CR 115.10a; team-aware via `is_opponent`)? The
    /// engine's tactical gate (redundant-removal suppression) and the
    /// cast-commit anti-whiff scoring both consult this BEFORE any
    /// target-legality gate: a wipe line that clears an un-targetable
    /// (hexproof/protected) population is a real removal line, not a whiff.
    pub(crate) fn has_opposing_mass_population(&self) -> bool {
        super::removal_lethality::has_opposing_mass_population(self)
    }
}

/// Walk a `ResolvedAbility`'s chain, collecting every effect it can produce.
///
/// A `ResolvedAbility` has only `sub_ability` and `else_ability` — the modes a
/// modal spell/ability commits to are linearised into one `sub_ability` chain
/// before resolution, so there is no `mode_abilities` branch to walk here. The
/// `else_ability` half is the "if you don't / otherwise" leg, which is a real
/// outcome of the same ability and must be visible to the policies that read
/// [`PolicyContext::effects`] during target selection. Structural twin of
/// `cast_facts::collect_definition_effects`, which does the same walk one level
/// up on `AbilityDefinition`.
pub(crate) fn collect_ability_effects(ability: &ResolvedAbility) -> Vec<&Effect> {
    let mut effects = Vec::new();
    push_resolved_effects(&mut effects, ability);
    effects
}

fn push_resolved_effects<'a>(effects: &mut Vec<&'a Effect>, ability: &'a ResolvedAbility) {
    effects.push(&ability.effect);
    if let Some(sub_ability) = &ability.sub_ability {
        push_resolved_effects(effects, sub_ability);
    }
    if let Some(else_ability) = &ability.else_ability {
        push_resolved_effects(effects, else_ability);
    }
}

/// CR 700.2: the modes a pending `SelectModes` decision is choosing among.
///
/// A modal SPELL carries them as the spell-kind abilities of the object being
/// cast (`modal_spell_mode_ability_refs`, the engine's authority, which
/// `handle_select_modes` indexes with the same `indices`); a modal activated or
/// triggered ABILITY carries them on the waiting payload.
///
/// Ordering invariant for the spell arm: `ModeChoice` indices address the FULL
/// `obj.abilities` list, while `modal_spell_mode_ability_refs` filters to
/// `AbilityKind::Spell`. The two index spaces coincide only while no non-Spell
/// ability precedes a Spell ability on a modal card — no modal card in the
/// current card data violates that. This function does NOT renumber; callers
/// stay in the engine's index space.
fn pending_mode_abilities<'a>(
    state: &'a GameState,
    waiting_for: &'a WaitingFor,
) -> Vec<&'a AbilityDefinition> {
    match waiting_for {
        WaitingFor::ModeChoice { pending_cast, .. } => state
            .objects
            .get(&pending_cast.object_id)
            .map(|obj| modal_spell_mode_ability_refs(obj).collect())
            .unwrap_or_default(),
        WaitingFor::AbilityModeChoice { mode_abilities, .. } => mode_abilities.iter().collect(),
        _ => Vec::new(),
    }
}

/// CR 601.2b / CR 700.2a: exactly the modes a `SelectModes { indices }`
/// candidate commits to, in the order the candidate names them. Out-of-range
/// indices are dropped rather than panicking — the candidate list is the
/// engine's, but a policy must never abort a search node on a stale index.
pub(crate) fn selected_mode_abilities<'a>(
    state: &'a GameState,
    waiting_for: &'a WaitingFor,
    indices: &[usize],
) -> Vec<&'a AbilityDefinition> {
    let modes = pending_mode_abilities(state, waiting_for);
    indices
        .iter()
        .filter_map(|index| modes.get(*index).copied())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use engine::ai_support::{ActionMetadata, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, EffectKind, ModalChoice, PtValue, QuantityExpr,
        TargetFilter, TypedFilter,
    };
    use engine::types::format::FormatConfig;
    use engine::types::game_state::{PendingCast, TargetEffectDetail, TargetSelectionSlot};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::ManaCost;
    use engine::types::zones::Zone;

    fn gain_life(amount: i32) -> Effect {
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: amount },
            player: TargetFilter::Controller,
        }
    }

    fn draw(count: i32) -> Effect {
        Effect::Draw {
            count: QuantityExpr::Fixed { value: count },
            target: TargetFilter::Controller,
        }
    }

    fn destroy_any() -> Effect {
        Effect::Destroy {
            target: TargetFilter::Any,
            cant_regenerate: false,
        }
    }

    /// A modal spell on the stack whose printed modes are its spell-kind
    /// abilities (`modal_spell_mode_ability_refs`, the engine's index space),
    /// plus the matching `WaitingFor::ModeChoice`.
    fn modal_spell_decision(
        state: &mut GameState,
        modes: Vec<AbilityDefinition>,
    ) -> (ObjectId, AiDecisionContext) {
        let mode_count = modes.len();
        let card_id = CardId(77);
        let spell_id = create_object(
            state,
            card_id,
            PlayerId(0),
            "Modal Specimen".to_string(),
            Zone::Stack,
        );
        let object = state.objects.get_mut(&spell_id).unwrap();
        *Arc::make_mut(&mut object.abilities) = modes;

        let resolved = ResolvedAbility::new(gain_life(1), Vec::new(), spell_id, PlayerId(0));
        let pending_cast = PendingCast::new(spell_id, card_id, resolved, ManaCost::zero());
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::ModeChoice {
                player: PlayerId(0),
                modal: ModalChoice {
                    min_choices: 1,
                    max_choices: 1,
                    mode_count,
                    ..ModalChoice::default()
                },
                pending_cast: Box::new(pending_cast),
                unavailable_modes: Vec::new(),
            },
            candidates: Vec::new(),
        };
        (spell_id, decision)
    }

    fn select_modes_candidate(indices: Vec<usize>) -> CandidateAction {
        CandidateAction {
            action: GameAction::SelectModes { indices },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Selection),
        }
    }

    fn policy_ctx<'a>(
        state: &'a GameState,
        decision: &'a AiDecisionContext,
        candidate: &'a CandidateAction,
        config: &'a AiConfig,
        context: &'a crate::context::AiContext,
    ) -> PolicyContext<'a> {
        PolicyContext {
            state,
            decision,
            candidate,
            ai_player: PlayerId(0),
            config,
            context,
            cast_facts: None,
            search_depth: SearchDepth::Root,
        }
    }

    /// CR 601.2b + CR 700.2a: the modes ARE chosen at this prompt, so
    /// `effects()` must report exactly the branches the candidate commits to.
    /// Before mode visibility landed this returned an empty Vec for every
    /// `SelectModes` candidate, so no policy could tell a card's harmful mode
    /// from its beneficial one.
    #[test]
    fn select_modes_effects_are_only_the_selected_modes() {
        let mut state = GameState::new_two_player(42);
        let config = AiConfig::default();
        let context = crate::context::AiContext::empty(&config.weights);

        // Witherbloom Charm shape: draw / gain life / destroy.
        let mut draw_mode = AbilityDefinition::new(AbilityKind::Spell, draw(2));
        draw_mode.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            gain_life(1),
        )));
        let (_, decision) = modal_spell_decision(
            &mut state,
            vec![
                draw_mode,
                AbilityDefinition::new(AbilityKind::Spell, gain_life(5)),
                AbilityDefinition::new(AbilityKind::Spell, destroy_any()),
            ],
        );

        let destroy_candidate = select_modes_candidate(vec![2]);
        let destroy_effects =
            policy_ctx(&state, &decision, &destroy_candidate, &config, &context).effects();
        assert_eq!(
            destroy_effects.len(),
            1,
            "only the chosen mode's chain may be reported, got {destroy_effects:?}"
        );
        assert!(matches!(destroy_effects[0], Effect::Destroy { .. }));

        let draw_candidate = select_modes_candidate(vec![0]);
        let draw_effects =
            policy_ctx(&state, &decision, &draw_candidate, &config, &context).effects();
        assert_eq!(
            draw_effects.len(),
            2,
            "the chosen mode's own sub-ability chain is part of that mode"
        );
        assert!(matches!(draw_effects[0], Effect::Draw { .. }));
        assert!(matches!(draw_effects[1], Effect::GainLife { .. }));

        let both = select_modes_candidate(vec![1, 2]);
        let both_effects = policy_ctx(&state, &decision, &both, &config, &context).effects();
        assert_eq!(both_effects.len(), 2, "a two-mode selection reports both");

        let out_of_range = select_modes_candidate(vec![9]);
        assert!(
            policy_ctx(&state, &decision, &out_of_range, &config, &context)
                .effects()
                .is_empty(),
            "a stale index is dropped, never a panic inside a search node"
        );
    }

    /// CR 700.2a / CR 700.2b: mode selection belongs to the spell being cast or
    /// to the modal ability's own source, so both prompt shapes must resolve a
    /// source object (policies that read the card — legend rule, aura polarity,
    /// ward — are otherwise blind at this prompt).
    #[test]
    fn select_modes_source_object_resolves_for_both_variants() {
        let mut state = GameState::new_two_player(42);
        let config = AiConfig::default();
        let context = crate::context::AiContext::empty(&config.weights);

        let (spell_id, decision) = modal_spell_decision(
            &mut state,
            vec![AbilityDefinition::new(AbilityKind::Spell, gain_life(5))],
        );
        let candidate = select_modes_candidate(vec![0]);
        assert_eq!(
            policy_ctx(&state, &decision, &candidate, &config, &context)
                .source_object()
                .map(|object| object.id),
            Some(spell_id),
            "ModeChoice resolves through the pending cast"
        );

        let permanent_id = create_object(
            &mut state,
            CardId(78),
            PlayerId(0),
            "Modal Ability Source".to_string(),
            Zone::Battlefield,
        );
        let ability_decision = AiDecisionContext {
            waiting_for: WaitingFor::AbilityModeChoice {
                player: PlayerId(0),
                modal: ModalChoice {
                    min_choices: 1,
                    max_choices: 1,
                    mode_count: 2,
                    ..ModalChoice::default()
                },
                source_id: permanent_id,
                mode_abilities: vec![
                    AbilityDefinition::new(AbilityKind::Activated, gain_life(3)),
                    AbilityDefinition::new(AbilityKind::Activated, destroy_any()),
                ],
                is_activated: true,
                ability_index: Some(0),
                ability_cost: None,
                unavailable_modes: Vec::new(),
            },
            candidates: Vec::new(),
        };
        let ability_candidate = select_modes_candidate(vec![1]);
        let ctx = policy_ctx(
            &state,
            &ability_decision,
            &ability_candidate,
            &config,
            &context,
        );
        assert_eq!(
            ctx.source_object().map(|object| object.id),
            Some(permanent_id),
            "AbilityModeChoice resolves through source_id"
        );
        let effects = ctx.effects();
        assert_eq!(effects.len(), 1);
        assert!(
            matches!(effects[0], Effect::Destroy { .. }),
            "the ability variant reads its modes off the waiting payload"
        );
    }

    /// One definition walker for the whole crate: at cast-commit `effects()`
    /// must report the same set `cast_facts::collect_definition_effects` does,
    /// including the `else_ability` (CR 608.2c "otherwise" leg) and
    /// `mode_abilities` (CR 601.2b — no mode is chosen yet at announcement)
    /// branches that the private walker this replaced silently dropped.
    #[test]
    fn definition_walker_is_the_cast_facts_walker() {
        let mut state = GameState::new_two_player(42);
        let config = AiConfig::default();
        let context = crate::context::AiContext::empty(&config.weights);

        let source_id = create_object(
            &mut state,
            CardId(80),
            PlayerId(0),
            "Branching Spell".to_string(),
            Zone::Hand,
        );
        let mut ability = AbilityDefinition::new(AbilityKind::Spell, draw(1));
        ability.else_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            gain_life(2),
        )));
        ability.mode_abilities = vec![AbilityDefinition::new(AbilityKind::Spell, destroy_any())];
        *Arc::make_mut(&mut state.objects.get_mut(&source_id).unwrap().abilities) = vec![ability];

        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: source_id,
                card_id: CardId(80),
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Spell),
        };
        let effects = policy_ctx(&state, &decision, &candidate, &config, &context).effects();

        assert_eq!(effects.len(), 3, "got {effects:?}");
        assert!(matches!(effects[0], Effect::Draw { .. }));
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::GainLife { .. })),
            "the else_ability leg must be visible"
        );
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::Destroy { .. })),
            "the mode_abilities branch must be visible"
        );
    }

    /// CR 700.2a: activating a modal ability announces the ability; the mode is
    /// chosen at the separate `AbilityModeChoice` prompt. So the activation step
    /// must NOT read the unchosen modes — Umezawa's Jitte activated at main
    /// phase would otherwise carry a Pump (combat_trick) and a Destroy
    /// (no-opposing-creature whiff) even when the intended mode is "gain 2
    /// life". The chosen modes stay visible at the `SelectModes` prompt.
    #[test]
    fn activate_ability_effects_exclude_unchosen_modes() {
        let mut state = GameState::new_two_player(42);
        let config = AiConfig::default();
        let context = crate::context::AiContext::empty(&config.weights);

        // Umezawa's Jitte shape: one activated root ("Remove a counter:")
        // carrying three modes.
        let source_id = create_object(
            &mut state,
            CardId(81),
            PlayerId(0),
            "Umezawa's Jitte".to_string(),
            Zone::Battlefield,
        );
        let mut jitte = AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp);
        jitte.mode_abilities = vec![
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Pump {
                    power: PtValue::Fixed(2),
                    toughness: PtValue::Fixed(2),
                    target: TargetFilter::Typed(TypedFilter::creature()),
                },
            ),
            AbilityDefinition::new(AbilityKind::Activated, destroy_any()),
            AbilityDefinition::new(AbilityKind::Activated, gain_life(2)),
        ];
        *Arc::make_mut(&mut state.objects.get_mut(&source_id).unwrap().abilities) =
            vec![jitte.clone()];

        let priority = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let activate = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id,
                ability_index: 0,
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Ability),
        };
        let activation_effects =
            policy_ctx(&state, &priority, &activate, &config, &context).effects();
        assert_eq!(
            activation_effects.len(),
            1,
            "the activation step sees the root chain only, got {activation_effects:?}"
        );
        assert!(matches!(activation_effects[0], Effect::NoOp));

        // The chosen mode is still visible one step later, at its own prompt.
        let mode_decision = AiDecisionContext {
            waiting_for: WaitingFor::AbilityModeChoice {
                player: PlayerId(0),
                modal: ModalChoice {
                    min_choices: 1,
                    max_choices: 1,
                    mode_count: 3,
                    ..ModalChoice::default()
                },
                source_id,
                mode_abilities: jitte.mode_abilities.clone(),
                is_activated: true,
                ability_index: Some(0),
                ability_cost: None,
                unavailable_modes: Vec::new(),
            },
            candidates: Vec::new(),
        };
        let gain_life_mode = select_modes_candidate(vec![2]);
        let mode_effects =
            policy_ctx(&state, &mode_decision, &gain_life_mode, &config, &context).effects();
        assert_eq!(mode_effects.len(), 1, "got {mode_effects:?}");
        assert!(
            matches!(mode_effects[0], Effect::GainLife { .. }),
            "the gain-life mode must not drag the Pump and Destroy modes with it"
        );
    }

    /// CR 608.2c: a resolving ability's "otherwise" leg is a real outcome of
    /// that ability, so target-selection policies must see it.
    #[test]
    fn resolved_walker_includes_else_ability() {
        let state = GameState::new_two_player(42);
        let config = AiConfig::default();
        let context = crate::context::AiContext::empty(&config.weights);

        let ability = ResolvedAbility::new(draw(1), Vec::new(), ObjectId(1), PlayerId(0))
            .sub_ability(ResolvedAbility::new(
                gain_life(2),
                Vec::new(),
                ObjectId(1),
                PlayerId(0),
            ))
            .else_ability(ResolvedAbility::new(
                destroy_any(),
                Vec::new(),
                ObjectId(1),
                PlayerId(0),
            ));
        let pending_cast = PendingCast::new(ObjectId(1), CardId(1), ability, ManaCost::zero());
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: PlayerId(0),
                pending_cast: Box::new(pending_cast),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: vec![],
                    optional: false,
                    chooser: None,
                    effect_kind: EffectKind::NoOp,
                    effect_detail: TargetEffectDetail::None,
                }],
                mode_labels: Vec::new(),
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget { target: None },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Target),
        };
        let effects = policy_ctx(&state, &decision, &candidate, &config, &context).effects();

        assert_eq!(effects.len(), 3, "got {effects:?}");
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, Effect::Destroy { .. })),
            "the else_ability leg must be visible"
        );
    }

    #[test]
    fn effects_returns_pending_cast_during_target_selection() {
        let state = GameState::new_two_player(42);
        let config = AiConfig::default();

        let ability = ResolvedAbility::new(
            Effect::Pump {
                power: PtValue::Fixed(3),
                toughness: PtValue::Fixed(3),
                target: TargetFilter::Any,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        let pending_cast = PendingCast::new(ObjectId(1), CardId(1), ability, ManaCost::zero());
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: PlayerId(0),
                pending_cast: Box::new(pending_cast),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: vec![],
                    optional: false,
                    chooser: None,
                    effect_kind: EffectKind::NoOp,
                    effect_detail: TargetEffectDetail::None,
                }],
                mode_labels: Vec::new(),
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(engine::types::ability::TargetRef::Object(ObjectId(2))),
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Target),
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        let effects = ctx.effects();
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], Effect::Pump { .. }));
    }

    #[test]
    fn effects_walks_sub_ability_chain() {
        let state = GameState::new_two_player(42);
        let config = AiConfig::default();

        let sub = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: engine::types::ability::TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::Pump {
                power: PtValue::Fixed(2),
                toughness: PtValue::Fixed(2),
                target: TargetFilter::Any,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        )
        .sub_ability(sub);

        let pending_cast = PendingCast::new(ObjectId(1), CardId(1), ability, ManaCost::zero());
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: PlayerId(0),
                pending_cast: Box::new(pending_cast),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: vec![],
                    optional: false,
                    chooser: None,
                    effect_kind: EffectKind::NoOp,
                    effect_detail: TargetEffectDetail::None,
                }],
                mode_labels: Vec::new(),
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget { target: None },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Target),
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        let effects = ctx.effects();
        assert_eq!(
            effects.len(),
            2,
            "Should collect both main and sub-ability effects"
        );
        assert!(matches!(effects[0], Effect::Pump { .. }));
        assert!(matches!(effects[1], Effect::Draw { .. }));
    }

    #[test]
    fn cast_spell_effects_walk_sub_ability_chain() {
        let mut state = GameState::new_two_player(42);
        let config = AiConfig::default();
        let card_id = CardId(1);
        let mut ability = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Pump {
                power: PtValue::Fixed(2),
                toughness: PtValue::Fixed(2),
                target: TargetFilter::Any,
            },
        );
        ability.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: engine::types::ability::TargetFilter::Controller,
            },
        )));
        let spell_id = create_object(
            &mut state,
            card_id,
            PlayerId(0),
            "Test Spell".to_string(),
            Zone::Hand,
        );
        state.objects.get_mut(&spell_id).unwrap().abilities = Arc::new(vec![ability]);

        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id,
                targets: Vec::new(),

                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Spell),
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        let effects = ctx.effects();
        assert_eq!(effects.len(), 2);
        assert!(matches!(effects[0], Effect::Pump { .. }));
        assert!(matches!(effects[1], Effect::Draw { .. }));
    }

    #[test]
    fn cast_facts_returns_spell_cast_facts_without_changing_effects() {
        let mut state = GameState::new_two_player(42);
        let config = AiConfig::default();
        let object_id = create_object(
            &mut state,
            CardId(9),
            PlayerId(0),
            "Test Creature".to_string(),
            Zone::Hand,
        );
        let object = state.objects.get_mut(&object_id).unwrap();
        object
            .card_types
            .core_types
            .push(engine::types::card_type::CoreType::Creature);
        Arc::make_mut(&mut object.abilities).push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: engine::types::ability::TargetFilter::Controller,
            },
        ));
        object.trigger_definitions.push(
            engine::types::ability::TriggerDefinition::new(
                engine::types::triggers::TriggerMode::ChangesZone,
            )
            .valid_card(TargetFilter::SelfRef)
            .destination(Zone::Battlefield)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Destroy {
                    target: TargetFilter::Any,
                    cant_regenerate: false,
                },
            )),
        );

        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id,
                card_id: CardId(9),
                targets: Vec::new(),

                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Spell),
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        assert_eq!(ctx.effects().len(), 1);
        let facts = ctx.cast_facts().expect("cast facts");
        assert_eq!(facts.immediate_etb_triggers.len(), 1);
        assert!(facts.has_direct_removal_text());
    }

    #[test]
    fn legal_opponent_creature_target_is_team_aware() {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        let source_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Test Spell".to_string(),
            Zone::Hand,
        );
        let teammate_id = create_object(
            &mut state,
            CardId(11),
            PlayerId(1),
            "Teammate Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&teammate_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: source_id,
                card_id: CardId(10),
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Spell),
        };
        let ai_context = crate::context::AiContext::empty(&config.weights);
        let creature_filter = TargetFilter::Typed(TypedFilter::creature());

        {
            let ctx = PolicyContext {
                state: &state,
                decision: &decision,
                candidate: &candidate,
                ai_player: PlayerId(0),
                config: &config,
                context: &ai_context,
                cast_facts: None,
                search_depth: SearchDepth::Root,
            };
            assert!(
                !ctx.has_legal_opponent_creature_target(&creature_filter, source_id, |_| true),
                "P1's legal creature target is P0's teammate in 2HG, not an opponent"
            );
        }

        let opponent_id = create_object(
            &mut state,
            CardId(12),
            PlayerId(2),
            "Opponent Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&opponent_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &ai_context,
            cast_facts: None,
            search_depth: SearchDepth::Root,
        };
        assert!(
            ctx.has_legal_opponent_creature_target(&creature_filter, source_id, |_| true),
            "P2's legal creature target is P0's opponent in 2HG"
        );
    }

    fn deadline_test_ctx<'a>(
        state: &'a GameState,
        decision: &'a AiDecisionContext,
        candidate: &'a CandidateAction,
        config: &'a AiConfig,
        context: &'a crate::context::AiContext,
    ) -> PolicyContext<'a> {
        PolicyContext {
            state,
            decision,
            candidate,
            ai_player: PlayerId(0),
            config,
            context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        }
    }

    #[test]
    fn deadline_expired_gates_projection() {
        // When the wall-clock deadline is already blown, projection-gated
        // policies must short-circuit — `can_afford_projection` returns false
        // so callers (velocity_score etc.) skip `get_or_project` and don't
        // blow past the user-visible turn-time budget on an uncached sim.
        let state = GameState::new_two_player(42);
        let config = AiConfig::default();
        let mut ai_ctx = crate::context::AiContext::empty(&config.weights);
        ai_ctx.deadline = engine::util::Deadline::after(0);
        std::thread::sleep(std::time::Duration::from_millis(2));

        let ability = ResolvedAbility::new(
            Effect::Pump {
                power: PtValue::Fixed(2),
                toughness: PtValue::Fixed(2),
                target: TargetFilter::Any,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        let pending_cast = PendingCast::new(ObjectId(1), CardId(1), ability, ManaCost::zero());
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: PlayerId(0),
                pending_cast: Box::new(pending_cast),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: vec![],
                    optional: false,
                    chooser: None,
                    effect_kind: EffectKind::NoOp,
                    effect_detail: TargetEffectDetail::None,
                }],
                mode_labels: Vec::new(),
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget { target: None },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Target),
        };
        let ctx = deadline_test_ctx(&state, &decision, &candidate, &config, &ai_ctx);

        assert!(ctx.deadline_expired(), "deadline should have expired");
        assert!(
            !ctx.can_afford_projection(),
            "expired deadline must disallow projection"
        );
    }

    #[test]
    fn fresh_deadline_allows_projection() {
        // Mirror of `deadline_expired_gates_projection`: with a healthy
        // remaining budget, `can_afford_projection` must return true so the
        // velocity signal still runs in the common case.
        let state = GameState::new_two_player(42);
        let config = AiConfig::default();
        let mut ai_ctx = crate::context::AiContext::empty(&config.weights);
        // 5s remaining — well above the default 500ms floor.
        ai_ctx.deadline = engine::util::Deadline::after(5_000);

        let ability = ResolvedAbility::new(
            Effect::Pump {
                power: PtValue::Fixed(2),
                toughness: PtValue::Fixed(2),
                target: TargetFilter::Any,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        let pending_cast = PendingCast::new(ObjectId(1), CardId(1), ability, ManaCost::zero());
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: PlayerId(0),
                pending_cast: Box::new(pending_cast),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: vec![],
                    optional: false,
                    chooser: None,
                    effect_kind: EffectKind::NoOp,
                    effect_detail: TargetEffectDetail::None,
                }],
                mode_labels: Vec::new(),
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget { target: None },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Target),
        };
        let ctx = deadline_test_ctx(&state, &decision, &candidate, &config, &ai_ctx);

        assert!(!ctx.deadline_expired());
        assert!(ctx.can_afford_projection());
    }

    #[test]
    fn zero_projection_floor_always_allows() {
        // Escape hatch: setting `projection_min_budget_ms = 0` forces the
        // policy to always attempt projection (used by difficulties with
        // ample budget, or by deterministic regression harnesses).
        let state = GameState::new_two_player(42);
        let mut config = AiConfig::default();
        config.search.projection_min_budget_ms = 0;

        let mut ai_ctx = crate::context::AiContext::empty(&config.weights);
        // Large budget keeps this deterministic under parallel test load —
        // with floor=0 the remaining time is never read, so any non-expired
        // deadline exercises the same branch.
        ai_ctx.deadline = engine::util::Deadline::after(60_000);

        let ability = ResolvedAbility::new(
            Effect::Pump {
                power: PtValue::Fixed(2),
                toughness: PtValue::Fixed(2),
                target: TargetFilter::Any,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        let pending_cast = PendingCast::new(ObjectId(1), CardId(1), ability, ManaCost::zero());
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: PlayerId(0),
                pending_cast: Box::new(pending_cast),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: vec![],
                    optional: false,
                    chooser: None,
                    effect_kind: EffectKind::NoOp,
                    effect_detail: TargetEffectDetail::None,
                }],
                mode_labels: Vec::new(),
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget { target: None },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Target),
        };
        let ctx = deadline_test_ctx(&state, &decision, &candidate, &config, &ai_ctx);

        // With floor=0, any non-expired deadline allows projection; only an
        // already-expired one blocks (covered by
        // `deadline_expired_gates_projection`).
        assert!(ctx.can_afford_projection());
    }
}
