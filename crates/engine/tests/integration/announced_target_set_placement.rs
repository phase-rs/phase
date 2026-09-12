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

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
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
/// the rules-incorrect one. It changes for all five `optional_targeting: false`
/// members of the "up to one target" sub-class: Boseiju Reaches Skyward,
/// Conjurer's Bauble, Dovin's Dismissal, Once and Future, Treason of Isengard.
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

    cast_drake_to_battlefield(&mut runner, drake);
    assert_eq!(
        runner.state().objects[&drake].zone,
        Zone::Battlefield,
        "reach guard: the Drake must have entered the battlefield"
    );
    runner.advance_until_stack_empty();

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
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the run must reach priority, got {:?}",
        runner.state().waiting_for
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

/// P1-C6 runtime half — ANCESTOR CONTAINMENT. Scroll Rack's placement clause is
/// a chained sub-ability beneath an ability that carries its own `multi_target`
/// ("exile any number of cards from your hand"). The descendant placement must
/// not inherit that ancestor spec and start placing the wrong set.
///
/// The board exiles TWO cards on purpose: "places exactly the cards it exiled"
/// is satisfied vacuously by a smoke that exiles none.
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
                let chosen: Vec<ObjectId> = if declared.iter().all(|d| cards.contains(d)) {
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
/// The recorded observation is asserted so the tree carries it: with P1 chosen
/// as the target player, the object slots offer P1's artifact cards. If a later
/// change moves this, the assertion below is where it surfaces.
#[test]
fn drafnas_restoration_binds_the_declared_target_player_at_slot_enumeration() {
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
