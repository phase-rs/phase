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

use engine::ai_support::legal_actions;
use engine::game::engine::apply_with_rejection;
use engine::game::scenario::{CastOutcome, GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::game::visibility::filter_state_for_viewer;
use engine::types::ability::{
    CardPlayMode, CastingPermission, Duration, EffectKind, LibraryPosition,
    PlayFromExileProvenance, TargetRef,
};
use engine::types::action_rejection::ActionRejectionCode;
use engine::types::actions::GameAction;
use engine::types::card_type::Supertype;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor, ZoneOpponentChooserPurpose};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::CastFrequency;
use engine::types::zones::{EtbTapState, Zone};

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

/// The arrangement prompt for two or more announced targets put into one
/// position of `owner`'s library at the same time, answered by `chooser`.
/// Asserted unconditionally: any other `WaitingFor` (including priority after
/// an unprompted placement) fails the row.
fn assert_arrangement_prompt(
    waiting_for: &WaitingFor,
    chooser: PlayerId,
    owner: PlayerId,
    arranger: Option<PlayerId>,
    cards: &[ObjectId],
    position: LibraryPosition,
) {
    match waiting_for {
        WaitingFor::EffectZoneChoice {
            player,
            cards: offered,
            count,
            min_count,
            up_to,
            effect_kind,
            library_position,
            mass_library_order,
            ..
        } => {
            assert_eq!(*player, chooser, "the prompt names the arranging player");
            assert_eq!(
                offered, cards,
                "the prompt names exactly that owner's cards"
            );
            assert_eq!(
                (*count, *min_count, *up_to),
                (cards.len(), cards.len(), false)
            );
            assert_eq!(*effect_kind, EffectKind::PutAtLibraryPosition);
            assert_eq!(*library_position, Some(position));
            let batch = mass_library_order
                .as_ref()
                .expect("the prompt carries the frozen owner batch");
            assert_eq!(batch.owner, owner, "the batch is the owner's cards");
            assert_eq!(
                batch.arranger, arranger,
                "the batch records the arranger the instruction named"
            );
        }
        other => {
            panic!("expected {chooser:?}'s ordering prompt over {owner:?}'s cards, got {other:?}")
        }
    }
}

/// CR 401.4: the owner's arrangement prompt — the instruction names no
/// arranger, so the cards' owner orders them.
fn assert_owner_arrangement_prompt(
    waiting_for: &WaitingFor,
    owner: PlayerId,
    cards: &[ObjectId],
    position: LibraryPosition,
) {
    assert_arrangement_prompt(waiting_for, owner, owner, None, cards, position);
}

/// CR 101.1 + CR 608.2c: the controller's arrangement prompt — "in any order"
/// closing the controller's instruction makes `chooser`, the controller,
/// arrange `owner`'s cards instead of the owner (CR 401.4 default).
fn assert_controller_arrangement_prompt(
    waiting_for: &WaitingFor,
    chooser: PlayerId,
    owner: PlayerId,
    cards: &[ObjectId],
    position: LibraryPosition,
) {
    assert_arrangement_prompt(waiting_for, chooser, owner, Some(chooser), cards, position);
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
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

const COMMANDEER_ORACLE: &str = "You may exile two blue cards from your hand rather than pay \
     this spell's mana cost.\nGain control of target noncreature spell. You may choose new \
     targets for it. (If that spell is an artifact, enchantment, or planeswalker, the permanent \
     enters under your control.)";

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
/// top-down in the owner's arrangement (CR 401.4), made as the effect is
/// applied (CR 608.2d). Three targets are declared and all three move; the
/// effect's `count` (a lowering default of `Fixed(1)`) must not truncate the
/// placement to one.
///
/// P0 owns all three cards, so P0 is prompted to order them. The chained
/// "Draw a card" is parked behind that prompt: at the prompt nothing has moved
/// and nothing has been drawn. P0 then orders `[c3, c1, c2]`, which differs
/// from the announcement, and the draw takes `c3`, the card P0 put on top —
/// that IS the top-down order assertion, read through the draw.
#[test]
fn gravepurge_places_every_announced_target() {
    let (mut runner, gravepurge, creatures, sorcery, lib_pre) =
        gravepurge_board(&["Graveyard Bear A", "Graveyard Bear B", "Graveyard Bear C"]);
    let (c1, c2, c3) = (creatures[0], creatures[1], creatures[2]);

    let outcome = runner
        .cast(gravepurge)
        .target_objects(&[c1, c2, c3])
        .resolve();

    // REVERT-FAILING: without the owner arrangement the placement and the draw
    // complete inside `.resolve()` and no prompt opens.
    assert_owner_arrangement_prompt(
        outcome.final_waiting_for(),
        P0,
        &[c1, c2, c3],
        LibraryPosition::Top,
    );
    for creature in [c1, c2, c3] {
        assert_eq!(
            zone(&runner, creature),
            Zone::Graveyard,
            "nothing moves while the owner decides"
        );
    }
    assert!(
        runner.state().players[P0.0 as usize].hand.is_empty(),
        "the chained Draw waits for the arrangement"
    );

    runner
        .act(GameAction::SelectCards {
            cards: vec![c3, c1, c2],
        })
        .expect("P0 submits their arrangement");

    assert_eq!(
        library(&runner, P0),
        vec![c1, c2, lib_pre],
        "the owner's arrangement is the top of the library (c3 was placed on top \
         and then drawn by the chained Draw)"
    );
    assert_eq!(
        zone(&runner, c3),
        Zone::Hand,
        "c3 was placed on TOP, so the chained Draw takes it — this is the order assertion"
    );
    // HOSTILE: same zone, same controller, wrong type.
    assert_eq!(
        zone(&runner, sorcery),
        Zone::Graveyard,
        "the sorcery is not a creature card and must never be placed"
    );
    assert_eq!(
        zone(&runner, gravepurge),
        Zone::Graveyard,
        "the resolved sorcery is in its owner's graveyard"
    );
    assert!(runner.state().stack.is_empty(), "the spell resolved");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the run must end at priority, got {:?}",
        runner.state().waiting_for
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
/// directly in the library. P0 owns the three cards and arranges them
/// (CR 401.4) as `[c2, c3, c1]`, which differs from the announcement.
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

    // REVERT-FAILING: without the owner arrangement no prompt opens.
    assert_owner_arrangement_prompt(
        outcome.final_waiting_for(),
        P0,
        &[c1, c2, c3],
        LibraryPosition::Top,
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![c2, c3, c1],
        })
        .expect("P0 submits their arrangement");

    assert_eq!(
        library(&runner, P0),
        vec![c2, c3, c1, lib_pre],
        "exactly the three declared targets are placed, top-down in the owner's arrangement"
    );
    // HOSTILE: a legal but UNDECLARED target must not be swept in.
    assert_eq!(
        zone(&runner, c4),
        Zone::Graveyard,
        "the fourth eligible card was never announced and must not move"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
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

/// Matrix row 5 — A-5, and maintainer review 4's row (N1). A multi-object
/// placement out of an OPPONENT's graveyard lands in that opponent's library,
/// not the caster's, in the order the CASTER arranges. Misinformation's
/// "… on top of their library in any order" is part of the caster's
/// instruction (CR 608.2c), and the card's text takes precedence over the
/// CR 401.4 owner default (CR 101.1; WotC ruling: "You don't need to show the
/// opponent what order you put the cards on top of their library."). P0 makes
/// that choice as the effect is applied (CR 608.2d); P1, the owner, cannot.
///
/// REVERT-FAILING: with the arranger dropped (parser gate removed, or the
/// prompt built for `batch.owner`), (a) the prompt names P1 and (b) P1's
/// submission is accepted.
///
/// NARROW CLAIM (stated deliberately): this fixture has a single non-caster
/// owner, so it establishes "a multi-object placement from an opponent's
/// graveyard lands in that opponent's library and not the caster's" — NOT
/// per-card owner routing in general. Owner routing itself is the zone
/// pipeline's (one `ZoneMoveRequest::effect(id, Zone::Library, source)` per
/// object); the per-owner grouping only decides which cards each prompt orders.
#[test]
fn misinformation_places_all_chosen_cards_into_that_opponents_library_in_its_casters_arrangement() {
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

    // (a) REVERT-FAILING: the caster, not the opponent who owns the cards, is
    // prompted; the batch is still P1's cards.
    assert_controller_arrangement_prompt(
        outcome.final_waiting_for(),
        P0,
        P1,
        &[o1, o2, o3],
        LibraryPosition::Top,
    );
    // Reach guard: Misinformation is mid-resolution and nothing has moved.
    assert_eq!(zone(&runner, misinformation), Zone::Stack);
    for card in [o1, o2, o3] {
        assert_eq!(zone(&runner, card), Zone::Graveyard);
    }

    // (b) REVERT-FAILING: the owner cannot submit the caster's arrangement, and
    // the rejected submission changes nothing.
    let before = runner.state().clone();
    let rejection = apply_with_rejection(
        runner.state_mut(),
        P1,
        GameAction::SelectCards {
            cards: vec![o3, o1, o2],
        },
    )
    .expect_err("P1 must not submit the caster's arrangement");
    assert_eq!(rejection.code, ActionRejectionCode::WrongPlayer);
    assert!(
        *runner.state() == before,
        "a rejected submission leaves the game unchanged"
    );

    // (c) The caster's arrangement is the exact top of P1's library.
    runner
        .act(GameAction::SelectCards {
            cards: vec![o3, o1, o2],
        })
        .expect("P0 submits their arrangement");

    assert_eq!(
        library(&runner, P1),
        vec![o3, o1, o2, p1_lib],
        "every chosen card lands on top of the OPPONENT's library, in the caster's \
         arrangement"
    );
    assert_eq!(
        library(&runner, P0),
        vec![p0_lib],
        "the caster's own library must be untouched"
    );
    assert_eq!(
        zone(&runner, mine),
        Zone::Graveyard,
        "the caster's own graveyard card is not a legal target and must not move"
    );
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
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
/// slots. Mode 4 places every chosen target on the bottom, in the controller's
/// arrangement: its closing "in any order" names the controller (CR 101.1 +
/// CR 608.2c), who here also owns the cards. P0 orders `[g2, g1]`, which
/// differs from the announcement.
#[test]
fn bow_of_nylea_mode_four_places_every_chosen_target() {
    let (mut runner, bow, graveyard, lib_pre, _creature) = bow_board();
    let (g1, g2, g3) = (graveyard[0], graveyard[1], graveyard[2]);

    let outcome = runner
        .activate(bow, 0)
        .modes(&[3])
        .target_objects(&[g1, g2])
        .resolve();

    // REVERT-FAILING: without the arrangement no prompt opens.
    assert_controller_arrangement_prompt(
        outcome.final_waiting_for(),
        P0,
        P0,
        &[g1, g2],
        LibraryPosition::Bottom,
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![g2, g1],
        })
        .expect("P0 submits their arrangement");

    assert_eq!(
        library(&runner, P0),
        vec![lib_pre, g2, g1],
        "both chosen targets go to the BOTTOM in the controller's arrangement"
    );
    assert_eq!(
        zone(&runner, g3),
        Zone::Graveyard,
        "the undeclared third graveyard card must not move"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
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
/// `library == [lib_pre, g2, g1]` cannot pass at base. That pairing proves the
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
// CR 401.4 owner arrangement — Nth-from-top and a parked delayed draw.
// ---------------------------------------------------------------------------

const RIPTIDE_GEARHULK_ORACLE: &str = "Double strike\n\
     Prowess\n\
     When this creature enters, for each opponent, put up to one target nonland permanent that \
     player controls into its owner's library third from the top.";

const LODESTONE_BAUBLE_ORACLE: &str = "{1}, {T}, Sacrifice this artifact: Put up to four target \
     basic land cards from a player's graveyard on top of their library in any order. That \
     player draws a card at the beginning of the next turn's upkeep.";

const P2: PlayerId = PlayerId(2);

const P3: PlayerId = PlayerId(3);

const EXTRACTOR_DEMON_ORACLE: &str = "Flying\n\
     Whenever another creature leaves the battlefield, you may have target player mill two \
     cards.\n\
     Unearth {2}{B} ({2}{B}: Return this card from your graveyard to the battlefield. It gains \
     haste. Exile it at the beginning of the next end step or if it would leave the battlefield. \
     Unearth only as a sorcery.)";

/// The four-player Riptide Gearhulk board. P0 is active and casts Riptide.
/// - P1 owns and controls Extractor Demon, and owns `borrowed`, which P2
///   controls (`base_controller = P2`, no control effect).
/// - P3 owns and controls `p3_trinket`, a noncreature artifact, and, unless
///   built with [`P3Bear::Absent`], the creature `p3_bear`.
/// - P1's library is `[f1, f2, f3]` and P3's is `[g1, g2, g3]`.
/// - P0 also has Lodestone Bauble, and P1 two basic lands in the graveyard,
///   for the later single-owner arrangement rows; P0's pool keeps one
///   colorless mana after Riptide for the Bauble's {1}.
struct RiptideBoard {
    runner: GameRunner,
    riptide: ObjectId,
    demon: ObjectId,
    borrowed: ObjectId,
    p3_bear: Option<ObjectId>,
    p3_trinket: ObjectId,
    bauble: ObjectId,
    lands: [ObjectId; 2],
    p1_library: Vec<ObjectId>,
    p3_library: Vec<ObjectId>,
}

/// Whether P3's creature `p3_bear` is on the Riptide board.
#[derive(Clone, Copy)]
enum P3Bear {
    OnBattlefield,
    /// P3 controls no creature, so P3 leaving the game moves no creature off
    /// the battlefield.
    Absent,
}

impl RiptideBoard {
    fn p3_bear(&self) -> ObjectId {
        self.p3_bear
            .expect("fixture: this row's board has P3's bear on the battlefield")
    }
}

fn riptide_four_player_board() -> RiptideBoard {
    riptide_four_player_board_with(P3Bear::OnBattlefield)
}

fn riptide_four_player_board_with(p3_bear: P3Bear) -> RiptideBoard {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    scenario.with_library_top(P1, &["P1 Library 1", "P1 Library 2", "P1 Library 3"]);
    scenario.with_library_top(P3, &["P3 Library 1", "P3 Library 2", "P3 Library 3"]);
    let demon = scenario
        .add_creature_from_oracle(P1, "Extractor Demon", 5, 5, EXTRACTOR_DEMON_ORACLE)
        .id();
    let borrowed = scenario
        .add_creature(P1, "Borrowed Bear", 2, 2)
        .controlled_by(P2)
        .id();
    let p3_bear = match p3_bear {
        P3Bear::OnBattlefield => Some(scenario.add_creature(P3, "P3 Bear", 2, 2).id()),
        P3Bear::Absent => None,
    };
    let p3_trinket = scenario.add_artifact_from_oracle(P3, "P3 Trinket", "").id();
    let bauble = scenario
        .add_artifact_from_oracle(P0, "Lodestone Bauble", LODESTONE_BAUBLE_ORACLE)
        .id();
    let l1 = scenario.add_land_to_graveyard(P1, "Graveyard Forest").id();
    let l2 = scenario.add_land_to_graveyard(P1, "Graveyard Island").id();
    let riptide = scenario
        .add_creature_to_hand_from_oracle(P0, "Riptide Gearhulk", 2, 5, RIPTIDE_GEARHULK_ORACLE)
        .id();
    let mut runner = scenario.build();
    for land in [l1, l2] {
        let object = runner.state_mut().objects.get_mut(&land).unwrap();
        object.card_types.supertypes.push(Supertype::Basic);
        object.base_card_types = object.card_types.clone();
    }
    assert_eq!(
        runner.state().objects[&borrowed].controller,
        P2,
        "fixture: P2 controls the bear P1 owns"
    );
    let p1_library = library(&runner, P1);
    let p3_library = library(&runner, P3);
    assert_eq!((p1_library.len(), p3_library.len()), (3, 3));
    RiptideBoard {
        runner,
        riptide,
        demon,
        borrowed,
        p3_bear,
        p3_trinket,
        bauble,
        lands: [l1, l2],
        p1_library,
        p3_library,
    }
}

/// Cast Riptide targeting one permanent per opponent and stop at the first
/// owner's prompt: P1, the first owner in APNAP order (P0 owns no target).
fn open_riptide_arrangement(board: &mut RiptideBoard) {
    let p3_bear = board.p3_bear();
    let outcome = board
        .runner
        .cast(board.riptide)
        .target_players(&[P1, P2, P3])
        .target_objects(&[board.demon, board.borrowed, p3_bear])
        .resolve();
    assert_owner_arrangement_prompt(
        outcome.final_waiting_for(),
        P1,
        &[board.demon, board.borrowed],
        LibraryPosition::NthFromTop { n: 3 },
    );
    for id in [board.demon, board.borrowed, board.p3_bear()] {
        assert_eq!(zone(&board.runner, id), Zone::Battlefield);
    }
}

/// P1 submits `[borrowed, demon]`; P3 is prompted next.
fn submit_p1_order(board: &mut RiptideBoard) {
    board
        .runner
        .act(GameAction::SelectCards {
            cards: vec![board.borrowed, board.demon],
        })
        .expect("P1 submits their arrangement");
    assert_owner_arrangement_prompt(
        &board.runner.state().waiting_for,
        P3,
        &[board.p3_bear()],
        LibraryPosition::NthFromTop { n: 3 },
    );
}

/// P1's library after P1's two cards are delivered third from the top in
/// P1's order. CHARACTERIZATION of the prompt's `NthFromTop` mapping (#10216):
/// each submitted card is inserted at index `n - 1`, so submitting
/// `[borrowed, demon]` leaves `demon` third and `borrowed` fourth. The card
/// does not fix this mapping; CR 401.4 lets the owner choose any order.
fn p1_library_after_both(board: &RiptideBoard) -> Vec<ObjectId> {
    let f = &board.p1_library;
    vec![f[0], f[1], board.demon, board.borrowed, f[2]]
}

fn p3_library_after(board: &RiptideBoard) -> Vec<ObjectId> {
    let g = &board.p3_library;
    vec![g[0], g[1], board.p3_bear(), g[2]]
}

fn owner_arrangement_carrier_is_open(runner: &GameRunner) -> bool {
    runner.state().pending_mass_library_order_choice.is_some()
}

/// Put pending triggers on the stack: order them as listed and give each the
/// first legal target, until a player has priority.
fn drive_trigger_prompts(runner: &mut GameRunner) {
    for _ in 0..64 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => return,
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .choose_first_legal_target()
                    .expect("a trigger takes its first legal target");
            }
            other => panic!("unexpected prompt while putting triggers on the stack: {other:?}"),
        }
    }
    panic!("triggers were not put on the stack within the prompt budget");
}

/// Resolve the stack to an empty priority window, declining every optional
/// mill so the libraries stay as asserted.
fn resolve_to_priority(runner: &mut GameRunner) {
    for _ in 0..128 {
        let state = runner.state();
        let action = match &state.waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                GameAction::DecideOptionalEffect { accept: false }
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
                continue;
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .choose_first_legal_target()
                    .expect("a trigger takes its first legal target");
                continue;
            }
            WaitingFor::Priority { .. } if !state.stack.is_empty() => GameAction::PassPriority,
            WaitingFor::Priority { .. } => return,
            other => panic!("unexpected prompt while resolving to priority: {other:?}"),
        };
        runner.act(action).expect("resolve the stack");
    }
    panic!("the stack did not resolve within the prompt budget");
}

fn stack_entries_from(runner: &GameRunner, source: ObjectId) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == source)
        .count()
}

/// CR 101.4 + CR 401.4 + CR 603.10a + CR 603.2c: Riptide Gearhulk in four
/// players puts one permanent per opponent into its owner's library third from
/// the top. P1 owns two of them (the Demon it controls and a bear P2
/// controls); P3 owns the third. Each owner arranges their own cards in APNAP
/// order, then all three move as one simultaneous action. Extractor Demon,
/// leaving in that same event, sees both other creatures leave (CR 603.10a;
/// its ruling: it triggers when it and another creature leave at the same
/// time) and triggers once for each (CR 603.2c).
///
/// REVERT-FAILING: with per-owner delivery restored, (a) P1's two cards are
/// already in P1's library when P3 is prompted, and (b) the Demon is in the
/// library when P3's bear leaves, so it triggers once instead of twice.
#[test]
fn riptide_gearhulk_collects_every_owners_order_before_one_simultaneous_move() {
    let mut board = riptide_four_player_board();
    open_riptide_arrangement(&mut board);
    submit_p1_order(&mut board);

    // (a) CR 101.4: nothing moves before the last owner chooses.
    for id in [board.demon, board.borrowed, board.p3_bear()] {
        assert_eq!(
            zone(&board.runner, id),
            Zone::Battlefield,
            "CR 101.4: no target moves before P3 chooses"
        );
    }
    assert_eq!(library(&board.runner, P1), board.p1_library);

    // Routing: P3's prompt belongs to P3 alone.
    let p3_prompt = board.runner.state().waiting_for.clone();
    let p3_bear = board.p3_bear();
    assert!(
        engine::game::engine::apply(
            board.runner.state_mut(),
            P1,
            GameAction::SelectCards {
                cards: vec![p3_bear],
            },
        )
        .is_err(),
        "P1 cannot answer P3's arrangement"
    );
    assert_eq!(board.runner.state().waiting_for, p3_prompt);
    // AI: the generic candidates offer P3's only arrangement.
    assert!(
        legal_actions(board.runner.state())
            .iter()
            .any(|action| matches!(
                action,
                GameAction::SelectCards { cards } if cards == &vec![board.p3_bear()]
            )),
        "the next owner's arrangement is a published legal action"
    );

    board
        .runner
        .act(GameAction::SelectCards {
            cards: vec![board.p3_bear()],
        })
        .expect("P3 submits their arrangement");
    assert_eq!(library(&board.runner, P1), p1_library_after_both(&board));
    assert_eq!(library(&board.runner, P3), p3_library_after(&board));
    assert!(!owner_arrangement_carrier_is_open(&board.runner));

    // (b) Reach guard: at least one trigger shows the Demon's ability parses
    // and fires; exactly two show it saw both co-departing creatures.
    drive_trigger_prompts(&mut board.runner);
    assert_eq!(
        stack_entries_from(&board.runner, board.demon),
        2,
        "CR 603.10a + CR 603.2c: the departing Demon sees the other owner's creature leave"
    );

    resolve_to_priority(&mut board.runner);
    assert_eq!(zone(&board.runner, board.riptide), Zone::Battlefield);
    assert_eq!(
        board.runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    );
    assert_eq!(library(&board.runner, P1), p1_library_after_both(&board));
}

/// CR 104.3a + CR 800.4a + CR 101.4: P3, the last chooser, concedes at their
/// prompt. P3's bear leaves the game with them, so P3 has nothing to arrange,
/// and the arrangement P1 already chose happens: P1's two cards move. Priority
/// then goes to the active player (CR 117.3b).
///
/// REVERT-FAILING: without the departure reconciliation, P3's prompt is
/// re-pointed to priority, P1's cards stay on the battlefield, and the
/// collected order is stranded in the carrier.
#[test]
fn riptide_owner_departure_at_their_prompt_delivers_the_collected_arrangement() {
    let mut board = riptide_four_player_board();
    open_riptide_arrangement(&mut board);
    submit_p1_order(&mut board);
    // Reach guard: P1's order was collected, not delivered.
    assert_eq!(zone(&board.runner, board.demon), Zone::Battlefield);
    assert_eq!(zone(&board.runner, board.borrowed), Zone::Battlefield);

    board
        .runner
        .act(GameAction::Concede { player_id: P3 })
        .expect("P3 concedes");

    assert_eq!(library(&board.runner, P1), p1_library_after_both(&board));
    assert_eq!(
        zone(&board.runner, board.p3_bear()),
        Zone::Exile,
        "CR 800.4a: P3's bear leaves the game with P3"
    );
    assert!(!owner_arrangement_carrier_is_open(&board.runner));

    resolve_to_priority(&mut board.runner);
    assert!(board.runner.state().stack.is_empty());
    assert_eq!(
        board.runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    );
}

/// CR 104.3a + CR 800.4a + CR 603.10a + CR 603.3: the departure-settled move
/// still puts its triggers on the stack. The board has no `p3_bear`, so P3
/// owns no creature, and Riptide targets P3's noncreature `p3_trinket`: P3
/// leaving the game takes no creature off the battlefield, so it gives the
/// Demon (which watches creatures) nothing to trigger on (CR 603.6c). The
/// settled move then takes the Demon and `borrowed` together; the Demon
/// looks back (CR 603.10a; its 2017-03-14 ruling: it triggers when it and
/// another creature leave at the same time) and triggers exactly once, for
/// `borrowed`. A Concede returns before the
/// post-action pipeline, so that trigger reaches the stack only through the
/// settle's own completion and the elimination drain.
///
/// REVERT-FAILING: if the triggers of the departure-settled move are not put
/// on the stack, the Demon's stack count is 0, not 1.
#[test]
fn riptide_owner_departure_settled_move_puts_its_triggers_on_the_stack() {
    let mut board = riptide_four_player_board_with(P3Bear::Absent);
    let outcome = board
        .runner
        .cast(board.riptide)
        .target_players(&[P1, P2, P3])
        .target_objects(&[board.demon, board.borrowed, board.p3_trinket])
        .resolve();
    assert_owner_arrangement_prompt(
        outcome.final_waiting_for(),
        P1,
        &[board.demon, board.borrowed],
        LibraryPosition::NthFromTop { n: 3 },
    );
    board
        .runner
        .act(GameAction::SelectCards {
            cards: vec![board.borrowed, board.demon],
        })
        .expect("P1 submits their arrangement");
    assert_owner_arrangement_prompt(
        &board.runner.state().waiting_for,
        P3,
        &[board.p3_trinket],
        LibraryPosition::NthFromTop { n: 3 },
    );
    // Reach guard: P1's order was collected, not delivered.
    assert_eq!(zone(&board.runner, board.demon), Zone::Battlefield);
    assert_eq!(zone(&board.runner, board.borrowed), Zone::Battlefield);

    board
        .runner
        .act(GameAction::Concede { player_id: P3 })
        .expect("P3 concedes");
    assert_eq!(
        zone(&board.runner, board.p3_trinket),
        Zone::Exile,
        "CR 800.4a: P3's artifact leaves the game with P3"
    );
    assert_eq!(library(&board.runner, P1), p1_library_after_both(&board));
    assert!(!owner_arrangement_carrier_is_open(&board.runner));

    drive_trigger_prompts(&mut board.runner);
    assert_eq!(
        stack_entries_from(&board.runner, board.demon),
        1,
        "CR 603.10a + CR 603.3: the Demon's trigger for `borrowed` reaches the stack"
    );

    resolve_to_priority(&mut board.runner);
    assert!(board.runner.state().stack.is_empty());
    assert_eq!(
        board.runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    );
    assert_eq!(library(&board.runner, P1), p1_library_after_both(&board));
}

/// CR 104.3a + CR 800.4a: P3, an owner still queued, concedes while P1 is
/// choosing. P3's bear leaves the game, so P3's batch is dropped; P1's
/// submission is then the last choice and moves P1's cards.
///
/// REVERT-FAILING: without the departure reconciliation, P1's submission opens
/// a prompt for the departed P3 instead of completing.
#[test]
fn riptide_queued_owner_departure_drops_their_batch() {
    let mut board = riptide_four_player_board();
    open_riptide_arrangement(&mut board);

    board
        .runner
        .act(GameAction::Concede { player_id: P3 })
        .expect("P3 concedes");
    assert_owner_arrangement_prompt(
        &board.runner.state().waiting_for,
        P1,
        &[board.demon, board.borrowed],
        LibraryPosition::NthFromTop { n: 3 },
    );
    assert!(owner_arrangement_carrier_is_open(&board.runner));
    assert_eq!(zone(&board.runner, board.p3_bear()), Zone::Exile);

    board
        .runner
        .act(GameAction::SelectCards {
            cards: vec![board.borrowed, board.demon],
        })
        .expect("P1 submits their arrangement");
    assert_eq!(library(&board.runner, P1), p1_library_after_both(&board));
    assert!(
        !matches!(
            board.runner.state().waiting_for,
            WaitingFor::EffectZoneChoice { .. }
        ),
        "no prompt is opened for the departed P3: {:?}",
        board.runner.state().waiting_for
    );
    assert!(!owner_arrangement_carrier_is_open(&board.runner));

    resolve_to_priority(&mut board.runner);
    assert_eq!(
        board.runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    );
}

/// CR 104.3a + CR 800.4a + CR 400.7: P2 concedes while P1 is choosing. P2
/// controls P1's `borrowed` with no control effect, so CR 800.4a exiles it as
/// an object still controlled by the departed player. It is a new object, so
/// P1's prompt is republished with the Demon alone; then P3 chooses, and the
/// two remaining cards move together.
///
/// REVERT-FAILING: without the departure reconciliation, P1's prompt still
/// names the exiled bear and every submission is rejected.
#[test]
fn riptide_member_exiled_with_its_controller_is_dropped_from_the_owners_prompt() {
    let mut board = riptide_four_player_board();
    open_riptide_arrangement(&mut board);

    board
        .runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("P2 concedes");
    assert_eq!(
        zone(&board.runner, board.borrowed),
        Zone::Exile,
        "CR 800.4a: an object still controlled by the departed player is exiled"
    );
    assert_owner_arrangement_prompt(
        &board.runner.state().waiting_for,
        P1,
        &[board.demon],
        LibraryPosition::NthFromTop { n: 3 },
    );
    assert!(
        legal_actions(board.runner.state())
            .iter()
            .any(|action| matches!(
                action,
                GameAction::SelectCards { cards } if cards == &vec![board.demon]
            )),
        "the republished arrangement is a published legal action"
    );

    board
        .runner
        .act(GameAction::SelectCards {
            cards: vec![board.demon],
        })
        .expect("P1 submits the republished arrangement");
    assert_owner_arrangement_prompt(
        &board.runner.state().waiting_for,
        P3,
        &[board.p3_bear()],
        LibraryPosition::NthFromTop { n: 3 },
    );
    assert_eq!(zone(&board.runner, board.demon), Zone::Battlefield);
    board
        .runner
        .act(GameAction::SelectCards {
            cards: vec![board.p3_bear()],
        })
        .expect("P3 submits their arrangement");

    let f = &board.p1_library;
    assert_eq!(
        library(&board.runner, P1),
        vec![f[0], f[1], board.demon, f[2]]
    );
    assert_eq!(library(&board.runner, P3), p3_library_after(&board));
    assert!(!owner_arrangement_carrier_is_open(&board.runner));

    resolve_to_priority(&mut board.runner);
    assert_eq!(
        board.runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    );
}

/// CR 104.3a + CR 800.4a + CR 101.4: P1, the current chooser, concedes at
/// their prompt. P1 owned both cards in that prompt, so both leave the game
/// with P1 and P1 has nothing left to arrange. The next owner, P3, chooses
/// (and is the acting player), and P3's card moves.
///
/// REVERT-FAILING: without the departure reconciliation, P1's prompt is
/// re-pointed to priority and P3 is never prompted.
#[test]
fn riptide_current_owner_departure_prompts_the_next_owner() {
    let mut board = riptide_four_player_board();
    open_riptide_arrangement(&mut board);

    board
        .runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("P1 concedes");
    assert_eq!(
        zone(&board.runner, board.demon),
        Zone::Exile,
        "CR 800.4a: the Demon P1 owned leaves the game with P1"
    );
    assert_eq!(
        zone(&board.runner, board.borrowed),
        Zone::Exile,
        "CR 800.4a: the bear P1 owned leaves the game with P1"
    );
    assert_owner_arrangement_prompt(
        &board.runner.state().waiting_for,
        P3,
        &[board.p3_bear()],
        LibraryPosition::NthFromTop { n: 3 },
    );
    assert_eq!(board.runner.state().priority_player, P3);
    assert!(owner_arrangement_carrier_is_open(&board.runner));

    board
        .runner
        .act(GameAction::SelectCards {
            cards: vec![board.p3_bear()],
        })
        .expect("P3 submits their arrangement");
    assert_eq!(library(&board.runner, P3), p3_library_after(&board));
    assert!(!owner_arrangement_carrier_is_open(&board.runner));

    resolve_to_priority(&mut board.runner);
    assert_eq!(
        board.runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    );
}

/// Sibling of `riptide_current_owner_departure_prompts_the_next_owner`: after
/// P1 leaves, P3 leaves at P3's prompt too. CR 104.3a + CR 800.4a: every
/// remaining target left the game with its owner, so nobody has a card to
/// arrange. P0's Riptide trigger survives and has begun resolving, so it
/// continues fully (CR 608.2m): the instruction completes with nothing moved
/// (CR 608.2c + CR 609.3), the resolution ends, and only then does the active
/// player P0 receive priority (CR 117.2e + CR 117.3b), all within the Concede.
/// The single completion is the engine's completion contract.
///
/// REVERT-FAILING: with the departed final chooser's arrangement retired
/// instead of completed, the Concede emits no completion and the Riptide
/// carrier stays live behind re-pointed priority. `Priority { P0 }` alone does
/// not discriminate; it is the CR 117.3b check.
#[test]
fn riptide_every_remaining_owner_departure_completes_the_arrangement_empty() {
    let mut board = riptide_four_player_board();
    open_riptide_arrangement(&mut board);
    board
        .runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("P1 concedes");
    assert_owner_arrangement_prompt(
        &board.runner.state().waiting_for,
        P3,
        &[board.p3_bear()],
        LibraryPosition::NthFromTop { n: 3 },
    );

    let conceded = board
        .runner
        .act(GameAction::Concede { player_id: P3 })
        .expect("P3 concedes");
    assert_eq!(
        put_at_library_position_resolutions(&conceded.events, board.riptide),
        1,
        "the surviving instruction completes once, empty"
    );
    let state = board.runner.state();
    assert!(
        state.resolving_stack_entry.is_none(),
        "the Riptide carrier settled"
    );
    assert!(
        state.active_ability_continuation().is_none(),
        "no continuation is left behind the departure"
    );
    assert!(!owner_arrangement_carrier_is_open(&board.runner));
    assert!(state.stack.is_empty());
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P0 });
    assert_eq!(state.priority_player, P0);
}

/// Activate Lodestone Bauble on P1's two graveyard lands and arrange them as
/// `[l2, l1]` — P0, the activator, arranges (CR 101.1 + CR 608.2c) — asserting
/// P1's library is exactly those two cards on top of what it held before.
fn lodestone_arrangement_moves_exactly_its_own_cards(board: &mut RiptideBoard) {
    let [l1, l2] = board.lands;
    assert_eq!(zone(&board.runner, l1), Zone::Graveyard);
    assert_eq!(zone(&board.runner, l2), Zone::Graveyard);
    let p1_before_bauble = library(&board.runner, P1);
    let outcome = board
        .runner
        .activate(board.bauble, 0)
        .target_objects(&[l1, l2])
        .resolve();
    assert_controller_arrangement_prompt(
        outcome.final_waiting_for(),
        P0,
        P1,
        &[l1, l2],
        LibraryPosition::Top,
    );
    board
        .runner
        .act(GameAction::SelectCards {
            cards: vec![l2, l1],
        })
        .expect("P0 submits their arrangement");

    let mut expected = vec![l2, l1];
    expected.extend(p1_before_bauble);
    assert_eq!(
        library(&board.runner, P1),
        expected,
        "the later arrangement moves exactly its own cards"
    );
    assert!(!owner_arrangement_carrier_is_open(&board.runner));
    assert!(board.runner.state().stack.is_empty());
    assert_eq!(
        board.runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    );
}

/// CR 104.3a + CR 800.4a + CR 608.2c: after a departure settled one
/// arrangement, a later single-owner arrangement (Lodestone Bauble) moves
/// exactly its own two cards; nothing from the earlier arrangement rides along.
///
/// Paired positive: `lodestone_bauble_after_a_completed_owner_arrangement_moves_exactly_its_own_cards`
/// reaches the same relation without a departure.
#[test]
fn lodestone_bauble_after_an_owner_departure_moves_exactly_its_own_cards() {
    let mut board = riptide_four_player_board();
    open_riptide_arrangement(&mut board);
    submit_p1_order(&mut board);
    board
        .runner
        .act(GameAction::Concede { player_id: P3 })
        .expect("P3 concedes");
    assert_eq!(library(&board.runner, P1), p1_library_after_both(&board));
    resolve_to_priority(&mut board.runner);

    lodestone_arrangement_moves_exactly_its_own_cards(&mut board);
}

/// Paired positive for `lodestone_bauble_after_an_owner_departure_moves_exactly_its_own_cards`:
/// the same later arrangement after the Riptide arrangement completed with
/// every owner still in the game.
#[test]
fn lodestone_bauble_after_a_completed_owner_arrangement_moves_exactly_its_own_cards() {
    let mut board = riptide_four_player_board();
    open_riptide_arrangement(&mut board);
    submit_p1_order(&mut board);
    board
        .runner
        .act(GameAction::SelectCards {
            cards: vec![board.p3_bear()],
        })
        .expect("P3 submits their arrangement");
    resolve_to_priority(&mut board.runner);
    assert_eq!(library(&board.runner, P1), p1_library_after_both(&board));

    lodestone_arrangement_moves_exactly_its_own_cards(&mut board);
}

/// Pass every priority window and declare nothing until the next turn's
/// upkeep has begun and its stack (the delayed trigger) has resolved.
fn advance_through_next_upkeep(runner: &mut GameRunner) {
    let turn = runner.state().turn_number;
    for _ in 0..128 {
        let in_next_upkeep =
            runner.state().turn_number > turn && runner.state().phase == Phase::Upkeep;
        if in_next_upkeep
            && runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            return;
        }
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            other => panic!("unexpected window while advancing to the next upkeep: {other:?}"),
        };
        runner.act(action).expect("advance to the next upkeep");
    }
    panic!("the next upkeep was not reached within the window budget");
}

/// CR 101.1 + CR 608.2c + CR 608.2d + CR 603.7: Lodestone Bauble's targets come
/// from P1's graveyard, and its "… on top of their library in any order" is
/// part of the activator's instruction, so P0 arranges them on top of P1's
/// library (the card's text takes precedence over the CR 401.4 owner default).
/// The card's delayed "That player draws a card at the beginning of the next
/// turn's upkeep" is created by the part of the ability parked behind that
/// prompt, and it must still draw for P1 — exactly the card P0 put on top.
///
/// REVERT-FAILING: with the arranger dropped, the prompt names P1, the owner;
/// with the arrangement gate removed, the activation resolves straight to
/// priority. Either way the unconditional prompt assertion fails.
#[test]
fn lodestone_bauble_controller_arranges_and_the_delayed_draw_takes_the_card_they_put_on_top() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, colorless(1));
    scenario.with_library_top(P0, &["P0 Library 1", "P0 Library 2"]);
    scenario.with_library_top(P1, &["P1 Library 1", "P1 Library 2"]);
    let bauble = scenario
        .add_artifact_from_oracle(P0, "Lodestone Bauble", LODESTONE_BAUBLE_ORACLE)
        .id();
    let l1 = scenario.add_land_to_graveyard(P1, "Graveyard Forest").id();
    let l2 = scenario.add_land_to_graveyard(P1, "Graveyard Island").id();
    let mut runner = scenario.build();
    for land in [l1, l2] {
        let object = runner.state_mut().objects.get_mut(&land).unwrap();
        object.card_types.supertypes.push(Supertype::Basic);
        object.base_card_types = object.card_types.clone();
    }
    grant_priority(&mut runner, P0);
    let p1_before = library(&runner, P1);
    let p0_hand_before = runner.state().players[P0.0 as usize].hand.len();

    let outcome = runner
        .activate(bauble, 0)
        .target_objects(&[l1, l2])
        .resolve();

    // REVERT-FAILING: P0, the activator, is prompted over P1's cards.
    assert_controller_arrangement_prompt(
        outcome.final_waiting_for(),
        P0,
        P1,
        &[l1, l2],
        LibraryPosition::Top,
    );
    assert_eq!(zone(&runner, l1), Zone::Graveyard);
    assert_eq!(zone(&runner, l2), Zone::Graveyard);

    runner
        .act(GameAction::SelectCards {
            cards: vec![l2, l1],
        })
        .expect("P0 submits their arrangement");

    let mut expected = vec![l2, l1];
    expected.extend(p1_before.iter().copied());
    assert_eq!(
        library(&runner, P1),
        expected,
        "P0's chosen order is the exact top of P1's library"
    );
    assert_eq!(
        zone(&runner, bauble),
        Zone::Graveyard,
        "reach guard: the Bauble's sacrifice cost was paid"
    );
    assert!(runner.state().stack.is_empty(), "the ability resolved");
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    );
    assert!(
        runner.state().players[P1.0 as usize].hand.is_empty(),
        "fixture: P1 starts with an empty hand"
    );

    advance_through_next_upkeep(&mut runner);

    assert_eq!(runner.state().active_player, P1, "the next turn is P1's");
    assert_eq!(
        runner.state().players[P1.0 as usize]
            .hand
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![l2],
        "P1 drew exactly the card P0 put on top"
    );
    let mut remaining = vec![l1];
    remaining.extend(p1_before);
    assert_eq!(library(&runner, P1), remaining);
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        p0_hand_before,
        "P0 drew nothing"
    );
}

// ---------------------------------------------------------------------------
// An owner arrangement whose last chooser leaves the game completes empty, and
// the resolution under way continues (CR 800.4a + CR 608.2m).
// ---------------------------------------------------------------------------

/// The `EffectResolved { PutAtLibraryPosition }` completions `source` emitted.
fn put_at_library_position_resolutions(events: &[GameEvent], source: ObjectId) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::PutAtLibraryPosition,
                    source_id,
                    ..
                } if *source_id == source
            )
        })
        .count()
}

/// How many times `spell` was reported resolved.
fn stack_resolved_count(events: &[GameEvent], spell: ObjectId) -> usize {
    events
        .iter()
        .filter(
            |event| matches!(event, GameEvent::StackResolved { object_id } if *object_id == spell),
        )
        .count()
}

/// `(controller, one_shot)` of every delayed trigger `source` created.
fn delayed_triggers_from(runner: &GameRunner, source: ObjectId) -> Vec<(PlayerId, bool)> {
    runner
        .state()
        .delayed_triggers
        .iter()
        .filter(|trigger| trigger.source_id == source)
        .map(|trigger| (trigger.controller, trigger.one_shot))
        .collect()
}

fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

/// The three-player Lodestone Bauble board. P0 is active, controls Lodestone
/// Bauble and has the {1} to activate it; `graveyard_owner` has two basic land
/// cards in their graveyard. Every player's library holds two cards and every
/// hand is empty.
///
/// Returns `(runner, bauble, [l1, l2])`.
fn lodestone_three_player_board(
    graveyard_owner: PlayerId,
) -> (GameRunner, ObjectId, [ObjectId; 2]) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, colorless(1));
    scenario.with_library_top(P0, &["P0 Library 1", "P0 Library 2"]);
    scenario.with_library_top(P1, &["P1 Library 1", "P1 Library 2"]);
    scenario.with_library_top(P2, &["P2 Library 1", "P2 Library 2"]);
    let bauble = scenario
        .add_artifact_from_oracle(P0, "Lodestone Bauble", LODESTONE_BAUBLE_ORACLE)
        .id();
    let l1 = scenario
        .add_land_to_graveyard(graveyard_owner, "Graveyard Forest")
        .id();
    let l2 = scenario
        .add_land_to_graveyard(graveyard_owner, "Graveyard Island")
        .id();
    let mut runner = scenario.build();
    for land in [l1, l2] {
        let object = runner.state_mut().objects.get_mut(&land).unwrap();
        object.card_types.supertypes.push(Supertype::Basic);
        object.base_card_types = object.card_types.clone();
    }
    grant_priority(&mut runner, P0);
    (runner, bauble, [l1, l2])
}

/// Activate the Bauble on `[l1, l2]`, owned by `owner`, and stop at P0's
/// arrangement prompt over them (CR 101.1 + CR 608.2c: the activator follows
/// "… in any order"), with the reach guard every departure row below relies
/// on: the cost was paid, nothing moved, the activation is still resolving
/// with its "That player draws a card …" tail parked behind the prompt, and
/// that tail has created no delayed trigger yet.
fn open_lodestone_arrangement(
    runner: &mut GameRunner,
    bauble: ObjectId,
    lands: [ObjectId; 2],
    owner: PlayerId,
) {
    let [l1, l2] = lands;
    let outcome = runner
        .activate(bauble, 0)
        .target_objects(&[l1, l2])
        .resolve();
    assert_controller_arrangement_prompt(
        outcome.final_waiting_for(),
        P0,
        owner,
        &[l1, l2],
        LibraryPosition::Top,
    );
    assert_eq!(zone(runner, l1), Zone::Graveyard);
    assert_eq!(zone(runner, l2), Zone::Graveyard);
    assert_eq!(
        zone(runner, bauble),
        Zone::Graveyard,
        "reach guard: the Bauble's sacrifice cost was paid"
    );
    let state = runner.state();
    assert_eq!(
        state
            .resolving_stack_entry
            .as_ref()
            .map(|entry| entry.source_id),
        Some(bauble),
        "reach guard: the activation is still resolving"
    );
    assert!(
        state.active_ability_continuation().is_some(),
        "reach guard: the delayed-draw tail is parked behind the prompt"
    );
    assert!(
        delayed_triggers_from(runner, bauble).is_empty(),
        "reach guard: the tail has not run"
    );
    assert!(!owner_arrangement_carrier_is_open(runner));
}

/// Pass every priority window and declare nothing until the first priority
/// window of the next turn (past its untap step) with an empty stack. Returns
/// the most stack entries from `source` seen at once during that next turn.
fn advance_into_next_turn_observing(runner: &mut GameRunner, source: ObjectId) -> usize {
    let start_turn = runner.state().turn_number;
    let mut seen = 0;
    for _ in 0..128 {
        let state = runner.state();
        if state.turn_number > start_turn {
            seen = seen.max(stack_entries_from(runner, source));
            if state.phase != Phase::Untap
                && state.stack.is_empty()
                && matches!(state.waiting_for, WaitingFor::Priority { .. })
            {
                return seen;
            }
        }
        let action = match &state.waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            other => panic!("unexpected window while advancing to the next turn: {other:?}"),
        };
        runner.act(action).expect("advance to the next turn");
    }
    panic!("the next turn was not reached within the window budget");
}

/// CR 104.3a + CR 800.4a + CR 608.2c + CR 608.2m: P0 activates Lodestone
/// Bauble on two basic lands in P1's graveyard; P2 stays in the game. P0
/// arranges them (CR 101.1 + CR 608.2c: the activator follows "… in any
/// order"), and P1, the owner, concedes at P0's arrangement prompt. P1's lands
/// leave the game with P1, so P0 has nothing left to arrange, but P0's
/// activation has begun resolving, so it continues fully: the placement
/// completes with nothing moved (CR 609.3), and the parked tail creates the
/// delayed draw, controlled by P0, the player who controlled the ability as it
/// resolved, with the Bauble as its source (CR 603.7a + CR 603.7e). Only then
/// does the active player P0 receive priority (CR 117.2e + CR 117.3b), all
/// within the Concede. The single completion is the engine's completion
/// contract.
///
/// REVERT-FAILING: with the arranger dropped, the pre-Concede prompt is P1's.
/// This row no longer discriminates the departed-final-chooser completion
/// (P0, the chooser, stays); `lodestone_bauble_controller_concession_at_own_arrangement_completes_before_priority`,
/// `plow_under_last_owner_concession_completes_the_spell_before_priority` and
/// the Riptide departure rows carry that.
///
/// SUPPLEMENTARY (V2, not the discriminator): at the next upkeep, P2's turn
/// (CR 800.4k), the living P0's trigger reaches the stack once and draws for
/// no one, because "that player" is the departed P1 (CR 603.7b + CR 800.4a +
/// CR 609.3).
///
/// Paired positive: `lodestone_bauble_controller_arranges_and_the_delayed_draw_takes_the_card_they_put_on_top`.
#[test]
fn lodestone_bauble_owner_concession_at_the_controllers_arrangement_completes_the_activation_before_priority(
) {
    let (mut runner, bauble, lands) = lodestone_three_player_board(P1);
    let [l1, l2] = lands;
    let p0_hand_before = hand_len(&runner, P0);
    let p2_hand_before = hand_len(&runner, P2);
    open_lodestone_arrangement(&mut runner, bauble, lands, P1);

    let conceded = runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("P1 concedes");

    // (a) CR 800.4a: P1's lands leave the game with P1.
    assert_eq!(zone(&runner, l1), Zone::Exile);
    assert_eq!(zone(&runner, l2), Zone::Exile);
    // (b) The surviving instruction completes once, with nothing moved.
    assert_eq!(
        put_at_library_position_resolutions(&conceded.events, bauble),
        1,
        "the placement completes once, empty, inside the Concede"
    );
    // (c) CR 603.7a + CR 603.7e: the tail ran and created P0's delayed draw.
    assert_eq!(
        delayed_triggers_from(&runner, bauble),
        vec![(P0, true)],
        "the parked tail created the delayed draw under P0"
    );
    // (d) The resolution is over.
    let state = runner.state();
    assert!(state.resolving_stack_entry.is_none(), "the carrier settled");
    assert!(
        state.active_ability_continuation().is_none(),
        "the continuation drained"
    );
    assert!(state.pending_mass_library_order_choice.is_none());
    assert!(state.stack.is_empty());
    // (e) CR 117.3b: the active player receives priority.
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P0 });
    assert_eq!(state.priority_player, P0);

    // SUPPLEMENTARY (V2).
    let seen = advance_into_next_turn_observing(&mut runner, bauble);
    assert_eq!(
        seen, 1,
        "CR 603.7b: the living P0's delayed trigger reaches the stack once"
    );
    assert_eq!(
        runner.state().active_player,
        P2,
        "CR 800.4k: the next turn is P2's"
    );
    assert!(
        delayed_triggers_from(&runner, bauble).is_empty(),
        "the one-shot delayed trigger is spent"
    );
    assert_eq!(
        hand_len(&runner, P0),
        p0_hand_before,
        "CR 609.3: P0 drew nothing"
    );
    assert_eq!(
        hand_len(&runner, P2),
        p2_hand_before,
        "CR 609.3: P2 drew nothing"
    );
}

/// UNCHANGED BRANCH (preservation): P2, a bystander who owns none of the
/// cards, concedes while P0 is arranging P1's cards. P0's prompt stands
/// untouched and nothing completes; P0's later submission then completes the
/// activation normally (CR 104.3a + CR 800.4a + CR 101.1 + CR 608.2c).
#[test]
fn lodestone_bauble_bystander_concession_leaves_the_controllers_arrangement_open() {
    let (mut runner, bauble, lands) = lodestone_three_player_board(P1);
    let [l1, l2] = lands;
    let p1_before = library(&runner, P1);
    open_lodestone_arrangement(&mut runner, bauble, lands, P1);

    let conceded = runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("P2 concedes");

    assert_controller_arrangement_prompt(
        &runner.state().waiting_for,
        P0,
        P1,
        &[l1, l2],
        LibraryPosition::Top,
    );
    assert_eq!(zone(&runner, l1), Zone::Graveyard);
    assert_eq!(zone(&runner, l2), Zone::Graveyard);
    assert!(delayed_triggers_from(&runner, bauble).is_empty());
    assert!(runner.state().resolving_stack_entry.is_some());
    assert_eq!(
        put_at_library_position_resolutions(&conceded.events, bauble),
        0,
        "a bystander's departure completes nothing"
    );

    runner
        .act(GameAction::SelectCards {
            cards: vec![l2, l1],
        })
        .expect("P0 submits their arrangement");
    let mut expected = vec![l2, l1];
    expected.extend(p1_before);
    assert_eq!(library(&runner, P1), expected);
    assert_eq!(delayed_triggers_from(&runner, bauble), vec![(P0, true)]);
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    );
}

/// CR 104.3a + CR 800.4a + CR 608.2m: P0 activates Lodestone Bauble on two
/// basic lands in P0's own graveyard, then concedes at P0's own arrangement
/// prompt; P1 and P2 stay in the game. P0's lands and the Bauble leave the
/// game, and P0's ability ceases to exist, but it has begun resolving, so it
/// continues fully: the placement completes with nothing moved (CR 609.3) and
/// the tail creates the delayed draw, controlled by P0 (CR 603.7a +
/// CR 603.7e) — creating a delayed triggered ability is not putting it on the
/// stack. The resolution ends, and because the active player P0 has left, P1
/// receives priority (CR 117.2e + CR 117.3b + CR 800.4j), all within the
/// Concede. Nothing downstream of the Concede is asserted. The single
/// completion is the engine's completion contract.
///
/// REVERT-FAILING: with the departed final chooser's arrangement retired
/// instead of completed, (b), (c) and (d) fail. (e) is the CR 800.4j check
/// and is not the discriminator.
#[test]
fn lodestone_bauble_controller_concession_at_own_arrangement_completes_before_priority() {
    let (mut runner, bauble, lands) = lodestone_three_player_board(P0);
    let [l1, l2] = lands;
    open_lodestone_arrangement(&mut runner, bauble, lands, P0);

    let conceded = runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("P0 concedes");

    // (a) CR 800.4a.
    assert!(runner.state().players[P0.0 as usize].is_eliminated);
    for id in [l1, l2, bauble] {
        assert_eq!(
            zone(&runner, id),
            Zone::Exile,
            "CR 800.4a: P0's cards leave the game with P0"
        );
    }
    // (b)
    assert_eq!(
        put_at_library_position_resolutions(&conceded.events, bauble),
        1,
        "the placement completes once, empty, inside the Concede"
    );
    // (c) CR 603.7e.
    assert_eq!(
        delayed_triggers_from(&runner, bauble),
        vec![(P0, true)],
        "the parked tail created the delayed draw under P0"
    );
    // (d)
    let state = runner.state();
    assert!(state.resolving_stack_entry.is_none(), "the carrier settled");
    assert!(
        state.active_ability_continuation().is_none(),
        "the continuation drained"
    );
    assert!(state.pending_mass_library_order_choice.is_none());
    assert!(state.stack.is_empty());
    // (e) CR 117.3b + CR 800.4j.
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P1 });
    assert_eq!(state.priority_player, P1);
}

/// CR 800.4a + CR 800.4g + CR 800.4h + CR 608.2m, ability side: P0 activates
/// Lodestone Bauble on two basic lands in P2's graveyard, then concedes at
/// P0's arrangement prompt. The lands stay (P2 is still in the game), and the
/// activation keeps resolving (CR 608.2m), so its instruction requires the
/// arrangement of a player who has left: the controller of the object would
/// choose another player to make it (CR 800.4g), and since that controller has
/// left too, the next player in turn order after them, P1, elects (CR 800.4h).
/// P1 may elect themself — "another player" is another than the one who left —
/// and then arranges; the owner P2 cannot submit. P1's submission completes the
/// parked activation, its delayed-draw tail drained, before priority
/// (CR 117.3b + CR 800.4j). The delayed draw's controller is left to the
/// CR 800.4d follow-up and is not asserted.
///
/// REVERT-FAILING: without the election, the Concede publishes P1's ordering
/// prompt directly, so the election assertion after the Concede fails.
#[test]
fn lodestone_bauble_controller_concession_lets_the_elector_choose_themself() {
    let (mut runner, bauble, lands) = lodestone_three_player_board(P2);
    let [l1, l2] = lands;
    let p2_before = library(&runner, P2);
    open_lodestone_arrangement(&mut runner, bauble, lands, P2);

    let conceded = runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("P0 concedes");

    // REVERT-FAILING: CR 800.4g + CR 800.4h — P1 elects the arranger.
    assert!(runner.state().players[P0.0 as usize].is_eliminated);
    assert_arranger_election(&runner.state().waiting_for, P1, &[P1, P2], bauble);
    assert_eq!(runner.state().priority_player, P1);
    assert_eq!(zone(&runner, l1), Zone::Graveyard);
    assert_eq!(zone(&runner, l2), Zone::Graveyard);
    assert_eq!(
        put_at_library_position_resolutions(&conceded.events, bauble),
        0,
        "nothing completes while the arranger is elected"
    );

    // CR 800.4g: the elector elects themself.
    runner
        .act(GameAction::ChooseZoneOpponentChooser { opponent: P1 })
        .expect("P1 elects themself");
    assert_controller_arrangement_prompt(
        &runner.state().waiting_for,
        P1,
        P2,
        &[l1, l2],
        LibraryPosition::Top,
    );

    // The owner cannot submit the arrangement P1 was elected to make.
    let before = runner.state().clone();
    let rejection = apply_with_rejection(
        runner.state_mut(),
        P2,
        GameAction::SelectCards {
            cards: vec![l2, l1],
        },
    )
    .expect_err("P2 must not submit P1's arrangement");
    assert_eq!(rejection.code, ActionRejectionCode::WrongPlayer);
    assert!(
        *runner.state() == before,
        "a rejected submission leaves the game unchanged"
    );

    let submitted = runner
        .act(GameAction::SelectCards {
            cards: vec![l2, l1],
        })
        .expect("P1 submits the arrangement");
    let mut expected = vec![l2, l1];
    expected.extend(p2_before);
    assert_eq!(library(&runner, P2), expected);
    assert_eq!(
        put_at_library_position_resolutions(&submitted.events, bauble),
        1,
        "the placement completes once"
    );
    let state = runner.state();
    assert!(state.resolving_stack_entry.is_none(), "the carrier settled");
    assert!(
        state.active_ability_continuation().is_none(),
        "the continuation drained"
    );
    assert!(state.pending_mass_library_order_choice.is_none());
    assert!(state.stack.is_empty());
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P1 });
}

/// The three-player Gravepurge board. P0 is active with Gravepurge (an
/// instant) in hand and {B}{B}{B}; P0's graveyard holds two creature cards and
/// P0's library one card. P1 and P2 each have a one-card library; every hand
/// but P0's Gravepurge is empty.
///
/// Returns `(runner, gravepurge, [c1, c2])`.
fn gravepurge_three_player_board() -> (GameRunner, ObjectId, [ObjectId; 2]) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, black_mana(3));
    let c1 = scenario
        .add_creature_to_graveyard(P0, "Graveyard Bear A", 2, 2)
        .id();
    let c2 = scenario
        .add_creature_to_graveyard(P0, "Graveyard Bear B", 2, 2)
        .id();
    scenario.add_card_to_library_top(P0, "Library Pre-existing");
    scenario.with_library_top(P1, &["P1 Library 1"]);
    scenario.with_library_top(P2, &["P2 Library 1"]);
    let gravepurge = scenario
        .add_spell_to_hand_from_oracle(P0, "Gravepurge", true, GRAVEPURGE_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Black],
        })
        .id();
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    (runner, gravepurge, [c1, c2])
}

/// CR 104.3a + CR 800.4a + CR 608.2m + CR 608.2n: P0 casts Gravepurge on two
/// creature cards in P0's own graveyard on a three-player board, then concedes
/// at P0's own arrangement prompt. P0's cards leave the game, the two targets
/// and Gravepurge itself included, but the spell has begun resolving, so it
/// continues fully: "Put … on top of your library" completes with nothing
/// moved and "Draw a card" is followed for the departed P0, so no living
/// player draws (CR 609.3). The resolution has finished; its final part
/// cannot put the spell into a graveyard because it is already in exile, and
/// under the engine's contract for a departed spell that continues to resolve
/// fully it is reported resolved exactly once (an engine reporting contract,
/// not a card fact). The active player P0 has left, so P1 receives priority
/// (CR 117.2e + CR 117.3b + CR 800.4j), all before the Concede returns.
///
/// REVERT-FAILING, two reverts:
/// - with the departed final chooser's arrangement retired instead of
///   completed, (b), (c) and (d) fail: the Draw is stranded behind a live
///   carrier;
/// - with the completion kept but the paused-spell block's settled-arrangement
///   conjunct removed, (c) fails: the block retires the carrier as
///   `Eliminated` and the spell is never reported resolved.
#[test]
fn gravepurge_controller_concession_at_own_arrangement_finishes_the_spell_before_priority() {
    let (mut runner, gravepurge, [c1, c2]) = gravepurge_three_player_board();
    let p1_hand_before = hand_len(&runner, P1);
    let p2_hand_before = hand_len(&runner, P2);

    let outcome = runner.cast(gravepurge).target_objects(&[c1, c2]).resolve();
    assert_owner_arrangement_prompt(
        outcome.final_waiting_for(),
        P0,
        &[c1, c2],
        LibraryPosition::Top,
    );
    // Reach guard: the spell is paused mid-resolution with its Draw parked.
    assert!(
        runner.state().players[P0.0 as usize].hand.is_empty(),
        "reach guard: the chained Draw waits for the arrangement"
    );
    assert_eq!(zone(&runner, c1), Zone::Graveyard);
    assert_eq!(zone(&runner, c2), Zone::Graveyard);
    assert_eq!(zone(&runner, gravepurge), Zone::Stack);
    assert_eq!(
        runner
            .state()
            .owed_spell_final_part()
            .map(|owed| owed.spell),
        Some(gravepurge),
        "reach guard: the spell owes its final part"
    );
    assert!(runner.state().active_ability_continuation().is_some());

    let conceded = runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("P0 concedes");

    // (a) CR 800.4a.
    assert!(runner.state().players[P0.0 as usize].is_eliminated);
    for id in [c1, c2, gravepurge] {
        assert_eq!(
            zone(&runner, id),
            Zone::Exile,
            "CR 800.4a: P0's cards leave the game with P0"
        );
    }
    // (b)
    assert_eq!(
        put_at_library_position_resolutions(&conceded.events, gravepurge),
        1,
        "the placement completes once, empty, inside the Concede"
    );
    // (c) CR 608.2m: the resolution finished and is reported once.
    assert_eq!(
        stack_resolved_count(&conceded.events, gravepurge),
        1,
        "the departed spell that continued to resolve fully is reported resolved once"
    );
    // (d)
    let state = runner.state();
    assert!(state.resolving_stack_entry.is_none(), "the carrier settled");
    assert!(
        state.owed_spell_final_part().is_none(),
        "the final part ran"
    );
    assert!(
        state.active_ability_continuation().is_none(),
        "the continuation drained"
    );
    assert!(state.pending_mass_library_order_choice.is_none());
    assert!(state.stack.is_empty());
    assert!(
        state
            .objects
            .values()
            .all(|object| object.zone != Zone::Stack),
        "no object is left on the stack"
    );
    // (e) CR 117.3b + CR 800.4j.
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P1 });
    assert_eq!(state.priority_player, P1);
    // SUPPLEMENTARY: no living player draws (CR 609.3), paired with (b) and (c).
    assert_eq!(hand_len(&runner, P1), p1_hand_before);
    assert_eq!(hand_len(&runner, P2), p2_hand_before);
}

const PLOW_UNDER_ORACLE: &str = "Put two target lands on top of their owners' libraries.";

/// N3 — CR 104.3a + CR 800.4a + CR 608.2m + CR 608.2n + CR 117.3b: P0 casts
/// Plow Under on two lands P1 owns, on a three-player board. Plow Under names
/// no arranger, so P1, the owner, arranges them (CR 401.4) and is the last and
/// only chooser. P1 concedes at P1's own prompt while P2 stays in the game.
/// P1's lands leave the game with P1, but the spell has begun resolving, so it
/// continues fully: the placement completes with nothing moved (CR 609.3), the
/// spell's final part puts it into P0's graveyard, and the active player P0
/// receives priority (CR 117.2e + CR 117.3b), all within the Concede.
///
/// The owner-actor mirror of the controller-actor Misinformation row, and the
/// Plow Under analogue of
/// `hallowed_burial_owner_whose_cards_leave_with_their_controller_completes_empty`.
/// REVERT-FAILING: with the departed final chooser's arrangement retired
/// instead of completed, no completion is emitted and the spell stays on the
/// stack behind a live resolution.
#[test]
fn plow_under_last_owner_concession_completes_the_spell_before_priority() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P1, &["P1 Library 1", "P1 Library 2"]);
    let plow_under = scenario
        .add_spell_to_hand_from_oracle(P0, "Plow Under", false, PLOW_UNDER_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let land_a = scenario
        .add_land_from_oracle(P1, "Opposing Forest", "")
        .id();
    let land_b = scenario
        .add_land_from_oracle(P1, "Opposing Island", "")
        .id();
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);

    let outcome = runner
        .cast(plow_under)
        .target_objects(&[land_a, land_b])
        .resolve();
    // Reach guard: the owner's prompt is open and the spell is mid-resolution.
    assert_owner_arrangement_prompt(
        outcome.final_waiting_for(),
        P1,
        &[land_a, land_b],
        LibraryPosition::Top,
    );
    assert_eq!(zone(&runner, plow_under), Zone::Stack);
    assert_eq!(zone(&runner, land_a), Zone::Battlefield);
    assert_eq!(zone(&runner, land_b), Zone::Battlefield);

    let conceded = runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("P1 concedes");

    // CR 800.4a: P1's lands leave the game with P1.
    assert_eq!(zone(&runner, land_a), Zone::Exile);
    assert_eq!(zone(&runner, land_b), Zone::Exile);
    assert_eq!(
        put_at_library_position_resolutions(&conceded.events, plow_under),
        1,
        "the placement completes once, empty, inside the Concede"
    );
    assert_eq!(
        zone(&runner, plow_under),
        Zone::Graveyard,
        "the spell's final part put it into P0's graveyard"
    );
    let state = runner.state();
    assert!(state.stack.is_empty());
    assert!(
        state.resolving_stack_entry.is_none(),
        "the resolution is over"
    );
    assert!(state.pending_mass_library_order_choice.is_none());
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P0 });
    assert_eq!(state.priority_player, P0);
}

/// The `players`-player Misinformation board. P0 is active with Misinformation
/// in hand and {B}; `owner` has three creature cards in their graveyard and a
/// one-card library, so the placement order is observable above it.
///
/// Returns `(runner, misinformation, [o1, o2, o3], owner_library_card)`.
fn misinformation_board(
    players: u8,
    owner: PlayerId,
) -> (GameRunner, ObjectId, [ObjectId; 3], ObjectId) {
    let (scenario, misinformation, cards, owner_library_card) =
        misinformation_scenario(players, owner);
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    (runner, misinformation, cards, owner_library_card)
}

/// The scenario `misinformation_board` builds, before it is built, so a row
/// can add cards to it.
///
/// Returns `(scenario, misinformation, [o1, o2, o3], owner_library_card)`.
fn misinformation_scenario(
    players: u8,
    owner: PlayerId,
) -> (GameScenario, ObjectId, [ObjectId; 3], ObjectId) {
    let mut scenario = GameScenario::new_n_player(players, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, black_mana(1));
    let (cards, owner_library_card) = stage_misinformation_targets(&mut scenario, owner);
    let misinformation = scenario
        .add_spell_to_hand_from_oracle(P0, "Misinformation", false, MISINFORMATION_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Black],
        })
        .id();
    (scenario, misinformation, cards, owner_library_card)
}

/// Misinformation's targets: three creature cards in `owner`'s graveyard and a
/// one-card library, so the placement order is observable above it.
///
/// Returns `([o1, o2, o3], owner_library_card)`.
fn stage_misinformation_targets(
    scenario: &mut GameScenario,
    owner: PlayerId,
) -> ([ObjectId; 3], ObjectId) {
    let mut cards = Vec::new();
    for name in ["Graveyard Card A", "Graveyard Card B", "Graveyard Card C"] {
        cards.push(scenario.add_creature_to_graveyard(owner, name, 2, 2).id());
    }
    let owner_library_card = scenario.add_card_to_library_top(owner, "Owner Library Card");
    ([cards[0], cards[1], cards[2]], owner_library_card)
}

/// The four-player board where P0 casts a Misinformation P2 owns from P2's
/// exile, under an impulse permission granted to P0 (the Gonti, Lord of Luxury
/// / Dire Fleet Daredevil class: "you may cast" another player's card). P0 is
/// active with {B}; P3 has three creature cards in their graveyard and a
/// one-card library.
///
/// Returns `(runner, misinformation, [o1, o2, o3], p3_library_card)`.
fn misinformation_from_owners_exile_board() -> (GameRunner, ObjectId, [ObjectId; 3], ObjectId) {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, black_mana(1));
    let (cards, p3_library_card) = stage_misinformation_targets(&mut scenario, P3);
    let misinformation = {
        let mut builder = scenario.add_spell_to_exile(P2, "Misinformation", true);
        builder.from_oracle_text(MISINFORMATION_ORACLE);
        builder.with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Black],
        });
        builder.id()
    };
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&misinformation)
        .unwrap()
        .casting_permissions
        .push(CastingPermission::PlayFromExile {
            provenance: PlayFromExileProvenance::Impulse,
            duration: Duration::UntilEndOfTurn,
            granted_to: P0,
            mode: CardPlayMode::Play,
            frequency: CastFrequency::Unlimited,
            source_id: None,
            invalidation: None,
            exiled_by_ability_controller: None,
            mana_spend_permission: None,
            card_filter: None,
            single_use_group: None,
            single_use: false,
            cast_cost_modifier: None,
            alt_ability_cost: None,
            without_paying_mana_cost: false,
            land_enter_tapped: EtbTapState::Unspecified,
        });
    grant_priority(&mut runner, P0);
    (runner, misinformation, cards, p3_library_card)
}

/// Cast Misinformation on `cards`, owned by `owner`, and stop at P0's
/// arrangement prompt over them (CR 101.1 + CR 608.2c: the caster follows
/// "… in any order"), mid-resolution: the reach guard every concession row
/// below relies on.
fn open_misinformation_arrangement(
    runner: &mut GameRunner,
    misinformation: ObjectId,
    cards: [ObjectId; 3],
    owner: PlayerId,
) {
    let outcome = runner.cast(misinformation).target_objects(&cards).resolve();
    assert_controller_arrangement_prompt(
        outcome.final_waiting_for(),
        P0,
        owner,
        &cards,
        LibraryPosition::Top,
    );
    assert_eq!(zone(runner, misinformation), Zone::Stack);
    for card in cards {
        assert_eq!(zone(runner, card), Zone::Graveyard);
    }
}

/// CR 800.4g + CR 800.4h: the open prompt is the election of an arranger —
/// `elector` chooses among `candidates`, for the object `source`.
fn assert_arranger_election(
    waiting_for: &WaitingFor,
    elector: PlayerId,
    candidates: &[PlayerId],
    source: ObjectId,
) {
    match waiting_for {
        WaitingFor::ChooseFromZoneOpponentChooser {
            player,
            candidates: offered,
            ability,
            purpose,
        } => {
            assert_eq!(
                *purpose,
                ZoneOpponentChooserPurpose::SubstituteArranger,
                "the prompt elects a substitute arranger"
            );
            assert_eq!(*player, elector, "the prompt names the elector");
            assert_eq!(
                offered.as_slice(),
                candidates,
                "every player still in the game, in APNAP order"
            );
            assert_eq!(
                ability.source_id, source,
                "the prompt carries the object that requires the choice"
            );
        }
        other => panic!("expected {elector:?}'s arranger election, got {other:?}"),
    }
}

/// `(owner, arranger, member ids)` of the batch the carrier holds while its
/// arranger is elected.
fn held_election_batch(runner: &GameRunner) -> (PlayerId, Option<PlayerId>, Vec<ObjectId>) {
    let batch = runner
        .state()
        .pending_mass_library_order_choice
        .as_ref()
        .and_then(|pending| pending.arranger_election.as_ref())
        .expect("the carrier holds the batch awaiting its arranger");
    (
        batch.owner,
        batch.arranger,
        batch
            .members
            .iter()
            .map(|member| member.identity.object_id)
            .collect(),
    )
}

/// The members of `cards` that `events` moved into a library.
fn library_arrivals(events: &[GameEvent], cards: &[ObjectId]) -> Vec<ObjectId> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::ZoneChanged {
                object_id,
                to: Zone::Library,
                ..
            } if cards.contains(object_id) => Some(*object_id),
            _ => None,
        })
        .collect()
}

/// N2 — CR 104.3a + CR 800.4a + CR 800.4g + CR 800.4h + CR 608.2m: P0 casts
/// Misinformation on a three-player board, targeting three cards in P2's
/// graveyard, and concedes at P0's own arrangement prompt (CR 101.1 +
/// CR 608.2c: the caster follows "… in any order"). P0's spell leaves the game
/// with P0, but it has begun resolving, so it continues fully (CR 608.2m).
/// P2's cards are still in the game and still need arranging, and the choice
/// was P0's: the controller of the object would choose another player to make
/// it (CR 800.4g), and since that controller has left, the next player in turn
/// order after them, P1, elects (CR 800.4h). P1 elects P2. P2's arrangement is
/// the exact top of P2's library, delivered in one move; the spell is reported
/// resolved once, and because the active player P0 has left, P1 receives
/// priority (CR 117.3b + CR 800.4j).
///
/// The elector P1, the nominee P2 and the departed controller P0 are three
/// players, so the election is told apart from both a turn-order hand-on and an
/// owner fallback.
///
/// REVERT-FAILING: without the election, (b) fails — the Concede publishes
/// P1's ordering prompt directly.
#[test]
fn misinformation_controller_concession_has_the_next_player_elect_the_arranger() {
    let (mut runner, misinformation, cards, p2_lib) = misinformation_board(3, P2);
    let [o1, o2, o3] = cards;
    // (a) Reach guard: the caster arranges P2's cards, mid-resolution.
    open_misinformation_arrangement(&mut runner, misinformation, cards, P2);

    let conceded = runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("P0 concedes");

    // (b) REVERT-FAILING: CR 800.4a — P0 and the spell leave; P2's cards stay.
    // CR 800.4g + CR 800.4h — P1 elects the player who arranges them.
    assert!(runner.state().players[P0.0 as usize].is_eliminated);
    assert_eq!(
        zone(&runner, misinformation),
        Zone::Exile,
        "CR 800.4a: P0's spell leaves the game with P0"
    );
    for card in cards {
        assert_eq!(zone(&runner, card), Zone::Graveyard);
    }
    assert_arranger_election(&runner.state().waiting_for, P1, &[P1, P2], misinformation);
    assert_eq!(runner.state().priority_player, P1);
    assert_eq!(
        held_election_batch(&runner),
        (P2, Some(P0), vec![o1, o2, o3]),
        "the batch waits, still naming its departed arranger"
    );
    assert_eq!(
        put_at_library_position_resolutions(&conceded.events, misinformation),
        0,
        "nothing completes while the arranger is elected"
    );
    assert!(library_arrivals(&conceded.events, &cards).is_empty());
    // CR 401.4: the held batch is never shown to any player.
    for viewer in [P1, P2] {
        assert!(
            filter_state_for_viewer(runner.state(), viewer)
                .pending_mass_library_order_choice
                .is_none(),
            "{viewer:?} never sees the held batch"
        );
    }

    // (c) The AI is offered every candidate and no arrangement yet; the
    // unelected P2 cannot answer the election, and P1 cannot elect the
    // departed P0. Neither rejection changes anything.
    let actions = legal_actions(runner.state());
    for candidate in [P1, P2] {
        assert!(
            actions.contains(&GameAction::ChooseZoneOpponentChooser {
                opponent: candidate
            }),
            "the elector may elect {candidate:?}"
        );
    }
    assert!(
        !actions
            .iter()
            .any(|action| matches!(action, GameAction::SelectCards { .. })),
        "no arrangement is open during the election"
    );
    let before = runner.state().clone();
    let rejection = apply_with_rejection(
        runner.state_mut(),
        P2,
        GameAction::ChooseZoneOpponentChooser { opponent: P2 },
    )
    .expect_err("P2 must not answer P1's election");
    assert_eq!(rejection.code, ActionRejectionCode::WrongPlayer);
    assert!(*runner.state() == before, "the rejection changes nothing");
    apply_with_rejection(
        runner.state_mut(),
        P1,
        GameAction::ChooseZoneOpponentChooser { opponent: P0 },
    )
    .expect_err("CR 102.1: a player who has left cannot be elected");
    assert!(*runner.state() == before, "the rejection changes nothing");

    // (d) CR 800.4g: the elected P2 arranges.
    let elected = runner
        .act(GameAction::ChooseZoneOpponentChooser { opponent: P2 })
        .expect("P1 elects P2");
    assert_controller_arrangement_prompt(
        &runner.state().waiting_for,
        P2,
        P2,
        &cards,
        LibraryPosition::Top,
    );
    assert_eq!(runner.state().priority_player, P2);
    for card in cards {
        assert_eq!(zone(&runner, card), Zone::Graveyard);
    }
    assert!(library_arrivals(&elected.events, &cards).is_empty());
    assert!(
        runner.state().pending_mass_library_order_choice.is_none(),
        "a single-batch arrangement keeps no carrier once its arranger is elected"
    );

    // (e) The unelected elector cannot submit the arrangement.
    let before = runner.state().clone();
    let rejection = apply_with_rejection(
        runner.state_mut(),
        P1,
        GameAction::SelectCards {
            cards: vec![o3, o1, o2],
        },
    )
    .expect_err("P1 must not submit P2's arrangement");
    assert_eq!(rejection.code, ActionRejectionCode::WrongPlayer);
    assert!(*runner.state() == before, "the rejection changes nothing");

    // (f) P2's arrangement completes the resolution, all three cards together.
    let submitted = runner
        .act(GameAction::SelectCards {
            cards: vec![o3, o1, o2],
        })
        .expect("P2 submits the arrangement");
    assert_eq!(
        library(&runner, P2),
        vec![o3, o1, o2, p2_lib],
        "P2's arrangement is the exact top of P2's library"
    );
    assert_eq!(
        put_at_library_position_resolutions(&submitted.events, misinformation),
        1,
        "the placement completes once"
    );
    let mut arrivals = library_arrivals(&submitted.events, &cards);
    arrivals.sort_by_key(|id| id.0);
    let mut expected = cards.to_vec();
    expected.sort_by_key(|id| id.0);
    assert_eq!(
        arrivals, expected,
        "CR 101.4: the three cards move in one batch"
    );
    // CR 608.2m: the departed spell that continued to resolve fully is
    // reported resolved once.
    assert_eq!(stack_resolved_count(&submitted.events, misinformation), 1);
    let state = runner.state();
    assert!(
        state.resolving_stack_entry.is_none(),
        "the resolution is over"
    );
    assert!(state.active_ability_continuation().is_none());
    assert!(state.pending_mass_library_order_choice.is_none());
    assert!(
        state
            .objects
            .values()
            .all(|object| object.zone != Zone::Stack),
        "no object is left on the stack"
    );
    // CR 117.3b + CR 800.4j.
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P1 });
}

/// CR 800.4g + CR 800.4h + CR 608.2m: the elected arranger leaves too. On a
/// four-player board P0 casts Misinformation on three cards in P3's graveyard
/// and concedes; P1, the next player after P0, elects P2, and P2 concedes at
/// their arrangement prompt. The object's controller is still the departed P0,
/// so the arrangement is elected again by the next player in turn order after
/// P0 — P1, not P3, the next after the departed nominee. P1 elects P3, whose
/// order completes the spell.
///
/// The controller, the elector, both nominees and the owner are told apart:
/// P0, P1, P2 and P3, with P3 both owner and second nominee.
///
/// REVERT-FAILING: with the elector derived from the departed arranger, the
/// second election names P3 (`next_player_in_turn_order(P2)`), not P1.
#[test]
fn misinformation_departed_nominee_is_replaced_by_the_controllers_elector() {
    let (mut runner, misinformation, cards, p3_lib) = misinformation_board(4, P3);
    let [o1, o2, o3] = cards;
    open_misinformation_arrangement(&mut runner, misinformation, cards, P3);
    runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("P0 concedes");
    // Reach guard: the first election, and P2's arrangement once elected.
    assert_arranger_election(
        &runner.state().waiting_for,
        P1,
        &[P1, P2, P3],
        misinformation,
    );
    runner
        .act(GameAction::ChooseZoneOpponentChooser { opponent: P2 })
        .expect("P1 elects P2");
    assert_controller_arrangement_prompt(
        &runner.state().waiting_for,
        P2,
        P3,
        &cards,
        LibraryPosition::Top,
    );

    let conceded = runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("P2 concedes");
    // REVERT-FAILING: the controller's elector, P1, elects again.
    assert_arranger_election(&runner.state().waiting_for, P1, &[P1, P3], misinformation);
    assert_eq!(runner.state().priority_player, P1);
    assert_eq!(
        held_election_batch(&runner),
        (P3, Some(P2), vec![o1, o2, o3]),
        "the batch waits, naming its departed nominee"
    );
    for card in cards {
        assert_eq!(zone(&runner, card), Zone::Graveyard);
    }
    assert_eq!(
        put_at_library_position_resolutions(&conceded.events, misinformation),
        0
    );

    runner
        .act(GameAction::ChooseZoneOpponentChooser { opponent: P3 })
        .expect("P1 elects P3");
    assert_controller_arrangement_prompt(
        &runner.state().waiting_for,
        P3,
        P3,
        &cards,
        LibraryPosition::Top,
    );
    let submitted = runner
        .act(GameAction::SelectCards {
            cards: vec![o2, o3, o1],
        })
        .expect("P3 submits the arrangement");
    assert_eq!(library(&runner, P3), vec![o2, o3, o1, p3_lib]);
    assert_eq!(
        put_at_library_position_resolutions(&submitted.events, misinformation),
        1
    );
    assert_eq!(stack_resolved_count(&submitted.events, misinformation), 1);
    assert!(runner.state().pending_mass_library_order_choice.is_none());
    // CR 117.3b + CR 800.4j.
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    );
}

/// CR 800.4g + CR 800.4h + CR 800.4a: the elector leaves during the election.
/// On the four-player board P0 concedes, then P1 concedes at the election. The
/// election is raised again against the players still in the game: the next
/// player in turn order after the departed controller P0 is now P2, choosing
/// between P2 and P3. P2 elects P3, whose order completes the spell.
///
/// REVERT-FAILING: without the open election's reconcile, the carrier is
/// retired with its departed elector and the generic repoint leaves priority,
/// so the re-raised election assertion fails.
#[test]
fn misinformation_elector_departure_reraises_the_election() {
    let (mut runner, misinformation, cards, p3_lib) = misinformation_board(4, P3);
    let [o1, o2, o3] = cards;
    open_misinformation_arrangement(&mut runner, misinformation, cards, P3);
    runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("P0 concedes");
    assert_arranger_election(
        &runner.state().waiting_for,
        P1,
        &[P1, P2, P3],
        misinformation,
    );

    let conceded = runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("P1 concedes");
    // REVERT-FAILING: the election is raised again for the players still in.
    assert_arranger_election(&runner.state().waiting_for, P2, &[P2, P3], misinformation);
    assert_eq!(runner.state().priority_player, P2);
    assert_eq!(
        held_election_batch(&runner),
        (P3, Some(P0), vec![o1, o2, o3])
    );
    assert_eq!(
        put_at_library_position_resolutions(&conceded.events, misinformation),
        0
    );

    runner
        .act(GameAction::ChooseZoneOpponentChooser { opponent: P3 })
        .expect("P2 elects P3");
    let submitted = runner
        .act(GameAction::SelectCards {
            cards: vec![o2, o3, o1],
        })
        .expect("P3 submits the arrangement");
    assert_eq!(library(&runner, P3), vec![o2, o3, o1, p3_lib]);
    assert_eq!(
        put_at_library_position_resolutions(&submitted.events, misinformation),
        1
    );
    assert_eq!(stack_resolved_count(&submitted.events, misinformation), 1);
    assert!(runner.state().pending_mass_library_order_choice.is_none());
    // CR 117.3b + CR 800.4j: the next living player after the departed active P0.
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P2 }
    );
}

/// CR 800.4a + CR 608.2m: the owner leaves during the election. On the
/// four-player board P0 concedes, then P3, the owner, concedes at the
/// election. P3's cards leave the game with P3, so nothing is left to
/// arrange; the spell keeps resolving, and its instruction completes with
/// nothing moved inside the Concede.
///
/// REVERT-FAILING: without the open election's emptied completion, the
/// carrier is retired and no completion is emitted.
#[test]
fn misinformation_owner_departure_during_the_election_completes_empty() {
    let (mut runner, misinformation, cards, _p3_lib) = misinformation_board(4, P3);
    open_misinformation_arrangement(&mut runner, misinformation, cards, P3);
    runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("P0 concedes");
    assert_arranger_election(
        &runner.state().waiting_for,
        P1,
        &[P1, P2, P3],
        misinformation,
    );

    let conceded = runner
        .act(GameAction::Concede { player_id: P3 })
        .expect("P3 concedes");
    for card in cards {
        assert_eq!(
            zone(&runner, card),
            Zone::Exile,
            "CR 800.4a: P3's cards leave the game with P3"
        );
    }
    // REVERT-FAILING: the instruction completes once, with nothing moved.
    assert_eq!(
        put_at_library_position_resolutions(&conceded.events, misinformation),
        1,
        "the placement completes once, empty, inside the Concede"
    );
    assert_eq!(stack_resolved_count(&conceded.events, misinformation), 1);
    let state = runner.state();
    assert!(state.pending_mass_library_order_choice.is_none());
    assert!(
        state
            .objects
            .values()
            .all(|object| object.zone != Zone::Stack),
        "no object is left on the stack"
    );
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P1 });
}

/// CR 800.4a + CR 608.2m + CR 608.2h + CR 109.4 + CR 400.7 + CR 800.4g +
/// CR 800.4h: a later departure that moves the exiled spell again does not
/// move its last known controller. On a four-player board P0 casts a
/// Misinformation that P2 owns from P2's exile and concedes at P0's own
/// arrangement; the spell, still controlled by P0, leaves the game with P0
/// (CR 800.4a) and continues to resolve (CR 608.2m). P1, the next player after
/// P0, elects P2, and P2 concedes at their arrangement. P2's departure moves
/// everything P2 owns out of the game — the already-exiled Misinformation
/// again, Exile to Exile — which overwrites the card's bare-id last known
/// information with its reset owner P2. But that is a different object now
/// (CR 400.7); the spell has no controller (CR 109.4), and its last known
/// information as a spell is the snapshot taken as it left the stack, naming
/// P0 (CR 608.2h). P0 has left, so P1, the next player after P0, elects again
/// (CR 800.4g + CR 800.4h), and P1 elects P3, whose order completes the spell.
///
/// The caster and last-known controller P0, the elector P1, the spell's owner
/// and first nominee P2 (the overwrite's source), and the cards' owner and
/// second nominee P3 are four distinct players.
///
/// REVERT-FAILING: (d) fails when the authority reads the bare-id `lki_cache`
/// — it answers the departed P2, so the next player after P2, P3, elects.
#[test]
fn misinformation_cast_from_its_owners_exile_is_reelected_by_its_departed_casters_successor() {
    let (mut runner, misinformation, cards, p3_lib) = misinformation_from_owners_exile_board();
    let [o1, o2, o3] = cards;

    // (a) Reach guard: P0 may cast P2's exiled card, and arranges P3's cards
    // mid-resolution, controlling a spell P2 owns.
    assert!(
        legal_actions(runner.state()).iter().any(|action| matches!(
            action,
            GameAction::CastSpell { object_id, .. } if *object_id == misinformation
        )),
        "P0 may cast the exiled card under the impulse permission"
    );
    let outcome = runner.cast(misinformation).target_objects(&cards).resolve();
    assert_controller_arrangement_prompt(
        outcome.final_waiting_for(),
        P0,
        P3,
        &cards,
        LibraryPosition::Top,
    );
    assert_eq!(zone(&runner, misinformation), Zone::Stack);
    assert_eq!(runner.state().objects[&misinformation].owner, P2);
    assert_eq!(runner.state().objects[&misinformation].controller, P0);

    // (b) CR 800.4a: the spell P0 controls leaves the game with P0; P1, the
    // next player after P0, elects the arranger.
    runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("P0 concedes");
    assert!(runner.state().players[P0.0 as usize].is_eliminated);
    assert_eq!(
        zone(&runner, misinformation),
        Zone::Exile,
        "CR 800.4a: the spell the leaver controls is exiled"
    );
    assert_eq!(
        runner.state().lki_cache[&misinformation].controller,
        P0,
        "reach guard: the stack-exit snapshot names P0"
    );
    assert_arranger_election(
        &runner.state().waiting_for,
        P1,
        &[P1, P2, P3],
        misinformation,
    );
    assert_eq!(
        held_election_batch(&runner),
        (P3, Some(P0), vec![o1, o2, o3])
    );

    // (c) P1 elects P2, the spell's owner.
    runner
        .act(GameAction::ChooseZoneOpponentChooser { opponent: P2 })
        .expect("P1 elects P2");
    assert_controller_arrangement_prompt(
        &runner.state().waiting_for,
        P2,
        P3,
        &cards,
        LibraryPosition::Top,
    );

    // (d) P2 concedes: P2's exiled card is moved again, overwriting its bare-id
    // last known information.
    let conceded = runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("P2 concedes");
    assert!(runner.state().players[P2.0 as usize].is_eliminated);
    assert_eq!(zone(&runner, misinformation), Zone::Exile);
    assert_eq!(
        runner.state().lki_cache[&misinformation].controller,
        P2,
        "hostile reach guard: P2's departure moved the exiled card again"
    );
    for card in cards {
        assert_eq!(zone(&runner, card), Zone::Graveyard);
    }
    // REVERT-FAILING: the spell's last known controller is P0, so P0's
    // successor P1 elects — not P3, the successor of the overwrite's P2.
    assert_arranger_election(&runner.state().waiting_for, P1, &[P1, P3], misinformation);
    assert_eq!(runner.state().priority_player, P1);
    assert_eq!(
        held_election_batch(&runner),
        (P3, Some(P2), vec![o1, o2, o3]),
        "the batch waits, naming its departed nominee"
    );
    assert_eq!(
        put_at_library_position_resolutions(&conceded.events, misinformation),
        0,
        "nothing completes while the arranger is elected"
    );
    assert!(library_arrivals(&conceded.events, &cards).is_empty());

    // (e) P3, the elector the overwritten snapshot would name, cannot elect.
    let before = runner.state().clone();
    let rejection = apply_with_rejection(
        runner.state_mut(),
        P3,
        GameAction::ChooseZoneOpponentChooser { opponent: P3 },
    )
    .expect_err("P3 must not answer P1's election");
    assert_eq!(rejection.code, ActionRejectionCode::WrongPlayer);
    assert!(*runner.state() == before, "the rejection changes nothing");

    // (f) P1 elects P3.
    runner
        .act(GameAction::ChooseZoneOpponentChooser { opponent: P3 })
        .expect("P1 elects P3");
    assert_controller_arrangement_prompt(
        &runner.state().waiting_for,
        P3,
        P3,
        &cards,
        LibraryPosition::Top,
    );

    // (g) P3's arrangement completes the resolution, all three cards together.
    let submitted = runner
        .act(GameAction::SelectCards {
            cards: vec![o2, o3, o1],
        })
        .expect("P3 submits the arrangement");
    assert_eq!(library(&runner, P3), vec![o2, o3, o1, p3_lib]);
    assert_eq!(
        put_at_library_position_resolutions(&submitted.events, misinformation),
        1,
        "the placement completes once"
    );
    let mut arrivals = library_arrivals(&submitted.events, &cards);
    arrivals.sort_by_key(|id| id.0);
    let mut expected = cards.to_vec();
    expected.sort_by_key(|id| id.0);
    assert_eq!(
        arrivals, expected,
        "CR 101.4: the three cards move in one batch"
    );
    assert_eq!(stack_resolved_count(&submitted.events, misinformation), 1);
    let state = runner.state();
    assert!(
        state.resolving_stack_entry.is_none(),
        "the resolution is over"
    );
    assert!(state.active_ability_continuation().is_none());
    assert!(state.pending_mass_library_order_choice.is_none());
    assert!(
        state
            .objects
            .values()
            .all(|object| object.zone != Zone::Stack),
        "no object is left on the stack"
    );
    assert_eq!(zone(&runner, misinformation), Zone::Exile);
    // CR 117.3b + CR 800.4j.
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P1 });
}

// ---------------------------------------------------------------------------
// A stolen arrangement whose thief leaves: the object's controller now — live
// on the stack, last-known once it has left — elects (CR 800.4g).
// ---------------------------------------------------------------------------

/// CR 117.3d: pass priority in turn order until `player` holds it, resolving
/// nothing on the way.
fn pass_priority_to(runner: &mut GameRunner, player: PlayerId) {
    let stack_len = runner.state().stack.len();
    for _ in 0..runner.state().players.len() {
        if runner.state().priority_player == player {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("the priority holder passes");
        assert_eq!(
            runner.state().stack.len(),
            stack_len,
            "passing priority to {player:?} resolves nothing"
        );
    }
    assert_eq!(runner.state().waiting_for, WaitingFor::Priority { player });
}

/// `thief`, holding priority, casts `commandeer` on `spell` and it resolves,
/// declining new targets: `thief` gains control of `spell`, which stays on the
/// stack (CR 613.1b).
fn resolve_commandeer(
    runner: &mut GameRunner,
    commandeer: ObjectId,
    spell: ObjectId,
    from: PlayerId,
    thief: PlayerId,
) {
    assert_eq!(runner.state().priority_player, thief);
    runner.cast(commandeer).target_object(spell).commit();
    let mut control_changed = false;
    for _ in 0..12 {
        if zone(runner, commandeer) != Zone::Stack
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        let result = match runner.state().waiting_for {
            WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
            WaitingFor::OptionalEffectChoice { .. } => {
                runner.act(GameAction::DecideOptionalEffect { accept: false })
            }
            ref other => panic!("unexpected Commandeer prompt: {other:?}"),
        }
        .expect("Commandeer resolves");
        control_changed |= result.events.iter().any(|event| {
            matches!(
                event,
                GameEvent::ControllerChanged { object_id, old_controller, new_controller, .. }
                    if *object_id == spell && *old_controller == from && *new_controller == thief
            )
        });
    }
    assert!(
        control_changed,
        "CR 613.1b: {thief:?} gains control of the spell from {from:?}"
    );
    assert_eq!(zone(runner, commandeer), Zone::Graveyard);
    assert_eq!(zone(runner, spell), Zone::Stack);
    assert_eq!(runner.state().objects[&spell].controller, thief);
}

/// The shared reach guard of the stolen-arrangement rows. On a four-player
/// board P0 casts Misinformation on three cards in P1's graveyard; P2 and then
/// P3 Commandeer it, each declining new targets (CR 613.1b + CR 613.7: P3's
/// later control effect governs). It resolves under P3, who follows its
/// instructions (CR 608.2c), so P3 arranges P1's cards and the batch names P3
/// its arranger. The spell's CR 112.2 default controller, its caster P0, is
/// unchanged.
///
/// Returns `(runner, misinformation, [o1, o2, o3], p1_library_card)`.
fn open_commandeered_misinformation_arrangement() -> (GameRunner, ObjectId, [ObjectId; 3], ObjectId)
{
    let (mut scenario, misinformation, cards, p1_lib) = misinformation_scenario(4, P1);
    let mut add_commandeer = |thief: PlayerId| {
        scenario
            .add_spell_to_hand_from_oracle(thief, "Commandeer", true, COMMANDEER_ORACLE)
            .with_mana_cost(ManaCost::generic(0))
            .id()
    };
    let p2_commandeer = add_commandeer(P2);
    let p3_commandeer = add_commandeer(P3);
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);

    runner.cast(misinformation).target_objects(&cards).commit();
    assert_eq!(zone(&runner, misinformation), Zone::Stack);
    pass_priority_to(&mut runner, P2);
    resolve_commandeer(&mut runner, p2_commandeer, misinformation, P0, P2);
    pass_priority_to(&mut runner, P3);
    resolve_commandeer(&mut runner, p3_commandeer, misinformation, P2, P3);
    // Every player passes in succession: Misinformation resolves.
    for _ in 0..runner.state().players.len() {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("the priority holder passes");
    }

    assert_controller_arrangement_prompt(
        &runner.state().waiting_for,
        P3,
        P1,
        &cards,
        LibraryPosition::Top,
    );
    let state = runner.state();
    assert_eq!(state.objects[&misinformation].zone, Zone::Stack);
    assert_eq!(state.objects[&misinformation].controller, P3);
    assert_eq!(
        state
            .resolving_stack_entry
            .as_ref()
            .map(|entry| entry.controller),
        Some(P0),
        "CR 112.2: the carrier keeps the caster as the default controller"
    );
    for card in cards {
        assert_eq!(zone(&runner, card), Zone::Graveyard);
    }
    (runner, misinformation, cards, p1_lib)
}

/// CR 800.4a + CR 613.1b + CR 613.7 + CR 800.4g + CR 109.4: the arranging
/// thief P3 concedes at the arrangement prompt. P3's control effect ends with
/// P3, so P2's earlier one governs the spell — still on the stack, still
/// resolving — and P2, the controller of the object, elects who arranges P1's
/// cards. The carrier's CR 112.2 default controller is still the caster P0.
fn concede_the_arranging_thief(
    runner: &mut GameRunner,
    misinformation: ObjectId,
    cards: [ObjectId; 3],
) {
    let conceded = runner
        .act(GameAction::Concede { player_id: P3 })
        .expect("P3 concedes");
    let state = runner.state();
    assert!(state.players[P3.0 as usize].is_eliminated);
    assert_eq!(
        state.objects[&misinformation].zone,
        Zone::Stack,
        "the spell is still resolving on the stack"
    );
    assert_eq!(
        state.objects[&misinformation].controller, P2,
        "CR 800.4a + CR 613.7: P3's control effect ended; P2's governs"
    );
    assert_eq!(
        state
            .resolving_stack_entry
            .as_ref()
            .map(|entry| entry.controller),
        Some(P0),
        "the carrier's default controller is not rewritten"
    );
    for card in cards {
        assert_eq!(zone(runner, card), Zone::Graveyard);
    }
    // REVERT-FAILING: the elector is the object's controller now, P2 — not the
    // caster P0 the carrier names. Every player still in the game is a
    // candidate, the elector and the caster included.
    assert_arranger_election(
        &runner.state().waiting_for,
        P2,
        &[P0, P1, P2],
        misinformation,
    );
    assert_eq!(runner.state().priority_player, P2);
    assert_eq!(
        held_election_batch(runner),
        (P1, Some(P3), cards.to_vec()),
        "the batch waits, still naming its departed arranger"
    );
    assert_eq!(
        put_at_library_position_resolutions(&conceded.events, misinformation),
        0,
        "nothing completes while the arranger is elected"
    );
    assert!(library_arrivals(&conceded.events, &cards).is_empty());
}

/// CR 800.4g: P2 elects the caster P0, who arranges P1's cards.
fn elect_the_caster(runner: &mut GameRunner, cards: [ObjectId; 3]) {
    let elected = runner
        .act(GameAction::ChooseZoneOpponentChooser { opponent: P0 })
        .expect("P2 elects P0");
    assert_controller_arrangement_prompt(
        &runner.state().waiting_for,
        P0,
        P1,
        &cards,
        LibraryPosition::Top,
    );
    assert_eq!(runner.state().priority_player, P0);
    for card in cards {
        assert_eq!(zone(runner, card), Zone::Graveyard);
    }
    assert!(library_arrivals(&elected.events, &cards).is_empty());
    assert!(
        runner.state().pending_mass_library_order_choice.is_none(),
        "a single-batch arrangement keeps no carrier once its arranger is elected"
    );
}

/// C1 — CR 800.4a + CR 613.1b + CR 613.7 + CR 800.4g + CR 109.4: P0 casts
/// Misinformation on three cards in P1's graveyard; P2 and then P3 Commandeer
/// it, and it resolves under P3, who arranges. P3 concedes. P3's control
/// effect ends (CR 800.4a), so P2's earlier one governs the spell, still on the
/// stack (CR 613.7). The object requires a choice of the departed P3, so the
/// controller of the object, P2, chooses another player to make it
/// (CR 800.4g). P2 elects the caster P0, who arranges; the spell completes
/// once and goes to P0's graveyard (CR 608.2n), and the active player P0
/// receives priority (CR 117.3b).
///
/// Four roles are four players: caster P0, cards' owner P1, controller and
/// elector P2, departed arranger P3. The caster cannot answer the election
/// but may arrange once elected.
///
/// REVERT-FAILING: with the elector read from the carrier's CR 112.2 default
/// controller, (b)'s `assert_arranger_election` names P0, the caster, not P2.
#[test]
fn commandeered_misinformation_reverted_controller_elects_the_arranger() {
    let (mut runner, misinformation, cards, p1_lib) =
        open_commandeered_misinformation_arrangement();
    let [o1, o2, o3] = cards;

    // (b) REVERT-FAILING: the controller of the object now, P2, elects.
    concede_the_arranging_thief(&mut runner, misinformation, cards);

    // (c) The AI is offered every candidate and no arrangement yet. The caster
    // and the owner cannot answer P2's election; P2 cannot elect the departed
    // P3. No rejection changes anything.
    let actions = legal_actions(runner.state());
    for candidate in [P0, P1, P2] {
        assert!(
            actions.contains(&GameAction::ChooseZoneOpponentChooser {
                opponent: candidate
            }),
            "the elector may elect {candidate:?}"
        );
    }
    assert!(
        !actions
            .iter()
            .any(|action| matches!(action, GameAction::SelectCards { .. })),
        "no arrangement is open during the election"
    );
    let before = runner.state().clone();
    for bystander in [P0, P1] {
        let rejection = apply_with_rejection(
            runner.state_mut(),
            bystander,
            GameAction::ChooseZoneOpponentChooser { opponent: P0 },
        )
        .expect_err("only the elector P2 answers the election");
        assert_eq!(rejection.code, ActionRejectionCode::WrongPlayer);
        assert!(*runner.state() == before, "the rejection changes nothing");
    }
    apply_with_rejection(
        runner.state_mut(),
        P2,
        GameAction::ChooseZoneOpponentChooser { opponent: P3 },
    )
    .expect_err("CR 102.1: a player who has left cannot be elected");
    assert!(*runner.state() == before, "the rejection changes nothing");

    // (d) CR 800.4g: the elected caster arranges.
    elect_the_caster(&mut runner, cards);

    // (e) The elector cannot submit the arrangement.
    let before = runner.state().clone();
    let rejection = apply_with_rejection(
        runner.state_mut(),
        P2,
        GameAction::SelectCards {
            cards: vec![o2, o3, o1],
        },
    )
    .expect_err("P2 must not submit P0's arrangement");
    assert_eq!(rejection.code, ActionRejectionCode::WrongPlayer);
    assert!(*runner.state() == before, "the rejection changes nothing");

    // (f) P0's arrangement completes the resolution, all three cards together.
    let submitted = runner
        .act(GameAction::SelectCards {
            cards: vec![o2, o3, o1],
        })
        .expect("P0 submits the arrangement");
    assert_eq!(
        library(&runner, P1),
        vec![o2, o3, o1, p1_lib],
        "P0's arrangement is the exact top of P1's library"
    );
    assert_eq!(
        put_at_library_position_resolutions(&submitted.events, misinformation),
        1,
        "the placement completes once"
    );
    let mut arrivals = library_arrivals(&submitted.events, &cards);
    arrivals.sort_by_key(|id| id.0);
    let mut expected = cards.to_vec();
    expected.sort_by_key(|id| id.0);
    assert_eq!(
        arrivals, expected,
        "CR 101.4: the three cards move in one batch"
    );
    assert_eq!(stack_resolved_count(&submitted.events, misinformation), 1);
    let state = runner.state();
    // CR 608.2n: the spell goes to its owner's graveyard.
    assert_eq!(state.objects[&misinformation].zone, Zone::Graveyard);
    assert!(state.players[P0.0 as usize]
        .graveyard
        .contains(&misinformation));
    assert!(
        state.resolving_stack_entry.is_none(),
        "the resolution is over"
    );
    assert!(state.active_ability_continuation().is_none());
    assert!(state.pending_mass_library_order_choice.is_none());
    assert!(
        state
            .objects
            .values()
            .all(|object| object.zone != Zone::Stack),
        "no object is left on the stack"
    );
    // CR 117.3b: the active player P0 is still in the game.
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P0 });
}

/// C2 — CR 800.4a + CR 608.2m + CR 608.2h + CR 109.4 + CR 800.4g: continuing
/// C1 to P0's arrangement prompt, the elected caster P0 concedes. Misinformation,
/// owned by P0, leaves the game with P0, but it has begun resolving, so it
/// continues fully (CR 608.2m). It has no controller now (CR 109.4); its last
/// known controller, P2 (CR 608.2h), is the controller of the object, so P2
/// elects again. P2 elects P1, whose order completes the spell, and the next
/// player after the departed active player, P1, receives priority
/// (CR 117.3b + CR 800.4j).
///
/// The object's default controller (P0), its controller off the stack (its
/// owner, P0) and its last known controller (P2) are three readings; only the
/// last names a living player who is not the next player after P0.
///
/// REVERT-FAILING: with the elector read from the carrier's default
/// controller, or from the object's controller off the stack, both P0, the
/// departed P0's successor P1 elects instead of P2.
#[test]
fn commandeered_misinformation_departed_nominee_is_reelected_by_the_last_known_controller() {
    let (mut runner, misinformation, cards, p1_lib) =
        open_commandeered_misinformation_arrangement();
    let [o1, o2, o3] = cards;
    concede_the_arranging_thief(&mut runner, misinformation, cards);
    elect_the_caster(&mut runner, cards);

    let conceded = runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("P0 concedes");
    assert!(runner.state().players[P0.0 as usize].is_eliminated);
    assert_eq!(
        zone(&runner, misinformation),
        Zone::Exile,
        "CR 800.4a: P0's spell leaves the game with P0"
    );
    for card in cards {
        assert_eq!(zone(&runner, card), Zone::Graveyard);
    }
    // REVERT-FAILING: the last known controller, P2, elects again.
    assert_arranger_election(&runner.state().waiting_for, P2, &[P1, P2], misinformation);
    assert_eq!(runner.state().priority_player, P2);
    assert_eq!(
        held_election_batch(&runner),
        (P1, Some(P0), vec![o1, o2, o3]),
        "the batch waits, naming its departed nominee"
    );
    assert_eq!(
        put_at_library_position_resolutions(&conceded.events, misinformation),
        0,
        "nothing completes while the arranger is elected"
    );
    assert!(library_arrivals(&conceded.events, &cards).is_empty());

    runner
        .act(GameAction::ChooseZoneOpponentChooser { opponent: P1 })
        .expect("P2 elects P1");
    assert_controller_arrangement_prompt(
        &runner.state().waiting_for,
        P1,
        P1,
        &cards,
        LibraryPosition::Top,
    );
    let submitted = runner
        .act(GameAction::SelectCards {
            cards: vec![o3, o1, o2],
        })
        .expect("P1 submits the arrangement");
    assert_eq!(library(&runner, P1), vec![o3, o1, o2, p1_lib]);
    assert_eq!(
        put_at_library_position_resolutions(&submitted.events, misinformation),
        1,
        "the placement completes once"
    );
    assert_eq!(stack_resolved_count(&submitted.events, misinformation), 1);
    let state = runner.state();
    assert!(state.pending_mass_library_order_choice.is_none());
    assert!(
        state.resolving_stack_entry.is_none(),
        "the resolution is over"
    );
    // CR 117.3b + CR 800.4j: the next living player after the departed active P0.
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P1 });
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
            payment_mode: CastPaymentMode::Auto,
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
/// `misinformation_places_all_chosen_cards_into_that_opponents_library_in_its_casters_arrangement`, whose
/// `library(P1) == [o3, o1, o2, p1_lib]` cannot pass at base. That pairing
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
