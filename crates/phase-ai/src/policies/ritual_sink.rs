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
//! * A commander in the command zone is a sink like a hand card: CR 903.8 lets
//!   it be cast from there for its cost plus the tax, and that zone is not
//!   `player.hand`, so the hand scan alone would veto the ordinary Commander
//!   line of ritualing into the commander.
//! * A mana-costed activated ability on our own board is a sink too — the
//!   ritual buys an activation (an X sink, an equip, a Walking Ballista
//!   counter) rather than a card. Mana abilities are never sinks (CR 605.1a:
//!   they make mana, they do not spend it), and sorcery-speed activations
//!   count only in our own main phase, mirroring the hand scan.
//! * Storm/prowess payoffs on our own board (Aetherflux Reservoir, Guttersnipe,
//!   magecraft, prowess) make the cast itself the payoff, independent of what
//!   the mana buys. That is the last stand-down.
//!
//! Perf: the card-local `is_ritual_parts` check and the output fold run first
//! and exit every non-ritual `CastSpell` candidate before any state read beyond
//! the candidate's own object. Only a confirmed, fixed-output ritual pays for
//! `available_mana` (one battlefield count), then a hand scan, then a
//! command-zone scan (a handful of objects), then — only if all three fail —
//! one battlefield walk that checks activation sinks and cast payoffs
//! together. No `find_legal_targets`, no `feasible_mana_capacity`, no state
//! clone.

use engine::game::commander::commander_tax;
use engine::game::game_object::GameObject;
use engine::game::keywords::has_flash;
use engine::game::mana_abilities::is_mana_ability;
use engine::game::turn_control::turn_decision_maker;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ActivationRestriction, Effect, ManaProduction,
};
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

        // 5. CR 903.8: a commander in the command zone is castable from there
        //    and is not in `player.hand`, so the hand scan cannot see it.
        if command_zone_has_sink(ctx, reach) {
            return neutral("ritual_sink_commander");
        }

        // 6. One own-battlefield walk, last and only once 1–5 hold: a
        //    mana-costed activation the ritual could buy, or a payoff that
        //    makes the cast itself the value.
        if let Some(kind) = battlefield_sink(ctx, reach) {
            return neutral(kind);
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
    let main_phase = own_main_phase(ctx);
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
        obj.mana_cost.mana_value() <= reach && castable_in_window(obj, main_phase)
    })
}

/// CR 307.1: whether this is a main phase of the AI's own turn — the window in
/// which sorcery-speed casts and activations are legal.
fn own_main_phase(ctx: &PolicyContext<'_>) -> bool {
    turn_decision_maker(ctx.state) == ctx.ai_player
        && matches!(
            ctx.state.phase,
            Phase::PreCombatMain | Phase::PostCombatMain
        )
}

/// CR 304.1 + CR 702.8a: an instant, or a card with flash, is castable whenever
/// the AI has priority. CR 307.1: everything else is sorcery-speed and needs
/// `main_phase`.
fn castable_in_window(obj: &GameObject, main_phase: bool) -> bool {
    obj.card_types.core_types.contains(&CoreType::Instant) || has_flash(obj) || main_phase
}

/// True when a commander the AI owns sits in the command zone and could be cast
/// from there with `reach` in this window. CR 903.8: the cast costs the
/// commander's mana cost plus {2} for each previous cast from the command zone,
/// so the tax is part of the price the ritual has to cover.
fn command_zone_has_sink(ctx: &PolicyContext<'_>, reach: u32) -> bool {
    let main_phase = own_main_phase(ctx);
    ctx.state.command_zone.iter().any(|&id| {
        let Some(obj) = ctx.state.objects.get(&id) else {
            return false;
        };
        obj.owner == ctx.ai_player
            && obj.is_commander
            && !is_ritual_parts(&obj.card_types.core_types, &obj.abilities)
            && obj
                .mana_cost
                .mana_value()
                .saturating_add(commander_tax(ctx.state, id))
                <= reach
            && castable_in_window(obj, main_phase)
    })
}

/// One walk over the AI's own battlefield permanents for either kind of sink,
/// returning the reason kind of the first one found:
///
/// * `ritual_sink_activation` — a permanent with an activated ability whose
///   mana cost the ritual could pay: the mana buys an activation rather than a
///   card (an X sink, an equip, a Walking Ballista counter).
/// * `ritual_sink_cast_payoff` — a permanent that pays off the *cast* itself:
///   prowess (CR 702.108a) or a caster-scoped spell-cast trigger (CR 601.2i),
///   the Guttersnipe / Aetherflux / magecraft shapes. For those, the ritual's
///   own cast is value even if the mana it makes is wasted.
fn battlefield_sink(ctx: &PolicyContext<'_>, reach: u32) -> Option<&'static str> {
    let main_phase = own_main_phase(ctx);
    ctx.state
        .battlefield
        .iter()
        .filter_map(|id| ctx.state.objects.get(id))
        .filter(|obj| obj.controller == ctx.ai_player)
        .find_map(|obj| {
            if obj
                .abilities
                .iter()
                .any(|ability| activation_is_sink(ability, reach, main_phase))
            {
                Some("ritual_sink_activation")
            } else if has_prowess_parts(&obj.keywords)
                || is_cast_payoff_parts(
                    obj.trigger_definitions
                        .iter_unchecked()
                        .map(|entry| &entry.definition),
                )
            {
                Some("ritual_sink_cast_payoff")
            } else {
                None
            }
        })
}

/// True when `ability` is an activated ability the ritual's mana could pay for.
///
/// CR 605.1a: a mana ability is never a sink — it makes mana rather than
/// spending it. CR 602.2: activating pays the ability's costs, so the mana
/// component of the cost is what the ritual buys. CR 702.6a + CR 307.1: an
/// "activate only as a sorcery" ability (equip is the common one) is legal only
/// in the AI's own main phase, the same window the hand scan applies to
/// sorcery-speed cards.
fn activation_is_sink(ability: &AbilityDefinition, reach: u32, main_phase: bool) -> bool {
    if ability.kind != AbilityKind::Activated || is_mana_ability(ability) {
        return false;
    }
    let sorcery_speed = ability
        .activation_restrictions
        .iter()
        .any(|restriction| matches!(restriction, ActivationRestriction::AsSorcery));
    if sorcery_speed && !main_phase {
        return false;
    }
    ability
        .cost
        .as_ref()
        .and_then(activation_mana_value)
        .is_some_and(|mana_value| mana_value >= 1 && mana_value <= reach)
}

/// Mana value of the mana component of an activation cost, or `None` when the
/// cost has no mana component. A `Composite` cost sums its components (CR
/// 601.2h: the total cost is paid as one), a `OneOf` cost takes its cheapest
/// mana arm, and an X cost (`ManaDynamic`) absorbs any positive amount, so its
/// cheapest useful activation is X=1. Every other cost shape (tap, sacrifice,
/// life, discard, …) spends no mana and is not what a ritual buys.
fn activation_mana_value(cost: &AbilityCost) -> Option<u32> {
    match cost {
        AbilityCost::Mana { cost } => Some(cost.mana_value()),
        AbilityCost::ManaDynamic { .. } => Some(1),
        AbilityCost::Composite { costs } => {
            let parts: Vec<u32> = costs.iter().filter_map(activation_mana_value).collect();
            (!parts.is_empty()).then(|| parts.iter().sum())
        }
        AbilityCost::OneOf { costs } => costs.iter().filter_map(activation_mana_value).min(),
        _ => None,
    }
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
    /// An activated ability on the AI's own board at `generic` mana, optionally
    /// sorcery-speed. `Effect::Proliferate` is a stand-in body: the policy reads
    /// only the ability's kind, cost, and restrictions.
    fn board_activation(
        state: &mut GameState,
        card_id: CardId,
        generic: u32,
        sorcery_speed: bool,
    ) -> ObjectId {
        let id = create_object(state, card_id, AI, "Outlet".to_string(), Zone::Battlefield);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        let mut ability = AbilityDefinition::new(AbilityKind::Activated, Effect::Proliferate);
        ability.cost = Some(AbilityCost::Mana {
            cost: generic_cost(generic),
        });
        if sorcery_speed {
            ability
                .activation_restrictions
                .push(ActivationRestriction::AsSorcery);
        }
        Arc::make_mut(&mut obj.abilities).push(ability);
        id
    }

    /// The AI's commander in the command zone at `mana_value`, a creature.
    fn commander_in_command_zone(state: &mut GameState, mana_value: u32) -> ObjectId {
        let id = create_object(
            state,
            CardId(90),
            AI,
            "Commander".to_string(),
            Zone::Command,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.mana_cost = generic_cost(mana_value);
        obj.is_commander = true;
        id
    }

    /// CR 602.2: the ritual's mana buys an activation. A {2} outlet on the
    /// AI's own board at the opponent's end step is a sink even with an empty
    /// hand behind the ritual.
    #[test]
    fn activation_on_own_board_is_a_sink() {
        let mut state = GameState::new_two_player(42);
        opponent_end_step(&mut state);
        untapped_land(&mut state, CardId(10));
        let ritual = dark_ritual(&mut state);
        board_activation(&mut state, CardId(20), 2, false);

        assert_neutral(verdict(&state, ritual, CardId(1)), "ritual_sink_activation");
    }

    /// CR 702.6a + CR 307.1: an "activate only as a sorcery" outlet is not a
    /// sink at the opponent's end step, but it is in the AI's own main phase.
    #[test]
    fn sorcery_speed_activation_is_a_sink_only_in_own_main_phase() {
        let mut state = GameState::new_two_player(42);
        opponent_end_step(&mut state);
        untapped_land(&mut state, CardId(10));
        let ritual = dark_ritual(&mut state);
        board_activation(&mut state, CardId(20), 2, true);

        assert_rejected(verdict(&state, ritual, CardId(1)), "ritual_no_sink");

        own_main_phase(&mut state);
        assert_neutral(verdict(&state, ritual, CardId(1)), "ritual_sink_activation");
    }

    /// An outlet the pool cannot reach is not a sink: {5} against a reach of 3.
    #[test]
    fn activation_out_of_reach_is_not_a_sink() {
        let mut state = GameState::new_two_player(42);
        opponent_end_step(&mut state);
        untapped_land(&mut state, CardId(10));
        let ritual = dark_ritual(&mut state);
        board_activation(&mut state, CardId(20), 5, false);

        assert_rejected(verdict(&state, ritual, CardId(1)), "ritual_no_sink");
    }

    /// CR 605.1a: a mana ability makes mana rather than spending it, so a
    /// tap-for-mana permanent on the AI's board is not a sink.
    #[test]
    fn mana_ability_on_own_board_is_not_a_sink() {
        let mut state = GameState::new_two_player(42);
        opponent_end_step(&mut state);
        untapped_land(&mut state, CardId(10));
        let ritual = dark_ritual(&mut state);
        let rock = create_object(
            &mut state,
            CardId(20),
            AI,
            "Rock".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&rock).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::Black],
                    contribution: ManaContribution::Base,
                },
                restrictions: Vec::new(),
                grants: Vec::new(),
                expiry: None,
                target: None,
            },
        );
        ability.cost = Some(AbilityCost::Tap);
        Arc::make_mut(&mut obj.abilities).push(ability);

        assert_rejected(verdict(&state, ritual, CardId(1)), "ritual_no_sink");
    }

    /// CR 903.8: the commander is castable from the command zone, which the
    /// hand scan cannot see. A {3} commander against a reach of 3 in the AI's
    /// own main phase is the ordinary ritual-into-commander line.
    #[test]
    fn commander_in_command_zone_is_a_sink() {
        let mut state = GameState::new_two_player(42);
        own_main_phase(&mut state);
        untapped_land(&mut state, CardId(10));
        let ritual = dark_ritual(&mut state);
        commander_in_command_zone(&mut state, 3);

        assert_neutral(verdict(&state, ritual, CardId(1)), "ritual_sink_commander");
    }

    /// CR 903.8: the {2} tax per previous command-zone cast is part of the
    /// price. One prior cast puts the same {3} commander at 5, past a reach of 3.
    #[test]
    fn commander_tax_can_put_the_commander_out_of_reach() {
        let mut state = GameState::new_two_player(42);
        own_main_phase(&mut state);
        untapped_land(&mut state, CardId(10));
        let ritual = dark_ritual(&mut state);
        let commander = commander_in_command_zone(&mut state, 3);
        state.commander_cast_count.insert(commander, 1);

        assert_rejected(verdict(&state, ritual, CardId(1)), "ritual_no_sink");
    }

    /// A creature commander is sorcery-speed: not a sink at the opponent's end
    /// step, mirroring the hand scan's CR 307.1 half.
    #[test]
    fn commander_is_not_a_sink_at_instant_speed() {
        let mut state = GameState::new_two_player(42);
        opponent_end_step(&mut state);
        untapped_land(&mut state, CardId(10));
        let ritual = dark_ritual(&mut state);
        commander_in_command_zone(&mut state, 3);

        assert_rejected(verdict(&state, ritual, CardId(1)), "ritual_no_sink");
    }

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
