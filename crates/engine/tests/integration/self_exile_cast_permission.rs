//! A card whose own text permits casting it from exile, read at the cast
//! permission authority and driven through `apply()`.
//!
//! | Card | Seat / zone | Why it is here |
//! |---|---|---|
//! | Misthollow Griffin | P0 exile | a class member that is neither Board C creature |
//! | Eternal Scourge | P1 exile | a member P0 does not own |
//! | seeded `{U}{U}` + two colorless | P0's mana pool | pays Misthollow Griffin's `{2}{U}{U}` |
//! | Grizzly Bears | P0 exile | no permission of its own |
//! | Gravecrawler | P0 exile | its permission names only the graveyard |
//! | Gravecrawler | P0 battlefield | a Zombie P0 controls, so the exiled copy's condition holds |
//! | Hundred-Battle Veteran | P0 graveyard | graveyard live control |
//! | Squee, the Immortal | P0 graveyard | the graveyard half of a graveyard-or-exile permission |

use engine::ai_support::legal_actions;
use engine::game::casting::spell_objects_available_to_cast;
use engine::game::engine::EngineError;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{CardPlayMode, Effect, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::game_state::{CastingVariant, StackEntryKind};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::{CastFrequency, StaticMode};
use engine::types::zones::Zone;

use crate::food_chain_board::cast_spell;
use crate::support::shared_card_db as load_db;

fn pool(types: &[ManaType]) -> Vec<ManaUnit> {
    types
        .iter()
        .map(|&mana| ManaUnit::new(mana, ObjectId(0), false, vec![]))
        .collect()
}

fn assert_not_castable_zone(result: Result<(), EngineError>) {
    assert!(
        matches!(
            &result,
            Err(EngineError::InvalidAction(message)) if message == "Card is not in a castable zone"
        ),
        "{result:?}"
    );
}

fn top_casting_variant(runner: &GameRunner) -> CastingVariant {
    match &runner
        .state()
        .stack
        .last()
        .expect("a spell is on the stack")
        .kind
    {
        StackEntryKind::Spell {
            casting_variant, ..
        } => *casting_variant,
        other => panic!("expected a spell, got {other:?}"),
    }
}

struct MemberBoard {
    runner: GameRunner,
    misthollow: ObjectId,
    foreign_scourge: ObjectId,
}

fn member_board() -> Option<MemberBoard> {
    let db = load_db()?;
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let misthollow = scenario.add_real_card(P0, "Misthollow Griffin", Zone::Exile, db);
    let foreign_scourge = scenario.add_real_card(P1, "Eternal Scourge", Zone::Exile, db);
    scenario.with_mana_pool(
        P0,
        pool(&[
            ManaType::Blue,
            ManaType::Blue,
            ManaType::Colorless,
            ManaType::Colorless,
        ]),
    );
    Some(MemberBoard {
        runner: scenario.build(),
        misthollow,
        foreign_scourge,
    })
}

/// CR 604.6 + CR 113.6f: a member's own permission casts it from exile, for
/// its owner only (CR 109.5 + CR 108.4a).
#[test]
fn misthollow_griffin_is_cast_from_exile_and_a_foreign_member_is_not() {
    let Some(MemberBoard {
        mut runner,
        misthollow,
        foreign_scourge,
    }) = member_board()
    else {
        return;
    };
    let available = spell_objects_available_to_cast(runner.state(), P0);
    assert!(available.contains(&misthollow));
    assert!(!available.contains(&foreign_scourge));
    let mut foreign = member_board().expect("board builds");
    assert_not_castable_zone(cast_spell(&mut foreign.runner, foreign.foreign_scourge));

    assert!(legal_actions(runner.state()).iter().any(
        |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == misthollow)
    ));
    cast_spell(&mut runner, misthollow).expect("Misthollow Griffin is cast from exile");
    assert_eq!(top_casting_variant(&runner), CastingVariant::Normal);
    for _ in 0..16 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority passes");
    }
    assert_eq!(runner.state().objects[&misthollow].zone, Zone::Battlefield);
}

struct HostileBoard {
    runner: GameRunner,
    bears: ObjectId,
    gravecrawler: ObjectId,
    veteran: ObjectId,
}

fn hostile_board() -> Option<HostileBoard> {
    let db = load_db()?;
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Exile, db);
    let gravecrawler = scenario.add_real_card(P0, "Gravecrawler", Zone::Exile, db);
    scenario.add_real_card(P0, "Gravecrawler", Zone::Battlefield, db);
    let veteran = scenario.add_real_card(P0, "Hundred-Battle Veteran", Zone::Graveyard, db);
    Some(HostileBoard {
        runner: scenario.build(),
        bears,
        gravecrawler,
        veteran,
    })
}

/// No permission, or a graveyard-only one, admits a card from exile.
#[test]
fn exiled_cards_without_an_own_exile_permission_are_not_castable() {
    let Some(board) = hostile_board() else {
        return;
    };
    let available = spell_objects_available_to_cast(board.runner.state(), P0);
    assert!(available.contains(&board.veteran));
    assert!(!available.contains(&board.bears));
    assert!(!available.contains(&board.gravecrawler));

    let pick: [fn(&HostileBoard) -> ObjectId; 2] = [|b| b.bears, |b| b.gravecrawler];
    for hostile in pick {
        let mut board = hostile_board().expect("board builds");
        let id = hostile(&board);
        assert_not_castable_zone(cast_spell(&mut board.runner, id));
    }
}

/// A self exile permission is castable only when its condition parsed and holds.
#[test]
fn self_exile_permission_with_an_unparsed_condition_is_not_castable() {
    let zombie = "You may cast this card from exile as long as you control a Zombie.";
    let unparsed = "You may cast this card from exile as long as you control a ~ planeswalker.";
    for (text, zombie_present, castable) in [
        (zombie, false, false),
        (zombie, true, true),
        (unparsed, false, false),
    ] {
        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let carrier = scenario
            .add_creature_to_exile(P0, "Exile Carrier", 2, 2)
            .from_oracle_text(text)
            .id();
        if zombie_present {
            scenario
                .add_creature(P0, "Zombie", 2, 2)
                .with_subtypes(vec!["Zombie"]);
        }
        let mut runner = scenario.build();
        assert_eq!(
            spell_objects_available_to_cast(runner.state(), P0).contains(&carrier),
            castable,
            "{text} zombie={zombie_present}"
        );
        let result = cast_spell(&mut runner, carrier);
        if castable {
            result.expect("carrier is cast from exile");
        } else {
            assert_not_castable_zone(result);
        }
    }
}

/// The graveyard half of Squee's permission keeps its graveyard-permission cast.
#[test]
fn squee_cast_from_graveyard_elects_its_graveyard_permission() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let squee = scenario.add_real_card(P0, "Squee, the Immortal", Zone::Graveyard, db);
    scenario.with_mana_pool(
        P0,
        pool(&[ManaType::Red, ManaType::Red, ManaType::Colorless]),
    );
    let mut runner = scenario.build();
    cast_spell(&mut runner, squee).expect("Squee is cast from the graveyard");
    assert!(matches!(
        top_casting_variant(&runner),
        CastingVariant::GraveyardPermission { source, .. } if source == squee
    ));
}

fn self_cast_permission_zones(face: &CardFace) -> Vec<Vec<Zone>> {
    face.static_abilities
        .iter()
        .filter(|definition| {
            matches!(
                definition.mode,
                StaticMode::GraveyardCastPermission {
                    frequency: CastFrequency::Unlimited,
                    play_mode: CardPlayMode::Cast,
                    ..
                }
            ) && matches!(definition.affected, Some(TargetFilter::SelfRef))
        })
        .map(|definition| definition.active_zones.clone())
        .collect()
}

/// The fixture the boards load carries this lane's parse of each member.
#[test]
fn fixture_carries_the_self_exile_permission_parse() {
    let Some(db) = load_db() else {
        return;
    };
    let face = |name: &str| {
        db.get_face_by_name(name)
            .unwrap_or_else(|| panic!("card '{name}' not found in CardDatabase"))
    };
    assert_eq!(
        self_cast_permission_zones(face("Gravecrawler")),
        vec![vec![Zone::Graveyard]]
    );
    assert_eq!(
        self_cast_permission_zones(face("Squee, the Immortal")),
        vec![vec![Zone::Graveyard, Zone::Exile]]
    );
    let scourge = face("Eternal Scourge");
    assert_eq!(self_cast_permission_zones(scourge), vec![vec![Zone::Exile]]);
    assert!(!scourge
        .abilities
        .iter()
        .any(|ability| matches!(*ability.effect, Effect::CastFromZone { .. })));
}
