//! Locke, Treasure Hunter — "If a land card was milled this way" is ONE
//! look-back over the whole multi-player mill, not a per-player gate (#9225).
//!
//! Oracle (Mug): "Whenever Locke attacks, each player mills a card. If a land
//! card was milled this way, create a Treasure token. Until end of turn, you may
//! cast a spell from among those cards."
//!
//! Ruling (2025-06-06): "As long as one or more lands were milled this way,
//! you'll create a Treasure token. Additional land cards milled beyond the first
//! won't cause you to create additional Treasures."
//!
//! CR anchors:
//!   - CR 608.2f: "each player mills a card" is one action taken on multiple
//!     players; the engine processes it per player, but it stays ONE action.
//!   - CR 608.2c: the following sentence is the next instruction in the order
//!     written, so its "this way" condition reads that whole action's result
//!     once, and its "you" is Locke's controller (CR 109.5).
//!
//! The defect only shows with three or more players: the player-scope splitter
//! kept every `ZoneChangedThisWay`-gated sub inside the per-player iteration,
//! so the Treasure clause re-ran once per player whose own mill was a land, with
//! its implicit controller rebound to that player. In a two-player game the
//! symptom can coincide with the correct answer, so these fixtures use four.
//!
//! The per-player class (Kroxa's "each opponent who didn't discard a nonland
//! card this way loses 3 life") must keep its per-iteration gate — that is
//! pinned by `kroxa_titan_nonland_discard_life_loss`.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::CastingPermission;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Verbatim Oracle text (Scryfall `cards/named?exact=Locke, Treasure Hunter`).
const LOCKE: &str = "Locke can't be blocked by creatures with greater power.\n\
     Mug — Whenever Locke attacks, each player mills a card. If a land card was \
     milled this way, create a Treasure token. Until end of turn, you may cast a \
     spell from among those cards.";

const PLAYER_COUNT: u8 = 4;
const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

/// Four players, Locke (P0) attacking P1, with one staged card on top of every
/// player's library: a land for each player in `land_milled_by`, a spell for the
/// rest. Returns the runner after the Mug trigger has resolved, plus the staged
/// cards in seat order.
fn locke_attacks_in_four_player_game(land_milled_by: &[PlayerId]) -> (GameRunner, Vec<ObjectId>) {
    let mut scenario = GameScenario::new_n_player(PLAYER_COUNT, 9225);
    // CR 504.1: start past the draw step so the staged top cards stay on top.
    scenario.at_phase(Phase::PreCombatMain);
    let locke = scenario
        .add_creature_from_oracle(P0, "Locke, Treasure Hunter", 3, 3, LOCKE)
        .id();
    let staged: Vec<ObjectId> = (0..PLAYER_COUNT)
        .map(|seat| {
            let player = PlayerId(seat);
            if land_milled_by.contains(&player) {
                scenario.add_land_to_library_top(player, "Milled Land").id()
            } else {
                scenario
                    .add_spell_to_library_top(player, "Milled Spell", false)
                    .with_mana_cost(engine::types::mana::ManaCost::zero())
                    .id()
            }
        })
        .collect();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(locke, AttackTarget::Player(P1))])
        .expect("Locke must be able to attack");
    runner.advance_until_stack_empty();
    (runner, staged)
}

/// Controllers of every Treasure on the battlefield.
fn treasure_controllers(runner: &GameRunner) -> Vec<PlayerId> {
    runner
        .state()
        .objects
        .values()
        .filter(|object| object.zone == Zone::Battlefield && object.name == "Treasure")
        .map(|object| object.controller)
        .collect()
}

fn assert_every_staged_card_was_milled(runner: &GameRunner, staged: &[ObjectId]) {
    for (seat, id) in staged.iter().enumerate() {
        assert_eq!(
            runner.state().objects[id].zone,
            Zone::Graveyard,
            "reach guard: player {seat} must have milled their staged top card"
        );
    }
}

/// Every Treasure on the battlefield.
fn treasures(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .objects
        .values()
        .filter(|object| object.zone == Zone::Battlefield && object.name == "Treasure")
        .map(|object| object.id)
        .collect()
}

/// Whether `id` holds the Mug's "you may cast a spell from among those cards"
/// grant (a single-use `PlayFromExile` permission over the milled batch).
fn holds_mug_cast_grant(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id]
        .casting_permissions
        .iter()
        .any(|permission| {
            matches!(
                permission,
                CastingPermission::PlayFromExile {
                    single_use: true,
                    ..
                }
            )
        })
}

/// CR 608.2c + CR 608.2f: every one of four players mills a land, and Locke's
/// controller creates exactly ONE Treasure. Reverting the splitter fix keeps the
/// gated Token inside the per-player iteration, creating a Treasure per player
/// (each under the iterating player's control).
#[test]
fn every_player_milling_a_land_creates_exactly_one_treasure_for_lockes_controller() {
    let (runner, staged) = locke_attacks_in_four_player_game(&[P0, P1, P2, P3]);
    assert_every_staged_card_was_milled(&runner, &staged);

    assert_eq!(
        treasure_controllers(&runner),
        vec![P0],
        "one or more lands milled this way creates exactly one Treasure, under \
         Locke's controller — not one per player who milled a land"
    );
    // CR 108.2b + CR 608.2c: "those cards" is every card the mill moved — all
    // four, from every player — and never the Treasure, which is not a card.
    for (seat, id) in staged.iter().enumerate() {
        assert!(
            holds_mug_cast_grant(&runner, *id),
            "player {seat}'s milled card is among \"those cards\""
        );
    }
    let treasure = treasures(&runner);
    assert_eq!(treasure.len(), 1, "reach guard: one Treasure was created");
    assert!(
        !holds_mug_cast_grant(&runner, treasure[0]),
        "the Treasure is a token, not one of the cards milled this way"
    );
}

/// CR 109.5 + CR 608.2c: only an opponent mills a land. The single Treasure is
/// still Locke's controller's — "you" never rebinds to the player whose mill
/// satisfied the condition. Reverting the fix hands the Treasure to P2.
#[test]
fn an_opponents_milled_land_creates_the_treasure_for_lockes_controller() {
    let (runner, staged) = locke_attacks_in_four_player_game(&[P2]);
    assert_every_staged_card_was_milled(&runner, &staged);

    assert_eq!(
        treasure_controllers(&runner),
        vec![P0],
        "the Treasure is created by Locke's controller even though only P2 milled a land"
    );
}

/// CR 608.2c: with no land among the milled cards the condition is false and no
/// Treasure is created. Pairs with the test above so the gate is shown to
/// discriminate, not merely to fire once.
#[test]
fn no_land_milled_creates_no_treasure() {
    let (runner, staged) = locke_attacks_in_four_player_game(&[]);
    assert_every_staged_card_was_milled(&runner, &staged);

    assert!(
        treasure_controllers(&runner).is_empty(),
        "no land card was milled this way, so no Treasure is created"
    );
}

/// Printed Locke against a defender controlling Rest in Peace and, when
/// `leyline` is set, Leyline of the Void, with the top cards staged.
///
/// CR 616.1: with both, two replacements apply to Locke's controller's (P0's)
/// milled card, so P0, the owner of the affected card, chooses between them. The
/// defender's own card meets only Rest in Peace (Leyline guards its controller's
/// opponents), so it is exiled with no prompt after the pause. Either way the
/// card is exiled, and CR 701.17c says an effect referring to a milled card finds
/// it in the public zone it moved to. Returns the runner once the stack is empty,
/// the number of replacement prompts answered, and the staged cards in seat order.
fn locke_mill_into_exile(
    leyline: bool,
    land_milled_by: &[PlayerId],
) -> (GameRunner, usize, Vec<ObjectId>) {
    locke_mill_into_exile_with(P1, leyline, land_milled_by)
}

/// [`locke_mill_into_exile`] with the replacement permanents controlled by
/// `defender_seat`: P1 pauses Locke's controller's (first) seat, P0 pauses the
/// LAST seat (P1's own card meets both of P0's replacements and P1 chooses).
fn locke_mill_into_exile_with(
    replacement_controller: PlayerId,
    leyline: bool,
    land_milled_by: &[PlayerId],
) -> (GameRunner, usize, Vec<ObjectId>) {
    use engine::game::scenario_db::GameScenarioDbExt;
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;

    let db = crate::support::shared_card_db().expect("card database");
    let mut scenario = GameScenario::new_n_player(2, 9225);
    scenario.at_phase(Phase::PreCombatMain);
    let locke = scenario.add_real_card(P0, "Locke, Treasure Hunter", Zone::Battlefield, db);
    scenario.add_real_card(
        replacement_controller,
        "Rest in Peace",
        Zone::Battlefield,
        db,
    );
    if leyline {
        scenario.add_real_card(
            replacement_controller,
            "Leyline of the Void",
            Zone::Battlefield,
            db,
        );
    }
    let staged: Vec<ObjectId> = [P0, P1]
        .into_iter()
        .map(|player| {
            if land_milled_by.contains(&player) {
                scenario.add_land_to_library_top(player, "Milled Land").id()
            } else {
                scenario
                    .add_spell_to_library_top(player, "Milled Spell", false)
                    .with_mana_cost(engine::types::mana::ManaCost::zero())
                    .id()
            }
        })
        .collect();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(locke, AttackTarget::Player(P1))])
        .expect("Locke must be able to attack");
    let mut prompts = 0;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice { .. } => {
                prompts += 1;
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("the replacement choice is accepted");
            }
            _ => {
                let state = runner.state();
                if state.stack.is_empty() && state.resolution_stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority passes");
            }
        }
    }
    (runner, prompts, staged)
}

fn assert_exiled_mill_results(runner: &GameRunner, staged: &[ObjectId], expected_treasures: usize) {
    for (seat, id) in staged.iter().enumerate() {
        assert_eq!(
            runner.state().objects[id].zone,
            Zone::Exile,
            "reach guard: seat {seat}'s milled card was exiled by a replacement"
        );
        assert!(
            holds_mug_cast_grant(runner, *id),
            "seat {seat}'s card is still a milled card, found in exile (CR 701.17c)"
        );
    }
    assert_eq!(treasures(runner).len(), expected_treasures);
    for treasure in treasures(runner) {
        assert_eq!(runner.state().objects[&treasure].controller, P0);
    }
}

/// CR 701.17c + CR 608.2c + CR 616.1: Locke's controller answers a replacement
/// choice for their own card; the defender's later card is exiled automatically
/// after the pause. Both are milled cards and carry the cast permission.
#[test]
fn a_replacement_pause_keeps_the_later_seats_exiled_spell_castable() {
    let (runner, prompts, staged) = locke_mill_into_exile(true, &[]);
    assert_eq!(prompts, 1, "reach guard: one choice, for P0's own card");
    assert_exiled_mill_results(&runner, &staged, 0);
}

/// CR 701.17c: the defender's land, exiled after the pause, still satisfies "a
/// land card was milled this way".
#[test]
fn a_later_seats_exiled_land_still_creates_the_treasure_across_the_pause() {
    let (runner, prompts, staged) = locke_mill_into_exile(true, &[P1]);
    assert_eq!(prompts, 1, "reach guard: one choice, for P0's own card");
    assert_exiled_mill_results(&runner, &staged, 1);
}

/// CR 701.17c: the paused seat's own exiled land counts too, and exactly one
/// Treasure is made when both seats' lands are exiled.
#[test]
fn an_exiled_land_on_the_paused_seat_creates_one_treasure() {
    let (runner, prompts, staged) = locke_mill_into_exile(true, &[P0]);
    assert_eq!(prompts, 1);
    assert_exiled_mill_results(&runner, &staged, 1);
    let (runner, prompts, staged) = locke_mill_into_exile(true, &[P0, P1]);
    assert_eq!(prompts, 1);
    assert_exiled_mill_results(&runner, &staged, 1);
}

/// Unpaused control: Rest in Peace alone applies, so no choice is offered and
/// the same results hold.
#[test]
fn without_a_replacement_choice_exiled_mills_give_the_same_results() {
    let (runner, prompts, staged) = locke_mill_into_exile(false, &[P1]);
    assert_eq!(prompts, 0, "reach guard: a single replacement asks nothing");
    assert_exiled_mill_results(&runner, &staged, 1);
    let (runner, prompts, staged) = locke_mill_into_exile(false, &[]);
    assert_eq!(prompts, 0);
    assert_exiled_mill_results(&runner, &staged, 0);
}

/// Walks combat to the postcombat main phase, answering the declare-blockers
/// step with no blocks (CR 509.1).
fn advance_past_combat(runner: &mut GameRunner) {
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    for _ in 0..40 {
        if runner.state().phase == Phase::PostCombatMain {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).expect("no blocks");
            }
            _ => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority is legal");
            }
        }
        runner.advance_until_stack_empty();
    }
    panic!("combat did not reach the postcombat main phase");
}

fn can_cast(runner: &GameRunner, id: ObjectId) -> bool {
    use engine::types::actions::GameAction;
    engine::ai_support::legal_actions(runner.state())
        .iter()
        .any(|action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == id))
}

/// CR 701.17c + CR 601.2a: after the paused flow, Locke's controller casts the
/// exiled spell through the public cast action at sorcery timing, it reaches the
/// stack, and the single-use grant leaves the OTHER eligible spell uncastable.
fn assert_one_shared_cast(mut runner: GameRunner, staged: &[ObjectId], first_cast: usize) {
    advance_past_combat(&mut runner);
    let (cast, other) = (staged[first_cast], staged[1 - first_cast]);
    assert!(
        can_cast(&runner, cast),
        "reach guard: the exiled spell is offered"
    );
    assert!(
        can_cast(&runner, other),
        "reach guard: its sibling is offered too"
    );
    runner.cast(cast).commit();
    assert_eq!(
        runner.state().objects[&cast].zone,
        Zone::Stack,
        "the exiled spell reached the stack through the public cast action"
    );
    runner.advance_until_stack_empty();
    assert!(
        !can_cast(&runner, other) && !holds_mug_cast_grant(&runner, other),
        "the grant allows ONE cast; the other eligible spell is closed out"
    );
}

/// Pause on the FIRST seat: the defender's later, automatically exiled spell is
/// cast, then the paused seat's spell is not.
#[test]
fn the_later_seats_exiled_spell_is_actually_cast_once_after_the_pause() {
    let (runner, prompts, staged) = locke_mill_into_exile(true, &[]);
    assert_eq!(prompts, 1, "reach guard: the first seat paused");
    assert_one_shared_cast(runner, &staged, 1);
}

/// Pause on the first seat, cast the paused seat's own exiled spell instead.
#[test]
fn the_paused_seats_exiled_spell_is_actually_cast_once_after_the_pause() {
    let (runner, prompts, staged) = locke_mill_into_exile(true, &[]);
    assert_eq!(prompts, 1);
    assert_one_shared_cast(runner, &staged, 0);
}

/// Pause on the LAST seat (P0 controls both replacements, P1 chooses): another
/// resume point, with nothing left to drain after the answer.
#[test]
fn a_pause_on_the_last_seat_keeps_both_exiled_spells_and_one_cast() {
    let (runner, prompts, staged) = locke_mill_into_exile_with(P0, true, &[]);
    assert_eq!(prompts, 1, "reach guard: the last seat paused");
    assert_exiled_mill_results(&runner, &staged, 0);
    assert_one_shared_cast(runner, &staged, 0);
}

/// The last-seat pause with a land: the exiled land still creates the Treasure.
#[test]
fn a_last_seat_pause_still_counts_its_exiled_land_for_the_treasure() {
    let (runner, prompts, staged) = locke_mill_into_exile_with(P0, true, &[P1]);
    assert_eq!(prompts, 1);
    assert_exiled_mill_results(&runner, &staged, 1);
}
