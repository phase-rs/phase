//! Phase 1 — announced target-set cardinality for positional library placement
//! (`Effect::PutAtLibraryPosition`).
//!
//! CR 115.1 + CR 601.2c: "put any number of target …" and "put up to N target …"
//! announce a VARIABLE-SIZE target set. The number of targets is announced
//! before the targets themselves and does not change afterwards, so the
//! placement set IS the set chosen at announcement — the effect's `count` (a
//! lowering default of 1 for these clauses) must neither truncate the placement
//! nor gate a second prompt over already-chosen targets.
//!
//! Every card here is staged from its verbatim Oracle text, checked against
//! `cargo export-cards` at this phase's base commit.
//!
//! GREEN-AT-BASE LABELLING (charter standard, `charter-frozen.md`): every row
//! here that is green at this phase's base says so and names what makes it
//! non-vacuous. The standard allows three pairings — a paired red-at-base
//! positive, a mutation probe, or the phase's snapshot gate. THE SNAPSHOT GATE
//! IS NOT AVAILABLE TO THIS PHASE and is never cited below: it is empty for
//! this population (none of the 21 census card names appears in any of the 303
//! `.snap` files), so green there could not witness a regression here. Every
//! label below that claims a pairing names a paired red-at-base row.
//! The standard binds every green-at-base row this phase writes, whether or not
//! the charter requires that row. Three green-at-base rows have none of the
//! three pairings and comply BY DISCLOSURE — their labels say so and they are
//! reported: the Scroll Rack smoke (no discriminating pairing exists at
//! runtime) and the two Once and Future records (an in-row reach guard plus a
//! discriminating positive, which is not one of the three named pairings).

use engine::game::scenario::{CastOutcome, GameRunner, GameScenario, P0, P1};
use engine::types::ability::{EffectKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const CONJURERS_BAUBLE_ORACLE: &str = "{T}, Sacrifice this artifact: Put up to one target card \
     from your graveyard on the bottom of your library. Draw a card.";

const SWIFTGEAR_DRAKE_ORACLE: &str = "Flying, haste\nWhen this creature enters, put up to one \
     target card from a graveyard on the bottom of its owner's library.";

fn colorless(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn grant_priority(runner: &mut GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

fn library(runner: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
    runner.state().players[player.0 as usize]
        .library
        .iter()
        .copied()
        .collect()
}

/// Conjurer's Bauble on P0's battlefield, `graveyard` creature cards in P0's
/// graveyard, and a two-card library (`[lib_top, lib_second]`) so the chained
/// "Draw a card" is unambiguous and a bottom placement is observable. P0's hand
/// is empty, so the hand baseline is clean.
///
/// Returns `(runner, bauble, graveyard_ids, lib_top, lib_second)`.
fn bauble_board(graveyard: &[&str]) -> (GameRunner, ObjectId, Vec<ObjectId>, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bauble = scenario
        .add_artifact_from_oracle(P0, "Conjurer's Bauble", CONJURERS_BAUBLE_ORACLE)
        .id();
    let mut graveyard_ids = Vec::new();
    for name in graveyard {
        graveyard_ids.push(scenario.add_creature_to_graveyard(P0, name, 2, 2).id());
    }
    let lib_second = scenario.add_card_to_library_top(P0, "Library Second");
    let lib_top = scenario.add_card_to_library_top(P0, "Library Top");
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    (runner, bauble, graveyard_ids, lib_top, lib_second)
}

/// PAIRED POSITIVE / REACH GUARD for the two decline rows below. On the very
/// same board, declaring the announced target DOES move it: `g1` reaches the
/// bottom of P0's library and the chained draw still happens. Without this
/// row, "nothing moved and the run reached Priority" would be equally
/// satisfied by a fixture whose Bauble was never activated at all.
///
/// GREEN AT THIS PHASE'S BASE. It is a reach guard, not a discriminator; its
/// paired RED-AT-BASE row is
/// `conjurers_bauble_declined_target_with_nonempty_graveyard_resolves_as_noop`.
#[test]
fn conjurers_bauble_places_its_declared_target_on_the_bottom() {
    let (mut runner, bauble, gy, lib_top, lib_second) = bauble_board(&["Graveyard Bear"]);
    let g1 = gy[0];

    let outcome = runner.activate(bauble, 0).target_objects(&[g1]).resolve();

    assert_eq!(
        outcome.zone_of(bauble),
        Zone::Graveyard,
        "reach guard: the Bauble's own sacrifice cost must have been paid"
    );
    assert_eq!(
        outcome.zone_of(g1),
        Zone::Library,
        "the declared target must be placed into the library"
    );
    assert_eq!(
        library(&runner, P0),
        vec![lib_second, g1],
        "CR 401.4: the declared target goes to the BOTTOM, and the chained draw \
         takes the former top card"
    );
    assert_eq!(
        outcome.zone_of(lib_top),
        Zone::Hand,
        "the chained Draw must draw the former top card"
    );
}

/// Matrix row 6 — A-9 / P1-C5, the `optional_targeting: false` cohort, EMPTY
/// POOL sub-case. CR 115.6: "up to one target" allows zero targets to be
/// chosen, so with no legal card in the graveyard the activation must still
/// happen and the chained "Draw a card" must still resolve.
///
/// Paired positive reach guard:
/// `conjurers_bauble_places_its_declared_target_on_the_bottom`, on the same
/// board shape with one graveyard card declared.
#[test]
fn conjurers_bauble_with_empty_graveyard_activates_and_still_draws() {
    let (mut runner, bauble, gy, lib_top, lib_second) = bauble_board(&[]);
    assert!(gy.is_empty(), "this row stages an empty graveyard");

    let outcome = runner.activate(bauble, 0).resolve();

    assert_eq!(
        outcome.zone_of(bauble),
        Zone::Graveyard,
        "reach guard: the activation happened — its sacrifice cost was paid"
    );
    assert_eq!(
        outcome.hand_drawn(P0),
        1,
        "CR 115.6: zero legal targets must not stop the chained Draw"
    );
    assert_eq!(
        outcome.zone_of(lib_top),
        Zone::Hand,
        "the chained Draw must draw the former top card"
    );
    assert_eq!(
        library(&runner, P0),
        vec![lib_second],
        "nothing may be placed into the library — the only library delta is the draw"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the run must return to priority, got {:?}",
        outcome.final_waiting_for()
    );
}

/// Matrix row 6b — A-9 / P1-C5, the `optional_targeting: false` cohort,
/// NON-EMPTY POOL DECLINE sub-case. This row records an INTENDED, CR-mandated
/// class-level change, not a regression: CR 601.2c (a spell with a variable
/// number of targets announces how many it will choose) + CR 115.6 (a spell or
/// ability that requires targets may allow zero to be chosen) make the BASE
/// behaviour — one REQUIRED slot, so the announced target cannot be declined —
/// the rules-incorrect one. It changes for four of the five
/// `optional_targeting: false` members of the "up to one target" sub-class:
/// Boseiju Reaches Skyward, Conjurer's Bauble, Dovin's Dismissal, Treason of
/// Isengard. The fifth, Once and Future, is observationally inert under this
/// phase because its placement clause mints no target slot — see
/// `once_and_future_known_bad_else_ability_placement_gets_no_target_slot`.
///
/// Paired positive reach guard:
/// `conjurers_bauble_places_its_declared_target_on_the_bottom` — the same
/// fixture with `g1` declared, proving the fixture can move a card at all.
#[test]
fn conjurers_bauble_declined_target_with_nonempty_graveyard_resolves_as_noop() {
    let (mut runner, bauble, gy, lib_top, lib_second) = bauble_board(&["Graveyard Bear"]);
    let g1 = gy[0];

    // No `.target_objects(..)`: an empty declared-intent list is how the
    // harness expresses declining an optional slot.
    let outcome = runner.activate(bauble, 0).resolve();

    assert_eq!(
        outcome.zone_of(bauble),
        Zone::Graveyard,
        "reach guard: the activation happened — its sacrifice cost was paid"
    );
    assert_eq!(
        outcome.zone_of(g1),
        Zone::Graveyard,
        "CR 115.6: the declined target must stay in the graveyard"
    );
    assert_eq!(
        library(&runner, P0),
        vec![lib_second],
        "nothing may be placed into the library — the only library delta is the draw"
    );
    assert_eq!(
        outcome.hand_drawn(P0),
        1,
        "the chained Draw must still resolve after a declined target"
    );
    assert_eq!(
        outcome.zone_of(lib_top),
        Zone::Hand,
        "the chained Draw must draw the former top card"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the run must return to priority, got {:?}",
        outcome.final_waiting_for()
    );
    // CR 115.6: the placement instruction itself must still resolve on a
    // declined optional target. Pairing: MP-EMPTY, which makes the zero-target
    // branch fail and removes this event.
    assert!(
        emitted_placement_resolved(outcome.events()),
        "the placement effect must resolve even with the target declined"
    );
}

/// Swiftgear Drake in P0's hand with exactly `{5}` floating, plus a two-card
/// library for each player. `graveyard` seeds one creature card per
/// `(owner, name)` entry. Returns `(runner, drake, graveyard_ids)`.
fn drake_board(graveyard: &[(PlayerId, &str)]) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, colorless(5));
    let mut graveyard_ids = Vec::new();
    for (owner, name) in graveyard {
        graveyard_ids.push(scenario.add_creature_to_graveyard(*owner, name, 2, 2).id());
    }
    for player in [P0, P1] {
        scenario.add_card_to_library_top(player, "Library Second");
        scenario.add_card_to_library_top(player, "Library Top");
    }
    let drake = scenario
        .add_creature_to_hand_from_oracle(P0, "Swiftgear Drake", 2, 4, SWIFTGEAR_DRAKE_ORACLE)
        .from_oracle_text_with_keywords(&["Flying", "Haste"], SWIFTGEAR_DRAKE_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            generic: 5,
            shards: vec![],
        })
        .id();
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    (runner, drake, graveyard_ids)
}

/// Cast the Drake and let it resolve so its ETB trigger is put on the stack.
fn cast_drake_to_battlefield(runner: &mut GameRunner, drake: ObjectId) {
    runner.cast(drake).commit();
    while runner.state().objects[&drake].zone == Zone::Stack {
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass must advance the Drake's resolution");
    }
}

/// PAIRED POSITIVE / REACH GUARD for `swiftgear_drake_declined_target_resolves_as_noop`.
/// With ONE card in an opponent's graveyard, the ETB moves exactly that card to
/// the bottom of ITS OWNER's library — proving the fixture reaches the placement
/// at all, and that the Drake really did enter the battlefield.
///
/// GREEN AT THIS PHASE'S BASE. It is a reach guard, not a discriminator; its
/// paired RED-AT-BASE row is `swiftgear_drake_declined_target_resolves_as_noop`.
#[test]
fn swiftgear_drake_places_its_declared_target_on_the_bottom() {
    let (mut runner, drake, gy) = drake_board(&[(P1, "Opponent Graveyard Bear")]);
    let g1 = gy[0];
    let p1_library_before = library(&runner, P1);

    cast_drake_to_battlefield(&mut runner, drake);
    assert_eq!(
        runner.state().objects[&drake].zone,
        Zone::Battlefield,
        "reach guard: the Drake must have entered the battlefield"
    );
    match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection { target_slots, .. } => {
            assert!(
                !target_slots.is_empty(),
                "reach guard: the ETB must offer at least one slot"
            );
        }
        other => panic!("the Drake's ETB must stop at its target prompt, got {other:?}"),
    }
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(g1)],
        })
        .expect("declaring the graveyard card must be accepted");
    runner.advance_until_stack_empty();

    let mut expected = p1_library_before;
    expected.push(g1);
    assert_eq!(
        library(&runner, P1),
        expected,
        "the declared card must land on the bottom of ITS OWNER's library"
    );
}

/// Matrix row 7 — A-9 / P1-C5, the `optional_targeting: true` cohort. With
/// every graveyard empty, the ETB trigger resolves as a no-op and the run
/// reaches priority without the test answering a target prompt.
///
/// MEASURED BASE OUTCOME (plan matrix row 7 requires this verbatim) AND ITS
/// MECHANISM. The state half of this row was green at base. That is what the
/// resolver's branch order says should be impossible, so the mechanism was
/// determined by running base's `put_on_top.rs` under additive instrumentation.
/// Verbatim output at base:
///
/// ```text
/// BASEPROBE reached-resolver collected=0 source=ObjectId(5)
/// BASEPROBE returning-Err expected=1
/// ```
///
/// So at base the trigger DOES reach `put_on_top::resolve`, and the resolver
/// DOES return `Err(EffectError::InvalidParam("PutAtLibraryPosition requires a
/// target"))`. Every earlier exit is skipped exactly as a static read predicts:
/// `collected_targets` is empty; `expected` is `count: Fixed(1)` so the
/// `expected == 0` arm is skipped; `extract_in_zone()` answers `Some(Graveyard)`
/// so the `Hand | Library` prompt branch is skipped; and the filter is not
/// `TargetFilter::Any` so the tutor no-op branch is skipped. The trigger
/// machinery then SWALLOWS that `EffectError` with no state-observable effect,
/// which is the only reason the state assertions were green at base.
///
/// Base and candidate therefore pass this row for STRUCTURALLY DIFFERENT
/// REASONS — a swallowed `EffectError` at base, a genuine `expected == 0` no-op
/// at candidate — so this phase silently repaired a swallowed engine error on
/// this path.
///
/// The `EffectResolved` assertion below is what witnesses that repair, and it
/// makes this row RED AT BASE rather than green: base returns `Err` before
/// pushing any event, so no `EffectResolved { PutAtLibraryPosition }` exists in
/// the stream there. A state-only reading of this row cannot tell the two
/// apart, which is why it is read through `Outcome::events()`.
///
/// Paired positive reach guard:
/// `swiftgear_drake_places_its_declared_target_on_the_bottom` — "nothing moved
/// and the run reached Priority" is equally what a fixture whose Drake never
/// entered the battlefield produces, so this row also asserts the Drake IS on
/// the battlefield before the no-op assertions are read.
#[test]
fn swiftgear_drake_declined_target_resolves_as_noop() {
    let (mut runner, drake, gy) = drake_board(&[]);
    assert!(gy.is_empty(), "this row stages empty graveyards");
    let p0_library_before = library(&runner, P0);
    let p1_library_before = library(&runner, P1);

    // Routed through the fluent driver rather than `cast_drake_to_battlefield`
    // + `advance_until_stack_empty` because `GameRunner` exposes no event
    // stream; only `Outcome` does, and the event is this row's discriminator.
    let outcome = runner.cast(drake).resolve();

    // REACH GUARD: the Drake really entered the battlefield, so its ETB
    // trigger really was put on the stack.
    assert_eq!(
        outcome.zone_of(drake),
        Zone::Battlefield,
        "reach guard: the Drake must have entered the battlefield"
    );
    // RED AT BASE: base returns `Err(InvalidParam)` from `put_on_top::resolve`
    // before any event is pushed, and the trigger machinery swallows it. Only
    // the post-change `expected == 0` arm emits this event.
    assert!(
        outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::PutAtLibraryPosition,
                ..
            }
        )),
        "the declined placement must resolve as a REAL no-op and emit \
         EffectResolved{{PutAtLibraryPosition}} — base swallows an \
         InvalidParam here instead; events={:?}",
        outcome.events()
    );
    assert_eq!(
        library(&runner, P0),
        p0_library_before,
        "an empty announced target set must place nothing in P0's library"
    );
    assert_eq!(
        library(&runner, P1),
        p1_library_before,
        "an empty announced target set must place nothing in P1's library"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the run must reach priority, got {:?}",
        outcome.final_waiting_for()
    );
}

// ---------------------------------------------------------------------------
// A-2 / A-3 / A-4 / A-5 — the announced target set IS the placement set.
// ---------------------------------------------------------------------------

const GRAVEPURGE_ORACLE: &str =
    "Put any number of target creature cards from your graveyard on top of your library.\n\
     Draw a card.";

const REINFORCEMENTS_ORACLE: &str =
    "Put up to three target creature cards from your graveyard on top of your library.";

const MISINFORMATION_ORACLE: &str = "Put up to three target cards from an opponent's graveyard \
     on top of their library in any order.";

fn black_mana(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
        .collect()
}

fn red_mana(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
        .collect()
}

/// The board shared by rows 1, 2 and 4, so "the same board" is literal.
///
/// P0's graveyard holds three creature cards plus one SORCERY — a hostile that
/// sits in the same zone and matches everything about the filter except its
/// type, so it must never move. P0's library holds one pre-existing card, which
/// makes the placement order observable beneath the placed cards.
///
/// Returns `(runner, gravepurge, [c1, c2, c3], sorcery, lib_pre)`.
fn gravepurge_board(
    creature_names: &[&str],
) -> (GameRunner, ObjectId, Vec<ObjectId>, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, black_mana(3));
    let mut creatures = Vec::new();
    for name in creature_names {
        creatures.push(scenario.add_creature_to_graveyard(P0, name, 2, 2).id());
    }
    let sorcery = scenario
        .add_spell_to_graveyard(P0, "Graveyard Sorcery", false)
        .id();
    let lib_pre = scenario.add_card_to_library_top(P0, "Library Pre-existing");
    let gravepurge = scenario
        .add_spell_to_hand_from_oracle(P0, "Gravepurge", false, GRAVEPURGE_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Black],
        })
        .id();
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    (runner, gravepurge, creatures, sorcery, lib_pre)
}

/// Matrix row 1 — A-2. CR 601.2c + CR 401.4: every announced target is placed,
/// top-down in announcement order. Three targets are declared and all three
/// move; the effect's `count` (a lowering default of `Fixed(1)`) must not
/// truncate the placement to one.
///
/// The chained "Draw a card" then takes the card that ended up on top, which is
/// why `c1` is asserted in hand rather than in the library: that IS the
/// top-down order assertion, read through the draw.
#[test]
fn gravepurge_places_every_announced_target() {
    let (mut runner, gravepurge, creatures, sorcery, lib_pre) =
        gravepurge_board(&["Graveyard Bear A", "Graveyard Bear B", "Graveyard Bear C"]);
    let (c1, c2, c3) = (creatures[0], creatures[1], creatures[2]);

    let outcome = runner
        .cast(gravepurge)
        .target_objects(&[c1, c2, c3])
        .resolve();

    // REVERT-FAILING: at base only `c1` moves, so `c2`/`c3` stay in the
    // graveyard and the library still reads `[lib_pre]`.
    assert_eq!(
        library(&runner, P0),
        vec![c2, c3, lib_pre],
        "all three announced targets must be placed on top in announcement order \
         (c1 was placed on top and then drawn by the chained Draw)"
    );
    assert_eq!(
        outcome.zone_of(c1),
        Zone::Hand,
        "c1 was placed on TOP, so the chained Draw takes it — this is the order assertion"
    );
    // HOSTILE: same zone, same controller, wrong type.
    assert_eq!(
        outcome.zone_of(sorcery),
        Zone::Graveyard,
        "the sorcery is not a creature card and must never be placed"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the run must end at priority, got {:?}",
        outcome.final_waiting_for()
    );
}

/// Matrix row 2 — A-2's legality half. CR 601.2c: the number of targets is
/// announced before the targets are, so the prompt must offer one slot per
/// legal creature card rather than a single required slot.
///
/// Drives the raw `GameAction::CastSpell` on purpose: the fluent `SpellCast`
/// driver exists to ANSWER target prompts, so a row that must INSPECT one has
/// to open the pipeline by hand.
#[test]
fn gravepurge_announces_one_slot_per_legal_creature_card() {
    let (mut runner, gravepurge, creatures, sorcery, _lib_pre) =
        gravepurge_board(&["Graveyard Bear A", "Graveyard Bear B", "Graveyard Bear C"]);
    let card_id = runner.state().objects[&gravepurge].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: gravepurge,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("begin Gravepurge cast");

    let target_slots = match runner.state().waiting_for.clone() {
        WaitingFor::TargetSelection { target_slots, .. } => target_slots,
        other => panic!("Gravepurge must stop at its target prompt, got {other:?}"),
    };

    // RED AT BASE, and the reach guard for the per-slot assertions below, which
    // are vacuous over an empty `target_slots`.
    assert_eq!(
        target_slots.len(),
        3,
        "one slot per legal creature card (base offers a single required slot), got {target_slots:?}"
    );
    for (index, slot) in target_slots.iter().enumerate() {
        for creature in &creatures {
            assert!(
                slot.legal_targets.contains(&TargetRef::Object(*creature)),
                "slot {index} must offer every legal creature card, got {:?}",
                slot.legal_targets
            );
        }
        // GREEN AT BASE, and labelled as such: the type filter already excluded
        // the sorcery before this phase. Paired with the slot-count positive
        // above, which is red at base.
        assert!(
            !slot.legal_targets.contains(&TargetRef::Object(sorcery)),
            "slot {index} must not offer the sorcery, got {:?}",
            slot.legal_targets
        );
    }
}

/// Matrix row 3 — A-3, the bounded sibling of row 1. CR 115.6: "up to three
/// target" with FOUR eligible cards places exactly the three declared, and
/// leaves the legal-but-undeclared fourth untouched.
///
/// Reinforcements has no chained draw, so the full placed order is observable
/// directly in the library.
#[test]
fn reinforcements_places_exactly_the_declared_targets() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::White, ObjectId(0), false, vec![])],
    );
    let mut creatures = Vec::new();
    for name in ["Bear A", "Bear B", "Bear C", "Bear D"] {
        creatures.push(scenario.add_creature_to_graveyard(P0, name, 2, 2).id());
    }
    let lib_pre = scenario.add_card_to_library_top(P0, "Library Pre-existing");
    let reinforcements = scenario
        .add_spell_to_hand_from_oracle(P0, "Reinforcements", false, REINFORCEMENTS_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::White],
        })
        .id();
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    let (c1, c2, c3, c4) = (creatures[0], creatures[1], creatures[2], creatures[3]);

    let outcome = runner
        .cast(reinforcements)
        .target_objects(&[c1, c2, c3])
        .resolve();

    // REVERT-FAILING: at base only `c1` moves.
    assert_eq!(
        library(&runner, P0),
        vec![c1, c2, c3, lib_pre],
        "exactly the three declared targets are placed, top-down in announcement order"
    );
    // HOSTILE: a legal but UNDECLARED target must not be swept in.
    assert_eq!(
        outcome.zone_of(c4),
        Zone::Graveyard,
        "the fourth eligible card was never announced and must not move"
    );
}

/// Matrix row 4 — A-4. CR 107.1c + CR 115.6: "any number of target" includes
/// zero, so declining every target is legal, resolves as a no-op, and the
/// chained "Draw a card" still happens.
///
/// Positive reach guard: `gravepurge_places_every_announced_target`, on the
/// identical board — declaring targets there DOES move cards, so "nothing
/// moved" here is a real decline rather than a fixture that never cast.
#[test]
fn gravepurge_with_zero_targets_resolves_and_still_draws() {
    let (mut runner, gravepurge, creatures, sorcery, lib_pre) =
        gravepurge_board(&["Graveyard Bear A", "Graveyard Bear B", "Graveyard Bear C"]);

    // No `.target_objects(..)` — decline every announced target.
    let outcome = runner.cast(gravepurge).resolve();

    // REVERT-FAILING: at base the single slot is REQUIRED
    // (`optional_targeting: false`), so the harness cannot express this decline
    // at all.
    for (index, creature) in creatures.iter().enumerate() {
        assert_eq!(
            outcome.zone_of(*creature),
            Zone::Graveyard,
            "declined creature card {index} must stay in the graveyard"
        );
    }
    assert_eq!(outcome.zone_of(sorcery), Zone::Graveyard);
    assert_eq!(
        outcome.hand_drawn(P0),
        1,
        "CR 115.6: zero targets must not stop the chained Draw"
    );
    assert_eq!(
        outcome.zone_of(lib_pre),
        Zone::Hand,
        "the chained Draw takes the untouched pre-existing library top"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the run must end at priority, got {:?}",
        outcome.final_waiting_for()
    );
    // CR 115.6: the placement instruction itself must still resolve on an empty
    // announced set — an `expected == 0` placement is a no-op, not a skipped
    // instruction. Pairing: MP-EMPTY, which makes the zero-target branch fail
    // and removes this event.
    assert!(
        emitted_placement_resolved(outcome.events()),
        "the placement effect must resolve even with no declared target"
    );
}

/// Matrix row 4's HOSTILE variant — the same decline with an EMPTY graveyard.
/// There is nothing to announce at all, and the spell must still resolve and
/// still draw (`collected_targets.is_empty()` with `expected == 0`).
#[test]
fn gravepurge_with_empty_graveyard_resolves_and_still_draws() {
    let (mut runner, gravepurge, creatures, _sorcery, lib_pre) = gravepurge_board(&[]);
    assert!(creatures.is_empty(), "this row stages no creature cards");

    let outcome = runner.cast(gravepurge).resolve();

    assert_eq!(
        outcome.hand_drawn(P0),
        1,
        "an empty legal set must not stop the chained Draw"
    );
    assert_eq!(outcome.zone_of(lib_pre), Zone::Hand);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

/// Matrix row 5 — A-5. A multi-object placement out of an OPPONENT's graveyard
/// lands in that opponent's library, not the caster's.
///
/// NARROW CLAIM (stated deliberately): this fixture has a single non-caster
/// owner, so it establishes "a multi-object placement from an opponent's
/// graveyard lands in that opponent's library and not the caster's" — NOT
/// per-card owner routing in general. Owner routing itself is pre-existing and
/// untouched by this phase: the placement builds one uniform
/// `ZoneMoveRequest::effect(id, Zone::Library, source)` per object with no
/// per-owner branch, and the routing is the zone pipeline's.
#[test]
fn misinformation_places_all_chosen_cards_into_that_opponents_library() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, black_mana(1));
    let mut opponent_cards = Vec::new();
    for name in ["Their Card A", "Their Card B", "Their Card C"] {
        opponent_cards.push(scenario.add_creature_to_graveyard(P1, name, 2, 2).id());
    }
    // HOSTILE: the caster's own graveyard card is not owned by an opponent.
    let mine = scenario
        .add_creature_to_graveyard(P0, "My Own Card", 2, 2)
        .id();
    let p0_lib = scenario.add_card_to_library_top(P0, "P0 Library Card");
    let p1_lib = scenario.add_card_to_library_top(P1, "P1 Library Card");
    let misinformation = scenario
        .add_spell_to_hand_from_oracle(P0, "Misinformation", false, MISINFORMATION_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Black],
        })
        .id();
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    let (o1, o2, o3) = (opponent_cards[0], opponent_cards[1], opponent_cards[2]);

    let outcome = runner
        .cast(misinformation)
        .target_objects(&[o1, o2, o3])
        .resolve();

    // REVERT-FAILING: at base only `o1` moves.
    assert_eq!(
        library(&runner, P1),
        vec![o1, o2, o3, p1_lib],
        "every chosen card lands on top of the OPPONENT's library, in announcement order"
    );
    assert_eq!(
        library(&runner, P0),
        vec![p0_lib],
        "the caster's own library must be untouched"
    );
    assert_eq!(
        outcome.zone_of(mine),
        Zone::Graveyard,
        "the caster's own graveyard card is not a legal target and must not move"
    );
}

// ---------------------------------------------------------------------------
// A-8 — modal threading (written because M6 passed).
// ---------------------------------------------------------------------------

const BOW_OF_NYLEA_ORACLE: &str = "Attacking creatures you control have deathtouch.\n\
     {1}{G}, {T}: Choose one —\n\
     • Put a +1/+1 counter on target creature.\n\
     • Bow of Nylea deals 2 damage to target creature with flying.\n\
     • You gain 3 life.\n\
     • Put up to four target cards from your graveyard on the bottom of your library in any order.";

/// Bow of Nylea on P0's battlefield with `{1}{G}` floating, three cards in P0's
/// graveyard and one pre-existing library card.
/// Returns `(runner, bow, [g1, g2, g3], lib_pre, creature)`.
fn bow_board() -> (GameRunner, ObjectId, Vec<ObjectId>, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
        ],
    );
    let creature = scenario.add_creature(P0, "Target Bear", 2, 2).id();
    let mut graveyard = Vec::new();
    for name in ["Graveyard Card A", "Graveyard Card B", "Graveyard Card C"] {
        graveyard.push(scenario.add_creature_to_graveyard(P0, name, 2, 2).id());
    }
    let lib_pre = scenario.add_card_to_library_top(P0, "Library Pre-existing");
    let bow = scenario
        .add_artifact_from_oracle(P0, "Bow of Nylea", BOW_OF_NYLEA_ORACLE)
        .id();
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    (runner, bow, graveyard, lib_pre, creature)
}

/// Matrix row 8 — A-8 runtime. CR 601.2b announces the mode BEFORE CR 601.2c
/// announces the targets, so the chosen mode's own `MultiTargetSpec` sizes the
/// slots. Mode 4 places every chosen target on the bottom, in announcement
/// order (`Bottom` preserves selection order, unlike `Top`).
#[test]
fn bow_of_nylea_mode_four_places_every_chosen_target() {
    let (mut runner, bow, graveyard, lib_pre, _creature) = bow_board();
    let (g1, g2, g3) = (graveyard[0], graveyard[1], graveyard[2]);

    let outcome = runner
        .activate(bow, 0)
        .modes(&[3])
        .target_objects(&[g1, g2])
        .resolve();

    // REVERT-FAILING: at base only `g1` moves.
    assert_eq!(
        library(&runner, P0),
        vec![lib_pre, g1, g2],
        "both chosen targets go to the BOTTOM in announcement order"
    );
    assert_eq!(
        outcome.zone_of(g3),
        Zone::Graveyard,
        "the undeclared third graveyard card must not move"
    );
}

/// Matrix row 8's HOSTILE half — WRONG-MODE LEAKAGE. Activating mode 1 must
/// apply mode 1 and only mode 1: the chosen creature gets exactly one `+1/+1`
/// counter and no graveyard card moves. The counter assertion is the reach
/// guard for the no-move assertion; together they prove the CHOSEN mode's spec
/// sizes the slots, rather than a union over every mode.
///
/// GREEN AT THIS PHASE'S BASE. Its non-vacuity comes from a PAIRED
/// RED-AT-BASE POSITIVE on the same modal ability,
/// `bow_of_nylea_mode_four_places_every_chosen_target`, whose
/// `library == [lib_pre, g1, g2]` cannot pass at base. That pairing proves the
/// mode-addressing instrument is live, so "mode 4 placed nothing" here is a
/// real negative rather than a mode that never resolved. The in-row
/// `counters(creature, Plus1Plus1) == 1` reach guard is the local support for
/// that, not the pairing itself.
#[test]
fn bow_of_nylea_mode_one_does_not_place_any_card() {
    let (mut runner, bow, graveyard, lib_pre, creature) = bow_board();

    let outcome = runner
        .activate(bow, 0)
        .modes(&[0])
        .target_objects(&[creature])
        .resolve();

    // REACH GUARD: mode 1 actually happened.
    assert_eq!(
        outcome.counters(creature, CounterType::Plus1Plus1),
        1,
        "mode 1 must put exactly one +1/+1 counter on the chosen creature"
    );
    assert_eq!(
        library(&runner, P0),
        vec![lib_pre],
        "mode 4 was not chosen, so nothing may be placed into the library"
    );
    for (index, card) in graveyard.iter().enumerate() {
        assert_eq!(
            outcome.zone_of(*card),
            Zone::Graveyard,
            "graveyard card {index} must not move when mode 1 was chosen"
        );
    }
}

// ---------------------------------------------------------------------------
// P1-C6 / P1-C7 runtime probes (recorded, not gates).
// ---------------------------------------------------------------------------

const SCROLL_RACK_ORACLE: &str = "{1}, {T}: Exile any number of cards from your hand face down. \
     Put that many cards from the top of your library into your hand. Then look at the exiled \
     cards and put them on top of your library in any order.";

/// P1-C6 runtime half — RECORDED RUNTIME SMOKE. Scroll Rack's placement clause
/// is a chained sub-ability beneath an ability that carries its own
/// `multi_target` ("exile any number of cards from your hand"). This row
/// records that the placement puts back exactly the cards that were exiled.
///
/// The board exiles TWO cards on purpose: "places exactly the cards it exiled"
/// is satisfied vacuously by a smoke that exiles none.
///
/// GREEN AT THIS PHASE'S BASE, with NO discriminating pairing available at
/// runtime; it is reported under the charter's green-at-base standard
/// ("reported rather than silently kept"). It guards no containment mechanism,
/// because there is no inheritance path to contain: `game/ability_utils.rs`
/// builds each node's `multi_target` from that node's own definition
/// (`resolved.multi_target = def.multi_target.clone()`,
/// `overridden.multi_target = sub.multi_target.clone()`), so a descendant never
/// receives an ancestor's spec. Nor does it discriminate `put_on_top`'s count
/// source: for this placement node `collected_targets` (the exiled-by-source
/// scan) and `count = Ref(CardsExiledBySource)` already agree, and a mutation
/// reducing `put_on_top`'s condition to `target_choice_timing == Stack` left
/// this row green. The in-row `hand contains l1 && l2` reach guard shows only
/// that the exile-and-refill half ran.
#[test]
fn scroll_rack_places_exactly_the_cards_it_exiled() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let h1 = scenario.add_card_to_hand(P0, "Hand Card A");
    let h2 = scenario.add_card_to_hand(P0, "Hand Card B");
    let l3 = scenario.add_card_to_library_top(P0, "Library Third");
    let l2 = scenario.add_card_to_library_top(P0, "Library Second");
    let l1 = scenario.add_card_to_library_top(P0, "Library Top");
    let rack = scenario
        .add_artifact_from_oracle(P0, "Scroll Rack", SCROLL_RACK_ORACLE)
        .id();
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);

    runner
        .act(GameAction::ActivateAbility {
            source_id: rack,
            ability_index: 0,
        })
        .expect("begin Scroll Rack activation");

    // Answer each resolution prompt by hand: the fluent activation driver has no
    // `.effect_zone(..)` setter, so it would halt at the first choice.
    let mut declared: Vec<ObjectId> = vec![h1, h2];
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::EffectZoneChoice { cards, count, .. } => {
                // `!declared.is_empty()` FIRST: once the first prompt has
                // taken `declared`, `[].iter().all(..)` is vacuously TRUE, so
                // without this guard a later prompt would select the empty vec
                // instead of falling through to `cards.take(count)`.
                let chosen: Vec<ObjectId> =
                    if !declared.is_empty() && declared.iter().all(|d| cards.contains(d)) {
                        std::mem::take(&mut declared)
                    } else {
                        cards.iter().copied().take(count).collect()
                    };
                runner
                    .act(GameAction::SelectCards { cards: chosen })
                    .expect("EffectZoneChoice selection must be accepted");
            }
            // The ability sits on the stack at the post-announcement priority
            // window (CR 602.2b); pass to resolve it.
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must advance the Scroll Rack activation");
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("finalizing the mana payment must be accepted");
            }
            other => panic!("unexpected Scroll Rack prompt: {other:?}"),
        }
    }

    // REACH GUARD: the exile-and-refill half really ran, so the placement half
    // below is not being read over an activation that did nothing.
    let hand: Vec<ObjectId> = runner.state().players[P0.0 as usize]
        .hand
        .iter()
        .copied()
        .collect();
    assert!(
        hand.contains(&l1) && hand.contains(&l2),
        "reach guard: exiling two cards must draw the top two library cards into hand, \
         got hand {hand:?}"
    );
    // BOTH exiled cards come back — and only they, on top of the untouched rest.
    assert_eq!(
        library(&runner, P0),
        vec![h1, h2, l3],
        "Scroll Rack must place exactly the two cards it exiled back on top"
    );
}

const DRAFNAS_RESTORATION_ORACLE: &str = "Put any number of target artifact cards from target \
     player's graveyard on top of their library in any order.";

/// P1-C7 / R-1 PROBE — RECORDED, NOT A GATE. Drafna's Restoration gains a
/// `TargetSet` spec from this phase but is deliberately NOT promised: its
/// filter binds the graveyard's owner through an `Owned { TargetPlayer }`
/// PROPERTY rather than through the filter's `controller` field, and this row
/// records whether that reference is bound to the declared player at
/// slot-enumeration time.
///
/// Records the observation at the INITIAL target prompt: both graveyards'
/// artifact cards are offered, so the `Owned { TargetPlayer }` reference is NOT
/// narrowed to a declared player at slot enumeration. No target player is
/// chosen by this row — it inspects the prompt and never submits a
/// `SelectTargets`. Update it when that binding is fixed (residue R-1).
///
/// GREEN AT THIS PHASE'S BASE. Its non-vacuity comes from a PAIRED
/// RED-AT-BASE POSITIVE in the same class — placement into a NON-controller's
/// library out of a targeted graveyard — namely
/// `misinformation_places_all_chosen_cards_into_that_opponents_library`, whose
/// `library(P1) == [o1, o2, o3, p1_lib]` cannot pass at base. That pairing
/// proves the cross-player placement instrument is live. The three in-row reach
/// guards (non-empty `target_slots`, an offered `TargetRef::Player(P1)` slot,
/// and a non-empty set of object slots) are the local support, not the pairing
/// itself.
#[test]
fn drafnas_restoration_known_bad_target_player_owner_unbound_at_slot_enumeration() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![])],
    );
    let theirs = scenario
        .add_creature_to_graveyard(P1, "Their Artifact", 0, 0)
        .as_artifact()
        .id();
    let mine = scenario
        .add_creature_to_graveyard(P0, "My Artifact", 0, 0)
        .as_artifact()
        .id();
    scenario.add_card_to_library_top(P1, "P1 Library Card");
    let drafna = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Drafna's Restoration",
            false,
            DRAFNAS_RESTORATION_ORACLE,
        )
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    let card_id = runner.state().objects[&drafna].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: drafna,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("begin Drafna's Restoration cast");

    let target_slots = match runner.state().waiting_for.clone() {
        WaitingFor::TargetSelection { target_slots, .. } => target_slots,
        other => panic!("Drafna's Restoration must stop at its target prompt, got {other:?}"),
    };
    // REACH GUARD: a prompt with slots exists at all.
    assert!(
        !target_slots.is_empty(),
        "reach guard: the cast must offer at least one target slot"
    );

    let offers_a_player = target_slots
        .iter()
        .any(|slot| slot.legal_targets.contains(&TargetRef::Player(P1)));
    let object_slots: Vec<_> = target_slots
        .iter()
        .filter(|slot| {
            slot.legal_targets
                .iter()
                .any(|t| matches!(t, TargetRef::Object(_)))
        })
        .collect();

    assert!(
        offers_a_player,
        "the \"target player\" slot must be offered, got {target_slots:?}"
    );
    assert!(
        !object_slots.is_empty(),
        "reach guard: at least one artifact-card slot must be offered, got {target_slots:?}"
    );
    // RECORDED OBSERVATION: whether the `Owned { TargetPlayer }` reference is
    // resolved at slot-enumeration time, or still open to both graveyards.
    let sees_theirs = object_slots
        .iter()
        .any(|slot| slot.legal_targets.contains(&TargetRef::Object(theirs)));
    let sees_mine = object_slots
        .iter()
        .any(|slot| slot.legal_targets.contains(&TargetRef::Object(mine)));
    assert!(
        sees_theirs || sees_mine,
        "reach guard: the artifact-card slots must offer some artifact card, got {object_slots:?}"
    );
    // RECORDED (phase-1 residue R-1), both halves asserted so the tree carries
    // the finding: the `Owned { TargetPlayer }` reference is NOT narrowed to a
    // declared player at slot-enumeration time — BOTH graveyards' artifact
    // cards are offered. That is why Drafna's Restoration is
    // touched-but-unpromised by this phase; the phase widens its cardinality
    // without fixing the owner binding. Update this row when that is fixed.
    assert!(
        sees_theirs,
        "the target player's artifact card must be offered, got {object_slots:?}"
    );
    assert!(
        sees_mine,
        "RECORDED: the caster's own artifact card is offered too, so the \
         `Owned {{ TargetPlayer }}` reference is unbound at slot enumeration. \
         object_slots={object_slots:?}"
    );
}

// ---------------------------------------------------------------------------
// MED-2 follow-up — Once and Future (RECORDED, NOT GATES).
// ---------------------------------------------------------------------------

const ONCE_AND_FUTURE_ORACLE: &str = "Return target card from your graveyard to your hand. Put up \
     to one other target card from your graveyard on top of your library. Exile Once and Future.\n\
     Adamant — If at least three green mana was spent to cast this spell, instead return those \
     cards to your hand and exile Once and Future.";

/// Once and Future in P0's hand, two cards in P0's graveyard, one pre-existing
/// library card so a top placement is observable, and a mana pool of THREE
/// COLORLESS plus ONE GREEN.
///
/// The pool's colours are load-bearing: the card's Adamant rider replaces the
/// whole effect when at least three GREEN mana was spent, so paying `{3}` with
/// green would silently exercise the replacement branch instead of the
/// placement branch these rows are about.
///
/// Returns `(runner, once_and_future, g1, g2, lib_pre)`.
fn once_and_future_board() -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut pool = colorless(3);
    pool.push(ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]));
    scenario.with_mana_pool(P0, pool);
    let g1 = scenario
        .add_creature_to_graveyard(P0, "Grave Bear A", 2, 2)
        .id();
    let g2 = scenario
        .add_creature_to_graveyard(P0, "Grave Bear B", 2, 2)
        .id();
    let lib_pre = scenario.add_card_to_library_top(P0, "Library Pre-existing");
    let once_and_future = scenario
        .add_spell_to_hand_from_oracle(P0, "Once and Future", false, ONCE_AND_FUTURE_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Green],
        })
        .id();
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    (runner, once_and_future, g1, g2, lib_pre)
}

/// KNOWN-BAD STRUCTURAL RECORD — Once and Future's placement clause is not
/// reachable from slot enumeration, so this phase cannot resize it.
///
/// CR 601.2c's multi-instance paragraph ("if the spell uses the word 'target'
/// in multiple places, the same object or player can be chosen once for each
/// instance") plus CR 115.1 would have this card announce TWO target sets: one
/// required for "return target card", one `0..=1` for "put up to one OTHER
/// target card". The engine announces ONE.
///
/// WHY. The parser lowers the card around its Adamant rider: the root is the
/// "return target card" `ChangeZone`, the root's `sub_ability` is the Adamant
/// `ConditionInstead` node, and the placement clause is that node's
/// `else_ability`. `collect_target_slots_inner` (`game/ability_utils.rs`)
/// recurses into `sub_ability` only — it never visits `else_ability` — so the
/// placement's slot is never minted.
///
/// WHAT THIS PHASE DOES TO THE CARD. The parser change DOES attach
/// `multi_target = up_to(Fixed(1))` to that `else_ability` node. Nothing reads
/// it: with no slot, `ability.targets` stays empty and the node falls back to
/// its parent's single target, so `put_on_top::resolve` computes
/// `expected == 1` both ways — `collected_targets.len()` post-change,
/// `count: Fixed(1)` at base. The phase is OBSERVATIONALLY INERT on this card,
/// which is why it is owed no behavioural row.
///
/// GREEN AT THIS PHASE'S BASE, and base-independently so: this phase does not
/// touch `collect_target_slots_inner`, so the slot count is one on both sides.
/// Its non-vacuity comes from the reach guard that the cast reached a target
/// prompt at all, paired with the positive assertion that the single offered
/// slot is the RETURN clause's — it offers BOTH graveyard cards, which the
/// placement's `Another` filter could not.
/// That reach guard plus in-row positive is not one of the green-at-base
/// standard's three named pairings, so this row complies BY DISCLOSURE and is
/// reported, as the module header states.
///
/// Update this row when `else_ability` target slots are minted; the slot count
/// is then 2 and the sibling row below stops recording a defect.
#[test]
fn once_and_future_known_bad_else_ability_placement_gets_no_target_slot() {
    let (mut runner, once_and_future, g1, g2, _lib_pre) = once_and_future_board();
    let card_id = runner.state().objects[&once_and_future].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: once_and_future,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("begin Once and Future cast");

    let target_slots = match runner.state().waiting_for.clone() {
        WaitingFor::TargetSelection { target_slots, .. } => target_slots,
        other => panic!("Once and Future must stop at its target prompt, got {other:?}"),
    };
    // REACH GUARD: a prompt with slots exists at all, so the count below is not
    // read off a cast that never announced anything.
    assert!(
        !target_slots.is_empty(),
        "reach guard: the cast must offer at least one target slot"
    );
    // POSITIVE / DISCRIMINATOR: the one slot that IS minted belongs to the
    // RETURN clause, identified by the slot's own `effect_kind`. A slot minted
    // for the placement clause would carry
    // `EffectKind::PutAtLibraryPosition`; this one carries
    // `EffectKind::ChangeZone` (destination Hand). This is what proves the
    // instrument read the right prompt.
    //
    // NOTE for a later reader: the placement filter's `Another` property does
    // NOT distinguish the two graveyard cards and must not be used here.
    // `FilterProp::Another` (`game/filter.rs`) has TWO branches: it is
    // recipient-relative when the evaluation carries a `recipient_id` (CR
    // 613.4c, per-recipient layer contexts), and source-relative otherwise.
    // Target-slot enumeration during a cast carries no `recipient_id`, so the
    // source-relative branch is the one that applies here — and neither
    // graveyard card is the source, so a placement slot would offer BOTH.
    // Either way it is never relative to the SIBLING clause's declared target,
    // which is what the Oracle's "one OTHER target card" means. That
    // distinctness constraint is not expressed by this lowering.
    assert_eq!(
        target_slots[0].effect_kind,
        EffectKind::ChangeZone,
        "the minted slot must be the RETURN clause's, got {:?}",
        target_slots[0]
    );
    assert!(
        !target_slots
            .iter()
            .any(|slot| slot.effect_kind == EffectKind::PutAtLibraryPosition),
        "no slot may belong to the placement clause, got {target_slots:?}"
    );
    // The return clause's filter admits both graveyard cards, so the fixture is
    // not degenerate: the prompt really had a choice to make.
    assert!(
        target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(g1))
            && target_slots[0]
                .legal_targets
                .contains(&TargetRef::Object(g2)),
        "the return slot must offer both graveyard cards, got {:?}",
        target_slots[0].legal_targets
    );
    // RECORDED: one slot, not two. CR 601.2c would announce a second,
    // `0..=1`-sized set for "put up to one other target card".
    assert_eq!(
        target_slots.len(),
        1,
        "RECORDED: the `else_ability` placement clause mints no target slot, so only \
         the return clause's slot is announced, got {target_slots:?}"
    );
}

/// KNOWN-BAD RUNTIME RECORD, PRE-EXISTING and NOT caused by this phase — the
/// card's declared return target ends on top of the library instead of in hand.
///
/// With one target declared, Once and Future should put that card in P0's hand
/// (CR 115.1: the declared target is what the effect affects) and place nothing,
/// because no second target was announced for "up to one other target card".
/// Instead the slot-less placement clause falls back to the parent's target and
/// places it, so the card never reaches hand.
///
/// This is the runtime face of the structural defect recorded by
/// `once_and_future_known_bad_else_ability_placement_gets_no_target_slot`, and
/// it is GREEN AT THIS PHASE'S BASE — measured green at base by running this
/// module against base sources in a phase-1 review run, which reported this row
/// `ok` in a result of 7 passed / 10 failed. That run recorded no freshness
/// guard, so the measurement is only as strong as that run; re-measure by
/// running this module on a base checkout. The derivation follows.
/// `put_on_top::resolve` finishes
/// computing `collected_targets` before it first reads `multi_target`, and
/// `targeting::resolved_targets` never consults `multi_target` at all, so
/// `collected_targets` is identical on both sides. The sides therefore differ
/// only in where `expected` comes from: `collected_targets.len()` post-change
/// versus `count: Fixed(1)` at base. Exactly one card is placed here
/// post-change, so `collected_targets.len() == 1 == count` and the two
/// outcomes coincide.
///
/// REACH GUARDS. `zone_of(once_and_future) == Exile` proves the spell resolved
/// rather than fizzling, and `library[0] == g1` proves the ADAMANT branch did
/// NOT apply — that branch is a `Bounce` to hand and cannot place anything in a
/// library, so a library placement witnesses the `else_ability` running.
/// These reach guards are not one of the green-at-base standard's three named
/// pairings, so this row complies BY DISCLOSURE and is reported, as the module
/// header states.
///
/// Update this row when the return clause keeps its own target.
#[test]
fn once_and_future_known_bad_declared_return_target_is_placed_instead_of_returned() {
    let (mut runner, once_and_future, g1, g2, lib_pre) = once_and_future_board();

    let outcome = runner.cast(once_and_future).target_objects(&[g1]).resolve();

    // REACH GUARD: the spell resolved.
    assert_eq!(
        outcome.zone_of(once_and_future),
        Zone::Exile,
        "reach guard: Once and Future exiles itself on resolution"
    );
    // REACH GUARD + RECORDED: a library placement can only come from the
    // `else_ability`, so this also proves Adamant was correctly not applied.
    assert_eq!(
        library(&runner, P0),
        vec![g1, lib_pre],
        "RECORDED: the declared RETURN target is placed on top of the library by the \
         slot-less placement clause"
    );
    // RECORDED: the defect proper — "return target card ... to your hand" put
    // nothing in hand.
    assert!(
        runner.state().players[P0.0 as usize].hand.is_empty(),
        "RECORDED: nothing reaches hand, though the return clause declared a target; \
         hand={:?}",
        runner.state().players[P0.0 as usize].hand
    );
    // HOSTILE: the undeclared second graveyard card must not be swept in. This
    // half is a genuine pass, not a record.
    assert_eq!(
        outcome.zone_of(g2),
        Zone::Graveyard,
        "the second graveyard card was never announced and must not move"
    );
}

// ── Phase 2 — resolution-time "any number of <population>" ─────────────────
//
// CR 107.1c + CR 115.10a + CR 608.2d: "put any number of cards from your hand on
// the bottom of your library" uses no `target` word, so the cards are not
// targets. They are chosen while the effect is applied, and "any number"
// includes zero. The placement is therefore a resolution-time choice of zero
// through every eligible card, and the chained "draw that many cards plus one"
// reads how many were actually put.
//
// GREEN-AT-BASE LABELLING (charter standard) for this section: every row here
// that is green at this phase's base says so in its own doc comment and names
// what makes it non-vacuous, which is a mutation probe run against this phase's
// candidate or a paired red-at-base positive. The snapshot gate is never cited,
// for the reason the module doc gives.

const VALAKUT_AWAKENING_ORACLE: &str = "Put any number of cards from your hand on the bottom of \
     your library, then draw that many cards plus one.";

const INTO_THE_FIRE_ORACLE: &str = "Choose one —\n\
     • Into the Fire deals 2 damage to each creature, planeswalker, and battle.\n\
     • Put any number of cards from your hand on the bottom of your library, then draw that many \
     cards plus one.";

const BRAINSTORM_ORACLE: &str =
    "Draw three cards, then put two cards from your hand on top of your library in any order.";

/// The eligible hand for the proper-subset, stall and choose-zero boards.
const FOUR_HAND_CARDS: [&str; 4] = ["Hand Card 1", "Hand Card 2", "Hand Card 3", "Hand Card 4"];

/// Library card names, index 0 = top. Each row takes the prefix it needs.
const LIBRARY_CARDS: [&str; 6] = [
    "Library 1",
    "Library 2",
    "Library 3",
    "Library 4",
    "Library 5",
    "Library 6",
];

/// Stage `names` as P0's library with `names[0]` on top, returning the ids in
/// the same top-first order.
fn stage_library_top_first(scenario: &mut GameScenario, names: &[&str]) -> Vec<ObjectId> {
    let mut ids: Vec<ObjectId> = names
        .iter()
        .rev()
        .map(|name| scenario.add_card_to_library_top(P0, name))
        .collect();
    ids.reverse();
    ids
}

/// P0: `hand_names` cards in hand plus the spell; a library of `library_names`
/// (index 0 = top) so "draw that many plus one" is observable; `{2}{R}` floating.
/// P1: one 2/2 creature on the battlefield (Into the Fire's mode-1 hostile).
/// Returns (runner, spell, hand_ids, library_ids, p1_creature).
fn any_number_placement_board(
    spell_name: &str,
    oracle: &str,
    is_instant: bool,
    hand_names: &[&str],
    library_names: &[&str],
) -> (GameRunner, ObjectId, Vec<ObjectId>, Vec<ObjectId>, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, red_mana(3));
    let hand_ids: Vec<ObjectId> = hand_names
        .iter()
        .map(|name| scenario.add_card_to_hand(P0, name))
        .collect();
    let library_ids = stage_library_top_first(&mut scenario, library_names);
    let p1_creature = scenario.add_creature(P1, "Opponent Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, spell_name, is_instant, oracle)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    (runner, spell, hand_ids, library_ids, p1_creature)
}

/// True when the stream carries `EffectResolved { PutAtLibraryPosition }`: the
/// placement effect actually ran, rather than returning an `Err` that the
/// engine swallows with no observable effect.
fn emitted_placement_resolved(events: &[GameEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::PutAtLibraryPosition,
                ..
            }
        )
    })
}

/// Where the spell and P0's cards ended up, for reach-guard failure messages.
fn board_state(runner: &GameRunner, spell: ObjectId, events: &[GameEvent]) -> String {
    let state = runner.state();
    format!(
        "spell zone={:?}, stack={:?}, P0 hand={:?}, P0 library={:?}, waiting_for={:?}, events={events:?}",
        state.objects[&spell].zone,
        state.stack,
        state.players[P0.0 as usize].hand,
        state.players[P0.0 as usize].library,
        state.waiting_for,
    )
}

/// The assertions rows 7 and 8 share, over one board derivation: hand
/// `{h1..h4}`, library `L1..L6`, and `h1`, `h2` declared.
fn assert_proper_subset_placed_and_drew_three(
    runner: &GameRunner,
    outcome: &CastOutcome,
    spell: ObjectId,
    hand: &[ObjectId],
    library_ids: &[ObjectId],
) {
    let (h1, h2, h3, h4) = (hand[0], hand[1], hand[2], hand[3]);
    // REVERT-FAILING: at base the prompt demands exactly one card, so the
    // harness places `h1` only and `h2` stays in hand.
    assert_eq!(
        outcome.zone_of(h1),
        Zone::Library,
        "h1 was declared and must be placed; {}",
        board_state(runner, spell, outcome.events())
    );
    assert_eq!(
        outcome.zone_of(h2),
        Zone::Library,
        "h2 was declared and must be placed; a one-card prompt places only h1"
    );
    let library_after = library(runner, P0);
    let bottom_two = &library_after[library_after.len().saturating_sub(2)..];
    assert!(
        bottom_two.len() == 2 && bottom_two.contains(&h1) && bottom_two.contains(&h2),
        "CR 401.4: the two placed cards are the bottom two library cards, in either \
         order; library={library_after:?}"
    );
    // PROPER SUBSET: legal but undeclared cards stay in hand.
    assert_eq!(
        outcome.zone_of(h3),
        Zone::Hand,
        "h3 was not declared and must stay in hand"
    );
    assert_eq!(
        outcome.zone_of(h4),
        Zone::Hand,
        "h4 was not declared and must stay in hand"
    );
    // "That many plus one" = 2 + 1: L1..L3 are drawn and L4 stays.
    for (index, card) in library_ids.iter().take(3).enumerate() {
        assert_eq!(
            outcome.zone_of(*card),
            Zone::Hand,
            "L{} must be drawn: that many (2) plus one is 3",
            index + 1
        );
    }
    assert_eq!(
        outcome.zone_of(library_ids[3]),
        Zone::Library,
        "L4 must stay in the library: exactly 3 cards are drawn"
    );
    assert!(
        emitted_placement_resolved(outcome.events()),
        "the placement must resolve; events={:?}",
        outcome.events()
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the run must end at priority, got {:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(
        outcome.zone_of(spell),
        Zone::Graveyard,
        "the spell must have resolved into the graveyard"
    );
}

/// Matrix row 7 — B-2, Valakut Awakening. CR 107.1c + CR 608.2d: while the
/// effect is applied, the player puts a PROPER SUBSET of the hand on the bottom
/// (two of four eligible cards), and the chained "draw that many cards plus
/// one" reads the number actually put: 2 + 1 = 3.
///
/// RED AT BASE: the prompt demands exactly one card (`count 1, up_to false`),
/// so the harness places only `h1` and the draw is 1 + 1 = 2. Also red against
/// a prompt that demands exactly the maximum (mutation MP-FLAG-A), the other
/// half B-2 requires.
///
/// HOSTILE: `h3` and `h4` are legal but undeclared and must stay in hand.
///
/// CR 401.4: the owner may arrange cards put into the same library position;
/// the engine uses selection order (phase-1 residue R-3), so the bottom two
/// cards are asserted as a set, not as an order. Observed order after the
/// phase-2 edit, declaring `[h1, h2]`: `h1` second from the bottom, `h2` at the
/// very bottom.
#[test]
fn valakut_awakening_places_a_proper_subset_and_draws_that_many_plus_one() {
    let (mut runner, valakut, hand, library_ids, _p1_creature) = any_number_placement_board(
        "Valakut Awakening",
        VALAKUT_AWAKENING_ORACLE,
        true,
        &FOUR_HAND_CARDS,
        &LIBRARY_CARDS,
    );

    let outcome = runner
        .cast(valakut)
        .effect_zone(&[hand[0], hand[1]])
        .resolve();

    assert_proper_subset_placed_and_drew_three(&runner, &outcome, valakut, &hand, &library_ids);
}

/// Matrix row 8 — B-2, Into the Fire, the modal member of the promised set.
/// CR 601.2b + CR 700.2a: mode 2 is announced at cast, and its placement then
/// behaves exactly as Valakut Awakening's (row 7): a proper subset is put on
/// the bottom and "that many plus one" is drawn.
///
/// RED AT BASE (the same one-card truncation as row 7), and red under mutation
/// MP-FLAG-A.
///
/// REACH / HOSTILE GUARD (no phase-2 claim): P1's creature is still on the
/// battlefield with no damage marked, so mode 1 did not run.
///
/// CR 401.4: the bottom two cards are asserted as a set (phase-1 residue R-3).
/// Observed order after the phase-2 edit, declaring `[h1, h2]`: `h1` second
/// from the bottom, `h2` at the very bottom.
#[test]
fn into_the_fire_mode_two_places_a_proper_subset_and_draws_that_many_plus_one() {
    let (mut runner, into_the_fire, hand, library_ids, p1_creature) = any_number_placement_board(
        "Into the Fire",
        INTO_THE_FIRE_ORACLE,
        false,
        &FOUR_HAND_CARDS,
        &LIBRARY_CARDS,
    );

    let outcome = runner
        .cast(into_the_fire)
        .modes(&[1])
        .effect_zone(&[hand[0], hand[1]])
        .resolve();

    assert_eq!(
        outcome.zone_of(p1_creature),
        Zone::Battlefield,
        "reach guard: mode 1 (2 damage to each creature) must not have run"
    );
    assert_eq!(
        outcome.damage_marked(p1_creature),
        0,
        "reach guard: mode 1 must not have dealt damage"
    );
    assert_proper_subset_placed_and_drew_three(
        &runner,
        &outcome,
        into_the_fire,
        &hand,
        &library_ids,
    );
}

/// Matrix row 9 — B-3 / P2-C5, the EMPTY POOL. With no card left in hand the
/// placement puts nothing, the chained draw still resolves, and "that many" is
/// zero, so exactly one card is drawn.
///
/// GREEN AT BASE. Base and the candidate take different early returns in
/// `put_on_top::resolve`, and neither stamps "that many". Its pairings are two
/// mutation probes on the candidate's empty-pool branch: MP-EMPTY (the branch
/// returns an `Err`, so the `EffectResolved` reach guard goes red) and
/// MP-EMPTY-COUNT (the branch stamps a non-zero "that many", so the draw count
/// goes red while `EffectResolved` stays present). Row 7, on the same board
/// helper, is its paired red-at-base positive.
#[test]
fn valakut_awakening_with_empty_hand_places_nothing_and_draws_one() {
    let (mut runner, valakut, hand, library_ids, _p1_creature) = any_number_placement_board(
        "Valakut Awakening",
        VALAKUT_AWAKENING_ORACLE,
        true,
        &[],
        &LIBRARY_CARDS[..3],
    );
    assert!(
        hand.is_empty(),
        "this row stages a hand holding only the spell"
    );

    let outcome = runner.cast(valakut).resolve();

    // REACH GUARDS, read before the no-op assertions.
    assert!(
        emitted_placement_resolved(outcome.events()),
        "reach guard: the empty-pool placement must resolve as a real no-op; events={:?}",
        outcome.events()
    );
    assert_eq!(
        outcome.zone_of(valakut),
        Zone::Graveyard,
        "reach guard: the spell must have resolved"
    );
    assert_eq!(
        outcome.zone_of(library_ids[0]),
        Zone::Hand,
        "the chained draw takes the top card"
    );
    assert_eq!(
        library(&runner, P0),
        vec![library_ids[1], library_ids[2]],
        "nothing is placed, and exactly one card is drawn from the top"
    );
    assert_eq!(
        outcome.hand_drawn(P0),
        1,
        "that many (0) plus one is exactly one card"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the run must end at priority, got {:?}",
        outcome.final_waiting_for()
    );
}

/// Matrix row 10 — P2-C6 / B-7, the harness consequence of the any-number
/// prompt. This is the measured consequence cited by the TEST-HARNESS note in
/// `put_on_top::resolve`.
///
/// (a) `SpellCast::resolve` with no `.effect_zone(..)` intent stops at the
/// placement prompt, which offers every card in hand with `up_to: true`,
/// `min_count: 0` and `count: 4`. (b) `GameRunner::advance_until_stack_empty`
/// then leaves that `up_to` prompt pending and places nothing.
///
/// RED AT BASE: the base prompt is `count 1, up_to false`, and at base
/// `advance_until_stack_empty` auto-answers it. (b) is observed before (a) is
/// asserted, so a red (a) still reports (b). Red under mutation MP-FLAG-A.
#[test]
fn valakut_awakening_stalls_at_any_number_prompt_without_declared_cards() {
    let (mut runner, valakut, hand, _library_ids, _p1_creature) = any_number_placement_board(
        "Valakut Awakening",
        VALAKUT_AWAKENING_ORACLE,
        true,
        &FOUR_HAND_CARDS,
        &LIBRARY_CARDS[..3],
    );

    // (a) `drive_resolution` with no declared cards.
    let outcome = runner.cast(valakut).resolve();
    let prompt_a = outcome.final_waiting_for().clone();

    // (b) The auto-answering driver, run over whatever (a) left pending.
    runner.advance_until_stack_empty();
    let waiting_b = runner.state().waiting_for.clone();
    let hand_zones_b: Vec<Zone> = hand
        .iter()
        .map(|id| runner.state().objects[id].zone)
        .collect();
    let observed_b = format!(
        "(b) observed: waiting_for={waiting_b:?}, hand zones={hand_zones_b:?}; after (b): {}",
        board_state(&runner, valakut, outcome.events())
    );

    let WaitingFor::EffectZoneChoice {
        effect_kind,
        zone,
        cards,
        up_to,
        min_count,
        count,
        ..
    } = &prompt_a
    else {
        panic!(
            "(a) resolve() without declared cards must stop at the placement prompt, \
             got {prompt_a:?}; {observed_b}"
        );
    };
    // REACH GUARD for (a): it is the placement's own prompt over the hand.
    assert_eq!(
        *effect_kind,
        EffectKind::PutAtLibraryPosition,
        "(a) reach guard; {observed_b}"
    );
    assert_eq!(*zone, Zone::Hand, "(a) reach guard; {observed_b}");
    assert!(
        cards.len() == 4 && hand.iter().all(|card| cards.contains(card)),
        "(a) the prompt must offer exactly the four hand cards, got {cards:?}; {observed_b}"
    );
    assert!(
        *up_to,
        "(a) CR 107.1c: an any-number prompt must accept fewer than its count; {observed_b}"
    );
    assert_eq!(*min_count, 0, "(a) zero is a legal choice; {observed_b}");
    assert_eq!(
        *count, 4,
        "(a) the maximum is every card in hand; {observed_b}"
    );

    // (b) The any-number prompt stays pending, and nothing was placed.
    assert!(
        matches!(
            waiting_b,
            WaitingFor::EffectZoneChoice {
                effect_kind: EffectKind::PutAtLibraryPosition,
                up_to: true,
                ..
            }
        ),
        "(b) advance_until_stack_empty must leave the any-number prompt pending, \
         got {waiting_b:?}"
    );
    assert!(
        hand_zones_b.iter().all(|zone| *zone == Zone::Hand),
        "(b) no hand card may be placed while the prompt is pending, got {hand_zones_b:?}"
    );
}

/// Matrix row 11 — CR 107.1c: "any number" includes zero, so choosing no card
/// from a NON-EMPTY hand is legal. Nothing is placed, "that many" is zero, and
/// exactly one card is drawn.
///
/// "Choose zero" cannot be declared through `.effect_zone(&[])` (an empty
/// intent is no intent), so the row submits `SelectCards { cards: [] }` itself.
///
/// RED AT BASE: the base validator rejects an empty selection against its
/// exactly-one prompt. Red under mutation MP-FLAG-A. Its paired positive is
/// row 7 on the same board helper.
#[test]
fn valakut_awakening_choosing_zero_cards_from_a_nonempty_hand_draws_one() {
    let (mut runner, valakut, hand, library_ids, _p1_creature) = any_number_placement_board(
        "Valakut Awakening",
        VALAKUT_AWAKENING_ORACLE,
        true,
        &FOUR_HAND_CARDS,
        &LIBRARY_CARDS[..3],
    );

    let outcome = runner.cast(valakut).resolve();
    // REACH GUARD: stalled at the placement's own prompt.
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::EffectZoneChoice {
                effect_kind: EffectKind::PutAtLibraryPosition,
                ..
            }
        ),
        "reach guard: the cast must stop at the placement prompt, got {:?}; {}",
        outcome.final_waiting_for(),
        board_state(&runner, valakut, outcome.events())
    );

    let result = runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("zero is a legal any-number choice");

    assert!(
        emitted_placement_resolved(&result.events),
        "reach guard: the zero selection must resolve the placement; events={:?}",
        result.events
    );
    for (index, card) in hand.iter().enumerate() {
        assert_eq!(
            runner.state().objects[card].zone,
            Zone::Hand,
            "hand card {} was not chosen and must stay in hand",
            index + 1
        );
    }
    assert_eq!(
        runner.state().objects[&library_ids[0]].zone,
        Zone::Hand,
        "that many (0) plus one draws the top card"
    );
    assert_eq!(
        runner.state().objects[&library_ids[1]].zone,
        Zone::Library,
        "exactly one card is drawn"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the run must end at priority, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().objects[&valakut].zone,
        Zone::Graveyard,
        "the spell must have resolved into the graveyard"
    );
}

/// Matrix row 12 — preservation. Brainstorm's placement is an EXACT count
/// ("put two cards"), so its prompt must still demand exactly two cards
/// (`up_to: false`, `count: 2`) and reject a one-card selection.
///
/// GREEN AT BASE. Its pairing is mutation MP-FLAG-T (the prompt flag forced
/// `true`), which must turn it red.
#[test]
fn brainstorm_prompt_still_demands_exactly_two() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![])],
    );
    let library_ids = stage_library_top_first(&mut scenario, &LIBRARY_CARDS[..5]);
    let brainstorm = scenario
        .add_spell_to_hand_from_oracle(P0, "Brainstorm", true, BRAINSTORM_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    let drawn = &library_ids[..3];

    let outcome = runner.cast(brainstorm).resolve();

    let WaitingFor::EffectZoneChoice {
        effect_kind,
        zone,
        cards,
        up_to,
        count,
        ..
    } = outcome.final_waiting_for()
    else {
        panic!(
            "reach guard: Brainstorm must stop at its placement prompt, got {:?}; {}",
            outcome.final_waiting_for(),
            board_state(&runner, brainstorm, outcome.events())
        );
    };
    // REACH GUARDS: the placement's prompt over the three drawn cards.
    assert_eq!(*effect_kind, EffectKind::PutAtLibraryPosition);
    assert_eq!(*zone, Zone::Hand);
    assert!(
        cards.len() == 3 && drawn.iter().all(|card| cards.contains(card)),
        "reach guard: the prompt must offer exactly the three drawn cards, got {cards:?}"
    );
    assert_eq!(outcome.zone_of(library_ids[3]), Zone::Library);
    assert_eq!(outcome.zone_of(library_ids[4]), Zone::Library);
    // PRESERVATION: an exact count still demands exactly that count.
    assert!(!*up_to, "an exact-count prompt must not accept fewer cards");
    assert_eq!(*count, 2, "Brainstorm puts exactly two cards");
    assert!(
        runner
            .act(GameAction::SelectCards {
                cards: vec![library_ids[0]]
            })
            .is_err(),
        "a one-card selection must be rejected by an exactly-two prompt"
    );
}
