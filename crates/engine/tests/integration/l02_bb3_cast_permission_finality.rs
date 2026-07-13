//! L02 BB3 — cast-permission → enters-with-finality-counter (3 Standard cards).
//!
//! Cards (Oracle text verified verbatim vs Scryfall):
//!   * Noctis, Prince of Lucis (FIN 235) — graveyard cast permission with an
//!     ADDITIONAL 3-life cost + finality enters-with rider.
//!   * Intrepid Paleontologist (LCI 193) — exile cast permission (owner=You,
//!     Dinosaur creature) + finality; pool auto-populated by the {2} exile.
//!   * Leonardo, Sewer Samurai (TMT 17) — graveyard cast permission gated to
//!     the controller's turn (P/T ≤ 1 creatures) + finality.
//!
//! Blast-radius watch: Dawnhand Dissident (Lorwyn Eclipsed 98) must stay a clean
//! gap; Festival of Embers must keep its additional-cost parse unchanged.
//!
//! Parse tests drive `parse_oracle_text`; runtime tests drive the real cast /
//! activation pipeline (`GameRunner::cast(..).resolve()` / `activate(..)`).
//! Finality assertions here check counter PLACEMENT; CR 122.1h death→exile is
//! enforced and covered separately.

use engine::game::casting::{can_cast_object_now, spell_objects_available_to_cast};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCost, CardPlayMode, ControllerRef, FilterProp, QuantityExpr, StaticCondition,
    StaticDefinition, TargetFilter,
};
use engine::types::counter::CounterType;
use engine::types::game_state::ExileLinkKind;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::{CastCostMode, CastExtraCost, ExileCardPool, StaticMode};
use engine::types::zones::Zone;

// ── Verbatim current Oracle text ─────────────────────────────────────────────

const NOCTIS_PERMISSION: &str = "Lifelink\nYou may cast artifact spells from your graveyard by paying 3 life in addition to paying their other costs. If you cast a spell this way, that artifact enters with a finality counter on it.";

const INTREPID_PERMISSION: &str = "You may cast Dinosaur creature spells from among cards you own exiled with this creature. If you cast a spell this way, that creature enters with a finality counter on it. (If a creature with a finality counter on it would die, exile it instead.)";

const INTREPID_EXILE_AND_PERMISSION: &str = "{2}: Exile target card from a graveyard.\nYou may cast Dinosaur creature spells from among cards you own exiled with this creature. If you cast a spell this way, that creature enters with a finality counter on it.";

const LEONARDO_PERMISSION: &str = "During your turn, you may cast creature spells with power or toughness 1 or less from your graveyard. If you cast a spell this way, that creature enters with a finality counter on it. (If a creature with a finality counter on it would die, exile it instead.)";

const DAWNHAND_PERMISSION: &str = "During your turn, you may cast creature spells from among cards you own exiled with this creature by removing three counters from among creatures you control in addition to paying their other costs.";

const FESTIVAL_PERMISSION: &str = "During your turn, you may cast instant and sorcery spells from your graveyard by paying 1 life in addition to their other costs.";

fn finality() -> CounterType {
    CounterType::Finality
}

fn pool_units(colors: &[ManaType]) -> Vec<ManaUnit> {
    let dummy = engine::types::identifiers::ObjectId(0);
    colors
        .iter()
        .map(|&color| ManaUnit::new(color, dummy, false, vec![]))
        .collect()
}

fn has_swallow(parsed: &engine::parser::oracle::ParsedAbilities, detector: &str) -> bool {
    parsed.parse_warnings.iter().any(|w| {
        format!("{w:?}").contains("SwallowedClause") && format!("{w:?}").contains(detector)
    })
}

fn permission_static(
    parsed: &engine::parser::oracle::ParsedAbilities,
) -> Option<&StaticDefinition> {
    parsed.statics.iter().find(|s| {
        matches!(
            s.mode,
            StaticMode::GraveyardCastPermission { .. } | StaticMode::ExileCastPermission { .. }
        )
    })
}

fn permission_counter(mode: &StaticMode) -> &Option<CounterType> {
    match mode {
        StaticMode::GraveyardCastPermission {
            enters_with_counter,
            ..
        }
        | StaticMode::ExileCastPermission {
            enters_with_counter,
            ..
        } => enters_with_counter,
        other => panic!("expected a cast permission, got {other:?}"),
    }
}

// ── Parse fidelity ───────────────────────────────────────────────────────────

/// Noctis parse: `GraveyardCastPermission` with `extra_cost = PayLife{3}
/// Additional` AND `enters_with_counter = Some(finality)`, and NO Condition_If.
/// Revert-probes: (a) drop `opt("paying ")` → extra_cost == None; (b) drop the
/// finality recognizer → enters_with_counter == None AND Condition_If reappears.
#[test]
fn noctis_parse_extra_cost_and_finality() {
    let parsed = parse_oracle_text(
        NOCTIS_PERMISSION,
        "Noctis, Prince of Lucis",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let def = permission_static(&parsed).expect("Noctis must emit a cast permission static");
    let StaticMode::GraveyardCastPermission {
        ref extra_cost,
        ref enters_with_counter,
        ..
    } = def.mode
    else {
        panic!("expected GraveyardCastPermission, got {:?}", def.mode);
    };
    // reach-guard + (a) 3-life additional cost captured (opt("paying ") fix).
    assert_eq!(
        extra_cost,
        &Some(CastExtraCost {
            cost: AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 3 },
            },
            mode: CastCostMode::Additional,
        }),
        "Noctis must capture the 3-life ADDITIONAL cost"
    );
    // (b) finality rider rides the permission.
    assert_eq!(
        enters_with_counter,
        &Some(finality()),
        "Noctis's finality rider must ride the permission"
    );
    // The linked "if you cast a spell this way" rider must not be a swallowed
    // condition (paired positive above proves the arm was reached).
    assert!(
        !has_swallow(&parsed, "Condition_If"),
        "Noctis's finality rider must not be a swallowed Condition_If: {:?}",
        parsed.parse_warnings
    );
}

/// Intrepid parse: a single `ExileCastPermission { pool: Persistent, play_mode:
/// Cast, enters_with_counter: Some(finality) }` whose `affected` carries a
/// Dinosaur-creature + owner=You filter; NO garbage `PutCounter` replacement,
/// NO Optional_YouMay, NO Condition_If. Revert-probe: reverting the classifier
/// "you own" anchor re-routes it to the replacement misparse (both markers +
/// the garbage replacement reappear).
#[test]
fn intrepid_parse_exile_permission_owner_and_finality() {
    let parsed = parse_oracle_text(
        INTREPID_PERMISSION,
        "Intrepid Paleontologist",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Artificer".to_string()],
    );
    // No garbage replacement misparse (reach-guard for the classifier route fix).
    assert!(
        parsed.replacements.is_empty(),
        "Intrepid must not misparse into a replacement, got {:?}",
        parsed.replacements
    );
    let def = permission_static(&parsed).expect("Intrepid must emit an ExileCastPermission");
    let StaticMode::ExileCastPermission {
        pool,
        play_mode,
        ref enters_with_counter,
        ..
    } = def.mode
    else {
        panic!("expected ExileCastPermission, got {:?}", def.mode);
    };
    assert_eq!(pool, ExileCardPool::Persistent);
    assert_eq!(play_mode, CardPlayMode::Cast);
    assert_eq!(
        enters_with_counter,
        &Some(finality()),
        "Intrepid's finality rider must ride the permission"
    );
    // affected filter: Dinosaur + Creature + owner=You.
    let TargetFilter::Typed(tf) = def.affected.as_ref().expect("affected filter present") else {
        panic!("expected a Typed affected filter, got {:?}", def.affected);
    };
    assert!(
        tf.properties.contains(&FilterProp::Owned {
            controller: ControllerRef::You,
        }),
        "Intrepid's filter must carry the owner=You constraint, got {:?}",
        tf.properties
    );
    assert!(
        tf.get_subtype() == Some("Dinosaur"),
        "Intrepid's filter must constrain to Dinosaur, got {:?}",
        tf.type_filters
    );
    assert!(
        !has_swallow(&parsed, "Optional_YouMay"),
        "Intrepid's optional permission must not be swallowed: {:?}",
        parsed.parse_warnings
    );
    assert!(
        !has_swallow(&parsed, "Condition_If"),
        "Intrepid's finality rider must not be a swallowed Condition_If: {:?}",
        parsed.parse_warnings
    );
}

/// Leonardo parse: `GraveyardCastPermission { enters_with_counter:
/// Some(finality) }`, `condition == Some(DuringYourTurn)`, affected = creature
/// with P/T ≤ 1; NO garbage replacement, NO Optional_YouMay, NO Condition_If.
/// Revert-probe: reverting the classifier "during your turn, " qualifier re-routes
/// it to the replacement misparse.
#[test]
fn leonardo_parse_graveyard_permission_timing_and_finality() {
    let parsed = parse_oracle_text(
        LEONARDO_PERMISSION,
        "Leonardo, Sewer Samurai",
        &[],
        &["Creature".to_string()],
        &[],
    );
    assert!(
        parsed.replacements.is_empty(),
        "Leonardo must not misparse into a replacement, got {:?}",
        parsed.replacements
    );
    let def = permission_static(&parsed).expect("Leonardo must emit a GraveyardCastPermission");
    assert!(
        matches!(def.mode, StaticMode::GraveyardCastPermission { .. }),
        "expected GraveyardCastPermission, got {:?}",
        def.mode
    );
    assert_eq!(
        permission_counter(&def.mode),
        &Some(finality()),
        "Leonardo's finality rider must ride the permission"
    );
    assert_eq!(
        def.condition,
        Some(StaticCondition::DuringYourTurn),
        "Leonardo's permission must be gated to the controller's turn"
    );
    assert!(
        !has_swallow(&parsed, "Optional_YouMay"),
        "Leonardo's optional permission must not be swallowed: {:?}",
        parsed.parse_warnings
    );
    assert!(
        !has_swallow(&parsed, "Condition_If"),
        "Leonardo's finality rider must not be a swallowed Condition_If: {:?}",
        parsed.parse_warnings
    );
}

/// Class-level: the finality recognizer's "that artifact" subject arm (Noctis)
/// and "that creature" arm (Leonardo/Intrepid) both resolve to Generic(finality).
/// Since the subject grammar is a shared authority, exercising both card shapes
/// through the pipeline covers the arm added for Noctis. Revert-probe: removing
/// the "that artifact enters with " arm drops Noctis's counter (see
/// `noctis_parse_extra_cost_and_finality` enters_with_counter assertion).
#[test]
fn finality_recognizer_covers_artifact_and_creature_subjects() {
    for (text, name) in [
        (NOCTIS_PERMISSION, "Noctis, Prince of Lucis"),
        (LEONARDO_PERMISSION, "Leonardo, Sewer Samurai"),
    ] {
        let parsed = parse_oracle_text(text, name, &[], &["Creature".to_string()], &[]);
        let def = permission_static(&parsed).unwrap_or_else(|| panic!("{name} must parse"));
        assert_eq!(
            permission_counter(&def.mode),
            &Some(finality()),
            "{name} must resolve the finality counter"
        );
    }
}

/// Class-level: the `opt("paying ")` extra_cost fix tolerates Noctis's gerund
/// ("in addition to PAYING their other costs") while leaving Festival of Embers
/// ("in addition to their other costs") intact. Revert-probe: removing
/// `opt("paying ")` drops Noctis's extra_cost while Festival still parses.
#[test]
fn extra_cost_gerund_tolerance_leaves_festival_intact() {
    let festival = parse_oracle_text(
        FESTIVAL_PERMISSION,
        "Festival of Embers",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    let def = permission_static(&festival).expect("Festival must still parse");
    let StaticMode::GraveyardCastPermission {
        ref extra_cost,
        ref enters_with_counter,
        ..
    } = def.mode
    else {
        panic!("expected GraveyardCastPermission, got {:?}", def.mode);
    };
    assert_eq!(
        extra_cost,
        &Some(CastExtraCost {
            cost: AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 1 },
            },
            mode: CastCostMode::Additional,
        }),
        "Festival's 1-life additional cost must be unchanged"
    );
    assert_eq!(
        enters_with_counter, &None,
        "Festival carries no finality rider"
    );
}

/// Blast-radius (Nit 3): Dawnhand Dissident's counter-removal extra_cost is
/// unmodeled; the classifier route fix sends it to the exile builder, which must
/// DECLINE cleanly — Dawnhand stays a clean gap (no permission static, no
/// replacement misparse). Reach-guard: the sibling Intrepid (same classifier
/// route) DOES parse to a permission, proving the route is live and the decline
/// is not vacuous.
#[test]
fn dawnhand_dissident_stays_clean_gap() {
    let dawnhand = parse_oracle_text(
        DAWNHAND_PERMISSION,
        "Dawnhand Dissident",
        &[],
        &["Creature".to_string()],
        &[],
    );
    assert!(
        permission_static(&dawnhand).is_none(),
        "Dawnhand's unmodeled counter-removal cost must NOT parse to a permission: {:?}",
        dawnhand.statics
    );
    assert!(
        dawnhand.replacements.is_empty(),
        "Dawnhand must not misparse into a replacement, got {:?}",
        dawnhand.replacements
    );
    // Reach-guard: the same classifier route yields a real permission for Intrepid.
    let intrepid = parse_oracle_text(
        INTREPID_PERMISSION,
        "Intrepid Paleontologist",
        &[],
        &["Creature".to_string()],
        &[],
    );
    assert!(
        permission_static(&intrepid).is_some(),
        "reach-guard: the Intrepid route must produce a permission"
    );
}

// ── Runtime ──────────────────────────────────────────────────────────────────

/// Noctis runtime: casting the artifact from the graveyard pays 3 life AND the
/// permanent enters with a finality counter; a non-artifact graveyard card is
/// NOT castable (filter); a normal hand cast enters with NO finality counter.
/// Revert-probes: omit the finalize_cast reader → finality absent; drop the
/// extra_cost → life delta 0.
#[test]
fn noctis_runtime_cost_filter_and_finality() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    scenario
        .add_creature(P0, "Noctis, Prince of Lucis", 4, 3)
        .from_oracle_text(NOCTIS_PERMISSION);
    let artifact = scenario
        .add_creature_to_graveyard(P0, "Test Relic", 0, 0)
        .as_artifact()
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 1,
        })
        .id();
    let instant = scenario
        .add_spell_to_graveyard(P0, "Test Bolt", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(P0, pool_units(&[ManaType::Colorless]));
    let mut runner = scenario.build();

    // (b) filter gate: the non-artifact graveyard card is not castable via Noctis.
    assert!(
        !can_cast_object_now(runner.state(), P0, instant),
        "a non-artifact graveyard card must not be castable via Noctis"
    );
    // reach-guard: the artifact IS surfaced as castable.
    assert!(
        spell_objects_available_to_cast(runner.state(), P0).contains(&artifact),
        "Noctis must surface the graveyard artifact as castable"
    );

    // (a) cast the artifact: pays 3 life, enters with a finality counter.
    let outcome = runner.cast(artifact).resolve();
    assert_eq!(
        outcome.life_delta(P0),
        -3,
        "casting via Noctis must pay 3 life in addition to the mana cost"
    );
    assert_eq!(
        outcome.zone_of(artifact),
        Zone::Battlefield,
        "the artifact must resolve onto the battlefield"
    );
    assert_eq!(
        outcome.counters(artifact, finality()),
        1,
        "the artifact cast via Noctis must enter with a finality counter"
    );
}

/// Noctis runtime (c): a NORMAL cast of the same artifact from hand enters with
/// NO finality counter — the counter rides the permission, not the card.
#[test]
fn noctis_normal_hand_cast_has_no_finality() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    scenario
        .add_creature(P0, "Noctis, Prince of Lucis", 4, 3)
        .from_oracle_text(NOCTIS_PERMISSION);
    let hand_artifact = scenario
        .add_creature_to_hand(P0, "Hand Relic", 0, 0)
        .as_artifact()
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 1,
        })
        .id();
    scenario.with_mana_pool(P0, pool_units(&[ManaType::Colorless]));
    let mut runner = scenario.build();

    let outcome = runner.cast(hand_artifact).resolve();
    assert_eq!(
        outcome.zone_of(hand_artifact),
        Zone::Battlefield,
        "the hand artifact must resolve onto the battlefield"
    );
    assert_eq!(
        outcome.counters(hand_artifact, finality()),
        0,
        "a normal hand cast must NOT enter with a finality counter"
    );
    assert_eq!(
        outcome.life_delta(P0),
        0,
        "a normal hand cast must NOT pay the permission's 3-life additional cost"
    );
}

/// Leonardo runtime: on the controller's turn, an eligible (P/T ≤ 1) creature is
/// castable from the graveyard and enters with a finality counter; a P/T ≥ 2
/// creature is NOT castable (filter). Revert-probe: omit the reader → finality
/// absent.
#[test]
fn leonardo_runtime_filter_and_finality_on_your_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    scenario
        .add_creature(P0, "Leonardo, Sewer Samurai", 2, 2)
        .from_oracle_text(LEONARDO_PERMISSION);
    let eligible = scenario
        .add_creature_to_graveyard(P0, "Tiny Rat", 1, 1)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 0,
        })
        .id();
    let ineligible = scenario
        .add_creature_to_graveyard(P0, "Big Ogre", 3, 3)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();

    // (a) filter: the P/T>=2 creature is not castable; the P/T<=1 creature is.
    assert!(
        !can_cast_object_now(runner.state(), P0, ineligible),
        "a P/T>=2 creature must not be castable via Leonardo"
    );
    assert!(
        spell_objects_available_to_cast(runner.state(), P0).contains(&eligible),
        "Leonardo must surface the P/T<=1 creature as castable"
    );

    let outcome = runner.cast(eligible).resolve();
    assert_eq!(
        outcome.zone_of(eligible),
        Zone::Battlefield,
        "the eligible creature must resolve onto the battlefield"
    );
    assert_eq!(
        outcome.counters(eligible, finality()),
        1,
        "the creature cast via Leonardo must enter with a finality counter"
    );
}

/// Leonardo runtime (b): the DuringYourTurn gate blocks the cast on the
/// opponent's turn. Revert-probe: flip the builder's `condition(DuringYourTurn)`
/// off → this "not castable" assertion fails.
#[test]
fn leonardo_permission_blocked_on_opponents_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    scenario
        .add_creature(P0, "Leonardo, Sewer Samurai", 2, 2)
        .from_oracle_text(LEONARDO_PERMISSION);
    let eligible = scenario
        .add_creature_to_graveyard(P0, "Tiny Rat", 1, 1)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    // reach-guard: castable on P0's own turn.
    assert!(
        can_cast_object_now(runner.state(), P0, eligible),
        "reach-guard: the eligible creature is castable on P0's turn"
    );
    // Opponent's turn: the DuringYourTurn gate must block P0.
    runner.state_mut().active_player = PlayerId(1);
    assert!(
        !can_cast_object_now(runner.state(), P0, eligible),
        "Leonardo's during-your-turn gate must block the cast on the opponent's turn"
    );
}

/// Intrepid runtime (R3 — real path): activate the {2} exile ability on an owned
/// Dinosaur creature card in a graveyard; the ExileCastPermission static auto-
/// populates the exile pool (source-scan tag), the card is castable via the
/// permission, and enters WITH a finality counter. Discriminators: a non-Dinosaur
/// owned exiled card is NOT castable (filter); an opponent-owned Dinosaur is NOT
/// castable (owner=You). Revert-probe lives in
/// `intrepid_pool_requires_permission_static`.
#[test]
fn intrepid_runtime_real_exile_then_cast_with_finality() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let intrepid = scenario
        .add_creature(P0, "Intrepid Paleontologist", 1, 3)
        .from_oracle_text(INTREPID_EXILE_AND_PERMISSION)
        .id();
    // Owned Dinosaur creature card in P0's graveyard (the positive). Positive
    // toughness so it survives SBA (CR 704.5f) and we can observe the finality
    // counter placement — CR 122.1h death→exile is now enforced
    // (finality_counter_death_to_exile.rs); a positive-toughness fixture keeps
    // this test focused on placement, not death.
    let dino = scenario
        .add_creature_to_graveyard(P0, "Ranging Raptors", 2, 2)
        .with_subtypes(vec!["Dinosaur"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(P0, pool_units(&[ManaType::Colorless, ManaType::Colorless]));
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;

    // Find the {2} exile ability index.
    let exile_idx = runner.state().objects[&intrepid]
        .abilities
        .iter()
        .position(|a| {
            matches!(
                a.effect.as_ref(),
                engine::types::ability::Effect::ChangeZone { .. }
            )
        })
        .expect("Intrepid must have a {2}: Exile activated ability");

    // Real path: activate {2} targeting the owned Dinosaur in the graveyard.
    let _ = runner
        .activate(intrepid, exile_idx)
        .target_object(dino)
        .resolve();

    // Reach-guard: the exile actually moved the Dinosaur and the pool auto-populated.
    assert_eq!(
        runner.state().objects[&dino].zone,
        Zone::Exile,
        "the {{2}} ability must exile the Dinosaur"
    );
    assert!(
        runner.state().exile_links.iter().any(|link| {
            link.exiled_id == dino
                && link.source_id == intrepid
                && matches!(link.kind, ExileLinkKind::TrackedBySource)
        }),
        "the ExileCastPermission static must auto-tag the exile with Intrepid: {:?}",
        runner.state().exile_links
    );

    // The owned Dinosaur is castable via the permission.
    assert!(
        can_cast_object_now(runner.state(), P0, dino),
        "the owned Dinosaur must be castable via Intrepid's permission"
    );

    let outcome = runner.cast(dino).resolve();
    assert_eq!(
        outcome.zone_of(dino),
        Zone::Battlefield,
        "the Dinosaur cast via Intrepid must resolve onto the battlefield"
    );
    assert_eq!(
        outcome.counters(dino, finality()),
        1,
        "the Dinosaur cast via Intrepid must enter with a finality counter"
    );
}

/// Intrepid runtime discriminators (b) filter + (c) owner=You: exile a
/// non-Dinosaur owned card and an opponent-owned Dinosaur via the {2} ability;
/// neither is castable via the permission.
#[test]
fn intrepid_filter_and_owner_constraints() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let intrepid = scenario
        .add_creature(P0, "Intrepid Paleontologist", 1, 3)
        .from_oracle_text(INTREPID_EXILE_AND_PERMISSION)
        .id();
    // (b) owned non-Dinosaur creature card in P0's graveyard.
    let non_dino = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    // (c) opponent-owned Dinosaur creature card in P1's graveyard.
    let opp_dino = scenario
        .add_creature_to_graveyard(P1, "Enemy Raptor", 2, 2)
        .with_subtypes(vec!["Dinosaur"])
        .id();
    scenario.with_mana_pool(
        P0,
        pool_units(&[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]),
    );
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;

    let exile_idx = runner.state().objects[&intrepid]
        .abilities
        .iter()
        .position(|a| {
            matches!(
                a.effect.as_ref(),
                engine::types::ability::Effect::ChangeZone { .. }
            )
        })
        .expect("Intrepid must have a {2}: Exile activated ability");

    let _ = runner
        .activate(intrepid, exile_idx)
        .target_object(non_dino)
        .resolve();
    let _ = runner
        .activate(intrepid, exile_idx)
        .target_object(opp_dino)
        .resolve();

    // Reach-guard: both cards were exiled + linked (so a "not castable" below is
    // a real filter/owner veto, not a vacuous empty pool).
    assert_eq!(runner.state().objects[&non_dino].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&opp_dino].zone, Zone::Exile);
    assert!(
        runner
            .state()
            .exile_links
            .iter()
            .any(|l| l.exiled_id == non_dino && l.source_id == intrepid),
        "the non-Dinosaur exile must be tracked (reach-guard)"
    );
    assert!(
        runner
            .state()
            .exile_links
            .iter()
            .any(|l| l.exiled_id == opp_dino && l.source_id == intrepid),
        "the opponent-owned exile must be tracked (reach-guard)"
    );

    // (b) filter: the non-Dinosaur is not castable.
    assert!(
        !can_cast_object_now(runner.state(), P0, non_dino),
        "a non-Dinosaur exiled card must not be castable via Intrepid (filter)"
    );
    // (c) owner=You: the opponent-owned Dinosaur is not castable.
    assert!(
        !can_cast_object_now(runner.state(), P0, opp_dino),
        "an opponent-owned Dinosaur must not be castable via Intrepid (owner=You)"
    );
}

/// Intrepid runtime revert-probe (R3): the source-scan tag is load-bearing —
/// WITHOUT the ExileCastPermission static on the source, the {2} exile is NOT
/// tracked, so the pool stays empty and the card is NOT castable. This flips to
/// FAIL if `"ExileCastPermission"` is removed from `LINKED_EXILE_CONSUMER_TAGS`
/// (the control here has only the {2} ability, no permission static).
#[test]
fn intrepid_pool_requires_permission_static() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    // Control: the {2} exile ability WITHOUT the cast permission static.
    let plain = scenario
        .add_creature(P0, "Plain Exiler", 1, 3)
        .from_oracle_text("{2}: Exile target card from a graveyard.")
        .id();
    let dino = scenario
        .add_creature_to_graveyard(P0, "Ranging Raptors", 0, 0)
        .with_subtypes(vec!["Dinosaur"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(P0, pool_units(&[ManaType::Colorless, ManaType::Colorless]));
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;

    let exile_idx = runner.state().objects[&plain]
        .abilities
        .iter()
        .position(|a| {
            matches!(
                a.effect.as_ref(),
                engine::types::ability::Effect::ChangeZone { .. }
            )
        })
        .expect("the control must have a {2}: Exile activated ability");

    let _ = runner
        .activate(plain, exile_idx)
        .target_object(dino)
        .resolve();

    // Reach-guard: the Dinosaur WAS exiled (the {2} ability ran) ...
    assert_eq!(
        runner.state().objects[&dino].zone,
        Zone::Exile,
        "the {{2}} ability must still exile the Dinosaur"
    );
    // ... but with no cast-permission static, the exile is NOT tracked-by-source.
    assert!(
        !runner
            .state()
            .exile_links
            .iter()
            .any(|l| l.exiled_id == dino && l.source_id == plain),
        "without the ExileCastPermission static the exile must NOT be tracked"
    );
    // ... so the card is not castable (empty pool).
    assert!(
        !can_cast_object_now(runner.state(), P0, dino),
        "without the permission static the exiled card must not be castable"
    );
}
