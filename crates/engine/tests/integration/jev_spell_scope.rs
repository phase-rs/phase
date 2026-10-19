//! Ordinary public spell ownership with genuine host-cost and regeneration controls.

use super::jev_token_followup::{assert_created, board, ORDINARY_CONSTRUCTION};
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::{oracle_nom::primitives::parse_mana_cost, parse_oracle_text};
use engine::types::ability::{Effect, EffectKind, PerpetualModification, ShieldKind};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::{CostModifyMode, StaticMode};
use engine::types::zones::Zone;
use nom::{combinator::all_consuming, Parser};

const FEUDKILLER: &str = "You gain 10 life. Then if you have more life than an opponent, create a 5/5 white Giant Warrior creature token.";
const PULSE: &str = "You gain 4 life. Then if an opponent has more life than you, return Pulse of the Fields to its owner's hand.";
const SURVIVAL: &str = "You gain 2 life. Then if you have more life than an opponent, draw a card.\nRebound (If you cast this spell from your hand, exile it as it resolves. At the beginning of your next upkeep, you may cast this card from exile without paying its mana cost.)";
const WHITE_SUN: &str = "You gain X life. Create X 1/1 colorless Phyrexian Mite artifact creature tokens with toxic 1 and \"This token can't block.\" If X is 5 or more, destroy all other creatures. (Players dealt combat damage by a creature with toxic 1 also get a poison counter.)";
const CHOICE: &str = "Seek two cards. You may shuffle them into your library. If you do, seek two cards.\nYou have no maximum hand size for the rest of the game.";
const DISCOVER: &str = "Seek three nonland cards, then nonland cards in your hand perpetually gain \"This spell costs {1} less to cast.\"";
const TEFERI: &str = "Counter target spell. Each card in its controller's graveyard, hand, and library with the same name as that spell perpetually gains \"This spell costs {2} more to cast.\"";

fn printed_cost(text: &str) -> ManaCost {
    all_consuming(parse_mana_cost).parse(text).unwrap().1
}

fn spell_pool(generic: usize, colors: &[ManaType]) -> Vec<ManaUnit> {
    colors
        .iter()
        .copied()
        .chain(std::iter::repeat_n(ManaType::Colorless, generic))
        .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect()
}

fn assert_spell_complete(state: &GameState) {
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        state.waiting_for
    );
    assert!(state.stack.is_empty(), "ordinary stack remains");
    assert!(
        state.resolution_stack.is_empty(),
        "suspended resolution remains"
    );
}

#[test]
fn numeric_life_chains_compare_after_gain() {
    let p2 = PlayerId(2);
    for opponents in [[15, 100], [20, 100], [21, 100]] {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario
            .at_phase(Phase::PreCombatMain)
            .with_life(P0, 10)
            .with_life(P1, opponents[0])
            .with_life(p2, opponents[1]);
        scenario.with_mana_pool(P0, spell_pool(4, &[ManaType::White, ManaType::White]));
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Feudkiller's Verdict", false, FEUDKILLER)
            .with_mana_cost(printed_cost("{4}{W}{W}"))
            .id();
        let mut runner = scenario.build();
        let outcome = runner.cast(spell).resolve();
        // CR 119.3 + CR 608.2c/h: the strict live Min comparison follows gain10.
        outcome.assert_life_delta(P0, 10);
        outcome.assert_life_delta(P1, 0);
        outcome.assert_life_delta(p2, 0);
        let tokens: Vec<_> = outcome
            .state()
            .objects
            .values()
            .filter(|object| object.is_token && object.zone == Zone::Battlefield)
            .collect();
        assert_eq!(tokens.len(), usize::from(opponents[0] < 20));
        for token in tokens {
            assert_eq!(token.controller, P0);
            assert_eq!((token.power, token.toughness), (Some(5), Some(5)));
            assert_eq!(token.color, vec![ManaColor::White]);
            assert!(token.card_types.core_types.contains(&CoreType::Creature));
            for subtype in ["Giant", "Warrior"] {
                assert!(token
                    .card_types
                    .subtypes
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case(subtype)));
            }
        }
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        outcome.assert_zone(&[spell], Zone::Graveyard);
        assert_spell_complete(runner.state());
    }
    for opponents in [[13, 15], [13, 14], [12, 13]] {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario
            .at_phase(Phase::PreCombatMain)
            .with_life(P0, 10)
            .with_life(P1, opponents[0])
            .with_life(p2, opponents[1]);
        scenario.with_mana_pool(P0, spell_pool(1, &[ManaType::White, ManaType::White]));
        let decoy = scenario
            .add_spell_to_hand_from_oracle(P0, "Pulse of the Fields", true, PULSE)
            .id();
        let grave_decoy = scenario
            .add_spell_to_graveyard(P0, "Pulse of the Fields", true)
            .id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Pulse of the Fields", true, PULSE)
            .with_mana_cost(printed_cost("{1}{W}{W}"))
            .id();
        let mut runner = scenario.build();
        let outcome = runner.cast(spell).resolve();
        // CR 608.2c/h + CR 608.2n: only this resolving source returns if Max>14.
        outcome.assert_life_delta(P0, 4);
        outcome.assert_life_delta(P1, 0);
        outcome.assert_life_delta(p2, 0);
        outcome.assert_zone(
            &[spell],
            if opponents[1] > 14 {
                Zone::Hand
            } else {
                Zone::Graveyard
            },
        );
        outcome.assert_zone(&[decoy], Zone::Hand);
        outcome.assert_zone(&[grave_decoy], Zone::Graveyard);
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_spell_complete(runner.state());
    }
}

#[test]
fn survival_cache_full_rebound_retains_ordered_gain_draw() {
    // Direct strict/equality Min controls retain the complete Rebound source.
    for opponent in [11, 12] {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario
            .at_phase(Phase::PreCombatMain)
            .with_life(P0, 10)
            .with_life(P1, opponent)
            .with_life(PlayerId(2), 30);
        scenario.with_library_top(P0, &["Draw witness", "Later witness"]);
        scenario.with_mana_pool(P0, spell_pool(2, &[ManaType::White]));
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Survival Cache", false, SURVIVAL)
            .with_keyword(Keyword::Rebound)
            .with_mana_cost(printed_cost("{2}{W}"))
            .id();
        let mut runner = scenario.build();
        let outcome = runner.cast(spell).resolve();
        // CR 119.3 + CR 608.2c/h: equality is not more life.
        outcome.assert_life_delta(P0, 2);
        outcome.assert_hand_drawn(P0, i64::from(opponent == 11));
        outcome.assert_zone(&[spell], Zone::Exile);
        assert_eq!(runner.state().delayed_triggers.len(), 1);
        assert_spell_complete(runner.state());
    }
    for accept in [true, false] {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario
            .at_phase(Phase::PreCombatMain)
            .with_life(P0, 10)
            .with_life(P1, 13)
            .with_life(PlayerId(2), 30);
        for player in [P0, P1, PlayerId(2)] {
            scenario.with_library_top(
                player,
                &[
                    "Upkeep draw witness",
                    "Rebound draw witness",
                    "Reserve draw witness",
                ],
            );
        }
        scenario.with_mana_pool(P0, spell_pool(2, &[ManaType::White]));
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Survival Cache", false, SURVIVAL)
            .with_keyword(Keyword::Rebound)
            .with_mana_cost(printed_cost("{2}{W}"))
            .id();
        let mut runner = scenario.build();
        let first = runner.cast(spell).resolve();
        first.assert_life_delta(P0, 2);
        first.assert_hand_drawn(P0, 0);
        first.assert_zone(&[spell], Zone::Exile);
        assert_eq!(runner.state().delayed_triggers.len(), 1);
        assert!(runner.state().objects[&spell]
            .casting_permissions
            .is_empty());
        assert_spell_complete(runner.state());
        let first_turn = runner.state().turn_number;
        // CR 702.88a/b + CR 608.2g: ordinary turn actions produce the real offer.
        for _ in 0..300 {
            if matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { .. }
            ) {
                break;
            }
            let action = match runner.state().waiting_for {
                WaitingFor::Priority { .. } => GameAction::PassPriority,
                WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                },
                WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                    assignments: vec![],
                },
                ref other => panic!("unexpected turn prompt: {other:?}"),
            };
            runner.act(action).unwrap();
        }
        assert!(runner.state().turn_number > first_turn);
        assert_eq!(runner.state().active_player, P0);
        assert_eq!(runner.state().phase, Phase::Upkeep);
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::OptionalEffectChoice { player: P0, source_id, .. } if source_id == spell)
        );
        let options = engine::ai_support::legal_actions(runner.state());
        for decision in [true, false] {
            assert!(options.iter().any(|action| matches!(action, GameAction::DecideOptionalEffect { accept } if *accept == decision)));
        }
        let hand_before = runner.state().players[0].hand.len();
        let draw_witness = runner.state().players[0].library[0];
        let mut events = runner
            .act(GameAction::DecideOptionalEffect { accept })
            .unwrap()
            .events;
        assert_eq!(
            runner.state().objects[&spell].zone,
            if accept { Zone::Stack } else { Zone::Exile }
        );
        for _ in 0..20 {
            if runner.state().stack.is_empty()
                && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            {
                break;
            }
            events.extend(runner.act(GameAction::PassPriority).unwrap().events);
        }
        assert_eq!(runner.state().players[0].life, if accept { 14 } else { 12 });
        assert_eq!(
            runner.state().players[0].hand.len(),
            hand_before + usize::from(accept)
        );
        assert_eq!(
            runner.state().objects[&draw_witness].zone,
            if accept { Zone::Hand } else { Zone::Library }
        );
        assert_eq!(
            runner.state().objects[&spell].zone,
            if accept { Zone::Graveyard } else { Zone::Exile }
        );
        assert!(runner.state().delayed_triggers.is_empty());
        assert!(runner.state().objects[&spell]
            .casting_permissions
            .is_empty());
        assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::EffectResolved { kind: EffectKind::GainLife, source_id, .. } if *source_id == spell)).count(), usize::from(accept));
        assert_spell_complete(runner.state());
    }
}

#[test]
fn white_suns_twilight_full_source_x0_x3_x5() {
    for x in [0, 3, 5] {
        let mut scenario = GameScenario::new();
        scenario
            .at_phase(Phase::PreCombatMain)
            .with_mana_pool(P0, spell_pool(x, &[ManaType::White, ManaType::White]));
        let victim = scenario.add_creature(P1, "Other creature", 2, 2).id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "White Sun's Twilight", false, WHITE_SUN)
            .with_mana_cost(printed_cost("{X}{W}{W}"))
            .id();
        let mut runner = scenario.build();
        let outcome = runner.cast(spell).x(x as u32).resolve();
        // CR 601.2b + CR 111.3 + CR 119.3: paid X and token-owned text survive.
        outcome.assert_life_delta(P0, x as i32);
        outcome.assert_zone(
            &[victim],
            if x >= 5 {
                Zone::Graveyard
            } else {
                Zone::Battlefield
            },
        );
        let mites: Vec<_> = runner
            .state()
            .objects
            .values()
            .filter(|object| object.is_token && object.zone == Zone::Battlefield)
            .collect();
        assert_eq!(mites.len(), x);
        for mite in mites {
            assert_eq!(mite.controller, P0);
            assert_eq!((mite.power, mite.toughness), (Some(1), Some(1)));
            assert!(mite.color.is_empty());
            for core in [CoreType::Artifact, CoreType::Creature] {
                assert!(mite.card_types.core_types.contains(&core));
            }
            for subtype in ["Phyrexian", "Mite"] {
                assert!(mite
                    .card_types
                    .subtypes
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case(subtype)));
            }
            assert!(mite.keywords.contains(&Keyword::Toxic(1)));
            assert!(mite
                .static_definitions
                .iter_unchecked()
                .any(|definition| definition.mode == StaticMode::CantBlock));
        }
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_spell_complete(runner.state());
    }
}

#[test]
fn choice_of_fortunes_complete_optional_and_persistent_rule() {
    for (available, accept) in [(6, true), (6, false), (1, true), (0, false)] {
        let mut scenario = GameScenario::new();
        scenario
            .at_phase(Phase::PreCombatMain)
            .with_mana_pool(P0, spell_pool(2, &[ManaType::Blue]));
        let existing: Vec<_> = (0..9)
            .map(|index| scenario.add_card_to_hand(P0, &format!("Preexisting {index}")))
            .collect();
        let foreign: Vec<_> = (0..9)
            .map(|index| scenario.add_card_to_hand(P1, &format!("Foreign {index}")))
            .collect();
        let library: Vec<_> = (0..available)
            .map(|index| {
                scenario
                    .add_spell_to_library_top(P0, &format!("Seek member {index}"), true)
                    .id()
            })
            .collect();
        scenario.with_library_top(P1, &["Opponent turn draw", "Opponent reserve"]);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Choice of Fortunes", false, CHOICE)
            .with_mana_cost(printed_cost("{2}{U}"))
            .id();
        let mut runner = scenario.build();
        let initial = runner.cast(spell).commit().stop_at_optional().resolve();
        assert!(
            matches!(initial.final_waiting_for(), WaitingFor::OptionalEffectChoice { player: P0, source_id, .. } if *source_id == spell),
            "{:?}",
            initial.final_waiting_for()
        );
        let options = engine::ai_support::legal_actions(runner.state());
        for decision in [true, false] {
            assert!(options.iter().any(|action| matches!(action, GameAction::DecideOptionalEffect { accept } if *accept == decision)));
        }
        let first_set: Vec<_> = library
            .iter()
            .copied()
            .filter(|id| runner.state().objects[id].zone == Zone::Hand)
            .collect();
        assert_eq!(first_set.len(), available.min(2));
        for id in &existing {
            assert_eq!(runner.state().objects[id].zone, Zone::Hand);
        }
        let mut events = initial.events().to_vec();
        events.extend(
            runner
                .act(GameAction::DecideOptionalEffect { accept })
                .unwrap()
                .events,
        );
        for _ in 0..20 {
            if runner.state().stack.is_empty()
                && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            {
                break;
            }
            events.extend(runner.act(GameAction::PassPriority).unwrap().events);
        }
        assert_spell_complete(runner.state());
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
        let resolved: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                GameEvent::EffectResolved {
                    kind, source_id, ..
                } if *source_id == spell => Some(*kind),
                _ => None,
            })
            .collect();
        assert_eq!(
            resolved,
            if accept {
                vec![
                    EffectKind::Seek,
                    EffectKind::ChangeZoneAll,
                    EffectKind::Shuffle,
                    EffectKind::Seek,
                    EffectKind::CreateEmblem,
                ]
            } else {
                vec![EffectKind::Seek, EffectKind::CreateEmblem]
            }
        );
        // Digital Seek retains exactly its first set; optional movement has one shuffle.
        for id in &first_set {
            let moved_back = events.iter().any(|event| matches!(event, GameEvent::ZoneChanged { object_id, from: Some(Zone::Hand), to: Zone::Library, .. } if object_id == id));
            assert_eq!(moved_back, accept);
        }
        let shuffles: Vec<_> = events
            .iter()
            .enumerate()
            .filter_map(|(index, event)| {
                matches!(
                    event,
                    GameEvent::PlayerPerformedAction {
                        player_id: P0,
                        action: PlayerActionKind::ShuffledLibrary,
                        ..
                    }
                )
                .then_some(index)
            })
            .collect();
        assert_eq!(shuffles.len(), usize::from(accept));
        let seeks: Vec<_> = events.iter().enumerate().filter_map(|(index, event)| matches!(event, GameEvent::EffectResolved { kind: EffectKind::Seek, source_id, .. } if *source_id == spell).then_some(index)).collect();
        if accept {
            assert!(seeks[0] < shuffles[0] && shuffles[0] < seeks[1]);
            assert!(events.iter().enumerate().filter(|(_, event)| matches!(event, GameEvent::ZoneChanged { object_id, from: Some(Zone::Hand), to: Zone::Library, .. } if first_set.contains(object_id))).all(|(index, _)| index < shuffles[0]));
        } else {
            for id in &first_set {
                assert_eq!(runner.state().objects[id].zone, Zone::Hand);
            }
        }
        for id in existing.iter().chain(&foreign) {
            assert!(events.iter().all(|event| !matches!(event, GameEvent::ZoneChanged { object_id, .. } if object_id == id)));
        }
        let emblems: Vec<_> = runner
            .state()
            .objects
            .values()
            .filter(|object| object.is_emblem)
            .collect();
        assert_eq!(emblems.len(), 1);
        assert_eq!(emblems[0].controller, P0);
        assert_eq!(emblems[0].zone, Zone::Command);
        let caster_hand = runner.state().players[0].hand.len();
        assert!(caster_hand > 7);
        let casting_turn = runner.state().turn_number;
        // CR 109.5 + CR 402.2 + CR 514.1: actual cleanup distinguishes recipients.
        // No discard may stop P0; P1 must receive its ordinary required choice.
        for _ in 0..180 {
            if matches!(
                runner.state().waiting_for,
                WaitingFor::DiscardToHandSize { .. }
            ) {
                break;
            }
            let action = match runner.state().waiting_for {
                WaitingFor::Priority { .. } => GameAction::PassPriority,
                WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                },
                WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                    assignments: vec![],
                },
                ref other => panic!("unexpected cleanup route: {other:?}"),
            };
            runner.act(action).unwrap();
        }
        let WaitingFor::DiscardToHandSize {
            player,
            count,
            cards,
        } = runner.state().waiting_for.clone()
        else {
            panic!("opponent must discard: {:?}", runner.state().waiting_for)
        };
        assert_eq!(
            player, P1,
            "constructor-only revert grants every player no maximum"
        );
        assert_eq!(runner.state().active_player, P1);
        assert_eq!(runner.state().phase, Phase::Cleanup);
        assert_eq!(cards, runner.state().players[1].hand);
        assert_eq!(count, cards.len() - 7);
        assert!(count > 0);
        assert_eq!(
            runner.state().players[0].hand.len(),
            caster_hand,
            "caster crossed cleanup without discarding"
        );
        let discarded = cards[..count].to_vec();
        runner
            .act(GameAction::SelectCards {
                cards: discarded.clone(),
            })
            .unwrap();
        assert_eq!(runner.state().players[1].hand.len(), 7);
        for id in discarded {
            assert_eq!(runner.state().objects[&id].zone, Zone::Graveyard);
        }
        assert_spell_complete(runner.state());
        assert!(runner.state().turn_number > casting_turn);
        assert_eq!(
            runner
                .state()
                .objects
                .values()
                .filter(|object| object.is_emblem && object.controller == P0)
                .count(),
            1
        );
    }
}

#[test]
fn hosted_no_max_ends_on_source_departure_and_tower_keeps_mana() {
    for remove in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        for index in 0..9 {
            scenario.add_card_to_hand(P0, &format!("Hand member {index}"));
        }
        scenario.with_library_top(P0, &["Next turn draw", "Reserve"]);
        scenario.with_library_top(P1, &["Opponent draw", "Reserve"]);
        let book = scenario
            .add_artifact_to_hand_from_oracle(P0, "Spellbook", "You have no maximum hand size.")
            .with_mana_cost(printed_cost("{0}"))
            .id();
        let bounce = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Labelled synthetic bounce",
                true,
                "Return target artifact to its owner's hand.",
            )
            .with_mana_cost(printed_cost("{0}"))
            .id();
        let mut runner = scenario.build();
        runner
            .cast(book)
            .resolve()
            .assert_zone(&[book], Zone::Battlefield);
        assert_spell_complete(runner.state());
        if remove {
            if runner.state().priority_player != P0 {
                runner.act(GameAction::PassPriority).unwrap();
            }
            runner
                .cast(bounce)
                .target_object(book)
                .resolve()
                .assert_zone(&[book], Zone::Hand);
            assert_spell_complete(runner.state());
        }
        let hand = runner.state().players[0].hand.len();
        for _ in 0..140 {
            if matches!(
                runner.state().waiting_for,
                WaitingFor::DiscardToHandSize { .. }
            ) || (runner.state().active_player == P1 && runner.state().phase == Phase::Upkeep)
            {
                break;
            }
            let action = match runner.state().waiting_for {
                WaitingFor::Priority { .. } => GameAction::PassPriority,
                WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                },
                WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                    assignments: vec![],
                },
                ref other => panic!("unexpected hosted cleanup: {other:?}"),
            };
            runner.act(action).unwrap();
        }
        if remove {
            let WaitingFor::DiscardToHandSize {
                player: P0,
                count,
                cards,
            } = runner.state().waiting_for.clone()
            else {
                panic!(
                    "source departure must restore cleanup: {:?}",
                    runner.state().waiting_for
                )
            };
            assert_eq!(count, hand - 7);
            assert_eq!(cards, runner.state().players[0].hand);
            runner
                .act(GameAction::SelectCards {
                    cards: cards[..count].to_vec(),
                })
                .unwrap();
            assert_eq!(runner.state().players[0].hand.len(), 7);
        } else {
            assert_eq!(
                runner.state().active_player,
                P1,
                "live host allowed P0 through cleanup"
            );
            assert_eq!(runner.state().players[0].hand.len(), hand);
        }
        // CR 611.3b: the live hosted rule and independently persistent spell differ.
        assert_spell_complete(runner.state());
    }
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tower = scenario
        .add_land_from_oracle(
            P0,
            "Reliquary Tower",
            "You have no maximum hand size.\n{T}: Add {C}.",
        )
        .id();
    let mut runner = scenario.build();
    runner.activate(tower, 0).resolve();
    assert_eq!(runner.state().players[0].mana_pool.total(), 1);
    assert!(runner.state().objects[&tower].tapped);
    assert_spell_complete(runner.state());
}

#[test]
fn discover_formula_grants_unsought_hand_and_future_paid_cast() {
    for available in [3, 1, 0] {
        let mut scenario = GameScenario::new();
        scenario
            .at_phase(Phase::PreCombatMain)
            .with_mana_pool(P0, spell_pool(4, &[ManaType::Blue, ManaType::Blue]));
        let existing = scenario
            .add_spell_to_hand_from_oracle(P0, "Labelled unsought", true, "You gain 1 life.")
            .with_mana_cost(printed_cost("{2}{U}"))
            .id();
        let foreign = scenario
            .add_spell_to_hand_from_oracle(P1, "Labelled unsought", true, "You gain 1 life.")
            .with_mana_cost(printed_cost("{2}{U}"))
            .id();
        let land = scenario.add_land_to_hand(P0, "Island").id();
        let sought: Vec<_> = (0..available)
            .map(|index| {
                scenario
                    .add_spell_to_library_top(P0, &format!("Seek nonland {index}"), true)
                    .id()
            })
            .collect();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Discover the Formula", true, DISCOVER)
            .with_mana_cost(printed_cost("{4}{U}{U}"))
            .id();
        let mut runner = scenario.build();
        let outcome = runner.cast(spell).resolve();
        outcome.assert_hand_drawn(P0, available as i64);
        assert_spell_complete(runner.state());
        for id in sought.iter().copied().chain([existing]) {
            assert_eq!(runner.state().objects[&id].zone, Zone::Hand);
            assert_eq!(runner.state().objects[&id].perpetual_mods.iter().filter(|modification| matches!(modification, PerpetualModification::ModifyCost { mode: CostModifyMode::Reduce, amount } if *amount == ManaCost::generic(1))).count(), 1);
        }
        for id in [land, foreign, spell] {
            assert!(runner.state().objects[&id].perpetual_mods.is_empty());
        }
        assert!(outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Seek,
                ..
            }
        )));
        if runner.state().priority_player != P0 {
            runner.act(GameAction::PassPriority).unwrap();
        }
        runner.state_mut().players[0].mana_pool = spell_pool(1, &[ManaType::Blue]);
        // CR 601.2f + CR 118.7a: unsought {2}{U} now costs exactly {1}{U}.
        let committed = runner.cast(existing).commit();
        assert_eq!(committed.state().objects[&existing].zone, Zone::Stack);
        assert_eq!(committed.state().players[0].mana_pool.total(), 0);
        committed.resolve().assert_life_delta(P0, 1);
        assert_spell_complete(runner.state());
    }
    // Same-name unmodified control requires the unreduced printed cost.
    let mut scenario = GameScenario::new();
    scenario
        .at_phase(Phase::PreCombatMain)
        .with_mana_pool(P0, spell_pool(2, &[ManaType::Blue]));
    let control = scenario
        .add_spell_to_hand_from_oracle(P0, "Labelled unsought", true, "You gain 1 life.")
        .with_mana_cost(printed_cost("{2}{U}"))
        .id();
    let mut runner = scenario.build();
    let commit = runner.cast(control).commit();
    assert_eq!(commit.state().players[0].mana_pool.total(), 0);
    commit.resolve().assert_life_delta(P0, 1);
    assert_spell_complete(runner.state());
}

#[test]
fn teferi_contingency_counters_real_spell_and_keeps_unmodeled_tail_honest() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        spell_pool(0, &[ManaType::White, ManaType::Blue, ManaType::Blue]),
    );
    scenario.with_mana_pool(P1, spell_pool(2, &[]));
    let target = scenario
        .add_spell_to_hand_from_oracle(P1, "Labelled opposing spell", true, "You gain 3 life.")
        .with_mana_cost(printed_cost("{1}"))
        .id();
    let unrelated = scenario
        .add_spell_to_hand_from_oracle(
            P1,
            "Labelled independent stack spell",
            true,
            "You gain 2 life.",
        )
        .with_mana_cost(printed_cost("{1}"))
        .id();
    let hand_decoy = scenario
        .add_spell_to_hand_from_oracle(P1, "Labelled opposing spell", true, "You gain 3 life.")
        .id();
    let gy_decoy = scenario
        .add_spell_to_graveyard(P1, "Labelled opposing spell", true)
        .id();
    let library_decoy = scenario
        .add_spell_to_library_top(P1, "Labelled opposing spell", true)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Teferi's Contingency", true, TEFERI)
        .with_mana_cost(printed_cost("{W}{U}{U}"))
        .id();
    let mut runner = scenario.build();
    runner.act(GameAction::PassPriority).unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    runner.cast(target).commit();
    assert_eq!(runner.state().objects[&target].zone, Zone::Stack);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    runner.cast(unrelated).commit();
    assert_eq!(runner.state().objects[&unrelated].zone, Zone::Stack);
    if runner.state().priority_player != P0 {
        runner.act(GameAction::PassPriority).unwrap();
    }
    let outcome = runner.cast(spell).target_object(target).resolve();
    assert!(outcome.events().iter().any(|event| matches!(event, GameEvent::SpellCountered { object_id, countered_by, .. } if *object_id == target && *countered_by == spell)));
    outcome.assert_zone(&[target, spell, gy_decoy, unrelated], Zone::Graveyard);
    outcome.assert_zone(&[hand_decoy], Zone::Hand);
    outcome.assert_zone(&[library_decoy], Zone::Library);
    outcome.assert_life_delta(P1, 2);
    assert!(outcome.events().iter().all(|event| !matches!(event, GameEvent::SpellCountered { object_id, .. } if *object_id == unrelated)));
    assert!(runner
        .state()
        .objects
        .values()
        .all(|object| object.perpetual_mods.is_empty()));
    assert!(runner
        .state()
        .objects
        .values()
        .all(|object| !object.is_emblem));
    assert!(runner.state().transient_continuous_effects.is_empty());
    assert_spell_complete(runner.state());
    let parsed = parse_oracle_text(
        TEFERI,
        "Teferi's Contingency",
        &[],
        &["Instant".into()],
        &[],
    );
    let tail = parsed.abilities[0].sub_ability.as_deref().unwrap();
    assert!(
        matches!(tail.effect.as_ref(), Effect::Unimplemented { description: Some(fragment), .. } if fragment == TEFERI.strip_prefix("Counter target spell. ").unwrap()),
        "{parsed:#?}"
    );
}

#[test]
fn construction_no_regeneration_rider_reaches_destroy() {
    // Explicitly synthetic control differs only by omitting the rider.
    let regenerable = "Destroy target nonartifact attacking creature. Create a black Spirit creature token. Its power is equal to that creature's power and its toughness is equal to that creature's toughness.";
    for (oracle, expected_zone) in [
        (ORDINARY_CONSTRUCTION, Zone::Graveyard),
        (regenerable, Zone::Battlefield),
    ] {
        let mut b = board(
            oracle,
            (7, 8),
            false,
            false,
            "Return target creature to its owner's hand.",
        );
        b.runner.cast(b.regen).target_object(b.b).resolve();
        // CR 117.3b + CR 117.3d: finish preparation before P0 casts again.
        assert!(matches!(
            b.runner.state().waiting_for,
            WaitingFor::Priority { player: P1 }
        ));
        b.runner.act(GameAction::PassPriority).unwrap();
        assert!(matches!(
            b.runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ));
        assert!(b.runner.state().objects[&b.b]
            .replacement_definitions
            .iter_unchecked()
            .any(|r| r.shield_kind == ShieldKind::Regeneration));
        assert!(b
            .runner
            .state()
            .combat
            .as_ref()
            .unwrap()
            .attackers
            .iter()
            .any(|a| a.object_id == b.b));
        let outcome = b.runner.cast(b.spell).target_object(b.b).resolve();
        // CR 701.19a/c + CR 608.2c: the rider suppresses the shield, not creation.
        outcome.assert_zone(&[b.b], expected_zone);
        assert_created(outcome.state(), outcome.events(), b.spell, 1, (7, 8));
        if expected_zone == Zone::Battlefield {
            assert!(outcome.state().objects[&b.b].tapped);
            assert!(outcome
                .state()
                .combat
                .as_ref()
                .is_none_or(|c| c.attackers.iter().all(|a| a.object_id != b.b)));
        }
    }
}

#[test]
fn ordinary_host_cost_and_token_definition_keep_distinct_owners() {
    for reduced in [false, true] {
        let text = if reduced {
            format!("This spell costs {{1}} less to cast.\n{ORDINARY_CONSTRUCTION}")
        } else {
            ORDINARY_CONSTRUCTION.into()
        };
        // SHAPE accompanies actual payment and resolution; it is not the runtime proof.
        let parsed = parse_oracle_text(
            &text,
            "Synthetic self-cost Spirit",
            &[],
            &["Instant".into()],
            &[],
        );
        assert_eq!(parsed.statics.len(), usize::from(reduced));
        if reduced {
            assert!(
                matches!(&parsed.statics[0].mode, StaticMode::ModifyCost { mode: CostModifyMode::Reduce, amount, .. } if amount == &ManaCost::generic(1))
            );
        }
        assert_eq!(parsed.abilities.len(), 1);
        let chain: Vec<_> =
            std::iter::successors(Some(&parsed.abilities[0]), |d| d.sub_ability.as_deref())
                .collect();
        assert!(chain
            .iter()
            .all(|d| !matches!(*d.effect, Effect::Unimplemented { .. })));
        assert!(chain
            .iter()
            .any(|d| matches!(*d.effect, Effect::Token { .. })));
        assert_eq!(
            chain
                .iter()
                .filter(|d| matches!(*d.effect, Effect::Destroy { .. }))
                .count(),
            1
        );
        let paid = if reduced { 1 } else { 2 };
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let victim = scenario
            .add_creature(P1, "Opponent's ordinary B", 7, 8)
            .id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Synthetic self-cost Spirit", true, &text)
            .with_mana_cost(ManaCost::generic(2))
            .id();
        let mut runner = scenario.build();
        runner.state_mut().active_player = P1;
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(victim, AttackTarget::Player(P0))])
            .unwrap();
        runner.advance_to_priority_window();
        if runner.state().priority_player == P1 {
            runner.act(GameAction::PassPriority).unwrap();
        }
        for _ in 0..paid {
            runner
                .state_mut()
                .add_mana_to_pool(
                    P0,
                    ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
                )
                .unwrap();
        }
        assert_eq!(runner.state().players[0].mana_pool.total(), paid);
        let committed = runner.cast(spell).target_object(victim).commit();
        // CR 601.2a/f: one available mana pays printed {2} only with the host reduction.
        assert_eq!(committed.state().objects[&spell].zone, Zone::Stack);
        assert_eq!(committed.state().players[0].mana_pool.total(), 0);
        let outcome = committed.resolve();
        outcome.assert_zone(&[victim], Zone::Graveyard);
        assert_created(outcome.state(), outcome.events(), spell, 1, (7, 8));
    }
}
