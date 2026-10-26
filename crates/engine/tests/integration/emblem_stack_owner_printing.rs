//! Owner printing follows the creating card captured by an emblem, not the emblem's own art.
//!
//! CR 114.1 + CR 114.4: emblems are created in the command zone and their abilities
//! function there. CR 707.10b: copied abilities retain their original source.
//! CR 708.2a: the face-down primitive removes the creator's named characteristics.
//! The printing choice is display policy, not a game rule: CR 203.1 + CR 206.1
//! make illustration and expansion symbol irrelevant to gameplay.

use engine::game::derived_views::{ClientGameStateRef, DerivedViews};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::visibility::{filter_state_for_unseated_viewer, filter_state_for_viewer};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, QuantityExpr, ResolvedAbility,
    TargetFilter,
};
use engine::types::card_printing::CardPrinting;
use engine::types::display_only::DisplayOnly;
use engine::types::game_state::{GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const NARSET: &str = "[+1]: You gain 2 life. Add {U}, {R}, or {W}. Spend this mana only to cast a noncreature spell.\n[−2]: Draw a card, then you may discard a card. When you discard a nonland card this way, Narset deals damage equal to that card's mana value to target creature or planeswalker.\n[−6]: You get an emblem with \"Whenever you cast a noncreature spell, this emblem deals 2 damage to any target.\"";

fn printing() -> CardPrinting {
    CardPrinting {
        set_code: "iko".into(),
        collector_number: "195".into(),
    }
}

fn assert_projected_printing(
    state: &GameState,
    emblem: ObjectId,
    entry: ObjectId,
    expected: Option<CardPrinting>,
) {
    for viewer in [Some(P0), Some(P1), Some(PlayerId(u8::MAX)), None] {
        let filtered = match viewer {
            Some(player) => filter_state_for_viewer(state, player),
            None => filter_state_for_unseated_viewer(state),
        };
        for envelope in [
            ClientGameStateRef::wrap(state, viewer),
            ClientGameStateRef::wrap_filtered(state, &filtered, viewer),
        ] {
            let wire = serde_json::to_string(&envelope).unwrap();
            // Client transport is JSON, not the trusted GameState decoder: private
            // firing carriers are deliberately removed from viewer projections.
            let decoded: serde_json::Value = serde_json::from_str(&wire).unwrap();
            let derived: DerivedViews = serde_json::from_value(decoded["derived"].clone()).unwrap();
            let source = &envelope.state().objects[&emblem];
            assert!(source.is_emblem);
            assert_eq!(source.zone, Zone::Command);
            assert_eq!(
                source.emblem_source.as_ref().unwrap().printing.get(),
                &expected
            );
            let row = &derived.stack_entry_details[&entry];
            assert_eq!(row.source_name, "Emblem");
            assert_eq!(
                row.printing.get(),
                &expected,
                "emblem stack row must carry the captured creator printing for {viewer:?}"
            );
        }
    }
}

#[test]
fn emblem_stack_owner_printing_narset_trigger_preserves_captured_pair() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let narset = scenario
        .add_planeswalker_from_oracle(P0, "Narset of the Ancient Way", "Narset", 6, NARSET)
        .as_planeswalker_with_loyalty("Narset", 6)
        .id();
    let spell = scenario
        .add_spell_to_hand(P0, "Noncreature trigger", true)
        .id();
    let mut runner = scenario.build();
    let source = runner.state_mut().objects.get_mut(&narset).unwrap();
    source.printing = DisplayOnly::new(Some(printing()));
    source.base_printing = DisplayOnly::new(Some(printing()));
    let index = source
        .abilities
        .iter()
        .position(|ability| matches!(ability.cost, Some(AbilityCost::Loyalty { amount: -6 })))
        .unwrap();
    runner.activate(narset, index).resolve();
    let emblem = *runner
        .state()
        .command_zone
        .last()
        .expect("ultimate created emblem");
    let object = &runner.state().objects[&emblem];
    assert!(object.printing.get().is_none());
    assert_eq!(
        object.emblem_source.as_ref().unwrap().name,
        "Narset of the Ancient Way"
    );
    assert_eq!(
        object.emblem_source.as_ref().unwrap().printing.get(),
        &Some(printing())
    );
    assert_eq!(runner.state().objects[&narset].zone, Zone::Graveyard);
    assert!(runner.state().lki_cache.contains_key(&narset));
    let before = runner.life(P1);
    let commit = runner.cast(spell).target_player(P1).commit();
    let entry = commit
        .state()
        .stack
        .iter()
        .find(|entry| {
            entry.source_id == emblem
                && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
        })
        .expect("production emblem trigger is on the stack");
    assert_projected_printing(commit.state(), emblem, entry.id, Some(printing()));
    // Synthetic creator removal cannot displace the already captured emblem authority.
    let mut departed = commit.state().clone();
    departed.objects.remove(&narset);
    departed.lki_cache.get_mut(&narset).unwrap().printing = DisplayOnly::new(Some(CardPrinting {
        set_code: "conflicting".into(),
        collector_number: "B".into(),
    }));
    assert_projected_printing(&departed, emblem, entry.id, Some(printing()));

    // Restore authority, never a viewer projection, before deriving the row again.
    let saved = serde_json::to_value(commit.state()).unwrap();
    let restored = serde_json::from_value::<engine::types::game_state::PersistedGameState>(saved)
        .unwrap()
        .into_game_state()
        .unwrap();
    assert_projected_printing(&restored, emblem, entry.id, Some(printing()));
    let outcome = commit.resolve();
    assert_eq!(outcome.state().players[P1.0 as usize].life, before - 2);
    assert!(outcome.state().stack.is_empty());
    assert!(matches!(
        outcome.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

fn command_activation(effect: Effect) -> AbilityDefinition {
    let mut ability = AbilityDefinition::new(AbilityKind::Activated, effect);
    // CR 114.4: match the production Momir definition's command-zone activation.
    ability.activation_zone = Some(Zone::Command);
    ability
}

#[test]
fn emblem_stack_owner_printing_activated_and_name_only_provenance() {
    for pair in [Some(printing()), None] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let copier = scenario.add_creature(P0, "Copier", 1, 1).id();
        let mut runner = scenario.build();
        let emblem = engine::game::effects::create_emblem::grant_emblem(
            runner.state_mut(),
            P0,
            vec![],
            vec![],
            vec![command_activation(Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 2 },
                player: TargetFilter::Controller,
            })],
        );
        runner
            .state_mut()
            .objects
            .get_mut(&emblem)
            .unwrap()
            .emblem_source = Some(engine::game::game_object::EmblemSource {
            // Name-only provenance models format-created Momir, not a deck card.
            name: "Momir Vig, Simic Visionary".into(),
            printed_ref: None,
            printing: DisplayOnly::new(pair.clone()),
        });
        let before = runner.life(P0);
        runner
            .act(engine::types::actions::GameAction::ActivateAbility {
                source_id: emblem,
                ability_index: 0,
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            })
            .expect("command-zone emblem activation is accepted");
        let entry = runner
            .state()
            .stack
            .back()
            .expect("activation reached stack");
        assert_eq!(entry.source_id, emblem);
        assert!(matches!(
            entry.kind,
            StackEntryKind::ActivatedAbility { .. }
        ));
        let original_id = entry.id;
        assert_projected_printing(runner.state(), emblem, original_id, pair.clone());
        let copy = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::Any,
                retarget: engine::types::ability::CopyRetargetPermission::KeepOriginalTargets,
                copier: None,
                additional_modifications: vec![],
                starting_loyalty_from_casualty_sacrifice: false,
            },
            vec![engine::types::ability::TargetRef::Object(original_id)],
            copier,
            P0,
        );
        engine::game::effects::copy_spell::resolve(runner.state_mut(), &copy, &mut vec![]).unwrap();
        let copied = runner.state().stack.back().unwrap();
        assert_ne!(copied.id, original_id);
        assert_eq!(
            copied.source_id, emblem,
            "ability copy retains emblem, not copier"
        );
        assert_ne!(copied.source_id, copier);
        assert_projected_printing(runner.state(), emblem, copied.id, pair);
        runner.resolve_top();
        runner.resolve_top();
        assert_eq!(runner.life(P0), before + 4);
        assert!(runner.state().stack.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

#[derive(Clone, Copy, Debug)]
enum CreatorCapture {
    Public,
    FaceDown,
    Hidden,
}

#[test]
fn emblem_stack_owner_printing_capture_disclosure_boundary() {
    for capture in [
        CreatorCapture::Public,
        CreatorCapture::FaceDown,
        CreatorCapture::Hidden,
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let creator = scenario
            .add_planeswalker_from_oracle(P0, "Narset of the Ancient Way", "Narset", 6, NARSET)
            .as_planeswalker_with_loyalty("Narset", 6)
            .id();
        let spell = scenario
            .add_spell_to_hand(P0, "Noncreature trigger", true)
            .id();
        let mut runner = scenario.build();
        let object = runner.state_mut().objects.get_mut(&creator).unwrap();
        object.printing = DisplayOnly::new(Some(printing()));
        object.base_printing = DisplayOnly::new(Some(printing()));
        let effect = object
            .abilities
            .iter()
            .find(|ability| matches!(ability.cost, Some(AbilityCost::Loyalty { amount: -6 })))
            .unwrap()
            .effect
            .as_ref()
            .clone();
        let mut events = Vec::new();
        match capture {
            CreatorCapture::Public => {}
            CreatorCapture::FaceDown => {
                engine::game::morph::apply_face_down_creature_characteristics(
                    runner.state_mut().objects.get_mut(&creator).unwrap(),
                    &engine::types::ability::FaceDownProfile::vanilla_2_2(),
                )
            }
            CreatorCapture::Hidden => {
                engine::game::zone_pipeline::move_object_for_test(
                    runner.state_mut(),
                    engine::game::zone_pipeline::ZoneMoveRequest::effect(
                        creator,
                        Zone::Hand,
                        creator,
                    ),
                    &mut events,
                );
            }
        }
        // Typed capture-primitive fixture, not a naturally occurring concealed ultimate.
        engine::game::effects::create_emblem::resolve(
            runner.state_mut(),
            &ResolvedAbility::new(effect, vec![], creator, P0),
            &mut events,
        )
        .unwrap();
        let emblem = *runner
            .state()
            .command_zone
            .last()
            .expect("capture primitive created emblem");
        let expected = match capture {
            CreatorCapture::FaceDown => {
                assert!(runner.state().objects[&creator].printing.get().is_none());
                assert!(runner.state().objects[&creator]
                    .base_printing
                    .get()
                    .is_none());
                None
            }
            CreatorCapture::Public | CreatorCapture::Hidden => Some(printing()),
        };
        if let CreatorCapture::Public = capture {
            engine::game::zone_pipeline::move_object_for_test(
                runner.state_mut(),
                engine::game::zone_pipeline::ZoneMoveRequest::effect(
                    creator,
                    Zone::Library,
                    creator,
                ),
                &mut events,
            );
        }
        let commit = runner.cast(spell).target_player(P1).commit();
        let entry = commit
            .state()
            .stack
            .iter()
            .find(|entry| entry.source_id == emblem)
            .expect("captured emblem produced trigger");
        assert_projected_printing(commit.state(), emblem, entry.id, expected);
        if let CreatorCapture::Public | CreatorCapture::Hidden = capture {
            let filtered = filter_state_for_viewer(commit.state(), P1);
            assert!(filtered
                .objects
                .get(&creator)
                .is_none_or(|object| object.printing.get().is_none()));
            assert_eq!(
                filtered.objects[&emblem]
                    .emblem_source
                    .as_ref()
                    .unwrap()
                    .printing
                    .get(),
                &Some(printing())
            );
            // Hidden-at-capture metadata survives today. This records the existing
            // disclosure gap; it is not a secrecy pass or a real-game reach witness.
        }
        let outcome = commit.resolve();
        assert_eq!(outcome.state().players[P1.0 as usize].life, 18);
    }
}

#[test]
fn emblem_stack_owner_printing_engine_group_ignores_captured_pair() {
    for second in [
        Some(CardPrinting {
            set_code: "cmm".into(),
            collector_number: "930".into(),
        }),
        Some(printing()),
        None,
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let first = scenario
            .add_planeswalker_from_oracle(P0, "Narset of the Ancient Way", "Narset", 6, NARSET)
            .as_planeswalker_with_loyalty("Narset", 6)
            .id();
        let next = scenario
            .add_planeswalker_from_oracle(P0, "Narset of the Ancient Way", "Narset", 6, NARSET)
            .as_planeswalker_with_loyalty("Narset", 6)
            .id();
        let spell = scenario
            .add_spell_to_hand(P0, "Noncreature trigger", true)
            .id();
        let mut runner = scenario.build();
        for (id, pair) in [(first, Some(printing())), (next, second.clone())] {
            let object = runner.state_mut().objects.get_mut(&id).unwrap();
            object.printing = DisplayOnly::new(pair.clone());
            object.base_printing = DisplayOnly::new(pair);
        }
        let mut events = vec![];
        // Keep the second fixture off the battlefield until the first ultimate
        // has paid all loyalty and resolved; no legend-rule choice is bypassed.
        engine::game::zone_pipeline::move_object_for_test(
            runner.state_mut(),
            engine::game::zone_pipeline::ZoneMoveRequest::effect(next, Zone::Exile, next),
            &mut events,
        );
        let mut emblems = Vec::new();
        for creator in [first, next] {
            if creator == next {
                engine::game::zone_pipeline::move_object_for_test(
                    runner.state_mut(),
                    engine::game::zone_pipeline::ZoneMoveRequest::effect(
                        next,
                        Zone::Battlefield,
                        next,
                    ),
                    &mut events,
                );
            }
            let index = runner.state().objects[&creator]
                .abilities
                .iter()
                .position(|ability| {
                    matches!(ability.cost, Some(AbilityCost::Loyalty { amount: -6 }))
                })
                .unwrap();
            runner.activate(creator, index).resolve();
            assert_eq!(runner.state().objects[&creator].zone, Zone::Graveyard);
            emblems.push(*runner.state().command_zone.last().unwrap());
        }
        let commit = runner
            .cast(spell)
            .target_player(P1)
            .target_player(P1)
            .commit();
        let entries: Vec<_> = commit
            .state()
            .stack
            .iter()
            .filter(|entry| emblems.contains(&entry.source_id))
            .collect();
        assert_eq!(entries.len(), 2, "both real emblems produced triggers");
        for (emblem, pair) in [(emblems[0], Some(printing())), (emblems[1], second)] {
            let entry = entries
                .iter()
                .find(|entry| entry.source_id == emblem)
                .unwrap();
            assert_projected_printing(commit.state(), emblem, entry.id, pair);
        }
        let wire =
            serde_json::to_value(ClientGameStateRef::wrap(commit.state(), Some(P0))).unwrap();
        let groups = wire["derived"]["stack_display_groups"].as_array().unwrap();
        let group = groups
            .iter()
            .find(|group| group["count"] == 2)
            .expect("engine retains one two-trigger group");
        let mut members: Vec<u64> = group["member_ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| id.as_u64().unwrap())
            .collect();
        let mut expected: Vec<u64> = entries.iter().map(|entry| entry.id.0).collect();
        members.sort_unstable();
        expected.sort_unstable();
        assert_eq!(members, expected);
        let outcome = commit.resolve();
        assert_eq!(outcome.state().players[P1.0 as usize].life, 16);
        assert!(outcome.state().stack.is_empty());
    }
}

#[test]
fn emblem_stack_owner_printing_delayed_trigger_uses_emblem_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let mut runner = scenario.build();
    let emblem = engine::game::effects::create_emblem::grant_emblem(
        runner.state_mut(),
        P0,
        vec![],
        vec![],
        vec![command_activation(Effect::CreateDelayedTrigger {
            condition: engine::types::ability::DelayedTriggerCondition::AtNextPhase {
                phase: Phase::End,
            },
            effect: Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 2 },
                    player: TargetFilter::Controller,
                },
            )),
            uses_tracked_set: false,
        })],
    );
    runner
        .state_mut()
        .objects
        .get_mut(&emblem)
        .unwrap()
        .emblem_source = Some(engine::game::game_object::EmblemSource {
        name: "Creator".into(),
        printed_ref: None,
        printing: DisplayOnly::new(Some(printing())),
    });
    runner.activate(emblem, 0).resolve();
    assert_eq!(runner.state().delayed_triggers.len(), 1);
    runner.advance_to_end_step();
    let entry = runner
        .state()
        .stack
        .back()
        .expect("delayed emblem trigger reached stack");
    assert_eq!(entry.source_id, emblem);
    assert!(matches!(
        entry.kind,
        StackEntryKind::TriggeredAbility { .. }
    ));
    assert_projected_printing(runner.state(), emblem, entry.id, Some(printing()));
    runner.resolve_top();
    assert_eq!(runner.life(P0), 22);
    assert!(runner.state().stack.is_empty());
}
