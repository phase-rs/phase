//! CR 701.20d: "any revealed cards that are reordered stop being revealed and
//! become new objects."
//!
//! Amplifire — "At the beginning of your upkeep, reveal cards from the top of
//! your library until you reveal a creature card. Until your next turn, this
//! creature's base power becomes twice that card's power and its base toughness
//! becomes twice that card's toughness. Put the revealed cards on the bottom
//! of your library in a random order."
//!
//! When the upkeep trigger resolves:
//! 1. The top cards are revealed until a creature card is revealed.
//! 2. A public `CardsRevealed` event is emitted.
//! 3. Amplifire's base power and toughness are set based on the revealed creature.
//! 4. The revealed cards are reordered to the bottom of the library in random order.
//! 5. Under CR 701.20d, the reorder causes those cards to stop being revealed and
//!    retires their old occurrences (bump incarnation).
//! 6. Immediately after settlement, active reveal marks (`revealed_cards`,
//!    `public_revealed_cards`) must be cleared, and seated views (P0, P1) as well
//!    as spectator views (unseated) must project those cards as hidden (`HIDDEN_CARD_NAME`).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::process_triggers;
use engine::game::visibility::{
    filter_state_for_unseated_viewer, filter_state_for_viewer, HIDDEN_CARD_NAME,
};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const AMPLIFIRE_ORACLE: &str = "At the beginning of your upkeep, reveal cards from the top of your library until you reveal a creature card. Until your next turn, this creature's base power becomes twice that card's power and its base toughness becomes twice that card's toughness. Put the revealed cards on the bottom of your library in a random order.";

fn put_library_top(runner: &mut GameRunner, id: ObjectId) {
    let owner = runner.state().objects[&id].owner;
    let zone = runner.state().objects[&id].zone;
    let mut events = Vec::new();
    if zone != Zone::Library {
        engine::game::zones::remove_from_zone(runner.state_mut(), id, zone, owner);
        runner.state_mut().objects.get_mut(&id).unwrap().zone = Zone::Library;
        runner.state_mut().players[owner.0 as usize]
            .library
            .push_back(id);
    }
    engine::game::zones::move_to_library_position(runner.state_mut(), id, true, &mut events);
}

fn arrange_library_top_first(runner: &mut GameRunner, ids: &[ObjectId]) {
    for &id in ids.iter().rev() {
        put_library_top(runner, id);
    }
    let owner = runner.state().objects[&ids[0]].owner;
    let top: Vec<ObjectId> = runner.state().players[owner.0 as usize]
        .library
        .iter()
        .take(ids.len())
        .copied()
        .collect();
    assert_eq!(
        top, ids,
        "library top must be staged in the requested order"
    );
}

#[test]
fn amplifire_reorder_ends_reveal_and_hides_cards_for_all_viewers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);

    let amplifire = scenario
        .add_creature_from_oracle(P0, "Amplifire", 1, 1, AMPLIFIRE_ORACLE)
        .id();

    // Stage library for P0:
    // Top to bottom:
    // 1. Lightning Bolt (Instant, non-creature)
    // 2. Mountain (Land, non-creature)
    // 3. Hill Giant (Creature, 3/3)
    // 4. Island (unrevealed land)
    // 5. Forest (unrevealed land)
    let bolt = scenario.add_spell_to_hand(P0, "Lightning Bolt", true).id();
    let mountain = scenario.add_land_to_library_top(P0, "Mountain").id();
    let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let island = scenario.add_land_to_library_top(P0, "Island").id();
    let forest = scenario.add_land_to_library_top(P0, "Forest").id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    arrange_library_top_first(&mut runner, &[bolt, mountain, giant, island, forest]);

    let incarnation_bolt_before = runner.state().objects[&bolt].incarnation;
    let incarnation_mountain_before = runner.state().objects[&mountain].incarnation;
    let incarnation_giant_before = runner.state().objects[&giant].incarnation;

    // Trigger Amplifire's beginning of upkeep ability
    let events = vec![GameEvent::PhaseChanged {
        phase: Phase::Upkeep,
    }];
    process_triggers(runner.state_mut(), &events);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Amplifire trigger must be on the stack"
    );

    // Pass priority from both players to resolve the upkeep trigger
    let _ = runner.act(GameAction::PassPriority).expect("P0 passes");
    let result = runner.act(GameAction::PassPriority).expect("P1 passes");
    runner.advance_until_stack_empty();

    // 1. Amplifire base P/T became twice Hill Giant's P/T (3/3 -> 6/6)
    let amplifire_obj = &runner.state().objects[&amplifire];
    assert_eq!(
        (amplifire_obj.power, amplifire_obj.toughness),
        (Some(6), Some(6)),
        "Amplifire base P/T must become twice the revealed creature's P/T (6/6)"
    );

    // 2. Verify public CardsRevealed event was emitted during resolution
    let revealed_in_event: Option<Vec<ObjectId>> = result.events.iter().find_map(|e| match e {
        GameEvent::CardsRevealed {
            player, card_ids, ..
        } if *player == P0 => Some(card_ids.clone()),
        _ => None,
    });
    assert!(
        revealed_in_event.is_some(),
        "CardsRevealed event must be emitted during resolution"
    );
    let revealed = revealed_in_event.unwrap();
    assert!(revealed.contains(&bolt));
    assert!(revealed.contains(&mountain));
    assert!(revealed.contains(&giant));
    assert!(!revealed.contains(&island));
    assert!(!revealed.contains(&forest));

    // CR 701.20d + CR 400.2: Filtered response boundary — reveal names are public
    // across both seated viewers (P0, P1) and spectators, while library positions
    // stay hidden (card_ids filtered out of the event for hidden library cards).
    for viewer in [P0, P1] {
        let filtered_events = engine::game::visibility::filter_events_for_viewer(
            &result.events,
            runner.state(),
            viewer,
        );
        let (card_names, card_ids) = filtered_events
            .iter()
            .find_map(|e| match e {
                GameEvent::CardsRevealed {
                    card_names,
                    card_ids,
                    ..
                } => Some((card_names, card_ids)),
                _ => None,
            })
            .expect("CardsRevealed must be present in filtered events");
        // Public reveal names:
        assert!(card_names.contains(&"Lightning Bolt".to_string()));
        assert!(card_names.contains(&"Mountain".to_string()));
        assert!(card_names.contains(&"Hill Giant".to_string()));
        // Library positions stay hidden:
        assert!(
            !card_ids.contains(&bolt),
            "reordered bolt ID must be redacted from filtered event for {viewer:?}"
        );
        assert!(
            !card_ids.contains(&mountain),
            "reordered mountain ID must be redacted from filtered event for {viewer:?}"
        );
        assert!(
            !card_ids.contains(&giant),
            "reordered giant ID must be redacted from filtered event for {viewer:?}"
        );
    }

    // 3. CR 701.20d: All 3 revealed cards were reordered to bottom in random order.
    // They stop being revealed and become new objects (bump incarnation).
    for (id, label, inc_before) in [
        (bolt, "Lightning Bolt", incarnation_bolt_before),
        (mountain, "Mountain", incarnation_mountain_before),
        (giant, "Hill Giant", incarnation_giant_before),
    ] {
        assert!(
            !runner.state().revealed_cards.contains(&id),
            "revealed_cards must not contain {label}"
        );
        assert!(
            !runner.state().public_revealed_cards.contains(&id),
            "public_revealed_cards must not contain {label}"
        );
        assert!(
            !runner.state().holds_stack_bound_reveal(id),
            "holds_stack_bound_reveal must be false for {label}"
        );
        assert!(
            !runner.state().viewer_knows_card_identity(P0, id),
            "P0 must not know card identity for {label}"
        );
        assert!(
            !runner.state().viewer_knows_card_identity(P1, id),
            "P1 must not know card identity for {label}"
        );
        assert!(
            runner.state().objects[&id].incarnation > inc_before,
            "incarnation must be bumped for {label}"
        );
    }

    // 4. Seated and spectator views immediately after settlement:
    // P0 view:
    let p0_view = filter_state_for_viewer(runner.state(), P0);
    assert_eq!(
        p0_view.objects[&bolt].name, HIDDEN_CARD_NAME,
        "P0 view must hide reordered bolt"
    );
    assert_eq!(
        p0_view.objects[&mountain].name, HIDDEN_CARD_NAME,
        "P0 view must hide reordered mountain"
    );
    assert_eq!(
        p0_view.objects[&giant].name, HIDDEN_CARD_NAME,
        "P0 view must hide reordered giant"
    );

    // P1 (opponent) view:
    let p1_view = filter_state_for_viewer(runner.state(), P1);
    assert_eq!(
        p1_view.objects[&bolt].name, HIDDEN_CARD_NAME,
        "P1 view must hide reordered bolt"
    );
    assert_eq!(
        p1_view.objects[&mountain].name, HIDDEN_CARD_NAME,
        "P1 view must hide reordered mountain"
    );
    assert_eq!(
        p1_view.objects[&giant].name, HIDDEN_CARD_NAME,
        "P1 view must hide reordered giant"
    );

    // Spectator (unseated) view:
    let spectator_view = filter_state_for_unseated_viewer(runner.state());
    assert_eq!(
        spectator_view.objects[&bolt].name, HIDDEN_CARD_NAME,
        "Spectator view must hide reordered bolt"
    );
    assert_eq!(
        spectator_view.objects[&mountain].name, HIDDEN_CARD_NAME,
        "Spectator view must hide reordered mountain"
    );
    assert_eq!(
        spectator_view.objects[&giant].name, HIDDEN_CARD_NAME,
        "Spectator view must hide reordered giant"
    );

    // 5. Verify the remaining library cards: Island and Forest are on top
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .library
            .front()
            .copied(),
        Some(island),
        "Island is on top of library"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].library.len(),
        5,
        "All 5 cards remain in library"
    );
}

/// CR 701.20d: the random bottom placement is one reorder of the whole revealed
/// set, even though the pipeline delivers it one card at a time. With the whole
/// library revealed (`[Bolt, Mountain, Giant]`), a sampled order that delivers
/// Hill Giant first leaves it in place on its own delivery; the two later
/// deliveries then carry it to the top. It was reordered all the same, so no
/// viewer may still see it.
///
/// The sampled order comes from the game RNG, so the test walks seeds until it
/// finds one whose final library starts with Hill Giant (the giant was delivered
/// first) and requires that witness to exist.
#[test]
fn amplifire_full_library_permutation_hides_every_reordered_card() {
    let mut shifted_witness_seen = false;
    for seed in 0..64u64 {
        let mut scenario = GameScenario::new_n_player(2, seed);
        scenario.at_phase(Phase::Upkeep);
        scenario.add_creature_from_oracle(P0, "Amplifire", 1, 1, AMPLIFIRE_ORACLE);
        let bolt = scenario.add_spell_to_hand(P0, "Lightning Bolt", true).id();
        let mountain = scenario.add_land_to_library_top(P0, "Mountain").id();
        let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();

        let mut runner = scenario.build();
        runner.state_mut().active_player = P0;
        arrange_library_top_first(&mut runner, &[bolt, mountain, giant]);
        let library_before: Vec<ObjectId> =
            runner.state().players[0].library.iter().copied().collect();
        assert_eq!(
            library_before,
            vec![bolt, mountain, giant],
            "the revealed set must be the whole library"
        );
        let incarnations_before: Vec<u64> = [bolt, mountain, giant]
            .iter()
            .map(|id| runner.state().objects[id].incarnation)
            .collect();

        process_triggers(
            runner.state_mut(),
            &[GameEvent::PhaseChanged {
                phase: Phase::Upkeep,
            }],
        );
        assert_eq!(runner.state().stack.len(), 1, "Amplifire trigger on stack");
        runner.act(GameAction::PassPriority).expect("P0 passes");
        let result = runner.act(GameAction::PassPriority).expect("P1 passes");
        runner.advance_until_stack_empty();

        // Reach guard: the reveal really covered the whole library.
        let revealed: Vec<ObjectId> = result
            .events
            .iter()
            .find_map(|e| match e {
                GameEvent::CardsRevealed { card_ids, .. } => Some(card_ids.clone()),
                _ => None,
            })
            .expect("Amplifire publishes its reveal");
        assert_eq!(revealed, vec![bolt, mountain, giant]);

        let library_after: Vec<ObjectId> =
            runner.state().players[0].library.iter().copied().collect();
        let mut sorted_after = library_after.clone();
        sorted_after.sort();
        let mut sorted_before = library_before.clone();
        sorted_before.sort();
        assert_eq!(sorted_after, sorted_before, "all three stay in the library");
        if library_after.first() == Some(&giant) && library_after != library_before {
            shifted_witness_seen = true;
        }
        if library_after == library_before {
            // The sampled order rebuilt the original library: nothing was reordered.
            continue;
        }

        let p0_view = filter_state_for_viewer(runner.state(), P0);
        let p1_view = filter_state_for_viewer(runner.state(), P1);
        let spectator_view = filter_state_for_unseated_viewer(runner.state());
        for (id, incarnation_before) in [bolt, mountain, giant].into_iter().zip(incarnations_before)
        {
            let state = runner.state();
            assert!(
                !state.revealed_cards.contains(&id)
                    && !state.public_revealed_cards.contains(&id)
                    && !state.holds_stack_bound_reveal(id),
                "seed {seed}: reordered card {id:?} must stop being revealed (order {library_after:?})"
            );
            assert!(
                state.objects[&id].incarnation > incarnation_before,
                "seed {seed}: reordered card {id:?} must become a new object"
            );
            for (label, view) in [
                ("P0", &p0_view),
                ("P1", &p1_view),
                ("spectator", &spectator_view),
            ] {
                assert_eq!(
                    view.objects[&id].name, HIDDEN_CARD_NAME,
                    "seed {seed}: {label} must not see reordered card {id:?} (order {library_after:?})"
                );
            }
        }
    }
    assert!(
        shifted_witness_seen,
        "some seed must deliver Hill Giant first so its position is only shifted"
    );
}

const CALIBRATED_BLAST_ORACLE: &str = "Reveal cards from the top of your library until you reveal a nonland card. Put the revealed cards on the bottom of your library in a random order. When you reveal a nonland card this way, Calibrated Blast deals damage equal to that card's mana value to any target.";
const SOLDIER_OF_FORTUNE_ORACLE: &str = "{R}, {T}: Target player shuffles their library.";

/// CR 701.24a + CR 701.20d: A shuffle of a multi-card library retires active reveal
/// leases (such as Calibrated Blast's stack-bound reflexive trigger lease) even when
/// the shuffle samples the unchanged permutation (identity shuffle).
///
/// In a two-player scenario, P0 casts Calibrated Blast. It reveals a nonland card C
/// and bottoms it, leaving P0's library as [L, C] while the reflexive trigger waits
/// on the stack with an active reveal lease on C.
///
/// While the reflexive waits, P0 activates Soldier of Fortune targeting P0.
/// Across seeds, we find a run where the 2-card shuffle samples the unchanged
/// permutation [L, C]. CR 701.24a governs the shuffle, and CR 701.20d governs reveal
/// retirement. The test asserts that C's reveal lease is retired, its incarnation is
/// bumped, and its name is concealed from both P1 and unseated spectators.
/// The test would fail if retirement required a changed permutation.
#[test]
fn multi_card_identity_shuffle_retires_reveal_lease_and_conceals_library() {
    let mut identity_witness_seen = false;

    for seed in 0..100u64 {
        let mut scenario = GameScenario::new_n_player(2, seed);
        scenario.at_phase(Phase::PreCombatMain);

        let blast = scenario
            .add_spell_to_hand_from_oracle(P0, "Calibrated Blast", true, CALIBRATED_BLAST_ORACLE)
            .id();
        let soldier = scenario
            .add_creature_from_oracle(P0, "Soldier of Fortune", 1, 1, SOLDIER_OF_FORTUNE_ORACLE)
            .id();

        scenario.with_mana_pool(
            P0,
            vec![
                ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
                ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
                ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
                ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
                ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
                ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ],
        );

        // Nonland C over land L: top is nonland, second is land.
        let land = scenario
            .add_spell_to_library_top(P0, "Forest", false)
            .as_land()
            .id();
        let nonland = scenario
            .add_spell_to_library_top(P0, "Three Drop", false)
            .with_mana_cost(ManaCost::generic(3))
            .id();

        let mut runner = scenario.build();

        // Cast Calibrated Blast: reveals C, bottoms it (singleton), puts reflexive on stack.
        runner.cast(blast).commit();

        // Pass priority until TriggerTargetSelection prompt for the reflexive trigger opens.
        for _ in 0..8 {
            match runner.state().waiting_for.clone() {
                WaitingFor::TriggerTargetSelection { .. } => break,
                WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                    runner.act(GameAction::PassPriority).expect("pass priority");
                }
                _ => break,
            }
        }

        let WaitingFor::TriggerTargetSelection { .. } = runner.state().waiting_for.clone() else {
            panic!(
                "expected TriggerTargetSelection, got {:?}",
                runner.state().waiting_for
            );
        };

        // Choose P1 as the target for the reflexive trigger
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(P1)),
            })
            .expect("choose reflexive target");

        // Reach guard: Calibrated Blast reflexive trigger holds a lease on C's occurrence.
        assert!(
            runner.state().holds_stack_bound_reveal(nonland),
            "reach guard: C's occurrence held by stack-bound reveal"
        );

        let library_before: Vec<ObjectId> =
            runner.state().players[0].library.iter().copied().collect();
        assert_eq!(
            library_before,
            vec![land, nonland],
            "library must be [L, C] before shuffle"
        );
        let nonland_incarnation_before = runner.state().objects[&nonland].incarnation;

        // Activate Soldier of Fortune targeting P0 while the reflexive trigger waits.
        runner
            .act(GameAction::ActivateAbility {
                source_id: soldier,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("activate Soldier of Fortune");

        if let WaitingFor::TargetSelection { .. } = runner.state().waiting_for.clone() {
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(P0)),
                })
                .expect("select P0 as target player for Soldier of Fortune");
        }

        assert_eq!(
            runner.state().stack.len(),
            2,
            "Soldier of Fortune ability on stack above reflexive"
        );

        // Pass priority to resolve Soldier of Fortune's ability
        runner.act(GameAction::PassPriority).expect("P0 passes");
        runner.act(GameAction::PassPriority).expect("P1 passes");

        let library_after: Vec<ObjectId> =
            runner.state().players[0].library.iter().copied().collect();

        // Check if the shuffle sampled the identity (unchanged) permutation
        if library_after == library_before {
            identity_witness_seen = true;

            // CR 701.24a + CR 701.20d: Even with an unchanged multi-card permutation,
            // the shuffle MUST retire reveals and make cards new objects.
            let state = runner.state();
            assert!(
                !state.revealed_cards.contains(&nonland)
                    && !state.public_revealed_cards.contains(&nonland)
                    && !state.holds_stack_bound_reveal(nonland),
                "seed {seed}: identity shuffle must retire reveal lease on C (order {library_after:?})"
            );
            assert!(
                state.objects[&nonland].incarnation > nonland_incarnation_before,
                "seed {seed}: identity shuffle must bump incarnation of C"
            );

            // Concealment from opponent (P1) and unseated spectators
            let p1_view = filter_state_for_viewer(state, P1);
            let spectator_view = filter_state_for_unseated_viewer(state);
            assert_eq!(
                p1_view.objects[&nonland].name, HIDDEN_CARD_NAME,
                "seed {seed}: P1 must not see concealed card after identity shuffle"
            );
            assert_eq!(
                spectator_view.objects[&nonland].name, HIDDEN_CARD_NAME,
                "seed {seed}: spectator must not see concealed card after identity shuffle"
            );
            break;
        }
    }

    assert!(
        identity_witness_seen,
        "a multi-card identity shuffle witness must be observed within 100 seeds"
    );
}
