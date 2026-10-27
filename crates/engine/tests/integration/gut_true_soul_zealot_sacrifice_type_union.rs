//! Gut's attack trigger must offer each independently determined sacrifice type.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::EffectKind;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const GUT: &str = "Whenever you attack, you may sacrifice another creature or an artifact. If you do, create a 4/1 black Skeleton creature token with menace that's tapped and attacking. (It can't be blocked except by two or more creatures.)\nChoose a Background (You can have a Background as a second commander.)";

fn board(
    artifact_controllers: &[PlayerId],
) -> (GameRunner, ObjectId, ObjectId, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gut = scenario
        .add_creature_from_oracle(P0, "Gut, True Soul Zealot", 2, 2, GUT)
        .id();
    let attacker = scenario.add_creature(P0, "Ally", 2, 2).id();
    let fodder = scenario.add_creature(P0, "Fodder", 1, 1).id();
    let artifacts = artifact_controllers
        .iter()
        .map(|&player| scenario.add_artifact_from_oracle(player, "Relic", "").id())
        .collect();
    (scenario.build(), gut, attacker, fodder, artifacts)
}

fn declare_attack(runner: &mut GameRunner, attacker: ObjectId) {
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("attacker declared");
}

fn decide_gut(runner: &mut GameRunner, gut: ObjectId, accept: bool) {
    // CR 508.3d: another creature can declare the attack that triggers Gut.
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice {
                player, source_id, ..
            } => {
                assert_eq!(player, P0);
                assert_eq!(source_id, gut, "decision must be Gut's own trigger");
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("Gut's optional sacrifice decision accepted");
                return;
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority passed");
            }
            other => panic!("Gut trigger did not reach optional decision: {other:?}"),
        }
    }
    panic!("Gut trigger did not reach optional decision");
}

fn choice_cards(runner: &mut GameRunner) -> Vec<ObjectId> {
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                effect_kind: EffectKind::Sacrifice,
                ..
            } => {
                assert_eq!(player, P0);
                return cards;
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority passed");
            }
            other => panic!("Gut never reached a sacrifice choice: {other:?}"),
        }
    }
    panic!("Gut sacrifice choice did not surface");
}

fn settle(runner: &mut GameRunner) {
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("resolve pending trigger");
            }
            other => panic!("Gut resolution did not settle: {other:?}"),
        }
    }
    panic!("Gut trigger did not finish resolving");
}

fn skeletons(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            let object = &runner.state().objects[id];
            object.is_token && object.name == "Skeleton" && object.controller == P0
        })
        .collect()
}

fn assert_printed_skeleton(runner: &GameRunner) {
    let tokens = skeletons(runner);
    assert_eq!(tokens.len(), 1, "the payment creates exactly one Skeleton");
    let token = &runner.state().objects[&tokens[0]];
    assert_eq!((token.power, token.toughness), (Some(4), Some(1)));
    assert!(token.color.contains(&ManaColor::Black));
    assert!(token.card_types.core_types.contains(&CoreType::Creature));
    assert!(token.card_types.subtypes.iter().any(|s| s == "Skeleton"));
    assert!(token.has_keyword(&Keyword::Menace));
    // CR 508.4: the created creature enters tapped and attacking without
    // itself being declared as an attacker.
    assert!(token.tapped);
    let combat = runner
        .state()
        .combat
        .as_ref()
        .expect("combat remains active");
    let attack = combat
        .attackers
        .iter()
        .find(|attack| attack.object_id == tokens[0])
        .expect("Skeleton entered attacking");
    assert_eq!(attack.defending_player, P1);
}

#[test]
fn gut_attack_sacrifice_offers_noncreature_artifact() {
    let (mut runner, gut, attacker, fodder, artifacts) = board(&[P0, P1]);
    let artifact = artifacts[0];
    let opponent_artifact = artifacts[1];
    assert!(
        !runner.state().objects[&artifact]
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "the artifact fixture must not qualify for the creature leg"
    );

    declare_attack(&mut runner, attacker);
    decide_gut(&mut runner, gut, true);
    let cards = choice_cards(&mut runner);
    // CR 701.21a: a controller can sacrifice their noncreature artifact.
    assert!(cards.contains(&attacker), "creature leg must be live");
    assert!(cards.contains(&fodder), "second creature forces a choice");
    assert!(
        cards.contains(&artifact),
        "artifact leg must be live: {cards:?}"
    );
    assert!(
        !cards.contains(&opponent_artifact),
        "an opponent's artifact cannot be sacrificed: {cards:?}"
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![artifact],
        })
        .expect("selecting artifact is legal");
    settle(&mut runner);
    assert_eq!(runner.state().objects[&artifact].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Battlefield);
    assert_printed_skeleton(&runner);
}

#[test]
fn gut_can_sacrifice_the_other_creature_instead() {
    let (mut runner, gut, attacker, fodder, artifacts) = board(&[P0]);
    let artifact = artifacts[0];
    declare_attack(&mut runner, attacker);
    decide_gut(&mut runner, gut, true);
    let cards = choice_cards(&mut runner);
    assert!(cards.contains(&fodder) && cards.contains(&artifact));
    assert!(
        !cards.contains(&gut),
        "ordinary Gut is not another creature"
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![fodder],
        })
        .expect("select another creature");
    settle(&mut runner);
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&artifact].zone, Zone::Battlefield);
    assert_printed_skeleton(&runner);
}

#[test]
fn declining_gut_sacrifice_skips_its_token() {
    let (mut runner, gut, attacker, fodder, artifacts) = board(&[P0]);
    declare_attack(&mut runner, attacker);
    decide_gut(&mut runner, gut, false);
    settle(&mut runner);
    assert!(skeletons(&runner).is_empty(), "declining produces no token");
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&artifacts[0]].zone,
        Zone::Battlefield
    );
}

#[test]
fn gut_without_eligible_payment_cannot_create_a_skeleton() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gut = scenario
        .add_creature_from_oracle(P0, "Gut, True Soul Zealot", 2, 2, GUT)
        .id();
    let mut runner = scenario.build();
    declare_attack(&mut runner, gut);
    decide_gut(&mut runner, gut, true);
    settle(&mut runner);
    assert!(skeletons(&runner).is_empty(), "no payment means no token");
    assert_eq!(runner.state().objects[&gut].zone, Zone::Battlefield);
}

#[test]
fn artifact_creature_gut_can_pay_with_itself_and_keep_its_trigger() {
    let (mut runner, gut, attacker, fodder, artifacts) = board(&[P0]);
    {
        let object = runner
            .state_mut()
            .objects
            .get_mut(&gut)
            .expect("Gut exists");
        object.card_types.core_types.push(CoreType::Artifact);
        object.base_card_types = object.card_types.clone();
    }
    declare_attack(&mut runner, attacker);
    decide_gut(&mut runner, gut, true);
    let cards = choice_cards(&mut runner);
    assert!(
        cards.contains(&gut),
        "Gut qualifies on unrestricted artifact leg"
    );
    assert!(
        cards.contains(&fodder),
        "other creature forces an explicit choice"
    );
    assert!(
        cards.contains(&artifacts[0]),
        "other artifact stays eligible"
    );
    runner
        .act(GameAction::SelectCards { cards: vec![gut] })
        .expect("Gut can sacrifice itself as an artifact");
    settle(&mut runner);
    // CR 113.7a: removing the source does not remove its triggered ability.
    assert_eq!(runner.state().objects[&gut].zone, Zone::Graveyard);
    assert_printed_skeleton(&runner);
}

#[test]
fn another_optional_attack_trigger_does_not_pay_guts_if_you_do() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gut = scenario
        .add_creature_from_oracle(P0, "Gut, True Soul Zealot", 2, 2, GUT)
        .id();
    let other = scenario
        .add_creature_from_oracle(
            P0,
            "Synthetic Optional Attack",
            1,
            1,
            "Whenever you attack, you may gain 1 life.",
        )
        .id();
    let attacker = scenario.add_creature(P0, "Ally", 2, 2).id();
    scenario.add_artifact_from_oracle(P0, "Relic", "");
    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;
    declare_attack(&mut runner, attacker);
    let mut gut_decided = false;
    let mut sibling_decided = false;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::OptionalEffectChoice { source_id, .. } => {
                let accept = if source_id == gut {
                    gut_decided = true;
                    false
                } else {
                    assert_eq!(source_id, other);
                    sibling_decided = true;
                    true
                };
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("optional decision accepted");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority passed");
            }
            other_wait => panic!("unexpected optional attack trigger wait: {other_wait:?}"),
        }
    }
    assert!(
        gut_decided && sibling_decided,
        "both triggers must reach resolution"
    );
    assert_eq!(runner.state().players[P0.0 as usize].life, life_before + 1);
    assert!(
        skeletons(&runner).is_empty(),
        "Gut's decline gates only its token"
    );
}

// Whole pinned Oracle faces for every adjudicated optional-sacrifice population.
const OPTIONAL_SACRIFICE_PRINTED: &[&str] = &[
    r###"{"name":"Ajani's Last Stand","manaCost":"{2}{W}{W}","manaValue":4.0,"type":"Enchantment","types":["Enchantment"],"subtypes":[],"supertypes":[],"colors":["W"],"colorIdentity":["W"],"layout":"normal","text":"Whenever a creature or planeswalker you control dies, you may sacrifice this enchantment. If you do, create a 4/4 white Avatar creature token with flying.\nWhen a spell or ability an opponent controls causes you to discard this card, if you control a Plains, create a 4/4 white Avatar creature token with flying.","identifiers":{"scryfallOracleId":"17c4807c-158e-4a86-90f1-d8fb2ebe892a"}}"###,
    r###"{"name":"Angelic Renewal","manaCost":"{1}{W}","manaValue":2.0,"type":"Enchantment","types":["Enchantment"],"subtypes":[],"supertypes":[],"colors":["W"],"colorIdentity":["W"],"layout":"normal","text":"Whenever a creature is put into your graveyard from the battlefield, you may sacrifice this enchantment. If you do, return that card to the battlefield.","identifiers":{"scryfallOracleId":"f86bcdde-8b1f-4efa-a01d-39e09dc826d0"}}"###,
    r###"{"name":"Automated Warfare System","manaCost":"{2}{B}","manaValue":3.0,"type":"Artifact Creature — Construct","types":["Artifact","Creature"],"subtypes":["Construct"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"power":"2","toughness":"2","layout":"normal","text":"Flying\nWhenever this creature attacks, you may sacrifice another creature or artifact. If you do, draw a card.","keywords":["Flying"],"identifiers":{"scryfallOracleId":"b28b4fe9-1a35-48c1-8818-f6f786b04e3e"}}"###,
    r###"{"name":"Beetle-Headed Merchants","manaCost":"{4}{B}","manaValue":5.0,"type":"Creature — Human Citizen","types":["Creature"],"subtypes":["Human","Citizen"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"power":"5","toughness":"4","layout":"normal","text":"Whenever this creature attacks, you may sacrifice another creature or artifact. If you do, draw a card and put a +1/+1 counter on this creature.","identifiers":{"scryfallOracleId":"0e6609b4-f359-4bf1-a06e-f4317a0e7b3c"}}"###,
    r###"{"name":"Blind Zealot","manaCost":"{1}{B}{B}","manaValue":3.0,"type":"Creature — Phyrexian Human Cleric","types":["Creature"],"subtypes":["Phyrexian","Human","Cleric"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"power":"2","toughness":"2","layout":"normal","text":"Intimidate (This creature can't be blocked except by artifact creatures and/or creatures that share a color with it.)\nWhenever this creature deals combat damage to a player, you may sacrifice it. If you do, destroy target creature that player controls.","keywords":["Intimidate"],"identifiers":{"scryfallOracleId":"30c7e304-ecfe-44c1-88d1-f507df63c080"}}"###,
    r###"{"name":"Blood Speaker","manaCost":"{3}{B}","manaValue":4.0,"type":"Creature — Ogre Shaman","types":["Creature"],"subtypes":["Ogre","Shaman"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"power":"3","toughness":"2","layout":"normal","text":"At the beginning of your upkeep, you may sacrifice this creature. If you do, search your library for a Demon card, reveal that card, put it into your hand, then shuffle.\nWhenever a Demon you control enters, return this card from your graveyard to your hand.","identifiers":{"scryfallOracleId":"2659c0ea-9ae0-4be3-b9f7-5a38230815ad"}}"###,
    r###"{"name":"Boilerbilges Ripper","manaCost":"{4}{R}","manaValue":5.0,"type":"Creature — Human Assassin","types":["Creature"],"subtypes":["Human","Assassin"],"supertypes":[],"colors":["R"],"colorIdentity":["R"],"power":"4","toughness":"4","layout":"normal","text":"When this creature enters, you may sacrifice another creature or enchantment. When you do, this creature deals 2 damage to any target.","identifiers":{"scryfallOracleId":"0cbe2a00-4303-4ade-98b5-4172d97f15fb"}}"###,
    r###"{"name":"Braids, Arisen Nightmare","manaCost":"{1}{B}{B}","manaValue":3.0,"type":"Legendary Creature — Nightmare","types":["Creature"],"subtypes":["Nightmare"],"supertypes":["Legendary"],"colors":["B"],"colorIdentity":["B"],"power":"3","toughness":"3","layout":"normal","text":"At the beginning of your end step, you may sacrifice an artifact, creature, enchantment, land, or planeswalker. If you do, each opponent may sacrifice a permanent of their choice that shares a card type with it. For each opponent who doesn't, that player loses 2 life and you draw a card.","identifiers":{"scryfallOracleId":"e0445c80-fa53-4c3e-881e-940e9fce7f57"}}"###,
    r###"{"name":"Buzzard-Wasp Colony","manaCost":"{3}{B}","manaValue":4.0,"type":"Creature — Bird Insect","types":["Creature"],"subtypes":["Bird","Insect"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"power":"2","toughness":"2","layout":"normal","text":"Flying\nWhen this creature enters, you may sacrifice an artifact or creature. If you do, draw a card.\nWhenever another creature you control dies, if it had counters on it, put its counters on this creature.","keywords":["Flying"],"identifiers":{"scryfallOracleId":"29fd050c-dd1d-4e1b-b3cc-f7d8d18e4dcb"}}"###,
    r###"{"name":"Cacophony Scamp","manaCost":"{R}","manaValue":1.0,"type":"Creature — Phyrexian Goblin Warrior","types":["Creature"],"subtypes":["Phyrexian","Goblin","Warrior"],"supertypes":[],"colors":["R"],"colorIdentity":["R"],"power":"1","toughness":"1","layout":"normal","text":"Whenever this creature deals combat damage to a player, you may sacrifice it. If you do, proliferate. (Choose any number of permanents and/or players, then give each another counter of each kind already there.)\nWhen this creature dies, it deals damage equal to its power to any target.","keywords":["Proliferate"],"identifiers":{"scryfallOracleId":"4854a41e-db49-462f-ae57-0ba4005bae75"}}"###,
    r###"{"name":"Campsite Cuisine","manaCost":"{1}{G}","manaValue":2.0,"type":"Enchantment","types":["Enchantment"],"subtypes":[],"supertypes":[],"colors":["G"],"colorIdentity":["G"],"layout":"normal","text":"Whenever this enchantment or a legendary creature you control enters, create a Food token.\nWhenever you attack, you may sacrifice X Foods. When you do, up to X target attacking creatures each get +3/+3 and gain trample and indestructible until end of turn.","keywords":["Food"],"identifiers":{"scryfallOracleId":"45f58002-c831-4b35-8b8c-9a502118e2ec"}}"###,
    r###"{"name":"Chitterspitter","manaCost":"{2}{G}","manaValue":3.0,"type":"Artifact","types":["Artifact"],"subtypes":[],"supertypes":[],"colors":["G"],"colorIdentity":["G"],"layout":"normal","text":"At the beginning of your upkeep, you may sacrifice a token. If you do, put an acorn counter on this artifact.\nSquirrels you control get +1/+1 for each acorn counter on this artifact.\n{G}, {T}: Create a 1/1 green Squirrel creature token.","identifiers":{"scryfallOracleId":"4a338863-d599-46e7-9c30-e11b898ae1b0"}}"###,
    r###"{"name":"Comet Crawler","manaCost":"{2}{B}","manaValue":3.0,"type":"Creature — Insect Horror","types":["Creature"],"subtypes":["Insect","Horror"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"power":"2","toughness":"3","layout":"normal","text":"Lifelink\nWhenever this creature attacks, you may sacrifice another creature or artifact. If you do, this creature gets +2/+0 until end of turn.","keywords":["Lifelink"],"identifiers":{"scryfallOracleId":"0c1b6305-8863-43e0-b50d-ae1c5e31be55"}}"###,
    r###"{"name":"Curse of Silence","manaCost":"{W}","manaValue":1.0,"type":"Enchantment — Aura Curse","types":["Enchantment"],"subtypes":["Aura","Curse"],"supertypes":[],"colors":["W"],"colorIdentity":["W"],"layout":"normal","text":"Enchant player\nAs this Aura enters, choose a card name.\nSpells with the chosen name enchanted player casts cost {2} more to cast.\nWhenever enchanted player casts a spell with the chosen name, you may sacrifice this Aura. If you do, draw a card.","keywords":["Enchant"],"identifiers":{"scryfallOracleId":"36e267c5-ea98-4e26-b2b4-3faa05493eb4"}}"###,
    r###"{"name":"Dauthi Mindripper","manaCost":"{3}{B}","manaValue":4.0,"type":"Creature — Dauthi Minion","types":["Creature"],"subtypes":["Dauthi","Minion"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"power":"2","toughness":"1","layout":"normal","text":"Shadow (This creature can block or be blocked by only creatures with shadow.)\nWhenever this creature attacks and isn't blocked, you may sacrifice it. If you do, defending player discards three cards.","keywords":["Shadow"],"identifiers":{"scryfallOracleId":"ecae5eba-8407-4c71-aeae-ab10fb1e5226"}}"###,
    r###"{"name":"Descendants' Fury","manaCost":"{3}{R}","manaValue":4.0,"type":"Enchantment","types":["Enchantment"],"subtypes":[],"supertypes":[],"colors":["R"],"colorIdentity":["R"],"layout":"normal","text":"Whenever one or more creatures you control deal combat damage to a player, you may sacrifice one of them. If you do, reveal cards from the top of your library until you reveal a creature card that shares a creature type with the sacrificed creature. Put that card onto the battlefield and the rest on the bottom of your library in a random order.","identifiers":{"scryfallOracleId":"97079964-7a2e-42c5-ad4a-9015c18a1e97"}}"###,
    r###"{"name":"Disturbing Mirth","manaCost":"{B}{R}","manaValue":2.0,"type":"Enchantment","types":["Enchantment"],"subtypes":[],"supertypes":[],"colors":["B","R"],"colorIdentity":["B","R"],"layout":"normal","text":"When this enchantment enters, you may sacrifice another enchantment or creature. If you do, draw two cards.\nWhen you sacrifice this enchantment, manifest dread. (Look at the top two cards of your library. Put one onto the battlefield face down as a 2/2 creature and the other into your graveyard. Turn it face up any time for its mana cost if it's a creature card.)","keywords":["Manifest","Manifest dread"],"identifiers":{"scryfallOracleId":"7010bfbb-8aea-473b-a9a5-27488de13cc5"}}"###,
    r###"{"name":"Dreamcatcher","manaCost":"{U}","manaValue":1.0,"type":"Creature — Spirit","types":["Creature"],"subtypes":["Spirit"],"supertypes":[],"colors":["U"],"colorIdentity":["U"],"power":"1","toughness":"1","layout":"normal","text":"Whenever you cast a Spirit or Arcane spell, you may sacrifice this creature. If you do, draw a card.","identifiers":{"scryfallOracleId":"61529c3a-9b7e-4e73-9f6e-58c992ef1ffc"}}"###,
    r###"{"name":"Euru, Acorn Scrounger","manaCost":"{2}{B}{G}","manaValue":4.0,"type":"Legendary Creature — Squirrel Soldier","types":["Creature"],"subtypes":["Squirrel","Soldier"],"supertypes":["Legendary"],"colors":["B","G"],"colorIdentity":["B","G"],"power":"3","toughness":"3","layout":"normal","text":"When Euru enters, you may forage. When you do, conjure a card named Chitterspitter onto the battlefield. (To forage, exile three cards from your graveyard or sacrifice a Food.)\nWhenever one or more Squirrels you control deal combat damage to a player, you may sacrifice a token. If you do, put an acorn counter on each permanent you control named Chitterspitter.","keywords":["Conjure","Forage"],"identifiers":{"scryfallOracleId":"b01657ad-120f-4d00-a367-fd112d403700"}}"###,
    r###"{"name":"Flamespeaker's Will","manaCost":"{R}","manaValue":1.0,"type":"Enchantment — Aura","types":["Enchantment"],"subtypes":["Aura"],"supertypes":[],"colors":["R"],"colorIdentity":["R"],"layout":"normal","text":"Enchant creature you control\nEnchanted creature gets +1/+1.\nWhenever enchanted creature deals combat damage to a player, you may sacrifice this Aura. If you do, destroy target artifact.","keywords":["Enchant"],"identifiers":{"scryfallOracleId":"3abc09a9-3baa-45ff-b048-53e42d4fa9f2"}}"###,
    r###"{"name":"Flaxen Intruder // Welcome Home","faceName":"Flaxen Intruder","side":"a","manaCost":"{G}","manaValue":1.0,"type":"Creature — Human Berserker","types":["Creature"],"subtypes":["Human","Berserker"],"supertypes":[],"colors":["G"],"colorIdentity":["G"],"power":"1","toughness":"2","layout":"adventure","text":"Whenever this creature deals combat damage to a player, you may sacrifice it. When you do, destroy target artifact or enchantment.","identifiers":{"scryfallOracleId":"bacedc99-46d9-4757-8a27-8df77d7c2f02"}}"###,
    r###"{"name":"Foot Chopper","manaCost":"{1}{B}","manaValue":2.0,"type":"Artifact — Equipment","types":["Artifact"],"subtypes":["Equipment"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"layout":"normal","text":"When this Equipment enters, create a 1/1 black Ninja creature token, then attach this Equipment to it.\nEquipped creature has flying.\nWhenever equipped creature deals combat damage to a player, you may sacrifice it. If you do, draw cards equal to its power.\nEquip {2}","keywords":["Equip"],"identifiers":{"scryfallOracleId":"1eed017a-342a-47a8-9395-890ef0bd1eb2"}}"###,
    r###"{"name":"Furnace Scamp","manaCost":"{R}","manaValue":1.0,"type":"Creature — Phyrexian Beast","types":["Creature"],"subtypes":["Phyrexian","Beast"],"supertypes":[],"colors":["R"],"colorIdentity":["R"],"power":"1","toughness":"1","layout":"normal","text":"Whenever this creature deals combat damage to a player, you may sacrifice it. If you do, this creature deals 3 damage to that player.","identifiers":{"scryfallOracleId":"d0868a52-5b19-4429-acf1-399f3ae8308c"}}"###,
    r###"{"name":"Gastal Blockbuster","manaCost":"{2}{R}","manaValue":3.0,"type":"Creature — Human Berserker","types":["Creature"],"subtypes":["Human","Berserker"],"supertypes":[],"colors":["R"],"colorIdentity":["R"],"power":"3","toughness":"2","layout":"normal","text":"When this creature enters, you may sacrifice a creature or Vehicle. When you do, destroy target artifact an opponent controls.","identifiers":{"scryfallOracleId":"3b601721-90ca-45d3-9a3a-19c719799bfa"}}"###,
    r###"{"name":"Glorifier of Suffering","manaCost":"{2}{W}","manaValue":3.0,"type":"Creature — Vampire Soldier","types":["Creature"],"subtypes":["Vampire","Soldier"],"supertypes":[],"colors":["W"],"colorIdentity":["W"],"power":"3","toughness":"2","layout":"normal","text":"When this creature enters, you may sacrifice another creature or artifact. When you do, put a +1/+1 counter on each of up to two target creatures.","identifiers":{"scryfallOracleId":"07a8ca73-9890-4c87-8b39-fd9eaea5ef45"}}"###,
    r###"{"name":"Goblin Grenadiers","manaCost":"{3}{R}","manaValue":4.0,"type":"Creature — Goblin","types":["Creature"],"subtypes":["Goblin"],"supertypes":[],"colors":["R"],"colorIdentity":["R"],"power":"2","toughness":"2","layout":"normal","text":"Whenever this creature attacks and isn't blocked, you may sacrifice it. If you do, destroy target creature and target land.","identifiers":{"scryfallOracleId":"be83a518-cfab-46b1-9fe9-7e3d54311c37"}}"###,
    r###"{"name":"Gut, True Soul Zealot","manaCost":"{2}{R}","manaValue":3.0,"type":"Legendary Creature — Goblin Shaman","types":["Creature"],"subtypes":["Goblin","Shaman"],"supertypes":["Legendary"],"colors":["R"],"colorIdentity":["R"],"power":"2","toughness":"2","layout":"normal","text":"Whenever you attack, you may sacrifice another creature or an artifact. If you do, create a 4/1 black Skeleton creature token with menace that's tapped and attacking. (It can't be blocked except by two or more creatures.)\nChoose a Background (You can have a Background as a second commander.)","keywords":["Choose a background"],"identifiers":{"scryfallOracleId":"60e7130a-d321-4e50-93a4-999013c76054"}}"###,
    r###"{"name":"Harvester Troll","manaCost":"{3}{G}","manaValue":4.0,"type":"Creature — Troll","types":["Creature"],"subtypes":["Troll"],"supertypes":[],"colors":["G"],"colorIdentity":["G"],"power":"2","toughness":"3","layout":"normal","text":"When this creature enters, you may sacrifice a creature or land. If you do, put two +1/+1 counters on this creature.","identifiers":{"scryfallOracleId":"28c44f88-1d47-4894-86ce-4089bd761360"}}"###,
    r###"{"name":"Haunted Cadaver","manaCost":"{3}{B}","manaValue":4.0,"type":"Creature — Zombie","types":["Creature"],"subtypes":["Zombie"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"power":"2","toughness":"2","layout":"normal","text":"Whenever this creature deals combat damage to a player, you may sacrifice it. If you do, that player discards three cards.\nMorph {1}{B} (You may cast this card face down as a 2/2 creature for {3}. Turn it face up any time for its morph cost.)","keywords":["Morph"],"identifiers":{"scryfallOracleId":"7f46839f-175d-4292-8057-c3e0da206075"}}"###,
    r###"{"name":"Hei Bai, Spirit of Balance","manaCost":"{2}{W/B}{W/B}","manaValue":4.0,"type":"Legendary Creature — Bear Spirit","types":["Creature"],"subtypes":["Bear","Spirit"],"supertypes":["Legendary"],"colors":["B","W"],"colorIdentity":["B","W"],"power":"3","toughness":"3","layout":"normal","text":"Whenever Hei Bai enters or attacks, you may sacrifice another creature or artifact. If you do, put two +1/+1 counters on Hei Bai.\nWhen Hei Bai leaves the battlefield, put its counters on target creature you control.","identifiers":{"scryfallOracleId":"8f709660-bb26-409f-9650-e5228ac8db7e"}}"###,
    r###"{"name":"Impaler Shrike","manaCost":"{2}{U}{U}","manaValue":4.0,"type":"Creature — Phyrexian Bird","types":["Creature"],"subtypes":["Phyrexian","Bird"],"supertypes":[],"colors":["U"],"colorIdentity":["U"],"power":"3","toughness":"1","layout":"normal","text":"Flying\nWhenever this creature deals combat damage to a player, you may sacrifice it. If you do, draw three cards.","keywords":["Flying"],"identifiers":{"scryfallOracleId":"db98ee99-d4e5-4623-8446-5cce5d11dca0"}}"###,
    r###"{"name":"Invasion of New Capenna // Holy Frazzle-Cannon","faceName":"Invasion of New Capenna","side":"a","manaCost":"{W}{B}","manaValue":2.0,"type":"Battle — Siege","types":["Battle"],"subtypes":["Siege"],"supertypes":[],"colors":["B","W"],"colorIdentity":["B","W"],"defense":"4","layout":"transform","text":"(As a Siege enters, choose an opponent to protect it. You and others can attack it. When it's defeated, exile it, then cast it transformed.)\nWhen this Siege enters, you may sacrifice an artifact or creature. When you do, exile target artifact or creature an opponent controls.","keywords":["Equip","Transform"],"identifiers":{"scryfallOracleId":"530dc119-e266-4525-abe9-7fab599e9de2"}}"###,
    r###"{"name":"Kill-Zone Acrobat","manaCost":"{2}{B}","manaValue":3.0,"type":"Creature — Human Soldier","types":["Creature"],"subtypes":["Human","Soldier"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"power":"3","toughness":"2","layout":"normal","text":"Whenever this creature attacks, you may sacrifice another creature or artifact. If you do, this creature gains flying until end of turn.","identifiers":{"scryfallOracleId":"7c3982c5-6af3-4937-8d1f-e006f4e89342"}}"###,
    r###"{"name":"Larval Scoutlander","manaCost":"{2}{G}","manaValue":3.0,"type":"Artifact — Spacecraft","types":["Artifact"],"subtypes":["Spacecraft"],"supertypes":[],"colors":["G"],"colorIdentity":["G"],"power":"3","toughness":"3","layout":"normal","text":"When this Spacecraft enters, you may sacrifice a land or Lander. If you do, search your library for up to two basic land cards, put them onto the battlefield tapped, then shuffle.\nStation (Tap another creature you control: Put charge counters equal to its power on this Spacecraft. Station only as a sorcery. It's an artifact creature at 7+.)\n7+ | Flying","keywords":["Station"],"identifiers":{"scryfallOracleId":"b3f3d55c-a3f6-441b-a380-60c0734dc514"}}"###,
    r###"{"name":"Mindstab Thrull","manaCost":"{1}{B}{B}","manaValue":3.0,"type":"Creature — Thrull","types":["Creature"],"subtypes":["Thrull"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"power":"2","toughness":"2","layout":"normal","text":"Whenever this creature attacks and isn't blocked, you may sacrifice it. If you do, defending player discards three cards.","identifiers":{"scryfallOracleId":"43abe1f3-7b92-411a-bf93-e158f949685c"}}"###,
    r###"{"name":"Mortal Obstinacy","manaCost":"{W}","manaValue":1.0,"type":"Enchantment — Aura","types":["Enchantment"],"subtypes":["Aura"],"supertypes":[],"colors":["W"],"colorIdentity":["W"],"layout":"normal","text":"Enchant creature you control\nEnchanted creature gets +1/+1.\nWhenever enchanted creature deals combat damage to a player, you may sacrifice this Aura. If you do, destroy target enchantment.","keywords":["Enchant"],"identifiers":{"scryfallOracleId":"ea29c489-bd04-4f36-8b59-bccaac153152"}}"###,
    r###"{"name":"Mukotai Soulripper","manaCost":"{1}{B}","manaValue":2.0,"type":"Artifact — Vehicle","types":["Artifact"],"subtypes":["Vehicle"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"power":"4","toughness":"3","layout":"normal","text":"Whenever this Vehicle attacks, you may sacrifice another artifact or creature. If you do, put a +1/+1 counter on this Vehicle and it gains menace until end of turn.\nCrew 2 (Tap any number of creatures you control with total power 2 or more: This Vehicle becomes an artifact creature until end of turn.)","keywords":["Crew"],"identifiers":{"scryfallOracleId":"060d109f-2969-4f9e-a723-535d662a571a"}}"###,
    r###"{"name":"Namazu Trader","manaCost":"{3}{B}","manaValue":4.0,"type":"Creature — Fish Citizen","types":["Creature"],"subtypes":["Fish","Citizen"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"power":"3","toughness":"4","layout":"normal","text":"When this creature enters, you lose 1 life and create a Treasure token.\nWhenever this creature attacks, you may sacrifice another creature or artifact. If you do, surveil 2. (Look at the top two cards of your library, then put any number of them into your graveyard and the rest on top of your library in any order.)","keywords":["Surveil","Treasure"],"identifiers":{"scryfallOracleId":"3d01c351-6dbd-4d63-9843-3aa1a3c039a6"}}"###,
    r###"{"name":"Necrite","manaCost":"{1}{B}{B}","manaValue":3.0,"type":"Creature — Thrull","types":["Creature"],"subtypes":["Thrull"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"power":"2","toughness":"2","layout":"normal","text":"Whenever this creature attacks and isn't blocked, you may sacrifice it. If you do, destroy target creature defending player controls. It can't be regenerated.","identifiers":{"scryfallOracleId":"e6ab8a04-e225-4ff0-bb8b-c80d0e6ce50c"}}"###,
    r###"{"name":"Ogre-Head Helm","manaCost":"{1}{R}","manaValue":2.0,"type":"Artifact Creature — Equipment Ogre","types":["Artifact","Creature"],"subtypes":["Equipment","Ogre"],"supertypes":[],"colors":["R"],"colorIdentity":["R"],"power":"2","toughness":"2","layout":"normal","text":"Equipped creature gets +2/+2.\nWhenever this creature or equipped creature deals combat damage to a player, you may sacrifice it. If you do, discard your hand, then draw three cards.\nReconfigure {3} ({3}: Attach to target creature you control; or unattach from a creature. Reconfigure only as a sorcery. While attached, this isn't a creature.)","keywords":["Reconfigure"],"identifiers":{"scryfallOracleId":"b73c297a-1c39-450f-99c3-c8daa31c5f3f"}}"###,
    r###"{"name":"Pitchstone Wall","manaCost":"{2}{R}","manaValue":3.0,"type":"Creature — Wall","types":["Creature"],"subtypes":["Wall"],"supertypes":[],"colors":["R"],"colorIdentity":["R"],"power":"2","toughness":"5","layout":"normal","text":"Defender (This creature can't attack.)\nWhenever you discard a card, you may sacrifice this creature. If you do, return the discarded card from your graveyard to your hand.","keywords":["Defender"],"identifiers":{"scryfallOracleId":"da6a9a2e-7bed-46d7-b7dc-18d4a220b81e"}}"###,
    r###"{"name":"Riptide Entrancer","manaCost":"{1}{U}{U}","manaValue":3.0,"type":"Creature — Human Wizard","types":["Creature"],"subtypes":["Human","Wizard"],"supertypes":[],"colors":["U"],"colorIdentity":["U"],"power":"1","toughness":"1","layout":"normal","text":"Whenever this creature deals combat damage to a player, you may sacrifice it. If you do, gain control of target creature that player controls. (This effect lasts indefinitely.)\nMorph {U}{U} (You may cast this card face down as a 2/2 creature for {3}. Turn it face up any time for its morph cost.)","keywords":["Morph"],"identifiers":{"scryfallOracleId":"d0bf59a2-347c-4c3e-85cf-b27a2af77192"}}"###,
    r###"{"name":"Safe Haven","manaValue":0.0,"type":"Land","types":["Land"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":[],"layout":"normal","text":"{2}, {T}: Exile target creature you control.\nAt the beginning of your upkeep, you may sacrifice this land. If you do, return each card exiled with this land to the battlefield under its owner's control.","identifiers":{"scryfallOracleId":"8f7f4061-8998-40e9-b980-c63f27e7a5cd"}}"###,
    r###"{"name":"Sanctum of Ugin","manaValue":0.0,"type":"Land","types":["Land"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":[],"layout":"normal","text":"{T}: Add {C}.\nWhenever you cast a colorless spell with mana value 7 or greater, you may sacrifice this land. If you do, search your library for a colorless creature card, reveal it, put it into your hand, then shuffle.","identifiers":{"scryfallOracleId":"72cb5dcd-9b24-435c-921a-3766108374c4"}}"###,
    r###"{"name":"Saw","manaCost":"{2}","manaValue":2.0,"type":"Artifact — Equipment","types":["Artifact"],"subtypes":["Equipment"],"supertypes":[],"colors":[],"colorIdentity":[],"layout":"normal","text":"Equipped creature gets +2/+0.\nWhenever equipped creature attacks, you may sacrifice a permanent other than that creature or this Equipment. If you do, draw a card.\nEquip {2} ({2}: Attach to target creature you control. Equip only as a sorcery.)","keywords":["Equip"],"identifiers":{"scryfallOracleId":"d88bfa98-6869-4d64-a9d2-07a308a67fe2"}}"###,
    r###"{"name":"Sephiroth, Fabled SOLDIER // Sephiroth, One-Winged Angel","faceName":"Sephiroth, One-Winged Angel","side":"b","manaValue":3.0,"type":"Legendary Creature — Angel Nightmare Avatar","types":["Creature"],"subtypes":["Angel","Nightmare","Avatar"],"supertypes":["Legendary"],"colors":["B"],"colorIdentity":["B"],"power":"5","toughness":"5","layout":"transform","text":"Flying\nSuper Nova — As this creature transforms into Sephiroth, One-Winged Angel, you get an emblem with \"Whenever a creature dies, target opponent loses 1 life and you gain 1 life.\"\nWhenever Sephiroth attacks, you may sacrifice any number of other creatures. If you do, draw that many cards.","keywords":["Flying","Super Nova","Transform"],"identifiers":{"scryfallOracleId":"70113003-be5e-406a-9aec-cb480468c36d"}}"###,
    r###"{"name":"Seraph of New Capenna // Seraph of New Phyrexia","faceName":"Seraph of New Phyrexia","side":"b","manaValue":3.0,"type":"Creature — Phyrexian Angel","types":["Creature"],"subtypes":["Phyrexian","Angel"],"supertypes":[],"colors":["B","W"],"colorIdentity":["B","W"],"power":"3","toughness":"3","layout":"transform","text":"Flying\nWhenever this creature attacks, you may sacrifice another creature or artifact. If you do, this creature gets +2/+1 until end of turn.","keywords":["Flying","Transform"],"identifiers":{"scryfallOracleId":"08574d1b-4b76-42dd-884b-29c5a61cfdda"}}"###,
    r###"{"name":"Servant of the Stinger","manaCost":"{1}{B}","manaValue":2.0,"type":"Creature — Human Warlock","types":["Creature"],"subtypes":["Human","Warlock"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"power":"1","toughness":"3","layout":"normal","text":"Deathtouch\nWhenever this creature deals combat damage to a player, if you've committed a crime this turn, you may sacrifice this creature. If you do, search your library for a card, put it into your hand, then shuffle. (Targeting opponents, anything they control, and/or cards in their graveyards is a crime.)","keywords":["Deathtouch"],"identifiers":{"scryfallOracleId":"0c9ec9a1-738a-4f69-b367-5dbf3d046bf1"}}"###,
    r###"{"name":"Shire Shirriff","manaCost":"{1}{W}","manaValue":2.0,"type":"Creature — Halfling Soldier","types":["Creature"],"subtypes":["Halfling","Soldier"],"supertypes":[],"colors":["W"],"colorIdentity":["W"],"power":"2","toughness":"2","layout":"normal","text":"Vigilance\nWhen this creature enters, you may sacrifice a token. When you do, exile target creature an opponent controls until this creature leaves the battlefield.","keywords":["Vigilance"],"identifiers":{"scryfallOracleId":"14f36157-f55f-495d-9ffe-f38135a59750"}}"###,
    r###"{"name":"Spare Dagger","manaCost":"{1}","manaValue":1.0,"type":"Artifact — Equipment","types":["Artifact"],"subtypes":["Equipment"],"supertypes":[],"colors":[],"colorIdentity":[],"layout":"normal","text":"Equipped creature gets +1/+0 and has \"Whenever this creature attacks, you may sacrifice Spare Dagger. When you do, this creature deals 1 damage to any target.\"\nEquip {1} ({1}: Attach to target creature you control. Equip only as a sorcery.)","keywords":["Equip"],"identifiers":{"scryfallOracleId":"d895df97-e7e7-4f15-a79b-0ce8f1a252d3"}}"###,
    r###"{"name":"Sunfire Torch","manaCost":"{R}","manaValue":1.0,"type":"Artifact — Equipment","types":["Artifact"],"subtypes":["Equipment"],"supertypes":[],"colors":["R"],"colorIdentity":["R"],"layout":"normal","text":"Equipped creature gets +1/+0 and has \"Whenever this creature attacks, you may sacrifice Sunfire Torch. When you do, this creature deals 2 damage to any target.\"\nEquip {1} ({1}: Attach to target creature you control. Equip only as a sorcery.)","keywords":["Equip"],"identifiers":{"scryfallOracleId":"d2da26d6-b0f7-4af0-9ce8-fb8d82fe2d85"}}"###,
    r###"{"name":"Swarm Culler","manaCost":"{3}{B}","manaValue":4.0,"type":"Creature — Insect Warrior","types":["Creature"],"subtypes":["Insect","Warrior"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"power":"2","toughness":"4","layout":"normal","text":"Flying\nWhenever this creature becomes tapped, you may sacrifice another creature or artifact. If you do, draw a card.","keywords":["Flying"],"identifiers":{"scryfallOracleId":"a6c28bde-10d9-471b-acd0-32a1c6195805"}}"###,
    r###"{"name":"Swashbuckler Extraordinaire","manaCost":"{2}{R}","manaValue":3.0,"type":"Creature — Dragon Rogue Warrior","types":["Creature"],"subtypes":["Dragon","Rogue","Warrior"],"supertypes":[],"colors":["R"],"colorIdentity":["R"],"power":"2","toughness":"2","layout":"normal","text":"When this creature enters, create a Treasure token. (It's an artifact with \"{T}, Sacrifice this token: Add one mana of any color.\")\nWhenever you attack, you may sacrifice one or more Treasures. When you do, up to that many target creatures gain double strike until end of turn.","keywords":["Treasure"],"identifiers":{"scryfallOracleId":"66f0f43c-829d-44d6-adda-d8885bfbfbaf"}}"###,
    r###"{"name":"Thalakos Deceiver","manaCost":"{3}{U}","manaValue":4.0,"type":"Creature — Thalakos Wizard","types":["Creature"],"subtypes":["Thalakos","Wizard"],"supertypes":[],"colors":["U"],"colorIdentity":["U"],"power":"1","toughness":"1","layout":"normal","text":"Shadow (This creature can block or be blocked by only creatures with shadow.)\nWhenever this creature attacks and isn't blocked, you may sacrifice it. If you do, gain control of target creature. (This effect lasts indefinitely.)","keywords":["Shadow"],"identifiers":{"scryfallOracleId":"c304777a-39a9-4a2a-8cec-8116afb34c5b"}}"###,
    r###"{"name":"The Falcon, Airship Restored","manaCost":"{2}{B}","manaValue":3.0,"type":"Legendary Artifact — Vehicle","types":["Artifact"],"subtypes":["Vehicle"],"supertypes":["Legendary"],"colors":["B"],"colorIdentity":["B"],"power":"4","toughness":"3","layout":"normal","text":"Flying\nWhenever The Falcon deals combat damage to a player, you may sacrifice it. When you do, return target creature card from your graveyard to the battlefield.\n{4}{B}: Return this card from your graveyard to the battlefield tapped.\nCrew 2","keywords":["Crew","Flying"],"identifiers":{"scryfallOracleId":"9f4093a6-83f1-4c1d-8b03-8fe15647fb11"}}"###,
    r###"{"name":"The Fantasticar","manaCost":"{3}","manaValue":3.0,"type":"Legendary Artifact — Vehicle","types":["Artifact"],"subtypes":["Vehicle"],"supertypes":["Legendary"],"colors":[],"colorIdentity":[],"power":"4","toughness":"4","layout":"normal","text":"Flying\nWhenever you cast a noncreature spell, you may have The Fantasticar become an artifact creature until end of turn.\nWhenever you cast your fourth noncreature spell each turn, you may sacrifice The Fantasticar. If you do, create four 4/4 colorless Construct artifact creature tokens with flying and haste.","keywords":["Flying"],"identifiers":{"scryfallOracleId":"9b0abb58-4e2c-45dc-99b1-61a4cb79354b"}}"###,
    r###"{"name":"The Sackville-Bagginses","manaCost":"{1}{B}","manaValue":2.0,"type":"Legendary Creature — Halfling Citizen","types":["Creature"],"subtypes":["Halfling","Citizen"],"supertypes":["Legendary"],"colors":["B"],"colorIdentity":["B"],"power":"2","toughness":"2","layout":"normal","text":"When The Sackville-Bagginses enter, you may sacrifice another creature or artifact. If you do, draw a card and create a Treasure token.\nWhenever you sacrifice a token, target opponent loses 1 life.","keywords":["Treasure"],"identifiers":{"scryfallOracleId":"e3a665f9-6e51-4e0d-923b-e9552d5978a4"}}"###,
    r###"{"name":"Throwing Knife","manaCost":"{2}","manaValue":2.0,"type":"Artifact — Equipment","types":["Artifact"],"subtypes":["Equipment"],"supertypes":[],"colors":[],"colorIdentity":[],"layout":"normal","text":"Equipped creature gets +2/+0.\nWhenever equipped creature attacks, you may sacrifice this Equipment. If you do, this Equipment deals 2 damage to any target.\nEquip {2} ({2}: Attach to target creature you control. Equip only as a sorcery.)","keywords":["Equip"],"identifiers":{"scryfallOracleId":"c3973121-ed59-48b3-8efa-754a1b2c704d"}}"###,
    r###"{"name":"Trickster's Talisman","manaCost":"{U}","manaValue":1.0,"type":"Artifact — Equipment","types":["Artifact"],"subtypes":["Equipment"],"supertypes":[],"colors":["U"],"colorIdentity":["U"],"layout":"normal","text":"Invoke Duplicity — Equipped creature gets +1/+1 and has \"Whenever this creature deals combat damage to a player, you may sacrifice Trickster's Talisman. If you do, create a token that's a copy of this creature.\"\nEquip {2}","keywords":["Equip"],"identifiers":{"scryfallOracleId":"17469f58-286e-4410-b717-5908d24eecdd"}}"###,
    r###"{"name":"Undercity Eliminator","manaCost":"{3}{B}{B}","manaValue":5.0,"type":"Creature — Gorgon Assassin","types":["Creature"],"subtypes":["Gorgon","Assassin"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"power":"3","toughness":"3","layout":"normal","text":"When this creature enters, you may sacrifice an artifact or creature. When you do, exile target creature an opponent controls.","identifiers":{"scryfallOracleId":"93cb9c7e-d6d3-4b1e-8bcf-2a4a1d95a549"}}"###,
    r###"{"name":"Wasp of the Bitter End","manaCost":"{1}{B}","manaValue":2.0,"type":"Creature — Insect Horror","types":["Creature"],"subtypes":["Insect","Horror"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"power":"2","toughness":"1","layout":"normal","text":"Flying\nWhenever you cast a Bolas planeswalker spell, you may sacrifice this creature. If you do, destroy target creature.","keywords":["Flying"],"identifiers":{"scryfallOracleId":"9331cfd7-a818-45ed-9e4a-4d3c381052e0"}}"###,
    r###"{"name":"West Wind Avatar","manaCost":"{5}{G}{G}","manaValue":7.0,"type":"Creature — Cat Spirit Avatar","types":["Creature"],"subtypes":["Cat","Spirit","Avatar"],"supertypes":[],"colors":["G"],"colorIdentity":["G"],"power":"7","toughness":"7","layout":"normal","text":"Trample\nWhenever this creature enters or attacks, you may sacrifice a token or a land. If you do, you gain 3 life.\nDisappear — At the beginning of your end step, if a permanent left the battlefield under your control this turn, draw a card.","keywords":["Disappear","Trample"],"identifiers":{"scryfallOracleId":"92e45a8f-c582-45e1-9b7f-def219023ebf"}}"###,
    r###"{"name":"Winter, Tormented Loner","manaCost":"{2}{B}","manaValue":3.0,"type":"Legendary Creature — Human Warlock","types":["Creature"],"subtypes":["Human","Warlock"],"supertypes":["Legendary"],"colors":["B"],"colorIdentity":["B"],"power":"0","toughness":"3","layout":"normal","text":"When Winter enters, you may sacrifice a creature or planeswalker. When you do, each opponent sacrifices a creature of their choice.\nWinter gets +1/+0 for each creature and planeswalker card in your graveyard.","identifiers":{"scryfallOracleId":"84eaac2e-74ce-4a16-9faa-409df7c58eb3"}}"###,
    r###"{"name":"Wyll of the Blade Pact","manaCost":"{3}{R}{R}{R}","manaValue":6.0,"type":"Legendary Creature — Human Warlock","types":["Creature"],"subtypes":["Human","Warlock"],"supertypes":["Legendary"],"colors":["R"],"colorIdentity":["R"],"power":"5","toughness":"5","layout":"normal","text":"When this creature specializes, you may sacrifice another creature or an artifact. If you do, untap Wyll of the Blade Pact. After this main phase, there is an additional combat phase followed by an additional main phase.","identifiers":{"scryfallOracleId":"c1cdb8fa-adae-47c1-a81d-d296590d8280"}}"###,
    r###"{"name":"Wyll of the Celestial Pact","manaCost":"{3}{R}{R}{W}","manaValue":6.0,"type":"Legendary Creature — Human Warlock","types":["Creature"],"subtypes":["Human","Warlock"],"supertypes":["Legendary"],"colors":["R","W"],"colorIdentity":["R","W"],"power":"5","toughness":"5","layout":"normal","text":"When this creature specializes, you may sacrifice another creature or an artifact. When you do, return target creature card from your graveyard to the battlefield. It gains haste. If its mana value is 4 or greater, sacrifice it at the beginning of your next end step.","identifiers":{"scryfallOracleId":"a74d6cb3-e2e0-4deb-8fce-971a341c1999"}}"###,
    r###"{"name":"Wyll of the Elder Pact","manaCost":"{3}{U}{R}{R}","manaValue":6.0,"type":"Legendary Creature — Human Warlock","types":["Creature"],"subtypes":["Human","Warlock"],"supertypes":["Legendary"],"colors":["R","U"],"colorIdentity":["R","U"],"power":"5","toughness":"5","layout":"normal","text":"When this creature specializes, you may sacrifice another creature or an artifact. When you do, return target instant or sorcery card from your graveyard to your hand.","identifiers":{"scryfallOracleId":"bf15a083-99ba-40e7-8cab-722ef1e39fb6"}}"###,
    r###"{"name":"Wyll of the Fiend Pact","manaCost":"{3}{B}{R}{R}","manaValue":6.0,"type":"Legendary Creature — Human Warlock","types":["Creature"],"subtypes":["Human","Warlock"],"supertypes":["Legendary"],"colors":["B","R"],"colorIdentity":["B","R"],"power":"5","toughness":"5","layout":"normal","text":"When this creature specializes, you may sacrifice another creature or an artifact. When you do, draw three cards unless target opponent pays 5 life.","identifiers":{"scryfallOracleId":"a65cb73e-d654-4975-8e6d-2842a0bae38d"}}"###,
];

#[test]
fn optional_sacrifice_whole_faces_keep_complete_existing_payment_programs() {
    // SHAPE: whole-face parser and normal-loader preservation; runtime classes follow below.
    use engine::database::{
        mtgjson::AtomicCard,
        synthesis::{build_oracle_face, build_oracle_face_multi},
        CardDatabase,
    };
    use engine::types::ability::Effect;
    use std::io::Write;
    let mut export = serde_json::Map::new();
    for printed in OPTIONAL_SACRIFICE_PRINTED {
        let card: AtomicCard = serde_json::from_str(printed).unwrap();
        let face = if card.face_name.is_some() {
            build_oracle_face_multi(&card, card.identifiers.scryfall_oracle_id.clone())
        } else {
            build_oracle_face(&card, card.identifiers.scryfall_oracle_id.clone())
        };
        let mut entry = serde_json::to_value(&face).unwrap();
        entry["layout"] = serde_json::json!(card.layout);
        entry["face_index"] = serde_json::json!(usize::from(card.side.as_deref() == Some("b")));
        export.insert(face.name.to_lowercase(), entry);
    }
    let json = serde_json::to_string(&export).unwrap();
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(json.as_bytes()).unwrap();
    file.flush().unwrap();
    for db in [
        CardDatabase::from_json_str(&json).unwrap(),
        CardDatabase::from_export(file.path()).unwrap(),
    ] {
        for printed in OPTIONAL_SACRIFICE_PRINTED {
            let card: AtomicCard = serde_json::from_str(printed).unwrap();
            let name = card.face_name.as_deref().unwrap_or(&card.name);
            let face = db.get_face_by_name(name).unwrap();
            let mut has_sacrifice = false;
            let mut refused_payment = false;
            let mut inspect = |effect: &Effect| {
                has_sacrifice |= matches!(effect, Effect::Sacrifice { .. });
                refused_payment |= matches!(effect, Effect::Unimplemented { name, .. } if name == "reflexive optional payment");
                std::ops::ControlFlow::Continue(())
            };
            for ability in face.abilities.iter().chain(
                face.triggers
                    .iter()
                    .filter_map(|trigger| trigger.execute.as_deref()),
            ) {
                let _ = engine::types::ability_visit::visit_ability_def(ability, &mut inspect);
            }
            for definition in &face.static_abilities {
                let _ = engine::types::ability_visit::visit_static(definition, &mut inspect);
            }
            if matches!(name, "Angelic Renewal" | "Saw" | "Campsite Cuisine") {
                assert!(
                    refused_payment,
                    "the incomplete dependent operation must remain honest: {name}"
                );
            } else {
                assert!(
                    !refused_payment,
                    "complete existing payment must survive: {name}"
                );
                assert!(has_sacrifice, "positive payment reach: {name}");
            }
        }
    }
}

fn printed_payment_text(name: &str) -> String {
    OPTIONAL_SACRIFICE_PRINTED
        .iter()
        .find_map(|record| {
            let card: engine::database::mtgjson::AtomicCard = serde_json::from_str(record).unwrap();
            (card.face_name.as_deref().unwrap_or(&card.name) == name).then_some(card.text.unwrap())
        })
        .unwrap()
}

#[test]
fn token_only_optional_payment_keeps_real_upkeep_choice_and_counter_gate() {
    // CR 118.12 + CR 701.21a: the resolving controller elects and pays the token sacrifice.
    use engine::types::counter::CounterType;
    for accept in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::Untap);
        let source = scenario
            .add_artifact_from_oracle(
                P0,
                "Chitterspitter",
                &printed_payment_text("Chitterspitter"),
            )
            .id();
        let chosen = scenario
            .add_artifact_from_oracle(P0, "Chosen token", "")
            .id();
        let other = scenario
            .add_artifact_from_oracle(P0, "Other token", "")
            .id();
        let foreign = scenario
            .add_artifact_from_oracle(P1, "Foreign token", "")
            .id();
        let nontoken = scenario
            .add_artifact_from_oracle(P0, "Ordinary artifact", "")
            .id();
        let mut runner = scenario.build();
        for id in [chosen, other, foreign] {
            runner.state_mut().objects.get_mut(&id).unwrap().is_token = true;
        }
        runner.advance_to_upkeep();
        decide_gut(&mut runner, source, accept);
        if accept {
            let choices = choice_cards(&mut runner);
            assert!(choices.contains(&chosen) && choices.contains(&other));
            assert!(
                !choices.contains(&foreign)
                    && !choices.contains(&nontoken)
                    && !choices.contains(&source)
            );
            runner
                .act(GameAction::SelectCards {
                    cards: vec![chosen],
                })
                .unwrap();
        }
        settle(&mut runner);
        assert_eq!(
            runner.state().objects[&source]
                .counters
                .get(&CounterType::Generic("acorn".into()))
                .copied()
                .unwrap_or(0),
            u32::from(accept)
        );
        assert_eq!(runner.state().battlefield.contains(&chosen), !accept);
        assert!(runner.state().battlefield.contains(&other));
        assert!(runner.state().battlefield.contains(&foreign));
        assert!(runner.state().battlefield.contains(&nontoken));
        assert!(runner.state().resolution_stack.is_empty());
    }
}

#[test]
fn variable_optional_payment_draws_actual_selected_count_after_public_attack() {
    // CR 107.1c + CR 118.12: any-number payment chooses the actual count, then gates the draw.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Draw one", "Draw two", "Keep three", "Keep four"]);
    let source = scenario
        .add_creature_from_oracle(
            P0,
            "Sephiroth, One-Winged Angel",
            4,
            4,
            &printed_payment_text("Sephiroth, One-Winged Angel"),
        )
        .id();
    let first = scenario.add_creature(P0, "First offered", 1, 1).id();
    let second = scenario.add_creature(P0, "Second offered", 2, 2).id();
    let keep = scenario.add_creature(P0, "Unselected third", 3, 3).id();
    let foreign = scenario.add_creature(P1, "Foreign creature", 1, 1).id();
    let mut runner = scenario.build();
    declare_attack(&mut runner, source);
    decide_gut(&mut runner, source, true);
    let choices = choice_cards(&mut runner);
    assert!(choices.contains(&first) && choices.contains(&second) && choices.contains(&keep));
    assert!(!choices.contains(&source) && !choices.contains(&foreign));
    runner
        .act(GameAction::SelectCards {
            cards: vec![second, first],
        })
        .unwrap();
    settle(&mut runner);
    assert_eq!(runner.state().objects[&first].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&second].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&keep].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&foreign].zone, Zone::Battlefield);
    assert_eq!(runner.state().players[0].hand.len(), 2);
    assert_eq!(runner.state().players[0].library.len(), 2);
    assert!(runner.state().resolution_stack.is_empty());
}

#[test]
fn self_sacrifice_optional_payment_is_owned_by_actual_combat_damage_source() {
    // CR 118.12 + CR 608.2k: only the named combat source pays its own optional sacrifice.
    for accept in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Draw one", "Draw two", "Draw three", "Reserve"]);
        let source = scenario
            .add_creature_from_oracle(
                P0,
                "Impaler Shrike",
                3,
                1,
                &printed_payment_text("Impaler Shrike"),
            )
            .id();
        let decoy = scenario.add_creature(P0, "Unchosen ally", 2, 2).id();
        let mut runner = scenario.build();
        declare_attack(&mut runner, source);
        runner.advance_to_phase(Phase::DeclareBlockers);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ));
        runner.declare_blockers(&[]).unwrap();
        decide_gut(&mut runner, source, accept);
        settle(&mut runner);
        assert_eq!(
            runner.life(P1),
            17,
            "a real combat hit produced the trigger"
        );
        assert_eq!(
            runner.state().objects[&source].zone,
            if accept {
                Zone::Graveyard
            } else {
                Zone::Battlefield
            }
        );
        assert_eq!(runner.state().objects[&decoy].zone, Zone::Battlefield);
        assert_eq!(
            runner.state().players[0].hand.len(),
            if accept { 3 } else { 0 }
        );
        assert!(runner.state().resolution_stack.is_empty());
    }
}
