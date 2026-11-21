use engine::game::combat::AttackTarget;
use engine::game::game_object::BackFaceData;
use engine::game::scenario::GameScenario;
use engine::types::card::PrintedLoyalty;
use engine::types::card_type::{CardType, CoreType, Supertype};
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::GameAction;

const ATTACKER: PlayerId = PlayerId(0);
const DEFENDER: PlayerId = PlayerId(1);
const ORACLE: &str = "At end of combat, if Kytheon and at least two other creatures attacked this combat, exile Kytheon, then return him to the battlefield transformed under his owner's control.\n{2}{W}: Kytheon gains indestructible until end of turn.";
const ADELINE_ORACLE: &str = "Vigilance\n\
    Adeline's power is equal to the number of creatures you control.\n\
    Whenever you attack, for each opponent, create a 1/1 white Human creature token that's tapped and attacking that player or a planeswalker they control.";

#[test]
fn kytheon_end_of_combat_uses_declared_attackers_and_returns_to_owner() {
    for (
        case,
        source_attacks,
        others_present,
        others_attacking,
        adeline_attacks,
        block,
        transforms,
    ) in [
        ("Kytheon alone", true, 0, 0, false, false, false),
        ("one other creature", true, 1, 1, false, false, false),
        (
            "Adeline creates an attacking Human",
            true,
            1,
            1,
            true,
            false,
            false,
        ),
        (
            "one of two other creatures stays home",
            true,
            2,
            1,
            false,
            false,
            false,
        ),
        (
            "two creatures attack without Kytheon",
            false,
            2,
            2,
            false,
            false,
            false,
        ),
        (
            "Kytheon and two others attack",
            true,
            2,
            2,
            false,
            false,
            true,
        ),
        (
            "a co-attacker dies before end of combat",
            true,
            2,
            2,
            false,
            true,
            true,
        ),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let kytheon = scenario
            .add_creature_from_oracle(DEFENDER, "Kytheon, Hero of Akros", 2, 1, ORACLE)
            .controlled_by(ATTACKER)
            .id();
        let first = if adeline_attacks {
            Some(
                scenario
                    .add_creature_from_oracle(
                        ATTACKER,
                        "Adeline, Resplendent Cathar",
                        0,
                        4,
                        ADELINE_ORACLE,
                    )
                    .id(),
            )
        } else {
            (others_present >= 1).then(|| {
                scenario
                    .add_creature(ATTACKER, "First co-attacker", 1, 1)
                    .id()
            })
        };
        let second = (others_present == 2).then(|| {
            scenario
                .add_creature(ATTACKER, "Second co-attacker", 1, 1)
                .id()
        });
        let blocker = scenario.add_creature(DEFENDER, "Blocker", 2, 2).id();
        let mut runner = scenario.build();
        runner
            .state_mut()
            .objects
            .get_mut(&kytheon)
            .unwrap()
            .back_face = Some(BackFaceData {
            is_swap_snapshot: false,
            trigger_printed_origins: Vec::new(),
            name: "Gideon, Battle-Forged".into(),
            power: None,
            toughness: None,
            loyalty: Some(3),
            printed_loyalty: Some(PrintedLoyalty::Fixed(3)),
            defense: None,
            card_types: CardType {
                supertypes: vec![Supertype::Legendary],
                core_types: vec![CoreType::Planeswalker],
                subtypes: vec!["Gideon".into()],
            },
            mana_cost: ManaCost::default(),
            keywords: vec![],
            abilities: vec![],
            trigger_definitions: Default::default(),
            replacement_definitions: Default::default(),
            static_definitions: Default::default(),
            color: vec![ManaColor::White],
            printed_ref: None,
            modal: None,
            additional_cost: None,
            strive_cost: None,
            casting_restrictions: vec![],
            casting_options: vec![],
            layout_kind: None,
            parse_warnings: vec![],
        });

        let front_incarnation = runner.state().objects[&kytheon].incarnation;
        runner.advance_to_combat();
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::DeclareAttackers {
                    player: ATTACKER,
                    ..
                }
            ),
            "{case}"
        );
        let attacks = source_attacks
            .then_some(kytheon)
            .into_iter()
            .chain([first, second].into_iter().flatten().take(others_attacking))
            .map(|id| (id, AttackTarget::Player(DEFENDER)))
            .collect::<Vec<_>>();
        runner.declare_attackers(&attacks).unwrap();
        if adeline_attacks {
            assert_eq!(runner.state().stack.len(), 1, "{case}");
            runner.resolve_top();
            assert!(runner.state().stack.is_empty(), "{case}");
            // CR 508.4: The Human enters attacking and was never declared.
            let state = runner.state();
            let attackers = &state.combat.as_ref().unwrap().attackers;
            assert_eq!(attackers.len(), 3, "{case}");
            assert!(
                attackers.iter().any(|attacker| {
                    let object = &state.objects[&attacker.object_id];
                    object.zone == Zone::Battlefield
                        && object.controller == ATTACKER
                        && object.is_token
                        && object.tapped
                        && object.card_types.subtypes.iter().any(|s| s == "Human")
                }),
                "{case}"
            );
        }
        for _ in 0..2 {
            if runner.state().phase == Phase::DeclareBlockers {
                break;
            }
            runner.act(GameAction::PassPriority).unwrap();
        }
        assert_eq!(runner.state().phase, Phase::DeclareBlockers, "{case}");
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::DeclareBlockers {
                    player: DEFENDER,
                    ..
                }
            ),
            "{case}"
        );
        let blocks = if block {
            vec![(blocker, first.unwrap())]
        } else {
            Vec::new()
        };
        runner.declare_blockers(&blocks).unwrap();

        runner.combat_damage();
        assert_eq!(runner.state().phase, Phase::EndCombat, "{case}");
        if block {
            // CR 506.4: The dead co-attacker is already out of combat.
            let casualty = first.unwrap();
            assert_eq!(runner.state().objects[&casualty].zone, Zone::Graveyard);
            assert!(runner
                .state()
                .combat
                .as_ref()
                .unwrap()
                .attackers
                .iter()
                .all(|attacker| attacker.object_id != casualty));
        }
        assert_eq!(
            runner.state().stack.len(),
            usize::from(transforms),
            "{case}"
        );

        // CR 511.3: Each row runs past the resolution window and out of combat.
        for _ in 0..4 {
            if runner.state().phase == Phase::PostCombatMain {
                break;
            }
            runner.act(GameAction::PassPriority).unwrap();
        }
        assert_eq!(runner.state().phase, Phase::PostCombatMain, "{case}");
        assert!(runner.state().stack.is_empty(), "{case}");
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { player: ATTACKER }
            ),
            "{case}"
        );

        let source = &runner.state().objects[&kytheon];
        assert_eq!(source.zone, Zone::Battlefield, "{case}");
        assert_eq!(source.transformed, transforms, "{case}");
        if transforms {
            // CR 712.14a + CR 306.5b: Gideon enters back-face-up with printed loyalty.
            assert_eq!(source.name, "Gideon, Battle-Forged", "{case}");
            assert_eq!(
                source.counters.get(&CounterType::Loyalty),
                Some(&3),
                "{case}"
            );
            assert_eq!(source.controller, DEFENDER, "{case}");
        } else {
            assert_eq!(source.name, "Kytheon, Hero of Akros", "{case}");
            assert_eq!(source.controller, ATTACKER, "{case}");
            assert_eq!(source.incarnation, front_incarnation, "{case}");
        }
    }
}
