//! Ritual-with-no-sink veto (Dark Ritual class).
//!
//! Report (Discord thread 1544203436172648468): the AI casts Dark Ritual at the
//! opponent's end step with nothing to spend the mana on. CR 106.4 — each
//! player's mana pool empties at the end of each step and phase — so the three
//! black mana are lost and the card is spent for nothing.
//!
//! Mechanism. Dark Ritual parses as an instant whose spell chain is
//! `Effect::Mana { produced: Fixed [B,B,B] }`, which
//! `features::mana_ramp::is_ritual_parts` classifies as a ritual (CR 605.5b: a
//! spell is never a mana ability). Every arm of `ramp_timing` that sees that
//! shape reasons about *whether* to ramp — on-curve, or unreachable threats in
//! hand — and never about whether the produced mana has a sink in this window;
//! `self_cost` prices `Effect::Mana` as a non-trivial payoff. So nothing
//! foreclosed the cast.
//!
//! Why a separate policy rather than an arm of `ramp_timing`. That policy's
//! `activation` opts out below a deck ramp-commitment floor and after turn 4.
//! The waste this veto describes is a property of the *window*, not of the
//! deck's ramp profile or the turn number, so it must not inherit those gates.
//!
//! Why `Reject` and not a penalty. Under the softmax a graduated penalty is a
//! rate: a candidate that is strictly value-negative would still be sampled at
//! some frequency. Only `Reject` (mapped to `NEG_INFINITY` by
//! `PolicyRegistry::score`) is a bound.
//!
//! The veto is deliberately biased toward standing down, because it is
//! categorical:
//!
//! * The reach estimate counts *sources*, not payable capacity —
//!   `zone_eval::available_mana` is a count of untapped mana sources plus pool,
//!   with no colour or capacity sweep. It therefore over-states what the AI can
//!   actually cast, which makes a hand card look reachable when it may not be:
//!   the error direction is "find a sink that isn't there", i.e. decline to
//!   veto. Same for a ritual whose `Effect::Mana` carries non-empty
//!   `restrictions` (Cabal Coffers-style "spend this only on…"): the restricted
//!   mana is counted toward reach as though it were free, again over-counting
//!   sinks. Both are the safe direction; do not "fix" either into a capacity
//!   solve, which would cost a feasibility sweep per candidate.
//! * A ritual whose output is not a fixed colour list — `Rite of Flame`'s
//!   graveyard-scaled second half, any `AnyOneColor`/`Colorless` quantity
//!   expression — is never vetoed at all, because its output is not knowable
//!   card-locally.
//! * Another ritual in hand is not counted as a sink: chaining rituals only
//!   defers the same question, so it must not be the thing that answers it.
//! * Storm/prowess payoffs on our own board (Aetherflux Reservoir, Guttersnipe,
//!   magecraft, prowess) make the cast itself the payoff, independent of what
//!   the mana buys. That is the last stand-down, and the only board walk.
//!
//! Perf: the card-local `is_ritual_parts` check and the output fold run first
//! and exit every non-ritual `CastSpell` candidate before any state read beyond
//! the candidate's own object. Only a confirmed, fixed-output ritual pays for
//! `available_mana` (one battlefield count), then a hand scan, then — only if
//! both fail — one battlefield walk. No `find_legal_targets`, no
//! `feasible_mana_capacity`, no state clone.

use engine::game::keywords::has_flash;
use engine::game::turn_control::turn_decision_maker;
use engine::types::ability::{AbilityDefinition, AbilityKind, Effect, ManaProduction};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::ability_chain::collect_chain_effects;
use crate::features::mana_ramp::is_ritual_parts;
use crate::features::spellslinger_prowess::{has_prowess_parts, is_cast_payoff_parts};
use crate::features::DeckFeatures;
use crate::zone_eval::available_mana;

pub struct RitualSinkPolicy;

impl TacticalPolicy for RitualSinkPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::RitualSink
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::CastSpell]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        // Any deck can hold a ritual, and the waste this vetoes is a property of
        // the window rather than of the deck's ramp commitment or the turn
        // number. `verdict` short-circuits on a card-local class check.
        // activation-constant: ritual-cast window check, deck-independent.
        Some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let neutral = |kind: &'static str| PolicyVerdict::neutral(PolicyReason::new(kind));

        // Only the plain hard-cast path. The alternative-cost family
        // (`CastSpellForFree`, `CastSpellAsMadness`, `CastSpellAsMiracle`, …)
        // is left neutral: those windows are not the reported one and each
        // carries its own reason to cast now that this policy does not model.
        let GameAction::CastSpell { object_id, .. } = &ctx.candidate.action else {
            return neutral("ritual_sink_na");
        };
        let Some(ritual) = ctx.state.objects.get(object_id) else {
            return neutral("ritual_sink_na");
        };

        // 1. Card-local class check, before anything that reads the board.
        if !is_ritual_parts(&ritual.card_types.core_types, &ritual.abilities) {
            return neutral("ritual_sink_na");
        }

        // 2. Card-local output fold. A non-fixed producer is not knowable here.
        let Some(produced) = fixed_mana_output(&ritual.abilities) else {
            return neutral("ritual_sink_output_unknown");
        };
        if produced == 0 {
            return neutral("ritual_sink_output_unknown");
        }

        // 3. What the pool can reach once the ritual has been paid for and has
        //    resolved. `saturating_sub` because `available_mana` counts sources
        //    rather than solving payment, so it can read below the ritual's own
        //    mana value in states where the cast is nonetheless legal.
        let reach = available_mana(ctx.state, ctx.ai_player)
            .saturating_sub(ritual.mana_cost.mana_value())
            .saturating_add(produced);

        // 4. Hand scan for something the mana could actually buy.
        if hand_has_sink(ctx, *object_id, reach) {
            return neutral("ritual_sink_present");
        }

        // 5. Own-battlefield payoff walk, last and only once 1–4 hold.
        if has_cast_payoff(ctx.state, ctx.ai_player) {
            return neutral("ritual_sink_cast_payoff");
        }

        // CR 106.4: the mana empties at the end of this step or phase, so a
        // ritual with nothing to spend it on burns a card for nothing.
        PolicyVerdict::reject(PolicyReason::new("ritual_no_sink").with_fact("reach", reach as i64))
    }
}

/// Total mana a spell's unconditional chain adds, or `None` when any
/// `Effect::Mana` in that chain is not a fixed colour list.
///
/// Only `AbilityKind::Spell` chains count — the same abilities `is_ritual_parts`
/// classified on. `ManaProduction::Fixed` is the one variant whose output is a
/// card-local constant; every other variant carries a `QuantityExpr` or a player
/// choice, so the fold stands down rather than guessing.
///
/// Multiple `Fixed` producers are summed. `collect_chain_effects` walks the
/// unconditional chain only (CR 601.2b: no mode is chosen yet at cast time), so
/// a conditional second half (Cabal Ritual's threshold "instead" parses as a
/// `sub_ability`) is double-counted — over-counting reach only makes a hand
/// card look reachable, which is the stand-down direction.
fn fixed_mana_output(abilities: &[AbilityDefinition]) -> Option<u32> {
    let mut total: u32 = 0;
    for ability in abilities.iter().filter(|a| a.kind == AbilityKind::Spell) {
        for effect in collect_chain_effects(ability) {
            let Effect::Mana { produced, .. } = effect else {
                continue;
            };
            let ManaProduction::Fixed { colors, .. } = produced else {
                return None;
            };
            total = total.saturating_add(colors.len() as u32);
        }
    }
    Some(total)
}

/// True when some nonland card in hand other than `ritual_id` costs at most
/// `reach` AND can legally be cast in this window.
///
/// CR 304.1 + CR 702.8a: an instant, or a card with flash, is castable whenever
/// the AI has priority, so it counts unconditionally. CR 307.1: everything else
/// is sorcery-speed and counts only during a main phase of the AI's own turn.
/// The empty-stack half of CR 307.1 is deliberately not required — CR 106.4
/// empties the pool at the end of the *phase*, so mana made while a spell is on
/// the stack is still there for a main-phase cast once the stack clears.
///
/// Another ritual is not a sink: chaining rituals defers the question this
/// policy is asking rather than answering it.
fn hand_has_sink(ctx: &PolicyContext<'_>, ritual_id: ObjectId, reach: u32) -> bool {
    let Some(player) = ctx.state.players.get(ctx.ai_player.0 as usize) else {
        return false;
    };
    let own_main_phase = turn_decision_maker(ctx.state) == ctx.ai_player
        && matches!(
            ctx.state.phase,
            Phase::PreCombatMain | Phase::PostCombatMain
        );
    player.hand.iter().any(|&oid| {
        if oid == ritual_id {
            return false;
        }
        let Some(obj) = ctx.state.objects.get(&oid) else {
            return false;
        };
        if obj.card_types.core_types.contains(&CoreType::Land) {
            return false;
        }
        if is_ritual_parts(&obj.card_types.core_types, &obj.abilities) {
            return false;
        }
        if obj.mana_cost.mana_value() > reach {
            return false;
        }
        obj.card_types.core_types.contains(&CoreType::Instant) || has_flash(obj) || own_main_phase
    })
}

/// True when the AI controls a battlefield permanent that pays off the *cast*
/// itself — prowess (CR 702.108a) or a caster-scoped spell-cast trigger
/// (CR 601.2i), the Guttersnipe / Aetherflux / magecraft shapes. For those, the
/// ritual's own cast is value even if the mana it makes is wasted.
fn has_cast_payoff(state: &GameState, ai_player: PlayerId) -> bool {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .any(|obj| {
            obj.controller == ai_player
                && (has_prowess_parts(&obj.keywords)
                    || is_cast_payoff_parts(
                        obj.trigger_definitions
                            .iter_unchecked()
                            .map(|entry| &entry.definition),
                    ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{ManaContribution, QuantityExpr, TargetFilter};
    use engine::types::game_state::{CastPaymentMode, WaitingFor};
    use engine::types::identifiers::CardId;
    use engine::types::keywords::Keyword;
    use engine::types::mana::{ManaColor, ManaCost};
    use engine::types::triggers::TriggerMode;
    use engine::types::zones::Zone;
    use engine::types::TriggerDefinition;
    use std::sync::Arc;

    use crate::config::AiConfig;
    use crate::context::AiContext;
    use crate::policies::registry::PolicyRegistry;

    const AI: PlayerId = PlayerId(0);
    const OPPONENT: PlayerId = PlayerId(1);

    /// `Add {B}{B}{B}` as a spell chain — Dark Ritual's whole text.
    fn fixed_mana_spell(colors: Vec<ManaColor>) -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors,
                    contribution: ManaContribution::Base,
                },
                restrictions: Vec::new(),
                grants: Vec::new(),
                expiry: None,
                target: None,
            },
        )
    }

    fn generic_cost(generic: u32) -> ManaCost {
        ManaCost::Cost {
            shards: Vec::new(),
            generic,
        }
    }

    /// Dark Ritual in the AI's hand: a `{B}`-costed instant adding {B}{B}{B}.
    fn dark_ritual(state: &mut GameState) -> ObjectId {
        let id = create_object(state, CardId(1), AI, "Dark Ritual".to_string(), Zone::Hand);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Instant);
        obj.mana_cost = generic_cost(1);
        Arc::make_mut(&mut obj.abilities).push(fixed_mana_spell(vec![
            ManaColor::Black,
            ManaColor::Black,
            ManaColor::Black,
        ]));
        id
    }

    /// A vanilla nonland card in the AI's hand at `mana_value`, of `core_type`.
    fn hand_card(
        state: &mut GameState,
        card_id: CardId,
        core_type: CoreType,
        mana_value: u32,
    ) -> ObjectId {
        let id = create_object(state, card_id, AI, "Follow Up".to_string(), Zone::Hand);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(core_type);
        obj.mana_cost = generic_cost(mana_value);
        id
    }

    /// An untapped Swamp under the AI's control — one point of `available_mana`.
    fn untapped_land(state: &mut GameState, card_id: CardId) -> ObjectId {
        let id = create_object(state, card_id, AI, "Swamp".to_string(), Zone::Battlefield);
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        id
    }

    /// The reported window: the opponent's end step, so no sorcery is castable.
    fn opponent_end_step(state: &mut GameState) {
        state.turn_number = 6;
        state.active_player = OPPONENT;
        state.phase = Phase::End;
    }

    /// The AI's own precombat main phase.
    fn own_main_phase(state: &mut GameState) {
        state.turn_number = 5;
        state.active_player = AI;
        state.phase = Phase::PreCombatMain;
    }

    /// No `evaluate_layers` pass: every read this policy makes is a direct field
    /// read (`card_types`, `abilities`, `keywords`, `trigger_definitions`,
    /// `tapped`), never a derived index, and the layer pass would clobber the
    /// post-creation edits these fixtures make on hand objects.
    fn verdict(state: &GameState, object_id: ObjectId, card_id: CardId) -> PolicyVerdict {
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id,
                card_id,
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Spell),
        };
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        };
        let config = AiConfig::default();
        let context = AiContext::empty(&config.weights);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        RitualSinkPolicy.verdict(&ctx)
    }

    fn assert_neutral(verdict: PolicyVerdict, kind: &str) {
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, kind, "reason kind");
                assert_eq!(delta, 0.0, "delta");
            }
            PolicyVerdict::Reject { reason } => panic!("unexpected reject: {}", reason.kind),
        }
    }

    fn assert_rejected(verdict: PolicyVerdict, kind: &str) {
        match verdict {
            PolicyVerdict::Reject { reason } => assert_eq!(reason.kind, kind, "reason kind"),
            PolicyVerdict::Score { reason, .. } => panic!("expected reject, got {}", reason.kind),
        }
    }

    /// The reported bug: Dark Ritual at the opponent's end step with an empty
    /// hand behind it. CR 106.4 — the {B}{B}{B} empties for nothing.
    #[test]
    fn ritual_with_no_castable_follow_up_is_rejected() {
        let mut state = GameState::new_two_player(42);
        opponent_end_step(&mut state);
        untapped_land(&mut state, CardId(90));
        let ritual = dark_ritual(&mut state);
        assert_rejected(verdict(&state, ritual, CardId(1)), "ritual_no_sink");
    }

    /// A 4-drop that one untapped land alone cannot reach, but the ritual can:
    /// 1 source − {B} + 3 produced = 3 … so a 3-drop is the sink.
    #[test]
    fn ritual_enabling_a_hand_card_is_not_rejected() {
        let mut state = GameState::new_two_player(42);
        own_main_phase(&mut state);
        untapped_land(&mut state, CardId(90));
        let ritual = dark_ritual(&mut state);
        hand_card(&mut state, CardId(2), CoreType::Creature, 3);
        assert_neutral(verdict(&state, ritual, CardId(1)), "ritual_sink_present");
    }

    /// Same reachable sorcery-speed card, but cast at the opponent's end step:
    /// CR 307.1 makes it uncastable in this window, so the mana is still wasted.
    #[test]
    fn ritual_at_instant_speed_with_only_sorceries_is_rejected() {
        let mut state = GameState::new_two_player(42);
        opponent_end_step(&mut state);
        untapped_land(&mut state, CardId(90));
        let ritual = dark_ritual(&mut state);
        hand_card(&mut state, CardId(2), CoreType::Creature, 3);
        assert_rejected(verdict(&state, ritual, CardId(1)), "ritual_no_sink");
    }

    /// An instant in reach is castable on the opponent's turn (CR 304.1).
    #[test]
    fn ritual_with_instant_in_reach_on_opponents_turn_is_not_rejected() {
        let mut state = GameState::new_two_player(42);
        opponent_end_step(&mut state);
        untapped_land(&mut state, CardId(90));
        let ritual = dark_ritual(&mut state);
        hand_card(&mut state, CardId(2), CoreType::Instant, 3);
        assert_neutral(verdict(&state, ritual, CardId(1)), "ritual_sink_present");
    }

    /// A flash creature is castable in the same window (CR 702.8a).
    #[test]
    fn ritual_with_flash_creature_in_reach_is_not_rejected() {
        let mut state = GameState::new_two_player(42);
        opponent_end_step(&mut state);
        untapped_land(&mut state, CardId(90));
        let ritual = dark_ritual(&mut state);
        let follow_up = hand_card(&mut state, CardId(2), CoreType::Creature, 3);
        state
            .objects
            .get_mut(&follow_up)
            .unwrap()
            .keywords
            .push(Keyword::Flash);
        assert_neutral(verdict(&state, ritual, CardId(1)), "ritual_sink_present");
    }

    /// A second ritual is not a sink — chaining defers the same question.
    #[test]
    fn second_ritual_in_hand_is_not_a_sink() {
        let mut state = GameState::new_two_player(42);
        opponent_end_step(&mut state);
        untapped_land(&mut state, CardId(90));
        let ritual = dark_ritual(&mut state);
        let other = create_object(
            &mut state,
            CardId(3),
            AI,
            "Cabal Ritual".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&other).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.mana_cost = generic_cost(2);
            Arc::make_mut(&mut obj.abilities)
                .push(fixed_mana_spell(vec![ManaColor::Black, ManaColor::Black]));
        }
        assert_rejected(verdict(&state, ritual, CardId(1)), "ritual_no_sink");
    }

    /// Guttersnipe shape: a caster-scoped spell-cast trigger on our own board
    /// makes the cast itself the payoff, so the veto stands down.
    #[test]
    fn ritual_with_cast_payoff_on_battlefield_is_not_rejected() {
        let mut state = GameState::new_two_player(42);
        opponent_end_step(&mut state);
        untapped_land(&mut state, CardId(90));
        let ritual = dark_ritual(&mut state);
        let payoff = create_object(
            &mut state,
            CardId(4),
            AI,
            "Guttersnipe".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&payoff).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::SpellCast)
                    .valid_target(TargetFilter::Controller)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::Proliferate,
                    )),
            );
        }
        assert_neutral(
            verdict(&state, ritual, CardId(1)),
            "ritual_sink_cast_payoff",
        );
    }

    /// An opponent's cast payoff is not ours — the stand-down must not fire.
    #[test]
    fn opponent_cast_payoff_does_not_stand_down() {
        let mut state = GameState::new_two_player(42);
        opponent_end_step(&mut state);
        untapped_land(&mut state, CardId(90));
        let ritual = dark_ritual(&mut state);
        let payoff = create_object(
            &mut state,
            CardId(4),
            OPPONENT,
            "Guttersnipe".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&payoff).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::SpellCast)
                    .valid_target(TargetFilter::Controller)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::Proliferate,
                    )),
            );
        }
        assert_rejected(verdict(&state, ritual, CardId(1)), "ritual_no_sink");
    }

    /// A ritual whose output is a quantity expression rather than a fixed colour
    /// list (Rite of Flame's graveyard-scaled half) is never vetoed.
    #[test]
    fn dynamic_output_ritual_is_not_vetoed() {
        let mut state = GameState::new_two_player(42);
        opponent_end_step(&mut state);
        untapped_land(&mut state, CardId(90));
        let ritual = dark_ritual(&mut state);
        Arc::make_mut(&mut state.objects.get_mut(&ritual).unwrap().abilities).push(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: Vec::new(),
                    grants: Vec::new(),
                    expiry: None,
                    target: None,
                },
            ),
        );
        assert_neutral(
            verdict(&state, ritual, CardId(1)),
            "ritual_sink_output_unknown",
        );
    }

    /// A non-ritual cast is out of class before any board read.
    #[test]
    fn non_ritual_cast_is_na() {
        let mut state = GameState::new_two_player(42);
        opponent_end_step(&mut state);
        let creature = hand_card(&mut state, CardId(5), CoreType::Creature, 2);
        assert_neutral(verdict(&state, creature, CardId(5)), "ritual_sink_na");
    }

    #[test]
    fn registry_registers_ritual_sink() {
        assert!(PolicyRegistry::default().has_policy(PolicyId::RitualSink));
    }
}
