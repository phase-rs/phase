//! Regression coverage for the Chord-of-Calling class: an X-spell that searches
//! the library for a creature card "with mana value X or less", puts it onto the
//! battlefield, then shuffles — payable in part (or entirely) via **Convoke**.
//!
//! The capability under test is composed entirely of shipped building blocks:
//!   * `Effect::SearchLibrary.filter` carries `FilterProp::Cmc { LE, Ref(X) }`
//!     (CR 202.3 mana value; CR 107.3a / CR 601.2b the announced X).
//!   * The search resolves that filter against the casting ability's `chosen_x`
//!     (CR 701.23a: look at all library cards, find matching).
//!   * Convoke (CR 702.51 / CR 702.51b) is *not* an additional/alternative cost
//!     and applies only after the total cost — including the {X} generic — is
//!     determined, so convoke-tapping toward the X-generic must NOT corrupt the
//!     announced X that the search filter binds to.
//!
//! CARD TEXT: the Oracle text below ("search ... for a creature card with mana
//! value X or less") is Chord of Calling's actual text as carried by this
//! engine's authoritative card data — verified identical in both MTGJSON
//! `AtomicCards.json` and the engine's parsed `card-data.json`. The X in
//! `{X}{G}{G}{G}` bounds the tutored creature's mana value (the classic
//! "convoke to chord out a creature of MV ≤ X" pattern), placing Chord in the
//! same X-MV search class as Nature's Rhythm / Green Sun's Zenith.

use engine::game::game_object::GameObject;
use engine::game::scenario::GameScenario;
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Comparator, Effect, FilterProp, QuantityExpr, QuantityRef,
    TargetFilter, TypeFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);

/// Planner-specified Oracle text for the X-MV search + Convoke capability.
const CHORD_ORACLE: &str = "Convoke (Your creatures can help cast this spell. Each \
creature you tap while casting this spell pays for {1} or one mana of that creature's \
color.)\nSearch your library for a creature card with mana value X or less, put it \
onto the battlefield, then shuffle.";

/// Chord's `{X}{G}{G}{G}` mana cost.
fn chord_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![
            ManaCostShard::X,
            ManaCostShard::Green,
            ManaCostShard::Green,
            ManaCostShard::Green,
        ],
        generic: 0,
    }
}

/// Add `count` units of `ty` mana to P0's pool (mirrors the Green Sun's Zenith
/// regression harness — deterministic payment without modelling lands).
fn add_mana(runner: &mut engine::game::scenario::GameRunner, ty: ManaType, count: usize) {
    for _ in 0..count {
        let unit = ManaUnit::new(ty, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }
}

/// Place a creature card with mana value `cmc` into P0's library. Mirrors the
/// `add_library_creature_with_cmc` helper used by the resolver-level tests in
/// `search_library.rs`: `ManaCost::generic(cmc)` fixes the mana value (CR 202.3).
fn add_library_creature_with_cmc(
    runner: &mut engine::game::scenario::GameRunner,
    name: &str,
    cmc: u32,
) -> ObjectId {
    let card_id = CardId(runner.state().next_object_id);
    let id = create_object(
        runner.state_mut(),
        card_id,
        P0,
        name.to_string(),
        Zone::Library,
    );
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.mana_cost = ManaCost::generic(cmc);
    id
}

/// Walk an `AbilityDefinition`'s `sub_ability` chain, pushing each link's
/// `effect` into `out`.
fn push_chain_effects(ability: &AbilityDefinition, out: &mut Vec<Effect>) {
    out.push((*ability.effect).clone());
    let mut node = ability.sub_ability.as_deref();
    while let Some(def) = node {
        out.push((*def.effect).clone());
        node = def.sub_ability.as_deref();
    }
}

/// Collect every effect the resolving spell will run. A spell's resolution chain
/// is the union of all `AbilityKind::Spell` abilities (each with its own
/// `sub_ability` chain) — `casting::combined_spell_ability_def` appends them into
/// one chain at cast time, so the search clause and its put-onto-battlefield /
/// shuffle continuation can live either as one chained ability or as siblings.
/// Collecting across both axes makes the SHAPE assertions storage-agnostic.
fn all_spell_effects(obj: &GameObject) -> Vec<Effect> {
    let mut out = Vec::new();
    for ability in obj
        .abilities
        .iter()
        .filter(|a| a.kind == AbilityKind::Spell)
    {
        push_chain_effects(ability, &mut out);
    }
    out
}

// ---------------------------------------------------------------------------
// Test 1 (SHAPE): assert the parsed AST shape, not a driven runtime transition.
// Labelled a SHAPE test per the runtime-tests-must-drive-the-pipeline rule.
// ---------------------------------------------------------------------------

/// SHAPE test — CR 202.3 + CR 107.3a + CR 701.23a. The parsed Chord ability must
/// be an `Effect::SearchLibrary` whose filter is type-restricted to Creature and
/// carries `FilterProp::Cmc { LE, Ref(Variable("X")) }`, with a `sub_ability`
/// chain that moves the found card to the battlefield (`ChangeZone`) and then
/// shuffles (`Shuffle`). This asserts the static parse output only — it does NOT
/// drive `apply()`.
#[test]
fn chord_of_calling_parses_x_mana_value_search_shape() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Chord of Calling", true, CHORD_ORACLE);
    builder.with_mana_cost(chord_cost());
    // Re-run synthesis with the Convoke keyword hint so the leading "Convoke
    // (reminder)" line is recognized as the keyword rather than glued onto the
    // search clause (which otherwise parses to Effect::Unimplemented). This mirrors
    // how card-data supplies Convoke for the real card, and the convoke test below.
    builder.from_oracle_text_with_keywords(&["Convoke"], CHORD_ORACLE);
    let spell_id = builder.id();

    let runner = scenario.build();
    let obj = &runner.state().objects[&spell_id];

    let effects = all_spell_effects(obj);

    // SearchLibrary with the X-MV creature filter.
    let search_filter = effects
        .iter()
        .find_map(|e| match e {
            Effect::SearchLibrary { filter, .. } => Some(filter),
            _ => None,
        })
        .unwrap_or_else(|| panic!("Chord must parse to a SearchLibrary effect, got {effects:?}"));

    let TargetFilter::Typed(typed) = search_filter else {
        panic!("expected a Typed search filter, got {search_filter:?}");
    };
    assert!(
        typed.type_filters.contains(&TypeFilter::Creature),
        "search filter must be type-restricted to Creature, got {:?}",
        typed.type_filters
    );
    assert!(
        typed.properties.iter().any(|p| matches!(
            p,
            FilterProp::Cmc {
                comparator: Comparator::LE,
                value: QuantityExpr::Ref { qty: QuantityRef::Variable { name } },
            } if name == "X"
        )),
        "search filter must carry Cmc {{ LE, Ref(Variable(\"X\")) }}, got {:?}",
        typed.properties
    );

    // Continuation: ChangeZone -> Battlefield, then Shuffle.
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::ChangeZone { destination, .. } if *destination == Zone::Battlefield
        )),
        "resolution must move the found card to the battlefield, got {effects:?}"
    );
    assert!(
        effects.iter().any(|e| matches!(e, Effect::Shuffle { .. })),
        "resolution must include a Shuffle, got {effects:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 2 (RUNTIME): the discriminating full-pipeline test for X-MV search.
// ---------------------------------------------------------------------------

/// RUNTIME — CR 107.3a + CR 601.2b + CR 701.23a. Cast Chord, announce X = 4
/// through the real `ChooseXValue` path (mana paid from pool, no convoke), then
/// resolve. The `SearchChoice` must offer EXACTLY the MV-2 and MV-4 creatures;
/// MV-5 and MV-8 are excluded because X = 4. The selected creature enters the
/// battlefield (not hand), and the chosen card leaves the library.
#[test]
fn chord_x_four_offers_only_mv_le_four_then_battlefield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Chord of Calling", true, CHORD_ORACLE);
    builder.with_mana_cost(chord_cost());
    // Re-run synthesis with the Convoke keyword hint so the leading "Convoke
    // (reminder)" line is recognized as the keyword rather than glued onto the
    // search clause (which otherwise parses to Effect::Unimplemented). This mirrors
    // how card-data supplies Convoke for the real card, and the convoke test below.
    builder.from_oracle_text_with_keywords(&["Convoke"], CHORD_ORACLE);
    let spell_id = builder.id();

    let mut runner = scenario.build();

    let mv2 = add_library_creature_with_cmc(&mut runner, "Mv2", 2);
    let mv4 = add_library_creature_with_cmc(&mut runner, "Mv4", 4);
    let mv5 = add_library_creature_with_cmc(&mut runner, "Mv5", 5);
    let mv8 = add_library_creature_with_cmc(&mut runner, "Mv8", 8);

    // X=4 → total cost {4}{G}{G}{G} = 7 green sources from pool.
    add_mana(&mut runner, ManaType::Green, 7);

    // CR 107.3a + CR 601.2b: announce X=4 through the fluent driver; the pool
    // auto-pays the {4}{G}{G}{G} cost and the driver halts at the mid-resolution
    // SearchChoice (the discriminating prompt for this test).
    let outcome = runner.cast(spell_id).x(4).resolve();

    // CR 701.23a: the offer must be exactly the MV-≤-4 creatures.
    match outcome.final_waiting_for() {
        WaitingFor::SearchChoice { cards, .. } => {
            assert_eq!(
                cards.len(),
                2,
                "expected exactly MV-2 and MV-4, got {cards:?}"
            );
            assert!(cards.contains(&mv2), "MV-2 must be offered");
            assert!(cards.contains(&mv4), "MV-4 must be offered (MV == X)");
            assert!(!cards.contains(&mv5), "MV-5 must be excluded (X=4)");
            assert!(!cards.contains(&mv8), "MV-8 must be excluded (X=4)");
        }
        other => panic!("expected SearchChoice, got {other:?}"),
    }

    runner
        .act(GameAction::SelectCards { cards: vec![mv4] })
        .expect("selecting MV-4 must continue resolution");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&mv4].zone,
        Zone::Battlefield,
        "the found creature must enter the battlefield"
    );
    assert!(
        !runner.state().players[0].library.contains(&mv4),
        "the found creature must leave the library"
    );
}

// ---------------------------------------------------------------------------
// Test 2b (RUNTIME, convoke): the planner-flagged residual risk.
// ---------------------------------------------------------------------------

/// RUNTIME — CR 702.51 / CR 702.51b + CR 107.3a + CR 601.2f. Identical to
/// Test 2, but the ENTIRE `{X}{G}{G}{G}` cost (including the {4} X-generic) is
/// paid via Convoke by tapping creatures. CR 702.51b: convoke applies only after
/// the total cost — including the {X} generic — is locked in, so convoke-tapping
/// toward X must NOT corrupt the announced X. We assert the post-X search offer
/// is still exactly MV-≤-4 (proving `chosen_x` survived the convoke payment).
#[test]
fn chord_convoke_paid_x_does_not_corrupt_announced_x() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // 7 green convoke-eligible creatures: 3 pay {G}{G}{G}, 4 pay the {4} generic.
    let convokers: Vec<ObjectId> = (0..7)
        .map(|i| {
            let b = scenario.add_creature(P0, &format!("Convoker {i}"), 1, 1);
            b.id()
        })
        .collect();

    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Chord of Calling", true, CHORD_ORACLE);
    builder.with_mana_cost(chord_cost());
    // Re-run synthesis with an explicit keyword hint so the "Convoke (reminder)"
    // line is recognized as the Convoke keyword (mirrors the Whir/Improvise test,
    // which relies solely on the keyword-hinted parse — `push_keyword` does not
    // dedup, so we do NOT also call `with_keyword`).
    builder.from_oracle_text_with_keywords(&["Convoke"], CHORD_ORACLE);
    let spell_id = builder.id();

    let mut runner = scenario.build();

    // Give every convoke creature green color so it can pay {G} or generic.
    for &c in &convokers {
        runner
            .state_mut()
            .objects
            .get_mut(&c)
            .unwrap()
            .color
            .push(ManaColor::Green);
    }

    let mv2 = add_library_creature_with_cmc(&mut runner, "Mv2", 2);
    let mv4 = add_library_creature_with_cmc(&mut runner, "Mv4", 4);
    let mv5 = add_library_creature_with_cmc(&mut runner, "Mv5", 5);
    let mv8 = add_library_creature_with_cmc(&mut runner, "Mv8", 8);

    assert!(
        runner.state().objects[&spell_id]
            .keywords
            .contains(&Keyword::Convoke),
        "Chord must carry the Convoke keyword"
    );

    // CR 601.2f + CR 702.51b: X is announced first, then the ENTIRE
    // {X}{G}{G}{G} cost is convoke-paid. The fluent driver announces X=4 and
    // taps every convoke creature for mana of its color (green) — 3 cover the
    // {G}{G}{G} colored shards and the remaining 4 green pay the {4} X-generic
    // (CR 702.51b: a tapped creature pays {1} or one mana of its color, and a
    // colored payment is a legal payment for a generic shard).
    let outcome = runner
        .cast(spell_id)
        .x(4)
        .convoke_with(&convokers)
        .resolve();

    // THE DISCRIMINATOR: convoke-toward-X must not have shifted the announced X.
    // The offer must still be exactly MV-≤-4.
    match outcome.final_waiting_for() {
        WaitingFor::SearchChoice { cards, .. } => {
            assert_eq!(
                cards.len(),
                2,
                "convoke-paid X must leave chosen_x = 4: expected MV-2 and MV-4, got {cards:?}"
            );
            assert!(cards.contains(&mv2), "MV-2 must be offered");
            assert!(cards.contains(&mv4), "MV-4 must be offered (MV == X)");
            assert!(
                !cards.contains(&mv5),
                "MV-5 must be excluded — chosen_x stayed 4"
            );
            assert!(
                !cards.contains(&mv8),
                "MV-8 must be excluded — chosen_x stayed 4"
            );
        }
        other => panic!("expected SearchChoice, got {other:?}"),
    }

    runner
        .act(GameAction::SelectCards { cards: vec![mv2] })
        .expect("selecting MV-2 must continue resolution");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&mv2].zone,
        Zone::Battlefield,
        "the found creature must enter the battlefield"
    );
    // The convoke creatures are tapped (CR 702.51a).
    for &c in &convokers {
        assert!(
            runner.state().objects[&c].tapped,
            "every convoke creature must be tapped"
        );
    }
}

// ---------------------------------------------------------------------------
// Test 3 (RUNTIME, X=0 boundary).
// ---------------------------------------------------------------------------

/// RUNTIME — CR 107.3a (X=0) + CR 701.23a. Cast Chord with X = 0 (pay only
/// `{G}{G}{G}`). With no MV-0 creatures in the library, the search offer is
/// empty and resolves cleanly as a fail-to-find (no panic, stack drains).
#[test]
fn chord_x_zero_offers_only_mv_zero_and_fails_to_find_cleanly() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Chord of Calling", true, CHORD_ORACLE);
    builder.with_mana_cost(chord_cost());
    // Re-run synthesis with the Convoke keyword hint so the leading "Convoke
    // (reminder)" line is recognized as the keyword rather than glued onto the
    // search clause (which otherwise parses to Effect::Unimplemented). This mirrors
    // how card-data supplies Convoke for the real card, and the convoke test below.
    builder.from_oracle_text_with_keywords(&["Convoke"], CHORD_ORACLE);
    let spell_id = builder.id();

    let mut runner = scenario.build();

    // Only MV-≥-2 creatures exist — nothing satisfies MV ≤ 0.
    add_library_creature_with_cmc(&mut runner, "Mv2", 2);
    add_library_creature_with_cmc(&mut runner, "Mv4", 4);

    // X=0 → total cost {G}{G}{G} = 3 green sources.
    add_mana(&mut runner, ManaType::Green, 3);

    // CR 107.3a (X=0): announce X=0 via the fluent driver; the pool auto-pays
    // {G}{G}{G} and the spell resolves to completion (no SearchChoice surfaces).
    let outcome = runner.cast(spell_id).x(0).resolve();

    // CR 701.23a: with no MV-0 creature, the search finds nothing — it must
    // resolve cleanly rather than pause on a non-empty SearchChoice. The driver
    // halts at the post-resolution Priority window, never a SearchChoice.
    assert!(
        !matches!(outcome.final_waiting_for(), WaitingFor::SearchChoice { .. }),
        "X=0 with no MV-0 creatures must fail to find (no SearchChoice prompt), got {:?}",
        outcome.final_waiting_for()
    );
    assert!(
        outcome.state().stack.is_empty(),
        "the stack must drain after the fail-to-find resolution"
    );
}
