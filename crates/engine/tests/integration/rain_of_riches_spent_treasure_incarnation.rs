//! CR 106.3 + CR 113.7 + CR 400.7 + CR 601.2h: a spent mana unit stays bound to
//! the source incarnation that PRODUCED it, even when paying the producing
//! ability's own cost moved that source out of the battlefield.
//!
//! Oracle (verified against Scryfall, `otc` 177 / `clb` 34 / `stx` 1):
//!
//! * **Rain of Riches** `{3}{R}{R}` Enchantment — "When this enchantment
//!   enters, create two Treasure tokens. / The first spell you cast each turn
//!   that mana from a Treasure was spent to cast has cascade."
//! * **Minimus Containment** `{2}{W}` Enchantment — Aura — "Enchant nonland
//!   permanent / Enchanted permanent is a Treasure artifact with "{T},
//!   Sacrifice this artifact: Add one mana of any color," and it loses all
//!   other abilities."
//! * **Environmental Sciences** `{2}` Sorcery — Lesson — "Search your library
//!   for a basic land card, reveal it, put it into your hand, then shuffle. You
//!   gain 2 life."
//!
//! The board is exactly the maintainer's repro: Rain of Riches on the
//! battlefield, a nontoken **Grizzly Bears** enchanted by Minimus Containment,
//! its granted mana ability activated and the Bears sacrificed as that
//! ability's own cost. CR 113.7 makes the permanent that generated the ability
//! the source of the mana (CR 106.3), so the unit's source is the Bear
//! *as a Treasure artifact* — the incarnation that existed when the ability was
//! activated, before the sacrifice cost moved it. CR 400.7: the card in the
//! graveyard is a different object, so the payment-time capture must not
//! re-resolve the storage id against the live object map and record a graveyard
//! Bear.
//!
//! The test reads the fact through every reader the property has, all of them
//! downstream of the real pipeline (no synthetic stamp):
//!
//! 1. the live cast object's `mana_spent_source_snapshots` (CR 601.2h),
//! 2. the turn's `SpellCastRecord` history (`mana_source_snapshots`, CR 601.2i),
//! 3. Rain of Riches' own `CastWithKeyword { Cascade }` grant, whose
//!    first-qualifying-spell gate reads that record through
//!    `FilterProp::ManaSpentFromSource`,
//! 4. the Cascade trigger the granted keyword actually enqueues off the cast.

use std::sync::Arc;

use engine::game::game_object::AttachTarget;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{ContinuousModification, Effect, StaticDefinition};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{
    CastPaymentMode, ManaChoice, SpellCastRecord, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const RAIN_OF_RICHES: &str = "When this enchantment enters, create two Treasure tokens.\nThe first spell you cast each turn that mana from a Treasure was spent to cast has cascade.";
const MINIMUS_TYPE_CHANGE: &str = "Enchanted permanent is a Treasure artifact with \"{T}, Sacrifice this artifact: Add one mana of any color,\" and it loses all other abilities.";
const ENVIRONMENTAL_SCIENCES: &str = "Search your library for a basic land card, reveal it, put it into your hand, then shuffle. You gain 2 life.";
const BEARS_NAME: &str = "Grizzly Bears";

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// Minimus Containment's type-change static, parsed from its own Oracle text
/// rather than hand-built: the parser is the authority for what the Aura does,
/// and `minimus_containment_treasure_grant` already pins that it yields the
/// `GrantAbility` + `RemoveAllAbilities` shape.
fn minimus_type_change() -> StaticDefinition {
    parse_oracle_text(
        MINIMUS_TYPE_CHANGE,
        "Minimus Containment",
        &[],
        &["Enchantment".to_string()],
        &["Aura".to_string()],
    )
    .statics
    .into_iter()
    .find(|static_def| {
        static_def
            .modifications
            .iter()
            .any(|modification| matches!(modification, ContinuousModification::GrantAbility { .. }))
    })
    .expect("Minimus Containment must parse to a GrantAbility type-change static")
}

struct Board {
    runner: GameRunner,
    rain: ObjectId,
    bears: ObjectId,
    forest: ObjectId,
    sciences: ObjectId,
}

/// PreCombatMain, P0 active, stack empty: the sorcery-speed window
/// Environmental Sciences (`{2}` sorcery, CR 307.1) needs.
fn board() -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest"]);
    let rain = scenario
        .add_enchantment_from_oracle(P0, "Rain of Riches", RAIN_OF_RICHES)
        .id();
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let bears = scenario.add_creature(P0, BEARS_NAME, 2, 2).id();
    let aura = scenario.add_creature(P0, "Minimus Containment", 0, 0).id();
    let sciences = {
        let mut builder = scenario.add_spell_to_hand_from_oracle(
            P0,
            "Environmental Sciences",
            false,
            ENVIRONMENTAL_SCIENCES,
        );
        builder.with_mana_cost(ManaCost::generic(2));
        builder.id()
    };

    let mut runner = scenario.build();
    let type_change = minimus_type_change();
    {
        let state = runner.state_mut();
        let aura_obj = state.objects.get_mut(&aura).unwrap();
        aura_obj.card_types.core_types = vec![CoreType::Enchantment];
        aura_obj.card_types.subtypes = vec!["Aura".to_string()];
        aura_obj.base_card_types = aura_obj.card_types.clone();
        aura_obj.power = None;
        aura_obj.toughness = None;
        aura_obj.base_power = None;
        aura_obj.base_toughness = None;
        aura_obj.attached_to = Some(AttachTarget::Object(bears));
        aura_obj.static_definitions.push(type_change.clone());
        Arc::make_mut(&mut aura_obj.base_static_definitions).push(type_change);
        state.layers_dirty.mark_full();
    }
    evaluate_layers(runner.state_mut());

    Board {
        runner,
        rain,
        bears,
        forest,
        sciences,
    }
}

fn pool_units(runner: &GameRunner) -> Vec<ManaUnit> {
    runner.state().players[P0.0 as usize]
        .mana_pool
        .units()
        .collect()
}

/// Activate a mana ability through the real activation driver, answering the
/// colour prompt it opens (`ManaProduction::ChosenColor` for "one mana of any
/// color"). Mana abilities do not use the stack (CR 605.3a), so the driver
/// returns with the production already done.
fn activate_mana_ability(runner: &mut GameRunner, source: ObjectId, color: ManaType) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activating the mana ability must be accepted");
    match runner.state().waiting_for.clone() {
        WaitingFor::ChooseManaColor { .. } => {
            runner
                .act(GameAction::ChooseManaColor {
                    choice: ManaChoice::SingleColor(color),
                    count: 1,
                })
                .expect("the colour answer must be accepted");
        }
        // A single fixed colour ("{T}: Add {G}.") needs no prompt.
        WaitingFor::Priority { .. } => {}
        other => panic!("unexpected state after activating a mana ability: {other:?}"),
    }
}

fn last_record(runner: &GameRunner) -> SpellCastRecord {
    runner
        .state()
        .spells_cast_this_turn_by_player
        .get(&P0)
        .and_then(|records| records.back())
        .cloned()
        .expect("the cast must be recorded in the turn-scoped history")
}

// ---------------------------------------------------------------------------
// The regression
// ---------------------------------------------------------------------------

/// The maintainer's repro, end to end: sacrifice the enchanted Bears for mana
/// and spend that mana on Environmental Sciences as the turn's first qualifying
/// spell. The spent-mana snapshot must describe the **Treasure artifact** that
/// produced the mana, and Rain of Riches must therefore grant cascade.
#[test]
fn spent_treasure_mana_records_the_producing_treasure_not_the_graveyard_bear() {
    let Board {
        mut runner,
        rain,
        bears,
        forest,
        sciences,
    } = board();

    // --- Precondition: Rain of Riches parsed to a GATED cast-keyword grant, so
    // the cascade assertions below test the first-qualifying-spell gate and not
    // an unconditional keyword.
    {
        let rain_obj = &runner.state().objects[&rain];
        let gated = rain_obj.static_definitions.iter_all().any(|static_def| {
            matches!(static_def.mode, StaticMode::CastWithKeyword { .. })
                && static_def.condition.is_some()
        });
        assert!(
            gated,
            "reach: Rain of Riches' static must be a conditioned CastWithKeyword grant, got {:?}",
            rain_obj
                .static_definitions
                .iter_all()
                .map(|static_def| &static_def.mode)
                .collect::<Vec<_>>()
        );
    }

    // --- Precondition: Minimus Containment made the Bear a Treasure artifact
    // with exactly one ability (CR 205.1a sets the type, CR 613.1d Layer 4 applies it, and
    // CR 613.1f Layer 6 removes the Bear's own abilities).
    {
        let bear = &runner.state().objects[&bears];
        assert!(
            bear.card_types.core_types.contains(&CoreType::Artifact),
            "reach: the Bear must be a Treasure artifact, got {:?}",
            bear.card_types.core_types
        );
        assert!(
            bear.card_types.subtypes.iter().any(|s| s == "Treasure"),
            "reach: the Bear must carry the Treasure subtype, got {:?}",
            bear.card_types.subtypes
        );
        assert!(
            !bear.is_token,
            "reach: the Bears must be a nontoken permanent"
        );
        assert_eq!(
            bear.abilities.len(),
            1,
            "reach: the wipe plus the grant must leave exactly one ability"
        );
    }
    let producing_incarnation = runner.state().objects[&bears].incarnation;

    // --- Act 1: sacrifice the Bears for mana, through the granted ability.
    activate_mana_ability(&mut runner, bears, ManaType::Green);

    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Graveyard,
        "CR 701.21a: the self-sacrifice cost moved the Bears to its owner's graveyard"
    );
    assert_ne!(
        runner.state().objects[&bears].incarnation,
        producing_incarnation,
        "CR 400.7: the graveyard card is a new object, so the live id no longer \
         names the incarnation that produced the mana"
    );
    let treasure_units = pool_units(&runner);
    assert_eq!(
        treasure_units.len(),
        1,
        "reach: the Treasure ability must have produced exactly one unit"
    );
    assert_eq!(
        treasure_units[0].source_id, bears,
        "CR 106.3 + CR 113.7: the mana's source is the permanent that generated the ability"
    );
    assert_eq!(
        treasure_units[0].source_incarnation,
        Some(producing_incarnation),
        "CR 400.7: production must pin the producing incarnation, not re-resolve \
         the storage id at payment time"
    );

    // --- Act 2: one more mana, from a plain Forest, so `{2}` is payable.
    activate_mana_ability(&mut runner, forest, ManaType::Green);
    assert_eq!(pool_units(&runner).len(), 2, "reach: two units fund {{2}}");

    // --- Act 3: cast the turn's first spell with that mana.
    {
        let commit = runner.cast(sciences).commit();

        // (1) The live cast object's payment-time capture (CR 601.2h).
        let snapshots = &commit.state().objects[&sciences].mana_spent_source_snapshots;
        assert_eq!(
            snapshots.len(),
            2,
            "each of the two spent units must stamp a source snapshot"
        );
        let treasure = snapshots
            .iter()
            .find(|snapshot| snapshot.source_id == bears)
            .unwrap_or_else(|| {
                panic!(
                    "the snapshot list must name the sacrificed Bear's storage id: {snapshots:?}"
                )
            });
        assert!(
            treasure.lki.subtypes.iter().any(|s| s == "Treasure"),
            "CR 106.3 + CR 400.7: the spent Treasure unit must snapshot the source's \
             battlefield characteristics — a Treasure artifact — not the graveyard \
             card. Recorded subtypes: {:?}",
            treasure.lki.subtypes
        );
        assert!(
            treasure.lki.card_types.contains(&CoreType::Artifact),
            "the snapshot must record an Artifact, got {:?}",
            treasure.lki.card_types
        );
        assert!(
            !treasure.lki.card_types.contains(&CoreType::Creature),
            "Minimus Containment removed the creature type (CR 205.1a); the snapshot \
             must not resurrect it. Got {:?}",
            treasure.lki.card_types
        );

        // The paired control: the Forest's unit records the Forest, so the
        // Treasure answer above is not a blanket "any source matches".
        let land = snapshots
            .iter()
            .find(|snapshot| snapshot.source_id == forest)
            .expect("the Forest unit must snapshot the Forest");
        assert!(
            land.lki.subtypes.iter().any(|s| s == "Forest"),
            "the Forest unit must record the Forest, got {:?}",
            land.lki.subtypes
        );

        // (2) The turn history projects the same capture (CR 601.2i).
        let record = commit.state().spells_cast_this_turn_by_player[&P0]
            .back()
            .cloned()
            .expect("the cast must be recorded");
        assert!(
            record
                .mana_source_snapshots()
                .iter()
                .any(|snapshot| snapshot.source_id == bears
                    && snapshot.lki.subtypes.iter().any(|s| s == "Treasure")),
            "the cast-history record must carry the Treasure snapshot, got {:?}",
            record.mana_source_snapshots()
        );

        // (3) Rain of Riches' first-spell predicate sees it, so the granted
        // keyword lands on the cast-time snapshot.
        let keywords = &commit.state().objects[&sciences].cast_spell_keywords;
        assert!(
            keywords.iter().any(|k| matches!(k, Keyword::Cascade)),
            "CR 702.85a + CR 611.3b: the first spell whose mana came from a Treasure \
             must be granted cascade by Rain of Riches; got {keywords:?}"
        );

        // (4) …and the granted keyword actually enqueued its Cascade trigger.
        let cascade_triggers = commit
            .state()
            .stack
            .iter()
            .filter(|entry| {
                entry.source_id == sciences
                    && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
                    && entry
                        .ability()
                        .is_some_and(|ability| matches!(ability.effect, Effect::Cascade))
            })
            .count();
        assert_eq!(
            cascade_triggers, 1,
            "the granted cascade must enqueue exactly one Cascade trigger off the cast"
        );
    }

    // Rain of Riches is the grant's source; a reach guard that it is still the
    // battlefield permanent the engine consulted.
    assert_eq!(
        runner.state().objects[&rain].zone,
        Zone::Battlefield,
        "reach: Rain of Riches is on the battlefield"
    );
    assert!(last_record(&runner).name.contains("Environmental"));
}
