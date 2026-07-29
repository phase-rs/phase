//! Issue #6435: Mosswort Bridge hideaway "play the exiled card" must
//! actually grant face-down exile look/play authority (CR 406.6 / 607.1 /
//! 607.2a / 702.75a) and allow both:
//! - casting the hidden spell without paying mana cost
//! - playing the hidden land as a land drop
//!
//! Root cause: `Effect::CastFromZone { mode: Play, ... }` currently granted
//! only `CastingPermission::ExileWithAltCost` and missed the `PlayFromExile`
//! authority needed for face-down exile actions.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::CastingPermission;
use engine::types::actions::GameAction;
use engine::types::game_state::{ExileLinkKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

fn fund_green(runner: &mut GameRunner) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    pool.add(ManaUnit::new(ManaType::Green, dummy, false, vec![]));
}

fn play_mosswort_and_hide(runner: &mut GameRunner, mosswort: ObjectId, hidden: ObjectId) {
    let mosswort_card_id = runner.state().objects[&mosswort].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: mosswort,
            card_id: mosswort_card_id,
        })
        .expect("playing Mosswort Bridge as land must be legal");

    let mut saw_dig_choice = false;
    for _ in 0..128 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DigChoice { cards, .. } => {
                saw_dig_choice = true;
                assert!(
                    cards.contains(&hidden),
                    "reach-guard: the hidden card must be offered by the Hideaway pick"
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![hidden],
                    })
                    .expect("selecting the Hideaway card must succeed");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() && saw_dig_choice {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected waiting state while driving Hideaway ETB: {other:?}"),
        }
    }
    assert!(saw_dig_choice, "Hideaway must surface a DigChoice prompt");
}

fn activate_mosswort_second_ability(
    runner: &mut GameRunner,
    mosswort: ObjectId,
    accept: bool,
) -> bool {
    let activate = runner.act(GameAction::ActivateAbility {
        source_id: mosswort,
        ability_index: 1,
    });
    if let Err(e) = &activate {
        panic!(
            "Mosswort Bridge second ability must be activatable: {:?}. phase={:?} waiting_for={:?} active={:?} priority_player={:?}",
            e,
            runner.state().phase,
            runner.state().waiting_for,
            runner.state().active_player,
            runner.state().priority_player
        );
    }
    activate.expect("Mosswort Bridge second ability must be activatable");

    let mut saw_optional = false;
    for _ in 0..128 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                saw_optional = true;
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("deciding the optional offer must succeed");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => {
                panic!("unexpected waiting state while driving Mosswort activation: {other:?}")
            }
        }
    }
    saw_optional
}

fn hidden_is_face_down_exiled_linked_to_source(
    runner: &GameRunner,
    hidden: ObjectId,
    mosswort: ObjectId,
) {
    let obj = runner.state().objects.get(&hidden).unwrap();
    assert_eq!(obj.zone, Zone::Exile, "hidden card must be in exile");
    assert!(obj.face_down, "hidden card must be face down (Hideaway)");
    assert!(
        runner.state().exile_links.iter().any(|link| {
            link.exiled_id == hidden
                && link.source_id == mosswort
                && link.kind == ExileLinkKind::HideawayLookable
        }),
        "hidden card must be linked to Mosswort Bridge via HideawayLookable"
    );
}

#[test]
fn mosswort_bridge_hideaway_free_casts_hidden_spell_when_total_power_ge_10() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mosswort = scenario.add_real_card(P0, "Mosswort Bridge", Zone::Hand, db);
    let hidden_spell = scenario.add_real_card(P0, "Shock", Zone::Library, db);
    // Hideaway 4 looks at the top 4 cards; seed enough so P0 doesn't lose
    // from an empty library after the hideaway exile removes the chosen card.
    scenario.add_real_card(P0, "Forest", Zone::Library, db);
    scenario.add_real_card(P0, "Island", Zone::Library, db);
    scenario.add_real_card(P0, "Mountain", Zone::Library, db);
    // Prevent the game from ending on P1's next draw step due to an empty library.
    scenario.add_real_card(P1, "Island", Zone::Library, db);

    // Total power >= 10 gate.
    scenario.add_creature(P0, "Power 10", 10, 1);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Turn 1: play Mosswort; resolve Hideaway ETB to exile the Shock.
    play_mosswort_and_hide(&mut runner, mosswort, hidden_spell);
    hidden_is_face_down_exiled_linked_to_source(&runner, hidden_spell, mosswort);

    // Advance to the next turn so Mosswort is untapped.
    runner.advance_to_end_step();
    runner.advance_to_upkeep();
    runner.advance_to_phase(Phase::PreCombatMain);
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ) {
        // There are no Hideaway-related requirements for combat in this test;
        // declare no attackers so we can reach the next untap/main window.
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            })
            .expect("declare no attackers");
        runner.advance_to_phase(Phase::PreCombatMain);
    }
    if runner.state().priority_player != P0 {
        runner.advance_to_end_step();
        runner.advance_to_upkeep();
        runner.advance_to_phase(Phase::PreCombatMain);
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ) {
            runner
                .act(GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                })
                .expect("declare no attackers");
            runner.advance_to_phase(Phase::PreCombatMain);
        }
    }

    fund_green(&mut runner);

    // Activate, accept the optional "you may play" offer.
    let saw_optional = activate_mosswort_second_ability(&mut runner, mosswort, true);
    assert!(saw_optional, "power>=10 must surface the optional offer");

    // The hidden spell must have both the free-cast permission and the
    // face-down exile look/play authority.
    let obj = runner.state().objects.get(&hidden_spell).unwrap();
    assert!(
        obj.casting_permissions.iter().any(|p| matches!(
            p,
            CastingPermission::ExileWithAltCost {
                cost,
                granted_to: Some(P0),
                duration: None,
                ..
            } if *cost == ManaCost::zero()
        )),
        "Mosswort must stamp a zero-cost ExileWithAltCost permission on the hidden spell"
    );
    assert!(
        obj.casting_permissions.iter().any(|p| matches!(
            p,
            CastingPermission::PlayFromExile {
                granted_to,
                exiled_by_ability_controller,
                card_filter,
                source_id: Some(src),
                ..
            } if *granted_to == P0
                && *exiled_by_ability_controller == Some(P0)
                && card_filter.is_none()
                && *src == mosswort
        )),
        "Mosswort must also stamp PlayFromExile so the player can look/cast the face-down exiled spell"
    );

    let life_before = runner.state().players[1].life;
    let mana_before = runner.state().players[0].mana_pool.total();

    let hidden_card_id = obj.card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: hidden_spell,
            card_id: hidden_card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("cast of the face-down exiled hidden spell must be accepted");

    // Drive target selection / stack settlement.
    for _ in 0..128 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(engine::types::ability::TargetRef::Player(P1)),
                    })
                    .expect("choose shock target");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected waiting state while settling hidden spell cast: {other:?}"),
        }
    }
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[1].life,
        life_before - 2,
        "Shock must resolve and deal 2 damage to the targeted player"
    );
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        mana_before,
        "casting the hidden spell without paying mana cost must not drain mana pool"
    );
}

#[test]
fn mosswort_bridge_hideaway_free_plays_hidden_land_when_total_power_ge_10() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mosswort = scenario.add_real_card(P0, "Mosswort Bridge", Zone::Hand, db);
    let hidden_land = scenario.add_real_card(P0, "Forest", Zone::Library, db);
    scenario.add_real_card(P0, "Island", Zone::Library, db);
    scenario.add_real_card(P0, "Mountain", Zone::Library, db);
    scenario.add_real_card(P0, "Swamp", Zone::Library, db);
    scenario.add_real_card(P1, "Island", Zone::Library, db);
    scenario.add_creature(P0, "Power 10", 10, 1);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    play_mosswort_and_hide(&mut runner, mosswort, hidden_land);
    hidden_is_face_down_exiled_linked_to_source(&runner, hidden_land, mosswort);

    runner.advance_to_end_step();
    runner.advance_to_upkeep();
    runner.advance_to_phase(Phase::PreCombatMain);
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ) {
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            })
            .expect("declare no attackers");
        runner.advance_to_phase(Phase::PreCombatMain);
    }
    if runner.state().priority_player != P0 {
        runner.advance_to_end_step();
        runner.advance_to_upkeep();
        runner.advance_to_phase(Phase::PreCombatMain);
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ) {
            runner
                .act(GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                })
                .expect("declare no attackers");
            runner.advance_to_phase(Phase::PreCombatMain);
        }
    }

    fund_green(&mut runner);
    let saw_optional = activate_mosswort_second_ability(&mut runner, mosswort, true);
    assert!(saw_optional, "power>=10 must surface the optional offer");

    let hidden_card_id = runner.state().objects[&hidden_land].card_id;
    assert!(
        runner.state().objects[&hidden_land].face_down,
        "reach-guard: the hidden land is still face down before playing it"
    );
    let lands_before = runner.state().lands_played_this_turn;

    runner
        .act(GameAction::PlayLand {
            object_id: hidden_land,
            card_id: hidden_card_id,
        })
        .expect("playing the face-down exiled hidden land must be accepted");

    assert_eq!(
        runner.state().objects[&hidden_land].zone,
        Zone::Battlefield,
        "playing the hidden land must move it to the battlefield"
    );
    assert!(
        !runner.state().objects[&hidden_land].face_down,
        "playing a land must turn it face up on entry"
    );
    assert_eq!(
        runner.state().lands_played_this_turn,
        lands_before + 1,
        "playing the hidden land consumes exactly one land play action"
    );
}

#[test]
fn mosswort_bridge_hideaway_with_power_lt_10_offers_nothing() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mosswort = scenario.add_real_card(P0, "Mosswort Bridge", Zone::Hand, db);
    let hidden_spell = scenario.add_real_card(P0, "Shock", Zone::Library, db);
    scenario.add_real_card(P1, "Island", Zone::Library, db);
    scenario.add_real_card(P0, "Forest", Zone::Library, db);
    scenario.add_real_card(P0, "Island", Zone::Library, db);
    scenario.add_real_card(P0, "Mountain", Zone::Library, db);

    // Total power < 10.
    scenario.add_creature(P0, "Power 9", 9, 1);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    play_mosswort_and_hide(&mut runner, mosswort, hidden_spell);
    hidden_is_face_down_exiled_linked_to_source(&runner, hidden_spell, mosswort);

    runner.advance_to_end_step();
    runner.advance_to_upkeep();
    runner.advance_to_phase(Phase::PreCombatMain);
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ) {
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            })
            .expect("declare no attackers");
        runner.advance_to_phase(Phase::PreCombatMain);
    }
    if runner.state().priority_player != P0 {
        runner.advance_to_end_step();
        runner.advance_to_upkeep();
        runner.advance_to_phase(Phase::PreCombatMain);
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ) {
            runner
                .act(GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                })
                .expect("declare no attackers");
            runner.advance_to_phase(Phase::PreCombatMain);
        }
    }

    fund_green(&mut runner);
    let saw_optional = activate_mosswort_second_ability(&mut runner, mosswort, true);
    assert!(
        !saw_optional,
        "power<10 must not surface the optional \"you may play\" offer"
    );

    let obj = runner.state().objects.get(&hidden_spell).unwrap();
    assert!(
        obj.casting_permissions
            .iter()
            .all(|p| !matches!(p, CastingPermission::ExileWithAltCost { .. })),
        "power<10 must not grant ExileWithAltCost to the hidden spell"
    );
    assert!(
        obj.casting_permissions
            .iter()
            .all(|p| !matches!(p, CastingPermission::PlayFromExile { .. })),
        "power<10 must not grant PlayFromExile to the hidden spell"
    );
}
