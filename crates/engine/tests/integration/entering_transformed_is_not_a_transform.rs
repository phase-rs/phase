//! CR 712.14a + CR 701.27a: a permanent that enters the battlefield transformed
//! enters with its back face up; it does not transform. "Whenever a permanent you
//! control transforms" abilities see a real transform but not such an entry
//! (Brutal Cathar ruling: "It won't enter with its daybound face up and then
//! transform.").

use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::AbilityKind;
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::{CastPaymentMode, DayNight, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const CULT_OF_THE_WAXING_MOON: &str = "Whenever a permanent you control transforms into a non-Human creature, create a 2/2 green Wolf creature token.";

const THRABEN_GARGOYLE: &str = "Defender\n{6}: Transform this creature.";

/// Jill, Shiva's Dominant's activation paragraph; her ETB paragraph is not under test.
const JILL_RETURN_TRANSFORMED: &str = "{3}{U}{U}, {T}: Exile Jill, then return it to the battlefield transformed under its owner's control. Activate only as a sorcery.";

const LUNARCH_VETERAN: &str = "Whenever another creature you control enters, you gain 1 life.\nDisturb {1}{W} (You may cast this card from your graveyard transformed for its disturb cost.)";

const STARTLED_AWAKE: &str = "Target opponent mills thirteen cards.\n{3}{U}{U}: Put this card from your graveyard onto the battlefield transformed. Activate only as a sorcery.";

const TAVERN_RUFFIAN: &str =
    "Daybound (If a player casts no spells during their own turn, it becomes night next turn.)";

fn non_human_creature_face(
    name: &str,
    power: i32,
    toughness: i32,
    subtypes: &[&str],
) -> BackFaceData {
    BackFaceData {
        name: name.to_string(),
        power: Some(power),
        toughness: Some(toughness),
        card_types: CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: subtypes.iter().map(|s| s.to_string()).collect(),
        },
        ..Default::default()
    }
}

fn wolves(runner: &GameRunner) -> usize {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter(|id| {
            let object = &state.objects[id];
            object.name == "Wolf" && object.controller == P0
        })
        .count()
}

fn activate_only_ability(runner: &mut GameRunner, source: ObjectId, mana: &[ManaType]) {
    for &kind in mana {
        runner
            .state_mut()
            .add_mana_to_pool(P0, ManaUnit::new(kind, ObjectId(0), false, vec![]));
    }
    let ability_index = runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| ability.kind == AbilityKind::Activated)
        .expect("the source has its printed activated ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate the printed ability");
}

fn resolve_stack(runner: &mut GameRunner) {
    for _ in 0..30 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt while resolving: {other:?}"),
        }
    }
    panic!("the stack never settled");
}

/// Thraben Gargoyle transforms (the reach guard) and Jill returns to the
/// battlefield transformed; only the transform makes a Wolf.
#[test]
fn returning_to_the_battlefield_transformed_is_not_a_transform() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Cult of the Waxing Moon", 5, 4, CULT_OF_THE_WAXING_MOON);
    let gargoyle = scenario
        .add_creature_from_oracle(P0, "Thraben Gargoyle", 2, 2, THRABEN_GARGOYLE)
        .id();
    let jill = scenario
        .add_creature_from_oracle(P0, "Jill, Shiva's Dominant", 2, 2, JILL_RETURN_TRANSFORMED)
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&gargoyle)
        .unwrap()
        .back_face = Some(non_human_creature_face(
        "Stonewing Antagonizer",
        4,
        2,
        &["Gargoyle", "Horror"],
    ));
    runner.state_mut().objects.get_mut(&jill).unwrap().back_face =
        Some(non_human_creature_face("Returned Back Face", 4, 5, &[]));

    activate_only_ability(&mut runner, gargoyle, &[ManaType::Colorless; 6]);
    resolve_stack(&mut runner);
    assert!(runner.state().objects[&gargoyle].transformed);
    assert_eq!(wolves(&runner), 1, "reach: a transform triggers Cult");

    let mut mana = vec![ManaType::Colorless; 3];
    mana.extend([ManaType::Blue; 2]);
    activate_only_ability(&mut runner, jill, &mana);
    resolve_stack(&mut runner);
    let jill_object = &runner.state().objects[&jill];
    assert!(
        jill_object.zone == Zone::Battlefield && jill_object.transformed,
        "reach: Jill entered transformed"
    );
    assert_eq!(
        wolves(&runner),
        1,
        "entering transformed is not a transform"
    );
}

/// CR 702.145b: a daybound creature cast at night enters with its nightbound face
/// up, and that entry is not a transform.
#[test]
fn daybound_entering_at_night_is_not_a_transform() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Cult of the Waxing Moon", 5, 4, CULT_OF_THE_WAXING_MOON);
    let ruffian = scenario
        .add_creature_to_hand(P0, "Tavern Ruffian", 2, 5)
        .from_oracle_text_with_keywords(&["Daybound"], TAVERN_RUFFIAN)
        .id();
    let mut runner = scenario.build();
    let mut smasher = non_human_creature_face("Tavern Smasher", 6, 5, &["Werewolf"]);
    smasher.keywords = vec![Keyword::Nightbound];
    runner
        .state_mut()
        .objects
        .get_mut(&ruffian)
        .unwrap()
        .back_face = Some(smasher);
    runner.state_mut().day_night = Some(DayNight::Night);

    runner.cast(ruffian).resolve();
    let entered = &runner.state().objects[&ruffian];
    assert!(
        entered.zone == Zone::Battlefield
            && entered.transformed
            && entered.name == "Tavern Smasher",
        "reach: the daybound creature entered nightbound face up"
    );
    assert_eq!(
        wolves(&runner),
        0,
        "entering transformed is not a transform"
    );
}

/// CR 702.146b: a disturbed spell resolves back face up, and that entry is not a
/// transform.
#[test]
fn disturb_resolving_back_face_up_is_not_a_transform() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Cult of the Waxing Moon", 5, 4, CULT_OF_THE_WAXING_MOON);
    let veteran = scenario
        .add_creature_to_graveyard(P0, "Lunarch Veteran", 1, 1)
        .from_oracle_text_with_keywords(&["Disturb"], LUNARCH_VETERAN)
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&veteran)
        .unwrap()
        .back_face = Some(non_human_creature_face(
        "Luminous Phantom",
        1,
        1,
        &["Spirit", "Cleric"],
    ));
    for kind in [ManaType::Colorless, ManaType::White] {
        runner
            .state_mut()
            .add_mana_to_pool(P0, ManaUnit::new(kind, ObjectId(0), false, vec![]));
    }

    let card_id = runner.state().objects[&veteran].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: veteran,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Lunarch Veteran with disturb");
    resolve_stack(&mut runner);
    let entered = &runner.state().objects[&veteran];
    assert!(
        entered.zone == Zone::Battlefield
            && entered.transformed
            && entered.name == "Luminous Phantom",
        "reach: the disturbed spell entered back face up"
    );
    assert_eq!(
        wolves(&runner),
        0,
        "entering transformed is not a transform"
    );
}

/// CR 712.14a: Startled Awake put onto the battlefield transformed enters back face
/// up, and that entry is not a transform.
#[test]
fn put_onto_the_battlefield_transformed_is_not_a_transform() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Cult of the Waxing Moon", 5, 4, CULT_OF_THE_WAXING_MOON);
    let startled = scenario
        .add_spell_to_graveyard(P0, "Startled Awake", false)
        .from_oracle_text(STARTLED_AWAKE)
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&startled)
        .unwrap()
        .back_face = Some(non_human_creature_face(
        "Persistent Nightmare",
        1,
        1,
        &["Nightmare"],
    ));

    let mut mana = vec![ManaType::Colorless; 3];
    mana.extend([ManaType::Blue; 2]);
    activate_only_ability(&mut runner, startled, &mana);
    resolve_stack(&mut runner);
    let entered = &runner.state().objects[&startled];
    assert!(
        entered.zone == Zone::Battlefield
            && entered.transformed
            && entered.name == "Persistent Nightmare",
        "reach: Startled Awake entered back face up"
    );
    assert_eq!(
        wolves(&runner),
        0,
        "entering transformed is not a transform"
    );
}
