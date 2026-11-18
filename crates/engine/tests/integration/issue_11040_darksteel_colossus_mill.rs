//! Regression for issue #11040: Darksteel Colossus milled from a library must
//! be revealed and shuffled into its owner's library instead of going to the
//! graveyard, and non-self "from anywhere" graveyard redirects (Rest in Peace)
//! must exile milled cards while functioning only from their host zones.
//!
//! https://github.com/phase-rs/phase/issues/11040
//!
//! Darksteel Colossus: "Trample / Indestructible / If Darksteel Colossus would
//! be put into a graveyard from anywhere, reveal Darksteel Colossus and
//! shuffle it into its owner's library instead."
//!
//! CR 113.6b: "An ability that states which zones it functions in functions
//! only from those zones."
//!
//! CR 701.17a: "For a player to mill a number of cards, that player puts that
//! many cards from the top of their library into their graveyard."

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

const DARKSTEEL_COLOSSUS: &str = "Trample (This creature can deal excess combat damage to the player or planeswalker it's attacking.)\nIndestructible (Damage and effects that say \"destroy\" don't destroy this creature.)\nIf Darksteel Colossus would be put into a graveyard from anywhere, reveal Darksteel Colossus and shuffle it into its owner's library instead.";

const TOME_SCOUR: &str = "Target player mills five cards.";

const MIND_ROT: &str = "Target player discards two cards.";

const REST_IN_PEACE: &str = "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

fn tome_scour_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Blue],
        generic: 0,
    }
}

fn mind_rot_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Black],
        generic: 2,
    }
}

fn darksteel_colossus_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![],
        generic: 11,
    }
}

fn rest_in_peace_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::White],
        generic: 1,
    }
}

/// Darksteel Colossus in library: Tome Scour mills it, so its self-replacement
/// must function from the library, reveal Darksteel Colossus, and shuffle it
/// into its owner's library while the other four milled cards reach the graveyard.
#[test]
fn darksteel_colossus_milled_from_library_shuffles_back() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P1's library: 10 fillers underneath, then Darksteel Colossus on top (`library[0]`).
    let mut fillers = Vec::new();
    for i in 0..10 {
        fillers.push(scenario.add_card_to_library_top(P1, &format!("Filler {i}")));
    }
    let colossus = scenario
        .add_spell_to_library_top(P1, "Darksteel Colossus", false)
        .as_creature()
        .as_artifact_creature()
        .with_mana_cost(darksteel_colossus_cost())
        .from_oracle_text_with_keywords(&["Trample", "Indestructible"], DARKSTEEL_COLOSSUS)
        .id();

    let tome_scour = scenario
        .add_spell_to_hand_from_oracle(P0, "Tome Scour", false, TOME_SCOUR)
        .with_mana_cost(tome_scour_cost())
        .id();
    scenario.with_mana_pool(P0, floating_mana(1, ManaType::Blue));

    let mut runner = scenario.build();

    assert_eq!(runner.state().objects[&colossus].zone, Zone::Library);
    assert_eq!(runner.state().players[1].library.len(), 11);
    assert_eq!(runner.state().players[1].library[0], colossus);
    assert!(runner.state().players[1].graveyard.is_empty());

    let repl = runner.state().objects[&colossus]
        .replacement_definitions
        .as_slice()
        .iter()
        .find(|d| d.event == ReplacementEvent::Moved)
        .expect("Darksteel Colossus must carry a Moved replacement");
    assert!(
        repl.active_zones.contains(&Zone::Library),
        "active_zones must include Library for 'from anywhere'"
    );

    let outcome = runner.cast(tome_scour).target_player(P1).resolve();

    // Paid real mana cost.
    assert!(
        outcome.events().iter().any(|e| matches!(
            e,
            GameEvent::ManaExpended {
                player_id,
                amount_spent: 1,
                ..
            } if *player_id == P0
        )),
        "Tome Scour must spend its {{U}} mana cost"
    );
    assert_eq!(outcome.mana_pool_total(P0), 0);

    // Reveal post-replacement step (CR 701.20) must fire for Darksteel Colossus.
    assert!(
        outcome.events().iter().any(|e| matches!(
            e,
            GameEvent::CardsRevealed { card_ids, card_names, .. }
                if card_ids.contains(&colossus)
                    && card_names.iter().any(|n| n == "Darksteel Colossus")
        )),
        "Darksteel Colossus replacement must reveal Darksteel Colossus (CR 701.20); events: {:?}",
        outcome.events()
    );

    // Shuffle post-replacement step (CR 701.24) must shuffle its owner's (P1's) library.
    assert!(
        outcome.events().iter().any(|e| matches!(
            e,
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } if *player_id == P1
        )),
        "Darksteel Colossus replacement must shuffle P1's library (CR 701.24); events: {:?}",
        outcome.events()
    );
    assert!(
        !outcome.events().iter().any(|e| matches!(
            e,
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } if *player_id == P0
        )),
        "P0's library must not be shuffled"
    );

    // Darksteel Colossus ends in its owner's library, never in the graveyard.
    assert_eq!(
        runner.state().objects[&colossus].zone,
        Zone::Library,
        "Darksteel Colossus must shuffle into its owner's library instead of going to the graveyard"
    );
    assert!(
        !runner.state().players[1].graveyard.contains(&colossus),
        "Darksteel Colossus must not reach the graveyard"
    );
    assert!(
        runner.state().players[1].library.contains(&colossus),
        "Darksteel Colossus must end up in its owner's library"
    );

    // Reach guard: the remaining 4 milled cards (top 4 fillers) went to P1's graveyard.
    assert_eq!(
        runner.state().players[1].graveyard.len(),
        4,
        "4 filler cards should reach P1's graveyard (5 milled - 1 Colossus redirected)"
    );
    assert_eq!(
        runner.state().players[1].library.len(),
        7,
        "P1's library should have 7 cards remaining (11 - 4 milled fillers)"
    );
    // The bottommost 6 fillers stayed in the library (now shuffled).
    assert!(fillers.iter().take(6).all(|id| {
        runner.state().players[1].library.contains(id)
            && !runner.state().players[1].graveyard.contains(id)
    }));

    // Tome Scour resolved to P0's graveyard and left stack empty at Priority.
    assert_eq!(runner.state().objects[&tome_scour].zone, Zone::Graveyard);
    assert_eq!(outcome.zone_count(P0, Zone::Graveyard), 1);
    assert!(runner.state().stack.is_empty());
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "resolution should end at priority, not {:?}",
        outcome.final_waiting_for()
    );
}

/// Sibling off-battlefield path: Darksteel Colossus discarded from hand
/// ("from anywhere") must also be revealed and shuffled into its owner's library.
#[test]
fn darksteel_colossus_discarded_from_hand_shuffles_back() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    for i in 0..5 {
        scenario.add_card_to_library_top(P1, &format!("Library Filler {i}"));
    }
    let hand_filler = scenario.add_card_to_hand(P1, "Hand Filler");
    let colossus = scenario
        .add_creature_to_hand_from_oracle(P1, "Darksteel Colossus", 11, 11, DARKSTEEL_COLOSSUS)
        .as_artifact_creature()
        .with_mana_cost(darksteel_colossus_cost())
        .id();

    let mind_rot = scenario
        .add_spell_to_hand_from_oracle(P0, "Mind Rot", false, MIND_ROT)
        .with_mana_cost(mind_rot_cost())
        .id();
    scenario.with_mana_pool(P0, floating_mana(3, ManaType::Black));

    let mut runner = scenario.build();

    let outcome = runner.cast(mind_rot).target_player(P1).resolve();

    assert!(
        outcome.events().iter().any(|e| matches!(
            e,
            GameEvent::CardsRevealed { card_ids, .. } if card_ids.contains(&colossus)
        )),
        "discarded Darksteel Colossus must be revealed"
    );
    assert!(
        outcome.events().iter().any(|e| matches!(
            e,
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } if *player_id == P1
        )),
        "discarded Darksteel Colossus must shuffle P1's library"
    );
    assert_eq!(runner.state().objects[&colossus].zone, Zone::Library);
    assert!(runner.state().players[1].library.contains(&colossus));
    assert!(runner.state().players[1].hand.is_empty());
    assert_eq!(runner.state().objects[&hand_filler].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().players[1].graveyard,
        vec![hand_filler].into()
    );
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

/// Rest in Peace on the battlefield: its "from anywhere" replacement affects
/// cards moving to a graveyard from ANY zone, including milled cards from a
/// library and resolving spells from the stack.
#[test]
fn rest_in_peace_on_battlefield_exiles_cards_milled_from_library() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let rip = scenario
        .add_enchantment_from_oracle(P0, "Rest in Peace", REST_IN_PEACE)
        .with_mana_cost(rest_in_peace_cost())
        .id();

    let bottom_card = scenario.add_card_to_library_top(P1, "Bottom Card");
    let mut top_five = Vec::new();
    for i in 0..5 {
        top_five.push(scenario.add_card_to_library_top(P1, &format!("Milled {i}")));
    }

    let tome_scour = scenario
        .add_spell_to_hand_from_oracle(P0, "Tome Scour", false, TOME_SCOUR)
        .with_mana_cost(tome_scour_cost())
        .id();
    scenario.with_mana_pool(P0, floating_mana(1, ManaType::Blue));

    let mut runner = scenario.build();

    assert_eq!(runner.state().objects[&rip].zone, Zone::Battlefield);

    let outcome = runner.cast(tome_scour).target_player(P1).resolve();

    assert!(
        outcome.events().iter().any(|e| matches!(
            e,
            GameEvent::ManaExpended {
                player_id,
                amount_spent: 1,
                ..
            } if *player_id == P0
        )),
        "Tome Scour must spend its {{U}} mana cost"
    );

    // Every milled card must be exiled, not put into P1's graveyard.
    for id in &top_five {
        assert_eq!(
            runner.state().objects[id].zone,
            Zone::Exile,
            "Rest in Peace on battlefield must exile milled card {}",
            id.0
        );
    }
    assert!(
        runner.state().players[1].graveyard.is_empty(),
        "no milled card may reach P1's graveyard while Rest in Peace is on the battlefield"
    );
    assert_eq!(
        runner.state().players[1].library,
        vec![bottom_card].into(),
        "reach guard: exactly 5 cards were milled from P1's library"
    );

    // Resolving Tome Scour itself from the stack is also exiled instead of going to P0's graveyard.
    assert_eq!(runner.state().objects[&tome_scour].zone, Zone::Exile);
    assert!(runner.state().players[0].graveyard.is_empty());
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

/// CR 113.6 + CR 113.6b: Rest in Peace's replacement affects cards moving from
/// anywhere, NOT where Rest in Peace itself functions. When Rest in Peace is
/// in its owner's hand or library (rather than the battlefield), it must have
/// empty `active_zones` and must NOT exile milled cards.
#[test]
fn rest_in_peace_in_hand_or_library_does_not_exile_milled_cards() {
    for host_zone in [Zone::Hand, Zone::Library] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);

        let rip = match host_zone {
            Zone::Hand => scenario
                .add_enchantment_to_hand_from_oracle(P0, "Rest in Peace", REST_IN_PEACE)
                .with_mana_cost(rest_in_peace_cost())
                .id(),
            Zone::Library => scenario
                .add_spell_to_library_top(P0, "Rest in Peace", false)
                .as_enchantment()
                .with_mana_cost(rest_in_peace_cost())
                .from_oracle_text(REST_IN_PEACE)
                .id(),
            other => panic!("unexpected host zone {other:?}"),
        };

        let bottom_card = scenario.add_card_to_library_top(P1, "Bottom Card");
        let mut top_five = Vec::new();
        for i in 0..5 {
            top_five.push(scenario.add_card_to_library_top(P1, &format!("Milled {i}")));
        }

        let tome_scour = scenario
            .add_spell_to_hand_from_oracle(P0, "Tome Scour", false, TOME_SCOUR)
            .with_mana_cost(tome_scour_cost())
            .id();
        scenario.with_mana_pool(P0, floating_mana(1, ManaType::Blue));

        let mut runner = scenario.build();

        assert_eq!(runner.state().objects[&rip].zone, host_zone);
        let rip_repl = runner.state().objects[&rip]
            .replacement_definitions
            .as_slice()
            .iter()
            .find(|d| d.event == ReplacementEvent::Moved)
            .expect("Rest in Peace must carry a Moved replacement");
        assert!(
            rip_repl.active_zones.is_empty(),
            "non-self replacement must keep empty active_zones (CR 113.6b), got {:?}",
            rip_repl.active_zones
        );

        let outcome = runner.cast(tome_scour).target_player(P1).resolve();

        // With Rest in Peace off the battlefield, all 5 milled cards reach P1's graveyard.
        for id in &top_five {
            assert_eq!(
                runner.state().objects[id].zone,
                Zone::Graveyard,
                "milled card must reach graveyard when Rest in Peace is in {host_zone:?}"
            );
        }
        assert_eq!(runner.state().players[1].graveyard.len(), 5);
        assert_eq!(runner.state().players[1].library, vec![bottom_card].into());
        assert!(
            runner.state().exile.is_empty(),
            "nothing may be exiled when Rest in Peace is in {host_zone:?}"
        );
        assert_eq!(runner.state().objects[&tome_scour].zone, Zone::Graveyard);
        assert_eq!(outcome.zone_count(P0, Zone::Graveyard), 1);
        assert!(runner.state().stack.is_empty());
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
    }
}
