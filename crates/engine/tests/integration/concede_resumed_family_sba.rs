//! A public concession that cancels one member of a resolving activated
//! family still owes the ordinary SBA/trigger boundary before priority.

use engine::game::combat::AttackTarget;
use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Effect, ReplacementDefinition, ReplacementMode,
    TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    GameState, LiminalEntryKind, ReplacementChoiceKind, WaitingFor, ZoneDeparture,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::triggers::TriggerMode;
use engine::types::zones::{EtbTapState, Zone};
use engine::types::TriggerDefinition;

const P2: PlayerId = PlayerId(2);

// Verbatim AtomicCards Oracle text, including the loyalty minus signs.
const LILIANA_VESS: &str = "[+1]: Target player discards a card.\n[−2]: Search your library for a card, then shuffle and put that card on top.\n[−8]: Put all creature cards from all graveyards onto the battlefield under your control.";
const CLONE: &str =
    "You may have this creature enter as a copy of any creature on the battlefield.";
const WALKING_BALLISTA: &str = "This creature enters with X +1/+1 counters on it.\n{4}: Put a +1/+1 counter on this creature.\nRemove a +1/+1 counter from this creature: It deals 1 damage to any target.";
const SOUL_WARDEN: &str = "Whenever another creature enters, you gain 1 life.";
const THRAGTUSK: &str = "When this creature enters, you gain 5 life.\nWhen this creature leaves the battlefield, create a 3/3 green Beast creature token.";
const BEACON_OF_UNREST: &str = "Put target artifact or creature card from a graveyard onto the battlefield under your control. Shuffle Beacon of Unrest into its owner's library.";
const MOX_DIAMOND: &str = "If this artifact would enter, you may discard a land card instead. If you do, put this artifact onto the battlefield. If you don't, put it into its owner's graveyard.\n{T}: Add one mana of any color.";
const OPEN_THE_VAULTS: &str = "Return all artifact and enchantment cards from all graveyards to the battlefield under their owners' control. (Auras with nothing to enchant remain in graveyards.)";

fn move_count(events: &[GameEvent], object: ObjectId, from: Zone, to: Zone) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::ZoneChanged { object_id, from: Some(origin), to: destination, .. }
                    if *object_id == object && *origin == from && *destination == to
            )
        })
        .count()
}

fn liliana_clone_ballista_scenario() -> (GameScenario, [ObjectId; 5]) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    // Nine loyalty is reachable from her printed five via earlier +1 uses;
    // paying -8 leaves her alive, so no source-loyalty SBA can mask the bug.
    let liliana = scenario
        .add_planeswalker_from_oracle(P0, "Liliana Vess", "Liliana", 9, LILIANA_VESS)
        .id();
    let warden = scenario
        .add_creature_from_oracle(P0, "Soul Warden", 1, 1, SOUL_WARDEN)
        .with_subtypes(vec!["Human", "Cleric"])
        .id();
    // A legal historical Bribery-style entry under P0's control. This is
    // P1's card, but its leave trigger belongs to surviving controller P0.
    // Both it and Soul Warden are legal battlefield models for Clone.
    let thragtusk = scenario
        .add_creature_from_oracle(P1, "Thragtusk", 5, 3, THRAGTUSK)
        .with_subtypes(vec!["Beast"])
        .controlled_by(P0)
        .id();
    // APNAP graveyard order reaches P1's Clone before P2's Ballista.
    let clone = scenario
        .add_creature_to_graveyard(P1, "Clone", 0, 0)
        .with_subtypes(vec!["Shapeshifter"])
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Blue],
        })
        .from_oracle_text(CLONE)
        .id();
    let ballista = scenario
        .add_creature_to_graveyard(P2, "Walking Ballista", 0, 0)
        .as_artifact_creature()
        .with_subtypes(vec!["Construct"])
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::X, ManaCostShard::X],
        })
        .from_oracle_text(WALKING_BALLISTA)
        .id();
    (scenario, [liliana, warden, thragtusk, clone, ballista])
}

/// CR 107.3g: Ballista's X is zero off the stack. CR 704.5f + CR 704.3:
/// the resulting 0/0 dies before the resumed ability's triggers are ordered,
/// not on the next action. CR 603.6c: a stolen Thragtusk leaving with its
/// owner supplies an independently owned departure-prefix trigger, which
/// must not be collected again by the resumed segment's convergence pass.
#[test]
fn liliana_vess_concession_resumes_family_through_sba_before_triggers() {
    let (scenario, [liliana, warden, thragtusk, clone, ballista]) =
        liliana_clone_ballista_scenario();

    let mut runner = scenario.build();
    let outcome = runner.activate(liliana, 2).resolve();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::ReplacementChoice {
                player: P0,
                kind: ReplacementChoiceKind::OptionalBranch,
                ..
            }
        ),
        "reach: Clone's actual optional copy replacement is unanswered: {:?}",
        outcome.final_waiting_for()
    );
    let state = runner.state();
    assert_eq!(state.objects[&liliana].loyalty, Some(1));
    assert_eq!(state.objects[&clone].zone, Zone::Graveyard);
    assert_eq!(state.objects[&ballista].zone, Zone::Graveyard);
    assert_eq!(state.objects[&thragtusk].owner, P1);
    assert_eq!(state.objects[&thragtusk].controller, P0);
    assert_eq!(
        move_count(
            outcome.events(),
            ballista,
            Zone::Graveyard,
            Zone::Battlefield
        ),
        0
    );

    let result = runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("P1 concedes publicly at the actual optional Clone replacement");

    // No priority drive, trigger answer, or other action may precede this
    // assertion: it discriminates convergence from merely resuming the family.
    let state = runner.state();
    assert_eq!(
        state.objects[&ballista].zone,
        Zone::Graveyard,
        "the surviving loyalty family must finish its SBA boundary before Concede returns"
    );
    assert!(state.players[P1.0 as usize].is_eliminated);
    assert!(!state.players[P0.0 as usize].is_eliminated);
    assert!(!state.players[P2.0 as usize].is_eliminated);
    assert!(state.players[P2.0 as usize].graveyard.contains(&ballista));
    assert!(!state.battlefield.contains(&ballista));
    assert_eq!(state.objects[&clone].zone, Zone::Exile);
    assert_eq!(state.objects[&liliana].zone, Zone::Battlefield);
    assert!(state.pending_replacement.is_none());
    assert!(
        state.resolution_stack.is_empty(),
        "stack={:?}",
        state.resolution_stack
    );
    assert!(state.resolving_stack_entry.is_none());
    assert_eq!(result.waiting_for, state.waiting_for);

    // The caller receives the departure prefix and resumed suffix once each.
    assert_eq!(
        move_count(&result.events, clone, Zone::Graveyard, Zone::Exile),
        1
    );
    assert_eq!(
        move_count(&result.events, clone, Zone::Graveyard, Zone::Battlefield),
        0
    );
    assert_eq!(
        move_count(&result.events, thragtusk, Zone::Battlefield, Zone::Exile),
        1
    );
    assert_eq!(
        move_count(&result.events, ballista, Zone::Graveyard, Zone::Battlefield),
        1
    );
    assert_eq!(
        move_count(&result.events, ballista, Zone::Battlefield, Zone::Graveyard),
        1
    );
    assert_eq!(
        result
            .events
            .iter()
            .filter(|event| { matches!(event, GameEvent::PlayerEliminated { player_id: P1 }) })
            .count(),
        1
    );
    let death = result
        .events
        .iter()
        .find_map(|event| match event {
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Battlefield),
                to: Zone::Graveyard,
                record,
                ..
            } if *object_id == ballista => Some(record),
            _ => None,
        })
        .expect("the resumed Ballista actually entered and then died");
    assert_eq!(
        death.controller, P0,
        "Liliana returned it under her controller's control"
    );
    assert_eq!(death.owner, P2, "the SBA uses its independent owner");
    assert_eq!(death.departure, ZoneDeparture::Moved);

    // The real departure observer and the real resumed-entry observer share
    // one orderable batch. Rescanning the prefix duplicates Thragtusk; losing
    // the resumed segment loses Soul Warden. Neither trigger has resolved.
    let WaitingFor::OrderTriggers { player, triggers } = &result.waiting_for else {
        panic!(
            "P0 orders the departure and entry triggers after SBAs: {:?}",
            result.waiting_for
        );
    };
    assert_eq!(*player, P0);
    let mut sources: Vec<_> = triggers.iter().map(|trigger| trigger.source_id).collect();
    sources.sort();
    let mut expected = vec![warden, thragtusk];
    expected.sort();
    assert_eq!(
        sources, expected,
        "each event segment is collected exactly once"
    );
    assert!(
        state.stack.is_empty(),
        "the complete batch still awaits ordering"
    );
    assert_eq!(runner.life(P0), 20);
}

/// CR 800.4b: a surviving owner's card cannot enter under the departed
/// resolving controller. CR 117.2e + CR 608.2m: retiring that exact unanswered
/// delivery must finish the begun family before anybody receives priority.
#[test]
fn liliana_vess_departed_clone_chooser_completes_family_before_priority() {
    let (scenario, [liliana, warden, thragtusk, clone, ballista]) =
        liliana_clone_ballista_scenario();
    let mut runner = scenario.build();
    let outcome = runner.activate(liliana, 2).resolve();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::ReplacementChoice {
                player: P0,
                kind: ReplacementChoiceKind::OptionalBranch,
                ..
            }
        ),
        "reach: the resolving controller owes Clone's actual optional replacement"
    );
    assert_eq!(runner.state().objects[&clone].owner, P1);
    assert_eq!(runner.state().objects[&ballista].owner, P2);
    assert_eq!(runner.state().objects[&clone].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&ballista].zone, Zone::Graveyard);

    let result = runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("the latched Clone chooser concedes through the public action");

    // Inspect the public action's immediate result, with no follow-up answer,
    // priority action, manual SBA pass, or private continuation drain.
    let state = runner.state();
    assert!(state.players[P0.0 as usize].is_eliminated);
    assert!(!state.players[P1.0 as usize].is_eliminated);
    assert!(!state.players[P2.0 as usize].is_eliminated);
    for (object, owner) in [(clone, P1), (ballista, P2)] {
        assert_eq!(state.objects[&object].zone, Zone::Graveyard);
        assert_eq!(state.objects[&object].owner, owner);
        assert_eq!(state.objects[&object].controller, owner);
        assert!(state.players[owner.0 as usize].graveyard.contains(&object));
        assert!(!state.battlefield.contains(&object));
        assert_eq!(
            move_count(&result.events, object, Zone::Graveyard, Zone::Battlefield),
            0,
            "CR 800.4b does not transfer the intended destination controller"
        );
        assert_eq!(
            move_count(&result.events, object, Zone::Graveyard, Zone::Exile),
            0,
            "the surviving owners' cards do not leave with the chooser"
        );
    }
    assert!(state.pending_replacement.is_none());
    assert!(
        state.resolution_stack.is_empty(),
        "stack={:?}",
        state.resolution_stack
    );
    assert!(state.resolving_stack_entry.is_none());
    assert!(state.stack.is_empty());
    assert_eq!(result.waiting_for, state.waiting_for);
    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority { player: P1 | P2 }
    ));
    assert_eq!(
        result
            .events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved { source_id, .. } if *source_id == liliana
            ))
            .count(),
        1,
        "the begun family completes exactly once even though its controller left"
    );
    assert_eq!(
        result
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::PlayerEliminated { player_id: P0 }))
            .count(),
        1
    );
    assert_eq!(
        move_count(&result.events, liliana, Zone::Battlefield, Zone::Exile),
        1
    );
    assert_eq!(
        move_count(&result.events, warden, Zone::Battlefield, Zone::Exile),
        1
    );
    assert_eq!(
        move_count(&result.events, thragtusk, Zone::Battlefield, Zone::Exile),
        1
    );
    assert_eq!(state.objects[&thragtusk].owner, P1);
    assert_eq!(runner.life(P1), 20);
    assert_eq!(runner.life(P2), 20);
}

fn beacon_mox_scenario() -> (GameScenario, [ObjectId; 4]) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let beacon = scenario
        .add_spell_to_hand_from_oracle(P0, "Beacon of Unrest", false, BEACON_OF_UNREST)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
        })
        .id();
    let mox = scenario
        .add_artifact_to_graveyard(P1, "Mox Diamond")
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text(MOX_DIAMOND)
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
    (scenario, [beacon, mox, forest, island])
}

/// CR 104.3a + CR 800.4f + CR 608.2m: a concession that ends a duel must stay
/// terminal even while an accepted entry cost is paused. The unpaid entry is
/// prevented and later members of the same batch never move.
#[test]
fn duel_concession_at_unpaid_mox_cost_stays_terminal() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let vaults = scenario
        .add_spell_to_hand_from_oracle(P0, "Open the Vaults", false, OPEN_THE_VAULTS)
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![ManaCostShard::White, ManaCostShard::White],
        })
        .id();
    let mox = scenario
        .add_artifact_to_graveyard(P1, "Mox Diamond")
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text(MOX_DIAMOND)
        .id();
    let forest = scenario.add_land_to_hand(P1, "Forest").id();
    let island = scenario.add_land_to_hand(P1, "Island").id();
    let _ = island;
    scenario.with_mana_pool(
        P0,
        [
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::White,
            ManaType::White,
        ]
        .into_iter()
        .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect(),
    );
    let mut runner = scenario.build();
    assert_eq!(runner.state().objects[&mox].zone, Zone::Graveyard);
    runner.cast(vaults).resolve();
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("P1 accepts the Mox cost");
    runner
        .act(GameAction::SelectCards {
            cards: vec![forest],
        })
        .expect("P1 selects the discard");
    let result = runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("P1 concedes from the unpaid cost");
    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::GameOver { .. }));
    assert_eq!(result.waiting_for, state.waiting_for);
    assert!(
        !state.battlefield.contains(&mox),
        "the unpaid entry stays off the battlefield"
    );
    assert_eq!(
        move_count(&result.events, mox, Zone::Graveyard, Zone::Battlefield),
        0
    );
}

/// CR 800.4f: the accepted Mox cost pauses on a nested graveyard-redirect
/// ordering prompt. A departure there must settle the unpaid entry, keep the
/// selected land in hand, and leave Mox in its owner's graveyard.
#[test]
fn departed_nested_mox_redirect_ordering_settles_unpaid_before_priority() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let vaults = scenario
        .add_spell_to_hand_from_oracle(P0, "Open the Vaults", false, OPEN_THE_VAULTS)
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![ManaCostShard::White, ManaCostShard::White],
        })
        .id();
    let mox = scenario
        .add_artifact_to_graveyard(P1, "Mox Diamond")
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text(MOX_DIAMOND)
        .id();
    let forest = scenario.add_land_to_hand(P1, "Forest").id();
    let island = scenario.add_land_to_hand(P1, "Island").id();
    let _ = island;
    for (name, description) in [
        (
            "Rest in Peace",
            "If a card would be put into a graveyard from anywhere, exile it instead. (RIP)",
        ),
        (
            "Leyline of the Void",
            "If a card would be put into a graveyard from anywhere, exile it instead. (Leyline)",
        ),
    ] {
        scenario
            .add_creature(P1, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(
                ReplacementDefinition::new(ReplacementEvent::Moved)
                    .destination_zone(Zone::Graveyard)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::ChangeZone {
                            library_shuffle: Default::default(),
                            destination: Zone::Exile,
                            origin: None,
                            target: TargetFilter::SelfRef,
                            owner_library: false,
                            enter_transformed: false,
                            enters_under: None,
                            enter_tapped: EtbTapState::Unspecified,
                            enters_attacking: false,
                            up_to: false,
                            enter_with_counters: vec![],
                            conditional_enter_with_counters: vec![],
                            face_down_profile: None,
                            enters_modified_if: None,
                        },
                    ))
                    .description(description.to_string()),
            );
    }
    scenario.with_mana_pool(
        P0,
        [
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::White,
            ManaType::White,
        ]
        .into_iter()
        .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect(),
    );
    let mut runner = scenario.build();
    runner.cast(vaults).resolve();
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("P1 accepts the Mox cost");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DiscardChoice { player: P1, .. }
        ),
        "P1 must be asked which land to discard: {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![forest],
        })
        .expect("P1 selects the land to discard");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the nested graveyard redirect must park the land's ordering choice: {:?}",
        runner.state().waiting_for
    );
    {
        let pending = runner
            .state()
            .pending_replacement
            .as_ref()
            .expect("the nested land ordering owns the active replacement");
        assert!(
            pending
                .interrupted_entry
                .as_ref()
                .is_some_and(|outer| outer.is_optional && outer.may_cost_paid),
            "the interrupted Mox cost must ride the nested park"
        );
    }
    let result = runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("P1 concedes inside the nested replacement ordering");
    let state = runner.state();
    assert!(state.players[P1.0 as usize].is_eliminated);
    let mox_moves: Vec<_> = result
        .events
        .iter()
        .filter_map(|event| match event {
            GameEvent::ZoneChanged { record, .. } => record
                .trigger_source_context()
                .filter(|context| context.identity.reference.object_id == mox)
                .map(|_| (record.from_zone, record.to_zone)),
            _ => None,
        })
        .collect();
    let land_graveyard_moves: Vec<_> = result
        .events
        .iter()
        .filter_map(|event| match event {
            GameEvent::ZoneChanged { record, .. } => record
                .trigger_source_context()
                .filter(|context| context.identity.reference.object_id == forest)
                .map(|_| (record.from_zone, record.to_zone)),
            _ => None,
        })
        .filter(|(_, to)| *to == Zone::Graveyard)
        .collect();
    // CR 800.4f: the entry is prevented, so Mox never reaches the
    // battlefield, and the accepted discard cost is not paid, so the chosen
    // land is never discarded. Both cards leave with their departing owner,
    // which is why the final zones here are exile rather than graveyard.
    assert!(
        !state.battlefield.contains(&mox),
        "waiting={:?} pending={:?} events={:?}",
        state.waiting_for,
        state.pending_replacement.as_ref().map(|p| (
            &p.proposed,
            p.is_optional,
            p.may_cost_paid,
            p.choice_player
        )),
        mox_moves
    );
    assert!(
        land_graveyard_moves.is_empty(),
        "the unpaid discard must not move the land to a graveyard: {land_graveyard_moves:?}"
    );
    assert!(
        !result.events.iter().any(|event| matches!(
            event,
            GameEvent::Discarded { object_id, .. } if *object_id == forest
        )),
        "the unpaid cost must not report the land as discarded"
    );
    assert!(state.pending_replacement.is_none());
    assert!(
        state.resolution_stack.is_empty(),
        "stack={:?}",
        state.resolution_stack
    );
    assert!(state.resolving_stack_entry.is_none());
    assert!(state.stack.is_empty());
    assert_eq!(
        move_count(&result.events, mox, Zone::Graveyard, Zone::Battlefield),
        0,
        "the unpaid entry must never put Mox onto the battlefield"
    );
    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority { player: P0 | P2 }
    ));
    assert_eq!(result.waiting_for, state.waiting_for);
}

/// CR 800.4f + CR 800.4b + CR 608.2m: a departed player cannot pay an
/// accepted entry cost. Its real discard choice must settle before the begun
/// reanimation family completes, without transferring the intended controller.
#[test]
fn beacon_of_unrest_departed_mox_discard_payer_completes_family_before_priority() {
    let (scenario, [beacon, mox, forest, island]) = beacon_mox_scenario();
    let mut runner = scenario.build();
    let outcome = runner.cast(beacon).target_objects(&[mox]).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::ReplacementChoice {
            player: P0,
            kind: ReplacementChoiceKind::OptionalBranch,
            ..
        }
    ));
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept Mox Diamond's actual optional land-discard cost");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DiscardChoice {
            player: P0,
            count: 1,
            ..
        }
    ));
    assert_eq!(runner.state().objects[&mox].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&forest].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&island].zone, Zone::Hand);

    let result = runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("the real entry-cost payer concedes at the discard sub-choice");

    // No extra action, manual SBA, or private continuation drain.
    let state = runner.state();
    assert!(state.players[P0.0 as usize].is_eliminated);
    assert!(!state.players[P1.0 as usize].is_eliminated);
    assert!(!state.players[P2.0 as usize].is_eliminated);
    assert_eq!(state.objects[&mox].zone, Zone::Graveyard);
    assert_eq!(state.objects[&mox].owner, P1);
    assert_eq!(state.objects[&mox].controller, P1);
    assert!(state.players[P1.0 as usize].graveyard.contains(&mox));
    assert!(
        state.pending_replacement.is_none(),
        "waiting={:?} pending={:?}",
        state.waiting_for,
        state.pending_replacement.as_ref().map(|p| (
            &p.proposed,
            p.is_optional,
            p.may_cost_paid,
            p.choice_player
        ))
    );
    assert!(
        state.resolution_stack.is_empty(),
        "stack={:?}",
        state.resolution_stack
    );
    assert!(state.resolving_stack_entry.is_none());
    assert!(state.stack.is_empty());
    assert_eq!(result.waiting_for, state.waiting_for);
    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority { player: P1 | P2 }
    ));
    assert_eq!(
        move_count(&result.events, mox, Zone::Graveyard, Zone::Battlefield),
        0
    );
    assert_eq!(
        move_count(&result.events, mox, Zone::Graveyard, Zone::Exile),
        0
    );
    for land in [forest, island] {
        assert_eq!(state.objects[&land].zone, Zone::Exile);
        assert_eq!(move_count(&result.events, land, Zone::Hand, Zone::Exile), 1);
        assert_eq!(
            move_count(&result.events, land, Zone::Hand, Zone::Graveyard),
            0,
            "a departed player pays no discard cost"
        );
    }
}

/// CR 118.12 + CR 614.12a + CR 608.2c: the living entry controller pays
/// the real accepted cost, independently of the resolving spell's controller.
/// The original family and the spell's final part finish before priority.
#[test]
fn open_the_vaults_paid_mox_discard_completes_family_before_priority() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let vaults = scenario
        .add_spell_to_hand_from_oracle(P0, "Open the Vaults", false, OPEN_THE_VAULTS)
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![ManaCostShard::White, ManaCostShard::White],
        })
        .id();
    let mox = scenario
        .add_artifact_to_graveyard(P1, "Mox Diamond")
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text(MOX_DIAMOND)
        .id();
    let forest = scenario.add_land_to_hand(P1, "Forest").id();
    let island = scenario.add_land_to_hand(P1, "Island").id();
    scenario.with_mana_pool(
        P0,
        [
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::White,
            ManaType::White,
        ]
        .into_iter()
        .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect(),
    );
    let mut runner = scenario.build();
    let outcome = runner.cast(vaults).resolve();
    assert_eq!(outcome.mana_pool_total(P0), 0);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::ReplacementChoice {
            player: P1,
            kind: ReplacementChoiceKind::OptionalBranch,
            ..
        }
    ));
    assert_eq!(runner.state().objects[&vaults].zone, Zone::Stack);
    assert_eq!(runner.state().objects[&mox].zone, Zone::Graveyard);
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("P1 accepts the real Mox cost during P0's resolving spell");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DiscardChoice {
            player: P1,
            count: 1,
            ..
        }
    ));
    let result = runner
        .act(GameAction::SelectCards {
            cards: vec![forest],
        })
        .expect("P1 selects one of their two real lands to pay the entry cost");

    // Inspect the answer immediately; no extra action or private drain.
    let state = runner.state();
    assert!(state.players.iter().all(|player| !player.is_eliminated));
    assert_eq!(state.objects[&mox].zone, Zone::Battlefield);
    assert_eq!(state.objects[&mox].owner, P1);
    assert_eq!(state.objects[&mox].controller, P1);
    assert!(state.battlefield.contains(&mox));
    assert_eq!(state.objects[&forest].zone, Zone::Graveyard);
    assert!(state.players[P1.0 as usize].graveyard.contains(&forest));
    assert_eq!(state.objects[&island].zone, Zone::Hand);
    assert!(state.players[P1.0 as usize].hand.contains(&island));
    assert_eq!(state.objects[&vaults].zone, Zone::Graveyard);
    assert!(state.players[P0.0 as usize].graveyard.contains(&vaults));
    assert!(
        state.pending_replacement.is_none(),
        "waiting={:?} pending={:?}",
        state.waiting_for,
        state.pending_replacement.as_ref().map(|p| (
            &p.proposed,
            p.is_optional,
            p.may_cost_paid,
            p.choice_player
        ))
    );
    assert!(
        state.resolution_stack.is_empty(),
        "stack={:?}",
        state.resolution_stack
    );
    assert!(state.resolving_stack_entry.is_none());
    assert!(state.stack.is_empty());
    assert_eq!(result.waiting_for, WaitingFor::Priority { player: P0 });
    assert_eq!(result.waiting_for, state.waiting_for);
    assert_eq!(
        move_count(&result.events, mox, Zone::Graveyard, Zone::Battlefield),
        1
    );
    assert_eq!(
        move_count(&result.events, forest, Zone::Hand, Zone::Graveyard),
        1
    );
    assert_eq!(
        move_count(&result.events, island, Zone::Hand, Zone::Graveyard),
        0
    );
    assert_eq!(
        move_count(&result.events, vaults, Zone::Stack, Zone::Graveyard),
        1,
        "Open the Vaults completes its actual final part exactly once"
    );
    assert_eq!(
        result
            .events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::Discarded { player_id: P1, object_id, .. } if *object_id == forest
            ))
            .count(),
        1,
        "the land is actually discarded by P1, not by the family controller"
    );
    assert_eq!(
        result
            .events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::StackResolved { object_id } if *object_id == vaults
            ))
            .count(),
        1,
        "the original begun spell completes exactly once"
    );
}

/// CR 614.12 + CR 800.4b: once the exact entry remains, its hypothetical
/// battlefield back face must not shadow the surviving graveyard card.
/// This is a latent typed DFC/return/optional-redirect witness, not a claim
/// that a printed card has this combination of instructions.
#[test]
fn transformed_entry_departed_chooser_releases_projection_before_priority_and_save() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let entrant = scenario
        .add_creature_to_graveyard(P1, "Latent front", 2, 2)
        .id();
    let latent_move = |target, destination, transformed| Effect::ChangeZone {
        library_shuffle: Default::default(),
        origin: Some(Zone::Graveyard),
        destination,
        target,
        owner_library: false,
        enter_transformed: transformed,
        enters_under: Some(ControllerRef::You),
        enter_tapped: EtbTapState::Unspecified,
        enters_attacking: false,
        up_to: false,
        enter_with_counters: vec![],
        conditional_enter_with_counters: vec![],
        face_down_profile: None,
        enters_modified_if: None,
    };
    scenario
        .add_creature(P2, "Latent optional entry redirect", 1, 1)
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::Moved)
                .mode(ReplacementMode::Optional { decline: None })
                .valid_card(TargetFilter::SpecificObject { id: entrant })
                .destination_zone(Zone::Battlefield)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    latent_move(TargetFilter::SelfRef, Zone::Graveyard, false),
                )),
        );
    let spell = scenario
        .add_spell_to_hand(P0, "Latent transformed return", false)
        .with_ability(latent_move(
            TargetFilter::SpecificObject { id: entrant },
            Zone::Battlefield,
            true,
        ))
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&entrant)
        .unwrap()
        .back_face = Some(BackFaceData {
        is_swap_snapshot: false,
        trigger_printed_origins: vec![],
        name: "Latent back".to_owned(),
        power: Some(3),
        toughness: Some(3),
        loyalty: None,
        printed_loyalty: None,
        defense: None,
        card_types: CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec![],
        },
        mana_cost: Default::default(),
        keywords: vec![],
        abilities: vec![],
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: vec![],
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: vec![],
        casting_options: vec![],
        layout_kind: None,
        parse_warnings: vec![],
    });

    runner.cast(spell).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P0, .. }
    ));
    assert!(matches!(
        runner
            .state()
            .liminal_entries
            .get(&entrant)
            .map(|entry| &entry.kind),
        Some(LiminalEntryKind::TransformedEntry)
    ));
    assert_eq!(
        runner
            .state()
            .entering_or_live_object(entrant)
            .unwrap()
            .name,
        "Latent back"
    );

    runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("the latched entry chooser concedes");
    assert_eq!(runner.state().objects[&entrant].zone, Zone::Graveyard);
    assert_eq!(
        runner
            .state()
            .entering_or_live_object(entrant)
            .unwrap()
            .name,
        "Latent front"
    );
    assert!(!runner.state().liminal_entries.contains_key(&entrant));
    assert!(runner.state().resolution_stack.is_empty());
    assert!(runner.state().pending_replacement.is_none());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    let restored: GameState = serde_json::from_slice(
        &serde_json::to_vec(runner.state()).expect("the settled state serializes"),
    )
    .expect("the settled state restores");
    assert_eq!(
        restored.entering_or_live_object(entrant).unwrap().name,
        "Latent front"
    );
    assert!(!restored.liminal_entries.contains_key(&entrant));
}

/// Verbatim Oracle text for the returning Aura and the surviving artifact.
const PACIFISM: &str = "Enchant creature\nEnchanted creature can't attack or block.";
const DARKSTEEL_INGOT: &str = "Indestructible\n{T}: Add one mana of any color.";

/// CR 303.4f + CR 800.4a + CR 608.2m: a returning Aura that parks on its host
/// pick is the mass family's own pause even though it carries no replacement
/// record. When the pick's controller concedes, the Aura's already-counted
/// entry stays counted, the prompt is dropped, and the surviving member parked
/// behind it still returns before priority.
#[test]
fn open_the_vaults_aura_host_chooser_concession_resumes_surviving_member() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let vaults = scenario
        .add_spell_to_hand_from_oracle(P0, "Open the Vaults", false, OPEN_THE_VAULTS)
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![ManaCostShard::White, ManaCostShard::White],
        })
        .id();
    // CR 303.4f: two legal creature hosts keep the returned Aura on the host
    // pick instead of auto-attaching to a single one.
    scenario.add_creature(P0, "Runeclaw Bear", 2, 2);
    scenario.add_creature(P2, "Grizzly Bears", 2, 2);
    let pacifism = scenario
        .add_enchantment_to_graveyard(P1, "Pacifism")
        .from_oracle_text(PACIFISM)
        .with_subtypes(vec!["Aura"])
        .id();
    // P2 owns the artifact parked behind the Aura; it must survive P1 leaving.
    let ingot = scenario
        .add_artifact_to_graveyard(P2, "Darksteel Ingot")
        .from_oracle_text(DARKSTEEL_INGOT)
        .id();
    scenario.with_mana_pool(
        P0,
        [
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::White,
            ManaType::White,
        ]
        .into_iter()
        .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect(),
    );
    let mut runner = scenario.build();
    {
        // CR 702.5a: the Aura's own Enchant filter is the host restriction
        // authority. Pushed to the live and base lists so a layer flush
        // re-derives it, and only when the Oracle line did not already parse it.
        let needs_enchant = runner.state().objects.get(&pacifism).is_some_and(|obj| {
            !obj.keywords
                .iter()
                .any(|k| matches!(k, Keyword::Enchant(_)))
        });
        if needs_enchant {
            let enchant = Keyword::Enchant(TargetFilter::Typed(TypedFilter::creature()));
            let obj = runner
                .state_mut()
                .objects
                .get_mut(&pacifism)
                .expect("the staged Aura exists");
            obj.keywords.push(enchant.clone());
            obj.base_keywords.push(enchant);
        }
    }

    let outcome = runner.cast(vaults).resolve();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::ReturnAsAuraTarget { player: P1, returned_id, .. }
                if *returned_id == pacifism
        ),
        "the returning Aura parks on P1's host pick; got {:?}",
        outcome.final_waiting_for()
    );
    // CR 303.4f: the host is chosen as the Aura enters, so the entry is still
    // held in its origin zone at the prompt.
    assert_eq!(runner.state().objects[&pacifism].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&ingot].zone, Zone::Graveyard);
    let pending = runner
        .state()
        .active_change_zone_frame()
        .and_then(|frame| frame.pending.as_ref())
        .expect("the mass return stays parked behind the host pick");
    assert_eq!(pending.remaining.len(), 1, "the artifact is still parked");
    assert_eq!(
        move_count(
            outcome.events(),
            pacifism,
            Zone::Graveyard,
            Zone::Battlefield
        ),
        0,
        "the Aura is not delivered before its host pick"
    );
    assert_eq!(
        move_count(outcome.events(), ingot, Zone::Graveyard, Zone::Battlefield),
        0,
        "the parked artifact has not entered before the pick is answered"
    );

    let result = runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("the host pick's controller concedes");
    let state = runner.state();
    assert!(state.players[P1.0 as usize].is_eliminated);
    assert_eq!(
        state.objects[&pacifism].zone,
        Zone::Exile,
        "P1's Aura leaves the game with its owner (CR 800.4a)"
    );
    assert_eq!(
        state.objects[&ingot].zone,
        Zone::Battlefield,
        "the surviving member parked behind the Aura still returns"
    );
    assert_eq!(state.objects[&ingot].controller, P2);
    assert!(state.battlefield.contains(&ingot));
    assert!(state.pending_replacement.is_none());
    assert!(
        state.resolution_stack.is_empty(),
        "stack={:?}",
        state.resolution_stack
    );
    assert!(state.stack.is_empty());
    assert_eq!(state.objects[&vaults].zone, Zone::Graveyard);
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "ordinary priority follows the resumed family; got {:?}",
        state.waiting_for
    );
    assert_eq!(result.waiting_for, state.waiting_for);
    assert_eq!(
        move_count(&result.events, ingot, Zone::Graveyard, Zone::Battlefield),
        1,
        "the artifact enters exactly once during the departure resume"
    );
    assert_eq!(
        result
            .events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::StackResolved { object_id } if *object_id == vaults
            ))
            .count(),
        1,
        "the begun spell completes exactly once after the concession"
    );
}

const OLIVIA_CRIMSON_BRIDE: &str = "Flying, haste\nWhenever Olivia, Crimson Bride attacks, return target creature card from your graveyard to the battlefield tapped and attacking.\nWhen you don't control a legendary Vampire, exile this creature.";

/// CR 104.3a + CR 303.4f: a host-choice concession that ends a duel must not
/// deliver the Aura or any later batch member.
#[test]
fn duel_concession_at_aura_host_choice_stays_terminal() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let vaults = scenario
        .add_spell_to_hand_from_oracle(P0, "Open the Vaults", false, OPEN_THE_VAULTS)
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![ManaCostShard::White, ManaCostShard::White],
        })
        .id();
    let later = scenario
        .add_artifact_to_graveyard(P1, "Darksteel Ingot")
        .from_oracle_text(DARKSTEEL_INGOT)
        .id();
    let pacifism = scenario
        .add_enchantment_to_graveyard(P0, "Pacifism")
        .from_oracle_text(PACIFISM)
        .with_subtypes(vec!["Aura"])
        .id();
    scenario.add_creature(P0, "Runeclaw Bear", 2, 2);
    scenario.add_creature(P1, "Grizzly Bears", 2, 2);
    scenario.with_mana_pool(
        P0,
        [
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::White,
            ManaType::White,
        ]
        .into_iter()
        .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect(),
    );
    let mut runner = scenario.build();
    assert_eq!(
        runner.state().objects[&later].zone,
        Zone::Graveyard,
        "the later batch member starts in the graveyard"
    );
    {
        let needs_enchant = runner.state().objects.get(&pacifism).is_some_and(|obj| {
            !obj.keywords
                .iter()
                .any(|k| matches!(k, Keyword::Enchant(_)))
        });
        if needs_enchant {
            let enchant = Keyword::Enchant(TargetFilter::Typed(TypedFilter::creature()));
            let obj = runner.state_mut().objects.get_mut(&pacifism).unwrap();
            obj.keywords.push(enchant.clone());
            obj.base_keywords.push(enchant);
        }
    }
    runner.cast(vaults).resolve();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReturnAsAuraTarget { player: P0, .. }
        ),
        "host pick parked; got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().objects[&later].zone,
        Zone::Graveyard,
        "later batch member is parked before the host choice; wait={:?}",
        runner.state().waiting_for
    );
    let result = runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("P1 concedes at the host choice");
    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::GameOver { .. }));
    assert_eq!(result.waiting_for, state.waiting_for);
    assert!(!state.battlefield.contains(&pacifism));
    let later_events: Vec<_> = result
        .events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| match event {
            GameEvent::ZoneChanged { record, .. } => record
                .trigger_source_context()
                .filter(|context| context.identity.reference.object_id == later)
                .map(|_| (index, record.from_zone, record.to_zone)),
            _ => None,
        })
        .collect();
    let eliminated_at = result.events.iter().position(
        |event| matches!(event, GameEvent::PlayerEliminated { player_id } if *player_id == P0),
    );
    assert_eq!(
        state.objects[&later].zone,
        Zone::Graveyard,
        "later moves={later_events:?} eliminated_at={eliminated_at:?} wait={:?}",
        state.waiting_for
    );
    assert_eq!(
        move_count(&result.events, later, Zone::Graveyard, Zone::Battlefield),
        0
    );
}

/// CR 508.4 + CR 614.1c: accepting Clone's copy replacement exposes the
/// entering-attacker defender choice. The approved move stays live across that
/// replacement-to-defender handoff and delivers after the answer.
#[test]
fn olivia_clone_replacement_then_defender_answer_delivers() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let olivia = scenario
        .add_creature_from_oracle(P0, "Olivia, Crimson Bride", 3, 4, OLIVIA_CRIMSON_BRIDE)
        .as_legendary()
        .with_subtypes(vec!["Vampire"])
        .id();
    let clone = scenario
        .add_creature_to_graveyard(P0, "Clone", 0, 0)
        .from_oracle_text(CLONE)
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(olivia, AttackTarget::Player(P1))])
        .expect("Olivia attacks P1");
    let mut answered_replacement = false;
    let mut answered_copy = false;
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::EntryAttackTargetChoice {
                player, object_id, ..
            } => {
                assert_eq!(player, P0);
                assert_eq!(object_id, clone);
                assert_eq!(
                    runner.state().objects[&clone].zone,
                    Zone::Graveyard,
                    "the approved entry is still parked"
                );
                break;
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("order Olivia's trigger");
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(clone)),
                    })
                    .expect("choose Clone");
            }
            WaitingFor::ReplacementChoice { .. } if !answered_replacement => {
                answered_replacement = true;
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("accept Clone's copy replacement");
            }
            WaitingFor::CopyTargetChoice { valid_targets, .. } if !answered_copy => {
                answered_copy = true;
                let target = valid_targets
                    .first()
                    .copied()
                    .expect("Clone offers a copy target");
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("choose the copy target");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass toward the trigger");
            }
            other => panic!("unexpected state before the defender choice: {other:?}"),
        }
    }
    runner
        .act(GameAction::ChooseEntryAttackTarget {
            target: AttackTarget::Player(P2),
        })
        .expect("choose a legal defender");
    if let WaitingFor::CopyTargetChoice { valid_targets, .. } = runner.state().waiting_for.clone() {
        let target = valid_targets
            .first()
            .copied()
            .expect("Clone offers a copy target after entry");
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .expect("choose the copy target");
    }
    let state = runner.state();
    let object = state.objects.get(&clone).expect("Clone entered");
    assert_eq!(object.zone, Zone::Battlefield);
    assert!(object.tapped, "Olivia's return enters tapped");
    assert!(
        state.combat.as_ref().is_some_and(|combat| combat
            .attackers
            .iter()
            .any(|attacker| attacker.object_id == clone && attacker.defending_player == P2)),
        "wait={:?} zone={:?} pending_event={:?}",
        state.waiting_for,
        state.objects[&clone].zone,
        state.pending_entry_attack_event
    );
}

/// CR 117.2e: concession from a real Winota defender prompt settles the batch
/// owner and lets the surviving continuation finish.
#[test]
fn winota_concession_settles_batch_before_priority() {
    const WINOTA: &str = "Whenever a non-Human creature you control attacks, look at the top six cards of your library. You may put a Human creature card from among them onto the battlefield tapped and attacking. It gains indestructible until end of turn. Put the rest of the cards on the bottom of your library in a random order.";
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let human = scenario.add_card_to_library_top(P0, "Winota Human");
    for name in ["Filler 1", "Filler 2", "Filler 3", "Filler 4", "Filler 5"] {
        scenario.add_card_to_library_top(P0, name);
    }
    scenario
        .add_creature_from_oracle(P0, "Winota, Joiner of Forces", 4, 4, WINOTA)
        .with_subtypes(vec!["Human", "Warrior"]);
    let attacker = scenario
        .add_creature(P0, "Non-Human Attacker", 2, 2)
        .with_subtypes(vec!["Goblin"])
        .id();
    let mut runner = scenario.build();
    {
        let card = runner.state_mut().objects.get_mut(&human).unwrap();
        card.card_types.core_types = vec![CoreType::Creature];
        card.card_types.subtypes = vec!["Human".to_string()];
        card.base_card_types = card.card_types.clone();
    }
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare the non-Human attacker");
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DigChoice { .. } => break,
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("order Winota's trigger");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass toward the Dig");
            }
            other => panic!("unexpected state before Dig: {other:?}"),
        }
    }
    runner
        .act(GameAction::SelectCards { cards: vec![human] })
        .expect("put the Human onto the battlefield");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::EntryAttackTargetChoice { .. }
    ));
    let result = runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("the Winota chooser concedes");
    let state = runner.state();
    assert!(state.players[P0.0 as usize].is_eliminated);
    assert!(
        state
            .active_batch_delivery()
            .and_then(|batch| batch.paused_current.as_ref())
            .is_none(),
        "the departing member must not stay parked; wait={:?}",
        state.waiting_for
    );
    assert!(state.pending_entry_attack_event.is_none());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty(), "stack={:?}", state.stack);
    assert_eq!(result.waiting_for, state.waiting_for);
    assert!(!state.battlefield.contains(&human));
}

/// CR 303.4f: a stale host answer with a surviving legal host reoffers the
/// remaining hosts instead of treating the entry as unhosted.
#[test]
fn stale_aura_host_reoffers_surviving_host() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let vaults = scenario
        .add_spell_to_hand_from_oracle(P0, "Open the Vaults", false, OPEN_THE_VAULTS)
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![ManaCostShard::White, ManaCostShard::White],
        })
        .id();
    let pacifism = scenario
        .add_enchantment_to_graveyard(P1, "Pacifism")
        .from_oracle_text(PACIFISM)
        .with_subtypes(vec!["Aura"])
        .id();
    let leaving_host = scenario.add_creature(P2, "Leaving Host", 2, 2).id();
    let surviving_host = scenario.add_creature(P0, "Surviving Host", 2, 2).id();
    scenario.with_mana_pool(
        P0,
        [
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::White,
            ManaType::White,
        ]
        .into_iter()
        .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect(),
    );
    let mut runner = scenario.build();
    {
        let enchant = Keyword::Enchant(TargetFilter::Typed(TypedFilter::creature()));
        let obj = runner.state_mut().objects.get_mut(&pacifism).unwrap();
        obj.keywords.push(enchant.clone());
        obj.base_keywords.push(enchant);
    }
    runner.cast(vaults).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReturnAsAuraTarget { player: P1, .. }
    ));
    runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("the third player concedes and removes one host");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(leaving_host)),
        })
        .expect("the stale id is answered");
    match runner.state().waiting_for.clone() {
        WaitingFor::ReturnAsAuraTarget { legal_targets, .. } => {
            assert_eq!(legal_targets, vec![TargetRef::Object(surviving_host)]);
        }
        other => panic!("expected a reoffered host list, got {other:?}"),
    }
    assert_eq!(runner.state().objects[&pacifism].zone, Zone::Graveyard);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(surviving_host)),
        })
        .expect("choose the surviving host");
    let state = runner.state();
    assert_eq!(state.objects[&pacifism].zone, Zone::Battlefield);
    assert_eq!(
        state.objects[&pacifism]
            .attached_to
            .and_then(|target| target.as_object()),
        Some(surviving_host)
    );
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

/// CR 303.4f + 303.4g: the offered host can leave before the chooser answers.
/// The stale id is rejected at entry time and the Aura stays in its origin.
#[test]
fn stale_aura_host_is_rejected_and_aura_remains_in_origin() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let vaults = scenario
        .add_spell_to_hand_from_oracle(P0, "Open the Vaults", false, OPEN_THE_VAULTS)
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![ManaCostShard::White, ManaCostShard::White],
        })
        .id();
    let pacifism = scenario
        .add_enchantment_to_graveyard(P1, "Pacifism")
        .from_oracle_text(PACIFISM)
        .with_subtypes(vec!["Aura"])
        .id();
    // Both hosts belong to the third player, who leaves while P1 chooses.
    let first_host = scenario.add_creature(P2, "First Host", 2, 2).id();
    scenario.add_creature(P2, "Second Host", 2, 2);
    scenario.with_mana_pool(
        P0,
        [
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::White,
            ManaType::White,
        ]
        .into_iter()
        .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect(),
    );
    let mut runner = scenario.build();
    {
        let needs_enchant = runner.state().objects.get(&pacifism).is_some_and(|obj| {
            !obj.keywords
                .iter()
                .any(|k| matches!(k, Keyword::Enchant(_)))
        });
        if needs_enchant {
            let enchant = Keyword::Enchant(TargetFilter::Typed(TypedFilter::creature()));
            let obj = runner.state_mut().objects.get_mut(&pacifism).unwrap();
            obj.keywords.push(enchant.clone());
            obj.base_keywords.push(enchant);
        }
    }
    runner.cast(vaults).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReturnAsAuraTarget { player: P1, .. }
    ));
    let result = runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("the third player concedes, removing both hosts");
    let state = runner.state();
    assert!(state.players[P2.0 as usize].is_eliminated);
    assert_eq!(state.objects[&pacifism].zone, Zone::Graveyard);
    assert!(
        result.events.iter().all(|event| !matches!(
            event,
            GameEvent::ZoneChanged { record, .. }
                if record
                    .trigger_source_context()
                    .is_some_and(|context| context.identity.reference.object_id == pacifism
                        && record.to_zone == Zone::Battlefield)
        )),
        "no battlefield-entry event for the Aura"
    );
    let result = runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(first_host)),
        })
        .expect("the stale host is still a legal-shaped answer");
    let state = runner.state();
    assert_eq!(state.objects[&pacifism].zone, Zone::Graveyard);
    assert!(state.pending_aura_entry.is_none());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(result.waiting_for, state.waiting_for);
}

const SPIKE_FEEDER: &str = "Flash\nThis creature enters with two +1/+1 counters on it.\n{T}: Remove a +1/+1 counter from this creature: You gain 2 life.\nRemove a +1/+1 counter from this creature: Put a +1/+1 counter on target creature.";

/// CR 614.1c + CR 508.4: the approved event parked at the defender choice
/// carries the replacement-granted counters, tap state, attacking status and
/// controller into the resumed delivery.
#[test]
fn olivia_approved_entry_keeps_counters_and_attacking_after_defender_answer() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let olivia = scenario
        .add_creature_from_oracle(P0, "Olivia, Crimson Bride", 3, 4, OLIVIA_CRIMSON_BRIDE)
        .as_legendary()
        .with_subtypes(vec!["Vampire"])
        .id();
    let feeder = scenario
        .add_creature_to_graveyard(P0, "Spike Feeder", 0, 0)
        .from_oracle_text(SPIKE_FEEDER)
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(olivia, AttackTarget::Player(P1))])
        .expect("Olivia attacks P1");
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::EntryAttackTargetChoice {
                player, object_id, ..
            } => {
                assert_eq!(player, P0);
                assert_eq!(object_id, feeder);
                break;
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("order Olivia's trigger");
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(feeder)),
                    })
                    .expect("choose Spike Feeder");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass toward the trigger");
            }
            other => panic!("unexpected state before the choice: {other:?}"),
        }
    }
    runner
        .act(GameAction::ChooseEntryAttackTarget {
            target: AttackTarget::Player(P2),
        })
        .expect("choose a legal defender");
    let state = runner.state();
    let object = state.objects.get(&feeder).expect("Spike Feeder entered");
    assert_eq!(object.zone, Zone::Battlefield);
    assert_eq!(object.controller, P0);
    assert!(object.tapped, "Olivia's return enters tapped");
    assert_eq!(
        object
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or_default(),
        2,
        "the replacement-granted +1/+1 counters survive the defender pause"
    );
    assert!(state.combat.as_ref().is_some_and(|combat| combat
        .attackers
        .iter()
        .any(|attacker| attacker.object_id == feeder && attacker.defending_player == P2)));
    assert!(state.pending_replacement.is_none());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

/// CR 508.4 + CR 608.2m + CR 800.4a: an entering attacker's defender pick is a
/// prompt owned by the family's own paused delivery, not a replacement. When
/// that chooser concedes, the entrant stays on the battlefield without being
/// declared attacking, and the parked family drains to completion before
/// priority instead of stranding.
#[test]
fn olivia_entry_attack_chooser_concession_resumes_parked_family() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let olivia = scenario
        .add_creature_from_oracle(P0, "Olivia, Crimson Bride", 3, 4, OLIVIA_CRIMSON_BRIDE)
        .as_legendary()
        .with_subtypes(vec!["Vampire"])
        .id();
    let bears = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(olivia, AttackTarget::Player(P1))])
        .expect("Olivia attacks P1");

    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::EntryAttackTargetChoice {
                player, object_id, ..
            } => {
                assert_eq!(player, P0);
                assert_eq!(object_id, bears);
                break;
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("order Olivia's attack trigger");
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(bears)),
                    })
                    .expect("choose the graveyard creature card");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority toward Olivia's trigger");
            }
            other => panic!("unexpected state before the entry attack choice: {other:?}"),
        }
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::EntryAttackTargetChoice { .. }
        ),
        "the returned attacker must park on its defender pick"
    );
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Graveyard,
        "CR 508.4: the defender choice is inside the delivery boundary, so the card has not moved"
    );
    assert!(!runner.state().objects[&bears].tapped);
    let paused = runner
        .state()
        .active_change_zone_frame()
        .and_then(|frame| frame.pending.as_ref())
        .and_then(|pending| pending.paused_current.as_ref())
        .expect("the return family stays parked behind the defender pick");
    assert!(
        paused.terminal_completion.is_none(),
        "an undelivered pause carries no terminal outcome"
    );

    let result = runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("the defender pick's controller concedes");
    let state = runner.state();
    assert!(state.players[P0.0 as usize].is_eliminated);
    assert_eq!(
        state.objects[&bears].zone,
        Zone::Exile,
        "the entry never settled (CR 800.4b), then the owner left the game (CR 800.4a)"
    );
    assert_eq!(
        move_count(&result.events, bears, Zone::Graveyard, Zone::Battlefield),
        0,
        "no entry event is produced for a delivery that never settled"
    );
    assert!(state.pending_replacement.is_none());
    assert!(
        state.resolution_stack.is_empty(),
        "stack={:?}",
        state.resolution_stack
    );
    assert!(state.stack.is_empty());
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "ordinary priority follows the drained family; got {:?}",
        state.waiting_for
    );
    assert_eq!(result.waiting_for, state.waiting_for);
}

/// CR 800.4b + CR 508.4: a card owned by a surviving player, returned under the
/// conceding controller's control, must stay in its origin zone when the entry
/// attacker choice is never answered. The choice is inside the delivery
/// boundary, so no entry event exists and the controller sweep cannot exile the
/// survivor's card.
#[test]
fn entry_attack_chooser_concession_keeps_surviving_owners_card_in_origin() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario
        .add_creature_to_graveyard(P1, "Grizzly Bears", 2, 2)
        .id();
    // Latent attack trigger, not a printed card: return P1's graveyard creature
    // under P0's control, tapped and attacking.
    let raider = scenario
        .add_creature(P0, "Latent Raider", 2, 2)
        .with_trigger_definition(TriggerDefinition::new(TriggerMode::Attacks).execute(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    library_shuffle: Default::default(),
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Battlefield,
                    target: TargetFilter::SpecificObject { id: bears },
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: Some(ControllerRef::You),
                    enter_tapped: EtbTapState::Tapped,
                    enters_attacking: true,
                    up_to: false,
                    enter_with_counters: vec![],
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
                },
            ),
        ))
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(raider, AttackTarget::Player(P1))])
        .expect("the latent attacker attacks P1");
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::EntryAttackTargetChoice {
                player, object_id, ..
            } if player == P0 && object_id == bears => break,
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("order the latent attack trigger");
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(bears)),
                    })
                    .expect("choose the surviving player's graveyard creature");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority toward the return trigger");
            }
            other => panic!("unexpected state before the entry attack choice: {other:?}"),
        }
    }
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Graveyard,
        "the entry has not settled, so the card is still in its origin zone"
    );

    let result = runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("the entry chooser concedes");
    let state = runner.state();
    assert_eq!(
        state.objects[&bears].zone,
        Zone::Graveyard,
        "CR 800.4b: the surviving owner's card remains in its current zone"
    );
    assert_eq!(state.objects[&bears].owner, P1);
    assert_eq!(
        move_count(&result.events, bears, Zone::Graveyard, Zone::Battlefield),
        0
    );
    assert!(state.pending_replacement.is_none());
    assert!(
        state.resolution_stack.is_empty(),
        "stack={:?}",
        state.resolution_stack
    );
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

/// CR 508.4: answering the entry-attacker choice delivers the paused entry with
/// the chosen defender — the move happens inside the resume, not before the
/// prompt, and the family completes before priority.
#[test]
fn entry_attack_choice_answer_delivers_with_chosen_defender() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario
        .add_creature_to_graveyard(P1, "Grizzly Bears", 2, 2)
        .id();
    let raider = scenario
        .add_creature(P0, "Latent Raider", 2, 2)
        .with_trigger_definition(TriggerDefinition::new(TriggerMode::Attacks).execute(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    library_shuffle: Default::default(),
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Battlefield,
                    target: TargetFilter::SpecificObject { id: bears },
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: Some(ControllerRef::You),
                    enter_tapped: EtbTapState::Tapped,
                    enters_attacking: true,
                    up_to: false,
                    enter_with_counters: vec![],
                    conditional_enter_with_counters: vec![],
                    face_down_profile: None,
                    enters_modified_if: None,
                },
            ),
        ))
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(raider, AttackTarget::Player(P1))])
        .expect("the latent attacker attacks P1");
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::EntryAttackTargetChoice {
                player, object_id, ..
            } if player == P0 && object_id == bears => break,
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("order the latent attack trigger");
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(bears)),
                    })
                    .expect("choose the graveyard creature");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority toward the return trigger");
            }
            other => panic!("unexpected state before the entry attack choice: {other:?}"),
        }
    }
    assert_eq!(runner.state().objects[&bears].zone, Zone::Graveyard);

    let result = runner
        .act(GameAction::ChooseEntryAttackTarget {
            target: AttackTarget::Player(PlayerId(2)),
        })
        .expect("the chooser picks the second defender");
    let state = runner.state();
    assert_eq!(state.objects[&bears].zone, Zone::Battlefield);
    assert_eq!(state.objects[&bears].owner, P1);
    assert_eq!(state.objects[&bears].controller, P0);
    assert!(state.objects[&bears].tapped);
    assert_eq!(
        move_count(&result.events, bears, Zone::Graveyard, Zone::Battlefield),
        1,
        "the paused entry is delivered exactly once on the answer"
    );
    let combat = state.combat.as_ref().expect("combat remains active");
    let attack = combat
        .attackers
        .iter()
        .find(|attacker| attacker.object_id == bears)
        .expect("the returned creature is attacking");
    assert_eq!(attack.defending_player, PlayerId(2));
    assert!(state.pending_replacement.is_none());
    assert!(state.resolution_stack.is_empty());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}
