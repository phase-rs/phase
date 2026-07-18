//! L02 BB6 — enters-with-counter gated by a condition (2 Standard cards).
//!
//! Cards (Oracle text verified verbatim vs Scryfall / `data/card-data.json`):
//!   * Freestrider Commando (OTJ 155, {2}{G} 3/3 Centaur Mercenary) — the
//!     enters-with-counters replacement is gated on "if it wasn't cast or no mana
//!     was spent to cast it" → a single `ManaSpentToCast == 0` leaf that flows
//!     through the existing `extract_enters_with_only_if_suffix` →
//!     `replacement_condition_from_static` (QuantityComparison → OnlyIfQuantity)
//!     pipeline (Blacksnag Buzzard precedent).
//!   * Undead Sprinter (DSK 121, {B}{R} 2/2 Zombie) — a self-granting graveyard
//!     cast permission gated on "if a non-Zombie creature died this turn", with a
//!     linked "If you do, this creature enters with a +1/+1 counter on it" rider.
//!
//! Parse tests drive `parse_oracle_text` / the single condition authority
//! `parse_inner_condition`; runtime tests drive the real cast pipeline
//! (`GameRunner::cast(..).resolve()`). Counter assertions check PLACEMENT only.
//!
//! FOLLOW-UP (out of BB6's zero-variant scope): Freestrider's
//! `mana_spent_to_cast_amount` is not cleared in `reset_for_battlefield_entry`
//! (a codebase-wide field-hygiene gap shared with Lavinia/Satoru). A Freestrider
//! hard-cast-and-countered earlier this game and later reanimated as the SAME
//! object reads a stale nonzero amount, so the `== 0` leaf wrongly suppresses the
//! counters. The single-leaf representation is rules-correct for every realistic
//! Standard path (hard-cast / Plot / reanimate-a-fresh-object); the true fix
//! needs `ReplacementCondition::Or` (a `Not(WasCast)` disjunct over the
//! per-entry-reset `cast_from_zone`), which is absent. Not tested here.

use engine::game::casting::can_cast_object_now;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle::{parse_oracle_text, ParsedAbilities};
use engine::parser::oracle_nom::condition::parse_inner_condition;
use engine::types::ability::{
    CastManaObjectScope, CastManaSpentMetric, Comparator, Effect, QuantityExpr, QuantityRef,
    ReplacementCondition, StaticCondition, StaticDefinition, TargetFilter, TypeFilter,
};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

// ── Verbatim Oracle text ─────────────────────────────────────────────────────

const FREESTRIDER: &str = "This creature enters with two +1/+1 counters on it if it wasn't cast or no mana was spent to cast it.\nPlot {3}{G} (You may pay {3}{G} and exile this card from your hand. Cast it as a sorcery on a later turn without paying its mana cost. Plot only as a sorcery.)";

const UNDEAD_SPRINTER: &str = "Trample, haste\nYou may cast this card from your graveyard if a non-Zombie creature died this turn. If you do, this creature enters with a +1/+1 counter on it.";

const NOCTIS: &str = "You may cast artifact spells from your graveyard by paying 3 life in addition to paying their other costs. If you cast a spell this way, that artifact enters with a finality counter on it. (If a permanent with a finality counter on it would go to a graveyard, exile it instead.)";

// ── Helpers ──────────────────────────────────────────────────────────────────

fn pool_units(colors: &[ManaType]) -> Vec<ManaUnit> {
    let dummy = ObjectId(0);
    colors
        .iter()
        .map(|&color| ManaUnit::new(color, dummy, false, vec![]))
        .collect()
}

fn has_swallow(parsed: &ParsedAbilities, detector: &str) -> bool {
    parsed.parse_warnings.iter().any(|w| {
        format!("{w:?}").contains("SwallowedClause") && format!("{w:?}").contains(detector)
    })
}

/// The enters-with-counters replacement (`execute.effect == PutCounter`).
fn enters_with_counter_replacement(
    parsed: &ParsedAbilities,
) -> Option<&engine::types::ability::ReplacementDefinition> {
    parsed.replacements.iter().find(|r| {
        r.execute
            .as_ref()
            .is_some_and(|e| matches!(&*e.effect, Effect::PutCounter { .. }))
    })
}

fn permission_static(parsed: &ParsedAbilities) -> Option<&StaticDefinition> {
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

fn zero_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![],
        generic: 0,
    }
}

// ══ Parse fidelity ════════════════════════════════════════════════════════════

/// Test 1 — Freestrider parse: the enters-with-counters replacement carries
/// `condition = OnlyIfQuantity{ ManaSpentToCast{SelfObject,Total} == 0 }` and its
/// execute effect is `PutCounter{Plus1Plus1, Fixed(2)}`, with NO Condition_If
/// swallowed. Revert-probe: remove the `parse_it_wasnt_cast_or_no_mana_spent` arm
/// → `condition == None` AND Condition_If reappears.
#[test]
fn freestrider_parse_condition_and_counters() {
    let parsed = parse_oracle_text(
        FREESTRIDER,
        "Freestrider Commando",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let repl = enters_with_counter_replacement(&parsed)
        .expect("Freestrider must emit an enters-with-counters replacement");

    // The counters (reach-guard: the replacement was reached and carries them).
    let execute = repl.execute.as_ref().expect("execute present");
    let Effect::PutCounter {
        ref counter_type,
        ref count,
        ..
    } = *execute.effect
    else {
        panic!("expected PutCounter, got {:?}", execute.effect);
    };
    assert_eq!(*counter_type, CounterType::Plus1Plus1);
    assert_eq!(*count, QuantityExpr::Fixed { value: 2 });

    // The load-bearing attach: the cast-context gate rides the replacement.
    assert_eq!(
        repl.condition,
        Some(ReplacementCondition::OnlyIfQuantity {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ManaSpentToCast {
                    scope: CastManaObjectScope::SelfObject,
                    metric: CastManaSpentMetric::Total,
                },
            },
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Fixed { value: 0 },
            active_player_req: None,
        }),
        "Freestrider's replacement must carry the ManaSpentToCast==0 gate"
    );
    assert!(
        !has_swallow(&parsed, "Condition_If"),
        "Freestrider's cast-context gate must not be a swallowed Condition_If: {:?}",
        parsed.parse_warnings
    );
}

/// Test 2 — Undead Sprinter parse: `GraveyardCastPermission{ enters_with_counter:
/// Some(Plus1Plus1) }` with `condition = QuantityComparison{ ZoneChangeCountThisTurn
/// (Battlefield→Graveyard, non-Zombie creature) >= 1 }`, and NO Condition_If.
/// Revert-probes: (a) remove the "if you do" split marker OR the "this creature"
/// subject arm → `enters_with_counter == None` + Condition_If; (b) remove the
/// filtered died-this-turn arm → `condition == None` + Condition_If.
#[test]
fn undead_sprinter_parse_gate_and_rider() {
    let parsed = parse_oracle_text(
        UNDEAD_SPRINTER,
        "Undead Sprinter",
        &[],
        &["Creature".to_string()],
        &["Zombie".to_string()],
    );
    let def =
        permission_static(&parsed).expect("Undead Sprinter must emit a GraveyardCastPermission");

    // (rider) the +1/+1 rides the self-granting permission.
    assert_eq!(
        permission_counter(&def.mode),
        &Some(CounterType::Plus1Plus1),
        "Undead Sprinter's +1/+1 rider must ride the permission"
    );

    // (gate) the died-this-turn condition, filtered to non-Zombie creatures.
    let Some(StaticCondition::QuantityComparison {
        lhs:
            QuantityExpr::Ref {
                qty: QuantityRef::ZoneChangeCountThisTurn { from, to, filter },
            },
        comparator,
        rhs,
    }) = def.condition.clone()
    else {
        panic!(
            "expected a ZoneChangeCountThisTurn gate, got {:?}",
            def.condition
        );
    };
    assert_eq!(from, Some(Zone::Battlefield));
    assert_eq!(to, Some(Zone::Graveyard));
    assert_eq!(comparator, Comparator::GE);
    assert_eq!(rhs, QuantityExpr::Fixed { value: 1 });
    assert_non_zombie_creature_filter(&filter);

    assert!(
        !has_swallow(&parsed, "Condition_If"),
        "Undead Sprinter's gate + rider must not be a swallowed Condition_If: {:?}",
        parsed.parse_warnings
    );
}

/// Test 3 (class-level) — the whole-phrase no-mana/uncast recognizer collapses
/// "it wasn't cast or no mana was spent to cast it" to a single fully-consumed
/// `ManaSpentToCast == 0` leaf. Revert-probe: remove the arm → the disjunction
/// falls to bare `Not(WasCast)` and strands " or no mana was spent to cast it"
/// (a non-empty residual), so the enters-with-only-if full-consume check fails.
#[test]
fn no_mana_or_uncast_recognizer_is_one_leaf() {
    let (rest, cond) = parse_inner_condition("it wasn't cast or no mana was spent to cast it")
        .expect("the whole-phrase arm must parse");
    assert!(
        rest.trim().is_empty(),
        "the whole-phrase arm must fully consume, left: {rest:?}"
    );
    assert_eq!(
        cond,
        StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ManaSpentToCast {
                    scope: CastManaObjectScope::SelfObject,
                    metric: CastManaSpentMetric::Total,
                },
            },
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Fixed { value: 0 },
        },
    );
}

/// Test 4 (class-level) — the enters-with-counter subject grammar covers the new
/// self-referential "this creature" (Undead Sprinter → Plus1Plus1) alongside the
/// incumbent "that artifact" (Noctis → finality), driven through the full-card
/// pipeline (the recognizer is `pub(crate)`). Revert-probe: removing the "this
/// creature enters with " arm drops Undead Sprinter's counter (see Test 2).
#[test]
fn enters_with_counter_subject_covers_this_creature_and_that_artifact() {
    let sprinter = parse_oracle_text(
        UNDEAD_SPRINTER,
        "Undead Sprinter",
        &[],
        &["Creature".to_string()],
        &["Zombie".to_string()],
    );
    assert_eq!(
        permission_counter(
            &permission_static(&sprinter)
                .expect("Undead Sprinter permission")
                .mode
        ),
        &Some(CounterType::Plus1Plus1),
        "\"this creature\" subject must resolve the +1/+1 rider"
    );

    let noctis = parse_oracle_text(
        NOCTIS,
        "Noctis, Prince of Lucis",
        &[],
        &["Creature".to_string()],
        &[],
    );
    assert_eq!(
        permission_counter(&permission_static(&noctis).expect("Noctis permission").mode),
        &Some(CounterType::Finality),
        "incumbent \"that artifact\" subject must still resolve the finality rider"
    );
}

/// Test 5 (class-level) — the filtered died-this-turn arm: (a) "a non-Zombie
/// creature died this turn" → filtered gate; (b) the bare "a creature died this
/// turn" still yields the UNFILTERED ref (no Morbid regression); (c) a
/// name-negation ("a creature not named Ebondeath, Dracolich died this turn") is
/// NOT claimed by the arm (parse_type_phrase leaves "not named …" leftover → the
/// arm rejects → clean gap). Revert-probe: remove the filtered arm → (a) errors.
#[test]
fn filtered_died_this_turn_arm_and_clean_gaps() {
    // (a) filtered non-Zombie form.
    let (rest, cond) = parse_inner_condition("a non-zombie creature died this turn")
        .expect("filtered non-Zombie died-this-turn must parse");
    assert!(rest.trim().is_empty(), "must fully consume, left: {rest:?}");
    let StaticCondition::QuantityComparison {
        lhs:
            QuantityExpr::Ref {
                qty: QuantityRef::ZoneChangeCountThisTurn { filter, .. },
            },
        ..
    } = cond
    else {
        panic!("expected a filtered died-this-turn gate, got {cond:?}");
    };
    assert_non_zombie_creature_filter(&filter);

    // (b) bare form stays UNFILTERED (no Non — Morbid non-regression).
    let (rest, cond) = parse_inner_condition("a creature died this turn")
        .expect("bare died-this-turn must still parse");
    assert!(rest.trim().is_empty());
    let StaticCondition::QuantityComparison {
        lhs:
            QuantityExpr::Ref {
                qty:
                    QuantityRef::ZoneChangeCountThisTurn {
                        filter: TargetFilter::Typed(tf),
                        ..
                    },
            },
        ..
    } = cond
    else {
        panic!("bare died-this-turn must parse to the unfiltered typed gate, got {cond:?}");
    };
    assert!(
        !tf.type_filters
            .iter()
            .any(|f| matches!(f, TypeFilter::Non(_))),
        "bare 'a creature died this turn' must stay UNFILTERED (no Non), got {:?}",
        tf.type_filters
    );

    // (c) Ebondeath name-negation stays a clean gap — the filtered arm is the ONLY
    // producer of a fully-consumed died-this-turn→Graveyard gate, so assert the
    // phrase never parses to one.
    let ebondeath =
        parse_inner_condition("a creature not named ebondeath, dracolich died this turn");
    let claimed_as_died_gate = matches!(
        &ebondeath,
        Ok((rest, StaticCondition::QuantityComparison {
            lhs: QuantityExpr::Ref { qty: QuantityRef::ZoneChangeCountThisTurn { to: Some(Zone::Graveyard), .. } },
            ..
        })) if rest.trim().is_empty()
    );
    assert!(
        !claimed_as_died_gate,
        "Ebondeath name-negation must stay a clean gap, not a mis-parsed died gate: {ebondeath:?}"
    );
}

/// Assert a `TargetFilter` is a Typed creature filter carrying `Non(Subtype(Zombie))`.
fn assert_non_zombie_creature_filter(filter: &TargetFilter) {
    let TargetFilter::Typed(tf) = filter else {
        panic!("expected a Typed filter, got {filter:?}");
    };
    assert!(
        tf.type_filters.contains(&TypeFilter::Creature),
        "filter must constrain to Creature, got {:?}",
        tf.type_filters
    );
    assert!(
        tf.type_filters.iter().any(|f| matches!(
            f,
            TypeFilter::Non(inner)
                if matches!(&**inner, TypeFilter::Subtype(s) if s.eq_ignore_ascii_case("zombie"))
        )),
        "filter must carry Non(Subtype(Zombie)), got {:?}",
        tf.type_filters
    );
}

// ══ Runtime ═══════════════════════════════════════════════════════════════════

/// Test 6 — Freestrider runtime: the cast-context gate is load-bearing.
/// (b) Hard-cast for {2}{G} (mana spent = 3) → the permanent gains NO +1/+1
/// counters. Revert-probe: dropping `repl.condition` (making it Mandatory) makes
/// this hard cast wrongly gain 2 counters. The paired positive (Test 6 below,
/// mana = 0 → 2 counters) proves this "0 counters" is a real veto, not vacuous.
#[test]
fn freestrider_runtime_hard_cast_no_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let freestrider = scenario
        .add_creature_to_hand_from_oracle(P0, "Freestrider Commando", 3, 3, FREESTRIDER)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 2,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        pool_units(&[ManaType::Green, ManaType::Colorless, ManaType::Colorless]),
    );
    let mut runner = scenario.build();

    let outcome = runner.cast(freestrider).resolve();
    assert_eq!(
        outcome.zone_of(freestrider),
        Zone::Battlefield,
        "the hard-cast Freestrider must resolve onto the battlefield"
    );
    assert_eq!(
        outcome.counters(freestrider, CounterType::Plus1Plus1),
        0,
        "a hard cast (mana spent > 0) must NOT gain +1/+1 counters"
    );
}

/// Test 6 (positive / non-vacuity reach-guard) — a 0-mana cast of the same card
/// (representative of Plot's free cast and a never-cast battlefield entry, which
/// all leave `mana_spent_to_cast_amount == 0`) DOES enter with 2 counters. This
/// proves the enters-with-counters replacement places counters when the gate
/// holds, so the "0 counters" assertion above is a genuine veto.
#[test]
fn freestrider_runtime_zero_mana_cast_gains_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let freestrider = scenario
        .add_creature_to_hand_from_oracle(P0, "Freestrider Commando", 3, 3, FREESTRIDER)
        .with_mana_cost(zero_cost())
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(freestrider).resolve();
    assert_eq!(
        outcome.zone_of(freestrider),
        Zone::Battlefield,
        "the 0-mana Freestrider must resolve onto the battlefield"
    );
    assert_eq!(
        outcome.counters(freestrider, CounterType::Plus1Plus1),
        2,
        "a 0-mana cast (mana spent == 0) must enter with two +1/+1 counters"
    );
}

/// Build a scenario with an Undead Sprinter card in P0's graveyard (the
/// self-granting source + cast object). {0} mana cost so the died-this-turn gate
/// (not mana) is the only variable.
fn sprinter_in_graveyard() -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let sprinter = scenario
        .add_creature_to_graveyard(P0, "Undead Sprinter", 2, 2)
        .from_oracle_text(UNDEAD_SPRINTER)
        .with_mana_cost(zero_cost())
        .id();
    (scenario, sprinter)
}

/// Seed a battlefield→graveyard death record this turn for a fresh creature with
/// the given subtypes (mirrors the real zones pipeline snapshot).
fn seed_death(scenario: &mut GameScenario, name: &str, subtypes: Vec<&str>) -> ObjectId {
    scenario
        .add_creature(P0, name, 2, 2)
        .with_subtypes(subtypes)
        .id()
}

fn push_death(runner: &mut engine::game::scenario::GameRunner, obj: ObjectId) {
    let rec = runner.state().objects[&obj].snapshot_for_zone_change(
        obj,
        Some(Zone::Battlefield),
        Zone::Graveyard,
    );
    runner.state_mut().zone_changes_this_turn.push(rec);
}

/// Test 7(a) + 7(i) — Undead Sprinter runtime: with a non-Zombie creature dead
/// this turn the self-granting graveyard cast is legal and the permanent enters
/// with a +1/+1 counter. The counter==1 assertion is the ENGINE-CHANGE
/// discriminator: reverting the self-granting reader fallback in
/// `selected_static_permission_enters_with_counter` makes it absent (0).
#[test]
fn undead_sprinter_runtime_gate_and_linked_counter() {
    let (mut scenario, sprinter) = sprinter_in_graveyard();
    let bear = seed_death(&mut scenario, "Grizzly Bears", vec!["Bear"]);
    let mut runner = scenario.build();
    push_death(&mut runner, bear);

    assert!(
        can_cast_object_now(runner.state(), P0, sprinter),
        "a non-Zombie creature died → Undead Sprinter is castable from the graveyard"
    );
    let outcome = runner.cast(sprinter).resolve();
    assert_eq!(
        outcome.zone_of(sprinter),
        Zone::Battlefield,
        "the graveyard cast must resolve onto the battlefield"
    );
    assert_eq!(
        outcome.counters(sprinter, CounterType::Plus1Plus1),
        1,
        "the self-granting graveyard cast must enter with a +1/+1 counter (reader fallback)"
    );
}

/// Test 7(b) — with nothing dead this turn, Undead Sprinter is NOT castable from
/// the graveyard. Non-vacuous: Test 7(a) proves it IS castable once a non-Zombie
/// death is seeded. Revert-probe: dropping the gate condition makes this castable.
#[test]
fn undead_sprinter_not_castable_when_nothing_died() {
    let (scenario, sprinter) = sprinter_in_graveyard();
    let runner = scenario.build();
    assert!(
        !can_cast_object_now(runner.state(), P0, sprinter),
        "nothing died this turn → Undead Sprinter must NOT be castable from the graveyard"
    );
}

/// Test 7(c) — the `Non(Zombie)` discriminator: with only a Zombie dead this turn
/// the gate is unsatisfied, so Undead Sprinter is NOT castable. Non-vacuous:
/// Test 7(a) proves a non-Zombie death DOES enable the cast.
#[test]
fn undead_sprinter_not_castable_when_only_zombie_died() {
    let (mut scenario, sprinter) = sprinter_in_graveyard();
    let zombie = seed_death(&mut scenario, "Rotting Zombie", vec!["Zombie"]);
    let mut runner = scenario.build();
    push_death(&mut runner, zombie);
    assert!(
        !can_cast_object_now(runner.state(), P0, sprinter),
        "only a Zombie died → the Non(Zombie) filter must keep Undead Sprinter uncastable"
    );
}

/// Test 7(d) — the +1/+1 rides the this-way graveyard cast, not the card: a
/// normal hard cast of a fresh Undead Sprinter from hand enters with NO counter.
#[test]
fn undead_sprinter_normal_hand_cast_has_no_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let hand_sprinter = scenario
        .add_creature_to_hand_from_oracle(P0, "Undead Sprinter", 2, 2, UNDEAD_SPRINTER)
        .with_mana_cost(zero_cost())
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(hand_sprinter).resolve();
    assert_eq!(
        outcome.zone_of(hand_sprinter),
        Zone::Battlefield,
        "the hand Undead Sprinter must resolve onto the battlefield"
    );
    assert_eq!(
        outcome.counters(hand_sprinter, CounterType::Plus1Plus1),
        0,
        "a normal hand cast must NOT enter with a +1/+1 counter (rides the graveyard cast)"
    );
}
