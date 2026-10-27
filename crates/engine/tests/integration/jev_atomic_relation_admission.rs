//! Honest atomic refusal preserves independent paid and event-driven effects.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityDefinition, Effect, EffectKind};
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const KAMACHAL: &str = "Menace\n{R}: Kamachal, Ship's Mascot gets +1/+0 until end of turn.\nWhenever Kamachal deals combat damage to a player, create a Treasure token. Then exile from that player's library a random card with mana value equal to the damage dealt. You may cast that card this turn.";
const KABOOM: &str = "Choose any number of target players or planeswalkers. For each of them, reveal cards from the top of your library until you reveal a nonland card, Kaboom! deals damage equal to that card's mana value to that player or planeswalker, then you put the revealed cards on the bottom of your library in any order.";
const UNKNOWN: &str = "Eminence — As long as The Unknown Wizard is in the command zone or on the battlefield, other playtest cards you cast cost {1} less to cast.\nWhenever The Unknown Wizard enters or attacks, look at the top ten cards of your library. You may put a legendary playtest card from among them onto the battlefield. Put the rest on the bottom in a random order.";
const HERO: &str = "When Hero's Uncle dies, you may search your library for a legendary team-up creature, reveal it, put it into your hand, then shuffle. (A legendary team-up creature is two or more legendary characters teamed up on the same legendary creature card, such as Drana and Linvala.)";
const YOU: &str = "{T}: Choose another target playtest card. Increase or decrease a number of your choice on it by 1 for as long as it remains on the stack or battlefield. (You can change numerals or number words.)\n{W}{U}{B}{R}{G}: Reveal the top eight cards of your library. Put each playtest card among them into your hand. Put the rest on the bottom of your library in a random order.";
const REVERENT: &str = "If you control a Forest, rather than pay this spell's mana cost, you may have each other player gain 6 life.\nDestroy all enchantments.";

fn unit(color: ManaType) -> ManaUnit {
    ManaUnit::new(color, ObjectId(0), false, vec![])
}
fn cost(generic: u32, shards: Vec<ManaCostShard>) -> ManaCost {
    ManaCost::Cost { shards, generic }
}
fn nodes(root: &AbilityDefinition) -> Vec<&AbilityDefinition> {
    std::iter::successors(Some(root), |node| node.sub_ability.as_deref()).collect()
}

#[test]
fn full_atomic_gap_fragments_preserve_every_independent_body() {
    for (name, text, expected) in [
        ("Kamachal, Ship's Mascot", KAMACHAL, vec!["Then exile from that player's library a random card with mana value equal to the damage dealt. You may cast that card this turn."]),
        ("Kaboom!", KABOOM, vec!["Choose any number of target players or planeswalkers. For each of them, reveal cards from the top of your library until you reveal a nonland card, ~ deals damage equal to that card's mana value to that player or planeswalker, then you put the revealed cards on the bottom of your library in any order."]),
        ("The Unknown Wizard", UNKNOWN, vec!["Eminence — As long as ~ is in the command zone or on the battlefield, other playtest cards you cast cost {1} less to cast.", "You may put a legendary playtest card from among them onto the battlefield."]),
        ("Hero's Uncle", HERO, vec!["you may search your library for a legendary team-up creature, reveal it, put it into your hand"]),
        ("You, Iterative Playtester", YOU, vec!["Choose another target playtest card. Increase or decrease a number of your choice on it by 1 for as long as it remains on the stack or battlefield.", "Put each playtest card among them into your hand."]),
        ("Reverent Silence", REVERENT, vec!["If you control a Forest, rather than pay this spell's mana cost, you may have each other player gain 6 life."]),
    ] {
        let types = if name == "Kaboom!" || name == "Reverent Silence" { vec!["Sorcery".to_owned()] } else { vec!["Creature".to_owned()] };
        let parsed = parse_oracle_text(text, name, &[], &types, &[]);
        let bodies: Vec<_> = parsed.abilities.iter().flat_map(nodes)
            .chain(parsed.triggers.iter().filter_map(|trigger| trigger.execute.as_deref()).flat_map(nodes))
            .collect();
        let gaps: Vec<_> = bodies.iter().filter_map(|node| match &*node.effect {
            Effect::Unimplemented { description: Some(fragment), .. } => Some(fragment.as_str()), _ => None,
        }).collect();
        assert_eq!(gaps.len(), expected.len(), "{name}: {bodies:#?}");
        for (actual, expected) in gaps.iter().zip(expected) {
            assert_eq!(actual.trim_end_matches('.'), expected.trim_end_matches('.'), "whole owned fragment for {name}");
        }
        match name {
            "Kamachal, Ship's Mascot" => {
                assert!(parsed.extracted_keywords.contains(&Keyword::Menace));
                assert!(bodies.iter().any(|node| matches!(&*node.effect, Effect::Pump { .. })));
                assert!(bodies.iter().any(|node| matches!(&*node.effect, Effect::Token { .. })));
            }
            "The Unknown Wizard" => assert!(bodies.iter().any(|node| matches!(&*node.effect, Effect::Dig { .. }))),
            "You, Iterative Playtester" => assert!(bodies.iter().any(|node| matches!(&*node.effect, Effect::RevealTop { count: 8, .. }))),
            "Hero's Uncle" => assert!(bodies.iter().any(|node| matches!(&*node.effect, Effect::Shuffle { .. }))),
            "Reverent Silence" => {
                assert!(parsed.casting_options.is_empty());
                assert!(bodies.iter().any(|node| matches!(&*node.effect, Effect::DestroyAll { .. })));
            }
            _ => {}
        }
    }
}

#[test]
fn reverent_silence_ordinary_paid_cast_destroys_enchantments_and_never_gifts_life() {
    for forest in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let own = scenario
            .add_enchantment_from_oracle(P0, "Independent enchantment", "")
            .id();
        let foreign = scenario
            .add_enchantment_from_oracle(P1, "Foreign enchantment", "")
            .id();
        let creature = scenario.add_creature(P1, "Unaffected creature", 2, 2).id();
        let forest_id = forest.then(|| scenario.add_basic_land(P0, ManaColor::Green));
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Reverent Silence", false, REVERENT)
            .with_mana_cost(cost(3, vec![ManaCostShard::Green]))
            .id();
        scenario.with_mana_pool(
            P0,
            vec![
                unit(ManaType::Green),
                unit(ManaType::Colorless),
                unit(ManaType::Colorless),
                unit(ManaType::Colorless),
            ],
        );
        let mut runner = scenario.build();
        let outcome = runner.cast(spell).resolve();
        outcome.assert_zone(&[own, foreign, spell], Zone::Graveyard);
        outcome.assert_zone(&[creature], Zone::Battlefield);
        if let Some(forest) = forest_id {
            outcome.assert_zone(&[forest], Zone::Battlefield);
        }
        outcome.assert_life_delta(P0, 0);
        outcome.assert_life_delta(P1, 0);
        assert_eq!(outcome.mana_pool_total(P0), 0);
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
        assert!(outcome.state().stack.is_empty() && outcome.state().resolution_stack.is_empty());
    }
}

#[test]
fn kamachal_paid_pump_actual_combat_treasure_and_cleanup_preserve_refused_cast_operation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kamachal = scenario
        .add_creature_from_oracle(P0, "Kamachal, Ship's Mascot", 2, 2, KAMACHAL)
        .id();
    let decoy = scenario
        .add_artifact_from_oracle(P0, "Previous Treasure", "")
        .with_subtypes(vec!["Treasure"])
        .id();
    let library = scenario.add_card_to_library_top(P1, "Unrelated library card");
    let old_exile = scenario
        .add_creature_to_exile(P0, "Unrelated prior exiled card", 2, 2)
        .id();
    scenario.with_mana_pool(P0, vec![unit(ManaType::Red)]);
    let mut runner = scenario.build();
    let pump = runner.activate(kamachal, 0).resolve();
    pump.assert_power_toughness(kamachal, 3, 2);
    assert_eq!(pump.mana_pool_total(P0), 0);
    assert!(pump.state().objects[&kamachal]
        .keywords
        .contains(&Keyword::Menace));
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(kamachal, AttackTarget::Player(P1))])
        .unwrap();
    let combat = runner.combat_damage();
    combat.assert_life_delta(P1, -3);
    let treasures: Vec<_> = combat
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            combat.state().objects[id]
                .subtypes
                .iter()
                .any(|subtype| subtype == "Treasure")
        })
        .copied()
        .collect();
    assert_eq!(treasures.len(), 2);
    assert!(treasures.contains(&decoy));
    combat.assert_zone(&[library], Zone::Library);
    combat.assert_zone(&[old_exile], Zone::Exile);
    assert!(combat
        .state()
        .objects
        .values()
        .all(|object| object.casting_permissions.is_empty()));
    assert!(combat.state().stack.is_empty() && combat.state().resolution_stack.is_empty());
    runner.advance_to_upkeep();
    assert_eq!(
        (
            runner.state().objects[&kamachal].power,
            runner.state().objects[&kamachal].toughness
        ),
        (Some(2), Some(2))
    );
}

#[test]
fn you_paid_five_colors_reveals_eight_and_bottoms_only_its_actual_revealed_cohort() {
    for count in [0, 3, 10] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let you = scenario
            .add_creature_from_oracle(P0, "You, Iterative Playtester", 1, 3, YOU)
            .id();
        let foreign = scenario.add_card_to_library_top(P1, "Foreign library sentinel");
        let cards: Vec<_> = (0..count)
            .map(|index| scenario.add_card_to_library_top(P0, &format!("Revealed cohort {index}")))
            .collect();
        scenario.with_mana_pool(
            P0,
            vec![
                unit(ManaType::White),
                unit(ManaType::Blue),
                unit(ManaType::Black),
                unit(ManaType::Red),
                unit(ManaType::Green),
            ],
        );
        let mut runner = scenario.build();
        let before: Vec<_> = runner.state().players[0].library.iter().copied().collect();
        let outcome = runner.activate(you, 1).resolve();
        assert_eq!(outcome.mana_pool_total(P0), 0);
        assert!(outcome.events().iter().any(|event| matches!(event, GameEvent::CardsRevealed { card_ids, .. } if card_ids == &before.iter().take(8).copied().collect::<Vec<_>>())));
        outcome.assert_zone(&cards, Zone::Library);
        outcome.assert_zone(&[foreign], Zone::Library);
        assert_eq!(
            outcome.state().players[1]
                .library
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![foreign]
        );
        assert!(outcome.state().players[0].hand.is_empty());
        let after: Vec<_> = outcome.state().players[0].library.iter().copied().collect();
        assert_eq!(after.len(), before.len());
        assert_eq!(&after[..count.saturating_sub(8)], &before[8.min(count)..]);
        assert!(after.iter().all(|id| cards.contains(id)));
        assert!(outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Reveal,
                ..
            }
        )));
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
        assert!(outcome.state().stack.is_empty() && outcome.state().resolution_stack.is_empty());
    }
}

#[test]
fn unknown_paid_entry_looks_privately_and_bottoms_the_complete_unchosen_ten() {
    for count in [0, 3, 12] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let wizard = scenario
            .add_creature_to_hand_from_oracle(P0, "The Unknown Wizard", 6, 6, UNKNOWN)
            .with_mana_cost(cost(
                2,
                vec![
                    ManaCostShard::White,
                    ManaCostShard::Blue,
                    ManaCostShard::Black,
                    ManaCostShard::Red,
                    ManaCostShard::Green,
                ],
            ))
            .id();
        let cards: Vec<_> = (0..count)
            .map(|index| scenario.add_card_to_library_top(P0, &format!("Private cohort {index}")))
            .collect();
        scenario.with_mana_pool(
            P0,
            vec![
                unit(ManaType::Colorless),
                unit(ManaType::Colorless),
                unit(ManaType::White),
                unit(ManaType::Blue),
                unit(ManaType::Black),
                unit(ManaType::Red),
                unit(ManaType::Green),
            ],
        );
        let mut runner = scenario.build();
        let before: Vec<_> = runner.state().players[0].library.iter().copied().collect();
        let outcome = runner.cast(wizard).resolve();
        outcome.assert_zone(&[wizard], Zone::Battlefield);
        outcome.assert_zone(&cards, Zone::Library);
        assert_eq!(outcome.mana_pool_total(P0), 0);
        assert!(outcome.state().players[0].hand.is_empty());
        assert_eq!(
            outcome.state().players[0]
                .library
                .iter()
                .take(before.len().saturating_sub(10))
                .copied()
                .collect::<Vec<_>>(),
            before[10.min(before.len())..]
        );
        assert!(!outcome
            .events()
            .iter()
            .any(|event| matches!(event, GameEvent::CardsRevealed { .. })));
        assert!(outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Dig,
                ..
            }
        )));
        let opponent = engine::game::visibility::filter_state_for_viewer(outcome.state(), P1);
        for card in &cards {
            assert_ne!(
                opponent.objects[card].name,
                outcome.state().objects[card].name
            );
        }
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
        assert!(outcome.state().stack.is_empty() && outcome.state().resolution_stack.is_empty());
    }
}

#[test]
fn hero_actual_death_keeps_shuffle_while_supported_search_selects_a_nondefault_qualified_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hero = scenario
        .add_creature_from_oracle(P0, "Hero's Uncle", 2, 2, HERO)
        .id();
    let bolt = scenario.add_bolt_to_hand(P0);
    let first = scenario.add_card_to_library_top(P0, "Ordinary library card");
    let second = scenario.add_card_to_library_top(P0, "Another library card");
    scenario.with_mana_pool(P0, vec![unit(ManaType::Red)]);
    let mut runner = scenario.build();
    let outcome = runner.cast(bolt).target_object(hero).resolve();
    outcome.assert_zone(&[hero, bolt], Zone::Graveyard);
    outcome.assert_zone(&[first, second], Zone::Library);
    assert!(outcome.events().iter().any(|event| matches!(
        event,
        GameEvent::EffectResolved {
            kind: EffectKind::Shuffle,
            ..
        }
    )));
    assert!(outcome.state().players[0].hand.is_empty());
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty() && outcome.state().resolution_stack.is_empty());

    // Adjacent supported qualification proves strict admission still reaches
    // the normal search/choice/reveal/hand/shuffle delivery path.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let search = scenario.add_spell_to_hand_from_oracle(P0, "Constructed complete creature tutor", false,
        "Search your library for a creature card, reveal it, put it into your hand, then shuffle.")
        .with_mana_cost(cost(0, vec![ManaCostShard::Green])).id();
    let decoy = scenario.add_card_to_library_top(P0, "Noncreature decoy");
    let selected = scenario
        .add_creature_to_hand(P0, "Nondefault selected creature", 2, 2)
        .id();
    let mut runner = scenario.build();
    engine::game::zones::move_to_zone(runner.state_mut(), selected, Zone::Library, &mut Vec::new());
    runner.state_mut().players[0]
        .mana_pool
        .add(unit(ManaType::Green));
    runner.cast(search).resolve();
    let WaitingFor::SearchChoice { player, cards, .. } = &runner.state().waiting_for else {
        panic!("actual search choice must be reached");
    };
    assert_eq!(*player, P0);
    assert!(cards.contains(&selected));
    assert!(!cards.contains(&decoy));
    let answer = runner
        .act(engine::types::actions::GameAction::SelectCards {
            cards: vec![selected],
        })
        .unwrap();
    assert!(answer.events.iter().any(|event| matches!(event, GameEvent::CardsRevealed { card_ids, .. } if card_ids == &vec![selected])));
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&selected].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&decoy].zone, Zone::Library);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty() && runner.state().resolution_stack.is_empty());
}

#[test]
fn supported_reveal_until_keeps_its_nonland_and_accepts_exact_nondefault_bottom_order() {
    // This constructed supported sibling omits Kaboom's unrepresented
    // per-recipient damage loop and uses the existing reveal/hand/order path.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario.add_spell_to_hand_from_oracle(P0, "Constructed complete reveal-until", false,
        "Reveal cards from the top of your library until you reveal a nonland card. Put that card into your hand and all other cards revealed this way on the bottom of your library in any order.")
        .with_mana_cost(cost(0, vec![ManaCostShard::Blue])).id();
    let deep = scenario.add_card_to_library_top(P0, "Untouched deeper card");
    let hit = scenario
        .add_spell_to_library_top(P0, "Nonland hit", false)
        .id();
    let second = scenario
        .add_land_to_library_top(P0, "Second revealed land")
        .id();
    let first = scenario
        .add_land_to_library_top(P0, "First revealed land")
        .id();
    let foreign = scenario.add_card_to_library_top(P1, "Foreign unrevealed card");
    scenario.with_mana_pool(P0, vec![unit(ManaType::Blue)]);
    let mut runner = scenario.build();
    {
        let _committed = runner.cast(spell).commit();
    }
    runner
        .act(engine::types::actions::GameAction::PassPriority)
        .unwrap();
    let resolved = runner
        .act(engine::types::actions::GameAction::PassPriority)
        .unwrap();
    let WaitingFor::RevealUntilBottomOrder { player, cards, .. } = &runner.state().waiting_for
    else {
        panic!("supported reveal must reach the real bottom-order prompt");
    };
    assert_eq!(*player, P0);
    assert_eq!(cards, &vec![first, second]);
    assert!(resolved.events.iter().any(
        |event| matches!(event, GameEvent::CardsRevealed { card_ids, .. }
        if card_ids == &vec![first, second, hit])
    ));
    assert_eq!(runner.state().objects[&hit].zone, Zone::Hand);
    assert!(runner
        .act(engine::types::actions::GameAction::SelectCards {
            cards: vec![first, first]
        })
        .is_err());
    runner
        .act(engine::types::actions::GameAction::SelectCards {
            cards: vec![second, first],
        })
        .expect("actual nondefault bottom permutation");
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().players[0]
            .library
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![deep, second, first]
    );
    assert_eq!(
        runner.state().players[1]
            .library
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![foreign]
    );
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty() && runner.state().resolution_stack.is_empty());
}
