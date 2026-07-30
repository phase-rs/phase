//! Runtime (engine-path) regression for Tomb of Annihilation "Oubliette".
//!
//! Oracle: "Discard a card and sacrifice a creature, an artifact, and a land."
//! The room was implemented as "Discard a card" only — the three mandatory
//! sacrifices (CR 701.21a) were silently dropped, so venturing into Oubliette
//! cost the player only a card and never the permanents. This drives the real
//! venture pipeline: with the marker in Trapped Entry (room 0 → branch to
//! [1 Veils of Fear, 2 Oubliette]), a venture + room choice enters Oubliette,
//! and resolving its ability discards the card AND sacrifices one creature, one
//! artifact, and one land. On the old code only the discard happens and the
//! sacrifice assertions below fail — so this discriminates the real behavior.

use engine::game::dungeon::DungeonId;
use engine::game::effects::venture;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{Effect, ResolvedAbility};
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;

#[test]
fn tomb_oubliette_discards_and_sacrifices_three_types_at_runtime() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Exactly one card in hand → "discard a card" auto-discards it.
    let hand_card = scenario.add_card_to_hand(P0, "Discard Fodder");
    // Exactly one eligible permanent of each type → each sacrifice auto-resolves
    // on its single legal choice.
    let creature = scenario.add_creature(P0, "Grazing Bear", 2, 2).id();
    let artifact = scenario.add_creature(P0, "Curio", 0, 0).as_artifact().id();
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();

    // Marker in Trapped Entry (Tomb room 0 → [1 Veils of Fear, 2 Oubliette]).
    {
        let prog = runner.state_mut().dungeon_progress.entry(P0).or_default();
        prog.current_dungeon = Some(DungeonId::TombOfAnnihilation);
        prog.current_room = 0;
    }

    // Venture → branch → choose Oubliette (room 2).
    let venture_ability =
        ResolvedAbility::new(Effect::VentureIntoDungeon, vec![], ObjectId(99), P0);
    let mut events = Vec::new();
    venture::resolve(runner.state_mut(), &venture_ability, &mut events).unwrap();
    runner
        .act(GameAction::ChooseDungeonRoom { room_index: 2 })
        .expect("choosing Oubliette must succeed");
    assert_eq!(
        runner.state().dungeon_progress[&P0].current_room,
        2,
        "venture must enter Oubliette (room 2)"
    );

    // Resolve Oubliette: discard a card, then sacrifice a creature, an artifact,
    // and a land. Drain the stack — each step auto-resolves on its single legal
    // choice, so no interactive input is required.
    for _ in 0..8 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner.resolve_top();
    }

    let graveyard = &runner.state().players[0].graveyard;
    for (id, what) in [
        (hand_card, "the discarded card"),
        (creature, "the sacrificed creature"),
        (artifact, "the sacrificed artifact"),
        (land, "the sacrificed land"),
    ] {
        assert!(
            graveyard.contains(&id),
            "Oubliette must put {what} into the graveyard (the old \"discard only\" \
             room never sacrificed the permanents); graveyard = {graveyard:?}"
        );
    }
    // And the permanents must have left the battlefield.
    let bf = &runner.state().battlefield;
    for id in [creature, artifact, land] {
        assert!(
            !bf.contains(&id),
            "sacrificed permanent {id:?} must no longer be on the battlefield"
        );
    }
}
