//! DynQty subgroup D — "[once] for each ⟨player-set⟩" lift on a fieldless
//! `Effect::Investigate` (parser-only).
//!
//! - **Teysa, Opulent Oligarch**: "At the beginning of your end step, investigate
//!   for each opponent who lost life this turn." → repeat_for
//!   `PlayerCount { OpponentLostLife }`.
//! - **Wojek Investigator**: "At the beginning of your upkeep, investigate once for
//!   each opponent who has more cards in hand than you." → repeat_for
//!   `PlayerCount { PlayerAttribute { Opponent, HandSize{ScopedPlayer}, GT,
//!   Ref(HandSize{Controller}) } }`.
//!
//! Matrix #5/#6 drive the real parse pipeline (`parse_oracle_text`) on verbatim
//! Oracle text; matrix #7 drives the real runtime through `apply()` (a 4-player
//! upkeep trigger → Clue tokens), including the hostile tie fixture that proves
//! the comparative operand binds the controller (CR 109.5) and the attr binds
//! per-candidate.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    Comparator, ControllerRef, Effect, FilterProp, PlayerFilter, PlayerRelation, PlayerScope,
    QuantityExpr, QuantityRef, TargetFilter, TypeFilter,
};
use engine::types::game_state::GameState;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

const TEYSA_ORACLE: &str = "Deathtouch\n\
    At the beginning of your end step, investigate for each opponent who lost life this turn.\n\
    Whenever a Clue you control is put into a graveyard from the battlefield, create a 1/1 white \
    and black Spirit creature token with flying. This ability triggers only once each turn.";

const WOJEK_ORACLE: &str = "Flying, vigilance\n\
    At the beginning of your upkeep, investigate once for each opponent who has more cards in \
    hand than you. (To investigate, create a Clue token. It's an artifact with \"{2}, Sacrifice \
    this token: Draw a card.\")";

// The upkeep trigger sentence in isolation (verbatim, reminder retained) — used to
// seed the runtime creature so the Flying/vigilance keyword line does not interfere
// with the trigger under test.
const WOJEK_UPKEEP_TRIGGER: &str = "At the beginning of your upkeep, investigate once for each \
    opponent who has more cards in hand than you. (To investigate, create a Clue token. It's an \
    artifact with \"{2}, Sacrifice this token: Draw a card.\")";

// Teysa's end-step trigger sentence in isolation — the runtime creature carries ONLY
// the Investigate trigger (no Deathtouch, no Clue-death Spirit trigger) so the Clue
// delta measures the for-each investigate alone.
const TEYSA_END_STEP_TRIGGER: &str =
    "At the beginning of your end step, investigate for each opponent who lost life this turn.";

/// Matrix #5 — Teysa's end-step trigger carries `repeat_for = PlayerCount{OpponentLostLife}`.
/// Reach-guard: the trigger parsed to a real `Investigate` (not `Unimplemented`), so
/// the swallow assertion is not vacuous. Fails iff EDIT 1 is reverted; passes with EDIT 2
/// reverted (Teysa does not exercise the comparative arm).
#[test]
fn teysa_end_step_investigate_lifts_opponent_lost_life() {
    let parsed = parse_oracle_text(TEYSA_ORACLE, "Teysa, Opulent Oligarch", &[], &[], &[]);

    let end_step = parsed
        .triggers
        .iter()
        .find(|t| t.phase == Some(Phase::End))
        .expect("Teysa has an end-step trigger");
    let execute = end_step.execute.as_ref().expect("end-step execute");

    // Reach-guard: the effect really is Investigate (not Unimplemented) — the
    // positive branch the swallow check would early-return past.
    assert!(
        matches!(execute.effect.as_ref(), Effect::Investigate),
        "end-step effect must be Investigate, got {:?}",
        execute.effect
    );
    assert_eq!(
        execute.repeat_for,
        Some(QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentLostLife,
            },
        }),
        "Teysa must investigate once per opponent who lost life this turn"
    );

    // The DynamicQty / Duration_ThisTurn swallow warnings must clear (the for-each
    // clause now owns the "this turn" duration and the player-set predicate).
    assert!(
        !parsed
            .parse_warnings
            .iter()
            .any(|w| format!("{w:?}").contains("SwallowedClause")),
        "no clause may remain swallowed: {:?}",
        parsed.parse_warnings
    );
}

/// Matrix #6 — Wojek's upkeep trigger carries the comparative `PlayerAttribute`
/// repeat_for. Verbatim input (reminder retained) self-guards reminder stripping.
/// Fails iff EDIT 1 OR EDIT 2 is reverted.
#[test]
fn wojek_upkeep_investigate_lifts_comparative_hand_size() {
    let parsed = parse_oracle_text(
        WOJEK_ORACLE,
        "Wojek Investigator",
        &["Flying".to_string(), "Vigilance".to_string()],
        &[],
        &[],
    );

    let upkeep = parsed
        .triggers
        .iter()
        .find(|t| t.phase == Some(Phase::Upkeep))
        .expect("Wojek has an upkeep trigger");
    let execute = upkeep.execute.as_ref().expect("upkeep execute");

    assert!(
        matches!(execute.effect.as_ref(), Effect::Investigate),
        "upkeep effect must be Investigate, got {:?}",
        execute.effect
    );
    assert_eq!(
        execute.repeat_for,
        Some(QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::PlayerAttribute {
                    relation: PlayerRelation::Opponent,
                    attr: Box::new(QuantityRef::HandSize {
                        player: PlayerScope::ScopedPlayer,
                    }),
                    comparator: Comparator::GT,
                    value: Box::new(QuantityExpr::Ref {
                        qty: QuantityRef::HandSize {
                            player: PlayerScope::Controller,
                        },
                    }),
                },
            },
        }),
        "Wojek must investigate once per opponent with strictly more cards in hand than the controller"
    );

    assert!(
        !parsed
            .parse_warnings
            .iter()
            .any(|w| format!("{w:?}").contains("SwallowedClause")),
        "no clause may remain swallowed: {:?}",
        parsed.parse_warnings
    );
}

/// Count battlefield Clue tokens controlled by `player` (CR 111.10f — Clue subtype).
fn count_clues(state: &GameState, player: PlayerId) -> usize {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| obj.controller == player)
        .filter(|obj| {
            obj.card_types
                .subtypes
                .iter()
                .any(|s| s.eq_ignore_ascii_case("Clue"))
        })
        .count()
}

fn hand_len(state: &GameState, player: PlayerId) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.hand.len())
        .unwrap_or(0)
}

/// Build a 4-player game (P0 controls Wojek) at P0's Untap step with the given
/// per-player hand sizes, then return the runner ready to advance into upkeep.
fn wojek_runner(hands: [(PlayerId, usize); 4]) -> engine::game::scenario::GameRunner {
    let mut scenario = GameScenario::new_n_player(4, 20);
    scenario.at_phase(Phase::Untap);
    scenario
        .add_creature(P0, "Wojek Investigator", 2, 2)
        .from_oracle_text(WOJEK_UPKEEP_TRIGGER);
    for (pid, n) in hands {
        for _ in 0..n {
            scenario.add_card_to_hand(pid, "Filler");
        }
    }
    let mut runner = scenario.build();
    runner.state_mut().turn_number = 2;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner
}

/// Matrix #7 — end-to-end runtime binding through `apply()`. Controller hand = 2;
/// opponents A=4 (qualifies), B=1 (no), C=3 (qualifies) → exactly 2 Clues.
///
/// Reach-guard (non-vacuous): the parsed upkeep trigger MUST carry the comparative
/// `PlayerAttribute` repeat_for before we cast — with EDIT 1/2 reverted the trigger
/// investigates once (1 Clue ≠ 2) and this precondition also fails first.
#[test]
fn wojek_runtime_makes_one_clue_per_opponent_with_more_cards() {
    // Reach-guard: the fix is active (repeat_for is the comparative PlayerAttribute).
    let parsed = parse_oracle_text(
        WOJEK_ORACLE,
        "Wojek Investigator",
        &["Flying".to_string(), "Vigilance".to_string()],
        &[],
        &[],
    );
    let repeat_for = parsed
        .triggers
        .iter()
        .find(|t| t.phase == Some(Phase::Upkeep))
        .and_then(|t| t.execute.as_ref())
        .and_then(|e| e.repeat_for.clone());
    assert!(
        matches!(
            repeat_for,
            Some(QuantityExpr::Ref {
                qty: QuantityRef::PlayerCount {
                    filter: PlayerFilter::PlayerAttribute {
                        comparator: Comparator::GT,
                        ..
                    }
                }
            })
        ),
        "reach-guard: Wojek's trigger must carry the comparative repeat_for, got {repeat_for:?}"
    );

    // Controller P0 = 2; A(P1)=4 qualifies, B(P2)=1 no, C(P3)=3 qualifies.
    let mut runner = wojek_runner([(P0, 2), (P1, 4), (P2, 1), (P3, 3)]);
    assert_eq!(
        hand_len(runner.state(), P0),
        2,
        "precondition: controller hand = 2"
    );
    assert_eq!(
        hand_len(runner.state(), P1),
        4,
        "precondition: opp A hand = 4"
    );
    assert_eq!(
        hand_len(runner.state(), P2),
        1,
        "precondition: opp B hand = 1"
    );
    assert_eq!(
        hand_len(runner.state(), P3),
        3,
        "precondition: opp C hand = 3"
    );
    let clues_before = count_clues(runner.state(), P0);

    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();

    assert_eq!(
        count_clues(runner.state(), P0) - clues_before,
        2,
        "Wojek investigates once per opponent with strictly more cards than P0 (A and C) → 2 Clues"
    );
}

/// Matrix #7 (hostile flip) — opp A ties the controller's hand size (2 == 2). GT
/// (CR 109.5 "more … than you") excludes the tie, so only C (3 > 2) qualifies → 1
/// Clue. Proves `value` binds the controller and the attr binds per-candidate: if
/// the operand were per-candidate (or GE), the tie would count and the total would
/// be 2.
#[test]
fn wojek_runtime_excludes_opponent_tied_with_controller() {
    let mut runner = wojek_runner([(P0, 2), (P1, 2), (P2, 1), (P3, 3)]);
    assert_eq!(
        hand_len(runner.state(), P1),
        2,
        "precondition: opp A ties controller at 2"
    );
    let clues_before = count_clues(runner.state(), P0);

    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();

    assert_eq!(
        count_clues(runner.state(), P0) - clues_before,
        1,
        "a tied opponent (A: 2 == controller 2) is excluded by GT → only C qualifies → 1 Clue"
    );
}

/// Build a 4-player game (P0 controls Teysa) after combat (P0's post-combat main
/// phase) with the given per-player life-loss totals, then return the runner ready to
/// advance into the end step. Starting after combat means `advance_to_end_step` neither
/// halts at DeclareAttackers nor wraps a turn boundary, so `life_lost_this_turn`
/// (CR 119.3 — losing life; reset only at turn start by `start_next_turn`) is seeded at
/// build and survives to the end-step trigger's resolution.
fn teysa_runner(losses: [(PlayerId, u32); 4]) -> engine::game::scenario::GameRunner {
    let mut scenario = GameScenario::new_n_player(4, 20);
    scenario.at_phase(Phase::PostCombatMain);
    scenario
        .add_creature(P0, "Teysa, Opulent Oligarch", 2, 3)
        .from_oracle_text(TEYSA_END_STEP_TRIGGER);
    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    for (pid, n) in losses {
        if let Some(p) = runner.state_mut().players.iter_mut().find(|p| p.id == pid) {
            p.life_lost_this_turn = n;
        }
    }
    runner
}

/// Matrix #7 (Teysa) — end-to-end runtime binding through `apply()`. Controller P0
/// lost 5 (self-excluded), A(P1) lost 3, B(P2) lost 0 (zero-loss excluded), C(P3) lost
/// 1 → exactly 2 Clues. The `OpponentLostLife` predicate is
/// `p.id != controller && p.life_lost_this_turn > 0` (CR 119.3): P0 is the controller
/// so its own loss never counts, and P2's zero loss fails `> 0`.
///
/// Reach-guard (non-vacuous): the parsed end-step trigger MUST carry the
/// `PlayerCount { OpponentLostLife }` repeat_for before we drive — with the lift
/// reverted the trigger is a bare Investigate (repeat_for == None → 1 Clue) and this
/// precondition also fails first.
///
/// Discrimination: a wrong filter that counted the controller or a zero-loss player
/// would make 3; an absent wire (bare Investigate) would make 1; only the correct
/// `p.id != controller && life > 0` predicate makes 2.
#[test]
fn teysa_runtime_makes_one_clue_per_opponent_who_lost_life() {
    // Reach-guard: the lift is active (repeat_for is PlayerCount{OpponentLostLife}).
    let parsed = parse_oracle_text(TEYSA_ORACLE, "Teysa, Opulent Oligarch", &[], &[], &[]);
    let repeat_for = parsed
        .triggers
        .iter()
        .find(|t| t.phase == Some(Phase::End))
        .and_then(|t| t.execute.as_ref())
        .and_then(|e| e.repeat_for.clone());
    assert_eq!(
        repeat_for,
        Some(QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentLostLife,
            },
        }),
        "reach-guard: Teysa's end-step trigger must carry the OpponentLostLife repeat_for, got {repeat_for:?}"
    );

    // Controller P0 lost 5 (self-excluded); A(P1)=3 qualifies, B(P2)=0 excluded,
    // C(P3)=1 qualifies.
    let mut runner = teysa_runner([(P0, 5), (P1, 3), (P2, 0), (P3, 1)]);
    let clues_before = count_clues(runner.state(), P0);

    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    assert_eq!(
        count_clues(runner.state(), P0) - clues_before,
        2,
        "Teysa investigates once per opponent who lost life this turn (A and C) → 2 Clues"
    );
}

/// Matrix #7 (Teysa, zero flip) — no opponent lost life this turn (P0 lost 4 but is the
/// controller; A/B/C lost 0). The for-each ranges over an empty player set, so the
/// repeat_for driver runs 0 iterations → 0 Clues (CR 513.1 — the end-step trigger still
/// fires and resolves; it simply investigates zero times). A non-lifted bare Investigate
/// would wrongly make 1 Clue, so the 0 delta is a crisp wire discriminator: the
/// revert-probe flips it 0 → 1.
#[test]
fn teysa_runtime_no_clue_when_no_opponent_lost_life() {
    let mut runner = teysa_runner([(P0, 4), (P1, 0), (P2, 0), (P3, 0)]);
    let clues_before = count_clues(runner.state(), P0);

    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    assert_eq!(
        count_clues(runner.state(), P0) - clues_before,
        0,
        "no opponent lost life → empty for-each → 0 iterations → 0 Clues (a bare Investigate would make 1)"
    );
}

// Verbatim (reminder retained). The only two "investigate ... for each" cards in the
// std corpus whose for-each ranges over OBJECTS (not a player set): Serene Sleuth
// ("for each goaded creature you control") and Sophina ("for each nontoken attacking
// creature"). The parameterized gate-widen + Gap A (`FilterProp::Goaded`) now lift
// Serene Sleuth to an `ObjectCount` repeat_for; Sophina stays a bare Investigate
// (Gap B deferred — parse_type_phrase leading-adjective order-dependence). Both
// branches drive the real seam and pair their `repeat_for` assertion with a positive
// `Effect::Investigate` reach-guard so neither is vacuous.
const SERENE_SLEUTH: &str = "When this creature enters, investigate. (Create a Clue token. It's \
    an artifact with \"{2}, Sacrifice this token: Draw a card.\")\n\
    At the beginning of combat on your turn, investigate for each goaded creature you control. \
    Then each creature you control is no longer goaded.";

const SOPHINA: &str = "Menace\n\
    Whenever Sophina, Spearsage Deserter attacks, investigate once for each nontoken attacking \
    creature. (To investigate, create a Clue token. It's an artifact with \"{2}, Sacrifice this \
    artifact: Draw a card.\")";

// Serene Sleuth's combat-trigger sentence in isolation — the runtime creature carries
// ONLY the object for-each Investigate (not the ETB Investigate, not the un-goad sibling)
// so the Clue delta measures the goaded-creature count alone.
const SERENE_COMBAT_TRIGGER: &str =
    "At the beginning of combat on your turn, investigate for each goaded creature you control.";

/// Build a 4-player game (P0 controls Serene Sleuth) in P0's precombat main with
/// `n_goaded` P0 creatures each goaded by P1 and `n_plain` ungoaded P0 creatures, ready
/// to advance into the beginning-of-combat step. Serene Sleuth itself is an ungoaded P0
/// creature, so a filter that ignored the goad designation (CR 701.15b/c) would
/// over-count (it would include Sleuth and the plain creatures).
fn serene_runner(n_goaded: usize, n_plain: usize) -> engine::game::scenario::GameRunner {
    let mut scenario = GameScenario::new_n_player(4, 20);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Serene Sleuth", 2, 2)
        .from_oracle_text(SERENE_COMBAT_TRIGGER);
    let mut goaded_ids = Vec::new();
    for i in 0..n_goaded {
        goaded_ids.push(
            scenario
                .add_creature(P0, &format!("Goaded Ox {i}"), 2, 2)
                .id(),
        );
    }
    for i in 0..n_plain {
        scenario.add_creature(P0, &format!("Calm Bear {i}"), 2, 2);
    }
    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    // CR 701.15b/c: designate each Ox as goaded by P1 — a nonempty `goaded_by` set is
    // exactly what `FilterProp::Goaded` reads.
    for id in goaded_ids {
        if let Some(obj) = runner.state_mut().objects.get_mut(&id) {
            obj.goaded_by.insert(P1);
        }
    }
    runner
}

/// Matrix #7 (Serene Sleuth) — RUNTIME discriminator, end-to-end binding through
/// `apply()`. P0 controls Serene Sleuth (ungoaded) + 3 creatures goaded by P1 + 1
/// ungoaded plain creature. The beginning-of-combat trigger investigates once per goaded
/// creature P0 controls (CR 701.16a + CR 701.15b/c) → exactly 3 Clues.
///
/// Reach-guard (non-vacuous): the parsed combat trigger MUST carry the
/// `ObjectCount { Typed(.., [Goaded]) }` repeat_for before we drive — with the gate
/// narrowed (revert-probe a) or Gap A reverted (revert-probe b) the trigger is a bare
/// Investigate (repeat_for None → 1 Clue) and this precondition also fails first.
///
/// Discrimination (three distinct outcomes): the correct goaded filter → 3; a filter
/// that ignored `FilterProp::Goaded` would count all 5 P0 creatures (Sleuth + 3 Ox + 1
/// Bear) → 5; a non-lifted bare Investigate → 1. Only the correct wire makes 3.
/// (The "no longer goaded" sibling sentence is not part of the isolated trigger text, so
/// nothing un-goads the Oxen mid-resolution.)
#[test]
fn serene_sleuth_runtime_makes_one_clue_per_goaded_creature() {
    // Reach-guard: the lift is active (repeat_for is ObjectCount carrying Goaded).
    let parsed = parse_oracle_text(SERENE_COMBAT_TRIGGER, "Serene Sleuth", &[], &[], &[]);
    let repeat_for = parsed
        .triggers
        .iter()
        .find(|t| t.phase == Some(Phase::BeginCombat))
        .and_then(|t| t.execute.as_ref())
        .and_then(|e| e.repeat_for.clone());
    match &repeat_for {
        Some(QuantityExpr::Ref {
            qty:
                QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(t),
                },
        }) => assert!(
            t.properties.contains(&FilterProp::Goaded),
            "reach-guard: repeat_for filter must carry FilterProp::Goaded, got {:?}",
            t.properties
        ),
        other => {
            panic!(
                "reach-guard: combat trigger must carry an ObjectCount repeat_for, got {other:?}"
            )
        }
    }

    let mut runner = serene_runner(3, 1);
    let clues_before = count_clues(runner.state(), P0);

    runner.advance_to_phase(Phase::BeginCombat);
    runner.advance_until_stack_empty();

    assert_eq!(
        count_clues(runner.state(), P0) - clues_before,
        3,
        "Serene Sleuth investigates once per goaded creature P0 controls (3 Ox) → 3 Clues \
         (a Goaded-blind filter would make 5; a bare Investigate would make 1)"
    );
}

#[test]
fn object_for_each_investigate_is_lifted() {
    // Serene Sleuth's combat trigger: object for-each (goaded creatures you control).
    let sleuth = parse_oracle_text(SERENE_SLEUTH, "Serene Sleuth", &[], &[], &[]);
    // The combat trigger (a Phase trigger → `phase.is_some()`) is the object
    // for-each; the ETB Investigate is a ChangesZone trigger (`phase.is_none()`).
    let combat = sleuth
        .triggers
        .iter()
        .filter(|t| t.phase.is_some())
        .filter_map(|t| t.execute.as_ref())
        .find(|e| matches!(e.effect.as_ref(), Effect::Investigate))
        .expect("Serene Sleuth has an Investigate combat trigger");
    // Reach-guard: the effect really is Investigate (not Unimplemented) — the
    // positive branch the seam gate reads before the lift.
    assert!(
        matches!(combat.effect.as_ref(), Effect::Investigate),
        "reach-guard: Serene Sleuth's clause must parse to Investigate"
    );
    // The parameterized gate-widen lifts the object for-each to an `ObjectCount`
    // repeat_for whose filter is `Typed(Creature, You, [Goaded])`.
    //   Revert-probe (a) — narrow the gate back to `PlayerCount`-only → the
    //     `ObjectCount` is rejected → repeat_for None → the `ObjectCount` match FAILS.
    //   Revert-probe (b) — drop Gap A parser sites 14/15 → "goaded creature you
    //     control" no longer parses to a typed filter → `parse_for_each_clause`
    //     returns None → the seam finds no count → repeat_for None → FAILS.
    let filter = match &combat.repeat_for {
        Some(QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount { filter },
        }) => filter,
        other => panic!(
            "Serene Sleuth combat trigger must lift to an ObjectCount repeat_for, got {other:?}"
        ),
    };
    let typed = match filter {
        TargetFilter::Typed(t) => t,
        other => panic!("the lifted ObjectCount filter must be Typed, got {other:?}"),
    };
    // Gap A is load-bearing for THIS card: the filter must carry `FilterProp::Goaded`.
    assert!(
        typed.properties.contains(&FilterProp::Goaded),
        "the lifted filter must carry FilterProp::Goaded (Gap A), got {:?}",
        typed.properties
    );
    assert!(
        typed.type_filters.contains(&TypeFilter::Creature),
        "the lifted filter must be a creature filter, got {:?}",
        typed.type_filters
    );
    assert_eq!(
        typed.controller,
        Some(ControllerRef::You),
        "the lifted filter must be scoped to 'you control', got {:?}",
        typed.controller
    );

    // Sophina's attack trigger: object for-each ("nontoken attacking creature") —
    // Gap B (DEFERRED). parse_type_phrase's leading-adjective order-dependence means
    // this for-each does NOT yet parse to a member-count, so the seam leaves it a bare
    // Investigate. Deferred-gap tripwire: paired with a positive `Effect::Investigate`
    // reach-guard (non-vacuous), it asserts the CURRENT bare-Investigate state and
    // FLIPS to fail when Gap B lands — the signal to update this expectation.
    let sophina = parse_oracle_text(SOPHINA, "Sophina, Spearsage Deserter", &[], &[], &[]);
    let attack = sophina
        .triggers
        .iter()
        .filter_map(|t| t.execute.as_ref())
        .find(|e| matches!(e.effect.as_ref(), Effect::Investigate))
        .expect("Sophina has an Investigate attack trigger");
    assert!(
        matches!(attack.effect.as_ref(), Effect::Investigate),
        "reach-guard: Sophina's clause must parse to Investigate"
    );
    assert!(
        attack.repeat_for.is_none(),
        "Gap B deferred (parse_type_phrase leading-adjective order-dependence): \
         'nontoken attacking creature' does not yet lift — flip this guard when Gap B lands: {:?}",
        attack.repeat_for
    );
}
