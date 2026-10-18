//! CR 205.1a (+ CR 205.3c/205.3d): creature types require a surviving Creature,
//! Kindred or legacy Tribal type. "If an object's card type is
//! removed, the subtypes correlated with that card type will remain if they are
//! also the subtypes of a card type the object currently has; otherwise, they are
//! also removed for the entire time the object's card type is removed."
//!
//! Real cards (the committed integration fixture): Overlord of the Balemurk
//! (impending), Lion Sash (reconfigure), Heliod, Sun-Crowned (devotion), Dryad
//! Arbor, Idol of False Gods (Kindred), March of the Machines, and the bestow
//! creatures Springheart Nantuko and Boon Satyr. Overlord of the Mistmoors,
//! Amoeboid Changeling and One with the Stars use their exact Oracle text with
//! scenario builders.
//!
//! Scenario tests run with an EMPTY `GameState::all_creature_types`, where the old
//! registry-keeps-creature-types clause is inert; every row that asserts a removal
//! installs a production-shaped registry first (`production_creature_registry`).

use engine::game::effects::attach::attach_to;
use engine::game::game_object::{AttachTarget, BestowFormState};
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{ContinuousModification, Duration, TargetFilter};
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastingVariant, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// Verbatim Oracle text (Scryfall), Overlord of the Mistmoors {5}{W}{W}.
/// Its sibling row is built from this text.
const MISTMOORS_ORACLE: &str = "Impending 4\u{2014}{2}{W}{W} (If you cast this spell for its impending cost, it enters with four time counters and isn't a creature until the last is removed. At the beginning of your end step, remove a time counter from it.)\nWhenever this permanent enters or attacks, create two 2/1 white Insect creature tokens with flying.";

/// A registry shaped like `deck_loading.rs`'s output: creature-face subtypes minus
/// subtypes that occur on non-creature faces. It holds every creature type under
/// test plus unrelated ones, and no noncreature subtype (Aura, Equipment, Forest).
fn production_creature_registry() -> Vec<String> {
    [
        "Avatar", "Bear", "Cat", "Dryad", "Eldrazi", "Elf", "God", "Goblin", "Horror", "Human",
        "Insect", "Monk", "Satyr",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn install_registry(runner: &mut GameRunner, asserted_types: &[&str]) {
    let state = runner.state_mut();
    state.all_creature_types = production_creature_registry();
    for ty in asserted_types {
        assert!(
            state.all_creature_types.iter().any(|t| t == ty),
            "reach guard: the registry contains {ty}"
        );
    }
}

fn refresh(runner: &mut GameRunner) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
}

fn add_mana(runner: &mut GameRunner, mana: ManaType, count: usize) {
    let pool = &mut runner.state_mut().players[0].mana_pool;
    for _ in 0..count {
        pool.add(ManaUnit::new(mana, ObjectId(0), false, vec![]));
    }
}

fn subtypes(runner: &GameRunner, id: ObjectId) -> Vec<String> {
    runner.state().objects[&id].card_types.subtypes.clone()
}

fn has_core(runner: &GameRunner, id: ObjectId, core: CoreType) -> bool {
    runner.state().objects[&id]
        .card_types
        .core_types
        .contains(&core)
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| s.to_string()).collect()
}

fn cost(generic: u32, shards: Vec<ManaCostShard>) -> ManaCost {
    ManaCost::Cost { shards, generic }
}

fn build(scenario: GameScenario) -> GameRunner {
    let db = load_db().expect("integration fixture");
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner
}

/// Perform exactly the `CastSpell` that `legal_actions` offers for `id` and let
/// the stack resolve (the Balemurk impending shape of
/// `hand_alternative_cost_legal_action_offer`).
fn perform_offered_cast(runner: &mut GameRunner, id: ObjectId) -> CastingVariant {
    let offered = engine::ai_support::legal_actions(runner.state())
        .into_iter()
        .find(
            |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == id),
        )
        .expect("legal_actions must offer the cast");
    runner
        .act(offered)
        .expect("the offered cast must be accepted");
    for _ in 0..4 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. }
        ) {
            runner
                .choose_first_legal_target()
                .expect("a legal target exists");
        }
    }
    let variant = runner
        .state()
        .stack
        .iter()
        .find_map(|entry| match &entry.kind {
            StackEntryKind::Spell {
                casting_variant, ..
            } if entry.id == id => Some(*casting_variant),
            _ => None,
        })
        .expect("the offered cast must put the spell on the stack");
    runner.advance_until_stack_empty();
    variant
}

/// A real Overlord of the Balemurk cast for its impending cost: five time counters
/// and not a creature, on the battlefield. `registry` is installed before the cast.
fn impending_balemurk(registry: &[&str]) -> (GameRunner, ObjectId) {
    let db = load_db().expect("integration fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let id = scenario.add_real_card(P0, "Overlord of the Balemurk", Zone::Hand, db);
    let mut runner = build(scenario);
    install_registry(&mut runner, registry);
    add_mana(&mut runner, ManaType::Black, 2);

    let variant = perform_offered_cast(&mut runner, id);

    assert_eq!(variant, CastingVariant::Impending, "reach: impending paid");
    assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&id]
            .counters
            .get(&CounterType::Time)
            .copied(),
        Some(5),
        "reach: five time counters"
    );
    refresh(&mut runner);
    (runner, id)
}

// ---------------------------------------------------------------------------
// Impending (CR 702.176a)
// ---------------------------------------------------------------------------

/// CR 702.176a + CR 205.1a: an impending Overlord with time counters is an
/// Enchantment with no creature types. Reverting the fix leaves `Avatar Horror`.
#[test]
fn impending_balemurk_has_no_creature_types_while_it_has_time_counters() {
    let (runner, id) = impending_balemurk(&["Avatar", "Horror"]);

    assert!(!has_core(&runner, id, CoreType::Creature));
    assert!(has_core(&runner, id, CoreType::Enchantment));
    assert_eq!(subtypes(&runner, id), Vec::<String>::new());
}

/// CR 613.1d: the loss lasts only while the type is removed; the pass rebuilds
/// from `base_card_types`, so the last counter's removal restores Avatar Horror.
#[test]
fn balemurk_regains_avatar_horror_when_last_time_counter_is_removed() {
    let (mut runner, id) = impending_balemurk(&["Avatar", "Horror"]);
    assert!(!has_core(&runner, id, CoreType::Creature), "reach: removed");
    assert!(subtypes(&runner, id).is_empty(), "reach: no types yet");

    runner
        .state_mut()
        .objects
        .get_mut(&id)
        .unwrap()
        .counters
        .remove(&CounterType::Time);
    refresh(&mut runner);

    assert!(has_core(&runner, id, CoreType::Creature));
    assert!(has_core(&runner, id, CoreType::Enchantment));
    assert_eq!(subtypes(&runner, id), strings(&["Avatar", "Horror"]));
}

/// The inline-Oracle sibling of the Balemurk row exercises the same behavior as
/// `impending_balemurk_has_no_creature_types_while_it_has_time_counters`.
#[test]
fn impending_mistmoors_has_no_creature_types_while_it_has_time_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let id = scenario
        .add_creature_to_hand_from_oracle(P0, "Overlord of the Mistmoors", 6, 6, MISTMOORS_ORACLE)
        .with_subtypes(vec!["Avatar", "Horror"])
        .with_mana_cost(cost(5, vec![ManaCostShard::White, ManaCostShard::White]))
        .from_oracle_text_with_keywords(&["Impending"], MISTMOORS_ORACLE)
        .id();
    for _ in 0..4 {
        scenario.add_basic_land(P0, engine::types::mana::ManaColor::White);
    }
    let mut runner = scenario.build();
    install_registry(&mut runner, &["Avatar", "Horror"]);
    {
        let obj = runner.state_mut().objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.base_card_types.core_types.push(CoreType::Enchantment);
    }

    let variant = perform_offered_cast(&mut runner, id);
    refresh(&mut runner);

    assert_eq!(variant, CastingVariant::Impending, "reach: impending paid");
    assert_eq!(
        runner.state().objects[&id]
            .counters
            .get(&CounterType::Time)
            .copied(),
        Some(4),
        "reach: four time counters"
    );
    assert!(!has_core(&runner, id, CoreType::Creature));
    assert!(has_core(&runner, id, CoreType::Enchantment));
    let types = subtypes(&runner, id);
    assert!(
        !types.iter().any(|s| s == "Avatar" || s == "Horror"),
        "no creature types while impending, got {types:?}"
    );
}

/// Registry independence: the same Balemurk board with an EMPTY registry.
#[test]
fn balemurk_creature_types_removed_with_empty_creature_type_registry() {
    let (mut runner, id) = impending_balemurk(&["Avatar", "Horror"]);
    runner.state_mut().all_creature_types.clear();
    refresh(&mut runner);

    assert!(!has_core(&runner, id, CoreType::Creature), "reach: removed");
    assert!(has_core(&runner, id, CoreType::Enchantment));
    assert_eq!(subtypes(&runner, id), Vec::<String>::new());
}

// ---------------------------------------------------------------------------
// Reconfigure (CR 702.151b), real Lion Sash
// ---------------------------------------------------------------------------

fn lion_sash_with_host() -> (GameRunner, ObjectId, ObjectId) {
    let db = load_db().expect("integration fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sash = scenario.add_real_card(P0, "Lion Sash", Zone::Battlefield, db);
    let host = scenario.add_creature(P0, "Host Bear", 2, 2).id();
    let mut runner = build(scenario);
    install_registry(&mut runner, &["Cat"]);
    refresh(&mut runner);
    (runner, sash, host)
}

fn detach(runner: &mut GameRunner, attachment: ObjectId, host: ObjectId) {
    let state = runner.state_mut();
    state.objects.get_mut(&attachment).unwrap().attached_to = None;
    state
        .objects
        .get_mut(&host)
        .unwrap()
        .attachments
        .retain(|id| *id != attachment);
    state.layers_dirty.mark_full();
    evaluate_layers(state);
}

/// CR 702.151b + CR 205.1a: attached, Lion Sash is `Artifact - Equipment`; the Cat
/// type returns when it is unattached. A transient artifact removal drops the
/// artifact type "Equipment" and keeps "Cat" (the artifact set is covered too).
#[test]
fn lion_sash_attached_loses_cat_keeps_equipment_and_regains_cat_when_unattached() {
    let (mut runner, sash, host) = lion_sash_with_host();
    assert!(
        has_core(&runner, sash, CoreType::Creature),
        "reach: creature"
    );
    assert_eq!(subtypes(&runner, sash), strings(&["Equipment", "Cat"]));

    attach_to(runner.state_mut(), sash, host);
    refresh(&mut runner);
    assert!(
        runner.state().objects[&sash].attached_to.is_some(),
        "reach: attached"
    );
    assert_eq!(
        runner.state().objects[&sash].card_types.core_types,
        vec![CoreType::Artifact]
    );
    assert_eq!(subtypes(&runner, sash), strings(&["Equipment"]));

    detach(&mut runner, sash, host);
    assert!(has_core(&runner, sash, CoreType::Creature));
    assert_eq!(subtypes(&runner, sash), strings(&["Equipment", "Cat"]));

    // Artifact-set generality: RemoveType { Artifact } drops "Equipment", keeps "Cat".
    runner.state_mut().add_transient_continuous_effect(
        sash,
        P0,
        Duration::UntilEndOfTurn,
        TargetFilter::SpecificObject { id: sash },
        vec![ContinuousModification::RemoveType {
            core_type: CoreType::Artifact,
        }],
        None,
    );
    evaluate_layers(runner.state_mut());
    assert!(!has_core(&runner, sash, CoreType::Artifact), "reach");
    assert_eq!(subtypes(&runner, sash), strings(&["Cat"]));
}

/// CR 613.8a + CR 613.1d + CR 613.7: the Sash's own `RemoveType { Creature }`
/// applies first; March's typed filter depends on it and then makes the Sash an
/// artifact creature. The Cat type removed first is not restored by that later
/// AddType. Only layers run (an Equipment creature would be unattached by
/// state-based actions, which is a different rule).
#[test]
fn march_of_the_machines_after_lion_sash_removal_does_not_restore_cat() {
    let db = load_db().expect("integration fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sash = scenario.add_real_card(P0, "Lion Sash", Zone::Battlefield, db);
    let host = scenario.add_creature(P0, "Host Bear", 2, 2).id();
    scenario.add_real_card(P0, "March of the Machines", Zone::Battlefield, db);
    let mut runner = build(scenario);
    install_registry(&mut runner, &["Cat"]);

    attach_to(runner.state_mut(), sash, host);
    refresh(&mut runner);

    assert!(
        runner.state().objects[&sash].attached_to.is_some(),
        "reach: attached"
    );
    assert!(has_core(&runner, sash, CoreType::Artifact));
    assert!(
        has_core(&runner, sash, CoreType::Creature),
        "reach: March applied (the Sash is an artifact creature)"
    );
    assert_eq!(subtypes(&runner, sash), strings(&["Equipment"]));

    detach(&mut runner, sash, host);
    assert!(has_core(&runner, sash, CoreType::Creature));
    assert_eq!(subtypes(&runner, sash), strings(&["Equipment", "Cat"]));
}

// ---------------------------------------------------------------------------
// Devotion gods, creature lands, Kindred
// ---------------------------------------------------------------------------

/// CR 205.1a + CR 205.3m: below devotion five Heliod isn't a creature and has no
/// God type; at five it is a creature God again.
#[test]
fn heliod_sun_crowned_below_devotion_is_not_a_creature_and_has_no_god_type_then_regains_it() {
    let db = load_db().expect("integration fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let heliod = scenario.add_real_card(P0, "Heliod, Sun-Crowned", Zone::Battlefield, db);
    let mut runner = build(scenario);
    install_registry(&mut runner, &["God"]);
    refresh(&mut runner);

    assert!(!has_core(&runner, heliod, CoreType::Creature));
    assert!(has_core(&runner, heliod, CoreType::Enchantment));
    assert_eq!(subtypes(&runner, heliod), Vec::<String>::new());
    assert!(
        runner.state().objects[&heliod]
            .keywords
            .iter()
            .any(|k| matches!(k, engine::types::keywords::Keyword::Indestructible)),
        "reach: Heliod's own static layer ran (indestructible)"
    );

    // Four more white pips ({2}{W} already counts one) reach devotion five.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let heliod = scenario.add_real_card(P0, "Heliod, Sun-Crowned", Zone::Battlefield, db);
    for name in ["White A", "White B"] {
        scenario
            .add_creature(P0, name, 1, 1)
            .with_mana_cost(cost(0, vec![ManaCostShard::White, ManaCostShard::White]));
    }
    let mut runner = build(scenario);
    install_registry(&mut runner, &["God"]);
    refresh(&mut runner);

    assert!(has_core(&runner, heliod, CoreType::Creature));
    assert!(subtypes(&runner, heliod).iter().any(|s| s == "God"));
}

/// CR 205.1a: removing Creature from Dryad Arbor drops Dryad (creature set) but
/// keeps Forest (land set); removing Land drops Forest and keeps Dryad.
#[test]
fn dryad_arbor_loses_dryad_not_forest_and_forest_not_dryad_by_removed_type() {
    for (removed, expected) in [
        (CoreType::Creature, strings(&["Forest"])),
        (CoreType::Land, strings(&["Dryad"])),
    ] {
        let db = load_db().expect("integration fixture");
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let arbor = scenario.add_real_card(P0, "Dryad Arbor", Zone::Battlefield, db);
        let mut runner = build(scenario);
        install_registry(&mut runner, &["Dryad"]);
        refresh(&mut runner);
        assert!(has_core(&runner, arbor, CoreType::Land), "reach");
        assert!(has_core(&runner, arbor, CoreType::Creature), "reach");
        assert_eq!(subtypes(&runner, arbor).len(), 2, "reach: Forest Dryad");

        runner.state_mut().add_transient_continuous_effect(
            arbor,
            P0,
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: arbor },
            vec![ContinuousModification::RemoveType { core_type: removed }],
            None,
        );
        evaluate_layers(runner.state_mut());

        assert!(!has_core(&runner, arbor, removed), "reach: type removed");
        assert_eq!(subtypes(&runner, arbor), expected, "removed {removed:?}");
    }
}

/// CR 205.1a + CR 308.2: a Kindred artifact keeps its creature types when its
/// Creature type is removed (over-removal guard).
#[test]
fn kindred_artifact_keeps_creature_types_when_its_creature_type_is_removed() {
    let db = load_db().expect("integration fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let idol = scenario.add_real_card(P0, "Idol of False Gods", Zone::Battlefield, db);
    let mut runner = build(scenario);
    install_registry(&mut runner, &["Eldrazi"]);
    runner
        .state_mut()
        .objects
        .get_mut(&idol)
        .unwrap()
        .counters
        .insert(CounterType::Plus1Plus1, 8);
    refresh(&mut runner);
    assert!(
        has_core(&runner, idol, CoreType::Creature),
        "reach: animated"
    );
    assert!(has_core(&runner, idol, CoreType::Kindred), "reach: Kindred");

    runner.state_mut().add_transient_continuous_effect(
        idol,
        P0,
        Duration::UntilEndOfTurn,
        TargetFilter::SpecificObject { id: idol },
        vec![ContinuousModification::RemoveType {
            core_type: CoreType::Creature,
        }],
        None,
    );
    evaluate_layers(runner.state_mut());

    assert!(!has_core(&runner, idol, CoreType::Creature));
    assert!(subtypes(&runner, idol).iter().any(|s| s == "Eldrazi"));
}

// ---------------------------------------------------------------------------
// Changeling (CR 702.73a + CR 205.3d)
// ---------------------------------------------------------------------------

#[test]
fn changeling_one_with_stars_does_not_regain_creature_types() {
    // Verbatim MTGJSON Oracle for both inline scenario cards.
    const AMOEBOID: &str = "Changeling (This card is every creature type.)\n{T}: Target creature gains all creature types until end of turn.\n{T}: Target creature loses all creature types until end of turn.";
    const STARS: &str = "Enchant creature or enchantment\nEnchanted permanent is an enchantment and loses all other card types. (It still has its abilities, but it's no longer a creature.)";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let changeling = scenario
        .add_creature(P0, "Amoeboid Changeling", 1, 1)
        .with_subtypes(vec!["Shapeshifter"])
        .from_oracle_text_with_keywords(&["Changeling"], AMOEBOID)
        .id();
    let sibling = scenario
        .add_creature(P1, "Amoeboid Changeling", 1, 1)
        .with_subtypes(vec!["Shapeshifter"])
        .from_oracle_text_with_keywords(&["Changeling"], AMOEBOID)
        .id();
    let aura = scenario
        .add_spell_to_hand_from_oracle(P0, "One with the Stars", false, STARS)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(cost(3, vec![ManaCostShard::Blue]))
        .from_oracle_text_with_keywords(&["enchant"], STARS)
        .id();
    let mut runner = scenario.build();
    install_registry(&mut runner, &["Bear", "Elf"]);
    refresh(&mut runner);
    // CR 303.4a: the parsed enchant union supplies the Aura spell's required target.
    assert!(
        runner.state().objects[&aura].keywords.iter().any(|keyword| {
            matches!(keyword, Keyword::Enchant(TargetFilter::Or { filters }) if filters.len() == 2)
        }),
        "reach guard: the Aura must retain its parsed creature-or-enchantment restriction"
    );
    for id in [changeling, sibling] {
        assert!(runner.state().objects[&id].has_keyword(&Keyword::Changeling));
        assert!(subtypes(&runner, id).contains(&"Bear".to_string()));
        assert!(subtypes(&runner, id).contains(&"Elf".to_string()));
    }
    add_mana(&mut runner, ManaType::Blue, 4);
    let outcome = runner.cast(aura).target_object(changeling).resolve();
    outcome.assert_zone(&[aura], Zone::Battlefield);
    assert_eq!(
        outcome.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(changeling))
    );
    assert_eq!(
        outcome.state().objects[&changeling].card_types.core_types,
        vec![CoreType::Enchantment]
    );
    assert!(outcome.state().objects[&changeling].has_keyword(&Keyword::Changeling));
    assert!(outcome.state().objects[&changeling]
        .card_types
        .subtypes
        .is_empty());
    drop(outcome);
    assert_eq!(runner.state().objects[&sibling].controller, P1);
    assert!(has_core(&runner, sibling, CoreType::Creature));
    for ty in ["Bear", "Elf"] {
        assert!(subtypes(&runner, sibling).contains(&ty.to_string()));
    }

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), aura, Zone::Graveyard, &mut events);
    refresh(&mut runner);
    assert!(has_core(&runner, changeling, CoreType::Creature));
    for id in [changeling, sibling] {
        for ty in ["Bear", "Elf"] {
            assert!(subtypes(&runner, id).contains(&ty.to_string()));
        }
    }
}

// ---------------------------------------------------------------------------
// Bestow (CR 702.103b/e/f/g + CR 205.1a), real Springheart Nantuko / Boon Satyr
// ---------------------------------------------------------------------------

struct BestowBoard {
    runner: GameRunner,
    springheart: ObjectId,
    satyr: ObjectId,
    springheart_host: ObjectId,
    satyr_host: ObjectId,
}

fn bestow_board(springheart_zone: Zone, satyr_zone: Zone) -> BestowBoard {
    let db = load_db().expect("integration fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let springheart = scenario.add_real_card(P0, "Springheart Nantuko", springheart_zone, db);
    let satyr = scenario.add_real_card(P0, "Boon Satyr", satyr_zone, db);
    let springheart_host = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let satyr_host = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let mut runner = build(scenario);
    install_registry(&mut runner, &["Insect", "Monk", "Satyr"]);
    BestowBoard {
        runner,
        springheart,
        satyr,
        springheart_host,
        satyr_host,
    }
}

fn payload(runner: &GameRunner, id: ObjectId) -> Option<Vec<String>> {
    runner.state().objects[&id]
        .bestow_form
        .as_ref()
        .map(|form| form.removed_creature_subtypes.clone())
}

/// CR 702.103b/f + CR 205.1a: full-state transport retains each real cast's
/// own restoration list through rehydration and independent unattachment.
#[test]
fn bestow_populated_game_state_roundtrip_preserves_independent_restore_payloads() {
    let db = load_db().expect("integration fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let springheart = scenario.add_real_card(P0, "Springheart Nantuko", Zone::Hand, db);
    let satyr = scenario.add_real_card(P0, "Boon Satyr", Zone::Hand, db);
    let springheart_host = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let satyr_host = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let plain = scenario.add_real_card(P1, "Springheart Nantuko", Zone::Library, db);
    let mut board = BestowBoard {
        runner: build(scenario),
        springheart,
        satyr,
        springheart_host,
        satyr_host,
    };
    install_registry(&mut board.runner, &["Insect", "Monk", "Satyr"]);
    add_mana(&mut board.runner, ManaType::Green, 7);
    for (id, host, expected) in [
        (
            board.springheart,
            board.springheart_host,
            strings(&["Insect", "Monk"]),
        ),
        (board.satyr, board.satyr_host, strings(&["Satyr"])),
    ] {
        let outcome = board
            .runner
            .cast(id)
            .alternative_cast(AlternativeCastDecision::Alternative)
            .target_object(host)
            .resolve();
        outcome.assert_zone(&[id], Zone::Battlefield);
        let obj = &outcome.state().objects[&id];
        assert_eq!(obj.attached_to, Some(AttachTarget::Object(host)));
        assert_eq!(obj.card_types.core_types, vec![CoreType::Enchantment]);
        assert_eq!(obj.card_types.subtypes, strings(&["Aura"]));
        assert_eq!(
            obj.bestow_form.as_ref().unwrap().removed_creature_subtypes,
            expected
        );
    }
    assert_ne!(board.springheart, board.satyr);
    assert!(
        board.runner.state().stack.is_empty(),
        "reach: both casts resolved"
    );
    let serialized =
        serde_json::to_string(board.runner.state()).expect("full GameState serializes");
    let restored = serde_json::from_str(&serialized).expect("full GameState deserializes");
    let mut runner = GameRunner::from_state(restored);
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    refresh(&mut runner);
    for (id, host, expected) in [
        (
            board.springheart,
            board.springheart_host,
            strings(&["Insect", "Monk"]),
        ),
        (board.satyr, board.satyr_host, strings(&["Satyr"])),
    ] {
        let obj = &runner.state().objects[&id];
        assert_eq!(obj.id, id);
        assert_eq!((obj.owner, obj.controller), (P0, P0));
        assert_eq!(obj.attached_to, Some(AttachTarget::Object(host)));
        assert_eq!(obj.card_types.core_types, vec![CoreType::Enchantment]);
        assert_eq!(obj.card_types.subtypes, strings(&["Aura"]));
        assert_eq!(payload(&runner, id), Some(expected));
    }
    let neighbor = &runner.state().objects[&plain];
    assert_eq!(
        (neighbor.zone, neighbor.owner, neighbor.controller),
        (Zone::Library, P1, P1)
    );
    assert!(neighbor.bestow_form.is_none());
    assert_eq!(
        neighbor.card_types.core_types,
        vec![CoreType::Enchantment, CoreType::Creature]
    );
    assert_eq!(neighbor.card_types.subtypes, strings(&["Insect", "Monk"]));
    assert!(runner.state().stack.is_empty());
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        board.springheart_host,
        Zone::Graveyard,
        &mut events,
    );
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);
    refresh(&mut runner);
    assert_eq!(payload(&runner, board.springheart), None);
    assert!(has_core(&runner, board.springheart, CoreType::Enchantment));
    assert!(has_core(&runner, board.springheart, CoreType::Creature));
    assert_eq!(
        subtypes(&runner, board.springheart),
        strings(&["Insect", "Monk"])
    );
    assert_eq!(payload(&runner, board.satyr), Some(strings(&["Satyr"])));
    assert_eq!(
        runner.state().objects[&board.satyr].attached_to,
        Some(AttachTarget::Object(board.satyr_host))
    );
    assert_eq!(subtypes(&runner, board.satyr), strings(&["Aura"]));
    assert!(!has_core(&runner, board.satyr, CoreType::Creature));
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        board.satyr_host,
        Zone::Graveyard,
        &mut events,
    );
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);
    refresh(&mut runner);
    assert_eq!(payload(&runner, board.satyr), None);
    assert!(has_core(&runner, board.satyr, CoreType::Enchantment));
    assert!(has_core(&runner, board.satyr, CoreType::Creature));
    assert_eq!(subtypes(&runner, board.satyr), strings(&["Satyr"]));
    assert_eq!(
        subtypes(&runner, board.springheart),
        strings(&["Insect", "Monk"])
    );
}

/// CR 702.103b + CR 205.1a: cast bestowed, Springheart is an Aura enchantment on
/// the stack with no creature types.
#[test]
fn springheart_cast_bestowed_is_aura_with_no_creature_types_on_the_stack() {
    let mut board = bestow_board(Zone::Hand, Zone::Library);
    add_mana(&mut board.runner, ManaType::Green, 2);

    let cast = board
        .runner
        .cast(board.springheart)
        .alternative_cast(AlternativeCastDecision::Alternative)
        .target_object(board.springheart_host)
        .commit();
    let obj = &cast.state().objects[&board.springheart];

    assert_eq!(obj.zone, Zone::Stack, "reach: on the stack");
    assert_eq!(
        obj.bestow_form,
        Some(BestowFormState {
            removed_creature_subtypes: strings(&["Insect", "Monk"]),
        }),
        "reach: bestowed, payload recorded"
    );
    assert_eq!(obj.card_types.core_types, vec![CoreType::Enchantment]);
    assert_eq!(obj.card_types.subtypes, strings(&["Aura"]));
}

/// CR 702.103b/f: resolved, the bestowed Aura has no creature types; when its host
/// leaves and state-based actions unattach it, it is a creature again with its
/// printed types in printed order.
#[test]
fn springheart_resolves_as_bestowed_aura_without_creature_types_and_loses_none_permanently() {
    let mut board = bestow_board(Zone::Hand, Zone::Library);
    add_mana(&mut board.runner, ManaType::Green, 2);
    board
        .runner
        .cast(board.springheart)
        .alternative_cast(AlternativeCastDecision::Alternative)
        .target_object(board.springheart_host)
        .resolve();
    let runner = &mut board.runner;
    let sh = board.springheart;

    assert_eq!(runner.state().objects[&sh].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&sh].attached_to,
        Some(AttachTarget::Object(board.springheart_host)),
        "reach: attached to the host"
    );
    assert!(!has_core(runner, sh, CoreType::Creature));
    assert_eq!(subtypes(runner, sh), strings(&["Aura"]));

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(
        runner.state_mut(),
        board.springheart_host,
        Zone::Graveyard,
        &mut events,
    );
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);
    refresh(runner);

    assert_eq!(runner.state().objects[&sh].zone, Zone::Battlefield);
    assert!(
        runner.state().objects[&sh].bestow_form.is_none(),
        "CR 702.103f: ceased to be bestowed"
    );
    assert!(has_core(runner, sh, CoreType::Creature));
    assert_eq!(subtypes(runner, sh), strings(&["Insect", "Monk"]));
}

/// CR 702.103e: with its target illegal as it begins resolving, the spell stops
/// being bestowed and resolves as a creature with its creature types.
#[test]
fn springheart_bestow_illegal_target_at_resolution_regains_creature_types() {
    let mut board = bestow_board(Zone::Hand, Zone::Library);
    add_mana(&mut board.runner, ManaType::Green, 2);
    let cast = board
        .runner
        .cast(board.springheart)
        .alternative_cast(AlternativeCastDecision::Alternative)
        .target_object(board.springheart_host)
        .commit();
    assert_eq!(
        cast.state().objects[&board.springheart].card_types.subtypes,
        strings(&["Aura"]),
        "reach: bestowed on the stack"
    );
    drop(cast);

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(
        board.runner.state_mut(),
        board.springheart_host,
        Zone::Graveyard,
        &mut events,
    );
    board.runner.advance_until_stack_empty();

    let runner = &board.runner;
    let sh = board.springheart;
    assert_eq!(runner.state().objects[&sh].zone, Zone::Battlefield);
    assert!(runner.state().objects[&sh].bestow_form.is_none());
    assert!(has_core(runner, sh, CoreType::Creature));
    let types = subtypes(runner, sh);
    assert!(types.iter().any(|s| s == "Insect") && types.iter().any(|s| s == "Monk"));
    assert!(!types.iter().any(|s| s == "Aura"));
}

/// Rehydration from card data re-applies the form (`apply_bestow_aura_form` runs a
/// second time on an already-bestowed object) without losing the record: the
/// payload is the union, never overwritten.
#[test]
fn springheart_bestow_survives_card_data_rehydration_without_regaining_creature_types() {
    let db = load_db().expect("integration fixture");
    let mut board = bestow_board(Zone::Battlefield, Zone::Library);
    let sh = board.springheart;
    board
        .runner
        .attach_as_bestowed_aura(sh, board.springheart_host);
    assert_eq!(subtypes(&board.runner, sh), strings(&["Aura"]), "reach");
    assert_eq!(
        payload(&board.runner, sh),
        Some(strings(&["Insect", "Monk"])),
        "reach: payload recorded"
    );

    engine::game::rehydrate_game_from_card_db(board.runner.state_mut(), db);
    refresh(&mut board.runner);
    assert_eq!(subtypes(&board.runner, sh), strings(&["Aura"]));
    assert_eq!(
        payload(&board.runner, sh),
        Some(strings(&["Insect", "Monk"]))
    );

    // CR 702.103f: the host leaves, state-based actions unattach the Aura, and
    // the payload restores the printed creature types.
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(
        board.runner.state_mut(),
        board.springheart_host,
        Zone::Graveyard,
        &mut events,
    );
    engine::game::sba::check_state_based_actions(board.runner.state_mut(), &mut events);
    refresh(&mut board.runner);
    assert!(
        payload(&board.runner, sh).is_none(),
        "ceased to be bestowed"
    );
    assert!(has_core(&board.runner, sh, CoreType::Creature));
    assert_eq!(subtypes(&board.runner, sh), strings(&["Insect", "Monk"]));
}

/// CR 400.7: a bestowed Aura that leaves the battlefield is a new object, back to
/// its printed creature face. Two bestow cards restore only their own lists; a
/// normal Springheart keeps its types (negative sibling).
#[test]
fn springheart_bestow_zone_exit_restores_printed_creature_face() {
    let mut board = bestow_board(Zone::Battlefield, Zone::Battlefield);
    let (sh, satyr) = (board.springheart, board.satyr);
    board
        .runner
        .attach_as_bestowed_aura(sh, board.springheart_host);
    board
        .runner
        .attach_as_bestowed_aura(satyr, board.satyr_host);
    refresh(&mut board.runner);
    assert_eq!(subtypes(&board.runner, sh), strings(&["Aura"]), "reach");
    assert_eq!(subtypes(&board.runner, satyr), strings(&["Aura"]), "reach");
    assert_eq!(
        payload(&board.runner, sh),
        Some(strings(&["Insect", "Monk"]))
    );
    assert_eq!(payload(&board.runner, satyr), Some(strings(&["Satyr"])));

    let mut events = Vec::new();
    for id in [sh, satyr] {
        engine::game::zones::move_to_zone(
            board.runner.state_mut(),
            id,
            Zone::Graveyard,
            &mut events,
        );
    }

    let runner = &board.runner;
    for (id, expected) in [
        (sh, strings(&["Insect", "Monk"])),
        (satyr, strings(&["Satyr"])),
    ] {
        let obj = &runner.state().objects[&id];
        assert_eq!(obj.zone, Zone::Graveyard);
        assert!(obj.bestow_form.is_none());
        assert!(obj.card_types.core_types.contains(&CoreType::Creature));
        assert_eq!(obj.card_types.subtypes, expected);
    }

    // Negative sibling: an unbestowed Springheart keeps Insect Monk and Creature.
    let plain = bestow_board(Zone::Battlefield, Zone::Library);
    let mut runner = plain.runner;
    refresh(&mut runner);
    assert!(runner.state().objects[&plain.springheart]
        .bestow_form
        .is_none());
    assert!(has_core(&runner, plain.springheart, CoreType::Creature));
    assert_eq!(
        subtypes(&runner, plain.springheart),
        strings(&["Insect", "Monk"])
    );
}
