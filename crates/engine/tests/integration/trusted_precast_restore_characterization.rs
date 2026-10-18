//! Test-only characterization of trusted pre-cast snapshots through the
//! engine's persisted JSON envelope. The checkpoints here are made at settled
//! priority before casting; this does not claim arbitrary mid-resolution or
//! mid-payment snapshots are restorable.
//! This exercises the native engine JSON path only, not the WASM/client
//! adapter, client privacy projections, or session race behavior.

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{
    CastOfferKind, GameState, PersistedGameState, TrustedGameStateEnvelope, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use rand::RngCore;

const COLLECTED_CONJURING: &str = "Exile the top six cards of your library. You may cast up to two sorcery spells with mana value 3 or less from among them without paying their mana costs. Put the exiled cards not cast this way on the bottom of your library in a random order.";

/// Serialize through the engine's trusted persistence envelope, then restore
/// through the same checked `PersistedGameState` path used by its consumers.
/// `pending_discard_for_cost` is intentionally skipped by serde; these tests
/// take the checkpoint at priority with no pending cost, so they make no claim
/// about restoring that transient mid-cost continuation.
fn trusted_json_at_priority(state: &GameState) -> (String, u128, u32) {
    assert!(
        state.pending_discard_for_cost.is_none(),
        "the checkpoint must be outside the skipped discard-cost continuation"
    );
    let mut exported = state.clone();
    exported.capture_rng_word_pos();
    let rng_word_pos = exported.rng_word_pos;
    assert_ne!(
        rng_word_pos, 0,
        "checkpoint must carry a real nonzero RNG cursor"
    );
    let mut expected_rng = exported.rng.clone();
    let expected_next_rng_output = expected_rng
        .draw(engine::types::game_state::RandomDraw::Outcome)
        .next_u32();

    let json = serde_json::to_string(&TrustedGameStateEnvelope::capture(exported))
        .expect("trusted game state serializes to JSON");
    let wire: serde_json::Value =
        serde_json::from_str(&json).expect("trusted export is valid JSON");
    assert!(
        wire.get("state").is_some(),
        "export must be the trusted envelope"
    );
    assert!(
        wire["state"].get("pending_discard_for_cost").is_none(),
        "the transient cost continuation is intentionally absent from JSON"
    );

    (json, rng_word_pos, expected_next_rng_output)
}

/// Advance the real game RNG through the public library-shuffle resolver on
/// the opponent's library. This leaves the tested player's library setup
/// untouched while giving the trusted checkpoint a nonzero saved cursor.
fn advance_rng_with_public_library_shuffle(runner: &mut GameRunner) {
    let mut events = Vec::new();
    engine::game::library::resolve_and_apply_library_shuffle(runner.state_mut(), P1, &mut events)
        .expect("the public library-shuffle resolver accepts the scenario player");
    assert!(
        !events.is_empty(),
        "the real shuffle publishes its action event"
    );
    assert!(runner.state().rng_word_pos > 0);
    assert_eq!(
        runner.state().rng_word_pos,
        runner.state().rng.get_word_pos()
    );
}

fn restore_trusted_json(json: &str) -> GameState {
    serde_json::from_str::<PersistedGameState>(json)
        .expect("trusted envelope decodes through PersistedGameState")
        .into_game_state()
        .expect("trusted persisted state passes the checked engine restore path")
}

fn player_zone_ids(
    state: &GameState,
    player: engine::types::player::PlayerId,
) -> (Vec<ObjectId>, Vec<ObjectId>, Vec<ObjectId>) {
    let player = state
        .players
        .iter()
        .find(|candidate| candidate.id == player)
        .expect("scenario player exists");
    (
        player.hand.iter().copied().collect(),
        player.library.iter().copied().collect(),
        player.graveyard.iter().copied().collect(),
    )
}

/// Compare ordered zone membership and pending resolution ownership directly;
/// `GameState::PartialEq` does not cover every persistence-relevant field.
fn assert_precast_zones_and_pending_work(pre: &GameState, restored: &GameState) {
    assert_eq!(restored.phase, pre.phase);
    assert_eq!(restored.active_player, pre.active_player);
    assert_eq!(restored.priority_player, pre.priority_player);
    assert_eq!(restored.waiting_for, pre.waiting_for);
    assert_eq!(player_zone_ids(restored, P0), player_zone_ids(pre, P0));
    assert_eq!(player_zone_ids(restored, P1), player_zone_ids(pre, P1));
    for state in [pre, restored] {
        for player_id in [P0, P1] {
            let player = state
                .players
                .iter()
                .find(|candidate| candidate.id == player_id)
                .expect("scenario player exists");
            for id in &player.library {
                assert_eq!(
                    state.objects[id].zone,
                    Zone::Library,
                    "each ordered library member's object zone agrees with its container"
                );
            }
        }
    }
    assert_eq!(
        restored.battlefield.iter().copied().collect::<Vec<_>>(),
        pre.battlefield.iter().copied().collect::<Vec<_>>(),
        "battlefield membership/order returns to the pre-cast snapshot"
    );
    assert_eq!(
        restored.exile.iter().copied().collect::<Vec<_>>(),
        pre.exile.iter().copied().collect::<Vec<_>>()
    );
    assert_eq!(
        restored.command_zone.iter().copied().collect::<Vec<_>>(),
        pre.command_zone.iter().copied().collect::<Vec<_>>()
    );
    assert_eq!(restored.stack.len(), pre.stack.len());
    assert!(
        restored.stack.is_empty(),
        "no stack item remains at this boundary"
    );
    assert!(restored.resolution_stack.is_empty());
    assert!(restored.resolving_stack_entry.is_none());
    assert!(restored.stack_resolution_session.is_none());
    assert!(restored.pending_resolution_completion.is_none());
    assert!(restored.pending_cast.is_none());
    assert!(restored.pending_replacement.is_none());
    assert!(restored.pending_combat_lifelink.is_none());
    assert!(restored.pending_trigger.is_none());
    assert!(restored.pending_trigger_entry.is_none());
    assert!(restored.pending_trigger_order.is_none());
    assert!(restored.pending_trigger_event_batch.is_empty());
    assert!(restored.deferred_triggers.is_empty());
    assert!(restored.pending_activations.is_empty());
    assert!(restored.pending_miracle_offers.is_empty());
    assert!(restored.pending_paradigm_remaining_offers.is_none());
    assert!(restored.pending_spell_cost_reductions.is_empty());
    assert!(restored.pending_next_spell_modifiers.is_empty());
    assert!(restored.pending_cost_move_resume.is_none());
    assert!(restored.pending_deferred_life_cost_resume.is_none());
    assert!(restored.pending_discard_for_cost.is_none());
    assert!(restored.pending_discard_batch.is_none());
    assert!(restored.pending_exile_from_top_until.is_none());
    assert!(restored.pending_mass_library_order_choice.is_none());
    assert!(restored.pending_scoped_library_search.is_none());
    assert!(restored.pending_library_search_delivery.is_none());
    assert!(restored.pending_search_found_batch.is_none());
    assert!(restored.pending_die_roll_instruction.is_none());
    assert!(restored.pending_player_scope_sacrifice_choice.is_none());
    assert!(restored.pending_player_scope_unless_payment.is_none());
    assert!(restored.pending_taps_for_mana_overrides.is_empty());
    assert!(restored.current_triggered_mana_override.is_none());
}

fn assert_rng_restored(restored: &GameState, expected_pos: u128, expected_next: u32) {
    assert_eq!(
        restored.rng_word_pos, expected_pos,
        "the persisted RNG position is outside GameState equality"
    );
    let mut restored_rng = restored.rng.clone();
    assert_eq!(
        restored_rng
            .draw(engine::types::game_state::RandomDraw::Outcome)
            .next_u32(),
        expected_next,
        "the next random output after restore must match the pre-cast stream"
    );
}

fn green_cost(generic: u32) -> ManaCost {
    ManaCost::Cost {
        generic,
        shards: vec![ManaCostShard::Green],
    }
}

#[test]
fn resolved_grizzly_bears_restore_returns_to_the_precast_priority_boundary() {
    let mut scenario = GameScenario::new_n_player(2, 0x32_00_01);
    scenario.at_phase(Phase::PreCombatMain);
    let forest_a = scenario.add_basic_land(P0, ManaColor::Green);
    let forest_b = scenario.add_basic_land(P0, ManaColor::Green);
    let bears = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(green_cost(1))
        .id();
    scenario.with_library_top(
        P1,
        &[
            "RNG witness one",
            "RNG witness two",
            "RNG witness three",
            "RNG witness four",
        ],
    );

    let mut runner = scenario.build();
    advance_rng_with_public_library_shuffle(&mut runner);
    runner
        .act(GameAction::ActivateAbility {
            source_id: forest_a,
            ability_index: 0,
        })
        .expect("tap Forest A and float green before the spell checkpoint");
    assert!(runner.state().objects[&forest_a].tapped);
    assert!(!runner.state().objects[&forest_b].tapped);
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 1);
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Green),
        1
    );

    // Export at the requested pre-cast boundary. The prior Forest activation
    // and floating mana are deliberately part of the checkpoint.
    let precast = runner.state().clone();
    let (json, rng_pos, expected_next_rng) = trusted_json_at_priority(&precast);
    let outcome = runner.cast(bears).resolve();
    let resolved = runner.state();
    assert_eq!(outcome.zone_of(bears), Zone::Battlefield);
    assert_eq!(resolved.objects[&bears].zone, Zone::Battlefield);
    assert!(resolved.objects[&forest_a].tapped);
    assert!(
        resolved.objects[&forest_b].tapped,
        "Forest B pays the generic portion during the Bears cast"
    );
    assert_eq!(resolved.players[P0.0 as usize].mana_pool.total(), 0);
    assert!(resolved.stack.is_empty());

    let restored = restore_trusted_json(&json);
    assert_precast_zones_and_pending_work(&precast, &restored);
    assert_rng_restored(&restored, rng_pos, expected_next_rng);
    assert_eq!(restored.objects[&bears].zone, Zone::Hand);
    assert!(restored.objects[&forest_a].tapped);
    assert!(!restored.objects[&forest_b].tapped);
    assert_eq!(restored.players[P0.0 as usize].mana_pool.total(), 1);
    assert_eq!(
        restored.players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Green),
        1
    );

    let legal = legal_actions(&restored);
    assert!(
        legal.iter().any(|action| matches!(
            action,
            GameAction::CastSpell { object_id, .. } if *object_id == bears
        )),
        "the restored game must offer Grizzly Bears as a legal next play"
    );
    let mut after_restore = GameRunner::from_state(restored);
    let committed = after_restore.cast(bears).commit();
    assert_eq!(committed.state().objects[&bears].zone, Zone::Stack);
    assert!(committed.state().objects[&forest_b].tapped);
}

#[test]
fn collected_conjuring_decline_restore_recovers_the_precast_state_and_rng() {
    let mut scenario = GameScenario::new_n_player(2, 0x32_95_03);
    scenario.at_phase(Phase::PreCombatMain);
    let payment_lands = [
        scenario.add_basic_land(P0, ManaColor::Red),
        scenario.add_basic_land(P0, ManaColor::Red),
        scenario.add_basic_land(P0, ManaColor::Blue),
        scenario.add_basic_land(P0, ManaColor::Green),
    ];
    let conjuring = scenario
        .add_spell_to_hand_from_oracle(P0, "Collected Conjuring", false, COLLECTED_CONJURING)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Blue, ManaCostShard::Red],
        })
        .id();

    // Match the #9503 shape: the top six are one castable mana-value-1
    // sorcery followed by five lands. The candidate is built from Oracle text
    // and runs through the production free-cast-window rules path.
    let library_lands: Vec<_> = (0..5)
        .map(|_| scenario.add_land_to_library_top(P0, "Forest").id())
        .collect();
    let preordain = scenario
        .add_spell_to_library_top(P0, "Preordain", false)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Blue],
        })
        .from_oracle_text("Scry 2. Draw a card.")
        .id();
    let followup = scenario
        .add_creature_to_hand(P0, "Restore Followup Creature", 1, 1)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    scenario.with_library_top(
        P1,
        &[
            "RNG witness one",
            "RNG witness two",
            "RNG witness three",
            "RNG witness four",
        ],
    );

    let mut runner = scenario.build();
    advance_rng_with_public_library_shuffle(&mut runner);
    let precast = runner.state().clone();
    let (json, rng_pos, expected_next_rng) = trusted_json_at_priority(&precast);
    let mut committed = runner.cast(conjuring).commit();
    assert_eq!(committed.state().objects[&conjuring].zone, Zone::Stack);
    assert!(payment_lands
        .iter()
        .all(|id| committed.state().objects[id].tapped));
    committed
        .act(GameAction::PassPriority)
        .expect("caster passes priority");
    committed
        .act(GameAction::PassPriority)
        .expect("opponent passes and Collected Conjuring resolves");

    match &committed.state().waiting_for {
        WaitingFor::CastOffer {
            player: P0,
            kind: CastOfferKind::FreeCastWindow { candidates, .. },
        } => assert!(
            candidates.contains(&preordain),
            "reach guard: the mana-value-1 sorcery must be offered; candidates={candidates:?}"
        ),
        other => panic!("expected Collected Conjuring's free-cast window, got {other:?}"),
    }
    committed
        .act(GameAction::FreeCastWindowChoice { selection: None })
        .expect("decline the child free cast");

    let after_decline = committed.state();
    let parent_on_stack = after_decline
        .stack
        .iter()
        .any(|entry| entry.id == conjuring);
    let orphan_reproduced =
        after_decline.objects[&conjuring].zone == Zone::Stack && !parent_on_stack;
    assert!(
        !matches!(after_decline.waiting_for, WaitingFor::CastOffer { .. }),
        "declining the child spell must leave the real free-cast prompt"
    );
    let uncast_cards: Vec<_> = std::iter::once(preordain)
        .chain(library_lands.iter().copied())
        .collect();
    let mut expected_uncast_ids = uncast_cards.clone();
    expected_uncast_ids.sort_unstable();
    let mut exiled_ids: Vec<_> = after_decline.exile.iter().copied().collect();
    exiled_ids.sort_unstable();
    let pinned_orphan_signature = orphan_reproduced
        && after_decline.stack.is_empty()
        && exiled_ids == expected_uncast_ids
        && after_decline.players[P0.0 as usize].library.is_empty();
    println!(
        "Current-build #9503 observation: orphan_signature={pinned_orphan_signature}, conjuring_zone={:?}, parent_on_stack={parent_on_stack}, stack_len={}, exiled_revealed_cards={}, library_len={}",
        after_decline.objects[&conjuring].zone,
        after_decline.stack.len(),
        exiled_ids.len(),
        after_decline.players[P0.0 as usize].library.len()
    );

    let restored = restore_trusted_json(&json);
    assert_precast_zones_and_pending_work(&precast, &restored);
    assert_rng_restored(&restored, rng_pos, expected_next_rng);
    assert_eq!(restored.objects[&conjuring].zone, Zone::Hand);
    assert_eq!(restored.objects[&preordain].zone, Zone::Library);
    let restored_p0 = &restored.players[P0.0 as usize];
    assert!(restored_p0.hand.contains(&conjuring));
    assert!(!restored_p0.graveyard.contains(&conjuring));
    assert!(!restored_p0.library.contains(&conjuring));
    assert!(restored_p0.library.contains(&preordain));
    assert_eq!(
        restored.objects[&conjuring].zone,
        Zone::Hand,
        "the restored object zone agrees with Collected Conjuring's hand membership"
    );
    assert_eq!(
        restored.objects[&preordain].zone,
        Zone::Library,
        "the restored object zone agrees with Preordain's library membership"
    );
    for id in payment_lands {
        assert_eq!(restored.objects[&id].zone, Zone::Battlefield);
        assert!(
            !restored.objects[&id].tapped,
            "the trusted pre-cast restore returns every payment land untapped"
        );
    }
    assert_eq!(
        restored_p0.mana_pool, precast.players[P0.0 as usize].mana_pool,
        "the entire pre-cast mana pool is restored after Collected Conjuring's payment"
    );
    for id in &player_zone_ids(&precast, P0).1 {
        assert_eq!(restored.objects[id].zone, Zone::Library);
    }
    assert_eq!(
        player_zone_ids(&restored, P0).1,
        player_zone_ids(&precast, P0).1,
        "the complete ordered library returns to its pre-cast sequence"
    );

    let legal = legal_actions(&restored);
    assert!(
        legal.iter().any(|action| matches!(
            action,
            GameAction::CastSpell { object_id, .. } if *object_id == followup
        )),
        "a different legal spell must be available after restore"
    );
    assert!(
        legal.iter().any(|action| matches!(
            action,
            GameAction::CastSpell { object_id, .. } if *object_id == conjuring
        )),
        "Collected Conjuring itself must also be a legal play after restore"
    );
    let mut after_restore = GameRunner::from_state(restored);
    let followup_outcome = after_restore.cast(followup).resolve();
    assert_eq!(followup_outcome.zone_of(followup), Zone::Battlefield);
    assert_eq!(
        after_restore.state().objects[&followup].zone,
        Zone::Battlefield
    );
    assert_eq!(after_restore.state().objects[&conjuring].zone, Zone::Hand);
    assert_eq!(
        after_restore.state().objects[&preordain].zone,
        Zone::Library
    );
}
