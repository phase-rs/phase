//! No Witnesses ({2}{W}{W} sorcery) — "Each player who controls the most
//! creatures investigates. Then destroy all creatures."
//!
//! The wrath (`DestroyAll`) already parsed; the gap was clause 1, which lowered
//! to `Effect::Unimplemented{"the"}` plus a placeholder `ControlsCount{All,
//! empty-filter, GE, Fixed(1)}`. The fix adds one superlative arm to
//! `parse_controls_permanent_object` that recognizes "who controls the most
//! <type>" and lowers it to `ControlsCount{All, <bare type>, GE,
//! Ref(ControlledByEachPlayer{<bare type>, Max})}`, so each player tied for the
//! greatest count of that permanent type is selected (CR 109.4 + CR 109.5).
//!
//! Every runtime test drives the real `apply()` cast pipeline (GameScenario +
//! GameRunner::cast(..).resolve()) and asserts measured board deltas (Clue
//! tokens grouped by controller, battlefield creature count). Investigate is
//! CR 701.16a ("create a Clue token"); a Clue token is CR 111.10f (a colorless
//! Clue artifact token).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, AggregateFunction, Comparator, Effect, PlayerFilter, PlayerRelation,
    QuantityExpr, QuantityRef, TargetFilter, TypeFilter,
};
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

/// Verbatim Oracle text (card-data.json), reminder text included. The card
/// build pipeline strips the parenthetical reminder before parsing abilities.
const NO_WITNESSES_ORACLE: &str = "Each player who controls the most creatures investigates. \
Then destroy all creatures. (To investigate, create a Clue token. It's an artifact with \
\"{2}, Sacrifice this token: Draw a card.\")";

/// Verbatim Oracle text (card-data.json) — class sibling proving the "controls
/// the most <type>" arm generalizes beyond Creature/Investigate to Land/Sacrifice.
const TECTONIC_HELLION_ORACLE: &str = "Haste\nWhenever this creature attacks, each player who \
controls the most lands sacrifices two lands of their choice.";

// --- helpers ---------------------------------------------------------------

/// Collect a definition and its `sub_ability` / `else_ability` chain (root first).
fn collect_defs<'a>(def: &'a AbilityDefinition, out: &mut Vec<&'a AbilityDefinition>) {
    out.push(def);
    if let Some(sub) = def.sub_ability.as_deref() {
        collect_defs(sub, out);
    }
    if let Some(els) = def.else_ability.as_deref() {
        collect_defs(els, out);
    }
}

/// Number of battlefield Clue tokens (CR 111.10f) controlled by `player`.
fn clues_controlled(state: &GameState, player: PlayerId) -> usize {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| o.controller == player && o.card_types.subtypes.iter().any(|s| s == "Clue"))
        .count()
}

/// Number of creatures currently on the battlefield.
fn creatures_on_battlefield(state: &GameState) -> usize {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| o.card_types.core_types.contains(&CoreType::Creature))
        .count()
}

/// Assert `scope` is the superlative `ControlsCount` for permanent type `ty`:
/// `{All, <bare ty>, GE, Ref(ControlledByEachPlayer{<bare ty>, Max})}`. Both
/// filter sides MUST be bare (no controller gate) — the resolvers apply the
/// per-player controller gate themselves.
fn assert_controls_the_most(scope: Option<&PlayerFilter>, ty: TypeFilter) {
    let Some(PlayerFilter::ControlsCount {
        relation,
        filter,
        comparator,
        count,
    }) = scope
    else {
        panic!("expected ControlsCount player_scope, got {scope:?}");
    };
    assert_eq!(
        *relation,
        PlayerRelation::All,
        "\"each player\" → relation All"
    );
    // CR 107.1 (integers): GE the max == EQ the max; selects exactly the tied-for-most set.
    assert_eq!(
        *comparator,
        Comparator::GE,
        "superlative compares GE the max"
    );

    let TargetFilter::Typed(tf) = filter else {
        panic!("expected a Typed carried filter, got {filter:?}");
    };
    assert_eq!(tf.type_filters, vec![ty.clone()], "carried filter type");
    assert!(
        tf.controller.is_none(),
        "carried filter must be BARE (no controller gate), got {:?}",
        tf.controller
    );

    // The count MUST be the cross-player extremum ref, NOT the old Fixed(1) placeholder.
    let QuantityExpr::Ref {
        qty:
            QuantityRef::ControlledByEachPlayer {
                filter: count_filter,
                aggregate,
            },
    } = count.as_ref()
    else {
        panic!("expected count = Ref(ControlledByEachPlayer{{Max}}), got {count:?} (NOT Fixed)");
    };
    assert_eq!(*aggregate, AggregateFunction::Max, "\"the most\" → Max");
    let TargetFilter::Typed(cf) = count_filter else {
        panic!("expected a Typed extremum filter, got {count_filter:?}");
    };
    assert_eq!(cf.type_filters, vec![ty], "extremum filter type");
    assert!(
        cf.controller.is_none(),
        "extremum filter must be BARE (no controller gate), got {:?}",
        cf.controller
    );
}

/// Build a scenario at P0's pre-combat main with `creatures[p]` vanilla 2/2s per
/// player, plus No Witnesses in P0's hand. Returns the runner and the spell id.
fn no_witnesses_scenario(
    player_count: u8,
    creatures: &[usize],
) -> (GameRunner, engine::types::ObjectId) {
    let mut scenario = if player_count == 2 {
        GameScenario::new()
    } else {
        GameScenario::new_n_player(player_count, 42)
    };
    scenario.at_phase(Phase::PreCombatMain);
    for (pi, &n) in creatures.iter().enumerate() {
        let player = PlayerId(pi as u8);
        for i in 0..n {
            scenario.add_creature(player, &format!("P{pi} Bear{i}"), 2, 2);
        }
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "No Witnesses", false, NO_WITNESSES_ORACLE)
        .id();
    (scenario.build(), spell)
}

// --- Test 1: parser round-trip (shape) --------------------------------------

/// SHAPE: the real card pipeline lowers clause 1 to `Effect::Investigate` scoped
/// by the superlative `ControlsCount`, keeps the `DestroyAll{creature}` sibling,
/// and leaves zero `Effect::Unimplemented`. Runtime semantics are covered by the
/// cast-pipeline tests below.
#[test]
fn no_witnesses_lowers_to_investigate_scoped_by_controls_the_most_plus_destroy_all() {
    let parsed = parse_oracle_text(
        NO_WITNESSES_ORACLE,
        "No Witnesses",
        &[],
        &["Sorcery".to_string()],
        &[],
    );

    let mut defs = Vec::new();
    for def in &parsed.abilities {
        collect_defs(def, &mut defs);
    }

    // Zero Unimplemented — the whole card parsed.
    assert!(
        !defs
            .iter()
            .any(|d| matches!(*d.effect, Effect::Unimplemented { .. })),
        "no clause may lower to Unimplemented, got {:?}",
        defs.iter().map(|d| &d.effect).collect::<Vec<_>>()
    );

    // Clause 1: Investigate, scoped by the superlative ControlsCount(creature).
    let investigate = defs
        .iter()
        .find(|d| matches!(*d.effect, Effect::Investigate))
        .expect("clause 1 must lower to Effect::Investigate");
    assert_controls_the_most(investigate.player_scope.as_ref(), TypeFilter::Creature);

    // Sibling wrath intact: DestroyAll over creatures, unscoped (runs once).
    let destroy = defs
        .iter()
        .find(|d| matches!(*d.effect, Effect::DestroyAll { .. }))
        .expect("clause 2 must remain DestroyAll");
    let Effect::DestroyAll {
        target: TargetFilter::Typed(tf),
        ..
    } = destroy.effect.as_ref()
    else {
        panic!("expected DestroyAll{{Typed}}, got {:?}", destroy.effect);
    };
    assert_eq!(tf.type_filters, vec![TypeFilter::Creature]);
    assert!(
        destroy.player_scope.is_none(),
        "the wrath is unconditional, not player-scoped"
    );
}

// --- Test 2: 2-player, single most ------------------------------------------

/// CR 109.5: with P0 controlling more creatures than P1, only P0 is "the player
/// who controls the most creatures" and investigates; P1 does not.
#[test]
fn two_player_only_most_creatures_investigates() {
    let (mut runner, spell) = no_witnesses_scenario(2, &[3, 1]);
    let outcome = runner.cast(spell).resolve();
    let st = outcome.state();

    // Positive reach-guard first: the tied-for-most player got exactly one Clue.
    assert_eq!(
        clues_controlled(st, P0),
        1,
        "P0 (most creatures) investigates"
    );
    // Negative: the non-most player did NOT investigate.
    assert_eq!(clues_controlled(st, P1), 0, "P1 (fewer creatures) does not");
    // Wrath is unconditional.
    assert_eq!(creatures_on_battlefield(st), 0, "all creatures destroyed");
}

// --- Test 3: 3-player, distinct counts --------------------------------------

/// CR 109.5: three distinct counts (4/2/1) — only the strict maximum holder
/// investigates.
#[test]
fn three_player_distinct_counts_only_max_investigates() {
    let (mut runner, spell) = no_witnesses_scenario(3, &[4, 2, 1]);
    let outcome = runner.cast(spell).resolve();
    let st = outcome.state();

    assert_eq!(
        clues_controlled(st, P0),
        1,
        "P0 (4 creatures, most) investigates"
    );
    assert_eq!(clues_controlled(st, P1), 0, "P1 (2 creatures) does not");
    assert_eq!(clues_controlled(st, P2), 0, "P2 (1 creature) does not");
    assert_eq!(creatures_on_battlefield(st), 0, "all creatures destroyed");
}

// --- Test 4: 3-player tie for most (>2p tie discriminator) ------------------

/// CR 109.5 + CR 107.1: when two or more players tie for the most, every tied
/// player investigates. GE-the-max selects the whole tied set, not just one.
#[test]
fn three_player_tie_for_most_all_tied_investigate() {
    let (mut runner, spell) = no_witnesses_scenario(3, &[3, 3, 1]);
    let outcome = runner.cast(spell).resolve();
    let st = outcome.state();

    // Both tied players investigate (positive on both before the negative).
    assert_eq!(clues_controlled(st, P0), 1, "P0 (tied at 3) investigates");
    assert_eq!(clues_controlled(st, P1), 1, "P1 (tied at 3) investigates");
    assert_eq!(clues_controlled(st, P2), 0, "P2 (1 creature) does not");
    assert_eq!(creatures_on_battlefield(st), 0, "all creatures destroyed");
}

// --- Test 5: wrath destroys the non-investigator's creatures ----------------

/// The wrath is unconditional: a player who did NOT investigate still has their
/// creatures destroyed. Tracks P1's specific creature to prove it left the
/// battlefield even though P1 got no Clue.
#[test]
fn wrath_destroys_non_investigators_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, "P0 Bear A", 2, 2);
    scenario.add_creature(P0, "P0 Bear B", 2, 2);
    let p1_creature = scenario.add_creature(P1, "P1 Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "No Witnesses", false, NO_WITNESSES_ORACLE)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).resolve();
    let st = outcome.state();

    // Reach-guard: P0 (most) investigated; P1 did not.
    assert_eq!(
        clues_controlled(st, P0),
        1,
        "P0 (most creatures) investigates"
    );
    assert_eq!(clues_controlled(st, P1), 0, "P1 does not investigate");
    // Yet P1's creature is destroyed by the unconditional wrath.
    assert_eq!(
        st.objects.get(&p1_creature).map(|o| o.zone),
        Some(Zone::Graveyard),
        "P1's creature must be destroyed even though P1 did not investigate"
    );
    assert_eq!(creatures_on_battlefield(st), 0, "no creatures survive");
}

// --- Test 6: all-at-zero edge (no >0 guard) ---------------------------------

/// CR 107.1 + the Tectonic Hellion "everyone same number → everyone" ruling:
/// when every player controls zero creatures they are all tied for the most, so
/// all investigate. Confirms there is no spurious `> 0` guard.
#[test]
fn all_players_at_zero_creatures_everyone_investigates() {
    let (mut runner, spell) = no_witnesses_scenario(2, &[0, 0]);
    let outcome = runner.cast(spell).resolve();
    let st = outcome.state();

    assert_eq!(clues_controlled(st, P0), 1, "P0 (tied at 0) investigates");
    assert_eq!(clues_controlled(st, P1), 1, "P1 (tied at 0) investigates");
    // DestroyAll no-ops with no creatures on the battlefield.
    assert_eq!(creatures_on_battlefield(st), 0, "no creatures to destroy");
}

// --- Test 7: class sibling — Tectonic Hellion (Land / Sacrifice) ------------

/// SHAPE: the same "controls the most <type>" arm generalizes to Land + a
/// Sacrifice effect on an attack trigger. Proves the fix is a building block for
/// the class, not a one-off for No Witnesses.
#[test]
fn tectonic_hellion_controls_the_most_lands_sacrifice_shape() {
    // Pass the "Haste" keyword hint so the keyword-only line is routed to
    // keywords rather than lowering to a spurious `Unimplemented{"Haste"}`.
    let parsed = parse_oracle_text(
        TECTONIC_HELLION_ORACLE,
        "Tectonic Hellion",
        &["Haste".to_string()],
        &["Creature".to_string()],
        &["Hellion".to_string()],
    );

    let mut defs = Vec::new();
    for trig in &parsed.triggers {
        if let Some(exec) = trig.execute.as_deref() {
            collect_defs(exec, &mut defs);
        }
    }
    for def in &parsed.abilities {
        collect_defs(def, &mut defs);
    }

    assert!(
        !defs
            .iter()
            .any(|d| matches!(*d.effect, Effect::Unimplemented { .. })),
        "Tectonic Hellion must parse with zero Unimplemented, got {:?}",
        defs.iter().map(|d| &d.effect).collect::<Vec<_>>()
    );

    let sacrifice = defs
        .iter()
        .find(|d| matches!(*d.effect, Effect::Sacrifice { .. }))
        .expect("the attack trigger must lower to Effect::Sacrifice");
    assert_controls_the_most(sacrifice.player_scope.as_ref(), TypeFilter::Land);
}
