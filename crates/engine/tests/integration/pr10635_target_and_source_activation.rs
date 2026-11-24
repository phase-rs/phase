//! Production controls for source-owner shuffles and properties-only targets.

use engine::game::combat::AttackTarget;
use engine::game::engine::EngineError;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{CastPaymentMode, ReplacementChoiceKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

// Full Oracle verified against MTGJSON AtomicCards.json.
const ETRATA: &str = "Etrata can't be blocked.\nWhenever Etrata deals combat damage to a player, exile target creature that player controls and put a hit counter on that card. That player loses the game if they own three or more exiled cards with hit counters on them. Etrata's owner shuffles Etrata into their library.";
const FORGE: &str = "{T}: Add {C}.\n{T}: Choose target commander that entered this turn. Put a +1/+1 counter on it if it's a creature and a loyalty counter on it if it's a planeswalker.";
const LIVING_LIBRARY: &str = "{6}, Sacrifice this creature: Choose target creature or planeswalker an opponent controls. Its owner shuffles it into their library.";
const DISJUNCTIVE_SHUFFLE: &str =
    "Choose target artifact or enchantment. Its owner shuffles it into their library.";

fn living_library_scenario() -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..7)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    scenario.with_library_top(P0, &["Activator A", "Activator B"]);
    scenario.with_library_top(P1, &["Controller A", "Controller B"]);
    scenario.with_library_top(P2, &["Owner A", "Owner B", "Owner C"]);
    let library = scenario
        .add_creature(P0, "Living Library", 0, 4)
        .as_artifact_creature()
        .with_mana_cost(ManaCost::generic(2))
        .with_subtypes(vec!["Book", "Illusion"])
        .from_oracle_text(LIVING_LIBRARY)
        .id();
    (scenario, library)
}

fn activate_living_library(
    runner: &mut GameRunner,
    library: ObjectId,
    target: ObjectId,
    other_candidate: ObjectId,
) -> (ObjectId, Vec<GameEvent>) {
    let mut events = runner
        .act(GameAction::ActivateAbility {
            source_id: library,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("start Living Library's activation")
        .events;
    let WaitingFor::TargetSelection {
        player,
        target_slots,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected target announcement: {:?}",
            runner.state().waiting_for
        );
    };
    // CR 115.1c + CR 602.2b: the controller chooses one mandatory target
    // from the alternatives before the cost is paid.
    assert_eq!(*player, P0);
    let [slot] = target_slots.as_slice() else {
        panic!("expected one target slot: {target_slots:?}");
    };
    assert!(!slot.optional);
    assert_eq!(slot.legal_targets.len(), 2);
    assert!(slot.legal_targets.contains(&TargetRef::Object(target)));
    assert!(slot
        .legal_targets
        .contains(&TargetRef::Object(other_candidate)));
    events.extend(
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .expect("announce the selected target and pay the activation cost")
            .events,
    );
    let state = runner.state();
    let entry = state.stack.last().expect("Living Library's stack entry");
    let activated = entry.ability().expect("the committed activation");
    assert_eq!(activated.targets, [TargetRef::Object(target)]);
    // CR 601.2h + CR 701.21a: pay six mana and sacrifice the source.
    assert_eq!(state.players[P0.0 as usize].mana_pool.total(), 1);
    assert_eq!(state.objects[&library].zone, Zone::Graveyard);
    (entry.id, events)
}

fn library_shufflers(events: &[GameEvent]) -> Vec<PlayerId> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } => Some(*player_id),
            _ => None,
        })
        .collect()
}

#[test]
fn living_library_shuffles_only_the_selected_permanents_owners_library() {
    for select_planeswalker in [false, true] {
        let (mut scenario, library) = living_library_scenario();
        let creature = scenario
            .add_creature(P2, "Stolen creature", 2, 2)
            .controlled_by(P1)
            .id();
        let planeswalker = scenario
            .add_planeswalker_from_oracle(P2, "Stolen planeswalker", "Jace", 3, "")
            .controlled_by(P1)
            .id();
        let own_creature = scenario.add_creature(P0, "Own creature", 2, 2).id();
        let own_planeswalker = scenario
            .add_planeswalker_from_oracle(P0, "Own planeswalker", "Chandra", 3, "")
            .id();
        let other_artifact = scenario
            .add_artifact_from_oracle(P1, "Noncreature artifact", "")
            .id();
        let mut runner = scenario.build();
        let untouched =
            [P0, P1].map(|player| runner.state().players[player.0 as usize].library.clone());
        let (target, other_candidate) = if select_planeswalker {
            (planeswalker, creature)
        } else {
            (creature, planeswalker)
        };
        let (_, mut events) =
            activate_living_library(&mut runner, library, target, other_candidate);

        for _ in 0..16 {
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            if runner.state().stack.is_empty() && runner.state().resolution_stack.is_empty() {
                break;
            }
            events.extend(
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority")
                    .events,
            );
        }
        let state = runner.state();
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.stack.is_empty());
        assert!(state.resolution_stack.is_empty());
        // CR 400.3 + CR 701.24a: the chosen permanent enters its owner's
        // library and that owner shuffles once.
        assert_eq!(state.objects[&target].zone, Zone::Library);
        assert!(state.players[P2.0 as usize].library.contains(&target));
        assert_eq!(library_shufflers(&events), [P2]);
        assert_eq!(state.objects[&library].zone, Zone::Graveyard);
        for decoy in [
            other_candidate,
            own_creature,
            own_planeswalker,
            other_artifact,
        ] {
            assert_eq!(state.objects[&decoy].zone, Zone::Battlefield);
        }
        for (player, before) in [P0, P1].into_iter().zip(untouched) {
            assert_eq!(state.players[player.0 as usize].library, before);
        }
    }
}

#[test]
fn living_library_does_not_shuffle_after_its_target_becomes_illegal() {
    let (mut scenario, library) = living_library_scenario();
    let target = scenario
        .add_creature(P2, "Stolen creature", 2, 2)
        .controlled_by(P1)
        .id();
    let other_creature = scenario.add_creature(P1, "Other legal creature", 2, 2).id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P1, "Murder", true, "Destroy target creature.")
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
        })
        .id();
    scenario.with_mana_pool(
        P1,
        [ManaType::Colorless, ManaType::Black, ManaType::Black]
            .into_iter()
            .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    let libraries =
        [P0, P1, P2].map(|player| runner.state().players[player.0 as usize].library.clone());
    let (activation, mut events) =
        activate_living_library(&mut runner, library, target, other_creature);
    if matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0, .. }
    ) {
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("pass priority to P1")
                .events,
        );
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1, .. }
    ));
    let response = runner.cast(murder).target_object(target).commit();
    assert_eq!(response.state().stack.len(), 2);
    assert_eq!(response.state().stack.last().unwrap().id, murder);
    let outcome = response.resolve();
    events.extend_from_slice(outcome.events());

    let destroyed = events.iter().position(|event| matches!(
        event,
        GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), to: Zone::Graveyard, .. } if *object_id == target
    )).expect("Murder destroys the original target");
    let finished = events
        .iter()
        .position(|event| {
            matches!(
                event, GameEvent::StackResolved { object_id } if *object_id == activation
            )
        })
        .expect("Living Library's activation leaves the stack");
    assert!(destroyed < finished);
    // CR 608.2b: losing the only target prevents the owner shuffle, even
    // while another valid creature remains on the battlefield.
    assert!(library_shufflers(&events).is_empty());
    let state = outcome.state();
    assert_eq!(state.objects[&target].zone, Zone::Graveyard);
    assert_eq!(state.objects[&other_creature].zone, Zone::Battlefield);
    for (player, before) in [P0, P1, P2].into_iter().zip(libraries) {
        assert_eq!(state.players[player.0 as usize].library, before);
    }
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty());
    assert!(state.resolution_stack.is_empty());
}

#[test]
fn living_library_cannot_be_activated_without_a_legal_target() {
    let (mut scenario, library) = living_library_scenario();
    scenario.add_creature(P0, "Own creature", 2, 2);
    scenario.add_artifact_from_oracle(P1, "Opponent's noncreature artifact", "");
    let mut runner = scenario.build();
    // CR 601.2c + CR 602.2b: announcement needs a legal target before payment.
    let activation = runner.act(GameAction::ActivateAbility {
        source_id: library,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    });
    assert!(
        matches!(activation, Err(EngineError::ActionNotAllowed(ref reason)) if reason == "No legal targets available")
    );
    let state = runner.state();
    assert_eq!(state.players[P0.0 as usize].mana_pool.total(), 7);
    assert_eq!(state.objects[&library].zone, Zone::Battlefield);
    assert!(matches!(
        state.waiting_for,
        WaitingFor::Priority { player: P0, .. }
    ));
    assert!(state.stack.is_empty());
    assert!(state.resolution_stack.is_empty());
}

#[test]
fn disjunctive_owner_shuffle_moves_only_the_announced_object() {
    for (name, is_token) in [("Deglamer", false), ("Unravel the Aether", true)] {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let target = scenario
            .add_artifact_from_oracle(P2, "Stolen artifact", "")
            .controlled_by(P1)
            .id();
        let decoy = scenario
            .add_artifact_from_oracle(P1, "Other legal artifact", "")
            .id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, name, true, DISJUNCTIVE_SHUFFLE)
            .with_mana_cost(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Green],
            })
            .id();
        scenario.with_mana_pool(
            P0,
            [ManaType::Colorless, ManaType::Green]
                .into_iter()
                .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
                .collect(),
        );
        scenario.with_library_top(P2, &["Owner A", "Owner B", "Owner C"]);
        let mut runner = scenario.build();
        runner
            .state_mut()
            .objects
            .get_mut(&target)
            .unwrap()
            .is_token = is_token;

        let committed = runner.cast(spell).target_object(target).commit();
        let selected = committed
            .state()
            .stack
            .last()
            .and_then(|entry| entry.ability())
            .expect("the targeted spell is on the stack");
        assert_eq!(selected.targets, [TargetRef::Object(target)]);
        let outcome = committed.resolve();
        let state = outcome.state();
        assert!(outcome.events().iter().any(|event| matches!(event,
            GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), to: Zone::Library, .. } if *object_id == target
        )));
        assert_eq!(library_shufflers(outcome.events()), [P2]);
        assert_eq!(state.objects[&decoy].zone, Zone::Battlefield);
        let owner_library = &state.players[P2.0 as usize].library;
        if is_token {
            // CR 111.2 + CR 704.5d: the creator owns the token. It ceases
            // to exist after the move, and the owner still shuffles.
            assert!(!state.objects.contains_key(&target));
            assert!(!owner_library.contains(&target));
        } else {
            assert_eq!(state.objects[&target].zone, Zone::Library);
            assert!(owner_library.contains(&target));
        }
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.stack.is_empty());
        assert!(state.resolution_stack.is_empty());
    }
}

#[test]
// The unrelated Etrata loss-condition gap is not a support claim here. A third
// player keeps the source-owner instruction observable even if that gap eliminates
// the damaged player; the targeted exile must still precede any elimination.
fn etrata_shuffles_the_source_not_its_earlier_exile_target() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let etrata = scenario
        .add_creature_from_oracle(P2, "Etrata, the Silencer", 3, 5, ETRATA)
        .controlled_by(P0)
        .id();
    let victim = scenario.add_creature(P1, "Exile target", 2, 2).id();
    let other_victim = scenario
        .add_creature(P1, "Other exile candidate", 2, 2)
        .id();
    scenario.with_library_top(P0, &["Controller's card"]);
    scenario.with_library_top(P1, &["Defender's card"]);
    scenario.with_library_top(P2, &["Owner A", "Owner B", "Owner C"]);
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(etrata, AttackTarget::Player(P1))])
        .expect("the controlled Etrata can attack another player");
    runner.combat_damage();
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        runner.declare_blockers(&[]).expect("no blockers declared");
        runner.combat_damage();
    }
    match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection {
            player,
            target_slots,
            ..
        } => {
            assert_eq!(*player, P0);
            assert_eq!(target_slots.len(), 1);
            assert_eq!(target_slots[0].legal_targets.len(), 2);
            assert!(target_slots[0]
                .legal_targets
                .contains(&TargetRef::Object(other_victim)));
            assert!(target_slots[0]
                .legal_targets
                .contains(&TargetRef::Object(victim)));
        }
        other => panic!(
            "Etrata's combat trigger must announce its exile target: {other:?}; phase={:?}",
            runner.state().phase,
        ),
    }
    let mut events = runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(victim)),
        })
        .expect("the damaged player's creature is a legal target")
        .events;
    for _ in 0..16 {
        if runner.state().stack.is_empty() {
            break;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("resolve the trigger")
                .events,
        );
    }
    let exile = events
        .iter()
        .position(|event| {
            matches!(
                event,
                GameEvent::ZoneChanged {
                    object_id,
                    from: Some(Zone::Battlefield),
                    to: Zone::Exile,
                    ..
                } if *object_id == victim
            )
        })
        .expect("the targeted instruction exiles the victim");
    if let Some(loss) = events.iter().position(|event| {
        matches!(
            event,
            GameEvent::PlayerLost { player_id, reasons: _ } | GameEvent::PlayerEliminated { player_id }
                if *player_id == P1
        )
    }) {
        assert!(
            exile < loss,
            "targeted exile must precede any unrelated elimination"
        );
    }
    let state = runner.state();
    // CR 201.5 + CR 400.3: the named source is distinct from the earlier target,
    // and goes to its owner's library even under another player's control.
    assert_eq!(state.objects[&etrata].zone, Zone::Library);
    assert!(state.players[P2.0 as usize].library.contains(&etrata));
    assert!(!state.players[P0.0 as usize].library.contains(&etrata));
    let shuffled_players: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } => Some(*player_id),
            _ => None,
        })
        .collect();
    assert!(
        !shuffled_players.is_empty(),
        "the source owner actually shuffles"
    );
    assert!(shuffled_players.iter().all(|player| *player == P2));
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty());
    assert!(state.resolution_stack.is_empty());
}

#[test]
fn forge_announces_only_commanders_that_entered_this_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let forge = scenario
        .add_land_from_oracle(P0, "Forge of Heroes", FORGE)
        .id();
    let eligible = scenario
        .add_creature(P0, "New commander", 2, 2)
        .commander()
        .with_summoning_sickness()
        .id();
    let other_eligible = scenario
        .add_creature(P1, "Opponent's new commander", 2, 2)
        .commander()
        .with_summoning_sickness()
        .id();
    let old_commander = scenario
        .add_creature(P0, "Old commander", 2, 2)
        .commander()
        .id();
    let ordinary = scenario
        .add_creature(P0, "New noncommander", 2, 2)
        .with_summoning_sickness()
        .id();
    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: forge,
            ability_index: 1,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Forge's targeted ability can be activated");
    // CR 115.1c + CR 903.3: commander designation and entry timing are both
    // meaningful properties even without a card-type or controller restriction.
    match &runner.state().waiting_for {
        WaitingFor::TargetSelection { target_slots, .. } => {
            assert_eq!(target_slots.len(), 1);
            let legal = &target_slots[0].legal_targets;
            assert_eq!(legal.len(), 2);
            assert!(legal.contains(&TargetRef::Object(eligible)));
            assert!(legal.contains(&TargetRef::Object(other_eligible)));
            assert!(!legal.contains(&TargetRef::Object(old_commander)));
            assert!(!legal.contains(&TargetRef::Object(ordinary)));
        }
        other => panic!("Forge must request its properties-only target: {other:?}"),
    }
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(eligible)),
        })
        .expect("the new commander may be selected");
    let announced = runner
        .state()
        .stack
        .last()
        .and_then(|entry| entry.ability())
        .expect("the activation with its selected target is on the stack");
    assert_eq!(announced.targets, vec![TargetRef::Object(eligible)]);
    // This row covers announcement/activation, not the pre-existing unsupported
    // conditional dual-counter tail in the full Oracle.
    runner.advance_until_stack_empty();
    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty());
    assert!(state.resolution_stack.is_empty());
}

#[test]
fn oblation_selected_owner_draws_after_the_owner_shuffle() {
    // Full Oracle verified from MTGJSON; this is the selected-owner counterpart
    // to Gandalf's untargeted source-owner coordinated draw.
    const OBLATION: &str =
        "The owner of target nonland permanent shuffles it into their library, then draws two cards.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario
        .add_creature(P1, "Owner's creature", 2, 2)
        .controlled_by(P0)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Oblation", true, OBLATION)
        .id();
    scenario.with_library_top(P0, &["Controller A", "Controller B", "Controller C"]);
    scenario.with_library_top(P1, &["Owner A", "Owner B", "Owner C", "Owner D", "Owner E"]);
    let mut runner = scenario.build();
    let controller_library = runner.state().players[P0.0 as usize].library.clone();
    let outcome = runner.cast(spell).target_object(victim).resolve();
    outcome.assert_hand_drawn(P1, 2);
    outcome.assert_hand_drawn(P0, 0);
    assert!(outcome.events().iter().any(|event| matches!(
        event,
        GameEvent::ZoneChanged {
            object_id,
            from: Some(Zone::Battlefield),
            to: Zone::Library,
            ..
        } if *object_id == victim
    )));
    let shuffled_players: Vec<_> = outcome
        .events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } => Some(*player_id),
            _ => None,
        })
        .collect();
    assert_eq!(shuffled_players.as_slice(), &[P1]);
    assert_eq!(outcome.state().players[P1.0 as usize].library.len(), 4);
    assert_eq!(
        outcome.state().players[P0.0 as usize].library,
        controller_library
    );
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty());
    assert!(outcome.state().resolution_stack.is_empty());
}

#[test]
fn paid_beacon_keeps_its_source_after_the_mox_entry_discard() {
    // Full Oracle; the public entry-cost pause is the forwarded-result seam.
    const BEACON: &str = "Put target artifact or creature card from a graveyard onto the battlefield under your control. Shuffle Beacon of Unrest into its owner's library.";
    const MOX: &str = "If this artifact would enter, you may discard a land card instead. If you do, put this artifact onto the battlefield. If you don't, put it into its owner's graveyard.\n{T}: Add one mana of any color.";
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let beacon = scenario
        .add_spell_to_hand_from_oracle(P0, "Beacon of Unrest", false, BEACON)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
        })
        .id();
    let mox = scenario
        .add_artifact_to_graveyard(P1, "Mox Diamond")
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text(MOX)
        .id();
    let forest = scenario.add_land_to_hand(P0, "Forest").id();
    let island = scenario.add_land_to_hand(P0, "Island").id();
    scenario.with_mana_pool(
        P0,
        [
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Black,
            ManaType::Black,
        ]
        .into_iter()
        .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect(),
    );
    let mut runner = scenario.build();
    let outcome = runner.cast(beacon).target_object(mox).resolve();
    assert_eq!(outcome.mana_pool_total(P0), 0);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::ReplacementChoice {
            player: P0,
            kind: ReplacementChoiceKind::OptionalBranch,
            ..
        }
    ));
    let mut events = outcome.events().to_vec();
    events.extend(
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("the intended controller accepts Mox Diamond's entry cost")
            .events,
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DiscardChoice {
            player: P0,
            count: 1,
            ..
        }
    ));
    events.extend(
        runner
            .act(GameAction::SelectCards {
                cards: vec![forest],
            })
            .expect("discard the chosen real land")
            .events,
    );
    for _ in 0..16 {
        if runner.state().stack.is_empty()
            && runner.state().resolution_stack.is_empty()
            && runner.state().resolving_stack_entry.is_none()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }
    assert_eq!(runner.state().objects[&mox].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&mox].owner, P1);
    assert_eq!(runner.state().objects[&mox].controller, P0);
    assert_eq!(runner.state().objects[&forest].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&island].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&beacon].zone, Zone::Library);
    let source_move = events
        .iter()
        .position(|event| {
            matches!(event, GameEvent::ZoneChanged {
            object_id, from: Some(Zone::Stack), to: Zone::Library, ..
        } if *object_id == beacon)
        })
        .expect("the printed source, not the returned Mox, enters its library");
    let shuffles: Vec<_> = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } => Some((index, *player_id)),
            _ => None,
        })
        .collect();
    assert_eq!(shuffles.len(), 1);
    assert_eq!(shuffles[0].1, P0);
    assert!(source_move < shuffles[0].0);
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn cathartic_parting_keeps_the_permanents_owner_and_the_caster_shuffles_distinct() {
    const CATHARTIC: &str = "The owner of target artifact or enchantment an opponent controls shuffles it into their library. You may shuffle up to four target cards from your graveyard into your library.";
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let artifact = scenario
        .add_artifact_from_oracle(P2, "Mox Pearl", "{T}: Add {W}.")
        .controlled_by(P1)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let selected = scenario.add_land_to_graveyard(P0, "Forest").id();
    let unselected = scenario.add_land_to_graveyard(P0, "Island").id();
    let other_graveyard = scenario.add_land_to_graveyard(P2, "Plains").id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Cathartic Parting", false, CATHARTIC)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Green],
        })
        .id();
    scenario.with_library_top(P0, &["Caster A", "Caster B"]);
    scenario.with_library_top(P1, &["Controller A", "Controller B"]);
    scenario.with_library_top(P2, &["Owner A", "Owner B"]);
    scenario.with_mana_pool(
        P0,
        [ManaType::Colorless, ManaType::Green]
            .into_iter()
            .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .target_objects(&[artifact, selected])
        .accept_optional()
        .resolve();
    assert_eq!(outcome.mana_pool_total(P0), 0);
    assert_eq!(outcome.state().objects[&artifact].owner, P2);
    assert_eq!(outcome.state().objects[&artifact].zone, Zone::Library);
    assert!(outcome.state().players[P2.0 as usize]
        .library
        .contains(&artifact));
    assert!(!outcome.state().players[P1.0 as usize]
        .library
        .contains(&artifact));
    assert_eq!(outcome.state().objects[&selected].zone, Zone::Library);
    assert!(outcome.state().players[P0.0 as usize]
        .library
        .contains(&selected));
    assert_eq!(outcome.state().objects[&unselected].zone, Zone::Graveyard);
    assert_eq!(
        outcome.state().objects[&other_graveyard].zone,
        Zone::Graveyard
    );
    let shufflers: Vec<_> = outcome
        .events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } => Some(*player_id),
            _ => None,
        })
        .collect();
    assert_eq!(
        shufflers,
        vec![P2, P0],
        "the owner shuffles first, then only the caster shuffles their chosen graveyard card"
    );
    assert_eq!(outcome.state().objects[&spell].zone, Zone::Graveyard);
    assert!(outcome.state().stack.is_empty());
    assert!(outcome.state().resolution_stack.is_empty());
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}
