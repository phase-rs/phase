//! Issue #9995: "put N target ..." keeps its announced target count, and an
//! unconditional "Each of them enters with ..." rider after a battlefield mover
//! is carried by that mover's own entry counters.
//!
//! CR 601.2c + CR 115.1a: a spell whose text says "one, two, or three target ..."
//! or "two target ..." announces that many targets as it is cast; the number of
//! targets then does not change. CR 614.12 + CR 122.6: a rider that makes the
//! moved permanents enter with counters modifies how each of them enters.
//! CR 400.3: "on top of their owners' libraries" puts each target on top of
//! its owner's library, with no shuffle. CR 401.4: when two or more of those
//! cards go into the same library, that library's owner arranges them.
//!
//! Every card is staged from its verbatim Oracle text (checked against
//! `data/mtgjson/AtomicCards.json`).

use engine::ai_support::legal_actions;
use engine::game::engine::apply;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{EffectKind, LibraryPosition};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const ABERRANT_RETURN: &str = "Put one, two, or three target creature cards from graveyards onto the battlefield under your control. Each of them enters with an additional -1/-1 counter on it.";

const REBUKING_CEREMONY: &str = "Put two target artifacts on top of their owners' libraries.";

const PLOW_UNDER: &str = "Put two target lands on top of their owners' libraries.";

const P0_FILLERS: [&str; 4] = [
    "P0 Library 1",
    "P0 Library 2",
    "P0 Library 3",
    "P0 Library 4",
];

const P1_FILLERS: [&str; 4] = [
    "P1 Library 1",
    "P1 Library 2",
    "P1 Library 3",
    "P1 Library 4",
];

/// `player`'s library, top first.
fn library(state: &GameState, player: PlayerId) -> Vec<ObjectId> {
    state.players[player.0 as usize]
        .library
        .iter()
        .copied()
        .collect()
}

/// P0's board for an Aberrant Return cast: `p0_graveyard` creature cards in
/// P0's graveyard and `p1_graveyard` creature cards in P1's graveyard (all
/// vanilla 2/2s), plus the zero-cost sorcery in P0's hand.
///
/// Returns `(runner, spell, p0_graveyard_ids, p1_graveyard_ids)`.
fn aberrant_return_board(
    p0_graveyard: &[&str],
    p1_graveyard: &[&str],
) -> (GameRunner, ObjectId, Vec<ObjectId>, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Aberrant Return", false, ABERRANT_RETURN)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let p0_ids: Vec<ObjectId> = p0_graveyard
        .iter()
        .map(|name| scenario.add_creature_to_graveyard(P0, name, 2, 2).id())
        .collect();
    let p1_ids: Vec<ObjectId> = p1_graveyard
        .iter()
        .map(|name| scenario.add_creature_to_graveyard(P1, name, 2, 2).id())
        .collect();
    (scenario.build(), spell, p0_ids, p1_ids)
}

fn controller_of(runner: &GameRunner, id: ObjectId) -> PlayerId {
    runner.state().objects[&id].controller
}

/// C1 + C3. "One, two, or three target creature cards from graveyards" accepts
/// three announced targets, each of them (owned by either player) enters under
/// P0's control (CR 110.2a) with exactly one -1/-1 counter (CR 614.12 + CR 122.6),
/// and the fourth candidate is untouched because the spell has three slots
/// (CR 601.2c).
///
/// Reverting the count recovery leaves one target slot, so B and C stay in the
/// graveyard. Reverting the rider carrier leaves the rider an unimplemented gap,
/// so no creature gets a counter.
#[test]
fn aberrant_return_moves_up_to_three_targets_each_with_a_counter() {
    let (mut runner, spell, p0_gy, p1_gy) = aberrant_return_board(
        &["Graveyard Bear A"],
        &["Graveyard Bear B", "Graveyard Bear C", "Graveyard Bear D"],
    );
    let a = p0_gy[0];
    let (b, c, d) = (p1_gy[0], p1_gy[1], p1_gy[2]);

    let outcome = runner.cast(spell).target_objects(&[a, b, c, d]).resolve();

    // CR 601.2c: three announced targets reach the battlefield.
    outcome.assert_zone(&[a, b, c], Zone::Battlefield);
    // CR 110.2a: put onto the battlefield "under your control" - including the
    // creatures P1 owns.
    for id in [a, b, c] {
        assert_eq!(
            controller_of(&runner, id),
            P0,
            "CR 110.2a: object {} enters under P0's control",
            id.0
        );
    }
    // CR 614.12 + CR 122.6: "Each of them enters with an additional -1/-1
    // counter" - exactly one counter on every moved creature, not just the first.
    for id in [a, b, c] {
        assert_eq!(
            outcome.counters(id, CounterType::Minus1Minus1),
            1,
            "object {} must have entered with exactly one -1/-1 counter",
            id.0
        );
        let object = &outcome.state().objects[&id];
        assert_eq!(
            (object.power, object.toughness),
            (Some(1), Some(1)),
            "a 2/2 with one -1/-1 counter is a 1/1 (CR 122.1)"
        );
    }
    // The spell has at most three target slots, so the fourth declared
    // candidate stays where it was.
    outcome.assert_zone(&[d], Zone::Graveyard);
    assert_eq!(outcome.counters(d, CounterType::Minus1Minus1), 0);
}

/// C1 sibling. The minimum is one target: a single announced target is legal,
/// and the one creature moved still gets its counter. The caster announces a
/// target count of one (CR 601.2c: a variable number of targets is announced
/// before the targets).
///
/// Reach guard for the "other candidates stay put" assertion: the declared
/// creature DID move, so the undeclared one staying in the graveyard is not an
/// artifact of the spell never resolving.
#[test]
fn aberrant_return_single_target_is_legal() {
    let (mut runner, spell, p0_gy, p1_gy) =
        aberrant_return_board(&["Graveyard Bear A"], &["Graveyard Bear B"]);
    let a = p0_gy[0];
    let b = p1_gy[0];

    let outcome = runner.cast(spell).target_objects(&[a]).resolve();

    outcome.assert_zone(&[a], Zone::Battlefield);
    assert_eq!(controller_of(&runner, a), P0);
    assert_eq!(
        outcome.counters(a, CounterType::Minus1Minus1),
        1,
        "CR 614.12 + CR 122.6: the one moved creature enters with its counter"
    );
    outcome.assert_zone(&[b], Zone::Graveyard);
    assert_eq!(outcome.counters(b, CounterType::Minus1Minus1), 0);
}

/// C2. "Put two target artifacts on top of their owners' libraries" announces
/// exactly two targets (CR 601.2c). One artifact is controlled by P1 and one by
/// P0; both leave the battlefield for their owners' libraries (CR 400.3), and a
/// third artifact that was not targeted stays on the battlefield.
///
/// The two-target count comes from the exact announced-target arm of
/// `peel_library_placement_cardinality` (CR 601.2c). Reverting that arm also
/// closes the plural gate in `parse_put_ast`, so the clause falls back to the
/// shuffling `ChangeZone` route and the top-of-library and order assertions
/// below fail.
///
/// Each target goes on top of its owner's library and the rest of that
/// library keeps its order (no shuffle), CR 400.3.
///
/// CR 401.4 needs two or more cards in one library: with one card per owner
/// there is nothing to arrange, so no ordering prompt opens and the spell
/// resolves straight to priority. The reach guard for that negative is
/// `rebuking_ceremony_owner_arranges_when_it_owns_both_targets`, where the same
/// card with both targets in one owner's library does prompt.
#[test]
fn rebuking_ceremony_targets_two_artifacts() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &P0_FILLERS);
    scenario.with_library_top(P1, &P1_FILLERS);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Rebuking Ceremony", false, REBUKING_CEREMONY)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let theirs = scenario
        .add_artifact_from_oracle(P1, "Opposing Relic", "")
        .id();
    let mine = scenario
        .add_artifact_from_oracle(P0, "Friendly Relic", "")
        .id();
    let bystander = scenario
        .add_artifact_from_oracle(P1, "Bystander Relic", "")
        .id();
    let mut runner = scenario.build();
    let p0_before = library(runner.state(), P0);
    let p1_before = library(runner.state(), P1);

    let outcome = runner.cast(spell).target_objects(&[theirs, mine]).resolve();

    // CR 601.2c: both announced targets are put into a library.
    outcome.assert_zone(&[theirs, mine], Zone::Library);
    // CR 400.3: each goes to its owner's library.
    assert!(
        outcome.state().players[P1.0 as usize]
            .library
            .contains(&theirs),
        "the opposing artifact goes to its owner's library"
    );
    assert!(
        outcome.state().players[P0.0 as usize]
            .library
            .contains(&mine),
        "my artifact goes to my library"
    );
    // Exactly two targets: the third artifact is untouched.
    outcome.assert_zone(&[bystander], Zone::Battlefield);

    // CR 400.3: each target is on TOP of its owner's library, and the cards
    // below it keep their pre-cast order (no shuffle).
    let p0_after = library(outcome.state(), P0);
    let p1_after = library(outcome.state(), P1);
    assert_eq!(
        p1_after[0], theirs,
        "the opposing artifact is on top of P1's library"
    );
    assert_eq!(p0_after[0], mine, "my artifact is on top of my library");
    assert_eq!(
        p1_after[1..],
        p1_before[..],
        "P1's library below the top is unshuffled"
    );
    assert_eq!(
        p0_after[1..],
        p0_before[..],
        "P0's library below the top is unshuffled"
    );
    assert_eq!(p1_after.len(), p1_before.len() + 1);
    assert_eq!(p0_after.len(), p0_before.len() + 1);

    // Completion boundary: the spell resolved and priority returned.
    assert!(outcome.state().stack.is_empty(), "the spell resolved");
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "expected priority after resolution, got {:?}",
        outcome.final_waiting_for()
    );
}

/// CR 401.4: the owner's arrangement prompt for two or more cards an effect
/// puts on top of `owner`'s library at the same time. Asserted unconditionally:
/// any other `WaitingFor` (including priority after an unprompted placement)
/// fails the row.
fn assert_owner_top_arrangement_prompt(
    waiting_for: &WaitingFor,
    owner: PlayerId,
    cards: &[ObjectId],
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
            assert_eq!(*player, owner, "CR 401.4: the cards' owner arranges them");
            assert_eq!(
                offered, cards,
                "the prompt names exactly that owner's cards"
            );
            assert_eq!(
                (*count, *min_count, *up_to),
                (cards.len(), cards.len(), false)
            );
            assert_eq!(*effect_kind, EffectKind::PutAtLibraryPosition);
            assert_eq!(*library_position, Some(LibraryPosition::Top));
            assert!(
                mass_library_order.is_some(),
                "the prompt carries the frozen owner batch"
            );
        }
        other => panic!("expected {owner:?}'s CR 401.4 ordering prompt, got {other:?}"),
    }
}

/// "Put two target lands on top of their owners' libraries" (Plow Under) with
/// both targets owned by P1 and cast by P0: CR 401.4 lets P1, the owner,
/// arrange the two lands as the effect is applied (CR 608.2d); P0's target
/// announcement (CR 601.2c) is not an arrangement. P1 orders them
/// `[land_b, land_a]`, which differs from both the announcement and the
/// prompt's order, and that is the exact top of P1's library afterwards, with
/// the cards below unshuffled (CR 400.3).
///
/// Reverting the owner arrangement in `put_on_top::resolve`: `.resolve()`
/// reaches priority with no prompt, so the prompt assertion fails. Reverting
/// the plural admission in `parse_put_ast` or the exact announced-target arm of
/// `peel_library_placement_cardinality`: the clause stays a shuffling
/// `ChangeZone`, so no prompt opens either.
#[test]
fn plow_under_owner_arranges_both_lands_on_top_of_their_library() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P1, &P1_FILLERS);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Plow Under", false, PLOW_UNDER)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let land_a = scenario
        .add_land_from_oracle(P1, "Opposing Forest", "")
        .id();
    let land_b = scenario
        .add_land_from_oracle(P1, "Opposing Island", "")
        .id();
    let bystander = scenario
        .add_land_from_oracle(P0, "Friendly Plains", "")
        .id();
    let mut runner = scenario.build();
    let p1_before = library(runner.state(), P1);

    let outcome = runner
        .cast(spell)
        .target_objects(&[land_a, land_b])
        .resolve();

    // REVERT-FAILING: P1, not the caster, is asked to order the two lands.
    assert_owner_top_arrangement_prompt(outcome.final_waiting_for(), P1, &[land_a, land_b]);
    // Nothing has moved while the owner decides.
    outcome.assert_zone(&[land_a, land_b], Zone::Battlefield);

    // The choice belongs to P1: the caster cannot submit it, and a rejected
    // submission leaves the prompt as it was.
    let prompt = runner.state().waiting_for.clone();
    assert!(
        apply(
            runner.state_mut(),
            P0,
            GameAction::SelectCards {
                cards: vec![land_b, land_a],
            },
        )
        .is_err(),
        "the caster must not be able to submit the owner's arrangement"
    );
    assert_eq!(runner.state().waiting_for, prompt);
    // The AI's candidate generation offers the owner a submission over both lands.
    assert!(
        legal_actions(runner.state()).iter().any(|action| matches!(
            action,
            GameAction::SelectCards { cards }
                if cards.len() == 2 && cards.contains(&land_a) && cards.contains(&land_b)
        )),
        "the owner must have a legal ordering submission, got {:?}",
        legal_actions(runner.state())
    );

    runner
        .act(GameAction::SelectCards {
            cards: vec![land_b, land_a],
        })
        .expect("P1 submits their arrangement");

    let mut expected = vec![land_b, land_a];
    expected.extend(p1_before);
    assert_eq!(
        library(runner.state(), P1),
        expected,
        "P1's chosen order is the exact top of P1's library, the rest unshuffled"
    );
    assert_eq!(
        runner.state().objects[&bystander].zone,
        Zone::Battlefield,
        "the untargeted land stays put"
    );
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Graveyard,
        "CR 608.2n: the resolved sorcery goes to its owner's graveyard"
    );
    assert!(runner.state().stack.is_empty(), "the spell resolved");
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "CR 117.3b: the active player receives priority after resolution"
    );
}

/// CR 401.4 + CR 400.3: the OWNER arranges, not the controller or the caster.
/// P0 casts Rebuking Ceremony on two artifacts P1 owns, one of which P0
/// controls. Both go to P1's library, so P1 orders them; P0's library is
/// untouched.
#[test]
fn rebuking_ceremony_owner_arranges_when_it_owns_both_targets() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &P0_FILLERS);
    scenario.with_library_top(P1, &P1_FILLERS);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Rebuking Ceremony", false, REBUKING_CEREMONY)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let borrowed = scenario
        .add_artifact_from_oracle(P1, "Borrowed Relic", "")
        .controlled_by(P0)
        .id();
    let theirs = scenario
        .add_artifact_from_oracle(P1, "Their Relic", "")
        .id();
    let mut runner = scenario.build();
    assert_eq!(
        controller_of(&runner, borrowed),
        P0,
        "fixture: P0 controls the borrowed relic"
    );
    let p0_before = library(runner.state(), P0);
    let p1_before = library(runner.state(), P1);

    let outcome = runner
        .cast(spell)
        .target_objects(&[borrowed, theirs])
        .resolve();

    // REVERT-FAILING: the owner (P1) is prompted, not the controller/caster (P0).
    assert_owner_top_arrangement_prompt(outcome.final_waiting_for(), P1, &[borrowed, theirs]);

    runner
        .act(GameAction::SelectCards {
            cards: vec![theirs, borrowed],
        })
        .expect("P1 submits their arrangement");

    let p1_after = library(runner.state(), P1);
    assert_eq!(
        p1_after[..2],
        [theirs, borrowed],
        "P1's chosen order is the exact top of P1's library"
    );
    assert_eq!(
        p1_after[2..],
        p1_before[..],
        "P1's library below the top two is unshuffled"
    );
    assert_eq!(
        library(runner.state(), P0),
        p0_before,
        "neither artifact goes to its controller's library"
    );
    assert!(runner.state().stack.is_empty(), "the spell resolved");
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    );
}

/// CR 400.3: an artifact P1 owns but P0 controls goes to its OWNER's library
/// (P1's), on top, not to its controller's.
///
/// Each library receives one card, so CR 401.4 has nothing to arrange and no
/// ordering prompt opens; `rebuking_ceremony_owner_arranges_when_it_owns_both_targets`
/// is the prompting reach guard on the same card.
///
/// Reverting the plural admission in `parse_put_ast`: the libraries are
/// shuffled, so the `[0]` and `[1..]` order assertions fail. Reverting the
/// exact announced-target arm of `peel_library_placement_cardinality`: the
/// predicate rejects "two target …", so the clause stays a shuffling
/// `ChangeZone` and the same assertions fail.
#[test]
fn rebuking_ceremony_returns_a_controlled_artifact_to_its_owners_library() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &P0_FILLERS);
    scenario.with_library_top(P1, &P1_FILLERS);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Rebuking Ceremony", false, REBUKING_CEREMONY)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let borrowed = scenario
        .add_artifact_from_oracle(P1, "Borrowed Relic", "")
        .controlled_by(P0)
        .id();
    let mine = scenario
        .add_artifact_from_oracle(P0, "Friendly Relic", "")
        .id();
    let mut runner = scenario.build();
    assert_eq!(
        controller_of(&runner, borrowed),
        P0,
        "fixture: P0 controls the borrowed relic"
    );
    let p0_before = library(runner.state(), P0);
    let p1_before = library(runner.state(), P1);

    let outcome = runner
        .cast(spell)
        .target_objects(&[borrowed, mine])
        .resolve();

    let p0_after = library(outcome.state(), P0);
    let p1_after = library(outcome.state(), P1);
    assert_eq!(
        p1_after[0], borrowed,
        "the borrowed relic is on top of its owner's library"
    );
    assert!(
        !p0_after.contains(&borrowed),
        "the borrowed relic does not go to its controller's library"
    );
    assert_eq!(p0_after[0], mine, "my relic is on top of my library");
    assert_eq!(
        p1_after[1..],
        p1_before[..],
        "P1's library below the top is unshuffled"
    );
    assert_eq!(
        p0_after[1..],
        p0_before[..],
        "P0's library below the top is unshuffled"
    );

    assert!(outcome.state().stack.is_empty(), "the spell resolved");
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "expected priority after resolution, got {:?}",
        outcome.final_waiting_for()
    );
}
