//! Complete pinned spell-context cards through the public casting and event paths.
//! Pin: MTGJSON 2026-10-04 / 5.3.0+20261004. No fixture export is consulted.
use engine::database::{
    mtgjson::AtomicCard,
    synthesis::{build_oracle_face, build_oracle_face_multi},
    CardDatabase,
};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const PRINTED: &[&str] = &[
    r###"{"name":"Alicia Masters, Skilled Sculptor","manaCost":"{1}{R}","manaValue":2,"type":"Legendary Creature — Human Artificer","types":["Creature"],"subtypes":["Human","Artificer"],"supertypes":["Legendary"],"colors":["R"],"colorIdentity":["R"],"power":"0","toughness":"4","layout":"normal","text":"At the beginning of combat on your turn, if you've cast a noncreature spell this turn, create a Treasure token.\nSense the Good — At the beginning of your end step, each player gains control of all creatures they own.","keywords":["Sense the Good","Treasure"],"identifiers":{"scryfallOracleId":"223504ba-174a-46f2-a4a2-5d663a82dfd3"}}"###,
    r###"{"name":"Council of Reeds","manaCost":"{2}{U}","manaValue":3,"type":"Legendary Creature — Human Scientist Hero","types":["Creature"],"subtypes":["Human","Scientist","Hero"],"supertypes":["Legendary"],"colors":["U"],"colorIdentity":["U"],"power":"2","toughness":"2","layout":"normal","text":"The \"legend rule\" doesn't apply to creatures you control.\nAt the beginning of combat on your turn, if you've cast a noncreature spell this turn, create a token that's a copy of Council of Reeds.","identifiers":{"scryfallOracleId":"33fa949c-f29e-4a98-9381-ecfa9e5c3f6d"}}"###,
    r###"{"name":"Eshki Dragonclaw","manaCost":"{1}{G}{U}{R}","manaValue":4,"type":"Legendary Creature — Human Warrior","types":["Creature"],"subtypes":["Human","Warrior"],"supertypes":["Legendary"],"colors":["G","R","U"],"colorIdentity":["G","R","U"],"power":"4","toughness":"4","layout":"normal","text":"Vigilance, trample, ward {1}\nAt the beginning of combat on your turn, if you've cast both a creature spell and a noncreature spell this turn, draw a card and put two +1/+1 counters on Eshki Dragonclaw.","keywords":["Trample","Vigilance","Ward"],"identifiers":{"scryfallOracleId":"85d87087-48ee-4dc8-a1c0-9c9ccb885adc"}}"###,
    r###"{"name":"Fae Offering","manaCost":"{2}{G}","manaValue":3,"type":"Enchantment","types":["Enchantment"],"subtypes":[],"supertypes":[],"colors":["G"],"colorIdentity":["G"],"layout":"normal","text":"At the beginning of each end step, if you've cast both a creature spell and a noncreature spell this turn, create a Clue token, a Food token, and a Treasure token.","keywords":["Food","Treasure"],"identifiers":{"scryfallOracleId":"885d68de-db97-4c9e-bb44-8e561a482dde"}}"###,
    r###"{"name":"H.E.R.B.I.E., Lovable Robot","manaCost":"{2}","manaValue":2,"type":"Legendary Artifact Creature — Robot Scout","types":["Artifact","Creature"],"subtypes":["Robot","Scout"],"supertypes":["Legendary"],"colors":[],"colorIdentity":[],"power":"1","toughness":"1","layout":"normal","text":"Flying\nAt the beginning of combat on your turn, if you've cast a noncreature spell this turn, surveil 1.\n{T}: Add {C}.\n{1}, {T}: Add one mana of any color.","keywords":["Flying","Surveil"],"identifiers":{"scryfallOracleId":"064a2f4a-ff3f-4622-8727-24b22965416d"}}"###,
    r###"{"name":"Human Torch","manaCost":"{3}{R}","manaValue":4,"type":"Legendary Creature — Human Hero","types":["Creature"],"subtypes":["Human","Hero"],"supertypes":["Legendary"],"colors":["R"],"colorIdentity":["G","R","U","W"],"power":"3","toughness":"2","layout":"normal","text":"At the beginning of combat on your turn, if you've cast a noncreature spell this turn, Human Torch gains flying, double strike, and haste until end of turn.\nWhenever Human Torch attacks, you may pay {R}{G}{W}{U}. If you do, until end of turn, whenever he deals combat damage to an opponent, he deals that much damage to each other opponent.","identifiers":{"scryfallOracleId":"d55cb52d-b35c-4602-a518-cd51f155a2d1"}}"###,
    r###"{"name":"Invisible Woman","manaCost":"{2}{W}","manaValue":3,"type":"Legendary Creature — Human Hero","types":["Creature"],"subtypes":["Human","Hero"],"supertypes":["Legendary"],"colors":["W"],"colorIdentity":["G","R","U","W"],"power":"3","toughness":"3","layout":"normal","text":"At the beginning of combat on your turn, if you've cast a noncreature spell this turn, create a 0/3 colorless Wall creature token with defender and reach.\nWhenever you attack, you may pay {R}{G}{W}{U}. When you do, target creature gets +1/+0 until end of turn for each creature you control and can't be blocked this turn.","identifiers":{"scryfallOracleId":"085016ee-4429-453f-b7f3-d61065f1214e"}}"###,
    r###"{"name":"Lockjaw, Slobbering Teleporter","manaCost":"{1}{U}","manaValue":2,"type":"Legendary Creature — Inhuman Dog Hero","types":["Creature"],"subtypes":["Inhuman","Dog","Hero"],"supertypes":["Legendary"],"colors":["U"],"colorIdentity":["U"],"power":"1","toughness":"1","layout":"normal","text":"Vigilance\nAt the beginning of combat on your turn, if you've cast a noncreature spell this turn, put a +1/+1 counter on Lockjaw. When you do, Lockjaw and up to one other target creature you control can't be blocked this turn.","keywords":["Vigilance"],"identifiers":{"scryfallOracleId":"5efce07e-24c4-4520-a46f-9954b67c4e6e"}}"###,
    r###"{"name":"Mister Fantastic","manaCost":"{2}{U}","manaValue":3,"type":"Legendary Creature — Human Scientist Hero","types":["Creature"],"subtypes":["Human","Scientist","Hero"],"supertypes":["Legendary"],"colors":["U"],"colorIdentity":["G","R","U","W"],"power":"2","toughness":"4","layout":"normal","text":"Reach, vigilance\nAt the beginning of combat on your turn, if you've cast a noncreature spell this turn, draw a card.\n{R}{G}{W}{U}, {T}: Copy target triggered ability you control twice. You may choose new targets for the copies.","keywords":["Reach","Vigilance"],"identifiers":{"scryfallOracleId":"a8011dce-17fb-46e1-8297-81a4acccbd05"}}"###,
    r###"{"name":"Seeker of Insight","manaCost":"{1}{U}","manaValue":2,"type":"Creature — Human Wizard","types":["Creature"],"subtypes":["Human","Wizard"],"supertypes":[],"colors":["U"],"colorIdentity":["U"],"power":"1","toughness":"3","layout":"normal","text":"{T}: Draw a card, then discard a card. Activate only if you've cast a noncreature spell this turn.","identifiers":{"scryfallOracleId":"a0b2d446-fe90-40f8-8674-7421c425b489"}}"###,
    r###"{"name":"Tapestry of the Ages","manaCost":"{4}","manaValue":4,"type":"Artifact","types":["Artifact"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":[],"layout":"normal","text":"{2}, {T}: Draw a card. Activate only if you've cast a noncreature spell this turn.","identifiers":{"scryfallOracleId":"9248f9b9-ddaa-459c-91e6-9045a65b0f07"}}"###,
    r###"{"name":"The Thing","manaCost":"{5}{G}","manaValue":6,"type":"Legendary Creature — Human Hero","types":["Creature"],"subtypes":["Human","Hero"],"supertypes":["Legendary"],"colors":["G"],"colorIdentity":["G","R","U","W"],"power":"5","toughness":"5","layout":"normal","text":"Trample\nAt the beginning of combat on your turn, if you've cast a noncreature spell this turn, put four +1/+1 counters on The Thing.\nWhenever The Thing attacks, you may pay {R}{G}{W}{U}. When you do, double the number of each kind of counter on any number of target permanents you control.","keywords":["Double","Trample"],"identifiers":{"scryfallOracleId":"c8de07d4-9bc1-4bfe-b5a4-0a58e05af022"}}"###,
    r###"{"name":"Franklin Richards, Ascendant","manaCost":"{5}{R}","manaValue":6,"type":"Legendary Creature — Mutant Hero","types":["Creature"],"subtypes":["Mutant","Hero"],"supertypes":["Legendary"],"colors":["R"],"colorIdentity":["R"],"power":"6","toughness":"6","layout":"normal","text":"At the beginning of combat on your turn, if you've cast a noncreature spell this turn, discover 6. (Exile cards from the top of your library until you exile a nonland card with mana value 6 or less. Cast it without paying its mana cost or put it into your hand. Put the rest on the bottom in a random order.)","keywords":["Discover"],"identifiers":{"scryfallOracleId":"af20f452-5cb9-47cc-9a81-71f23204ddd0"}}"###,
    r###"{"name":"Traxos, Academy Guardian","manaCost":"{3}{U}","manaValue":4,"type":"Legendary Artifact Creature — Dragon Construct","types":["Artifact","Creature"],"subtypes":["Dragon","Construct"],"supertypes":["Legendary"],"colors":["U"],"colorIdentity":["U"],"power":"1","toughness":"5","layout":"normal","text":"This spell costs {2} less to cast if you've cast a noncreature spell this turn.\nFlying, vigilance\nProwess (Whenever you cast a noncreature spell, this creature gets +1/+1 until end of turn.)","keywords":["Flying","Prowess","Vigilance"],"identifiers":{"scryfallOracleId":"7b9a574f-6d56-421b-8fbd-94da1e6e3ce1"}}"###,
    r###"{"name":"Lyse Hext","manaCost":"{1}{W}{U}","manaValue":3,"type":"Legendary Creature — Human Rebel Monk","types":["Creature"],"subtypes":["Human","Rebel","Monk"],"supertypes":["Legendary"],"colors":["U","W"],"colorIdentity":["U","W"],"power":"2","toughness":"2","layout":"normal","text":"Prowess (Whenever you cast a noncreature spell, this creature gets +1/+1 until end of turn.)\nNoncreature spells you cast cost {1} less to cast.\nAs long as you've cast two or more noncreature spells this turn, Lyse Hext has double strike.","keywords":["Prowess"],"identifiers":{"scryfallOracleId":"47bce63e-8edb-43c1-b273-4b86da5c616b"}}"###,
    r###"{"name":"Silburlind Snapper","manaCost":"{5}{U}","manaValue":6,"type":"Creature — Turtle","types":["Creature"],"subtypes":["Turtle"],"supertypes":[],"colors":["U"],"colorIdentity":["U"],"power":"6","toughness":"6","layout":"normal","text":"This creature can't attack unless you've cast a noncreature spell this turn.","identifiers":{"scryfallOracleId":"eb2fe2f7-60f7-44d3-8d4a-4e102012435d"}}"###,
    r###"{"name":"Sword Coast Serpent // Capsizing Wave","faceName":"Sword Coast Serpent","side":"a","manaCost":"{5}{U}{U}","manaValue":7,"type":"Creature — Serpent Dragon","types":["Creature"],"subtypes":["Serpent","Dragon"],"supertypes":[],"colors":["U"],"colorIdentity":["U"],"power":"6","toughness":"6","layout":"adventure","text":"This creature can't be blocked as long as you've cast a noncreature spell this turn.","identifiers":{"scryfallOracleId":"d60e14b4-f750-40cc-85b5-17c87ae8a584"}}"###,
    r###"{"name":"Sword Coast Serpent // Capsizing Wave","faceName":"Capsizing Wave","side":"b","manaCost":"{1}{U}","manaValue":7,"type":"Instant — Adventure","types":["Instant"],"subtypes":["Adventure"],"supertypes":[],"colors":["U"],"colorIdentity":["U"],"layout":"adventure","text":"Return target creature to its owner's hand. (Then exile this card. You may cast the creature later from exile.)","identifiers":{"scryfallOracleId":"d60e14b4-f750-40cc-85b5-17c87ae8a584"}}"###,
    r###"{"name":"Gaddock Teeg","manaCost":"{G}{W}","manaValue":2,"type":"Legendary Creature — Kithkin Advisor","types":["Creature"],"subtypes":["Kithkin","Advisor"],"supertypes":["Legendary"],"colors":["G","W"],"colorIdentity":["G","W"],"power":"2","toughness":"2","layout":"normal","text":"Noncreature spells with mana value 4 or greater can't be cast.\nNoncreature spells with {X} in their mana costs can't be cast.","identifiers":{"scryfallOracleId":"98dd6d43-cc56-4656-9e0c-240d0679b404"}}"###,
    r###"{"name":"Ethersworn Canonist","manaCost":"{1}{W}","manaValue":2,"type":"Artifact Creature — Human Cleric","types":["Artifact","Creature"],"subtypes":["Human","Cleric"],"supertypes":[],"colors":["W"],"colorIdentity":["W"],"power":"2","toughness":"2","layout":"normal","text":"Each player who has cast a nonartifact spell this turn can't cast additional nonartifact spells.","identifiers":{"scryfallOracleId":"ec3471ad-b65c-4e35-a7d1-68da9555a9ec"}}"###,
    r###"{"name":"Heartwood Storyteller Avatar","manaValue":0,"type":"Vanguard","types":["Vanguard"],"subtypes":[],"supertypes":[],"colors":[],"colorIdentity":[],"layout":"vanguard","text":"The first creature spell you cast each turn costs {1} less to cast.\nThe first noncreature spell each opponent casts each turn costs {1} more to cast.","identifiers":{"scryfallOracleId":"96a78b38-114e-4048-943a-a8173d5cb214"}}"###,
    r###"{"name":"Calamity's Wake","manaCost":"{1}{W}","manaValue":2,"type":"Instant","types":["Instant"],"subtypes":[],"supertypes":[],"colors":["W"],"colorIdentity":["W"],"layout":"normal","text":"Exile all graveyards. Players can't cast noncreature spells this turn. Exile Calamity's Wake.","identifiers":{"scryfallOracleId":"a6436330-f35a-4d31-9230-03e0c62f9e7e"}}"###,
    r###"{"name":"Permission Denied","manaCost":"{W}{U}","manaValue":2,"type":"Instant","types":["Instant"],"subtypes":[],"supertypes":[],"colors":["U","W"],"colorIdentity":["U","W"],"layout":"normal","text":"Counter target noncreature spell. Your opponents can't cast noncreature spells this turn.","identifiers":{"scryfallOracleId":"95aae5ba-7634-4cad-a0be-496c3398fa3c"}}"###,
    r###"{"name":"Ranger-Captain of Eos","manaCost":"{1}{W}{W}","manaValue":3,"type":"Creature — Human Soldier Ranger","types":["Creature"],"subtypes":["Human","Soldier","Ranger"],"supertypes":[],"colors":["W"],"colorIdentity":["W"],"power":"3","toughness":"3","layout":"normal","text":"When this creature enters, you may search your library for a creature card with mana value 1 or less, reveal it, put it into your hand, then shuffle.\nSacrifice this creature: Your opponents can't cast noncreature spells this turn.","identifiers":{"scryfallOracleId":"cada3481-cc2b-4412-b9b5-0436af53aad2"}}"###,
    r###"{"name":"Sift Through Sands","manaCost":"{1}{U}{U}","manaValue":3,"type":"Instant — Arcane","types":["Instant"],"subtypes":["Arcane"],"supertypes":[],"colors":["U"],"colorIdentity":["U"],"layout":"normal","text":"Draw two cards, then discard a card.\nIf you've cast a spell named Peer Through Depths and a spell named Reach Through Mists this turn, you may search your library for a card named The Unspeakable, put it onto the battlefield, then shuffle.","identifiers":{"scryfallOracleId":"4f5978a6-40c2-49df-8277-48a6d4f6f23e"}}"###,
    r###"{"name":"Relic Runner","manaCost":"{1}{U}","manaValue":2,"type":"Creature — Human Rogue","types":["Creature"],"subtypes":["Human","Rogue"],"supertypes":[],"colors":["U"],"colorIdentity":["U"],"power":"2","toughness":"1","layout":"normal","text":"This creature can't be blocked if you've cast a historic spell this turn. (Artifacts, legendaries, and Sagas are historic.)","identifiers":{"scryfallOracleId":"69e4e5bf-f0e2-4ed1-8b17-c8c7a9381551"}}"###,
    r###"{"name":"Nightscape Familiar","manaCost":"{1}{B}","manaValue":2,"type":"Creature — Zombie","types":["Creature"],"subtypes":["Zombie"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"power":"1","toughness":"1","layout":"normal","text":"Blue spells and red spells you cast cost {1} less to cast.\n{1}{B}: Regenerate this creature.","keywords":["Regenerate"],"identifiers":{"scryfallOracleId":"57296ea3-3c0d-49b7-bc08-0d0d8414e9ad"}}"###,
    r###"{"name":"Jhoira's Familiar","manaCost":"{4}","manaValue":4,"type":"Artifact Creature — Bird","types":["Artifact","Creature"],"subtypes":["Bird"],"supertypes":[],"colors":[],"colorIdentity":[],"power":"2","toughness":"2","layout":"normal","text":"Flying\nHistoric spells you cast cost {1} less to cast. (Artifacts, legendaries, and Sagas are historic.)","keywords":["Flying"],"identifiers":{"scryfallOracleId":"df4b5f7b-9d83-49b1-bd5e-77d0652eb34c"}}"###,
    r###"{"name":"Impending Flux","manaCost":"{2}{R}","manaValue":3,"type":"Sorcery","types":["Sorcery"],"subtypes":[],"supertypes":[],"colors":["R"],"colorIdentity":["R"],"layout":"normal","text":"Paradox — Impending Flux deals X damage to each opponent and each creature they control, where X is 1 plus the number of spells you've cast from anywhere other than your hand this turn.\nForetell {1}{R}{R} (During your turn, you may pay {2} and exile this card from your hand face down. Cast it on a later turn for its foretell cost.)","keywords":["Foretell","Paradox"],"identifiers":{"scryfallOracleId":"7089512e-f9b6-4d8e-bccc-29e6785cc041"}}"###,
    r###"{"name":"Demon of Fate's Design","manaCost":"{4}{B}{B}","manaValue":6,"type":"Enchantment Creature — Demon","types":["Enchantment","Creature"],"subtypes":["Demon"],"supertypes":[],"colors":["B"],"colorIdentity":["B"],"power":"6","toughness":"6","layout":"normal","text":"Flying, trample\nOnce during each of your turns, you may cast an enchantment spell by paying life equal to its mana value rather than paying its mana cost.\n{2}{B}, Sacrifice another enchantment: This creature gets +X/+0 until end of turn, where X is the sacrificed enchantment's mana value.","keywords":["Flying","Trample"],"identifiers":{"scryfallOracleId":"53716e0b-4d4e-40b0-8370-38e11f73d195"}}"###,
    r###"{"name":"Conspiracy Unraveler","manaCost":"{5}{U}{U}","manaValue":7,"type":"Creature — Sphinx Detective","types":["Creature"],"subtypes":["Sphinx","Detective"],"supertypes":[],"colors":["U"],"colorIdentity":["U"],"power":"6","toughness":"6","layout":"normal","text":"Flying\nYou may collect evidence 10 rather than pay the mana cost for spells you cast. (To collect evidence 10, exile cards with total mana value 10 or greater from your graveyard.)","keywords":["Collect evidence","Flying"],"identifiers":{"scryfallOracleId":"e4541d02-2860-46ef-b630-1d6307b60766"}}"###,
    r###"{"name":"Ichneumon Druid","manaCost":"{1}{G}{G}","manaValue":3,"type":"Creature — Human Druid","types":["Creature"],"subtypes":["Human","Druid"],"supertypes":[],"colors":["G"],"colorIdentity":["G"],"power":"1","toughness":"1","layout":"normal","text":"Whenever an opponent casts an instant spell other than the first instant spell that player casts each turn, this creature deals 4 damage to that player.","identifiers":{"scryfallOracleId":"6aba2a04-0117-429e-a5df-6ba4645129d8"}}"###,
    r###"{"name":"Peer Through Depths","manaCost":"{1}{U}","manaValue":2,"type":"Instant — Arcane","types":["Instant"],"subtypes":["Arcane"],"supertypes":[],"colors":["U"],"colorIdentity":["U"],"layout":"normal","text":"Look at the top five cards of your library. You may reveal an instant or sorcery card from among them and put it into your hand. Put the rest on the bottom of your library in any order.","identifiers":{"scryfallOracleId":"a61f5100-aa38-4996-b23c-3ce4ed25ed09"}}"###,
    r###"{"name":"Reach Through Mists","manaCost":"{U}","manaValue":1,"type":"Instant — Arcane","types":["Instant"],"subtypes":["Arcane"],"supertypes":[],"colors":["U"],"colorIdentity":["U"],"layout":"normal","text":"Draw a card.","identifiers":{"scryfallOracleId":"c81ca8ff-92f3-481e-82f7-0673b6c74ea0"}}"###,
    r###"{"name":"The Unspeakable","manaCost":"{6}{U}{U}{U}","manaValue":9,"type":"Legendary Creature — Spirit","types":["Creature"],"subtypes":["Spirit"],"supertypes":["Legendary"],"colors":["U"],"colorIdentity":["U"],"power":"6","toughness":"7","layout":"normal","text":"Flying, trample\nWhenever The Unspeakable deals combat damage to a player, you may return target Arcane card from your graveyard to your hand.","keywords":["Flying","Trample"],"identifiers":{"scryfallOracleId":"64e413c9-96e5-43c3-a182-84d53fef4327"}}"###,
];

pub(super) fn printed_db() -> CardDatabase {
    let mut export = serde_json::Map::new();
    for record in PRINTED {
        let atomic: AtomicCard = serde_json::from_str(record).unwrap();
        let face = if atomic.face_name.is_some() {
            build_oracle_face_multi(&atomic, atomic.identifiers.scryfall_oracle_id.clone())
        } else {
            build_oracle_face(&atomic, atomic.identifiers.scryfall_oracle_id.clone())
        };
        let mut entry = serde_json::to_value(&face).unwrap();
        entry["layout"] = serde_json::json!(atomic.layout);
        entry["face_index"] = serde_json::json!(if atomic.side.as_deref() == Some("b") {
            1
        } else {
            0
        });
        export.insert(face.name.to_lowercase(), entry);
    }
    CardDatabase::from_json_str(&serde_json::to_string(&export).unwrap()).unwrap()
}

fn blue_pool(scenario: &mut GameScenario, player: engine::types::PlayerId, count: usize) {
    scenario.with_mana_pool(
        player,
        (0..count)
            .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
            .collect(),
    );
}

fn settled(runner: &GameRunner) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().trigger_stack.is_empty());
    assert!(runner.state().pending_cast.is_none());
}

fn paid_history_spell(scenario: &mut GameScenario, kind: CoreType) -> ObjectId {
    match kind {
        CoreType::Creature => scenario
            .add_creature_to_hand_from_oracle(P0, "Synthetic history creature", 1, 1, "")
            .with_mana_cost(ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::Blue],
            })
            .id(),
        CoreType::Instant => scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Synthetic history instant",
                true,
                "You gain 1 life.",
            )
            .with_mana_cost(ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::Blue],
            })
            .id(),
        _ => panic!("history fixture requires Creature or Instant"),
    }
}

/// CR 601.2i + CR 603.4: a completed cast is history even after its object
/// resolves; the phase event and its intervening-if use the actual caster.
#[test]
fn retained_noncreature_history_drives_complete_combat_parent_payoffs() {
    let db = printed_db();
    for name in [
        "Alicia Masters, Skilled Sculptor",
        "Council of Reeds",
        "Human Torch",
        "Invisible Woman",
        "The Thing",
        "Lockjaw, Slobbering Teleporter",
        "Mister Fantastic",
    ] {
        for (caster, creature) in [(P0, true), (P0, false), (P1, false)] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let host = scenario.add_real_card(P0, name, Zone::Battlefield, &db);
            let other_host = scenario.add_real_card(P1, name, Zone::Battlefield, &db);
            let history = if caster == P0 {
                paid_history_spell(
                    &mut scenario,
                    if creature {
                        CoreType::Creature
                    } else {
                        CoreType::Instant
                    },
                )
            } else {
                scenario
                    .add_spell_to_hand_from_oracle(
                        caster,
                        "Synthetic history instant",
                        true,
                        "You gain 1 life.",
                    )
                    .with_mana_cost(ManaCost::Cost {
                        generic: 0,
                        shards: vec![ManaCostShard::Blue],
                    })
                    .id()
            };
            scenario.with_library_top(P0, &["Draw witness", "Reserve"]);
            blue_pool(&mut scenario, caster, 2);
            let mut runner = scenario.build();
            let original_hand = runner.state().players[0].hand.len();
            if caster == P1 {
                runner.act(GameAction::PassPriority).unwrap();
            }
            runner.cast(history).resolve();
            settled(&runner);
            assert_eq!(
                runner.state().players[caster.0 as usize].mana_pool.total(),
                1
            );
            let records = &runner.state().spells_cast_this_turn_by_player[&caster];
            assert_eq!(records.len(), 1);
            assert_eq!(
                records[0].name,
                if creature {
                    "Synthetic history creature"
                } else {
                    "Synthetic history instant"
                }
            );
            assert_eq!(records[0].from_zone, Zone::Hand);
            runner.advance_to_phase(Phase::BeginCombat);
            runner.advance_until_stack_empty();
            settled(&runner);
            let state = runner.state();
            let qualifies = !creature && caster == P0;
            let expected = usize::from(qualifies);
            match name {
                "Alicia Masters, Skilled Sculptor" => assert_eq!(
                    state
                        .battlefield
                        .iter()
                        .filter(|id| state.objects[id]
                            .card_types
                            .subtypes
                            .iter()
                            .any(|s| s == "Treasure"))
                        .count(),
                    expected
                ),
                "Council of Reeds" => {
                    let copies: Vec<_> = state
                        .battlefield
                        .iter()
                        .filter(|id| {
                            state.objects[id].name == name && state.objects[id].controller == P0
                        })
                        .collect();
                    assert_eq!(copies.len(), 1 + expected);
                    assert_eq!(
                        copies
                            .iter()
                            .copied()
                            .filter(|id| state.objects[*id].is_token)
                            .count(),
                        expected
                    );
                    assert!(copies.iter().all(|id| state.objects[*id].power == Some(2)
                        && state.objects[*id].toughness == Some(2)));
                }
                "Human Torch" => {
                    for keyword in [Keyword::Flying, Keyword::DoubleStrike, Keyword::Haste] {
                        assert_eq!(state.objects[&host].keywords.contains(&keyword), qualifies);
                    }
                }
                "Invisible Woman" => {
                    let walls: Vec<_> = state
                        .battlefield
                        .iter()
                        .filter(|id| {
                            state.objects[id]
                                .card_types
                                .subtypes
                                .iter()
                                .any(|s| s == "Wall")
                        })
                        .collect();
                    assert_eq!(walls.len(), expected);
                    for id in walls {
                        let wall = &state.objects[id];
                        assert_eq!(wall.power, Some(0));
                        assert_eq!(wall.toughness, Some(3));
                        assert!(wall.keywords.contains(&Keyword::Defender));
                        assert!(wall.keywords.contains(&Keyword::Reach));
                        assert!(wall.color.is_empty());
                    }
                }
                "The Thing" => assert_eq!(
                    state.objects[&host]
                        .counters
                        .get(&CounterType::P1P1)
                        .copied()
                        .unwrap_or(0),
                    if qualifies { 4 } else { 0 }
                ),
                "Lockjaw, Slobbering Teleporter" => assert_eq!(
                    state.objects[&host]
                        .counters
                        .get(&CounterType::P1P1)
                        .copied()
                        .unwrap_or(0),
                    u32::from(qualifies)
                ),
                "Mister Fantastic" => {
                    assert_eq!(
                        state.players[0].hand.len(),
                        original_hand - usize::from(caster == P0) + expected
                    )
                }
                _ => unreachable!(),
            }
            assert_eq!(state.objects[&other_host].controller, P1);
            assert_eq!(state.objects[&other_host].zone, Zone::Battlefield);
            assert!(state.objects[&other_host].counters.is_empty());
            if name == "Human Torch" {
                for keyword in [Keyword::Flying, Keyword::DoubleStrike, Keyword::Haste] {
                    assert!(!state.objects[&other_host].keywords.contains(&keyword));
                }
            }
        }
    }
}

/// CR 603.4: BOTH is two existential predicates, not OR or total count two.
#[test]
fn retained_both_history_requires_both_populations_at_phase_event() {
    let db = printed_db();
    for name in ["Eshki Dragonclaw", "Fae Offering"] {
        for countered in [false, true] {
            for kinds in [
                vec![],
                vec![true],
                vec![false],
                vec![true, true],
                vec![false, false],
                vec![true, false],
                vec![false, true],
            ] {
                let mut scenario = GameScenario::new();
                scenario.at_phase(Phase::PreCombatMain);
                let host = scenario.add_real_card(P0, name, Zone::Battlefield, &db);
                let other_host = scenario.add_real_card(P1, name, Zone::Battlefield, &db);
                let counter = scenario
                    .add_spell_to_hand_from_oracle(
                        P1,
                        "Synthetic BOTH history counter",
                        true,
                        "Counter target spell.",
                    )
                    .with_mana_cost(ManaCost::generic(0))
                    .id();
                let spells: Vec<_> = kinds
                    .iter()
                    .map(|creature| {
                        paid_history_spell(
                            &mut scenario,
                            if *creature {
                                CoreType::Creature
                            } else {
                                CoreType::Instant
                            },
                        )
                    })
                    .collect();
                scenario.with_library_top(P0, &["Draw witness", "Reserve"]);
                scenario.with_library_top(P1, &["Next turn draw", "Reserve"]);
                blue_pool(&mut scenario, P0, 3);
                let mut runner = scenario.build();
                for (index, spell) in spells.into_iter().enumerate() {
                    if countered && index == 0 {
                        runner.cast(spell).commit();
                        runner.act(GameAction::PassPriority).unwrap();
                        runner.cast(counter).target_object(spell).resolve();
                        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
                    } else {
                        runner.cast(spell).resolve();
                    }
                    settled(&runner);
                }
                assert_eq!(
                    runner
                        .state()
                        .spells_cast_this_turn_by_player
                        .get(&P0)
                        .map_or(0, |records| records.len()),
                    kinds.len()
                );
                let before_hand = runner.state().players[0].hand.len();
                if name == "Fae Offering" {
                    runner.advance_to_combat();
                    runner.declare_attackers(&[]).unwrap();
                }
                runner.advance_to_phase(if name == "Eshki Dragonclaw" {
                    Phase::BeginCombat
                } else {
                    Phase::End
                });
                runner.advance_until_stack_empty();
                settled(&runner);
                let both = kinds.contains(&true) && kinds.contains(&false);
                assert!(runner.state().objects[&other_host].counters.is_empty());
                if name == "Eshki Dragonclaw" {
                    assert_eq!(
                        runner.state().players[0].hand.len(),
                        before_hand + usize::from(both)
                    );
                    assert_eq!(
                        runner.state().objects[&host]
                            .counters
                            .get(&CounterType::P1P1)
                            .copied()
                            .unwrap_or(0),
                        if both { 2 } else { 0 }
                    );
                } else {
                    for kind in ["Clue", "Food", "Treasure"] {
                        assert_eq!(
                            runner
                                .state()
                                .battlefield
                                .iter()
                                .filter(|id| runner.state().objects[id]
                                    .card_types
                                    .subtypes
                                    .iter()
                                    .any(|s| s == kind))
                                .count(),
                            usize::from(both),
                            "{kind}"
                        );
                    }
                }
                if name == "Eshki Dragonclaw" {
                    runner.advance_to_combat();
                    runner.declare_attackers(&[]).unwrap();
                    runner.advance_to_end_step();
                }
                runner.advance_to_phase(Phase::PreCombatMain);
                assert_eq!(runner.state().active_player, P1);
                assert!(runner
                    .state()
                    .spells_cast_this_turn_by_player
                    .values()
                    .all(|records| records.is_empty()));
                settled(&runner);
            }
        }
    }
}

/// CR 602.2b + CR 601.2e: legality is checked before tap/mana costs are paid.
#[test]
fn seeker_and_tapestry_activation_use_real_matching_history_and_pay_costs() {
    let db = printed_db();
    for name in ["Seeker of Insight", "Tapestry of the Ages"] {
        for (caster, creature) in [(P0, true), (P0, false), (P1, false)] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let source = scenario.add_real_card(P0, name, Zone::Battlefield, &db);
            let history = if caster == P0 {
                paid_history_spell(
                    &mut scenario,
                    if creature {
                        CoreType::Creature
                    } else {
                        CoreType::Instant
                    },
                )
            } else {
                scenario
                    .add_spell_to_hand_from_oracle(
                        P1,
                        "Synthetic foreign activation history",
                        true,
                        "You gain 1 life.",
                    )
                    .with_mana_cost(ManaCost::Cost {
                        generic: 0,
                        shards: vec![ManaCostShard::Blue],
                    })
                    .id()
            };
            let keep = scenario.add_card_to_hand(P0, "Keep this card");
            let discard = scenario.add_card_to_hand(P0, "Discard second choice");
            let draw = scenario.add_card_to_library_top(P0, "Draw this card");
            blue_pool(&mut scenario, P0, 5);
            blue_pool(&mut scenario, P1, 1);
            let mut runner = scenario.build();
            if caster == P1 {
                runner.act(GameAction::PassPriority).unwrap();
            }
            runner.cast(history).resolve();
            settled(&runner);
            assert_eq!(
                runner.state().spells_cast_this_turn_by_player[&caster].len(),
                1
            );
            let before = runner.state().players[0].mana_pool.total();
            let result = runner.act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            });
            if creature || caster != P0 {
                assert!(result.is_err());
                assert!(!runner.state().objects[&source].tapped);
                assert_eq!(runner.state().players[0].mana_pool.total(), before);
                assert_eq!(runner.state().objects[&draw].zone, Zone::Library);
                settled(&runner);
            } else {
                result.unwrap();
                assert!(runner.state().objects[&source].tapped);
                runner.advance_until_stack_empty();
                if name == "Seeker of Insight" {
                    let WaitingFor::DiscardChoice {
                        player,
                        count,
                        cards,
                        up_to,
                        ..
                    } = &runner.state().waiting_for
                    else {
                        panic!("mandatory discard: {:?}", runner.state().waiting_for)
                    };
                    assert_eq!(*player, P0);
                    assert_eq!(*count, 1);
                    assert!(!up_to);
                    assert_eq!(
                        cards
                            .iter()
                            .copied()
                            .collect::<std::collections::BTreeSet<_>>(),
                        [keep, discard, draw].into_iter().collect()
                    );
                    runner
                        .act(GameAction::SelectCards {
                            cards: vec![discard],
                        })
                        .unwrap();
                    runner.advance_until_stack_empty();
                    assert_eq!(runner.state().objects[&discard].zone, Zone::Graveyard);
                    assert_eq!(runner.state().objects[&keep].zone, Zone::Hand);
                }
                assert_eq!(runner.state().objects[&draw].zone, Zone::Hand);
                assert_eq!(
                    runner.state().players[0].mana_pool.total(),
                    before - if name == "Tapestry of the Ages" { 2 } else { 0 }
                );
                settled(&runner);
            }
        }
    }
}

/// CR 701.25a: the actual surveil producer offers its controller the top card.
#[test]
fn herbie_combat_surveille_moves_the_selected_top_card() {
    let db = printed_db();
    for creature in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source =
            scenario.add_real_card(P0, "H.E.R.B.I.E., Lovable Robot", Zone::Battlefield, &db);
        scenario.add_real_card(P1, "H.E.R.B.I.E., Lovable Robot", Zone::Battlefield, &db);
        let foreign_top = scenario.add_card_to_library_top(P1, "Other Herbie's top card");
        let history = paid_history_spell(
            &mut scenario,
            if creature {
                CoreType::Creature
            } else {
                CoreType::Instant
            },
        );
        let underneath = scenario.add_card_to_library_top(P0, "Underneath");
        let top = scenario.add_card_to_library_top(P0, "Surveil me away");
        blue_pool(&mut scenario, P0, 2);
        let mut runner = scenario.build();
        assert!(runner.state().objects[&source]
            .keywords
            .contains(&Keyword::Flying));
        runner.cast(history).resolve();
        runner.advance_to_phase(Phase::BeginCombat);
        runner.advance_until_stack_empty();
        if creature {
            settled(&runner);
            assert_eq!(runner.state().objects[&top].zone, Zone::Library);
        } else {
            assert_eq!(
                runner.state().waiting_for,
                WaitingFor::SurveilChoice {
                    player: P0,
                    cards: vec![top]
                }
            );
            runner
                .act(GameAction::SelectCards { cards: vec![] })
                .unwrap();
            runner.advance_until_stack_empty();
            assert_eq!(runner.state().objects[&top].zone, Zone::Graveyard);
            assert_eq!(runner.state().players[0].library.front(), Some(&underneath));
            settled(&runner);
        }
        assert_eq!(runner.state().objects[&foreign_top].zone, Zone::Library);
    }
}

/// CR 701.57a: discover's offered card is bound by the real resolution producer.
#[test]
fn franklin_combat_discovers_six_with_explicit_hand_and_cast_choices() {
    use engine::types::actions::CastChoice;
    use engine::types::game_state::CastOfferKind;
    let db = printed_db();
    for choice in [CastChoice::Decline, CastChoice::Cast] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_real_card(P0, "Franklin Richards, Ascendant", Zone::Battlefield, &db);
        let history = paid_history_spell(&mut scenario, CoreType::Instant);
        let hit = scenario.add_real_card(P0, "Demon of Fate's Design", Zone::Library, &db);
        let above_ceiling = scenario.add_real_card(P0, "Sword Coast Serpent", Zone::Library, &db);
        let leading_land = scenario
            .add_land_to_library_top(P0, "Discover leading land")
            .id();
        blue_pool(&mut scenario, P0, 2);
        let mut runner = scenario.build();
        runner.cast(history).resolve();
        runner.advance_to_phase(Phase::BeginCombat);
        runner.advance_until_stack_empty();
        let WaitingFor::CastOffer {
            player,
            kind:
                CastOfferKind::Discover {
                    hit_card,
                    discover_value,
                    exiled_misses,
                    ..
                },
        } = &runner.state().waiting_for
        else {
            panic!("actual discover offer: {:?}", runner.state().waiting_for)
        };
        assert_eq!(*player, P0);
        assert_eq!(*hit_card, hit);
        assert_eq!(*discover_value, 6);
        assert_eq!(exiled_misses, &vec![leading_land]);
        runner.act(GameAction::DiscoverChoice { choice }).unwrap();
        runner.advance_until_stack_empty();
        assert_eq!(
            runner.state().objects[&hit].zone,
            if choice == CastChoice::Cast {
                Zone::Battlefield
            } else {
                Zone::Hand
            }
        );
        assert_eq!(
            runner.state().spells_cast_this_turn_by_player[&P0].len(),
            if choice == CastChoice::Cast { 2 } else { 1 }
        );
        assert_eq!(runner.state().objects[&above_ceiling].zone, Zone::Library);
        assert_eq!(runner.state().objects[&leading_land].zone, Zone::Library);
        settled(&runner);
    }
}

/// CR 313.4 + CR 902.7: this fixture supplies the Vanguard Command context by
/// opting the two parsed statics into that zone. It claims no format setup.
#[test]
fn heartwood_command_context_taxes_each_casters_first_qualified_spell_only() {
    use engine::game::functioning_abilities::game_functioning_statics;
    let db = printed_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let avatar = scenario.add_real_card(P0, "Heartwood Storyteller Avatar", Zone::Command, &db);
    let own: Vec<_> = (0..2)
        .map(|_| {
            scenario
                .add_creature_to_hand_from_oracle(P0, "Synthetic Avatar creature", 1, 1, "")
                .with_mana_cost(ManaCost::Cost {
                    generic: 2,
                    shards: vec![ManaCostShard::Blue],
                })
                .id()
        })
        .collect();
    let opposing: Vec<_> = (0..2)
        .map(|_| {
            scenario
                .add_spell_to_hand_from_oracle(
                    P1,
                    "Synthetic Avatar instant",
                    true,
                    "You gain 1 life.",
                )
                .with_mana_cost(ManaCost::Cost {
                    generic: 1,
                    shards: vec![ManaCostShard::Blue],
                })
                .id()
        })
        .collect();
    blue_pool(&mut scenario, P0, 10);
    blue_pool(&mut scenario, P1, 10);
    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&avatar).unwrap();
        assert_eq!(obj.zone, Zone::Command);
        assert_eq!(obj.owner, obj.controller);
        assert_eq!(obj.mana_cost, ManaCost::NoCost);
        assert!(obj.card_types.core_types.is_empty());
        assert!(!obj.is_commander && !obj.is_emblem);
        let defs: Vec<_> = obj
            .base_static_definitions
            .iter()
            .cloned()
            .map(|mut def| {
                def.active_zones = vec![Zone::Command];
                def
            })
            .collect();
        assert_eq!(defs.len(), 2);
        obj.static_definitions = defs.clone().into();
        obj.base_static_definitions = std::sync::Arc::new(defs);
    }
    runner.state_mut().layers_dirty.mark_full();
    assert_eq!(
        game_functioning_statics(runner.state())
            .filter(|(source, _)| source.id == avatar)
            .count(),
        2
    );
    runner.cast(own[0]).resolve();
    settled(&runner);
    assert_eq!(runner.state().players[0].mana_pool.total(), 8);
    runner.act(GameAction::PassPriority).unwrap();
    runner.cast(opposing[0]).resolve();
    settled(&runner);
    assert_eq!(runner.state().players[1].mana_pool.total(), 7);
    // CR 117.3b: resolution returned priority to the active player P0.
    runner.act(GameAction::PassPriority).unwrap();
    runner.cast(opposing[1]).resolve();
    settled(&runner);
    assert_eq!(runner.state().players[1].mana_pool.total(), 5);
    runner.cast(own[1]).resolve();
    settled(&runner);
    assert_eq!(runner.state().players[0].mana_pool.total(), 5);
    for player in [P0, P1] {
        assert_eq!(
            runner.state().spells_cast_this_turn_by_player[&player].len(),
            2
        );
    }
    assert_eq!(runner.state().objects[&avatar].zone, Zone::Command);
}

/// CR 101.2: the live ban excludes the full noncreature population and permits
/// a creature with the same mana cost. Failed casts cannot write history.
#[test]
fn gaddock_full_printed_mana_value_and_x_bans_allow_creature_twins() {
    let db = printed_db();
    for (creature, generic, has_x, allowed) in [
        (false, 3, false, false),
        (false, 2, false, true),
        (false, 0, true, false),
        (true, 3, false, true),
        (true, 0, true, true),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_real_card(P0, "Gaddock Teeg", Zone::Battlefield, &db);
        let mut shards = vec![ManaCostShard::Blue];
        if has_x {
            shards.push(ManaCostShard::X);
        }
        let cost = ManaCost::Cost { generic, shards };
        let spell = if creature {
            scenario
                .add_creature_to_hand_from_oracle(P0, "Synthetic Gaddock creature", 1, 1, "")
                .with_mana_cost(cost)
                .id()
        } else {
            scenario
                .add_spell_to_hand_from_oracle(
                    P0,
                    "Synthetic Gaddock instant",
                    true,
                    "You gain 1 life.",
                )
                .with_mana_cost(cost)
                .id()
        };
        blue_pool(&mut scenario, P0, 8);
        let mut runner = scenario.build();
        let result = runner.cast(spell).x(0).try_resolve();
        if allowed {
            result.unwrap();
            assert_eq!(runner.state().spells_cast_this_turn_by_player[&P0].len(), 1);
            assert_eq!(runner.state().players[0].mana_pool.total(), 8 - generic - 1);
        } else {
            assert!(result.is_err());
            assert_eq!(runner.state().objects[&spell].zone, Zone::Hand);
            assert_eq!(runner.state().players[0].mana_pool.total(), 8);
            assert!(runner
                .state()
                .spells_cast_this_turn_by_player
                .get(&P0)
                .is_none_or(|records| records.is_empty()));
        }
        settled(&runner);
    }
}

/// CR 611.2a + CR 514.2: resolution installs a turn-bound noncreature ban;
/// Calamity's two independent exile instructions still execute in order.
#[test]
fn calamitys_wake_exiles_graveyards_and_its_source_then_bans_only_noncreatures() {
    let db = printed_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wake = scenario.add_real_card(P0, "Calamity's Wake", Zone::Hand, &db);
    let grave_a = scenario.add_real_card(P0, "Seeker of Insight", Zone::Graveyard, &db);
    let grave_b = scenario.add_real_card(P1, "Tapestry of the Ages", Zone::Graveyard, &db);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Blue);
    scenario.with_library_top(P1, &["Next-turn draw", "Reserve"]);
    let forbidden = paid_history_spell(&mut scenario, CoreType::Instant);
    let legal = paid_history_spell(&mut scenario, CoreType::Creature);
    blue_pool(&mut scenario, P0, 8);
    let mut runner = scenario.build();
    runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
        ManaType::White,
        ObjectId(0),
        false,
        vec![],
    ));
    runner.cast(wake).resolve();
    settled(&runner);
    for id in [wake, grave_a, grave_b] {
        assert_eq!(runner.state().objects[&id].zone, Zone::Exile);
    }
    let before = runner.state().players[0].mana_pool.total();
    assert!(runner.cast(forbidden).try_resolve().is_err());
    assert_eq!(runner.state().objects[&forbidden].zone, Zone::Hand);
    assert_eq!(runner.state().players[0].mana_pool.total(), before);
    runner.cast(legal).resolve();
    settled(&runner);
    assert_eq!(runner.state().objects[&legal].zone, Zone::Battlefield);
    assert_eq!(runner.state().spells_cast_this_turn_by_player[&P0].len(), 2);
    let turn = runner.state().turn_number;
    runner.advance_to_combat();
    runner.declare_attackers(&[]).unwrap();
    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert!(runner.state().turn_number > turn);
    runner.act(GameAction::PassPriority).unwrap();
    runner.cast(forbidden).resolve();
    settled(&runner);
    assert_eq!(runner.state().objects[&forbidden].zone, Zone::Graveyard);
}

/// CR 508.1c + CR 509.1b: the real declaration checks use the complete live
/// history predicate. A ready legal blocker guards the unblockability negative.
#[test]
fn snapper_and_serpent_real_combat_distinguish_creature_only_history() {
    use engine::game::combat::AttackTarget;
    let db = printed_db();
    for name in ["Silburlind Snapper", "Sword Coast Serpent"] {
        for creature in [true, false] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let attacker = scenario.add_real_card(P0, name, Zone::Battlefield, &db);
            let blocker = scenario.add_creature(P1, "Ready blocker", 0, 8).id();
            let guard = scenario
                .add_creature(P0, "Ready legal attack witness", 1, 1)
                .id();
            let history = paid_history_spell(
                &mut scenario,
                if creature {
                    CoreType::Creature
                } else {
                    CoreType::Instant
                },
            );
            blue_pool(&mut scenario, P0, 2);
            let mut runner = scenario.build();
            runner.cast(history).resolve();
            settled(&runner);
            assert_eq!(runner.state().objects[&attacker].power, Some(6));
            assert_eq!(runner.state().objects[&attacker].toughness, Some(6));
            assert!(!runner.state().objects[&attacker].summoning_sick);
            runner.advance_to_combat();
            if name == "Silburlind Snapper" && creature {
                assert!(runner
                    .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
                    .is_err());
                runner
                    .declare_attackers(&[(guard, AttackTarget::Player(P1))])
                    .unwrap();
                runner.advance_to_phase(Phase::DeclareBlockers);
                runner.declare_blockers(&[]).unwrap();
                let damage = runner.combat_damage();
                damage.assert_life_delta(P1, -1);
            } else {
                runner
                    .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
                    .unwrap();
                runner.advance_to_phase(Phase::DeclareBlockers);
                if name == "Sword Coast Serpent" {
                    let result = runner.declare_blockers(&[(blocker, attacker)]);
                    if creature {
                        result.unwrap();
                    } else {
                        assert!(result.is_err());
                        runner.declare_blockers(&[]).unwrap();
                    }
                } else {
                    runner.declare_blockers(&[]).unwrap();
                }
                let damage = runner.combat_damage();
                damage.assert_life_delta(
                    P1,
                    if name == "Sword Coast Serpent" && creature {
                        0
                    } else {
                        -6
                    },
                );
            }
            settled(&runner);
        }
    }
}

/// CR 601.2i + CR 603.4: current and legacy origin serialization carry the
/// actual paid history into the same public phase-event continuation.
#[test]
fn retained_history_save_resume_preserves_the_continued_payoff() {
    let db = printed_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_real_card(P0, "Mister Fantastic", Zone::Battlefield, &db);
    let cast = paid_history_spell(&mut scenario, CoreType::Instant);
    let draw = scenario.add_card_to_library_top(P0, "Saved draw witness");
    blue_pool(&mut scenario, P0, 2);
    let mut runner = scenario.build();
    runner.cast(cast).resolve();
    settled(&runner);
    let serialized = serde_json::to_value(runner.state()).unwrap();
    for legacy in [false, true] {
        let mut value = serialized.clone();
        if legacy {
            value["spells_cast_this_turn_by_player"]["0"][0]["from_zone"] = serde_json::Value::Null;
        }
        let state: engine::types::game_state::GameState = serde_json::from_value(value).unwrap();
        assert_eq!(state.objects[&source].controller, P0);
        assert_eq!(
            state.spells_cast_this_turn_by_player[&P0][0].from_zone,
            Zone::Hand
        );
        let mut continued = GameRunner::from_state(state);
        continued.advance_to_phase(Phase::BeginCombat);
        continued.advance_until_stack_empty();
        assert_eq!(continued.state().objects[&draw].zone, Zone::Hand);
        settled(&continued);
    }
}

/// CR 601.2i: Canonist counts earlier nonartifacts independently for each
/// caster, and every artifact cast remains exempt from that limit.
#[test]
fn canonist_keeps_nonartifact_limit_separate_from_artifacts_and_other_casters() {
    let db = printed_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Ethersworn Canonist", Zone::Battlefield, &db);
    let first = paid_history_spell(&mut scenario, CoreType::Instant);
    let second = paid_history_spell(&mut scenario, CoreType::Instant);
    let artifacts: Vec<_> = (0..2)
        .map(|_| {
            scenario
                .add_artifact_to_hand_from_oracle(P0, "Synthetic Canonist artifact", "")
                .with_mana_cost(ManaCost::Cost {
                    generic: 0,
                    shards: vec![ManaCostShard::Blue],
                })
                .id()
        })
        .collect();
    let opposing: Vec<_> = (0..2)
        .map(|_| {
            scenario
                .add_spell_to_hand_from_oracle(
                    P1,
                    "Synthetic opposing Canonist instant",
                    true,
                    "You gain 1 life.",
                )
                .with_mana_cost(ManaCost::Cost {
                    generic: 0,
                    shards: vec![ManaCostShard::Blue],
                })
                .id()
        })
        .collect();
    blue_pool(&mut scenario, P0, 8);
    blue_pool(&mut scenario, P1, 8);
    let mut runner = scenario.build();
    runner.cast(first).resolve();
    settled(&runner);
    for spell in artifacts {
        runner.cast(spell).resolve();
        settled(&runner);
    }
    assert_eq!(runner.state().players[0].mana_pool.total(), 5);
    assert_eq!(runner.state().spells_cast_this_turn_by_player[&P0].len(), 3);
    assert!(runner.cast(second).try_resolve().is_err());
    assert_eq!(runner.state().players[0].mana_pool.total(), 5);
    assert_eq!(runner.state().objects[&second].zone, Zone::Hand);
    runner.act(GameAction::PassPriority).unwrap();
    runner.cast(opposing[0]).resolve();
    settled(&runner);
    assert_eq!(runner.state().players[1].mana_pool.total(), 7);
    runner.act(GameAction::PassPriority).unwrap();
    assert!(runner.cast(opposing[1]).try_resolve().is_err());
    assert_eq!(runner.state().spells_cast_this_turn_by_player[&P1].len(), 1);
    settled(&runner);
}

/// CR 118.9: a real enchantment spell can select Demon's life-as-mana-value
/// cost once on its controller's turn; the nonenchantment twin pays normally.
#[test]
fn demon_life_alternative_is_filtered_and_actually_paid_once() {
    use engine::types::ability::{AbilityCost, QuantityExpr, QuantityRef};
    let db = printed_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Demon of Fate's Design", Zone::Battlefield, &db);
    let enchantment = scenario.add_real_card(P0, "Fae Offering", Zone::Hand, &db);
    let second = scenario.add_real_card(P0, "Fae Offering", Zone::Hand, &db);
    let ordinary = paid_history_spell(&mut scenario, CoreType::Instant);
    blue_pool(&mut scenario, P0, 8);
    let mut runner = scenario.build();
    runner
        .act(GameAction::CastSpell {
            object_id: enchantment,
            card_id: runner.state().objects[&enchantment].card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .unwrap();
    let WaitingFor::OptionalCostChoice {
        player,
        cost: engine::types::ability::AdditionalCost::Choice(alternative, _),
        ..
    } = &runner.state().waiting_for
    else {
        panic!("life-cost choice: {:?}", runner.state().waiting_for)
    };
    assert_eq!(*player, P0);
    assert_eq!(
        *alternative,
        AbilityCost::PayLife {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::SelfManaValue
            }
        }
    );
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .unwrap();
    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P0), 17);
    assert_eq!(runner.state().players[0].mana_pool.total(), 8);
    assert_eq!(runner.state().objects[&enchantment].zone, Zone::Battlefield);
    settled(&runner);
    runner.cast(ordinary).resolve();
    settled(&runner);
    assert_eq!(runner.life(P0), 18);
    assert_eq!(runner.state().players[0].mana_pool.total(), 7);
    assert!(
        runner.cast(second).try_resolve().is_err(),
        "the spent grant cannot bypass missing green mana"
    );
    assert_eq!(runner.state().objects[&second].zone, Zone::Hand);
    assert_eq!(runner.state().spells_cast_this_turn_by_player[&P0].len(), 2);
}

/// CR 611.2a: the resolved sacrifice ability retains its source controller
/// after the source leaves; its full noncreature predicate excludes creatures.
#[test]
fn ranger_captain_sacrifices_source_and_resolved_ban_survives_departure() {
    let db = printed_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_real_card(P0, "Ranger-Captain of Eos", Zone::Battlefield, &db);
    let own = paid_history_spell(&mut scenario, CoreType::Instant);
    let forbidden = scenario
        .add_spell_to_hand_from_oracle(
            P1,
            "Synthetic Ranger forbidden instant",
            true,
            "You gain 1 life.",
        )
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    blue_pool(&mut scenario, P0, 3);
    blue_pool(&mut scenario, P1, 3);
    scenario.add_basic_land(P1, engine::types::mana::ManaColor::Blue);
    scenario.with_library_top(P1, &["Ranger next-turn draw", "Reserve"]);
    let mut runner = scenario.build();
    runner.activate(source, 0).resolve();
    settled(&runner);
    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    runner.cast(own).resolve();
    settled(&runner);
    assert_eq!(runner.state().players[0].mana_pool.total(), 2);
    runner.act(GameAction::PassPriority).unwrap();
    assert!(runner.cast(forbidden).try_resolve().is_err());
    assert_eq!(runner.state().objects[&forbidden].zone, Zone::Hand);
    assert_eq!(runner.state().players[1].mana_pool.total(), 3);
    settled(&runner);
    let turn = runner.state().turn_number;
    runner.advance_to_combat();
    runner.declare_attackers(&[]).unwrap();
    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert!(runner.state().turn_number > turn);
    runner.cast(forbidden).resolve();
    settled(&runner);
    assert_eq!(runner.state().objects[&forbidden].zone, Zone::Graveyard);
}

/// CR 608.2b: the legal counter target's inability to be countered does not
/// stop the following all-opponent prohibition from being installed.
#[test]
fn permission_denied_counters_selected_spell_and_bans_opponents_only() {
    let db = printed_db();
    for uncounterable in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let permission = scenario.add_real_card(P0, "Permission Denied", Zone::Hand, &db);
        let own = paid_history_spell(&mut scenario, CoreType::Instant);
        let target = scenario
            .add_spell_to_hand_from_oracle(
                P1,
                "Synthetic Permission target",
                true,
                if uncounterable {
                    "This spell can't be countered.\nYou gain 1 life."
                } else {
                    "You gain 1 life."
                },
            )
            .with_mana_cost(ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::Blue],
            })
            .id();
        let forbidden = scenario
            .add_spell_to_hand_from_oracle(
                P1,
                "Synthetic second opposing instant",
                true,
                "You gain 1 life.",
            )
            .with_mana_cost(ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::Blue],
            })
            .id();
        blue_pool(&mut scenario, P0, 7);
        blue_pool(&mut scenario, P1, 7);
        scenario.add_basic_land(P1, engine::types::mana::ManaColor::Blue);
        scenario.with_library_top(P1, &["Permission next-turn draw", "Reserve"]);
        let mut runner = scenario.build();
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::White,
            ObjectId(0),
            false,
            vec![],
        ));
        runner.act(GameAction::PassPriority).unwrap();
        drop(runner.cast(target).commit());
        assert_eq!(runner.state().objects[&target].zone, Zone::Stack);
        runner.act(GameAction::PassPriority).unwrap();
        runner.cast(permission).target_object(target).resolve();
        settled(&runner);
        assert_eq!(runner.state().objects[&target].zone, Zone::Graveyard);
        assert_eq!(runner.life(P1), if uncounterable { 21 } else { 20 });
        runner.cast(own).resolve();
        settled(&runner);
        runner.act(GameAction::PassPriority).unwrap();
        let before = runner.state().players[1].mana_pool.total();
        assert!(runner.cast(forbidden).try_resolve().is_err());
        assert_eq!(runner.state().players[1].mana_pool.total(), before);
        assert_eq!(runner.state().objects[&forbidden].zone, Zone::Hand);
        assert_eq!(runner.state().spells_cast_this_turn_by_player[&P1].len(), 1);
        settled(&runner);
        let turn = runner.state().turn_number;
        runner.advance_to_combat();
        runner.declare_attackers(&[]).unwrap();
        runner.advance_to_end_step();
        runner.advance_to_phase(Phase::PreCombatMain);
        assert!(runner.state().turn_number > turn);
        runner.cast(forbidden).resolve();
        settled(&runner);
        assert_eq!(runner.state().objects[&forbidden].zone, Zone::Graveyard);
    }
}

/// CR 601.2i: the history records the cast origin and the characteristics
/// actually used on the stack, even after Adventure changes to its other face.
#[test]
fn impending_flux_counts_real_adventure_exile_cast_but_not_hand_casts() {
    let db = printed_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let adventurer = scenario.add_real_card(P0, "Sword Coast Serpent", Zone::Hand, &db);
    let flux = scenario.add_real_card(P0, "Impending Flux", Zone::Hand, &db);
    let returned = scenario
        .add_creature(P1, "Adventure return witness", 1, 1)
        .id();
    let damaged = scenario.add_creature(P1, "Flux damage witness", 0, 8).id();
    blue_pool(&mut scenario, P0, 13);
    let mut runner = scenario.build();
    runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
        ManaType::Red,
        ObjectId(0),
        false,
        vec![],
    ));
    runner
        .cast(adventurer)
        .adventure_face(false)
        .target_object(returned)
        .resolve();
    settled(&runner);
    assert_eq!(runner.state().objects[&returned].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&adventurer].zone, Zone::Exile);
    assert_eq!(
        runner.state().spells_cast_this_turn_by_player[&P0][0].core_types,
        vec![engine::types::card_type::CoreType::Instant]
    );
    runner.cast(adventurer).adventure_face(true).resolve();
    settled(&runner);
    let records = &runner.state().spells_cast_this_turn_by_player[&P0];
    assert_eq!(records.len(), 2);
    assert_eq!(
        records[0].core_types,
        vec![engine::types::card_type::CoreType::Instant]
    );
    assert_eq!(records[0].from_zone, Zone::Hand);
    assert_eq!(records[1].from_zone, Zone::Exile);
    assert_eq!(
        records[1].core_types,
        vec![engine::types::card_type::CoreType::Creature]
    );
    let outcome = runner.cast(flux).resolve();
    outcome.assert_life_delta(P1, -2);
    assert_eq!(outcome.state().objects[&damaged].damage_marked, 2);
    assert_eq!(
        outcome.state().spells_cast_this_turn_by_player[&P0].len(),
        3
    );
    settled(&runner);
}

/// CR 700.6 + CR 509.1b: each historic axis independently enables Relic
/// Runner's actual unblockability; ordinary spells leave the legal block intact.
#[test]
fn relic_runner_historic_axes_are_live_at_real_block_declaration() {
    use engine::game::combat::AttackTarget;
    let db = printed_db();
    for axis in ["artifact", "legendary", "saga", "ordinary"] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let runner_id = scenario.add_real_card(P0, "Relic Runner", Zone::Battlefield, &db);
        let blocker = scenario.add_creature(P1, "Relic blocker", 0, 8).id();
        let spell = match axis {
            "artifact" => scenario
                .add_artifact_to_hand_from_oracle(P0, "Synthetic historic artifact", "")
                .with_mana_cost(ManaCost::Cost {
                    generic: 0,
                    shards: vec![ManaCostShard::Blue],
                })
                .id(),
            "legendary" => scenario
                .add_creature_to_hand_from_oracle(P0, "Synthetic historic legend", 1, 1, "")
                .as_legendary()
                .with_mana_cost(ManaCost::Cost {
                    generic: 0,
                    shards: vec![ManaCostShard::Blue],
                })
                .id(),
            "saga" => scenario
                .add_spell_to_hand_from_oracle(P0, "Synthetic historic Saga", false, "")
                .as_enchantment()
                .with_subtypes(vec!["Saga"])
                .with_mana_cost(ManaCost::Cost {
                    generic: 0,
                    shards: vec![ManaCostShard::Blue],
                })
                .id(),
            _ => paid_history_spell(&mut scenario, CoreType::Instant),
        };
        blue_pool(&mut scenario, P0, 2);
        let mut runner = scenario.build();
        runner.cast(spell).resolve();
        settled(&runner);
        assert_eq!(runner.state().spells_cast_this_turn_by_player[&P0].len(), 1);
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(runner_id, AttackTarget::Player(P1))])
            .unwrap();
        runner.advance_to_phase(Phase::DeclareBlockers);
        let result = runner.declare_blockers(&[(blocker, runner_id)]);
        if axis == "ordinary" {
            result.unwrap();
        } else {
            assert!(result.is_err());
            runner.declare_blockers(&[]).unwrap();
        }
        let outcome = runner.combat_damage();
        outcome.assert_life_delta(P1, if axis == "ordinary" { 0 } else { -2 });
        settled(&runner);
    }
}

/// CR 109.5 + CR 603.4: two same-name hosts cannot borrow the other caster's
/// history, and the trigger belongs to the active player's source controller.
#[test]
fn two_mister_hosts_use_each_controllers_history_without_borrowing() {
    let db = printed_db();
    for caster in [P0, P1] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_real_card(P0, "Mister Fantastic", Zone::Battlefield, &db);
        scenario.add_real_card(P1, "Mister Fantastic", Zone::Battlefield, &db);
        let spell = scenario
            .add_spell_to_hand_from_oracle(
                caster,
                "Synthetic per-caster instant",
                true,
                "You gain 1 life.",
            )
            .with_mana_cost(ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::Blue],
            })
            .id();
        let own_draw = scenario.add_card_to_library_top(P0, "P0 unique draw");
        let other_draw = scenario.add_card_to_library_top(P1, "P1 unique draw");
        blue_pool(&mut scenario, caster, 2);
        let mut runner = scenario.build();
        if caster == P1 {
            runner.act(GameAction::PassPriority).unwrap();
        }
        runner.cast(spell).resolve();
        settled(&runner);
        assert_eq!(
            runner.state().spells_cast_this_turn_by_player[&caster].len(),
            1
        );
        runner.advance_to_phase(Phase::BeginCombat);
        runner.advance_until_stack_empty();
        assert_eq!(
            runner.state().objects[&own_draw].zone,
            if caster == P0 {
                Zone::Hand
            } else {
                Zone::Library
            }
        );
        assert_eq!(runner.state().objects[&other_draw].zone, Zone::Library);
        settled(&runner);
    }
}

/// CR 109.5: Alicia's independent end-step instruction returns each creature
/// to its owner, even where that owner differs from its current controller.
#[test]
fn alicia_end_step_restores_both_stolen_creatures_to_their_actual_owners() {
    let db = printed_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(
        P0,
        "Alicia Masters, Skilled Sculptor",
        Zone::Battlefield,
        &db,
    );
    let a = scenario.add_creature(P0, "Owned by P0", 1, 1).id();
    let b = scenario.add_creature(P1, "Owned by P1", 1, 1).id();
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&a).unwrap().controller = P1;
    runner.state_mut().objects.get_mut(&b).unwrap().controller = P0;
    runner.advance_to_combat();
    runner.declare_attackers(&[]).unwrap();
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&a].owner, P0);
    assert_eq!(runner.state().objects[&a].controller, P0);
    assert_eq!(runner.state().objects[&b].owner, P1);
    assert_eq!(runner.state().objects[&b].controller, P1);
    settled(&runner);
}

/// CR 605.3a: both printed mana abilities pay their own tap and mana costs.
#[test]
fn herbie_independent_mana_abilities_pay_and_choose_green() {
    use engine::types::game_state::{CastPaymentMode, ManaChoice, ManaChoicePrompt};
    let db = printed_db();
    for ability_index in [0, 1] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source =
            scenario.add_real_card(P0, "H.E.R.B.I.E., Lovable Robot", Zone::Battlefield, &db);
        blue_pool(&mut scenario, P0, 2);
        scenario.with_library_top(P0, &["Own mana reset draw", "Reserve"]);
        scenario.with_library_top(P1, &["Other mana reset draw", "Reserve"]);
        let mut runner = scenario.build();
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index,
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        if ability_index == 1 {
            let WaitingFor::ChooseManaColor {
                player,
                choice: ManaChoicePrompt::SingleColor { options },
                ..
            } = &runner.state().waiting_for
            else {
                panic!("actual color choice: {:?}", runner.state().waiting_for)
            };
            assert_eq!(*player, P0);
            assert!(options.contains(&ManaType::Green));
            assert_eq!(
                options
                    .iter()
                    .copied()
                    .collect::<std::collections::BTreeSet<_>>(),
                [
                    ManaType::White,
                    ManaType::Blue,
                    ManaType::Black,
                    ManaType::Red,
                    ManaType::Green
                ]
                .into_iter()
                .collect()
            );
            assert_eq!(options.len(), 5);
            runner
                .act(GameAction::ChooseManaColor {
                    choice: ManaChoice::SingleColor(ManaType::Green),
                    count: 1,
                })
                .unwrap();
            assert_eq!(
                runner.state().players[0]
                    .mana_pool
                    .count_color(ManaType::Green),
                1
            );
            assert_eq!(runner.state().players[0].mana_pool.total(), 2);
        } else {
            assert_eq!(
                runner.state().players[0]
                    .mana_pool
                    .count_color(ManaType::Colorless),
                1
            );
            assert_eq!(runner.state().players[0].mana_pool.total(), 3);
        }
        assert!(runner.state().objects[&source].tapped);
        assert!(runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index,
                payment_mode: CastPaymentMode::Auto
            })
            .is_err());
        for _ in 0..2 {
            runner.advance_to_combat();
            runner.declare_attackers(&[]).unwrap();
            runner.advance_to_end_step();
            runner.advance_to_phase(Phase::PreCombatMain);
        }
        assert_eq!(runner.state().active_player, P0);
        assert!(!runner.state().objects[&source].tapped);
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        assert!(runner.state().objects[&source].tapped);
        assert_eq!(
            runner.state().players[0]
                .mana_pool
                .count_color(ManaType::Colorless),
            1
        );
        settled(&runner);
        settled(&runner);
    }
}

/// CR 707.10: two copies refer to the chosen actual triggered stack entry,
/// while a second eligible trigger proves selection is not an arbitrary top.
#[test]
fn mister_copies_selected_real_phase_trigger_twice_with_actual_payment() {
    use engine::types::ability::TargetRef;
    use engine::types::game_state::CastPaymentMode;
    use engine::types::mana::ManaColor;
    let db = printed_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mister = scenario.add_real_card(P0, "Mister Fantastic", Zone::Battlefield, &db);
    let other_host = scenario
        .add_enchantment_from_oracle(
            P0,
            "Synthetic independent draw host",
            "At the beginning of combat on your turn, draw a card.",
        )
        .id();
    let history = paid_history_spell(&mut scenario, CoreType::Instant);
    let own_spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic non-trigger stack option",
            true,
            "You gain 1 life.",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let foreign_spell = scenario
        .add_spell_to_hand_from_oracle(
            P1,
            "Synthetic foreign cast trigger",
            true,
            "When you cast this spell, draw a card.\nYou gain 1 life.",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_library_top(P0, &["Draw A", "Draw B", "Draw C", "Draw D", "Reserve"]);
    scenario.with_library_top(P1, &["Foreign trigger draw", "Foreign reserve"]);
    for color in [
        ManaColor::Red,
        ManaColor::Green,
        ManaColor::White,
        ManaColor::Blue,
    ] {
        scenario.add_basic_land(P0, color);
    }
    blue_pool(&mut scenario, P0, 1);
    let mut runner = scenario.build();
    runner.cast(history).resolve();
    settled(&runner);
    runner.advance_to_phase(Phase::BeginCombat);
    let WaitingFor::OrderTriggers {
        player, triggers, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "two actual phase triggers: {:?}",
            runner.state().waiting_for
        )
    };
    assert_eq!(*player, P0);
    assert_eq!(triggers.len(), 2);
    assert_eq!(
        triggers
            .iter()
            .map(|trigger| trigger.source_id)
            .collect::<std::collections::BTreeSet<_>>(),
        [mister, other_host].into_iter().collect()
    );
    let ordered_sources = [triggers[1].source_id, triggers[0].source_id];
    runner
        .act(GameAction::OrderTriggers { order: vec![1, 0] })
        .unwrap();
    let eligible: Vec<_> = runner.state().stack.iter().map(|entry| entry.id).collect();
    assert_eq!(eligible.len(), 2);
    for (entry, expected_source) in runner.state().stack.iter().zip(ordered_sources) {
        let engine::types::game_state::StackEntryKind::TriggeredAbility {
            source_id, ability, ..
        } = &entry.kind
        else {
            panic!("each original option is a real phase trigger")
        };
        assert_eq!(*source_id, expected_source);
        assert_eq!(entry.controller, P0);
        assert_eq!(ability.source_id, expected_source);
    }
    let chosen = eligible[0];
    let chosen_entry = runner.state().stack[0].clone();
    assert_ne!(chosen, *eligible.last().unwrap());
    runner.cast(own_spell).commit();
    runner.act(GameAction::PassPriority).unwrap();
    runner.cast(foreign_spell).commit();
    let foreign_trigger = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.controller == P1 && ![own_spell, foreign_spell].contains(&entry.id))
        .expect("actual opposing triggered ability")
        .id;
    runner.act(GameAction::PassPriority).unwrap();
    let before = runner.state().players[0].hand.len();
    runner
        .act(GameAction::ActivateAbility {
            source_id: mister,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    let WaitingFor::TargetSelection {
        player,
        target_slots,
        selection,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("actual trigger target selection")
    };
    assert_eq!(*player, P0);
    assert_eq!(
        target_slots[selection.current_slot]
            .legal_targets
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>(),
        eligible.iter().copied().map(TargetRef::Object).collect()
    );
    for invalid in [own_spell, foreign_trigger] {
        assert!(runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(invalid))
            })
            .is_err());
    }
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(chosen)),
        })
        .unwrap();
    assert!(runner.state().objects[&mister].tapped);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    runner.resolve_top();
    assert_eq!(runner.state().stack.len(), 7);
    for copy in runner.state().stack.iter().rev().take(2) {
        assert_ne!(copy.id, chosen);
        assert_eq!(copy.controller, P0);
        assert_eq!(copy.source_id, chosen_entry.source_id);
        assert_eq!(copy.kind, chosen_entry.kind);
    }
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().players[0].hand.len(), before + 4);
    settled(&runner);
}

/// CR 201.2 + CR 601.2i: the two named histories are separate predicates;
/// two actual casts with one name cannot replace the missing other name.
#[test]
fn sift_named_histories_drive_real_peer_choices_discard_and_exact_search() {
    let db = printed_db();
    for prior in [1, 2, 3] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let peer = scenario.add_real_card(P0, "Peer Through Depths", Zone::Hand, &db);
        let sift = scenario.add_real_card(P0, "Sift Through Sands", Zone::Hand, &db);
        let mut looked =
            vec![scenario.add_real_card(P0, "Tapestry of the Ages", Zone::Library, &db)];
        let found_reach = scenario.add_real_card(P0, "Reach Through Mists", Zone::Library, &db);
        looked.push(found_reach);
        for _ in 0..3 {
            looked.push(scenario.add_real_card(P0, "Tapestry of the Ages", Zone::Library, &db));
        }
        let draws: Vec<_> = (0..3)
            .map(|_| scenario.add_real_card(P0, "Seeker of Insight", Zone::Library, &db))
            .collect();
        let unspeakable = scenario.add_real_card(P0, "The Unspeakable", Zone::Library, &db);
        let reaches: Vec<_> = if prior == 3 {
            vec![]
        } else {
            (0..prior)
                .map(|_| scenario.add_real_card(P0, "Reach Through Mists", Zone::Hand, &db))
                .collect()
        };
        blue_pool(&mut scenario, P0, 12);
        let mut runner = scenario.build();
        if prior == 3 {
            runner.cast(peer).resolve();
            let WaitingFor::DigChoice {
                player,
                cards,
                selectable_cards,
                keep_count,
                ..
            } = &runner.state().waiting_for
            else {
                panic!("real Peer dig choice: {:?}", runner.state().waiting_for)
            };
            assert_eq!(*player, P0);
            assert_eq!(*keep_count, 1);
            assert_eq!(cards, &looked);
            assert_eq!(selectable_cards, &vec![found_reach]);
            runner
                .act(GameAction::SelectCards {
                    cards: vec![found_reach],
                })
                .unwrap();
            let WaitingFor::EffectZoneChoice {
                player,
                cards,
                count,
                ..
            } = &runner.state().waiting_for
            else {
                panic!("real rest ordering: {:?}", runner.state().waiting_for)
            };
            assert_eq!(*player, P0);
            assert_eq!(*count, 4);
            let mut rest = looked
                .iter()
                .copied()
                .filter(|id| *id != found_reach)
                .collect::<Vec<_>>();
            assert_eq!(
                cards
                    .iter()
                    .copied()
                    .collect::<std::collections::BTreeSet<_>>(),
                rest.iter().copied().collect()
            );
            rest.reverse();
            runner.act(GameAction::SelectCards { cards: rest }).unwrap();
            runner.advance_until_stack_empty();
            settled(&runner);
            assert_eq!(runner.state().objects[&found_reach].zone, Zone::Hand);
            runner.cast(found_reach).resolve();
            settled(&runner);
            assert_eq!(runner.state().objects[&draws[0]].zone, Zone::Hand);
        } else {
            for reach in reaches {
                runner.cast(reach).resolve();
                settled(&runner);
            }
        }
        runner.cast(sift).commit();
        runner.advance_until_stack_empty();
        let WaitingFor::DiscardChoice {
            player,
            cards,
            count,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("Sift mandatory discard: {:?}", runner.state().waiting_for)
        };
        assert_eq!(*player, P0);
        assert_eq!(*count, 1);
        assert!(cards.len() >= 2);
        let expected_hand = match prior {
            1 => vec![peer, looked[0], found_reach, looked[2]],
            2 => vec![peer, looked[0], found_reach, looked[2], looked[3]],
            3 => draws.clone(),
            _ => unreachable!(),
        };
        assert_eq!(cards.len(), expected_hand.len());
        assert_eq!(
            cards
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            expected_hand.iter().copied().collect()
        );
        let discard = if prior == 3 { draws[1] } else { looked[2] };
        assert_ne!(discard, cards[0]);
        runner
            .act(GameAction::SelectCards {
                cards: vec![discard],
            })
            .unwrap();
        assert_eq!(runner.state().objects[&discard].zone, Zone::Graveyard);
        if prior == 3 {
            let WaitingFor::OptionalEffectChoice { player, .. } = &runner.state().waiting_for
            else {
                panic!(
                    "named-history search offer: {:?}",
                    runner.state().waiting_for
                )
            };
            assert_eq!(*player, P0);
            runner
                .act(GameAction::DecideOptionalEffect { accept: true })
                .unwrap();
            let WaitingFor::SearchChoice {
                player,
                cards,
                count,
                ..
            } = &runner.state().waiting_for
            else {
                panic!("exact named search: {:?}", runner.state().waiting_for)
            };
            assert_eq!(*player, P0);
            assert_eq!(*count, 1);
            assert_eq!(cards, &vec![unspeakable]);
            runner
                .act(GameAction::SelectCards {
                    cards: vec![unspeakable],
                })
                .unwrap();
            runner.advance_until_stack_empty();
            assert_eq!(runner.state().objects[&unspeakable].zone, Zone::Battlefield);
            assert_eq!(runner.state().objects[&unspeakable].power, Some(6));
            assert_eq!(runner.state().objects[&unspeakable].toughness, Some(7));
        } else {
            assert_eq!(runner.state().objects[&unspeakable].zone, Zone::Library);
        }
        settled(&runner);
        assert_eq!(
            runner.state().spells_cast_this_turn_by_player[&P0].len(),
            if prior == 3 { 3 } else { prior as usize + 1 }
        );
    }
}

/// CR 603.3a + CR 603.4: a real control-changing creature's ETB makes
/// the new controller's history authoritative before the phase event.
#[test]
fn combat_history_uses_live_controller_after_real_control_change() {
    let db = printed_db();
    for name in [
        "Council of Reeds",
        "Alicia Masters, Skilled Sculptor",
        "Human Torch",
        "Invisible Woman",
        "The Thing",
        "Lockjaw, Slobbering Teleporter",
        "Mister Fantastic",
    ] {
        for matching in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let host = scenario.add_real_card(P1, name, Zone::Battlefield, &db);
            let theft = scenario
                .add_creature_to_hand_from_oracle(
                    P0,
                    "Synthetic creature theft",
                    1,
                    1,
                    "When this creature enters, gain control of target creature.",
                )
                .with_mana_cost(ManaCost::Cost {
                    generic: 0,
                    shards: vec![ManaCostShard::Blue],
                })
                .id();
            let own = paid_history_spell(&mut scenario, CoreType::Instant);
            let foreign = scenario
                .add_spell_to_hand_from_oracle(
                    P1,
                    "Synthetic previous controller instant",
                    true,
                    "You gain 1 life.",
                )
                .with_mana_cost(ManaCost::Cost {
                    generic: 0,
                    shards: vec![ManaCostShard::Blue],
                })
                .id();
            let draw = scenario.add_card_to_library_top(P0, "New controller phase draw");
            blue_pool(&mut scenario, P0, 3);
            blue_pool(&mut scenario, P1, 2);
            let mut runner = scenario.build();
            runner.act(GameAction::PassPriority).unwrap();
            runner.cast(foreign).resolve();
            settled(&runner);
            runner.cast(theft).target_object(host).resolve();
            settled(&runner);
            assert_eq!(runner.state().objects[&host].owner, P1);
            assert_eq!(runner.state().objects[&host].controller, P0);
            if matching {
                runner.cast(own).resolve();
                settled(&runner);
            }
            assert_eq!(
                runner.state().spells_cast_this_turn_by_player[&P0].len(),
                if matching { 2 } else { 1 }
            );
            runner.advance_to_phase(Phase::BeginCombat);
            runner.advance_until_stack_empty();
            settled(&runner);
            match name {
                "Council of Reeds" => {
                    let copies: Vec<_> = runner
                        .state()
                        .battlefield
                        .iter()
                        .filter(|id| runner.state().objects[id].name == name)
                        .collect();
                    assert_eq!(copies.len(), if matching { 2 } else { 1 });
                    assert!(copies
                        .iter()
                        .all(|id| runner.state().objects[*id].controller == P0));
                }
                "Alicia Masters, Skilled Sculptor" | "Invisible Woman" => {
                    let kind = if name == "Invisible Woman" {
                        "Wall"
                    } else {
                        "Treasure"
                    };
                    assert_eq!(
                        runner
                            .state()
                            .battlefield
                            .iter()
                            .filter(|id| {
                                let obj = &runner.state().objects[id];
                                obj.is_token
                                    && obj.controller == P0
                                    && obj.card_types.subtypes.iter().any(|s| s == kind)
                            })
                            .count(),
                        usize::from(matching)
                    );
                }
                "Human Torch" => {
                    for keyword in [Keyword::Flying, Keyword::DoubleStrike, Keyword::Haste] {
                        assert_eq!(
                            runner.state().objects[&host].keywords.contains(&keyword),
                            matching
                        );
                    }
                }
                "The Thing" | "Lockjaw, Slobbering Teleporter" => assert_eq!(
                    runner.state().objects[&host]
                        .counters
                        .get(&CounterType::P1P1)
                        .copied()
                        .unwrap_or(0),
                    if matching {
                        if name == "The Thing" {
                            4
                        } else {
                            1
                        }
                    } else {
                        0
                    }
                ),
                "Mister Fantastic" => assert_eq!(
                    runner.state().objects[&draw].zone,
                    if matching { Zone::Hand } else { Zone::Library }
                ),
                _ => unreachable!(),
            }
        }
    }
}

/// CR 113.7a + CR 603.3a: a queued draw still belongs to its trigger
/// controller after the full printed host leaves the battlefield.
#[test]
fn mister_queued_trigger_survives_source_departure_and_uses_original_library() {
    let db = printed_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_real_card(P0, "Mister Fantastic", Zone::Battlefield, &db);
    let history = paid_history_spell(&mut scenario, CoreType::Instant);
    let bounce = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic source bounce",
            true,
            "Return target creature to its owner's hand.",
        )
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    let draw = scenario.add_card_to_library_top(P0, "Original controller draw");
    let foreign_draw = scenario.add_card_to_library_top(P1, "Other controller draw");
    blue_pool(&mut scenario, P0, 3);
    // CR 106.4: the initial pool empties before the paid combat-step response.
    let response_land = scenario.add_basic_land(P0, engine::types::mana::ManaColor::Blue);
    let mut runner = scenario.build();
    runner.cast(history).resolve();
    settled(&runner);
    runner.advance_to_phase(Phase::BeginCombat);
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert!(!runner.state().objects[&response_land].tapped);
    runner.cast(bounce).target_object(host).commit();
    assert!(runner.state().objects[&response_land].tapped);
    runner.resolve_top();
    assert_eq!(runner.state().objects[&host].zone, Zone::Hand);
    assert_eq!(runner.state().stack.len(), 1);
    runner.advance_until_stack_empty();
    settled(&runner);
    assert_eq!(runner.state().objects[&draw].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&foreign_draw].zone, Zone::Library);
}

/// CR 514.2: resolved Torch grants expire at cleanup, whereas placed
/// Thing counters survive the same actual turn transition.
#[test]
fn torch_grants_expire_while_thing_counters_persist() {
    let db = printed_db();
    for name in ["Human Torch", "The Thing"] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let host = scenario.add_real_card(P0, name, Zone::Battlefield, &db);
        let history = paid_history_spell(&mut scenario, CoreType::Instant);
        scenario.with_library_top(P1, &["Next draw", "Reserve"]);
        blue_pool(&mut scenario, P0, 2);
        let mut runner = scenario.build();
        runner.cast(history).resolve();
        runner.advance_to_phase(Phase::BeginCombat);
        runner.advance_until_stack_empty();
        settled(&runner);
        if name == "Human Torch" {
            assert!(runner.state().objects[&host]
                .keywords
                .contains(&Keyword::DoubleStrike));
        } else {
            assert_eq!(
                runner.state().objects[&host]
                    .counters
                    .get(&CounterType::P1P1),
                Some(&4)
            );
        }
        let turn = runner.state().turn_number;
        runner.advance_to_combat();
        runner.declare_attackers(&[]).unwrap();
        runner.advance_to_end_step();
        runner.advance_to_phase(Phase::PreCombatMain);
        assert!(runner.state().turn_number > turn);
        assert!(runner
            .state()
            .spells_cast_this_turn_by_player
            .values()
            .all(|records| records.is_empty()));
        if name == "Human Torch" {
            for keyword in [Keyword::Flying, Keyword::DoubleStrike, Keyword::Haste] {
                assert!(!runner.state().objects[&host].keywords.contains(&keyword));
            }
        } else {
            assert_eq!(
                runner.state().objects[&host]
                    .counters
                    .get(&CounterType::P1P1),
                Some(&4)
            );
            assert_eq!(runner.state().objects[&host].power, Some(9));
        }
        settled(&runner);
    }
}

/// CR 603.4 + CR 601.2i: Fae's each-end trigger also sees spells cast
/// on the opponent's turn, with artifact-creature/noncreature-artifact twins.
#[test]
fn fae_offering_opponents_end_step_uses_two_actual_artifact_populations() {
    let db = printed_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Fae Offering", Zone::Battlefield, &db);
    let creature = scenario
        .add_creature_to_hand_from_oracle(P0, "Synthetic flash artifact creature", 1, 1, "Flash")
        .as_artifact_creature()
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let noncreature = scenario
        .add_artifact_to_hand_from_oracle(P0, "Synthetic flash artifact", "Flash")
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_library_top(P1, &["Next draw", "Reserve"]);
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner.declare_attackers(&[]).unwrap();
    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P1);
    runner.act(GameAction::PassPriority).unwrap();
    runner.cast(creature).resolve();
    settled(&runner);
    runner.act(GameAction::PassPriority).unwrap();
    runner.cast(noncreature).resolve();
    settled(&runner);
    assert_eq!(runner.state().spells_cast_this_turn_by_player[&P0].len(), 2);
    runner.advance_to_combat();
    runner.declare_attackers(&[]).unwrap();
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();
    settled(&runner);
    for kind in ["Clue", "Food", "Treasure"] {
        assert_eq!(
            runner
                .state()
                .battlefield
                .iter()
                .filter(|id| {
                    let obj = &runner.state().objects[id];
                    obj.is_token
                        && obj.controller == P0
                        && obj.card_types.subtypes.iter().any(|s| s == kind)
                })
                .count(),
            1
        );
    }
}

/// CR 509.1b: a later noncreature cast changes future block legality;
/// it does not undo a block that was legal when declared.
#[test]
fn serpent_spell_after_legal_block_does_not_remove_that_block() {
    use engine::game::combat::AttackTarget;
    let db = printed_db();
    for before_block in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let serpent = scenario.add_real_card(P0, "Sword Coast Serpent", Zone::Battlefield, &db);
        let blocker = scenario
            .add_creature(P1, "Ready post-block witness", 0, 8)
            .id();
        let history = paid_history_spell(&mut scenario, CoreType::Instant);
        scenario.add_basic_land(P0, engine::types::mana::ManaColor::Blue);
        scenario.with_library_top(P0, &["Own Serpent reset draw", "Reserve"]);
        scenario.with_library_top(P1, &["Other Serpent reset draw", "Reserve"]);
        let mut runner = scenario.build();
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(serpent, AttackTarget::Player(P1))])
            .unwrap();
        if before_block {
            runner.cast(history).resolve();
            settled(&runner);
        }
        runner.advance_to_phase(Phase::DeclareBlockers);
        let block = runner.declare_blockers(&[(blocker, serpent)]);
        if before_block {
            assert!(block.is_err());
            runner.declare_blockers(&[]).unwrap();
        } else {
            block.unwrap();
            runner.cast(history).resolve();
            settled(&runner);
        }
        assert_eq!(runner.state().spells_cast_this_turn_by_player[&P0].len(), 1);
        runner
            .combat_damage()
            .assert_life_delta(P1, if before_block { -6 } else { 0 });
        assert_eq!(
            runner.state().objects[&blocker].damage_marked,
            if before_block { 0 } else { 6 }
        );
        settled(&runner);
        runner.advance_to_end_step();
        runner.advance_to_phase(Phase::PreCombatMain);
        runner.advance_to_combat();
        runner.declare_attackers(&[]).unwrap();
        runner.advance_to_end_step();
        runner.advance_to_phase(Phase::PreCombatMain);
        assert_eq!(runner.state().active_player, P0);
        assert!(runner
            .state()
            .spells_cast_this_turn_by_player
            .values()
            .all(|records| records.is_empty()));
        assert_eq!(runner.state().spells_cast_this_game_by_player[&P0].len(), 1);
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(serpent, AttackTarget::Player(P1))])
            .unwrap();
        runner.advance_to_phase(Phase::DeclareBlockers);
        runner.declare_blockers(&[(blocker, serpent)]).unwrap();
        runner.combat_damage().assert_life_delta(P1, 0);
        settled(&runner);
    }
}

/// CR 601.2i: countering the spell does not retract its completed cast;
/// Snapper's next-turn attack check loses that history through the real reset.
#[test]
fn snapper_countered_noncreature_history_allows_attack_then_resets() {
    use engine::game::combat::AttackTarget;
    let db = printed_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let snapper = scenario.add_real_card(P0, "Silburlind Snapper", Zone::Battlefield, &db);
    let history = paid_history_spell(&mut scenario, CoreType::Instant);
    scenario.add_creature(P1, "Ready reset blocker", 0, 8);
    let counter = scenario
        .add_spell_to_hand_from_oracle(
            P1,
            "Synthetic history counter",
            true,
            "Counter target spell.",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_library_top(P0, &["Own draw", "Reserve"]);
    scenario.with_library_top(P1, &["Other draw", "Reserve"]);
    blue_pool(&mut scenario, P0, 1);
    let mut runner = scenario.build();
    runner.cast(history).commit();
    runner.act(GameAction::PassPriority).unwrap();
    runner.cast(counter).target_object(history).resolve();
    settled(&runner);
    assert_eq!(runner.life(P0), 20);
    assert_eq!(runner.state().objects[&history].zone, Zone::Graveyard);
    assert_eq!(runner.state().spells_cast_this_turn_by_player[&P0].len(), 1);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(snapper, AttackTarget::Player(P1))])
        .unwrap();
    runner.advance_to_phase(Phase::DeclareBlockers);
    runner.declare_blockers(&[]).unwrap();
    runner.combat_damage().assert_life_delta(P1, -6);
    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::PreCombatMain);
    runner.advance_to_combat();
    runner.declare_attackers(&[]).unwrap();
    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P0);
    assert!(!runner.state().objects[&snapper].tapped);
    runner.advance_to_combat();
    assert!(runner
        .declare_attackers(&[(snapper, AttackTarget::Player(P1))])
        .is_err());
    runner.declare_attackers(&[]).unwrap();
    settled(&runner);
}

/// CR 313.4 + CR 902.7: supplied Command active zones model the disclosed
/// Vanguard context. Three real owners keep first-spell histories independent.
#[test]
fn heartwood_three_casters_composition_and_next_turn_first_tax() {
    use engine::game::functioning_abilities::game_functioning_statics;
    use engine::types::PlayerId;
    let db = printed_db();
    let p2 = PlayerId(2);
    for owners in [vec![P0], vec![P0, P1, p2]] {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let avatars: Vec<_> = owners
            .iter()
            .map(|owner| {
                scenario.add_real_card(*owner, "Heartwood Storyteller Avatar", Zone::Command, &db)
            })
            .collect();
        let own = scenario
            .add_creature_to_hand_from_oracle(P0, "Synthetic own first creature", 1, 1, "")
            .with_mana_cost(ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Blue],
            })
            .id();
        let opposing: Vec<_> = [P1, p2]
            .into_iter()
            .map(|player| {
                (
                    player,
                    (0..3)
                        .map(|_| {
                            scenario
                                .add_spell_to_hand_from_oracle(
                                    player,
                                    "Synthetic independent first instant",
                                    true,
                                    "You gain 1 life.",
                                )
                                .with_mana_cost(ManaCost::Cost {
                                    generic: 1,
                                    shards: vec![ManaCostShard::Blue],
                                })
                                .id()
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        for player in [P0, P1, p2] {
            blue_pool(&mut scenario, player, 15);
            for _ in 0..4 {
                scenario.add_basic_land(player, engine::types::mana::ManaColor::Blue);
            }
            scenario.with_library_top(player, &["Next draw", "Reserve"]);
        }
        let mut runner = scenario.build();
        for avatar in &avatars {
            let obj = runner.state_mut().objects.get_mut(avatar).unwrap();
            assert_eq!(obj.zone, Zone::Command);
            assert!(obj.card_types.core_types.is_empty());
            assert_eq!(obj.mana_cost, ManaCost::NoCost);
            assert_eq!(obj.owner, obj.controller);
            assert!(!obj.is_commander && !obj.is_emblem);
            let mut defs = obj.base_static_definitions.as_ref().clone();
            for def in &mut defs {
                def.active_zones = vec![Zone::Command];
            }
            obj.static_definitions = defs.clone().into();
            obj.base_static_definitions = std::sync::Arc::new(defs);
        }
        runner.state_mut().layers_dirty.mark_full();
        assert_eq!(
            game_functioning_statics(runner.state())
                .filter(|(source, _)| avatars.contains(&source.id))
                .count(),
            2 * avatars.len()
        );
        runner.cast(own).resolve();
        settled(&runner);
        assert_eq!(runner.state().players[0].mana_pool.total(), 13);
        for (player, spells) in &opposing {
            for index in 0..2 {
                for _ in 0..player.0 {
                    runner.act(GameAction::PassPriority).unwrap();
                }
                let before = runner.state().players[player.0 as usize].mana_pool.total();
                runner.cast(spells[index]).resolve();
                settled(&runner);
                let tax = if index == 0 {
                    owners.iter().filter(|owner| **owner != *player).count()
                } else {
                    0
                };
                assert_eq!(
                    runner.state().players[player.0 as usize].mana_pool.total(),
                    before - 2 - tax
                );
                assert_eq!(
                    runner.state().spells_cast_this_turn_by_player[player].len(),
                    index + 1
                );
            }
        }
        let old_turn = runner.state().turn_number;
        runner.advance_to_combat();
        runner.declare_attackers(&[]).unwrap();
        runner.advance_to_end_step();
        runner.advance_to_phase(Phase::PreCombatMain);
        assert!(runner.state().turn_number > old_turn);
        assert_eq!(runner.state().active_player, P1);
        let spell = opposing[0].1[2];
        runner.cast(spell).resolve();
        settled(&runner);
        assert_eq!(runner.state().spells_cast_this_turn_by_player[&P1].len(), 1);
        let paid_lands = runner
            .state()
            .battlefield
            .iter()
            .filter(|id| {
                let obj = &runner.state().objects[id];
                obj.controller == P1
                    && obj.tapped
                    && obj
                        .card_types
                        .core_types
                        .contains(&engine::types::card_type::CoreType::Land)
            })
            .count();
        assert_eq!(
            paid_lands,
            2 + owners.iter().filter(|owner| **owner != P1).count()
        );
        for avatar in &avatars {
            assert_eq!(runner.state().objects[avatar].zone, Zone::Command);
        }
    }
}

/// CR 604.1 + CR 601.2i: the limit sees casts made before Canonist's
/// entry, and a returned new incarnation still consults those real records.
#[test]
fn canonist_entry_departure_return_and_next_turn_use_real_history() {
    let db = printed_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let canonist = scenario.add_real_card(P0, "Ethersworn Canonist", Zone::Hand, &db);
    let spells: Vec<_> = (0..3)
        .map(|_| paid_history_spell(&mut scenario, CoreType::Instant))
        .collect();
    let bounce = scenario
        .add_artifact_from_oracle(
            P0,
            "Synthetic live static remover",
            "Sacrifice this artifact: Return target creature to its owner's hand.",
        )
        .id();
    scenario.with_mana_pool(
        P0,
        (0..8)
            .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
            .chain((0..2).map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![])))
            .collect(),
    );
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Blue);
    scenario.with_library_top(P1, &["Next draw", "Reserve"]);
    let mut runner = scenario.build();
    runner.cast(spells[0]).resolve();
    runner.cast(canonist).resolve();
    settled(&runner);
    assert_eq!(runner.state().spells_cast_this_turn_by_player[&P0].len(), 2);
    let before = runner.state().players[0].mana_pool.total();
    assert!(runner.cast(spells[1]).try_resolve().is_err());
    assert_eq!(runner.state().players[0].mana_pool.total(), before);
    runner.activate(bounce, 0).target_object(canonist).resolve();
    settled(&runner);
    assert_eq!(runner.state().objects[&bounce].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&canonist].zone, Zone::Hand);
    runner.cast(spells[1]).resolve();
    runner.cast(canonist).resolve();
    settled(&runner);
    assert_eq!(runner.state().spells_cast_this_turn_by_player[&P0].len(), 4);
    assert!(runner.cast(spells[2]).try_resolve().is_err());
    assert_eq!(runner.state().objects[&spells[2]].zone, Zone::Hand);
    runner.advance_to_combat();
    runner.declare_attackers(&[]).unwrap();
    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P1);
    runner.act(GameAction::PassPriority).unwrap();
    runner.cast(spells[2]).resolve();
    settled(&runner);
    assert_eq!(runner.state().spells_cast_this_turn_by_player[&P0].len(), 1);
}

/// CR 608.2b: removing Permission's sole target before resolution prevents
/// both its counter and its following prohibition.
#[test]
fn permission_denied_illegal_target_does_not_install_a_ban() {
    let db = printed_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let permission = scenario.add_real_card(P0, "Permission Denied", Zone::Hand, &db);
    let counter = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic earlier target removal",
            true,
            "Counter target spell.",
        )
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    let opponents: Vec<_> = (0..2)
        .map(|_| {
            scenario
                .add_spell_to_hand_from_oracle(
                    P1,
                    "Synthetic legal opposing instant",
                    true,
                    "You gain 1 life.",
                )
                .with_mana_cost(ManaCost::Cost {
                    generic: 0,
                    shards: vec![ManaCostShard::Blue],
                })
                .id()
        })
        .collect();
    blue_pool(&mut scenario, P1, 3);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
        ],
    );
    let mut runner = scenario.build();
    runner.act(GameAction::PassPriority).unwrap();
    runner.cast(opponents[0]).commit();
    runner.act(GameAction::PassPriority).unwrap();
    runner.cast(permission).target_object(opponents[0]).commit();
    runner.cast(counter).target_object(opponents[0]).commit();
    runner.resolve_top();
    assert_eq!(runner.state().objects[&opponents[0]].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&permission].zone, Zone::Stack);
    runner.advance_until_stack_empty();
    settled(&runner);
    assert_eq!(runner.state().objects[&permission].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    runner.act(GameAction::PassPriority).unwrap();
    runner.cast(opponents[1]).resolve();
    settled(&runner);
    assert_eq!(runner.life(P1), 21);
    assert_eq!(runner.state().players[1].mana_pool.total(), 1);
}

/// CR 117.3c + CR 611.2a: an opponent may commit a response before the
/// sacrificed Captain's ability resolves; the later spell is then forbidden.
#[test]
fn ranger_captain_response_precedes_ban_and_etb_search_choices_are_real() {
    use engine::types::game_state::CastPaymentMode;
    let db = printed_db();
    for search in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario.add_real_card(P0, "Ranger-Captain of Eos", Zone::Hand, &db);
        let candidate = scenario
            .add_spell_to_library_top(P0, "Synthetic one-mana search target", false)
            .as_creature()
            .with_mana_cost(ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::Blue],
            })
            .id();
        let opposing: Vec<_> = (0..2)
            .map(|_| {
                scenario
                    .add_spell_to_hand_from_oracle(
                        P1,
                        "Synthetic Captain response",
                        true,
                        "You gain 1 life.",
                    )
                    .with_mana_cost(ManaCost::generic(0))
                    .id()
            })
            .collect();
        scenario.with_mana_pool(
            P0,
            vec![
                ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
                ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
                ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
            ],
        );
        let mut runner = scenario.build();
        runner.cast(source).commit().stop_at_optional().resolve();
        let WaitingFor::OptionalEffectChoice { player, .. } = &runner.state().waiting_for else {
            panic!(
                "full Captain ETB optional search: {:?}",
                runner.state().waiting_for
            )
        };
        assert_eq!(*player, P0);
        runner
            .act(GameAction::DecideOptionalEffect { accept: search })
            .unwrap();
        if search {
            let WaitingFor::SearchChoice { player, cards, .. } = &runner.state().waiting_for else {
                panic!("Captain search producer")
            };
            assert_eq!(*player, P0);
            assert_eq!(cards, &vec![candidate]);
            runner
                .act(GameAction::SelectCards {
                    cards: vec![candidate],
                })
                .unwrap();
        }
        runner.advance_until_stack_empty();
        settled(&runner);
        assert_eq!(
            runner.state().objects[&candidate].zone,
            if search { Zone::Hand } else { Zone::Library }
        );
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
        runner.act(GameAction::PassPriority).unwrap();
        runner.cast(opposing[0]).commit();
        runner.resolve_top();
        assert_eq!(runner.life(P1), 21);
        assert_eq!(runner.state().stack.len(), 1);
        runner.advance_until_stack_empty();
        settled(&runner);
        runner.act(GameAction::PassPriority).unwrap();
        assert!(runner.cast(opposing[1]).try_resolve().is_err());
        assert_eq!(runner.state().objects[&opposing[1]].zone, Zone::Hand);
        assert_eq!(runner.state().spells_cast_this_turn_by_player[&P1].len(), 1);
        settled(&runner);
    }
}

/// CR 101.2 + CR 611.2a: the ban's affected-player authority is independent
/// of the countered spell's caster in an actual three-player game.
#[test]
fn complete_temporary_bans_reach_every_opponent_in_three_player_games() {
    use engine::types::PlayerId;
    let db = printed_db();
    let p2 = PlayerId(2);
    for name in [
        "Calamity's Wake",
        "Permission Denied",
        "Ranger-Captain of Eos",
    ] {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario.add_real_card(
            P0,
            name,
            if name == "Ranger-Captain of Eos" {
                Zone::Battlefield
            } else {
                Zone::Hand
            },
            &db,
        );
        let own = paid_history_spell(&mut scenario, CoreType::Instant);
        let witnesses: Vec<_> = [P1, p2]
            .into_iter()
            .map(|player| {
                let instant = scenario
                    .add_spell_to_hand_from_oracle(
                        player,
                        "Synthetic three-player denied instant",
                        true,
                        "You gain 1 life.",
                    )
                    .with_mana_cost(ManaCost::Cost {
                        generic: 0,
                        shards: vec![ManaCostShard::Blue],
                    })
                    .id();
                let creature = scenario
                    .add_creature_to_hand_from_oracle(
                        player,
                        "Synthetic three-player legal creature",
                        1,
                        1,
                        "Flash",
                    )
                    .with_mana_cost(ManaCost::Cost {
                        generic: 0,
                        shards: vec![ManaCostShard::Blue],
                    })
                    .id();
                (player, instant, creature)
            })
            .collect();
        let target = scenario
            .add_spell_to_hand_from_oracle(
                P1,
                "Synthetic selected counter target",
                true,
                "You gain 1 life.",
            )
            .with_mana_cost(ManaCost::generic(0))
            .id();
        for player in [P0, P1, p2] {
            blue_pool(&mut scenario, player, 5);
        }
        let mut runner = scenario.build();
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::White,
            ObjectId(0),
            false,
            vec![],
        ));
        if name == "Ranger-Captain of Eos" {
            runner.activate(source, 0).resolve();
            assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
        } else if name == "Permission Denied" {
            runner.act(GameAction::PassPriority).unwrap();
            runner.cast(target).commit();
            runner.act(GameAction::PassPriority).unwrap();
            runner.act(GameAction::PassPriority).unwrap();
            runner.cast(source).target_object(target).resolve();
            assert_eq!(runner.state().objects[&target].zone, Zone::Graveyard);
        } else {
            runner.cast(source).resolve();
            assert_eq!(runner.state().objects[&source].zone, Zone::Exile);
        }
        settled(&runner);
        if name == "Calamity's Wake" {
            assert!(runner.cast(own).try_resolve().is_err());
        } else {
            runner.cast(own).resolve();
            settled(&runner);
        }
        for (player, denied, legal) in witnesses {
            for _ in 0..player.0 {
                runner.act(GameAction::PassPriority).unwrap();
            }
            assert_eq!(runner.state().waiting_for, WaitingFor::Priority { player });
            let before = runner.state().players[player.0 as usize].mana_pool.total();
            assert!(runner.cast(denied).try_resolve().is_err());
            assert_eq!(runner.state().objects[&denied].zone, Zone::Hand);
            assert_eq!(
                runner.state().players[player.0 as usize].mana_pool.total(),
                before
            );
            runner.cast(legal).resolve();
            settled(&runner);
            assert_eq!(runner.state().objects[&legal].zone, Zone::Battlefield);
            assert_eq!(
                runner.state().players[player.0 as usize].mana_pool.total(),
                before - 1
            );
        }
    }
}

/// CR 702.143a + CR 601.2i: an actually foretold Flux counts its own
/// Exile-origin cast, whereas its ordinary Hand cast did not count itself.
#[test]
fn impending_flux_actual_foretell_cast_counts_itself_after_turn_transition() {
    let db = printed_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let flux = scenario.add_real_card(P0, "Impending Flux", Zone::Hand, &db);
    for _ in 0..3 {
        scenario.add_basic_land(P0, engine::types::mana::ManaColor::Red);
    }
    scenario.with_library_top(P0, &["Own draw", "Reserve"]);
    scenario.with_library_top(P1, &["Other draw", "Reserve"]);
    blue_pool(&mut scenario, P0, 2);
    let mut runner = scenario.build();
    runner
        .act(GameAction::Foretell {
            object_id: flux,
            card_id: runner.state().objects[&flux].card_id,
        })
        .unwrap();
    assert_eq!(runner.state().objects[&flux].zone, Zone::Exile);
    assert!(runner.state().objects[&flux].face_down);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    let old_turn = runner.state().turn_number;
    runner.advance_to_combat();
    runner.declare_attackers(&[]).unwrap();
    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::PreCombatMain);
    runner.advance_to_combat();
    runner.declare_attackers(&[]).unwrap();
    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P0);
    assert!(runner.state().turn_number > old_turn);
    runner.cast(flux).resolve().assert_life_delta(P1, -2);
    settled(&runner);
    let records = &runner.state().spells_cast_this_turn_by_player[&P0];
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].from_zone, Zone::Exile);
    assert_eq!(runner.state().objects[&flux].zone, Zone::Graveyard);
    assert!(!runner.state().objects[&flux].face_down);
}

/// CR 118.9d + CR 601.2h: a life alternative replaces only mana;
/// mandatory sacrifice still has its own actual choice and payment.
#[test]
fn demon_two_grants_keep_usage_independent_and_pay_mandatory_sacrifice() {
    use engine::types::game_state::{CastPaymentMode, PayCostKind};
    let db = printed_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let demons: Vec<_> = (0..2)
        .map(|_| scenario.add_real_card(P0, "Demon of Fate's Design", Zone::Battlefield, &db))
        .collect();
    let fodder: Vec<_> = (0..2)
        .map(|_| {
            scenario
                .add_creature(P0, "Synthetic additional-cost sacrifice", 1, 1)
                .id()
        })
        .collect();
    let first = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic enchantment additional sacrifice",
            false,
            "As an additional cost to cast this spell, sacrifice a creature.\nDraw a card.",
        )
        .as_enchantment()
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    let second = scenario.add_real_card(P0, "Fae Offering", Zone::Hand, &db);
    let third = scenario.add_real_card(P0, "Fae Offering", Zone::Hand, &db);
    let drawn = scenario.add_card_to_library_top(P0, "Mandatory draw witness");
    blue_pool(&mut scenario, P0, 8);
    let mut runner = scenario.build();
    runner
        .act(GameAction::CastSpell {
            object_id: first,
            card_id: runner.state().objects[&first].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    let WaitingFor::OptionalCostChoice {
        player,
        cost: engine::types::ability::AdditionalCost::Choice(..),
        ..
    } = &runner.state().waiting_for
    else {
        panic!("first independent life grant")
    };
    assert_eq!(*player, P0);
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .unwrap();
    let WaitingFor::PayCost {
        player,
        kind: PayCostKind::Sacrifice,
        choices,
        count,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "mandatory sacrifice survives life alternative: {:?}",
            runner.state().waiting_for
        )
    };
    assert_eq!(*player, P0);
    assert_eq!(*count, 1);
    assert_eq!(
        choices
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        demons.iter().chain(&fodder).copied().collect()
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![fodder[1]],
        })
        .unwrap();
    runner.advance_until_stack_empty();
    settled(&runner);
    assert_eq!(runner.life(P0), 17);
    assert_eq!(runner.state().players[0].mana_pool.total(), 8);
    assert_eq!(runner.state().objects[&fodder[1]].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&fodder[0]].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&drawn].zone, Zone::Hand);
    runner
        .act(GameAction::CastSpell {
            object_id: second,
            card_id: runner.state().objects[&second].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    let WaitingFor::OptionalCostChoice {
        player,
        cost: engine::types::ability::AdditionalCost::Choice(..),
        ..
    } = &runner.state().waiting_for
    else {
        panic!("second source's unused life grant")
    };
    assert_eq!(*player, P0);
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .unwrap();
    runner.advance_until_stack_empty();
    settled(&runner);
    assert_eq!(runner.life(P0), 14);
    assert_eq!(runner.state().players[0].mana_pool.total(), 8);
    assert_eq!(runner.state().objects[&second].zone, Zone::Battlefield);
    assert!(runner.cast(third).try_resolve().is_err());
    assert_eq!(runner.state().objects[&third].zone, Zone::Hand);
    assert_eq!(runner.state().spells_cast_this_turn_by_player[&P0].len(), 2);
    settled(&runner);
}

/// CR 611.3b: leaving before announcement disables a live alternative grant.
#[test]
fn demon_source_departure_before_announcement_disables_life_alternative() {
    let db = printed_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let demon = scenario.add_real_card(P0, "Demon of Fate's Design", Zone::Battlefield, &db);
    let enchantment = scenario.add_real_card(P0, "Fae Offering", Zone::Hand, &db);
    let bounce = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic alternative-source bounce",
            true,
            "Return target creature to its owner's hand.",
        )
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    blue_pool(&mut scenario, P0, 8);
    let mut runner = scenario.build();
    runner.cast(bounce).target_object(demon).resolve();
    settled(&runner);
    assert_eq!(runner.state().objects[&demon].zone, Zone::Hand);
    assert!(runner.cast(enchantment).try_resolve().is_err());
    assert_eq!(runner.life(P0), 20);
    assert_eq!(runner.state().players[0].mana_pool.total(), 7);
    assert_eq!(runner.state().objects[&enchantment].zone, Zone::Hand);
    assert_eq!(runner.state().spells_cast_this_turn_by_player[&P0].len(), 1);
    settled(&runner);
}

/// CR 601.2i + CR 602.2a: a completed cast unlocks both printed
/// activations while still on the stack and after it is countered.
#[test]
fn activation_history_includes_stack_and_countered_cast_then_resets() {
    use engine::types::game_state::CastPaymentMode;
    let db = printed_db();
    for name in ["Seeker of Insight", "Tapestry of the Ages"] {
        for countered in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let source = scenario.add_real_card(P0, name, Zone::Battlefield, &db);
            let history = paid_history_spell(&mut scenario, CoreType::Instant);
            let counter = scenario
                .add_spell_to_hand_from_oracle(
                    P1,
                    "Synthetic activation history counter",
                    true,
                    "Counter target spell.",
                )
                .with_mana_cost(ManaCost::generic(0))
                .id();
            let keep = scenario.add_card_to_hand(P0, "Activation retained card");
            scenario.with_library_top(P0, &["Next own draw", "Guard draw", "Reserve"]);
            let activation_draw = scenario.add_card_to_library_top(P0, "Activation draw");
            scenario.with_library_top(P1, &["Other draw", "Reserve"]);
            for _ in 0..2 {
                scenario.add_basic_land(P0, engine::types::mana::ManaColor::Blue);
            }
            blue_pool(&mut scenario, P0, 5);
            let mut runner = scenario.build();
            runner.cast(history).commit();
            assert_eq!(runner.state().objects[&history].zone, Zone::Stack);
            if countered {
                runner.act(GameAction::PassPriority).unwrap();
                runner.cast(counter).target_object(history).resolve();
                settled(&runner);
                assert_eq!(runner.state().objects[&history].zone, Zone::Graveyard);
                assert_eq!(runner.life(P0), 20);
            }
            let before = runner.state().players[0].mana_pool.total();
            runner
                .act(GameAction::ActivateAbility {
                    source_id: source,
                    ability_index: 0,
                    payment_mode: CastPaymentMode::Auto,
                })
                .unwrap();
            runner.advance_until_stack_empty();
            if name == "Seeker of Insight" {
                let WaitingFor::DiscardChoice {
                    player,
                    cards,
                    count,
                    ..
                } = &runner.state().waiting_for
                else {
                    panic!("actual history-unlocked discard")
                };
                assert_eq!(*player, P0);
                assert_eq!(*count, 1);
                assert!(cards.contains(&keep));
                assert_eq!(cards.len(), 2);
                assert_eq!(
                    cards
                        .iter()
                        .copied()
                        .collect::<std::collections::BTreeSet<_>>(),
                    [keep, activation_draw].into_iter().collect()
                );
                runner
                    .act(GameAction::SelectCards { cards: vec![keep] })
                    .unwrap();
                runner.advance_until_stack_empty();
                assert_eq!(runner.state().objects[&keep].zone, Zone::Graveyard);
            }
            settled(&runner);
            assert_eq!(
                runner.state().players[0].mana_pool.total(),
                before - if name == "Tapestry of the Ages" { 2 } else { 0 }
            );
            assert!(runner.state().objects[&source].tapped);
            for _ in 0..2 {
                runner.advance_to_combat();
                runner.declare_attackers(&[]).unwrap();
                runner.advance_to_end_step();
                runner.advance_to_phase(Phase::PreCombatMain);
            }
            assert_eq!(runner.state().active_player, P0);
            assert!(!runner.state().objects[&source].tapped);
            assert!(!runner.state().objects[&source].summoning_sick);
            assert!(runner
                .state()
                .spells_cast_this_turn_by_player
                .values()
                .all(|records| records.is_empty()));
            assert!(runner
                .act(GameAction::ActivateAbility {
                    source_id: source,
                    ability_index: 0,
                    payment_mode: CastPaymentMode::Auto
                })
                .is_err());
            assert!(!runner.state().objects[&source].tapped);
            settled(&runner);
        }
    }
}

/// CR 701.57a: a no-hit discover finishes without inventing a cast offer;
/// a creature-only history never starts the producer at all.
#[test]
fn franklin_discover_creature_only_and_no_eligible_library_finish_lawfully() {
    let db = printed_db();
    for creature in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_real_card(P0, "Franklin Richards, Ascendant", Zone::Battlefield, &db);
        let history = paid_history_spell(
            &mut scenario,
            if creature {
                CoreType::Creature
            } else {
                CoreType::Instant
            },
        );
        let witness = scenario.add_real_card(
            P0,
            if creature {
                "Demon of Fate's Design"
            } else {
                "Sword Coast Serpent"
            },
            Zone::Library,
            &db,
        );
        let land = scenario
            .add_land_to_library_top(P0, "Discover no-hit land")
            .id();
        blue_pool(&mut scenario, P0, 1);
        let mut runner = scenario.build();
        runner.cast(history).resolve();
        runner.advance_to_phase(Phase::BeginCombat);
        runner.advance_until_stack_empty();
        settled(&runner);
        for id in [witness, land] {
            assert_eq!(runner.state().objects[&id].zone, Zone::Library);
        }
        assert_eq!(runner.state().spells_cast_this_turn_by_player[&P0].len(), 1);
        assert_eq!(runner.state().players[0].library.len(), 2);
    }
}

/// CR 605.3a: the second mana ability cannot start without its printed {1}.
#[test]
fn herbie_unfunded_mana_activation_keeps_source_untapped() {
    let db = printed_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let herbie = scenario.add_real_card(P0, "H.E.R.B.I.E., Lovable Robot", Zone::Battlefield, &db);
    let mut runner = scenario.build();
    assert!(runner
        .act(GameAction::ActivateAbility {
            source_id: herbie,
            ability_index: 1,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto
        })
        .is_err());
    assert!(!runner.state().objects[&herbie].tapped);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    settled(&runner);
}

/// CR 611.2a: two separately resolved bans retain their own controller
/// authority after both sacrificed sources leave the battlefield.
#[test]
fn two_ranger_bans_do_not_borrow_the_other_source_controller() {
    let db = printed_db();
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let sources: Vec<_> = [P0, P1]
        .into_iter()
        .map(|player| {
            scenario.add_real_card(player, "Ranger-Captain of Eos", Zone::Battlefield, &db)
        })
        .collect();
    let own: Vec<_> = (0..2)
        .map(|_| paid_history_spell(&mut scenario, CoreType::Instant))
        .collect();
    blue_pool(&mut scenario, P0, 3);
    let mut runner = scenario.build();
    runner.activate(sources[0], 0).resolve();
    settled(&runner);
    runner.cast(own[0]).resolve();
    settled(&runner);
    assert_eq!(runner.state().players[0].mana_pool.total(), 2);
    runner.act(GameAction::PassPriority).unwrap();
    runner.activate(sources[1], 0).resolve();
    settled(&runner);
    for source in sources {
        assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    }
    assert!(runner.cast(own[1]).try_resolve().is_err());
    assert_eq!(runner.state().objects[&own[1]].zone, Zone::Hand);
    assert_eq!(runner.state().players[0].mana_pool.total(), 2);
    assert_eq!(runner.state().spells_cast_this_turn_by_player[&P0].len(), 1);
    settled(&runner);
}

/// CR 601.2f + CR 118.9d: sacrificing the selected granting source as a
/// mandatory cost does not replace the already determined life alternative.
#[test]
fn demon_life_cost_lock_survives_its_own_mandatory_sacrifice() {
    use engine::types::game_state::{CastPaymentMode, PayCostKind};
    let db = printed_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let demon = scenario.add_real_card(P0, "Demon of Fate's Design", Zone::Battlefield, &db);
    let enchantment = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic source-sacrificing enchantment",
            false,
            "As an additional cost to cast this spell, sacrifice a creature.\nDraw a card.",
        )
        .as_enchantment()
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    let draw = scenario.add_card_to_library_top(P0, "Locked alternative draw");
    blue_pool(&mut scenario, P0, 5);
    let mut runner = scenario.build();
    runner
        .act(GameAction::CastSpell {
            object_id: enchantment,
            card_id: runner.state().objects[&enchantment].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    let WaitingFor::OptionalCostChoice {
        player,
        cost: engine::types::ability::AdditionalCost::Choice(..),
        ..
    } = &runner.state().waiting_for
    else {
        panic!("actual selected source life alternative")
    };
    assert_eq!(*player, P0);
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .unwrap();
    let WaitingFor::PayCost {
        player,
        kind: PayCostKind::Sacrifice,
        choices,
        count,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("mandatory source sacrifice")
    };
    assert_eq!(*player, P0);
    assert_eq!(*count, 1);
    assert_eq!(choices, &vec![demon]);
    runner
        .act(GameAction::SelectCards { cards: vec![demon] })
        .unwrap();
    runner.advance_until_stack_empty();
    settled(&runner);
    assert_eq!(runner.state().objects[&demon].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&enchantment].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&draw].zone, Zone::Hand);
    assert_eq!(runner.life(P0), 17);
    assert_eq!(runner.state().players[0].mana_pool.total(), 5);
    assert_eq!(runner.state().spells_cast_this_turn_by_player[&P0].len(), 1);
}

/// CR 118.9: Conspiracy's genuine unfiltered spell noun grants the
/// printed alternative to both a creature and a noncreature spell.
#[test]
fn conspiracy_unraveler_bare_spell_grant_collects_actual_graveyard_evidence() {
    use engine::types::ability::{AbilityCost, AdditionalCost};
    use engine::types::game_state::CastPaymentMode;
    let db = printed_db();
    for name in ["Lyse Hext", "Tapestry of the Ages"] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_real_card(P0, "Conspiracy Unraveler", Zone::Battlefield, &db);
        let spell = scenario.add_real_card(P0, name, Zone::Hand, &db);
        let evidence: Vec<_> = ["Sword Coast Serpent", "Traxos, Academy Guardian"]
            .into_iter()
            .map(|name| scenario.add_real_card(P0, name, Zone::Graveyard, &db))
            .collect();
        let decoy = scenario.add_real_card(P1, "Sword Coast Serpent", Zone::Graveyard, &db);
        let mut runner = scenario.build();
        runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id: runner.state().objects[&spell].card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        let WaitingFor::OptionalCostChoice {
            player,
            cost: AdditionalCost::Choice(cost, _),
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "real collect-evidence alternative offer: {:?}",
                runner.state().waiting_for
            )
        };
        assert_eq!(*player, P0);
        assert_eq!(*cost, AbilityCost::CollectEvidence { amount: 10 });
        runner
            .act(GameAction::DecideOptionalCost { pay: true })
            .unwrap();
        let WaitingFor::CollectEvidenceChoice {
            player,
            cards,
            minimum_mana_value,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("actual graveyard evidence selection")
        };
        assert_eq!(*player, P0);
        assert_eq!(*minimum_mana_value, 10);
        assert_eq!(
            cards
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            evidence.iter().copied().collect()
        );
        assert!(runner
            .act(GameAction::SelectCards {
                cards: vec![evidence[0]]
            })
            .is_err());
        runner
            .act(GameAction::SelectCards {
                cards: vec![evidence[1], evidence[0]],
            })
            .unwrap();
        runner.advance_until_stack_empty();
        settled(&runner);
        assert_eq!(runner.state().objects[&spell].zone, Zone::Battlefield);
        for id in evidence {
            assert_eq!(runner.state().objects[&id].zone, Zone::Exile);
        }
        assert_eq!(runner.state().objects[&decoy].zone, Zone::Graveyard);
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_eq!(runner.state().spells_cast_this_turn_by_player[&P0].len(), 1);
    }
}

/// CR 118.9: a complete noncreature subject gates the general mana
/// alternative constructor without offering that alternative to creatures.
#[test]
fn complete_noncreature_mana_alternative_is_offered_and_paid_only_for_its_subject() {
    use engine::types::ability::{AbilityCost, AdditionalCost};
    use engine::types::game_state::CastPaymentMode;
    for creature in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_enchantment_from_oracle(
            P0,
            "Synthetic filtered mana alternative",
            "You may pay {0} rather than pay the mana cost for noncreature spells you cast.",
        );
        let spell = if creature {
            scenario
                .add_creature_to_hand_from_oracle(
                    P0,
                    "Synthetic alternative creature twin",
                    1,
                    1,
                    "",
                )
                .with_mana_cost(ManaCost::Cost {
                    generic: 1,
                    shards: vec![ManaCostShard::Blue],
                })
                .id()
        } else {
            scenario
                .add_spell_to_hand_from_oracle(
                    P0,
                    "Synthetic alternative instant twin",
                    true,
                    "You gain 1 life.",
                )
                .with_mana_cost(ManaCost::Cost {
                    generic: 1,
                    shards: vec![ManaCostShard::Blue],
                })
                .id()
        };
        blue_pool(&mut scenario, P0, 3);
        let mut runner = scenario.build();
        runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id: runner.state().objects[&spell].card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        if !creature {
            let WaitingFor::OptionalCostChoice {
                player,
                cost: AdditionalCost::Choice(cost, _),
                ..
            } = &runner.state().waiting_for
            else {
                panic!("actual filtered mana alternative")
            };
            assert_eq!(*player, P0);
            assert_eq!(
                *cost,
                AbilityCost::Mana {
                    cost: ManaCost::generic(0)
                }
            );
            runner
                .act(GameAction::DecideOptionalCost { pay: true })
                .unwrap();
        } else {
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { player: P0 }
            ));
            assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
        }
        runner.advance_until_stack_empty();
        settled(&runner);
        assert_eq!(
            runner.state().players[0].mana_pool.total(),
            if creature { 1 } else { 3 }
        );
        assert_eq!(runner.state().spells_cast_this_turn_by_player[&P0].len(), 1);
    }
}

/// CR 601.2a + CR 118.9: the same complete noun reaches ordinary zone,
/// free-cast and mana-type permission consumers without admitting creatures.
#[test]
fn complete_noncreature_permission_callers_reach_actual_casting_consumers() {
    for permission in ["graveyard", "combined graveyard", "library", "hand", "mana"] {
        for creature in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let text = match permission {
            "graveyard" => "You may cast noncreature spells from your graveyard.",
            "combined graveyard" => "You may play lands and cast noncreature spells from your graveyard.",
            "library" => "You may cast noncreature spells from the top of your library.",
            "hand" => "You may cast noncreature spells from your hand without paying their mana costs.",
            "mana" => "You may spend mana of any type to cast noncreature spells.",
            _ => unreachable!(),
        };
            scenario.add_enchantment_from_oracle(P0, "Synthetic complete-subject permission", text);
            let origin = match permission {
                "graveyard" | "combined graveyard" => Zone::Graveyard,
                "library" => Zone::Library,
                _ => Zone::Hand,
            };
            let spell = {
                let mut builder = match permission {
                    "graveyard" | "combined graveyard" => {
                        scenario.add_spell_to_graveyard(P0, "Synthetic permission population", true)
                    }
                    "library" => scenario.add_spell_to_library_top(
                        P0,
                        "Synthetic permission population",
                        true,
                    ),
                    _ => scenario.add_spell_to_hand(P0, "Synthetic permission population", true),
                };
                if creature {
                    builder.as_creature();
                } else {
                    builder.from_oracle_text("You gain 1 life.");
                }
                builder
                    .with_mana_cost(ManaCost::Cost {
                        generic: 0,
                        shards: vec![ManaCostShard::Blue],
                    })
                    .id()
            };
            if permission != "hand" {
                scenario.with_mana_pool(
                    P0,
                    vec![ManaUnit::new(
                        if permission == "mana" {
                            ManaType::White
                        } else {
                            ManaType::Blue
                        },
                        ObjectId(0),
                        false,
                        vec![],
                    )],
                );
            }
            let mut runner = scenario.build();
            if creature {
                assert!(runner.cast(spell).try_resolve().is_err());
                assert_eq!(runner.state().objects[&spell].zone, origin);
                assert_eq!(
                    runner.state().players[0].mana_pool.total(),
                    usize::from(permission != "hand")
                );
                assert!(runner
                    .state()
                    .spells_cast_this_turn_by_player
                    .get(&P0)
                    .is_none_or(|records| records.is_empty()));
            } else {
                if permission == "hand" {
                    runner.cast(spell).free_cast().resolve();
                } else {
                    runner.cast(spell).resolve();
                }
                assert_eq!(runner.life(P0), 21);
                assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
                assert_eq!(runner.state().players[0].mana_pool.total(), 0);
                let records = &runner.state().spells_cast_this_turn_by_player[&P0];
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].from_zone, origin);
            }
            settled(&runner);
        }
    }
}

/// CR 603.3a + CR 603.4: changing the source's controller after the
/// phase event does not substitute that player's creature-only cast history.
#[test]
fn queued_combat_trigger_keeps_controller_after_real_flash_creature_theft() {
    let db = printed_db();
    for name in [
        "Alicia Masters, Skilled Sculptor",
        "Council of Reeds",
        "Human Torch",
        "Invisible Woman",
        "The Thing",
        "Lockjaw, Slobbering Teleporter",
        "Mister Fantastic",
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let host = scenario.add_real_card(P0, name, Zone::Battlefield, &db);
        let history = paid_history_spell(&mut scenario, CoreType::Instant);
        let theft = scenario
            .add_creature_to_hand_from_oracle(
                P1,
                "Synthetic flash creature theft",
                1,
                1,
                "Flash\nWhen this creature enters, gain control of target creature.",
            )
            .with_mana_cost(ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::Blue],
            })
            .id();
        let draw = scenario.add_card_to_library_top(P0, "Captured controller draw");
        let foreign_draw = scenario.add_card_to_library_top(P1, "Unrelated controller draw");
        blue_pool(&mut scenario, P0, 1);
        blue_pool(&mut scenario, P1, 1);
        // CR 106.4: every theft-loop member funds its response after the phase boundary.
        let response_land = scenario.add_basic_land(P1, engine::types::mana::ManaColor::Blue);
        let mut runner = scenario.build();
        runner.cast(history).resolve();
        settled(&runner);
        runner.advance_to_phase(Phase::BeginCombat);
        assert_eq!(runner.state().stack.len(), 1);
        assert_eq!(runner.state().players[1].mana_pool.total(), 0);
        assert!(!runner.state().objects[&response_land].tapped);
        runner.act(GameAction::PassPriority).unwrap();
        runner.cast(theft).target_object(host).resolve();
        assert!(runner.state().objects[&response_land].tapped);
        settled(&runner);
        assert_eq!(runner.state().objects[&host].owner, P0);
        assert_eq!(runner.state().objects[&host].controller, P1);
        assert_eq!(
            runner.state().spells_cast_this_turn_by_player[&P1][0].core_types,
            vec![CoreType::Creature]
        );
        assert_eq!(runner.state().objects[&foreign_draw].zone, Zone::Library);
        match name {
            "Alicia Masters, Skilled Sculptor" | "Invisible Woman" => {
                let kind = if name == "Invisible Woman" {
                    "Wall"
                } else {
                    "Treasure"
                };
                assert_eq!(
                    runner
                        .state()
                        .battlefield
                        .iter()
                        .filter(|id| {
                            let obj = &runner.state().objects[id];
                            obj.is_token
                                && obj.controller == P0
                                && obj.card_types.subtypes.iter().any(|s| s == kind)
                        })
                        .count(),
                    1
                );
            }
            "Council of Reeds" => {
                let copies: Vec<_> = runner
                    .state()
                    .battlefield
                    .iter()
                    .filter(|id| {
                        let obj = &runner.state().objects[id];
                        obj.is_token && obj.name == name && obj.controller == P0
                    })
                    .collect();
                assert_eq!(copies.len(), 1);
                assert_ne!(**copies.first().unwrap(), host);
            }
            "Human Torch" => {
                for keyword in [Keyword::Flying, Keyword::DoubleStrike, Keyword::Haste] {
                    assert!(runner.state().objects[&host].keywords.contains(&keyword));
                }
            }
            "The Thing" | "Lockjaw, Slobbering Teleporter" => assert_eq!(
                runner.state().objects[&host]
                    .counters
                    .get(&CounterType::P1P1)
                    .copied(),
                Some(if name == "The Thing" { 4 } else { 1 })
            ),
            "Mister Fantastic" => assert_eq!(runner.state().objects[&draw].zone, Zone::Hand),
            _ => unreachable!(),
        }
    }
}

/// CR 601.2i: the actual other-than-first producer compares the same
/// qualifying population and caster even with an intervening creature cast.
#[test]
fn druid_and_noncreature_other_than_first_use_real_public_priority_and_paid_casts() {
    let db = printed_db();
    for noun in ["instant", "noncreature"] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = if noun == "instant" {
            scenario.add_real_card(P0, "Ichneumon Druid", Zone::Battlefield, &db)
        } else {
            scenario.add_creature_from_oracle(P0, "Synthetic noncreature Druid analogue", 1, 1, "Whenever an opponent casts a noncreature spell other than the first noncreature spell that player casts each turn, this creature deals 4 damage to that player.").id()
        };
        let own = paid_history_spell(&mut scenario, CoreType::Instant);
        let instants: Vec<_> = (0..2)
            .map(|_| {
                scenario
                    .add_spell_to_hand_from_oracle(
                        P1,
                        "Synthetic paid Druid instant",
                        true,
                        "Draw a card.",
                    )
                    .with_mana_cost(ManaCost::Cost {
                        generic: 0,
                        shards: vec![ManaCostShard::Blue],
                    })
                    .id()
            })
            .collect();
        let creature = scenario
            .add_creature_to_hand_from_oracle(
                P1,
                "Synthetic intervening flash creature",
                1,
                1,
                "Flash",
            )
            .with_mana_cost(ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::Blue],
            })
            .id();
        scenario.with_library_top(
            P1,
            &[
                "First actual Druid draw",
                "Second actual Druid draw",
                "Reserve",
            ],
        );
        blue_pool(&mut scenario, P0, 1);
        blue_pool(&mut scenario, P1, 3);
        let mut runner = scenario.build();
        runner.cast(own).resolve();
        settled(&runner);
        assert_eq!(runner.life(P0), 21);
        assert_eq!(runner.life(P1), 20);
        for spell in [instants[0], creature, instants[1]] {
            runner.act(GameAction::PassPriority).unwrap();
            runner.cast(spell).resolve();
            settled(&runner);
            assert_eq!(runner.life(P1), if spell == instants[1] { 16 } else { 20 });
        }
        assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
        assert_eq!(runner.state().spells_cast_this_turn_by_player[&P1].len(), 3);
        assert_eq!(runner.state().players[1].mana_pool.total(), 0);
        assert_eq!(runner.state().spells_cast_this_turn_by_player[&P0].len(), 1);
    }
}

/// CR 109.5 + CR 508.1c + CR 509.1b: the ready attacker uses its new
/// controller's history after an actual control-and-haste creature ETB.
#[test]
fn combat_restrictions_use_current_controller_rather_than_owner_history() {
    use engine::game::combat::AttackTarget;
    let db = printed_db();
    for name in ["Silburlind Snapper", "Sword Coast Serpent"] {
        for matching in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let attacker = scenario.add_real_card(P1, name, Zone::Battlefield, &db);
            let blocker = scenario
                .add_creature(P1, "Ready original-controller blocker", 0, 8)
                .id();
            let guard = scenario
                .add_creature(P0, "Independent legal attacker", 1, 1)
                .id();
            let theft = scenario.add_creature_to_hand_from_oracle(P0, "Synthetic combat theft creature", 1, 1, "When this creature enters, gain control of target creature. It gains haste until end of turn.").with_mana_cost(ManaCost::Cost { generic: 0, shards: vec![ManaCostShard::Blue] }).id();
            let own = paid_history_spell(&mut scenario, CoreType::Instant);
            let foreign = scenario
                .add_spell_to_hand_from_oracle(
                    P1,
                    "Synthetic original-controller history",
                    true,
                    "You gain 1 life.",
                )
                .with_mana_cost(ManaCost::Cost {
                    generic: 0,
                    shards: vec![ManaCostShard::Blue],
                })
                .id();
            blue_pool(&mut scenario, P0, 3);
            blue_pool(&mut scenario, P1, 1);
            let mut runner = scenario.build();
            runner.act(GameAction::PassPriority).unwrap();
            runner.cast(foreign).resolve();
            runner.cast(theft).target_object(attacker).resolve();
            settled(&runner);
            assert_eq!(runner.state().objects[&attacker].owner, P1);
            assert_eq!(runner.state().objects[&attacker].controller, P0);
            assert!(runner.state().objects[&attacker]
                .keywords
                .contains(&Keyword::Haste));
            if matching {
                runner.cast(own).resolve();
                settled(&runner);
            }
            runner.advance_to_combat();
            if name == "Silburlind Snapper" && !matching {
                assert!(runner
                    .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
                    .is_err());
                runner
                    .declare_attackers(&[(guard, AttackTarget::Player(P1))])
                    .unwrap();
                runner.advance_to_phase(Phase::DeclareBlockers);
                runner.declare_blockers(&[]).unwrap();
                runner.combat_damage().assert_life_delta(P1, -1);
            } else {
                runner
                    .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
                    .unwrap();
                runner.advance_to_phase(Phase::DeclareBlockers);
                if name == "Sword Coast Serpent" {
                    let block = runner.declare_blockers(&[(blocker, attacker)]);
                    if matching {
                        assert!(block.is_err());
                        runner.declare_blockers(&[]).unwrap();
                    } else {
                        block.unwrap();
                    }
                } else {
                    runner.declare_blockers(&[]).unwrap();
                }
                runner.combat_damage().assert_life_delta(
                    P1,
                    if name == "Sword Coast Serpent" && !matching {
                        0
                    } else {
                        -6
                    },
                );
            }
            settled(&runner);
        }
    }
}

/// CR 118.9d: discovering a spell without its mana cost still requires
/// its mandatory additional discard before it is committed.
#[test]
fn franklin_discovered_spell_pays_its_actual_additional_cost() {
    use engine::types::actions::CastChoice;
    use engine::types::game_state::{CastOfferKind, PayCostKind};
    let db = printed_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Franklin Richards, Ascendant", Zone::Battlefield, &db);
    let history = paid_history_spell(&mut scenario, CoreType::Instant);
    let hit = scenario
        .add_spell_to_library_top(P0, "Synthetic discover additional-cost instant", true)
        .from_oracle_text(
            "As an additional cost to cast this spell, discard a card.\nYou gain 1 life.",
        )
        .with_mana_cost(ManaCost::generic(6))
        .id();
    let keep = scenario.add_card_to_hand(P0, "Keep discover hand card");
    let discard = scenario.add_card_to_hand(P0, "Discard nonfirst discover hand card");
    blue_pool(&mut scenario, P0, 1);
    let mut runner = scenario.build();
    runner.cast(history).resolve();
    runner.advance_to_phase(Phase::BeginCombat);
    runner.advance_until_stack_empty();
    let WaitingFor::CastOffer {
        player,
        kind:
            CastOfferKind::Discover {
                hit_card,
                discover_value,
                ..
            },
    } = &runner.state().waiting_for
    else {
        panic!("real full-card discover offer")
    };
    assert_eq!(*player, P0);
    assert_eq!(*hit_card, hit);
    assert_eq!(*discover_value, 6);
    runner
        .act(GameAction::DiscoverChoice {
            choice: CastChoice::Cast,
        })
        .unwrap();
    let WaitingFor::PayCost {
        player,
        kind: PayCostKind::Discard,
        choices,
        count,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "discovered spell mandatory discard: {:?}",
            runner.state().waiting_for
        )
    };
    assert_eq!(*player, P0);
    assert_eq!(*count, 1);
    assert_eq!(
        choices
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        [keep, discard].into_iter().collect()
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![discard],
        })
        .unwrap();
    runner.advance_until_stack_empty();
    settled(&runner);
    assert_eq!(runner.state().objects[&discard].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&keep].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&hit].zone, Zone::Graveyard);
    assert_eq!(runner.life(P0), 22);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert_eq!(runner.state().spells_cast_this_turn_by_player[&P0].len(), 2);
    assert_eq!(
        runner.state().spells_cast_this_turn_by_player[&P0][1].from_zone,
        Zone::Exile
    );
}

/// CR 601.2a: a typed exile permission consumes the cohort exiled by
/// its own source, with an independently filtered sibling source present.
#[test]
fn complete_exile_spell_subjects_keep_the_actual_source_pool() {
    for creature in [false, true] {
        for pool_type in [CoreType::Creature, CoreType::Instant] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let noncreature_host = scenario.add_enchantment_from_oracle(P0, "Synthetic noncreature exile host", "{T}: Exile target card from your graveyard.\nYou may cast noncreature spells from among cards you own exiled with this enchantment.").id();
            let creature_host = scenario.add_enchantment_from_oracle(P0, "Synthetic creature exile host", "{T}: Exile target card from your graveyard.\nYou may cast creature spells from among cards you own exiled with this enchantment.").id();
            let spell = if creature {
                scenario
                    .add_creature_to_graveyard(P0, "Synthetic same-named exile subject", 1, 1)
                    .with_mana_cost(ManaCost::Cost {
                        generic: 0,
                        shards: vec![ManaCostShard::Blue],
                    })
                    .id()
            } else {
                scenario
                    .add_spell_to_graveyard(P0, "Synthetic same-named exile subject", true)
                    .from_oracle_text("You gain 1 life.")
                    .with_mana_cost(ManaCost::Cost {
                        generic: 0,
                        shards: vec![ManaCostShard::Blue],
                    })
                    .id()
            };
            blue_pool(&mut scenario, P0, 1);
            let mut runner = scenario.build();
            let chosen_host = if pool_type == CoreType::Creature {
                creature_host
            } else {
                noncreature_host
            };
            runner
                .activate(chosen_host, 0)
                .target_object(spell)
                .resolve();
            settled(&runner);
            assert_eq!(runner.state().objects[&spell].zone, Zone::Exile);
            assert!(runner.state().objects[&chosen_host].tapped);
            let allowed = creature == (pool_type == CoreType::Creature);
            let result = runner.cast(spell).try_resolve();
            if allowed {
                result.unwrap();
                assert_eq!(runner.state().players[0].mana_pool.total(), 0);
                assert_eq!(runner.state().spells_cast_this_turn_by_player[&P0].len(), 1);
                assert_eq!(
                    runner.state().spells_cast_this_turn_by_player[&P0][0].from_zone,
                    Zone::Exile
                );
                if !creature {
                    assert_eq!(runner.life(P0), 21);
                }
            } else {
                assert!(result.is_err());
                assert_eq!(runner.state().objects[&spell].zone, Zone::Exile);
                assert_eq!(runner.state().players[0].mana_pool.total(), 1);
                assert!(runner
                    .state()
                    .spells_cast_this_turn_by_player
                    .get(&P0)
                    .is_none_or(|records| records.is_empty()));
            }
            settled(&runner);
        }
    }
}
