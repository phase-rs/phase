//! CI-enforced equivalence between the read-only draw preflight and the live
//! draw pipeline (CR 121.1 / CR 614.6 / CR 614.11).
//!
//! `game::effects::draw::can_draw_at_least_one` answers "would a draw right now
//! actually put a card into this player's hand, emitting `GameEvent::CardDrawn`?"
//! It exists so an AI payoff policy can decline to reward a draw that will fire
//! no "whenever you draw" trigger. Because it is read-only it cannot run the
//! pipeline — so the standing hazard is that it becomes a PARTIAL MIRROR of the
//! pipeline and silently drifts: each un-modeled suppression leg is a candidate
//! scored as a draw engine that draws nothing.
//!
//! The preflight is built to make drift impossible by construction — its
//! substitution leg calls `draw_is_substituted_away`, the very function
//! `apply_single_replacement` uses to pre-zero the live count, and its
//! applicability comes from `find_applicable_replacements`, the live authority.
//! This test enforces that property from the outside instead of trusting it:
//! every shape below asks the preflight for a prediction, then DRIVES THE REAL
//! DRAW and observes what the pipeline did. A leg the preflight stops modeling
//! shows up here as a prediction/observation mismatch, whichever direction it
//! drifts in.
//!
//! Suppression legs covered — every way `can_draw_at_least_one` can answer "no":
//!   1. a draw restriction — `CantDraw` shown; `PerTurnDrawLimit` exhaustion is
//!      the same leg, both resolved by `allowed_draw_count`
//!   2. empty library (CR 704.5b — an attempted draw delivers no card)
//!   3. mandatory `QuantityModification::Prevent` (CR 614.6, Living Conundrum)
//!   4. mandatory non-Draw substitute, in `execute` (Chains of Mephistopheles,
//!      Jace Wielder of Mysteries) and in `runtime_execute` (Words of Worship,
//!      "{1}: The next time you would draw a card this turn, you gain 5 life
//!      instead") — CR 614.11
//!   5. mandatory count modification resolving to zero (CR 614.11a)
//!
//! Surviving controls: an unreplaced draw, and a count-modifying replacement
//! that rescales rather than removes ("…draw two cards instead" — Alhammarret's
//! Archive, Teferi's Ageless Insight). Without these the equivalence is
//! satisfiable by a preflight that always predicts "no draw".

use engine::game::effects::draw::can_draw_at_least_one;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, DrawReplacementScope, Effect, QuantityExpr,
    QuantityModification, ReplacementDefinition, ResolvedAbility, StaticDefinition, TargetFilter,
};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::card_type::CoreType;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::statics::{ProhibitionScope, StaticMode};
use engine::types::zones::Zone;

/// "…you gain 5 life instead" — a substitute that is not a draw. The classifier
/// keys on "not a `Draw`, not a pure event modifier", so one non-draw effect
/// stands in for the whole class (discard, win-the-game, reveal-until, token).
fn gain_life_substitute() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 5 },
            player: TargetFilter::Controller,
        },
    )
}

/// "…draw two cards instead" — a count modification. Still a draw (CR 614.11a).
fn draw_count_substitute(value: i32) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value },
            target: TargetFilter::Controller,
        },
    )
}

/// A replacement-bearing permanent to seat: its card name, and a shaper that
/// fills in the `ReplacementDefinition` given the permanent's `ObjectId` (needed
/// because a `runtime_execute` substitute binds its own source).
type ReplacementShape = (
    &'static str,
    Box<dyn Fn(&mut ReplacementDefinition, ObjectId)>,
);

/// Seats P0 with `library` cards, plus a replacement-bearing permanent when
/// `customize` is supplied. P1 always gets a library so no state-based action
/// ends the game mid-test.
fn scenario(library: usize, customize: Option<ReplacementShape>) -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..library {
        scenario.add_card_to_library_top(P0, &format!("Lib {i}"));
    }
    for i in 0..5 {
        scenario.add_card_to_library_top(P1, &format!("P1 Lib {i}"));
    }
    // 1/1, not 0/0: the replacement source must survive the state-based-action
    // check that runs while the draw resolves, or the pipeline would see a board
    // the preflight never predicted against (CR 704.5f).
    let source = customize
        .as_ref()
        .map(|(name, _)| scenario.add_creature(P0, name, 1, 1).id());
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    if let (Some(source), Some((_, shape))) = (source, customize) {
        let mut repl = ReplacementDefinition::new(ReplacementEvent::Draw)
            .draw_scope(DrawReplacementScope::IndividualDraw);
        shape(&mut repl, source);
        runner
            .state_mut()
            .objects
            .get_mut(&source)
            .expect("replacement source must exist")
            .replacement_definitions
            .push(repl);
    }
    runner
}

/// THE ASSERTION: ask the preflight, then run the real draw and compare.
///
/// `expected_delivery` pins what the pipeline is supposed to do, so a regression
/// that breaks BOTH sides in the same direction still fails here rather than
/// quietly agreeing at the wrong answer.
fn assert_preflight_matches_pipeline(shape: &str, mut runner: GameRunner, expected_delivery: bool) {
    let predicted = can_draw_at_least_one(runner.state(), P0);
    let hand_before = runner.state().players[P0.0 as usize].hand.len();

    runner
        .act(GameAction::Debug(DebugAction::DrawCards {
            player_id: P0,
            count: 1,
        }))
        .expect("debug draw must be accepted");
    runner.advance_until_stack_empty();

    let delivered = runner.state().players[P0.0 as usize].hand.len() > hand_before;

    assert_eq!(
        delivered, expected_delivery,
        "{shape}: the live pipeline delivered={delivered}, but this shape is \
         specified to deliver={expected_delivery} — the test's model of the \
         pipeline is stale, fix that before reading the preflight comparison"
    );
    assert_eq!(
        predicted, delivered,
        "{shape}: can_draw_at_least_one predicted {predicted} but the live draw \
         pipeline delivered {delivered}. The preflight has drifted from the \
         pipeline — an AI draw-payoff bonus is now being awarded to a draw that \
         emits no CardDrawn (or withheld from one that does)."
    );
}

/// Control: nothing suppresses the draw, so preflight and pipeline both say yes.
/// Without this the equivalence is satisfiable by always predicting "no draw".
#[test]
fn unreplaced_draw_is_predicted_and_delivered() {
    assert_preflight_matches_pipeline("unreplaced draw", scenario(3, None), true);
}

/// CR 704.5b: an empty-library draw records an attempt and delivers no card.
#[test]
fn empty_library_draw_is_predicted_and_not_delivered() {
    assert_preflight_matches_pipeline("empty library", scenario(0, None), false);
}

/// CR 121.1: a `CantDraw` static permits no draw at all, so the draw event never
/// occurs and no card is delivered. The restriction leg — `allowed_draw_count`
/// resolves it, and an exhausted `PerTurnDrawLimit` reaches the same zero the
/// same way.
#[test]
fn cant_draw_static_is_predicted_and_not_delivered() {
    let mut runner = scenario(3, None);
    let state = runner.state_mut();
    let card_id = CardId(state.next_object_id);
    let hoser = create_object(
        state,
        card_id,
        P1,
        "Draw Hoser".to_string(),
        Zone::Battlefield,
    );
    let obj = state
        .objects
        .get_mut(&hoser)
        .expect("the draw-restricting permanent must exist");
    obj.card_types.core_types.push(CoreType::Creature);
    obj.static_definitions
        .push(StaticDefinition::new(StaticMode::CantDraw {
            who: ProhibitionScope::AllPlayers,
        }));
    assert_preflight_matches_pipeline("CantDraw static", runner, false);
}

/// CR 614.6: a mandatory `Prevent` replaces the draw away — Living Conundrum's
/// "skip that draw instead". The replaced event never happens.
#[test]
fn mandatory_prevent_is_predicted_and_not_delivered() {
    let runner = scenario(
        3,
        Some((
            "Living Conundrum",
            Box::new(|repl: &mut ReplacementDefinition, _source| {
                repl.quantity_modification = Some(QuantityModification::Prevent);
            }),
        )),
    );
    assert_preflight_matches_pipeline("mandatory prevent", runner, false);
}

/// CR 614.11: a mandatory non-Draw substitute in `execute` — the printed-static
/// half of the class (Chains of Mephistopheles, Jace Wielder of Mysteries).
/// `apply_single_replacement` zeroes the count, so no card is delivered.
#[test]
fn mandatory_execute_substitute_is_predicted_and_not_delivered() {
    let runner = scenario(
        3,
        Some((
            "Chains of Mephistopheles",
            Box::new(|repl: &mut ReplacementDefinition, _source| {
                repl.execute = Some(Box::new(gain_life_substitute()));
            }),
        )),
    );
    assert_preflight_matches_pipeline("mandatory execute substitute", runner, false);
}

/// CR 614.11: the same substitution delivered through `runtime_execute`, the
/// activated-one-shot half of the class (Words of Worship). A preflight that
/// inspects only `execute` misses this leg entirely.
#[test]
fn mandatory_runtime_execute_substitute_is_predicted_and_not_delivered() {
    let runner = scenario(
        3,
        Some((
            "Words of Worship",
            Box::new(|repl: &mut ReplacementDefinition, source: ObjectId| {
                repl.runtime_execute = Some(Box::new(ResolvedAbility::new(
                    gain_life_substitute().effect.as_ref().clone(),
                    Vec::new(),
                    source,
                    P0,
                )));
            }),
        )),
    );
    assert_preflight_matches_pipeline("mandatory runtime_execute substitute", runner, false);
}

/// CR 614.11a: a count modification RESCALES the draw ("…draw two cards
/// instead") rather than removing it, so a card is still delivered and
/// `CardDrawn` still fires. The discriminating control for the two substitute
/// cases: same mandatory `execute` slot, opposite outcome.
#[test]
fn count_modifying_replacement_is_predicted_and_delivered() {
    let runner = scenario(
        3,
        Some((
            "Alhammarret's Archive",
            Box::new(|repl: &mut ReplacementDefinition, _source| {
                repl.execute = Some(Box::new(draw_count_substitute(2)));
            }),
        )),
    );
    assert_preflight_matches_pipeline("count-modifying replacement", runner, true);
}

/// CR 614.11a: the boundary of that same count surface — a modification
/// resolving to zero leaves no card to draw, so no `CardDrawn` is emitted. The
/// `execute` here IS a draw, so the substitution classifier declines it and only
/// the resolved count discriminates.
#[test]
fn zero_count_replacement_is_predicted_and_not_delivered() {
    let runner = scenario(
        3,
        Some((
            "Zero-Count Draw Rescaler",
            Box::new(|repl: &mut ReplacementDefinition, _source| {
                repl.execute = Some(Box::new(draw_count_substitute(0)));
            }),
        )),
    );
    assert_preflight_matches_pipeline("zero-count replacement", runner, false);
}

// ─── candidate-instruction quantity (CR 121.1 + CR 107.1b) ───────────────────
//
// The cases above vary the PLAYER's ability to draw. A draw also fails to fire
// an engine when the instruction's OWN count resolves to zero — a distinct axis,
// gated in `DrawPayoffPolicy` by requiring a positive resolved candidate
// quantity. These pin the live-resolver behavior that gate models: the resolver
// resolves the effect's quantity and emits `CardDrawn` only per delivered card,
// so a zero-count draw emits none even with a healthy library.

/// Resolves a controller-targeted `Effect::Draw` of `count` on a fresh board and
/// reports whether the live resolver emitted any `CardDrawn` event.
fn live_draw_emits_card_drawn(count: i32) -> bool {
    let mut runner = scenario(3, None);
    let source = runner.state().players[P0.0 as usize]
        .library
        .iter()
        .next()
        .copied()
        .expect("seeded library");
    let ability = ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Fixed { value: count },
            target: TargetFilter::Controller,
        },
        Vec::new(),
        source,
        P0,
    );
    let mut events = Vec::new();
    engine::game::effects::draw::resolve(runner.state_mut(), &ability, &mut events)
        .expect("draw resolution must succeed");
    events
        .iter()
        .any(|e| matches!(e, engine::types::events::GameEvent::CardDrawn { .. }))
}

/// CR 107.1b: a zero-count draw instruction delivers no card, so the resolver
/// emits no `CardDrawn` and a "whenever you draw" engine never triggers — the
/// live fact behind `DrawPayoffPolicy` requiring a positive candidate quantity.
/// Paired with a positive control so this cannot pass by the resolver breaking.
#[test]
fn zero_count_draw_instruction_emits_no_card_drawn() {
    assert!(
        !live_draw_emits_card_drawn(0),
        "a draw of zero cards must emit no CardDrawn event"
    );
    assert!(
        live_draw_emits_card_drawn(1),
        "control: a draw of one card must emit CardDrawn"
    );
}
