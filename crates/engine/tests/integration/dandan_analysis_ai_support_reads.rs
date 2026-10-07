//! Dandân shared pile: the AI candidate generator and the activation block
//! read-out resolve a seat's graveyard and library through the storage
//! authority. Pile cards are staged owned by the acting seat, in a pile that
//! also holds the other seat's card.

use engine::ai_support::{activation_block_reasons, candidate_actions};
use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::AbilityBlockKind;
use engine::types::actions::GameAction;
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

/// Dandân read from either seat, and the Standard control.
const CASES: [(bool, PlayerId); 3] = [(true, P1), (true, P0), (false, P1)];

fn format_of(shared: bool) -> FormatConfig {
    if shared {
        FormatConfig::dandan()
    } else {
        FormatConfig::standard()
    }
}

fn other(seat: PlayerId) -> PlayerId {
    if seat == P0 {
        P1
    } else {
        P0
    }
}

fn plenty_of_mana() -> Vec<ManaUnit> {
    [ManaType::Black, ManaType::Blue, ManaType::Colorless]
        .into_iter()
        .flat_map(|color| (0..4).map(move |_| ManaUnit::new(color, ObjectId(0), false, vec![])))
        .collect()
}

fn scenario(shared: bool) -> GameScenario {
    let mut scenario = GameScenario::new_with_format(format_of(shared), 2, 11);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

fn start(mut scenario: GameScenario, actor: PlayerId, mana: bool) -> GameRunner {
    if mana {
        scenario.with_mana_pool(actor, plenty_of_mana());
    }
    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.active_player = actor;
    state.priority_player = actor;
    state.waiting_for = WaitingFor::Priority { player: actor };
    runner
}

/// The acting seat's card sits behind the other seat's, so a read that
/// resolves the wrong container or filters by position misses it.
fn graveyard_with(
    sc: &mut GameScenario,
    db: &CardDatabase,
    actor: PlayerId,
    name: &str,
) -> ObjectId {
    sc.add_real_card(other(actor), "Island", Zone::Graveyard, db);
    sc.add_real_card(actor, name, Zone::Graveyard, db)
}

fn offers_activation(runner: &GameRunner, source: ObjectId) -> bool {
    candidate_actions(runner.state()).iter().any(|c| {
        matches!(&c.action, GameAction::ActivateAbility { source_id, .. } if *source_id == source)
    })
}

#[test]
fn v5_a_graveyard_activated_ability_is_offered_from_the_pile() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        for mana in [true, false] {
            let mut sc = scenario(shared);
            let ghoul = graveyard_with(&mut sc, db, actor, "Blessed Ghoul");
            let runner = start(sc, actor, mana);
            assert_eq!(
                offers_activation(&runner, ghoul),
                mana,
                "{shared} {actor:?} mana={mana}: offered exactly when payable"
            );
        }
    }
}

#[test]
fn v5_an_empty_pile_graveyard_offers_no_activation() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(shared);
        sc.add_real_card(actor, "Blessed Ghoul", Zone::Hand, db);
        let runner = start(sc, actor, true);
        let actions = candidate_actions(runner.state());
        assert!(
            actions
                .iter()
                .any(|c| matches!(c.action, GameAction::PassPriority)),
            "{shared} {actor:?}: reach, candidates were generated"
        );
        assert!(
            !actions
                .iter()
                .any(|c| matches!(c.action, GameAction::ActivateAbility { .. })),
            "{shared} {actor:?}: nothing to activate"
        );
    }
}

#[test]
fn v6_a_graveyard_mana_ability_is_offered_from_the_pile() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(shared);
        let jack = graveyard_with(&mut sc, db, actor, "Jack-o'-Lantern");
        let runner = start(sc, actor, true);
        assert!(
            offers_activation(&runner, jack),
            "{shared} {actor:?}: the exile-from-graveyard mana ability (CR 605.1a)"
        );
    }
}

#[test]
fn v7_the_block_read_out_covers_the_pile_graveyard() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        for mana in [false, true] {
            let mut sc = scenario(shared);
            let champion = graveyard_with(&mut sc, db, actor, "Bloodsoaked Champion");
            let attacker = sc.add_creature(actor, "Raider", 2, 2).id();
            let mut runner = start(sc, actor, mana);
            // Raid counts attack declarations recorded this turn.
            let record =
                runner.state().objects[&attacker].snapshot_for_attack_declaration(attacker);
            runner
                .state_mut()
                .attacker_declarations_this_turn
                .push(record);
            let blocked: Vec<AbilityBlockKind> = activation_block_reasons(runner.state())
                .get(&champion)
                .map(|entries| entries.iter().map(|e| e.reason.kind).collect())
                .unwrap_or_default();
            let tag = format!("{shared} {actor:?} mana={mana}");
            if mana {
                assert!(blocked.is_empty(), "{tag}: payable, so not blocked");
                assert!(
                    offers_activation(&runner, champion),
                    "{tag}: reach, raid is met and the ability is offered"
                );
            } else {
                assert_eq!(blocked, vec![AbilityBlockKind::CostNotPayableNow], "{tag}");
            }
        }
    }
}

#[test]
fn v8_card_name_candidates_come_from_the_pile() {
    let Some(db) = shared_card_db() else { return };
    for (shared, actor) in CASES {
        let mut sc = scenario(shared);
        sc.add_real_card(actor, "Island", Zone::Library, db);
        sc.add_real_card(actor, "Brainstorm", Zone::Library, db);
        sc.add_real_card(actor, "Memory Lapse", Zone::Graveyard, db);
        let spell = sc.add_real_card(actor, "Predict", Zone::Hand, db);
        let mut runner = start(sc, actor, true);
        runner.state_mut().all_card_names = std::sync::Arc::from([
            "Control Magic".to_string(),
            "Island".to_string(),
            "Brainstorm".to_string(),
            "Memory Lapse".to_string(),
        ]);
        if shared {
            runner
                .state_mut()
                .deck_pools
                .push(engine::types::game_state::PlayerDeckPool {
                    player: P0,
                    current_main: std::sync::Arc::new(
                        ["Control Magic", "Island", "Brainstorm", "Memory Lapse"]
                            .map(|name| engine::game::deck_loading::DeckEntry {
                                card: engine::types::card::CardFace {
                                    name: name.to_string(),
                                    ..Default::default()
                                },
                                count: 1,
                            })
                            .to_vec(),
                    ),
                    ..Default::default()
                });
        }

        runner.cast(spell).target_player(actor).resolve();

        let WaitingFor::NamedChoice { player, .. } = runner.state().waiting_for.clone() else {
            panic!(
                "expected the card-name prompt, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(player, actor, "{shared} {actor:?}: prompt reached");
        let names: Vec<String> = candidate_actions(runner.state())
            .into_iter()
            .filter_map(|c| match c.action {
                GameAction::ChooseOption { choice } => Some(choice),
                _ => None,
            })
            .collect();
        let expected: &[&str] = if shared {
            &["Memory Lapse", "Control Magic", "Island", "Brainstorm"]
        } else {
            &["Memory Lapse", "Island", "Brainstorm"]
        };
        assert_eq!(
            names,
            expected.iter().map(|n| n.to_string()).collect::<Vec<_>>(),
            "{shared} {actor:?}"
        );
    }
}
