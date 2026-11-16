//! Elixir of Immortality — "{2}, {T}: You gain 5 life. Shuffle this artifact
//! and your graveyard into their owner's library."
//!
//! The graveyard is shuffled as a whole zone: activation must not ask for a
//! graveyard-card target, and every card in the controller's graveyard (plus
//! the Elixir itself) must end up in the library.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 701.24a: to shuffle a library, randomize its cards.
//!   - CR 400.6: a zone change moves the object to the new zone.
//!   - CR 115.1: only words "target" on the ability create targets.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::EffectKind;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::resolved_commands::ResolvedRulesCommand;
use engine::types::zones::Zone;

const ELIXIR: &str = "{2}, {T}: You gain 5 life. Shuffle this artifact and your graveyard into their owner's library.";

#[test]
fn elixir_shuffles_entire_graveyard_without_targeting() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let elixir = scenario
        .add_artifact_from_oracle(P0, "Elixir of Immortality", ELIXIR)
        .id();
    let mine_a = scenario.add_creature_to_graveyard(P0, "Mine A", 1, 1).id();
    let mine_b = scenario.add_creature_to_graveyard(P0, "Mine B", 2, 2).id();
    let theirs = scenario.add_creature_to_graveyard(P1, "Theirs", 3, 3).id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Colorless, ObjectId(9_999), false, vec![]); 2],
    );
    let mut runner = scenario.build();
    let life_before = runner.state().players[0].life;

    // No target selection is supplied: a graveyard-card target slot would stall here.
    let outcome = runner.activate(elixir, 0).resolve();

    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty());

    // The terminal library shuffle must resolve through the production event path.
    let terminal_shuffles = outcome
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Shuffle,
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        terminal_shuffles, 1,
        "the terminal library shuffle must resolve"
    );

    // Verify ownership: each affected owner's library is shuffled (ShuffledLibrary player action).
    // Elixir is owned by P0, graveyard cards are owned by P0 → only P0's library shuffled.
    let shuffled_libraries = outcome
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    player_id,
                    action: PlayerActionKind::ShuffledLibrary,
                    ..
                } if *player_id == P0
            )
        })
        .count();
    assert_eq!(shuffled_libraries, 1, "P0's library must be shuffled");
    // P1's library must NOT be shuffled (no cards owned by P1 moved)
    let p1_shuffled = outcome
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    player_id,
                    action: PlayerActionKind::ShuffledLibrary,
                    ..
                } if *player_id == P1
            )
        })
        .count();
    assert_eq!(p1_shuffled, 0, "P1's library must not be shuffled");

    let state = outcome.state();
    // The Elixir must be in P0's library (its owner's library).
    assert!(state.players[P0.0 as usize].library.contains(&elixir));
    assert_eq!(state.players[0].life, life_before + 5);
    for id in [elixir, mine_a, mine_b] {
        assert_eq!(
            state.objects[&id].zone,
            Zone::Library,
            "{id:?} must be shuffled in"
        );
    }
    assert_eq!(
        state.objects[&theirs].zone,
        Zone::Graveyard,
        "an opponent's graveyard is untouched"
    );
}

/// CR 701.24c + CR 400.3: when the artifact's owner differs from its controller,
/// the terminal shuffle must fan over BOTH owners — the artifact's owner (for
/// the artifact itself) and the graveyard cards' owner (for the graveyard
/// cards) — not just the controller's library.
#[test]
fn elixir_shuffles_all_affected_owners_when_owner_differs_from_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Elixir is owned by P1 but controlled by P0: "your graveyard" is P0's,
    // the artifact goes to P1's library.
    let elixir = scenario
        .add_artifact_from_oracle(P1, "Elixir of Immortality", ELIXIR)
        .controlled_by(P0)
        .id();
    let mine_a = scenario.add_creature_to_graveyard(P0, "Mine A", 1, 1).id();
    let theirs = scenario.add_creature_to_graveyard(P1, "Theirs", 3, 3).id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Colorless, ObjectId(9_999), false, vec![]); 2],
    );
    let mut runner = scenario.build();
    let life_before = runner.state().players[0].life;

    // P0 controls the Elixir, so P0 activates it. No target selection is supplied.
    let outcome = runner.activate(elixir, 0).resolve();

    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty());

    // Both owners' libraries must be shuffled by the terminal fan-out.
    let terminal_shuffles = outcome
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Shuffle,
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        terminal_shuffles, 2,
        "one terminal shuffle per affected owner (P0 graveyard cards + P1 artifact)"
    );

    // Both P0's and P1's libraries must be shuffled (ShuffledLibrary player action).
    for player in [P0, P1] {
        let shuffled_libraries = outcome
            .events()
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    GameEvent::PlayerPerformedAction {
                        player_id,
                        action: PlayerActionKind::ShuffledLibrary,
                        ..
                    } if *player_id == player
                )
            })
            .count();
        assert_eq!(
            shuffled_libraries, 1,
            "{player:?}'s library must be shuffled"
        );
    }

    let state = outcome.state();
    assert_eq!(state.players[0].life, life_before + 5);
    // The artifact goes to ITS owner (P1), not the controller's library.
    assert_eq!(
        state.objects[&elixir].zone,
        Zone::Library,
        "Elixir must go to its owner's (P1's) library"
    );
    // Verify the Elixir is in P1's library.
    assert!(state.players[P1.0 as usize].library.contains(&elixir));
    assert_eq!(
        state.objects[&mine_a].zone,
        Zone::Library,
        "P0's graveyard card must go to P0's library"
    );
    assert_eq!(
        state.objects[&theirs].zone,
        Zone::Graveyard,
        "P1's graveyard card is untouched (not in P0's graveyard)"
    );
}

/// CR 701.24d: even when the graveyard is empty, the owner's library must still
/// be shuffled — "shuffle {possessive} graveyard into their owner's library"
/// shuffles the library regardless of how many cards the graveyard contributes.
/// When the artifact's owner differs from its controller, BOTH owners' libraries
/// must be shuffled (artifact owner + graveyard owner).
#[test]
fn elixir_shuffles_empty_graveyard_owners_library() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Elixir is owned by P1 but controlled by P0: "your graveyard" is P0's,
    // the artifact goes to P1's library. Both libraries must be shuffled.
    let elixir = scenario
        .add_artifact_from_oracle(P1, "Elixir of Immortality", ELIXIR)
        .controlled_by(P0)
        .id();
    // P0's graveyard is deliberately empty — no cards are added to it.
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Colorless, ObjectId(9_999), false, vec![]); 2],
    );
    let mut runner = scenario.build();
    let life_before = runner.state().players[0].life;

    let outcome = runner.activate(elixir, 0).resolve();

    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty());

    // The terminal library shuffle must resolve for BOTH owners (P0 graveyard + P1 artifact).
    let terminal_shuffles = outcome
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Shuffle,
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        terminal_shuffles, 2,
        "one terminal library shuffle must resolve for each affected owner (CR 701.24d)"
    );

    // Both P0's and P1's libraries must be shuffled (ShuffledLibrary player action).
    for player in [P0, P1] {
        let shuffled_libraries = outcome
            .events()
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    GameEvent::PlayerPerformedAction {
                        player_id,
                        action: PlayerActionKind::ShuffledLibrary,
                        ..
                    } if *player_id == player
                )
            })
            .count();
        assert_eq!(
            shuffled_libraries, 1,
            "{player:?}'s library must be shuffled"
        );
    }

    let state = outcome.state();
    assert_eq!(state.players[0].life, life_before + 5);
    // The artifact goes to ITS owner (P1), not the controller's library.
    assert_eq!(
        state.objects[&elixir].zone,
        Zone::Library,
        "Elixir must go to its owner's (P1's) library"
    );
    // Verify the Elixir is in P1's library.
    assert!(state.players[P1.0 as usize].library.contains(&elixir));
}

/// CR 701.24c + CR 400.3: "shuffle ~ and its owner's graveyard into their owner's library"
/// preserves the parsed zone possessor as a typed `TargetFilter::Owner` reference,
/// moving the owner's (P1's) graveyard cards rather than deriving the zone from
/// the controller's (P0's) zone.
#[test]
fn shuffle_source_and_its_owners_graveyard_moves_owners_graveyard() {
    const OWNER_ELIXIR: &str =
        "{2}, {T}: Shuffle ~ and its owner's graveyard into their owner's library.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let artifact = scenario
        .add_artifact_from_oracle(P1, "Owner Elixir", OWNER_ELIXIR)
        .controlled_by(P0)
        .id();
    let p0_card = scenario.add_creature_to_graveyard(P0, "P0 Card", 1, 1).id();
    let p1_card = scenario.add_creature_to_graveyard(P1, "P1 Card", 2, 2).id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Colorless, ObjectId(9_999), false, vec![]); 2],
    );
    let mut runner = scenario.build();

    let outcome = runner.activate(artifact, 0).resolve();

    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty());

    let state = outcome.state();
    // Artifact and P1's graveyard card go to P1's library.
    assert_eq!(state.objects[&artifact].zone, Zone::Library);
    assert_eq!(state.objects[&p1_card].zone, Zone::Library);
    assert!(state.players[P1.0 as usize].library.contains(&artifact));
    assert!(state.players[P1.0 as usize].library.contains(&p1_card));

    // P0's graveyard card remains in P0's graveyard.
    assert_eq!(state.objects[&p0_card].zone, Zone::Graveyard);

    // Only P1's library was shuffled.
    let p1_shuffled = outcome
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    player_id,
                    action: PlayerActionKind::ShuffledLibrary,
                    ..
                } if *player_id == P1
            )
        })
        .count();
    assert_eq!(p1_shuffled, 1, "P1's library must be shuffled");

    let p0_shuffled = outcome
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    player_id,
                    action: PlayerActionKind::ShuffledLibrary,
                    ..
                } if *player_id == P0
            )
        })
        .count();
    assert_eq!(p0_shuffled, 0, "P0's library must not be shuffled");
}
/// CR 701.24d: an empty filtered set still shuffles the named library, without
/// adding the caster's different library to the affected participants.
#[test]
fn repopulate_empty_target_creature_set_shuffles_only_the_target_library() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let repopulate = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Repopulate",
            true,
            "Shuffle all creature cards from target player's graveyard into that player's library.\nCycling {2} ({2}, Discard this card: Draw a card.)",
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();
    for player in [P0, P1] {
        for name in ["First", "Second", "Third"] {
            scenario.add_card_to_library_top(player, &format!("{player:?} {name}"));
        }
    }
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Green, ObjectId(9_999), false, vec![]); 2],
    );
    let mut runner = scenario.build();
    let caster_library = runner.state().players[P0.0 as usize].library.clone();

    let outcome = runner.cast(repopulate).target_player(P1).resolve();

    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty());
    let shuffled_libraries: Vec<_> = outcome
        .state()
        .resolved_rules_journal
        .entries()
        .iter()
        .filter_map(|entry| match &entry.command {
            Some(ResolvedRulesCommand::LibraryShuffle(command)) => Some(command.player),
            _ => None,
        })
        .collect();
    assert_eq!(shuffled_libraries, vec![P1]);
    assert_eq!(
        outcome.state().players[P0.0 as usize].library,
        caster_library
    );
}
