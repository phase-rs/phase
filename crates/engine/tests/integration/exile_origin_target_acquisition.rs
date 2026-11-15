//! Source metadata acquisition and narrow preservation slices.
//! Generic grammar cases are not full card-support claims.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{ControllerRef, FilterProp, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::StackEntryKind;
use engine::types::game_state::WaitingFor;
use engine::types::game_state::{CastPaymentMode, CastingVariant};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::player::PlayerId;

use engine::types::phase::Phase;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

// Verbatim pinned MTGJSON Oracle. The lesser-power comparator and the
// attacking-copy tail are separate parser/continuation defects.
const GYRUS: &str = "Gyrus enters with a number of +1/+1 counters on it equal to the amount of mana spent to cast it.\nWhenever Gyrus attacks, you may exile target creature card with lesser power from your graveyard. If you do, create a token that's a copy of that card and that's tapped and attacking. Exile the token at end of combat.";

struct AttackFixture {
    runner: GameRunner,
    source: ObjectId,
    own: Vec<ObjectId>,
    battlefield: ObjectId,
    opponent: ObjectId,
    unsummon: ObjectId,
    cremate: ObjectId,
    ruinous: ObjectId,
    prior_artifact: ObjectId,
    recipient: ObjectId,
    ray: ObjectId,
}

fn gyrus_board(own_count: usize) -> AttackFixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, "Gyrus, Waker of Corpses", 0, 0, GYRUS)
        .with_plus_counters(3)
        .id();
    let own = (0..own_count)
        .map(|index| {
            scenario
                .add_creature_to_graveyard(P0, &format!("Own card {index}"), 2, index as i32 + 2)
                .id()
        })
        .collect();
    let battlefield = scenario.add_creature(P0, "Battlefield decoy", 2, 2).id();
    let opponent = scenario
        .add_creature_to_graveyard(P1, "Opponent decoy", 2, 2)
        .id();
    let unsummon = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Unsummon",
            true,
            "Return target creature to its owner's hand.",
        )
        .with_mana_cost(ManaCost::zero())
        .id();
    let cremate = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Cremate",
            true,
            "Exile target card from a graveyard.\nDraw a card.",
        )
        .with_mana_cost(ManaCost::zero())
        .id();
    let ruinous = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Ruinous Intrusion",
            true,
            "Exile target artifact or enchantment. Put X +1/+1 counters on target creature you control, where X is the mana value of the permanent exiled this way.",
        )
        .with_mana_cost(ManaCost::zero())
        .id();
    let prior_artifact = scenario
        .add_creature(P1, "Prior artifact", 6, 7)
        .as_artifact()
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let recipient = scenario.add_creature(P0, "Counter recipient", 1, 1).id();
    let ray = scenario.add_spell_to_hand_from_oracle(P1, "Ray of Command", true, "Untap target creature an opponent controls and gain control of it until end of turn. That creature gains haste until end of turn. When you lose control of the creature, tap it.").with_mana_cost(ManaCost::zero()).id();
    scenario.add_card_to_library_top(P0, "Draw filler");
    let runner = scenario.build();
    AttackFixture {
        runner,
        source,
        own,
        battlefield,
        opponent,
        unsummon,
        cremate,
        ruinous,
        prior_artifact,
        recipient,
        ray,
    }
}

fn start_attack(fixture: &mut AttackFixture) {
    assert!(fixture.runner.state().objects[&fixture.source]
        .trigger_definitions
        .iter_unchecked().any(|entry| {
            let trigger = &entry.definition;
            trigger.mode == engine::types::triggers::TriggerMode::Attacks
                && trigger.execute.as_ref().is_some_and(|ability| {
                    !ability.optional_targeting && matches!(ability.effect.as_ref(),
                        engine::types::ability::Effect::ChangeZone {
                            origin: Some(Zone::Graveyard), destination: Zone::Exile,
                            target: TargetFilter::Typed(typed), ..
                        } if typed.type_filters.contains(&engine::types::ability::TypeFilter::Creature))
                })
        }), "a genuine attack exile instruction requires a creature object target before its optional resolution");
    fixture.runner.advance_to_combat();
    assert_eq!(
        fixture.runner.state().objects[&fixture.source].power,
        Some(3)
    );
    fixture
        .runner
        .declare_attackers(&[(fixture.source, AttackTarget::Player(P1))])
        .unwrap();
}

fn gyrus_attack(own_count: usize) -> AttackFixture {
    let mut fixture = gyrus_board(own_count);
    start_attack(&mut fixture);
    fixture
}

fn to_target_or_optional(runner: &mut GameRunner) {
    for _ in 0..40 {
        match runner.state().waiting_for {
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![] })
                    .unwrap();
            }
            _ => return,
        }
    }
    panic!("attack did not reach its target/optional boundary");
}

fn announce_own(fixture: &mut AttackFixture, index: usize) {
    to_target_or_optional(&mut fixture.runner);
    let WaitingFor::TriggerTargetSelection {
        target_slots,
        selection,
        ..
    } = &fixture.runner.state().waiting_for
    else {
        panic!(
            "two legal cards must offer announced targets: {:?}",
            fixture.runner.state().waiting_for
        );
    };
    let legal = &target_slots[selection.current_slot].legal_targets;
    eprintln!(
        "Gyrus source={:?} expected={:?} menu={legal:?}",
        fixture.source, fixture.own
    );
    // CR 115.1d + CR 603.3d: acquire the required trigger target on stack placement.
    assert_eq!(legal.len(), fixture.own.len());
    for &id in &fixture.own {
        assert!(legal.contains(&TargetRef::Object(id)));
    }
    for id in [fixture.source, fixture.battlefield, fixture.opponent] {
        assert!(!legal.contains(&TargetRef::Object(id)));
    }
    fixture
        .runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(fixture.own[index])),
        })
        .unwrap();
    assert!(matches!(
        fixture.runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    let entry = fixture.runner.state().stack.back().unwrap();
    assert_eq!(entry.controller, P0);
    assert_eq!(entry.source_id, fixture.source);
    let ability = entry.ability().expect("actual announced trigger");
    assert_eq!(ability.targets, vec![TargetRef::Object(fixture.own[index])]);
    assert!(ability.target_occurrence_is_current(0, fixture.runner.state()));
}

fn choose_own(fixture: &mut AttackFixture, index: usize) {
    announce_own(fixture, index);
    to_target_or_optional(&mut fixture.runner);
    assert_optional_target(&fixture.runner, fixture.own[index]);
}

fn assert_optional_target(runner: &GameRunner, target: ObjectId) {
    // CR 603.5: the optional effect is decided at resolution, after target announcement.
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    ));
    assert_eq!(
        runner
            .state()
            .active_optional_effect_frame()
            .unwrap()
            .ability
            .targets,
        vec![TargetRef::Object(target)]
    );
}

fn finish(runner: &mut GameRunner) {
    for _ in 0..40 {
        let action = match runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OrderTriggers { .. } => GameAction::OrderTriggers { order: vec![] },
            // The attacking-entry/resumption behavior belongs to PR #9450.
            WaitingFor::EntryAttackTargetChoice { .. } => return,
            ref other => panic!("unexpected continuation: {other:?}"),
        };
        runner.act(action).unwrap();
    }
    panic!("continuation did not reach a supported boundary");
}

#[test]
fn gyrus_announces_own_graveyard_card_then_exiles_and_copies_selected_card() {
    let mut fixture = gyrus_attack(2);
    choose_own(&mut fixture, 1);
    fixture
        .runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .unwrap();
    finish(&mut fixture.runner);
    // CR 400.7j + CR 707.2: the following copy uses the card this instruction moved.
    assert_eq!(
        fixture.runner.state().objects[&fixture.own[1]].zone,
        Zone::Exile
    );
    assert_eq!(
        fixture.runner.state().objects[&fixture.own[0]].zone,
        Zone::Graveyard
    );
    let created = &fixture.runner.state().last_created_token_ids;
    assert_eq!(created.len(), 1);
    let token = &fixture.runner.state().objects[&created[0]];
    assert_eq!(token.name, "Own card 1");
    assert_eq!((token.base_power, token.base_toughness), (Some(2), Some(3)));
}

#[test]
fn gyrus_decline_preserves_announced_card_and_creates_no_copy() {
    let mut fixture = gyrus_attack(2);
    choose_own(&mut fixture, 0);
    fixture
        .runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .unwrap();
    finish(&mut fixture.runner);
    assert_eq!(
        fixture.runner.state().objects[&fixture.own[0]].zone,
        Zone::Graveyard
    );
    assert!(fixture.runner.state().last_created_token_ids.is_empty());
}

#[test]
fn gyrus_sole_graveyard_target_is_autoassigned_before_optional_effect() {
    let mut fixture = gyrus_attack(1);
    to_target_or_optional(&mut fixture.runner);
    assert_optional_target(&fixture.runner, fixture.own[0]);
    fixture
        .runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .unwrap();
    finish(&mut fixture.runner);
    assert_eq!(
        fixture.runner.state().objects[&fixture.own[0]].zone,
        Zone::Exile
    );
    assert_eq!(fixture.runner.state().last_created_token_ids.len(), 1);
}

#[test]
fn gyrus_without_legal_graveyard_card_has_no_trigger_option() {
    let mut fixture = gyrus_attack(0);
    to_target_or_optional(&mut fixture.runner);
    // CR 603.3d: an unfulfillable required target removes the trigger.
    assert!(matches!(
        fixture.runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(fixture.runner.state().stack.is_empty());
    assert!(fixture
        .runner
        .state()
        .active_optional_effect_frame()
        .is_none());
    assert!(fixture.runner.state().last_created_token_ids.is_empty());
}

fn resolve_response(runner: &mut GameRunner, spell: ObjectId) {
    for _ in 0..40 {
        if runner.state().objects[&spell].zone != Zone::Stack {
            return;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        runner.act(GameAction::PassPriority).unwrap();
    }
    panic!("response spell did not resolve");
}

#[test]
fn gyrus_selected_target_survives_source_removal_and_pending_state_roundtrip() {
    let mut fixture = gyrus_attack(2);
    announce_own(&mut fixture, 1);
    let unsummon = fixture.unsummon;
    let _ = fixture
        .runner
        .cast(unsummon)
        .target_object(fixture.source)
        .commit();
    resolve_response(&mut fixture.runner, unsummon);
    assert_eq!(
        fixture.runner.state().objects[&fixture.source].zone,
        Zone::Hand
    );
    to_target_or_optional(&mut fixture.runner);
    assert_optional_target(&fixture.runner, fixture.own[1]);
    let encoded = serde_json::to_vec(fixture.runner.state()).unwrap();
    fixture.runner = GameRunner::from_state(serde_json::from_slice(&encoded).unwrap());
    assert_optional_target(&fixture.runner, fixture.own[1]);
    // CR 113.7a + CR 608.2h: the trigger exists independently of its departed source.
    fixture
        .runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .unwrap();
    finish(&mut fixture.runner);
    assert_eq!(
        fixture.runner.state().objects[&fixture.own[1]].zone,
        Zone::Exile
    );
    assert_eq!(fixture.runner.state().last_created_token_ids.len(), 1);
}

fn generic_owner_graveyard_case(
    qualifier: &str,
    wanted: PlayerId,
    target_player: Option<PlayerId>,
) {
    // Generic grammar fixtures exercise the reusable producer; these are not
    // claims that a fabricated named card is an affected real-card beneficiary.
    let mut scenario = GameScenario::new_n_player(3, 9112);
    scenario.at_phase(Phase::PreCombatMain);
    let mut pools = Vec::new();
    for owner in [P0, P1, P2] {
        pools.push([
            scenario
                .add_creature_to_graveyard(owner, "Owner A", 2, 2)
                .id(),
            scenario
                .add_creature_to_graveyard(owner, "Owner B", 2, 3)
                .id(),
        ]);
    }
    let decoy = scenario.add_creature(P0, "Battlefield decoy", 1, 1).id();
    let attachment = if qualifier == "an opponent's" || target_player.is_some() {
        ""
    } else {
        " with lesser power"
    };
    let oracle = format!("Exile target creature card{attachment} from {qualifier} graveyard.");
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Generic exile grammar fixture", true, &oracle)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let chosen = pools[wanted.0 as usize][1];
    if let Some(player) = target_player {
        // CR 601.2c: the dependent object menu uses the actually chosen player.
        // Inspect normal sequential prompts; rebuilding initial slots loses this binding.
        runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id: runner.state().objects[&spell].card_id,
                targets: vec![],
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            })
            .unwrap();
        let WaitingFor::TargetSelection { selection, .. } = &runner.state().waiting_for else {
            panic!("actual companion-player target prompt");
        };
        assert!(selection
            .current_legal_targets
            .contains(&TargetRef::Player(player)));
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(player)),
            })
            .unwrap();
        let WaitingFor::TargetSelection { selection, .. } = &runner.state().waiting_for else {
            panic!("actual dependent object target prompt");
        };
        assert_eq!(selection.current_legal_targets.len(), 2);
        for &id in &pools[wanted.0 as usize] {
            assert!(selection
                .current_legal_targets
                .contains(&TargetRef::Object(id)));
        }
        assert!(!selection
            .current_legal_targets
            .contains(&TargetRef::Object(decoy)));
        eprintln!(
            "Generic {qualifier}: chosen-player={player:?} actual menu={:?} selected={chosen:?}",
            selection.current_legal_targets
        );
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(chosen)),
            })
            .unwrap();
        finish(&mut runner);
        assert_eq!(runner.state().objects[&chosen].zone, Zone::Exile);
        assert_eq!(runner.state().objects[&decoy].zone, Zone::Battlefield);
        for &id in pools.iter().flatten().filter(|&&id| id != chosen) {
            assert_eq!(runner.state().objects[&id].zone, Zone::Graveyard);
        }
        return;
    }
    let committed = runner.cast(spell).target_object(chosen).commit();
    let StackEntryKind::Spell { ability, .. } = &committed.state().stack.last().unwrap().kind
    else {
        panic!("normal committed spell");
    };
    let slots = engine::game::ability_utils::build_target_slots(
        committed.state(),
        ability.as_deref().unwrap(),
    )
    .unwrap();
    let objects = slots
        .iter()
        .find(|slot| slot.legal_targets.contains(&TargetRef::Object(chosen)))
        .expect("positive object target slot");
    let expected: Vec<_> = pools
        .iter()
        .enumerate()
        .filter(|(index, _)| {
            qualifier == "a"
                || (matches!(qualifier, "an opponent's" | "each opponent's") && *index != 0)
                || (!matches!(qualifier, "an opponent's" | "each opponent's")
                    && *index == wanted.0 as usize)
        })
        .flat_map(|(_, ids)| ids.iter().map(|id| TargetRef::Object(*id)))
        .collect();
    assert_eq!(objects.legal_targets.len(), expected.len());
    for target in expected {
        assert!(objects.legal_targets.contains(&target));
    }
    assert!(!objects.legal_targets.contains(&TargetRef::Object(decoy)));
    eprintln!(
        "Generic {qualifier}: committed menu={:?} selected={chosen:?} authority={target_player:?}",
        objects.legal_targets
    );
    let outcome = committed.resolve();
    outcome.assert_zone(&[chosen], Zone::Exile);
    outcome.assert_zone(&[decoy], Zone::Battlefield);
    for pair in &pools {
        for &id in pair {
            if id != chosen {
                outcome.assert_zone(&[id], Zone::Graveyard);
            }
        }
    }
}

#[test]
fn generic_your_graveyard_recovery() {
    generic_owner_graveyard_case("your", P0, None);
}
#[test]
fn generic_plain_graveyard_recovery() {
    generic_owner_graveyard_case("a", P2, None);
}
#[test]
fn generic_already_zoned_target_player_graveyard_preservation() {
    generic_owner_graveyard_case("target player's", P2, Some(P2));
}
#[test]
fn generic_already_zoned_opponent_graveyard_preservation() {
    generic_owner_graveyard_case("an opponent's", P1, None);
}

#[test]
fn generic_each_opponent_graveyard_singular_recovery() {
    generic_owner_graveyard_case("each opponent's", P2, None);
}

#[test]
fn generic_each_player_owner_binding_selects_only_each_iterations_graveyard() {
    let mut scenario = GameScenario::new_n_player(3, 9113);
    scenario.at_phase(Phase::PreCombatMain);
    let mut chosen = Vec::new();
    let mut unchosen = Vec::new();
    for player in [P0, P1, P2] {
        chosen.push(
            scenario
                .add_creature_to_graveyard(player, "Scoped selected", 2, 2)
                .id(),
        );
        unchosen.push(
            scenario
                .add_creature_to_graveyard(player, "Scoped unselected", 2, 3)
                .id(),
        );
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Generic scoped exile grammar fixture",
            true,
            "Each player exiles a creature card with lesser power from their graveyard.",
        )
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let _ = runner.cast(spell).commit();
    for index in 0..3 {
        to_target_or_optional(&mut runner);
        let WaitingFor::EffectZoneChoice {
            player,
            cards,
            zone,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("scoped iteration missing: {:?}", runner.state().waiting_for);
        };
        assert_eq!(*player, PlayerId(index as u8));
        assert_eq!(*zone, Zone::Graveyard);
        assert_eq!(cards.len(), 2);
        assert!(cards.contains(&chosen[index]) && cards.contains(&unchosen[index]));
        eprintln!("Scoped iteration {index} cards={cards:?}");
        runner
            .act(GameAction::SelectCards {
                cards: vec![chosen[index]],
            })
            .unwrap();
    }
    finish(&mut runner);
    for id in chosen {
        assert_eq!(runner.state().objects[&id].zone, Zone::Exile);
    }
    for id in unchosen {
        assert_eq!(runner.state().objects[&id].zone, Zone::Graveyard);
    }
}

#[test]
fn generic_already_zoned_persisted_source_player_preservation() {
    let mut scenario = GameScenario::new_n_player(3, 9114);
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1, P2] {
        scenario.add_card_to_library_top(player, "Draw filler");
    }
    let mut pools = Vec::new();
    for player in [P0, P1, P2] {
        pools.push([
            scenario
                .add_creature_to_graveyard(player, "Chosen authority A", 1, 2)
                .id(),
            scenario
                .add_creature_to_graveyard(player, "Chosen authority B", 2, 3)
                .id(),
        ]);
    }
    let source = scenario.add_creature_to_hand_from_oracle(P0, "Generic source choice grammar fixture", 3, 3,
        "Haste\nAs this creature enters, choose a player.\nWhenever this creature attacks, target player draws a card. Exile target creature card from the chosen player's graveyard.").with_mana_cost(ManaCost::zero()).id();
    let mut runner = scenario.build();
    let outcome = runner
        .cast(source)
        .choose_option(&P2.0.to_string())
        .resolve();
    outcome.assert_zone(&[source], Zone::Battlefield);
    assert_eq!(runner.state().objects[&source].chosen_player(), Some(P2));
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(source, AttackTarget::Player(P1))])
        .unwrap();
    to_target_or_optional(&mut runner);
    for expected in [TargetRef::Player(P1), TargetRef::Object(pools[2][1])] {
        let WaitingFor::TriggerTargetSelection {
            target_slots,
            selection,
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "expected source-choice target slot: {:?}",
                runner.state().waiting_for
            );
        };
        let legal = &target_slots[selection.current_slot].legal_targets;
        assert!(legal.contains(&expected));
        if matches!(expected, TargetRef::Object(_)) {
            assert_eq!(legal.len(), 2);
            assert!(legal.contains(&TargetRef::Object(pools[2][0])));
            assert!(!legal.contains(&TargetRef::Object(pools[0][0])));
            assert!(!legal.contains(&TargetRef::Object(pools[1][0])));
        }
        runner
            .act(GameAction::ChooseTarget {
                target: Some(expected),
            })
            .unwrap();
    }
    finish(&mut runner);
    assert_eq!(runner.state().objects[&pools[2][1]].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&pools[0][0]].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&pools[1][0]].zone, Zone::Graveyard);
}

#[test]
fn gyrus_does_not_rebind_a_target_that_leaves_and_returns_to_its_graveyard() {
    let mut fixture = gyrus_attack(2);
    announce_own(&mut fixture, 0);
    let target = fixture.own[0];
    let incarnation = fixture.runner.state().objects[&target].incarnation;
    let _ = fixture
        .runner
        .cast(fixture.cremate)
        .target_object(target)
        .commit();
    resolve_response(&mut fixture.runner, fixture.cremate);
    assert_eq!(fixture.runner.state().objects[&target].zone, Zone::Exile);
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(
        fixture.runner.state_mut(),
        target,
        Zone::Graveyard,
        &mut events,
    );
    assert!(
        !events.is_empty(),
        "the product zone authority emitted the return"
    );
    assert_eq!(
        fixture.runner.state().objects[&target].zone,
        Zone::Graveyard
    );
    assert!(fixture.runner.state().objects[&target].incarnation > incarnation);
    finish(&mut fixture.runner);
    // CR 400.7 + CR 608.2b: the returned incarnation is not the announced target.
    assert_eq!(
        fixture.runner.state().objects[&target].zone,
        Zone::Graveyard
    );
    assert!(fixture
        .runner
        .state()
        .active_optional_effect_frame()
        .is_none());
    assert!(fixture.runner.state().last_created_token_ids.is_empty());
}

#[test]
fn gyrus_trigger_keeps_original_controller_after_source_control_changes() {
    let mut fixture = gyrus_attack(2);
    announce_own(&mut fixture, 0);
    // Give P1 priority to cast its real Ray of Command response.
    fixture.runner.act(GameAction::PassPriority).unwrap();
    let _ = fixture
        .runner
        .cast(fixture.ray)
        .target_object(fixture.source)
        .commit();
    resolve_response(&mut fixture.runner, fixture.ray);
    assert_eq!(
        fixture.runner.state().objects[&fixture.source].controller,
        P1
    );
    to_target_or_optional(&mut fixture.runner);
    assert_optional_target(&fixture.runner, fixture.own[0]);
    assert_eq!(
        fixture
            .runner
            .state()
            .active_optional_effect_frame()
            .unwrap()
            .ability
            .controller,
        P0
    );
    // CR 109.5: an existing trigger's `your` remains its triggering controller.
    fixture
        .runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .unwrap();
    finish(&mut fixture.runner);
    assert_eq!(
        fixture.runner.state().objects[&fixture.own[0]].zone,
        Zone::Exile
    );
    assert_eq!(
        fixture.runner.state().objects[&fixture.opponent].zone,
        Zone::Graveyard
    );
}

#[test]
fn gyrus_exile_result_does_not_replace_an_unrelated_tracked_set() {
    let mut fixture = gyrus_board(2);
    let outcome = fixture
        .runner
        .cast(fixture.ruinous)
        .target_objects(&[fixture.prior_artifact, fixture.recipient])
        .resolve();
    outcome.assert_zone(&[fixture.prior_artifact], Zone::Exile);
    // CR 608.2c + CR 202.3 + CR 122.1: the selected exiled permanent supplies X.
    assert_eq!(
        outcome.state().objects[&fixture.recipient]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&4)
    );
    let prior = fixture.runner.state().tracked_object_sets.clone();
    assert!(
        prior
            .values()
            .any(|members| members.contains(&fixture.prior_artifact)),
        "reach guard: earlier exile published its result"
    );
    start_attack(&mut fixture);
    choose_own(&mut fixture, 1);
    fixture
        .runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .unwrap();
    finish(&mut fixture.runner);
    let created = &fixture.runner.state().last_created_token_ids;
    assert_eq!(created.len(), 1);
    assert_eq!(
        fixture.runner.state().objects[&created[0]].name,
        "Own card 1"
    );
    assert_eq!(
        fixture.runner.state().objects[&fixture.own[1]].zone,
        Zone::Exile
    );
    assert_eq!(
        fixture.runner.state().objects[&fixture.own[0]].zone,
        Zone::Graveyard
    );
    assert_eq!(
        (
            fixture.runner.state().objects[&created[0]].base_power,
            fixture.runner.state().objects[&created[0]].base_toughness
        ),
        (Some(2), Some(3))
    );
}

#[test]
fn ruinous_intrusion_reference_producer_consumer() {
    let mut fixture = gyrus_board(2);
    let outcome = fixture
        .runner
        .cast(fixture.ruinous)
        .target_objects(&[fixture.prior_artifact, fixture.recipient])
        .resolve();
    outcome.assert_zone(&[fixture.prior_artifact], Zone::Exile);
    // CR 608.2c + CR 202.3 + CR 122.1: X refers to the selected exiled permanent.
    assert_eq!(
        outcome.state().objects[&fixture.recipient]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&4)
    );
    let published: Vec<_> = outcome
        .state()
        .tracked_object_sets
        .iter()
        .filter(|(_, members)| members.contains(&fixture.prior_artifact))
        .collect();
    assert!(
        !published.is_empty(),
        "real producer/consumer published selected artifact"
    );
    eprintln!(
        "Ruinous: artifact={:?} recipient={:?} counters=4 published={published:?}",
        fixture.prior_artifact, fixture.recipient
    );
}

const ARCANE_PROXY: &str = "Prototype {1}{U}{U} — 2/1 (You may cast this spell with different mana cost, color, and size. It keeps its abilities and types.)\nWhen this creature enters, if you cast it, exile target instant or sorcery card with mana value less than or equal to this creature's power from your graveyard. Copy that card. You may cast the copy without paying its mana cost.";
const NARSET: &str = "Creatures you control have prowess.\nWhenever Narset attacks, exile target noncreature, nonland card with mana value less than Narset's power from a graveyard and copy it. You may cast the copy without paying its mana cost.";

#[test]
fn arcane_proxy_cast_etb_preserves_zoned_target_acquisition() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let proxy = scenario
        .add_creature_to_hand(P0, "Arcane Proxy", 4, 3)
        .as_artifact()
        .as_creature()
        .with_mana_cost(ManaCost::generic(7))
        .from_oracle_text(ARCANE_PROXY)
        .id();
    let cheap = scenario
        .add_spell_to_graveyard(P0, "Generic own MV1 instant", true)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let ceiling = scenario
        .add_spell_to_graveyard(P0, "Generic own MV4 sorcery", false)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let opposing = scenario
        .add_spell_to_graveyard(P1, "Generic opposing MV1 instant", true)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let expensive = scenario
        .add_spell_to_graveyard(P0, "Generic own MV5 instant", true)
        .with_mana_cost(ManaCost::generic(5))
        .id();
    let creature = scenario
        .add_creature_to_graveyard(P0, "Generic GY creature", 1, 1)
        .id();
    let artifact = scenario
        .add_creature_to_graveyard(P0, "Generic GY artifact", 1, 1)
        .as_artifact()
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let land = scenario
        .add_creature_to_graveyard(P0, "Generic GY land", 1, 1)
        .as_land()
        .id();
    let hand = scenario
        .add_spell_to_hand(P0, "Generic hand instant", true)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let exile = scenario
        .add_spell_to_exile(P0, "Generic exiled instant", true)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let decoy = scenario
        .add_creature(P0, "Higher power source decoy", 9, 9)
        .id();
    scenario.with_mana_pool(
        P0,
        (0..7)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    {
        let mut committed = runner
            .cast(proxy)
            .casting_variant(CastingVariant::Normal)
            .commit();
        assert_eq!(committed.state().objects[&proxy].zone, Zone::Stack);
        for _ in 0..40 {
            let action = match &committed.state().waiting_for {
                WaitingFor::Priority { .. } => GameAction::PassPriority,
                WaitingFor::OrderTriggers { .. } => GameAction::OrderTriggers { order: vec![] },
                WaitingFor::TriggerTargetSelection { .. } => break,
                other => panic!("Proxy did not reach cast ETB acquisition: {other:?}"),
            };
            committed.act(action).unwrap();
        }
    }
    assert_eq!(runner.state().objects[&proxy].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&proxy].power, Some(4));
    assert_eq!(
        runner.state().objects[&proxy].cast_from_zone,
        Some(Zone::Hand)
    );
    let WaitingFor::TriggerTargetSelection {
        source_id,
        trigger_controller,
        target_slots,
        selection,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "Proxy acquisition missing: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*source_id, Some(proxy));
    assert_eq!(*trigger_controller, Some(P0));
    let menu = &target_slots[selection.current_slot].legal_targets;
    eprintln!("Proxy source={proxy:?} controller=P0 power4 menu={menu:?}");
    // CR 603.4 + CR 115.1d + CR 202.3: cast ETB announces own-GY spells at the inclusive source-power ceiling.
    assert_eq!(menu.len(), 2);
    for id in [cheap, ceiling] {
        assert!(menu.contains(&TargetRef::Object(id)));
    }
    for id in [
        opposing, expensive, creature, artifact, land, hand, exile, decoy,
    ] {
        assert!(!menu.contains(&TargetRef::Object(id)));
    }
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(ceiling)),
        })
        .unwrap();
    let entry = runner.state().stack.last().unwrap();
    assert_eq!(entry.controller, P0);
    assert_eq!(entry.source_id, proxy);
    let StackEntryKind::TriggeredAbility { ability, .. } = &entry.kind else {
        panic!("actual ETB trigger required");
    };
    assert_eq!(ability.targets, vec![TargetRef::Object(ceiling)]);
    assert!(ability
        .target_pins
        .iter()
        .flatten()
        .any(|pin| pin.object_id == ceiling
            && pin.incarnation == runner.state().objects[&ceiling].incarnation));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn narset_attack_preserves_zoned_target_acquisition() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let narset = scenario
        .add_creature_from_oracle(P0, "Narset, Enlightened Exile", 3, 4, NARSET)
        .id();
    let cheap = scenario
        .add_spell_to_graveyard(P0, "Generic own MV1 instant", true)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let opposing = scenario
        .add_spell_to_graveyard(P1, "Generic opposing MV2 sorcery", false)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let artifact = scenario
        .add_creature_to_graveyard(P0, "Generic GY MV2 artifact", 1, 1)
        .as_artifact()
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let equal = scenario
        .add_spell_to_graveyard(P0, "Generic own MV3 instant", true)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let expensive = scenario
        .add_spell_to_graveyard(P0, "Generic own MV4 instant", true)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let creature = scenario
        .add_creature_to_graveyard(P0, "Generic GY creature", 1, 1)
        .id();
    let land = scenario
        .add_creature_to_graveyard(P0, "Generic GY land", 1, 1)
        .as_land()
        .id();
    let hand = scenario
        .add_spell_to_hand(P0, "Generic hand instant", true)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let exile = scenario
        .add_spell_to_exile(P0, "Generic exiled instant", true)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let decoy = scenario
        .add_creature(P0, "Higher power source decoy", 9, 9)
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(narset, AttackTarget::Player(P1))])
        .unwrap();
    to_target_or_optional(&mut runner);
    assert_eq!(runner.state().objects[&narset].power, Some(3));
    let WaitingFor::TriggerTargetSelection {
        source_id,
        trigger_controller,
        target_slots,
        selection,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "Narset acquisition missing: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*source_id, Some(narset));
    assert_eq!(*trigger_controller, Some(P0));
    let menu = &target_slots[selection.current_slot].legal_targets;
    eprintln!("Narset source={narset:?} controller=P0 power3 menu={menu:?}");
    // CR 115.1d + CR 202.3: any-owner GY noncreature/nonland cards must be below this Narset's power.
    assert_eq!(menu.len(), 3);
    for id in [cheap, opposing, artifact] {
        assert!(menu.contains(&TargetRef::Object(id)));
    }
    for id in [equal, expensive, creature, land, hand, exile, decoy] {
        assert!(!menu.contains(&TargetRef::Object(id)));
    }
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(opposing)),
        })
        .unwrap();
    let entry = runner.state().stack.last().unwrap();
    assert_eq!(entry.controller, P0);
    assert_eq!(entry.source_id, narset);
    let StackEntryKind::TriggeredAbility { ability, .. } = &entry.kind else {
        panic!("actual attack trigger required");
    };
    assert_eq!(ability.targets, vec![TargetRef::Object(opposing)]);
    assert!(ability
        .target_pins
        .iter()
        .flatten()
        .any(|pin| pin.object_id == opposing
            && pin.incarnation == runner.state().objects[&opposing].incarnation));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

fn generic_mass_type_union_case(quantifier: &str, populate: bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let artifact = scenario
        .add_creature_to_graveyard(P0, "Generic artifact", 1, 1)
        .as_artifact()
        .id();
    let creature = scenario
        .add_creature_to_graveyard(P0, "Generic creature", 1, 2)
        .id();
    let nonmatching = scenario
        .add_spell_to_graveyard(P0, "Generic instant", true)
        .id();
    let opponent = scenario
        .add_creature_to_graveyard(P1, "Opposing creature", 1, 1)
        .id();
    let battlefield = scenario.add_creature(P0, "Battlefield decoy", 1, 1).id();
    let oracle = format!(
        "Exile {quantifier} artifact or creature cards with lesser power from your graveyard."
    );
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Generic mass union", true, &oracle)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    assert!(
        runner.state().objects[&spell]
            .abilities
            .iter()
            .any(|ability| {
                matches!(
                    ability.effect.as_ref(),
                    engine::types::ability::Effect::ChangeZoneAll {
                        origin: Some(Zone::Graveyard),
                        destination: Zone::Exile,
                        ..
                    }
                )
            }),
        "both populated and empty union cases reach the genuine mass-exile instruction"
    );
    if !populate {
        for id in [artifact, creature] {
            engine::game::zones::move_to_zone(runner.state_mut(), id, Zone::Hand, &mut Vec::new());
        }
    }
    let outcome = runner.cast(spell).resolve();
    outcome.assert_zone(
        &[artifact, creature],
        if populate { Zone::Exile } else { Zone::Hand },
    );
    outcome.assert_zone(&[nonmatching, opponent], Zone::Graveyard);
    outcome.assert_zone(&[battlefield], Zone::Battlefield);
}

#[test]
fn generic_all_mass_union_recovery() {
    generic_mass_type_union_case("all", true);
}
#[test]
fn generic_each_mass_union_recovery() {
    generic_mass_type_union_case("each", true);
}
#[test]
fn generic_all_mass_union_empty_recovery() {
    generic_mass_type_union_case("all", false);
}
#[test]
fn generic_each_mass_union_empty_recovery() {
    generic_mass_type_union_case("each", false);
}

// CR 109.5 + CR 400.1 + CR 400.3: each opponent's graveyard is owner-scoped,
// independent of the battlefield controller and of the other opponent's pool.
#[test]
fn generic_each_opponent_graveyard_mass_recovery() {
    let mut scenario = GameScenario::new_n_player(3, 9115);
    scenario.at_phase(Phase::PreCombatMain);
    let pools = [P0, P1, P2].map(|owner| {
        [
            scenario
                .add_creature_to_graveyard(owner, "Mass A", 2, 2)
                .id(),
            scenario
                .add_creature_to_graveyard(owner, "Mass B", 2, 3)
                .id(),
        ]
    });
    let battlefield = scenario.add_creature(P0, "Mass BF decoy", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Generic opponent mass grammar",
            true,
            "Exile all creature cards with lesser power from each opponent's graveyard.",
        )
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();
    outcome.assert_zone(&pools[0], Zone::Graveyard);
    outcome.assert_zone(&pools[1], Zone::Exile);
    outcome.assert_zone(&pools[2], Zone::Exile);
    outcome.assert_zone(&[battlefield], Zone::Battlefield);
}

#[test]
fn generic_each_opponent_graveyard_mass_empty_recovery() {
    let mut scenario = GameScenario::new_n_player(3, 9115);
    scenario.at_phase(Phase::PreCombatMain);
    let own = scenario
        .add_creature_to_graveyard(P0, "Own graveyard decoy", 2, 2)
        .id();
    let battlefield = [P0, P1, P2].map(|owner| {
        scenario
            .add_creature(owner, "Empty opponent BF decoy", 2, 2)
            .id()
    });
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Generic empty opponent mass grammar",
            true,
            "Exile all creature cards with lesser power from each opponent's graveyard.",
        )
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    assert!(
        runner.state().objects[&spell]
            .abilities
            .iter()
            .any(|ability| {
                matches!(
                    ability.effect.as_ref(),
                    engine::types::ability::Effect::ChangeZoneAll {
                        origin: Some(Zone::Graveyard),
                        destination: Zone::Exile,
                        ..
                    }
                )
            }),
        "the empty case reaches the same genuine mass-exile producer as its populated sibling"
    );
    assert!([P1, P2]
        .iter()
        .all(|player| runner.state().players[player.0 as usize]
            .graveyard
            .is_empty()));
    let outcome = runner.cast(spell).resolve();
    outcome.assert_zone(&[own], Zone::Graveyard);
    outcome.assert_zone(&battlefield, Zone::Battlefield);
    assert!(runner.state().stack.is_empty());
}

#[test]
fn tormods_crypt_activation_preserves_player_possessive_route() {
    let mut scenario = GameScenario::new_n_player(3, 9116);
    scenario.at_phase(Phase::PreCombatMain);
    let crypt = scenario
        .add_artifact_from_oracle(
            P0,
            "Tormod's Crypt",
            "{T}, Sacrifice this artifact: Exile target player's graveyard.",
        )
        .id();
    let own = scenario
        .add_creature_to_graveyard(P0, "Own graveyard", 1, 1)
        .id();
    let other = scenario
        .add_creature_to_graveyard(P1, "Other graveyard", 1, 1)
        .id();
    let chosen = [
        scenario
            .add_creature_to_graveyard(P2, "Target creature", 1, 1)
            .id(),
        scenario
            .add_spell_to_graveyard(P2, "Target instant", true)
            .id(),
    ];
    let mut runner = scenario.build();
    runner.activate(crypt, 0).target_player(P2).resolve();
    // CR 602.2 + CR 115.10a: the activation pays costs and targets only the player.
    assert_eq!(runner.state().objects[&crypt].zone, Zone::Graveyard);
    for id in chosen {
        assert_eq!(runner.state().objects[&id].zone, Zone::Exile);
    }
    for id in [own, other] {
        assert_eq!(runner.state().objects[&id].zone, Zone::Graveyard);
    }
}

#[test]
fn soul_separator_preserves_selected_exile_and_spirit_identity() {
    let oracle = "{5}, {T}, Sacrifice this artifact: Exile target creature card from your graveyard. Create a token that's a copy of that card, except it's 1/1, it's a Spirit in addition to its other types, and it has flying. Create a black Zombie creature token with power equal to that card's power and toughness equal to that card's toughness.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let separator = scenario
        .add_artifact_from_oracle(P0, "Soul Separator", oracle)
        .id();
    let selected = scenario
        .add_creature_to_graveyard(P0, "Selected Spirit identity", 2, 3)
        .id();
    let unselected = scenario
        .add_creature_to_graveyard(P0, "Other identity", 4, 5)
        .id();
    let opposing = scenario
        .add_creature_to_graveyard(P1, "Wrong owner", 2, 2)
        .id();
    scenario.with_mana_pool(
        P0,
        (0..5)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    runner
        .activate(separator, 0)
        .target_object(selected)
        .resolve();
    // CR 602.2 + CR 400.7j + CR 707.2: paid activation copies the selected exile successor.
    assert_eq!(runner.state().objects[&separator].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&selected].zone, Zone::Exile);
    for id in [unselected, opposing] {
        assert_eq!(runner.state().objects[&id].zone, Zone::Graveyard);
    }
    let tokens: Vec<_> = runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|object| object.is_token)
        .collect();
    assert!(tokens
        .iter()
        .any(|token| token.name == "Selected Spirit identity"
            && token
                .card_types
                .subtypes
                .iter()
                .any(|kind| kind == "Spirit")));
    // The independent Zombie quantity instruction remains unsupported; this test certifies only the selected Spirit identity slice.
    eprintln!(
        "Soul Separator selected={selected:?} tokens={:?}",
        tokens.iter().map(|token| &token.name).collect::<Vec<_>>()
    );
}

#[test]
fn identity_crisis_preserves_p2_hand_and_graveyard_authority() {
    let mut scenario = GameScenario::new_n_player(3, 9117);
    scenario.at_phase(Phase::PreCombatMain);
    let chosen = [
        scenario.add_card_to_hand(P2, "P2 hand card"),
        scenario
            .add_creature_to_graveyard(P2, "P2 GY creature", 1, 1)
            .id(),
        scenario
            .add_spell_to_graveyard(P2, "P2 GY instant", true)
            .id(),
    ];
    let wrong = [
        scenario.add_card_to_hand(P0, "P0 hand"),
        scenario.add_card_to_hand(P1, "P1 hand"),
        scenario.add_creature_to_graveyard(P0, "P0 GY", 1, 1).id(),
        scenario.add_creature_to_graveyard(P1, "P1 GY", 1, 1).id(),
        scenario.add_creature(P2, "P2 BF", 1, 1).id(),
        scenario.add_card_to_library_top(P2, "P2 library"),
    ];
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Identity Crisis",
            false,
            "Exile all cards from target player's hand and graveyard.",
        )
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let before: Vec<_> = wrong
        .iter()
        .map(|id| runner.state().objects[id].zone)
        .collect();
    let outcome = runner.cast(spell).target_player(P2).resolve();
    outcome.assert_zone(&chosen, Zone::Exile);
    for (id, zone) in wrong.iter().zip(before) {
        outcome.assert_zone(&[*id], zone);
    }
}

#[test]
fn generic_counted_graveyard_preserves_resolution_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cards: Vec<_> = (0..3)
        .map(|index| {
            scenario
                .add_creature_to_graveyard(P0, &format!("Generic counted {index}"), 1, 1)
                .id()
        })
        .collect();
    let other = scenario
        .add_creature_to_graveyard(P1, "Wrong owner", 1, 1)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Generic counted GY grammar",
            true,
            "Exile two cards from your graveyard.",
        )
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).effect_zone(&cards[..2]).resolve();
    outcome.assert_zone(&cards[..2], Zone::Exile);
    outcome.assert_zone(&[cards[2], other], Zone::Graveyard);
}

#[test]
fn chrome_mox_preserves_optional_own_hand_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mox = scenario.add_creature_to_hand(P0, "Chrome Mox", 0, 0).as_artifact().from_oracle_text("Imprint — When this artifact enters, you may exile a nonartifact, nonland card from your hand.\n{T}: Add one mana of any of the exiled card's colors.").with_mana_cost(ManaCost::zero()).id();
    let chosen = scenario
        .add_spell_to_hand(P0, "Generic qualifying instant", true)
        .id();
    let other = scenario
        .add_spell_to_hand(P0, "Generic qualifying sorcery", false)
        .id();
    let artifact = scenario
        .add_creature_to_hand(P0, "Generic hand artifact", 1, 1)
        .as_artifact()
        .id();
    let land = scenario
        .add_creature_to_hand(P0, "Generic hand land", 1, 1)
        .as_land()
        .id();
    let opposing = scenario
        .add_spell_to_hand(P1, "Opposing hand instant", true)
        .id();
    let mut runner = scenario.build();
    let outcome = runner
        .cast(mox)
        .accept_optional()
        .effect_zone(&[chosen])
        .resolve();
    outcome.assert_zone(&[mox], Zone::Battlefield);
    outcome.assert_zone(&[chosen], Zone::Exile);
    outcome.assert_zone(&[other, artifact, land, opposing], Zone::Hand);
}

#[test]
fn scavenging_ooze_preserves_any_graveyard_target_and_creature_result() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ooze = scenario.add_creature_from_oracle(P0, "Scavenging Ooze", 2, 2, "{G}: Exile target card from a graveyard. If it was a creature card, put a +1/+1 counter on this creature and you gain 1 life.").id();
    let chosen = scenario
        .add_creature_to_graveyard(P1, "Opposing creature", 1, 1)
        .id();
    let own = scenario
        .add_spell_to_graveyard(P0, "Own instant", true)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![])],
    );
    let mut runner = scenario.build();
    let before = runner.state().players[0].life;
    runner.activate(ooze, 0).target_object(chosen).resolve();
    assert_eq!(runner.state().objects[&chosen].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&own].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&ooze]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&1)
    );
    assert_eq!(runner.state().players[0].life, before + 1);
}

#[test]
fn act_on_impulse_preserves_top_three_and_selected_play_permission() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let fourth = scenario
        .add_spell_to_library_top(P0, "Fourth library spell", true)
        .from_oracle_text("Draw a card.")
        .id();
    let top: Vec<_> = (0..3)
        .map(|index| {
            scenario
                .add_spell_to_library_top(P0, &format!("Generic impulse spell {index}"), true)
                .from_oracle_text("Draw a card.")
                .with_mana_cost(ManaCost::zero())
                .id()
        })
        .collect();
    let unrelated = scenario
        .add_spell_to_exile(P0, "Unrelated exiled spell", true)
        .from_oracle_text("Draw a card.")
        .with_mana_cost(ManaCost::zero())
        .id();
    let impulse = scenario.add_spell_to_hand_from_oracle(P0, "Act on Impulse", false, "Exile the top three cards of your library. Until end of turn, you may play those cards. (If you cast a spell this way, you still pay its costs. You can play a land this way only if you have an available land play remaining.)").with_mana_cost(ManaCost::zero()).id();
    let mut runner = scenario.build();
    let outcome = runner.cast(impulse).resolve();
    outcome.assert_zone(&top, Zone::Exile);
    outcome.assert_zone(&[fourth], Zone::Library);
    let legal = engine::ai_support::legal_actions(runner.state());
    for selected in &top {
        assert!(legal.iter().any(|action| matches!(action, GameAction::CastSpell { object_id, .. } if object_id == selected)));
    }
    assert!(!legal.iter().any(|action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == unrelated)));
}

#[test]
fn soul_jar_full_oracle_death_exiles_the_elf_not_the_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let jar = scenario.add_artifact_from_oracle(P0, "Serpent's Soul-Jar", "Whenever an Elf you control dies, exile it.\n{T}, Pay 2 life: Until end of turn, you may cast a creature spell from among cards exiled with this artifact.").id();
    let elf = scenario
        .add_creature(P0, "Generic Elf", 1, 1)
        .with_subtypes(vec!["Elf"])
        .id();
    let other = scenario.add_creature(P0, "Generic non-Elf", 2, 2).id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, "Destroy target creature.")
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(murder).target_object(elf).resolve();
    outcome.assert_zone(&[elf], Zone::Exile);
    outcome.assert_zone(&[jar, other], Zone::Battlefield);
    assert!(outcome
        .state()
        .exile_links
        .iter()
        .any(|link| link.source_id == jar && link.exiled_id == elf));
}

#[test]
fn doom_time_platform_full_oracle_preserves_enter_with_counters_rider() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let platform = scenario.add_artifact_from_oracle(P0, "Doom's Time Platform", "Whenever you attack, exile target nonland card from your graveyard with two time counters on it. If it doesn't have suspend, it gains suspend. (At the beginning of your upkeep, remove a time counter. When the last is removed, you may cast it without paying its mana cost. If it's a creature, it has haste.)").id();
    let first = scenario
        .add_creature_to_graveyard(P0, "First GY card", 1, 1)
        .id();
    let chosen = scenario
        .add_spell_to_graveyard(P0, "Chosen GY instant", true)
        .id();
    let opposing = scenario
        .add_spell_to_graveyard(P1, "Opposing GY instant", true)
        .id();
    let land = scenario
        .add_creature_to_graveyard(P0, "Own GY land", 1, 1)
        .as_land()
        .id();
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .unwrap();
    to_target_or_optional(&mut runner);
    let WaitingFor::TriggerTargetSelection {
        target_slots,
        selection,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "Doom target boundary missing: {:?}",
            runner.state().waiting_for
        );
    };
    let menu = &target_slots[selection.current_slot].legal_targets;
    assert_eq!(menu.len(), 2);
    for id in [first, chosen] {
        assert!(menu.contains(&TargetRef::Object(id)));
    }
    for id in [opposing, land, platform, attacker] {
        assert!(!menu.contains(&TargetRef::Object(id)));
    }
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(chosen)),
        })
        .unwrap();
    finish(&mut runner);
    // CR 122.1 + CR 122.2: the counterless-origin clause supplies exile entry counters.
    assert_eq!(runner.state().objects[&chosen].zone, Zone::Exile);
    assert_eq!(
        runner.state().objects[&chosen]
            .counters
            .get(&CounterType::Time),
        Some(&2)
    );
    assert_eq!(runner.state().objects[&platform].zone, Zone::Battlefield);
}

const RECOVERED_YOUR: &str =
    "Target player draws a card. Exile target creature card with lesser power from your graveyard.";
const ZONED_YOUR: &str =
    "Target player draws a card. Exile target creature card from your graveyard.";

fn independent_player_board(
    oracle: &str,
) -> (GameRunner, ObjectId, [Vec<ObjectId>; 3], [ObjectId; 2]) {
    let mut scenario = GameScenario::new_n_player(3, 9140);
    scenario.at_phase(Phase::PreCombatMain);
    let pools = [P0, P1, P2].map(|owner| {
        (0..3)
            .map(|index| {
                scenario
                    .add_creature_to_graveyard(
                        owner,
                        &format!("Independent owner {} card {index}", owner.0),
                        2,
                        2,
                    )
                    .id()
            })
            .collect()
    });
    let battlefield = [
        scenario.add_creature(P0, "Independent BF P0", 2, 2).id(),
        scenario.add_creature(P1, "Independent BF P1", 2, 2).id(),
    ];
    scenario.add_card_to_library_top(P1, "Independent P1 draw");
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Generic independent-player grammar", true, oracle)
        .with_mana_cost(ManaCost::zero())
        .id();
    (scenario.build(), spell, pools, battlefield)
}

fn exact_menu(actual: &[TargetRef], expected: &[TargetRef]) {
    assert_eq!(
        actual.len(),
        expected.len(),
        "menu {actual:?} expected {expected:?}"
    );
    for target in expected {
        assert!(actual.contains(target), "missing {target:?} in {actual:?}");
    }
}

fn independent_player_your_menu_case(oracle: &str) {
    let (mut runner, spell, pools, battlefield) = independent_player_board(oracle);
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: runner.state().objects[&spell].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    let WaitingFor::TargetSelection {
        target_slots,
        selection,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("public two-clause target prompt required");
    };
    assert_eq!(target_slots.len(), 2);
    exact_menu(
        &selection.current_legal_targets,
        &[
            TargetRef::Player(P0),
            TargetRef::Player(P1),
            TargetRef::Player(P2),
        ],
    );
    let expected: Vec<_> = pools[0].iter().copied().map(TargetRef::Object).collect();
    // CR 109.5 + CR 601.2c: the first instruction's Player is independent of
    // the declaring spell controller whose own graveyard the second names.
    exact_menu(&target_slots[1].legal_targets, &expected);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .unwrap();
    let WaitingFor::TargetSelection { selection, .. } = &runner.state().waiting_for else {
        panic!("actual post-P1 object prompt required");
    };
    exact_menu(&selection.current_legal_targets, &expected);
    let original = &runner.state().objects[&spell].abilities[0];
    let TargetFilter::Typed(tf) = original
        .sub_ability
        .as_ref()
        .unwrap()
        .effect
        .target_filter()
        .unwrap()
    else {
        panic!("public parser's original object-class target");
    };
    assert!(tf.properties.contains(&FilterProp::InZone {
        zone: Zone::Graveyard
    }));
    if oracle == RECOVERED_YOUR {
        assert!(tf.properties.contains(&FilterProp::Owned {
            controller: ControllerRef::You
        }));
    } else {
        assert_eq!(tf.controller, Some(ControllerRef::You));
    }
    let selected = pools[0][2];
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(selected)),
        })
        .unwrap();
    let entry = runner.state().stack.back().unwrap();
    assert_eq!(entry.controller, P0);
    assert_eq!(entry.source_id, spell);
    let node = entry.ability().unwrap().sub_ability.as_ref().unwrap();
    assert_eq!(node.targets, vec![TargetRef::Object(selected)]);
    assert!(node.target_occurrence_is_current(0, runner.state()));
    finish(&mut runner);
    // CR 701.13a + CR 608.2c: only the announced card moves; P1 draws independently.
    assert_eq!(runner.state().objects[&selected].zone, Zone::Exile);
    for &id in pools.iter().flatten().filter(|&&id| id != selected) {
        assert_eq!(runner.state().objects[&id].zone, Zone::Graveyard);
    }
    for id in battlefield {
        assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
    }
    assert_eq!(runner.state().players[1].hand.len(), 1);
}

#[test]
fn recovered_your_static_and_live_menus_ignore_independent_draw_player() {
    independent_player_your_menu_case(RECOVERED_YOUR);
}

#[test]
fn already_zoned_your_static_and_live_menus_ignore_independent_draw_player() {
    independent_player_your_menu_case(ZONED_YOUR);
}

// CR 109.5 + CR 608.2c: the canonical cast driver preserves both authorities.
fn independent_player_your_canonical_case(oracle: &str) {
    let (mut runner, spell, pools, battlefield) = independent_player_board(oracle);
    let outcome = runner
        .cast(spell)
        .target_player(P1)
        .target_object(pools[0][2])
        .resolve();
    outcome.assert_zone(&[pools[0][2]], Zone::Exile);
    outcome.assert_zone(&pools[0][..2], Zone::Graveyard);
    outcome.assert_zone(&pools[1], Zone::Graveyard);
    outcome.assert_zone(&pools[2], Zone::Graveyard);
    outcome.assert_zone(&battlefield, Zone::Battlefield);
    outcome.assert_hand_drawn(P1, 1);
}

#[test]
fn recovered_your_canonical_driver_selects_nonfirst_card() {
    independent_player_your_canonical_case(RECOVERED_YOUR);
}

#[test]
fn already_zoned_your_canonical_driver_selects_nonfirst_card() {
    independent_player_your_canonical_case(ZONED_YOUR);
}
