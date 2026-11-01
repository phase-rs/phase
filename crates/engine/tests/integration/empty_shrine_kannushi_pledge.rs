//! Live protection sets: Empty-Shrine Kannushi and Pledge of Loyalty.
//! Regenerate integration_cards.json.gz after oracle-gen with gen-test-fixture.py;
//! the public-loader rows deliberately require fresh exported real cards.

use std::io::Read;

use engine::database::card_db::CardDatabase;
use engine::game::coverage::card_face_gaps;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::targeting::find_legal_targets;
use engine::types::ability::{
    ContinuousModification, ProtectionDoesNotRemove, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, ProtectionColorSet, ProtectionTarget};
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const KANNUSHI: &str = "This creature has protection from each color among permanents you control.";
const PLEDGE: &str = "Enchant creature\nEnchanted creature has protection from each color among permanents you control. This effect doesn't remove this Aura.";

fn db() -> &'static CardDatabase {
    shared_card_db().expect("fresh integration card fixture must load")
}

fn scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

fn prepare(runner: &mut GameRunner, player: PlayerId) {
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    let state = runner.state_mut();
    state.active_player = player;
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
    for mana in [
        ManaType::White,
        ManaType::Blue,
        ManaType::Black,
        ManaType::Red,
        ManaType::Green,
        ManaType::Colorless,
    ] {
        for _ in 0..10 {
            state
                .add_mana_to_pool(player, ManaUnit::new(mana, ObjectId(0), false, vec![]))
                .unwrap();
        }
    }
}

fn complete(runner: &GameRunner) {
    assert!(runner.state().stack.is_empty(), "stack must settle");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        runner.state().waiting_for
    );
}

fn colors(runner: &GameRunner, host: ObjectId) -> Vec<ManaColor> {
    let mut result: Vec<_> = runner.state().objects[&host]
        .keywords
        .iter()
        .filter_map(|keyword| match keyword {
            Keyword::Protection(ProtectionTarget::Color(color)) => Some(*color),
            _ => None,
        })
        .collect();
    result.sort();
    result
}

fn cast(runner: &mut GameRunner, player: PlayerId, spell: ObjectId) {
    prepare(runner, player);
    let outcome = runner.cast(spell).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    complete(runner);
}

fn cast_on(runner: &mut GameRunner, player: PlayerId, spell: ObjectId, target: ObjectId) {
    prepare(runner, player);
    let outcome = runner.cast(spell).target_object(target).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    complete(runner);
}

fn attached(runner: &GameRunner, aura: ObjectId, host: ObjectId) {
    assert_eq!(runner.state().objects[&aura].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&aura].attached_to, Some(host.into()));
    assert!(runner.state().objects[&host].attachments.contains(&aura));
}

fn assert_grant(database: &CardDatabase, name: &str, oracle: &str, aura: bool) {
    let face = database
        .get_face_by_name(name)
        .expect("selected real face must exist");
    assert_eq!(face.oracle_text.as_deref(), Some(oracle));
    let modification = ContinuousModification::AddKeyword {
        keyword: Keyword::Protection(ProtectionTarget::EachColor(
            ProtectionColorSet::AmongPermanentsYouControl,
        )),
    };
    let definition = face
        .static_abilities
        .iter()
        .find(|definition| definition.modifications.contains(&modification))
        .expect("positive typed live-grant reach guard");
    assert_eq!(
        definition.protection_does_not_remove,
        aura.then_some(ProtectionDoesNotRemove::Source)
    );
    assert!(!face
        .keywords
        .iter()
        .any(|keyword| matches!(keyword, Keyword::Protection(ProtectionTarget::EachColor(_)))));
    assert!(
        card_face_gaps(face).is_empty(),
        "{name}: {:?}",
        card_face_gaps(face)
    );
}

// CR 702.16i: each member is a separate concrete protection ability.
// CR 110.1: off-battlefield cards and the opponent's permanents don't contribute.
#[test]
fn kannushi_pledge_self_and_aura_protect_from_noncreature_color() {
    for (aura, phased_out) in [(false, false), (false, true), (true, false), (true, true)] {
        let mut s = scenario();
        let fervor = s.add_real_card(P0, "Fervor", Zone::Battlefield, db());
        s.add_real_card(P0, "Fervor", Zone::Hand, db());
        s.add_real_card(P0, "Lightning Bolt", Zone::Graveyard, db());
        s.add_creature(P1, "Opponent green fixture", 4, 4)
            .with_color(vec![ManaColor::Green]);
        let victim = s.add_creature(P1, "Unprotected fixture", 1, 1).id();
        let host = s.add_creature(P0, "Colorless host fixture", 4, 6).id();
        let blue = s
            .add_creature(P0, "Blue census fixture", 4, 6)
            .with_color(vec![ManaColor::Blue])
            .id();
        let grant = s.add_real_card(
            P0,
            if aura {
                "Pledge of Loyalty"
            } else {
                "Empty-Shrine Kannushi"
            },
            Zone::Hand,
            db(),
        );
        let sweep = s.add_real_card(P1, "Pyroclasm", Zone::Hand, db());
        let mut runner = s.build();
        // CR 702.26b: status fixture only; no phasing delivery is claimed.
        if phased_out {
            runner
                .state_mut()
                .objects
                .get_mut(&blue)
                .unwrap()
                .phase_status = engine::game::game_object::PhaseStatus::PhasedOut {
                cause: engine::game::game_object::PhaseOutCause::Directly,
            };
        }
        let protected = if aura {
            cast_on(&mut runner, P0, grant, host);
            attached(&runner, grant, host);
            host
        } else {
            cast(&mut runner, P0, grant);
            grant
        };
        assert_eq!(runner.state().objects[&fervor].color, vec![ManaColor::Red]);
        assert!(runner.state().objects[&fervor].is_phased_in_permanent());
        assert!(runner.state().battlefield.contains(&blue));
        assert_eq!(runner.state().objects[&blue].controller, P0);
        assert_eq!(runner.state().objects[&blue].color, vec![ManaColor::Blue]);
        assert_eq!(runner.state().objects[&blue].is_phased_out(), phased_out);
        assert_eq!(
            colors(&runner, protected),
            if phased_out {
                vec![ManaColor::White, ManaColor::Red]
            } else {
                vec![ManaColor::White, ManaColor::Blue, ManaColor::Red]
            }
        );
        cast(&mut runner, P1, sweep);
        assert_eq!(runner.state().objects[&protected].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&protected].damage_marked, 0);
        assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
    }
    assert_grant(db(), "Empty-Shrine Kannushi", KANNUSHI, false);
    assert_grant(db(), "Pledge of Loyalty", PLEDGE, true);
}

#[test]
fn kannushi_pledge_distinct_multicolor_and_duplicate_contributors() {
    let mut s = scenario();
    s.add_creature(P0, "Multicolor fixture", 2, 2)
        .with_color(vec![ManaColor::Red, ManaColor::Blue]);
    s.add_real_card(P0, "Fervor", Zone::Battlefield, db());
    s.add_real_card(P0, "Fervor", Zone::Battlefield, db());
    let grant = s.add_real_card(P0, "Empty-Shrine Kannushi", Zone::Hand, db());
    let mut runner = s.build();
    cast(&mut runner, P0, grant);
    assert_eq!(
        colors(&runner, grant),
        vec![ManaColor::White, ManaColor::Blue, ManaColor::Red]
    );
    assert_eq!(
        engine::game::keywords::protection_color_set_members(
            runner.state(),
            &ProtectionColorSet::AmongPermanentsYouControl,
            P0
        ),
        vec![ManaColor::White, ManaColor::Blue, ManaColor::Red]
    );
}

// CR 105.3 + CR 611.3a: replacing colors changes the existing live grant.
#[test]
fn kannushi_pledge_color_changes_and_empty_set_are_live() {
    for (aura, red_remains) in [(false, false), (false, true), (true, false), (true, true)] {
        let mut s = scenario();
        let host = s.add_creature(P1, "Colorless host fixture", 4, 6).id();
        if red_remains {
            s.add_real_card(P0, "Fervor", Zone::Battlefield, db());
        }
        let grant = s.add_real_card(
            P0,
            if aura {
                "Pledge of Loyalty"
            } else {
                "Empty-Shrine Kannushi"
            },
            Zone::Hand,
            db(),
        );
        let moonlace = s.add_real_card(P0, "Moonlace", Zone::Hand, db());
        let mut runner = s.build();
        let protected = if aura {
            cast_on(&mut runner, P0, grant, host);
            host
        } else {
            cast(&mut runner, P0, grant);
            grant
        };
        assert_eq!(
            colors(&runner, protected),
            if red_remains {
                vec![ManaColor::White, ManaColor::Red]
            } else {
                vec![ManaColor::White]
            }
        );
        cast_on(&mut runner, P0, moonlace, grant);
        assert!(runner.state().objects[&grant].color.is_empty());
        assert_eq!(
            colors(&runner, protected),
            if red_remains {
                vec![ManaColor::Red]
            } else {
                vec![]
            }
        );
        assert!(runner.state().objects[&grant]
            .static_definitions
            .iter_all()
            .next()
            .is_some());
        assert_eq!(runner.state().objects[&grant].zone, Zone::Battlefield);
    }
    let mut s = scenario();
    let grant = s.add_real_card(P0, "Empty-Shrine Kannushi", Zone::Hand, db());
    let hour = s.add_real_card(P1, "Darkest Hour", Zone::Hand, db());
    let destroy = s.add_real_card(P0, "Demystify", Zone::Hand, db());
    let mut runner = s.build();
    cast(&mut runner, P0, grant);
    assert_eq!(colors(&runner, grant), vec![ManaColor::White]);
    cast(&mut runner, P1, hour);
    assert_eq!(runner.state().objects[&grant].color, vec![ManaColor::Black]);
    assert_eq!(colors(&runner, grant), vec![ManaColor::Black]);
    cast_on(&mut runner, P0, destroy, hour);
    assert_eq!(colors(&runner, grant), vec![ManaColor::White]);
}

// No test-side layer flush: entry must invalidate pre-existing recipients.
#[test]
fn kannushi_pledge_plain_entry_updates_existing_recipient() {
    let mut s = scenario();
    let grant = s.add_real_card(P0, "Empty-Shrine Kannushi", Zone::Hand, db());
    let red = s.add_real_card(P0, "Raging Goblin", Zone::Hand, db());
    let own_colorless = s.add_real_card(P0, "Ornithopter", Zone::Hand, db());
    let opponent = s.add_real_card(P1, "Raging Goblin", Zone::Hand, db());
    let mut runner = s.build();
    cast(&mut runner, P0, grant);
    assert_eq!(colors(&runner, grant), vec![ManaColor::White]);
    cast(&mut runner, P1, opponent);
    assert_eq!(colors(&runner, grant), vec![ManaColor::White]);
    cast(&mut runner, P0, own_colorless);
    assert_eq!(colors(&runner, grant), vec![ManaColor::White]);
    cast(&mut runner, P0, red);
    assert_eq!(
        colors(&runner, grant),
        vec![ManaColor::White, ManaColor::Red]
    );
}

// CR 613.1e precedes CR 613.1f. P1's black permanents never contribute to P0.
#[test]
fn kannushi_pledge_darkest_hour_colorless_entry_updates_existing_recipient() {
    for with_hour in [false, true] {
        let mut s = scenario();
        let host = s.add_creature(P1, "Opponent host fixture", 4, 6).id();
        if with_hour {
            s.add_real_card(P1, "Darkest Hour", Zone::Battlefield, db());
        }
        let pledge = s.add_real_card(P0, "Pledge of Loyalty", Zone::Hand, db());
        let thopter = s.add_real_card(P0, "Ornithopter", Zone::Hand, db());
        let mut runner = s.build();
        cast_on(&mut runner, P0, pledge, host);
        assert_eq!(colors(&runner, host), vec![ManaColor::White]);
        assert!(runner.state().objects[&thopter].base_color.is_empty());
        assert!(runner.state().objects[&thopter]
            .base_keywords
            .contains(&Keyword::Flying));
        assert_eq!(
            runner.state().objects[&thopter].card_types.subtypes,
            vec!["Thopter".to_string()]
        );
        assert!(runner.state().objects[&thopter]
            .card_types
            .core_types
            .contains(&engine::types::card_type::CoreType::Artifact));
        assert!(runner.state().objects[&thopter]
            .card_types
            .core_types
            .contains(&engine::types::card_type::CoreType::Creature));
        cast(&mut runner, P0, thopter);
        assert_eq!(runner.state().objects[&thopter].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&thopter].controller, P0);
        assert_eq!(runner.state().objects[&pledge].controller, P0);
        assert_eq!(runner.state().objects[&host].controller, P1);
        assert_eq!(
            runner.state().objects[&thopter].color,
            if with_hour {
                vec![ManaColor::Black]
            } else {
                vec![]
            }
        );
        assert_eq!(
            colors(&runner, host),
            if with_hour {
                vec![ManaColor::White, ManaColor::Black]
            } else {
                vec![ManaColor::White]
            }
        );
        attached(&runner, pledge, host);
    }
}

#[test]
fn kannushi_pledge_departure_removes_last_color() {
    let mut s = scenario();
    let host = s.add_creature(P0, "Colorless host fixture", 4, 6).id();
    let pledge = s.add_real_card(P0, "Pledge of Loyalty", Zone::Hand, db());
    let first = s.add_real_card(P0, "Fervor", Zone::Battlefield, db());
    let last = s.add_real_card(P0, "Fervor", Zone::Battlefield, db());
    let destroy_first = s.add_real_card(P1, "Demystify", Zone::Hand, db());
    let destroy_last = s.add_real_card(P1, "Demystify", Zone::Hand, db());
    let bolt = s.add_real_card(P1, "Lightning Bolt", Zone::Hand, db());
    let mut runner = s.build();
    cast_on(&mut runner, P0, pledge, host);
    assert_eq!(
        colors(&runner, host),
        vec![ManaColor::White, ManaColor::Red]
    );
    cast_on(&mut runner, P1, destroy_first, first);
    assert_eq!(
        colors(&runner, host),
        vec![ManaColor::White, ManaColor::Red]
    );
    cast_on(&mut runner, P1, destroy_last, last);
    assert_eq!(colors(&runner, host), vec![ManaColor::White]);
    cast_on(&mut runner, P1, bolt, host);
    assert_eq!(runner.state().objects[&host].damage_marked, 3);
}

// CR 109.5 + CR 303.4e: the Aura's controller, not its owner or host, binds you.
#[test]
fn kannushi_pledge_your_tracks_granting_permanent_controller() {
    let mut s = GameScenario::new_n_player(3, 42);
    s.at_phase(Phase::PreCombatMain);
    s.add_creature(PlayerId(2), "Third player black fixture", 4, 6)
        .with_color(vec![ManaColor::Black]);
    let host = s
        .add_creature(P1, "Opponent green host fixture", 4, 6)
        .with_color(vec![ManaColor::Green])
        .id();
    s.add_real_card(P0, "Fervor", Zone::Battlefield, db());
    let pledge = s.add_real_card(P0, "Pledge of Loyalty", Zone::Hand, db());
    let steal = s.add_real_card(P1, "Steal Enchantment", Zone::Hand, db());
    let mut runner = s.build();
    cast_on(&mut runner, P0, pledge, host);
    assert_eq!(
        colors(&runner, host),
        vec![ManaColor::White, ManaColor::Red]
    );
    cast_on(&mut runner, P1, steal, pledge);
    assert_eq!(runner.state().objects[&pledge].controller, P1);
    assert_eq!(runner.state().objects[&pledge].owner, P0);
    assert_eq!(runner.state().objects[&host].controller, P1);
    assert_eq!(
        colors(&runner, host),
        vec![ManaColor::White, ManaColor::Blue, ManaColor::Green]
    );
    attached(&runner, pledge, host);

    // A separately controlled blue Aura changes only host control. Its blue
    // color is outside P0's census, so it remains legally attached.
    let mut s = scenario();
    let host = s
        .add_creature(P0, "Green host fixture", 4, 6)
        .with_color(vec![ManaColor::Green])
        .id();
    s.add_real_card(P0, "Fervor", Zone::Battlefield, db());
    let pledge = s.add_real_card(P0, "Pledge of Loyalty", Zone::Hand, db());
    let control = s.add_real_card(P1, "Control Magic", Zone::Hand, db());
    let mut runner = s.build();
    cast_on(&mut runner, P0, pledge, host);
    assert_eq!(
        colors(&runner, host),
        vec![ManaColor::White, ManaColor::Red, ManaColor::Green]
    );
    cast_on(&mut runner, P1, control, host);
    assert_eq!(runner.state().objects[&host].controller, P1);
    assert_eq!(runner.state().objects[&pledge].controller, P0);
    assert_eq!(
        colors(&runner, host),
        vec![ManaColor::White, ManaColor::Red]
    );
    attached(&runner, control, host);
    attached(&runner, pledge, host);
}

// CR 702.16n + CR 704.5m: the rider exempts its own Aura only.
#[test]
fn kannushi_pledge_source_rider_keeps_only_its_own_aura() {
    let mut s = scenario();
    let host = s.add_creature(P1, "Colorless host fixture", 4, 6).id();
    let sibling = s.add_creature(P1, "Unprotected sibling fixture", 4, 6).id();
    let later_white = s.add_real_card(P0, "Pacifism", Zone::Hand, db());
    let pacifism = s.add_real_card(P0, "Pacifism", Zone::Hand, db());
    let pledge = s.add_real_card(P0, "Pledge of Loyalty", Zone::Hand, db());
    let mut runner = s.build();
    cast_on(&mut runner, P0, pacifism, host);
    attached(&runner, pacifism, host);
    cast_on(&mut runner, P0, pledge, host);
    assert_eq!(colors(&runner, host), vec![ManaColor::White]);
    attached(&runner, pledge, host);
    assert_eq!(runner.state().objects[&pacifism].zone, Zone::Graveyard);
    let legal = find_legal_targets(
        runner.state(),
        &TargetFilter::Typed(TypedFilter::creature()),
        P0,
        later_white,
    );
    assert!(legal.contains(&TargetRef::Object(sibling)));
    assert!(
        !legal.contains(&TargetRef::Object(host)),
        "the Source rider is not a targeting exemption"
    );
}

// CR 702.16n + CR 704.3: independent grants remove BOTH matching Auras
// simultaneously. Moonlace makes Ward's intervening white cast legal.
#[test]
fn kannushi_pledge_independent_protection_removes_matching_auras() {
    let mut s = scenario();
    let host = s.add_creature(P1, "Colorless host fixture", 4, 6).id();
    let pledge = s.add_real_card(P0, "Pledge of Loyalty", Zone::Hand, db());
    let moonlace = s.add_real_card(P0, "Moonlace", Zone::Hand, db());
    let ward = s.add_real_card(P1, "Flickering Ward", Zone::Hand, db());
    let painter = s.add_real_card(P0, "Painter's Servant", Zone::Hand, db());
    let mut runner = s.build();
    cast_on(&mut runner, P0, pledge, host);
    assert_eq!(colors(&runner, host), vec![ManaColor::White]);
    cast_on(&mut runner, P0, moonlace, pledge);
    assert!(colors(&runner, host).is_empty());
    prepare(&mut runner, P1);
    let outcome = runner.cast(ward).target_object(host).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::NamedChoice { player, .. } if *player == P1)
    );
    let WaitingFor::NamedChoice {
        player, options, ..
    } = &runner.state().waiting_for
    else {
        panic!("Ward must ask a color")
    };
    assert_eq!(*player, P1);
    let white = options
        .iter()
        .find(|option| option.eq_ignore_ascii_case("white"))
        .expect("white option")
        .clone();
    runner
        .act(GameAction::ChooseOption { choice: white })
        .unwrap();
    runner.advance_until_stack_empty();
    complete(&runner);
    attached(&runner, pledge, host);
    attached(&runner, ward, host);
    assert_eq!(colors(&runner, host), vec![ManaColor::White]);
    assert!(runner.state().objects[&pledge].color.is_empty());
    assert_eq!(
        runner.state().objects[&ward].chosen_color(),
        Some(ManaColor::White)
    );
    for source in [pledge, ward] {
        assert!(runner.state().objects[&source]
            .static_definitions
            .iter_all()
            .any(|definition| definition.protection_does_not_remove
                == Some(ProtectionDoesNotRemove::Source)));
    }
    prepare(&mut runner, P0);
    let outcome = runner.cast(painter).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::NamedChoice { player, .. } if *player == P0)
    );
    let WaitingFor::NamedChoice {
        player, options, ..
    } = &runner.state().waiting_for
    else {
        panic!("Painter must ask a color")
    };
    assert_eq!(*player, P0);
    let white = options
        .iter()
        .find(|option| option.eq_ignore_ascii_case("white"))
        .expect("white option")
        .clone();
    runner
        .act(GameAction::ChooseOption { choice: white })
        .unwrap();
    runner.advance_until_stack_empty();
    complete(&runner);
    for (aura, owner) in [(pledge, P0), (ward, P1)] {
        assert_eq!(runner.state().objects[&aura].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&aura].owner, owner);
    }
    assert_eq!(runner.state().objects[&host].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&painter].zone, Zone::Battlefield);
}

#[test]
fn kannushi_pledge_export_and_game_state_roundtrip_keep_live_semantics() {
    let mut json = String::new();
    flate2::read::GzDecoder::new(&include_bytes!("../fixtures/integration_cards.json.gz")[..])
        .read_to_string(&mut json)
        .unwrap();
    let string_db = CardDatabase::from_json_str(&json).unwrap();
    let directory = std::env::temp_dir().join(format!("kannushi-pledge-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("cards.json");
    std::fs::write(&path, &json).unwrap();
    let file_db = CardDatabase::from_export(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::remove_dir(&directory).unwrap();
    for database in [&string_db, &file_db] {
        let old = database
            .get_face_by_name("Commander's Plate")
            .expect("old set fixture must still load");
        assert!(old
            .static_abilities
            .iter()
            .any(
                |definition| definition.modifications.iter().any(|modification| matches!(
                    modification,
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Protection(ProtectionTarget::EachColor(
                            ProtectionColorSet::OutsideCommanderColorIdentity
                        ))
                    }
                ))
            ));
        let mut s = scenario();
        let host = s.add_creature(P1, "Colorless host fixture", 4, 6).id();
        let pledge = s.add_real_card(P0, "Pledge of Loyalty", Zone::Hand, database);
        let kannushi = s.add_real_card(P0, "Empty-Shrine Kannushi", Zone::Hand, database);
        let pacifism = s.add_real_card(P0, "Pacifism", Zone::Hand, database);
        s.add_real_card(P0, "Fervor", Zone::Battlefield, database);
        let sweep = s.add_real_card(P1, "Pyroclasm", Zone::Hand, database);
        let mut runner = s.build();
        cast(&mut runner, P0, kannushi);
        assert_eq!(
            colors(&runner, kannushi),
            vec![ManaColor::White, ManaColor::Red]
        );
        cast_on(&mut runner, P0, pacifism, host);
        let wire = serde_json::to_string(runner.state()).unwrap();
        let restored: GameState = serde_json::from_str(&wire).unwrap();
        let mut runner = GameRunner::from_state(restored);
        cast_on(&mut runner, P0, pledge, host);
        assert_eq!(
            colors(&runner, host),
            vec![ManaColor::White, ManaColor::Red]
        );
        assert_eq!(runner.state().objects[&pacifism].zone, Zone::Graveyard);
        attached(&runner, pledge, host);
        cast(&mut runner, P1, sweep);
        assert_eq!(runner.state().objects[&host].damage_marked, 0);
        assert_eq!(runner.state().objects[&host].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&kannushi].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&kannushi].damage_marked, 0);
        assert_grant(database, "Empty-Shrine Kannushi", KANNUSHI, false);
        assert_grant(database, "Pledge of Loyalty", PLEDGE, true);
    }
}

#[test]
fn kannushi_pledge_protection_reaches_targeting() {
    let mut s = scenario();
    let host = s.add_creature(P0, "Colorless host fixture", 4, 6).id();
    let sibling = s.add_creature(P0, "Unprotected fixture", 4, 6).id();
    let pledge = s.add_real_card(P0, "Pledge of Loyalty", Zone::Hand, db());
    s.add_real_card(P0, "Fervor", Zone::Battlefield, db());
    let bolt = s.add_real_card(P1, "Lightning Bolt", Zone::Hand, db());
    let mut runner = s.build();
    cast_on(&mut runner, P0, pledge, host);
    assert_eq!(
        colors(&runner, host),
        vec![ManaColor::White, ManaColor::Red]
    );
    let legal = find_legal_targets(
        runner.state(),
        &TargetFilter::Typed(TypedFilter::creature()),
        P1,
        bolt,
    );
    assert!(legal.contains(&TargetRef::Object(sibling)));
    assert!(!legal.contains(&TargetRef::Object(host)));
    cast_on(&mut runner, P1, bolt, sibling);
    assert_eq!(runner.state().objects[&sibling].damage_marked, 3);
}

// CR 702.16f: blocking reads the same concrete colors as targeting and damage.
#[test]
fn kannushi_pledge_protection_reaches_blocking() {
    use engine::game::combat::AttackTarget;

    let mut s = scenario();
    let host = s.add_creature(P0, "Attacker fixture", 4, 6).id();
    let sibling = s.add_creature(P0, "Sibling attacker fixture", 4, 6).id();
    let red = s
        .add_creature(P1, "Red blocker fixture", 2, 6)
        .with_color(vec![ManaColor::Red])
        .id();
    let colorless = s.add_creature(P1, "Colorless blocker fixture", 2, 6).id();
    let pledge = s.add_real_card(P0, "Pledge of Loyalty", Zone::Hand, db());
    s.add_real_card(P0, "Fervor", Zone::Battlefield, db());
    let mut runner = s.build();
    cast_on(&mut runner, P0, pledge, host);
    assert_eq!(
        colors(&runner, host),
        vec![ManaColor::White, ManaColor::Red]
    );
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (host, AttackTarget::Player(P1)),
            (sibling, AttackTarget::Player(P1)),
        ])
        .unwrap();
    runner.pass_both_players();
    let before = serde_json::to_value(runner.state()).unwrap();
    let error = runner
        .declare_blockers(&[(red, host)])
        .expect_err("red blocker must be rejected");
    assert!(
        error.to_string().to_lowercase().contains("protection"),
        "{error}"
    );
    assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
    runner
        .declare_blockers(&[(red, sibling), (colorless, host)])
        .unwrap();
    runner.combat_damage();
    assert_eq!(runner.state().objects[&host].damage_marked, 2);
    assert_eq!(runner.state().objects[&sibling].damage_marked, 2);
    assert_eq!(runner.state().objects[&red].damage_marked, 4);
    assert_eq!(runner.state().objects[&colorless].damage_marked, 4);
    complete(&runner);
}

// SHAPE / coverage honesty: adversarial serialized encodings, not gameplay
// substitutes for either real Oracle card. Runtime support is guarded above.
#[test]
fn kannushi_pledge_coverage_keeps_unexpanded_and_inert_encodings_red() {
    use engine::game::coverage::{audit_resolver_features, build_parse_details_for_face};

    let face = db().get_face_by_name("Empty-Shrine Kannushi").unwrap();
    let mut printed = face.clone();
    printed.static_abilities.clear();
    printed.keywords = vec![Keyword::Protection(ProtectionTarget::EachColor(
        ProtectionColorSet::AmongPermanentsYouControl,
    ))];
    assert!(card_face_gaps(&printed)
        .iter()
        .any(|gap| gap == "Keyword:ProtectionColorSetUnexpanded"));
    assert!(build_parse_details_for_face(&printed)
        .iter()
        .any(|item| !item.is_fully_supported()));

    let mut inert = face.clone();
    inert.static_abilities[0].modifications = vec![ContinuousModification::AddKeyword {
        keyword: Keyword::Protection(ProtectionTarget::CardType(
            "each color among permanents you control".to_string(),
        )),
    }];
    let encoded = serde_json::json!({ "empty-shrine kannushi": inert });
    let inert_db = CardDatabase::from_json_str(&encoded.to_string()).unwrap();
    let audit = audit_resolver_features(&inert_db);
    assert_eq!(
        audit.total_supported_audited, 1,
        "the structural audit must reach the inert payload"
    );
    assert!(audit
        .flagged_cards
        .iter()
        .any(|card| card.card_name == "empty-shrine kannushi"
            && card
                .unhandled_features
                .iter()
                .any(|feature| feature == "structural:inert_protection_quality_grant")));
    let audit = audit_resolver_features(db());
    for name in ["Empty-Shrine Kannushi", "Pledge of Loyalty"] {
        assert!(db().get_face_by_name(name).is_some());
        assert!(
            !audit
                .flagged_cards
                .iter()
                .any(|card| card.card_name == name.to_lowercase()),
            "{name}: live static must not be structurally inert"
        );
    }
}

#[test]
fn kannushi_pledge_raw_oracle_and_nonbattlefield_colors() {
    let mut s = scenario();
    let kannushi = s
        .add_creature_to_hand_from_oracle(P0, "Empty-Shrine Kannushi", 1, 1, KANNUSHI)
        .with_color(vec![ManaColor::White])
        .id();
    for zone in [Zone::Hand, Zone::Graveyard, Zone::Library, Zone::Exile] {
        s.add_real_card(P0, "Fervor", zone, db());
    }
    let bolt = s.add_real_card(P0, "Lightning Bolt", Zone::Hand, db());
    let control = s.add_creature(P1, "Damage control fixture", 4, 6).id();
    let mut runner = s.build();
    cast(&mut runner, P0, kannushi);
    assert_eq!(colors(&runner, kannushi), vec![ManaColor::White]);
    prepare(&mut runner, P0);
    let committed = runner.cast(bolt).target_object(control).commit();
    assert_eq!(committed.state().objects[&bolt].zone, Zone::Stack);
    assert!(!committed.state().stack.is_empty());
    assert_eq!(
        engine::game::keywords::protection_color_set_members(
            committed.state(),
            &ProtectionColorSet::AmongPermanentsYouControl,
            P0,
        ),
        vec![ManaColor::White]
    );
    let outcome = committed.resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert_eq!(outcome.state().objects[&control].damage_marked, 3);
    complete(&runner);
    assert_eq!(colors(&runner, kannushi), vec![ManaColor::White]);
}
