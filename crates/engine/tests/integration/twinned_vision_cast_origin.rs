//! Cast-origin self-replacement and ordinary Commandeer control/retarget contracts.

use engine::ai_support::legal_actions;
use engine::game::casting::{can_cast_object_now, spell_objects_available_to_cast};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastingVariant, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

// Verbatim MTGJSON Oracle text, including the complete flashback cost.
const TWINNED_VISION: &str = "Draw a card. If this spell wasn't cast from your hand, draw two cards instead.\nFlashback—{1}{U/R}{U/R}, Discard a card. (You may cast this card from your graveyard for its flashback cost. Then exile it.)";
const TWINCAST: &str =
    "Copy target instant or sorcery spell. You may choose new targets for the copy.";
const COMMANDEER: &str = "You may exile two blue cards from your hand rather than pay this spell's mana cost.\nGain control of target noncreature spell. You may choose new targets for it. (If that spell is an artifact, enchantment, or planeswalker, the permanent enters under your control.)";
const FESTIVAL_OF_EMBERS: &str = "During your turn, you may cast instant and sorcery spells from your graveyard by paying 1 life in addition to their other costs.\nIf a card or token would be put into your graveyard from anywhere, exile it instead.\n{1}{R}: Sacrifice this enchantment.";

fn pool(colors: &[ManaType]) -> Vec<ManaUnit> {
    colors
        .iter()
        .map(|&color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect()
}

fn stage_library(scenario: &mut GameScenario, player: PlayerId) {
    for index in 0..8 {
        scenario.add_card_to_library_top(player, &format!("Draw witness {index}"));
    }
}

fn add_twinned(scenario: &mut GameScenario, zone: Zone) -> ObjectId {
    let mut card = match zone {
        Zone::Hand => scenario.add_spell_to_hand(P0, "Twinned Vision", true),
        Zone::Graveyard => scenario.add_spell_to_graveyard(P0, "Twinned Vision", true),
        other => panic!("unsupported fixture zone: {other:?}"),
    };
    card.with_mana_cost(ManaCost::Cost {
        shards: vec![ManaCostShard::BlueRed],
        generic: 1,
    })
    .from_oracle_text(TWINNED_VISION)
    .id()
}

/// CR 614.15 + CR 614.6: the hand-cast original performs only its one-card draw.
#[test]
fn twinned_vision_hand_cast_draws_one() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stage_library(&mut scenario, P0);
    let spell = add_twinned(&mut scenario, Zone::Hand);
    scenario.with_mana_pool(P0, pool(&[ManaType::Colorless, ManaType::Red]));
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).resolve();

    outcome.assert_hand_drawn(P0, 1);
    outcome.assert_zone(&[spell], Zone::Graveyard);
    assert!(outcome.state().stack.is_empty());
}

/// CR 702.34a + CR 701.9a: flashback pays mana and the selected discard, then exiles.
/// CR 614.15 + CR 614.6: the two-card self-replacement displaces the original draw.
#[test]
fn twinned_vision_flashback_draws_two_and_pays_discard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stage_library(&mut scenario, P0);
    let spell = add_twinned(&mut scenario, Zone::Graveyard);
    let discard = scenario.add_card_to_hand(P0, "Selected discard");
    let retained = scenario.add_card_to_hand(P0, "Retained card");
    scenario.with_mana_pool(
        P0,
        pool(&[ManaType::Colorless, ManaType::Blue, ManaType::Red]),
    );
    let mut runner = scenario.build();

    let committed = runner
        .cast(spell)
        .casting_variant(CastingVariant::Flashback)
        .pay_cost_with(&[discard])
        .commit();
    assert_eq!(committed.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(committed.state().objects[&discard].zone, Zone::Graveyard);
    assert_eq!(committed.state().objects[&retained].zone, Zone::Hand);
    let outcome = committed.resolve();

    outcome.assert_hand_drawn(P0, 2);
    outcome.assert_zone(&[spell], Zone::Exile);
    outcome.assert_zone(&[discard], Zone::Graveyard);
    outcome.assert_zone(&[retained], Zone::Hand);
}

/// CR 601.2a + CR 702.34a: a later flashback cast records its own graveyard origin.
#[test]
fn twinned_vision_flashback_after_hand_cast_uses_fresh_origin() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stage_library(&mut scenario, P0);
    let spell = add_twinned(&mut scenario, Zone::Hand);
    let discard = scenario.add_card_to_hand(P0, "Recast discard");
    scenario.with_mana_pool(
        P0,
        pool(&[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Blue,
            ManaType::Blue,
            ManaType::Red,
        ]),
    );
    let mut runner = scenario.build();
    let first = runner.cast(spell).resolve();
    first.assert_hand_drawn(P0, 1);
    first.assert_zone(&[spell], Zone::Graveyard);

    let second = runner
        .cast(spell)
        .casting_variant(CastingVariant::Flashback)
        .pay_cost_with(&[discard])
        .resolve();
    second.assert_hand_drawn(P0, 2);
    second.assert_zone(&[spell], Zone::Exile);
    second.assert_zone(&[discard], Zone::Graveyard);
}

/// CR 707.10: an uncast spell copy satisfies "wasn't cast from your hand".
#[test]
fn twinned_vision_hand_cast_copy_draws_three_total() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stage_library(&mut scenario, P0);
    let spell = add_twinned(&mut scenario, Zone::Hand);
    let twincast = scenario
        .add_spell_to_hand_from_oracle(P0, "Twincast", true, TWINCAST)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        pool(&[
            ManaType::Colorless,
            ManaType::Red,
            ManaType::Blue,
            ManaType::Blue,
        ]),
    );
    let mut runner = scenario.build();
    let mut original = runner.cast(spell).commit();
    assert_eq!(original.state().objects[&spell].zone, Zone::Stack);

    // Twinned has no targets: CopyRetarget is unreachable, so resolve directly.
    let outcome = original.cast(twincast).target_object(spell).resolve();
    let copy = outcome
        .events()
        .iter()
        .find_map(|event| match event {
            GameEvent::SpellCopied {
                original_id,
                object_id,
                controller,
                ..
            } if *original_id == spell && *controller == P0 => Some(*object_id),
            _ => None,
        })
        .expect("Twincast must create a copy of the committed Twinned spell");
    assert_ne!(copy, spell, "the copy must have its own source object");
    // CR 707.10a: the distinct copied object ceases to exist after leaving the stack.
    assert!(!outcome.state().objects.contains_key(&copy));
    outcome.assert_hand_drawn(P0, 3);
    outcome.assert_zone(&[spell, twincast], Zone::Graveyard);
    assert!(outcome.state().stack.is_empty());
}

/// CR 109.5 + CR 400.3: "your hand" refers to the resolving controller's hand.
#[test]
fn twinned_vision_stolen_hand_spell_draws_two_for_new_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stage_library(&mut scenario, P0);
    stage_library(&mut scenario, P1);
    let spell = add_twinned(&mut scenario, Zone::Hand);
    let commandeer = scenario
        .add_spell_to_hand_from_oracle(P1, "Commandeer", true, COMMANDEER)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 5,
        })
        .id();
    scenario.with_mana_pool(P0, pool(&[ManaType::Colorless, ManaType::Red]));
    scenario.with_mana_pool(
        P1,
        pool(&[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Blue,
            ManaType::Blue,
        ]),
    );
    let mut runner = scenario.build();
    let mut original = runner.cast(spell).commit();
    assert_eq!(original.state().objects[&spell].owner, P0);
    assert_eq!(original.state().objects[&spell].controller, P0);
    original.act(GameAction::PassPriority).unwrap();
    let mut response = original.cast(commandeer).target_object(spell).commit();
    let mut optional_declined = false;
    let mut control_changed = false;
    for _ in 0..12 {
        if response.state().objects[&commandeer].zone != Zone::Stack
            && matches!(response.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        let result = match response.state().waiting_for {
            WaitingFor::Priority { .. } => response.act(GameAction::PassPriority).unwrap(),
            WaitingFor::OptionalEffectChoice { .. } => {
                optional_declined = true;
                response
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .unwrap()
            }
            ref other => panic!("unexpected Commandeer prompt: {other:?}"),
        };
        control_changed |= result.events.iter().any(|event| {
            matches!(
                event,
                GameEvent::ControllerChanged { object_id, old_controller, new_controller, .. }
                    if *object_id == spell && *old_controller == P0 && *new_controller == P1
            )
        });
    }
    assert!(
        optional_declined,
        "Commandeer's retarget rider must be reached"
    );
    assert!(
        control_changed,
        "Commandeer must emit the spell's control change"
    );
    assert_eq!(response.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(response.state().objects[&spell].owner, P0);
    assert_eq!(response.state().objects[&spell].controller, P1);
    // Commandeer's commit baseline accounts for its departure from P1's hand.
    let outcome = response.resolve();
    outcome.assert_hand_drawn(P1, 2);
    outcome.assert_hand_drawn(P0, 0);
    outcome.assert_zone(&[spell, commandeer], Zone::Graveyard);
}

/// CR 702.34a + CR 601.2h: flashback requires a discard even when mana is available.
#[test]
fn twinned_vision_flashback_requires_a_discardable_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = add_twinned(&mut scenario, Zone::Graveyard);
    scenario.with_mana_pool(
        P0,
        pool(&[ManaType::Colorless, ManaType::Blue, ManaType::Red]),
    );
    let runner = scenario.build();
    assert!(spell_objects_available_to_cast(runner.state(), P0).contains(&spell));
    assert!(!can_cast_object_now(runner.state(), P0, spell));
    assert!(!legal_actions(runner.state()).iter().any(|action| matches!(
        action,
        GameAction::CastSpell { object_id, .. } if *object_id == spell
    )));
}

/// CR 601.2a: another permission still records a graveyard cast, without flashback.
#[test]
fn twinned_vision_nonflashback_graveyard_permission_draws_two() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    stage_library(&mut scenario, P0);
    scenario
        .add_creature(P0, "Festival of Embers", 0, 0)
        .as_enchantment()
        .from_oracle_text(FESTIVAL_OF_EMBERS);
    let spell = add_twinned(&mut scenario, Zone::Graveyard);
    scenario.with_mana_pool(P0, pool(&[ManaType::Colorless, ManaType::Red]));
    let mut runner = scenario.build();
    assert!(runner.state().players[0].hand.is_empty());
    assert!(can_cast_object_now(runner.state(), P0, spell));
    let committed = runner.cast(spell).commit();
    assert_eq!(committed.state().objects[&spell].zone, Zone::Stack);
    let outcome = committed.resolve();
    outcome.assert_life_delta(P0, -1);
    outcome.assert_hand_drawn(P0, 2);
    outcome.assert_zone(&[spell], Zone::Exile);
}

mod commandeer_ordinary {
    use super::*;
    use engine::game::ability_utils::declared_targets_in_chain;
    use engine::game::scenario::{CastCommit, GameRunner};
    use engine::game::{stack::stack_object_controller, targeting::resolved_targets};
    use engine::types::ability::{Effect, TargetRef};
    use engine::types::game_state::{GameState, StackEntry, StackEntryKind};

    const THINK_TWICE: &str = "Draw a card.\nFlashback {2}{U} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";
    const BOLT: &str = "Lightning Bolt deals 3 damage to any target.";

    struct Board {
        runner: GameRunner,
        think: ObjectId,
        bolt: ObjectId,
        commandeer: ObjectId,
        tops: [ObjectId; 2],
    }

    fn board(cast_think: bool, cast_bolt: bool) -> Board {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let tops = [
            scenario.add_card_to_library_top(P0, "Island"),
            scenario.add_card_to_library_top(P1, "Mountain"),
        ];
        let think = scenario
            .add_spell_to_hand_from_oracle(P0, "Think Twice", true, THINK_TWICE)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 1,
            })
            .id();
        let bolt = scenario
            .add_spell_to_hand_from_oracle(P0, "Lightning Bolt", true, BOLT)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            })
            .id();
        let commandeer = scenario
            .add_spell_to_hand_from_oracle(P1, "Commandeer", true, COMMANDEER)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
                generic: 5,
            })
            .id();
        let mut mana = Vec::new();
        if cast_think {
            mana.extend([ManaType::Colorless, ManaType::Blue]);
        }
        if cast_bolt {
            mana.push(ManaType::Red);
        }
        scenario.with_mana_pool(P0, pool(&mana));
        scenario.with_mana_pool(
            P1,
            pool(&[
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Blue,
                ManaType::Blue,
            ]),
        );
        Board {
            runner: scenario.build(),
            think,
            bolt,
            commandeer,
            tops,
        }
    }

    fn entry(state: &GameState, spell: ObjectId) -> &StackEntry {
        let mut entries = state.stack.iter().filter(|entry| entry.id == spell);
        let result = entries
            .next()
            .expect("addressed original spell remains on stack");
        assert!(entries.next().is_none(), "the spell is not duplicated");
        result
    }

    // CR 601.2h: each ordinary cast pays its complete printed cost.
    fn normal_cast(state: &GameState, spell: ObjectId, mana: u32) {
        assert!(matches!(&entry(state, spell).kind,
            StackEntryKind::Spell { casting_variant: CastingVariant::Normal,
                actual_mana_spent, .. } if *actual_mana_spent == mana));
    }

    // CR 112.2 + CR 613.1b: default caster and owner survive live control changes.
    fn control(state: &GameState, spell: ObjectId, controller: PlayerId) {
        assert_eq!(state.objects[&spell].zone, Zone::Stack);
        assert_eq!(state.objects[&spell].owner, P0);
        assert_eq!(entry(state, spell).controller, P0);
        assert_eq!(state.objects[&spell].controller, controller);
        assert_eq!(
            stack_object_controller(state, entry(state, spell)),
            controller
        );
    }

    // CR 400.3: a resolved stolen spell enters its owner's graveyard.
    fn graveyard(state: &GameState, spell: ObjectId, owner: PlayerId) {
        assert_eq!(state.objects[&spell].zone, Zone::Graveyard);
        assert_eq!(state.objects[&spell].owner, owner);
        assert!(state.players[owner.0 as usize].graveyard.contains(&spell));
        assert!(!state.players[(1 - owner.0) as usize]
            .graveyard
            .contains(&spell));
    }

    fn settled(state: &GameState) {
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.resolution_stack.is_empty());
        assert!(state.resolving_stack_entry.is_none());
        assert!(state.pending_resolution_completion.is_none());
    }

    // CR 121.1: positive event identity accompanies the hand delta and untouched top.
    fn draws(
        state: &GameState,
        events: &[GameEvent],
        tops: [ObjectId; 2],
        drawer: Option<PlayerId>,
    ) {
        let actual: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                GameEvent::CardDrawn {
                    player_id,
                    object_id,
                    ..
                } => Some((*player_id, *object_id)),
                _ => None,
            })
            .collect();
        let expected: Vec<_> = drawer
            .into_iter()
            .map(|p| (p, tops[p.0 as usize]))
            .collect();
        assert_eq!(actual, expected);
        for player in [P0, P1] {
            let top = tops[player.0 as usize];
            assert_eq!(
                state.objects[&top].zone,
                if drawer == Some(player) {
                    Zone::Hand
                } else {
                    Zone::Library
                }
            );
            if drawer == Some(player) {
                assert!(state.players[player.0 as usize].hand.contains(&top));
            } else {
                assert!(state.players[player.0 as usize].library.contains(&top));
            }
        }
    }

    fn pass(response: &mut CastCommit<'_>, events: &mut Vec<GameEvent>) {
        assert!(matches!(
            response.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        events.extend(response.act(GameAction::PassPriority).unwrap().events);
    }

    fn only_selected_control_changed(events: &[GameEvent], spell: ObjectId) {
        let changes: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                GameEvent::ControllerChanged {
                    object_id,
                    old_controller,
                    new_controller,
                    ..
                } => Some((*object_id, *old_controller, *new_controller)),
                _ => None,
            })
            .collect();
        assert_eq!(changes, vec![(spell, P0, P1)]);
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::SpellCopied { .. })));
    }

    // CR 608.2c + CR 608.2d: mandatory control precedes the optional retarget answer.
    fn optional_boundary(
        response: &mut CastCommit<'_>,
        spell: ObjectId,
        commandeer: ObjectId,
    ) -> Vec<GameEvent> {
        normal_cast(response.state(), commandeer, 7);
        assert_eq!(
            declared_targets_in_chain(entry(response.state(), commandeer).ability().unwrap()),
            vec![TargetRef::Object(spell)]
        );
        let mut events = Vec::new();
        for _ in 0..12 {
            if matches!(
                response.state().waiting_for,
                WaitingFor::OptionalEffectChoice { .. }
            ) {
                break;
            }
            pass(response, &mut events);
        }
        let state = response.state();
        assert!(matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice { player: P1, .. }
        ));
        let frame = state
            .resolution_stack
            .active_optional_effect()
            .expect("required optional frame");
        let Effect::ChangeTargets { target, .. } = &frame.ability.effect else {
            panic!(
                "only retargeting may be optional: {:?}",
                frame.ability.effect
            );
        };
        assert_eq!(frame.ability.controller, P1);
        assert_eq!(frame.ability.source_id, commandeer);
        assert_eq!(
            resolved_targets(&frame.ability, target, state),
            vec![TargetRef::Object(spell)]
        );
        control(state, spell, P1);
        only_selected_control_changed(&events, spell);
        for accept in [false, true] {
            assert!(legal_actions(state).iter().any(|action| matches!(action,
                GameAction::DecideOptionalEffect { accept: offered } if *offered == accept)));
        }
        events
    }

    fn commandeer_finished(
        response: &mut CastCommit<'_>,
        events: &mut Vec<GameEvent>,
        commandeer: ObjectId,
        spell: ObjectId,
    ) {
        for _ in 0..12 {
            if response.state().objects[&commandeer].zone == Zone::Graveyard
                && matches!(response.state().waiting_for, WaitingFor::Priority { .. })
            {
                break;
            }
            pass(response, events);
        }
        graveyard(response.state(), commandeer, P1);
        settled(response.state());
        control(response.state(), spell, P1);
    }

    // CR 115.7d + CR 115.7e: legal alternatives and keep address the selected Bolt only.
    fn retarget_boundary(state: &GameState, spell: ObjectId) {
        let WaitingFor::RetargetChoice {
            player,
            stack_entry_index,
            current_targets,
            slots,
            slot_pools,
            ..
        } = &state.waiting_for
        else {
            panic!(
                "accepted targeted spell must require retarget choice: {:?}",
                state.waiting_for
            );
        };
        assert_eq!(*player, P1);
        assert_eq!(state.stack[*stack_entry_index].id, spell);
        assert_eq!(*current_targets, vec![TargetRef::Player(P1)]);
        assert_eq!(slots.len(), 1);
        assert_eq!(slot_pools.len(), 1);
        assert!(slot_pools[0].contains(&TargetRef::Player(P0)));
        for expected in [vec![None], vec![Some(TargetRef::Player(P0))]] {
            assert!(legal_actions(state).iter().any(|action| matches!(action,
                GameAction::RetargetSpell { new_targets } if *new_targets == expected)));
        }
        assert_eq!(
            entry(state, spell).ability().unwrap().targets,
            vec![TargetRef::Player(P1)]
        );
        control(state, spell, P1);
    }

    // CR 109.5 + CR 121.1: ordinary Think Twice draws once for its resolving controller.
    #[test]
    fn r3_think_twice_normal() {
        let mut b = board(true, false);
        let committed = b.runner.cast(b.think).commit();
        normal_cast(committed.state(), b.think, 2);
        let outcome = committed.resolve();
        outcome.assert_hand_drawn(P0, 1);
        outcome.assert_hand_drawn(P1, 0);
        draws(outcome.state(), outcome.events(), b.tops, Some(P0));
        graveyard(outcome.state(), b.think, P0);
        settled(outcome.state());
        assert!(outcome.state().stack.is_empty());
    }

    // CR 120.3a: Bolt's damage causes three life loss to its selected player.
    #[test]
    fn r3_bolt_normal() {
        let mut b = board(false, true);
        let committed = b.runner.cast(b.bolt).target_player(P1).commit();
        normal_cast(committed.state(), b.bolt, 1);
        let outcome = committed.resolve();
        outcome.assert_life_delta(P0, 0);
        outcome.assert_life_delta(P1, -3);
        draws(outcome.state(), outcome.events(), b.tops, None);
        graveyard(outcome.state(), b.bolt, P0);
        settled(outcome.state());
        assert!(outcome.state().stack.is_empty());
    }

    // CR 109.5 + CR 121.1: the new controller draws; optional retargeting cannot undo control.
    fn stolen_think(accept: bool) {
        let mut b = board(true, false);
        let mut original = b.runner.cast(b.think).commit();
        normal_cast(original.state(), b.think, 2);
        original.act(GameAction::PassPriority).unwrap();
        let mut response = original.cast(b.commandeer).target_object(b.think).commit();
        let mut events = optional_boundary(&mut response, b.think, b.commandeer);
        assert!(entry(response.state(), b.think)
            .ability()
            .unwrap()
            .targets
            .is_empty());
        events.extend(
            response
                .act(GameAction::DecideOptionalEffect { accept })
                .unwrap()
                .events,
        );
        // Accepted targetless retargeting must finish, never demand an impossible selection.
        commandeer_finished(&mut response, &mut events, b.commandeer, b.think);
        assert_eq!(response.state().stack.len(), 1);
        normal_cast(response.state(), b.think, 2);
        // B1: the response baseline includes Commandeer's departure from P1's hand.
        let outcome = response.resolve();
        events.extend_from_slice(outcome.events());
        only_selected_control_changed(&events, b.think);
        outcome.assert_hand_drawn(P1, 1);
        outcome.assert_hand_drawn(P0, 0);
        draws(outcome.state(), &events, b.tops, Some(P1));
        graveyard(outcome.state(), b.think, P0);
        graveyard(outcome.state(), b.commandeer, P1);
        settled(outcome.state());
        assert!(outcome.state().stack.is_empty());
    }

    #[test]
    fn r3_think_twice_decline() {
        stolen_think(false);
    }

    #[test]
    fn r3_think_twice_accept_targetless() {
        stolen_think(true);
    }

    // CR 608.2c + CR 608.2d + CR 115.7d + CR 115.7e: control persists through retargeting.
    // CR 120.3a: the final target determines which player loses three life.
    fn stolen_bolt(accept: bool, keep: bool, illegal_first: bool, second_spell: bool) {
        let mut b = board(second_spell, true);
        if second_spell {
            let original = b.runner.cast(b.think).commit();
            normal_cast(original.state(), b.think, 2);
        }
        let mut original = b.runner.cast(b.bolt).target_player(P1).commit();
        normal_cast(original.state(), b.bolt, 1);
        original.act(GameAction::PassPriority).unwrap();
        let mut response = original.cast(b.commandeer).target_object(b.bolt).commit();
        let mut events = optional_boundary(&mut response, b.bolt, b.commandeer);
        if second_spell {
            control(response.state(), b.think, P0);
            assert!(!events.iter().any(|event| matches!(event, GameEvent::ControllerChanged { object_id, .. } if *object_id == b.think)));
        }
        assert_eq!(
            entry(response.state(), b.bolt).ability().unwrap().targets,
            vec![TargetRef::Player(P1)]
        );
        events.extend(
            response
                .act(GameAction::DecideOptionalEffect { accept })
                .unwrap()
                .events,
        );
        if accept {
            retarget_boundary(response.state(), b.bolt);
            if illegal_first {
                let before = response.state().waiting_for.clone();
                assert!(response
                    .act(GameAction::RetargetSpell {
                        new_targets: vec![Some(TargetRef::Object(b.tops[0]))],
                    })
                    .is_err());
                assert_eq!(response.state().waiting_for, before);
                retarget_boundary(response.state(), b.bolt);
            }
            events.extend(
                response
                    .act(GameAction::RetargetSpell {
                        new_targets: vec![if keep {
                            None
                        } else {
                            Some(TargetRef::Player(P0))
                        }],
                    })
                    .unwrap()
                    .events,
            );
        }
        commandeer_finished(&mut response, &mut events, b.commandeer, b.bolt);
        let changed = accept && !keep;
        assert_eq!(
            entry(response.state(), b.bolt).ability().unwrap().targets,
            vec![TargetRef::Player(if changed { P0 } else { P1 })]
        );
        normal_cast(response.state(), b.bolt, 1);
        assert_eq!(
            response.state().stack.len(),
            if second_spell { 2 } else { 1 }
        );
        if second_spell {
            for _ in 0..12 {
                if response.state().objects[&b.bolt].zone == Zone::Graveyard {
                    break;
                }
                pass(&mut response, &mut events);
            }
            graveyard(response.state(), b.bolt, P0);
            control(response.state(), b.think, P0);
            assert_eq!(response.state().players[0].life, 17);
            assert_eq!(response.state().players[1].life, 20);
            draws(response.state(), &events, b.tops, None);
        }
        let outcome = response.resolve();
        events.extend_from_slice(outcome.events());
        only_selected_control_changed(&events, b.bolt);
        outcome.assert_life_delta(P0, if changed { -3 } else { 0 });
        outcome.assert_life_delta(P1, if changed { 0 } else { -3 });
        outcome.assert_hand_drawn(P0, if second_spell { 1 } else { 0 });
        outcome.assert_hand_drawn(P1, 0);
        draws(
            outcome.state(),
            &events,
            b.tops,
            if second_spell { Some(P0) } else { None },
        );
        graveyard(outcome.state(), b.bolt, P0);
        graveyard(outcome.state(), b.commandeer, P1);
        if second_spell {
            graveyard(outcome.state(), b.think, P0);
        }
        settled(outcome.state());
        assert!(outcome.state().stack.is_empty());
    }

    #[test]
    fn r3_bolt_decline() {
        stolen_bolt(false, false, false, false);
    }

    #[test]
    fn r3_bolt_accept_illegal_then_change() {
        stolen_bolt(true, false, true, false);
    }

    #[test]
    fn r3_bolt_accept_keep() {
        stolen_bolt(true, true, false, false);
    }

    #[test]
    fn r3_bolt_selected_subject_above_think_twice() {
        stolen_bolt(true, false, false, true);
    }
}
