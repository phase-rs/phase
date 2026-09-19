//! Agent Frank Horrigan — "has indestructible as long as it attacked this turn".
//!
//! Two seams meet here, and each has its own regression direction:
//!
//! 1. **Parser (U-1).** At base the gate parsed to
//!    `StaticCondition::Unrecognized { text: "it attacked this turn" }`, and
//!    `evaluate_condition_with_context` evaluates `Unrecognized => true`. The
//!    card was therefore permanently indestructible — rules-wrong permissive
//!    (CR 702.12b). So the assertions that regress when the grammar/rewrite is
//!    reverted are the NEGATIVE ones: before the creature has attacked
//!    (`horrigan_gains_indestructible_only_after_it_attacked`), on a sibling
//!    attacker (`other_attacker_does_not_grant_horrigan_indestructible`), and
//!    after the turn rolls over (`horrigan_loses_indestructible_on_next_turn`).
//!
//! 2. **Engine (U-2).** At base `commit_attack_declaration` flushed layers
//!    (CR 613.1) BEFORE writing `state.creatures_attacked_this_turn`, so the
//!    declaration flush evaluated a `FilterProp::AttackedThisTurn` gate against
//!    an empty ledger and left the layer cache `Clean` — nothing re-dirties it
//!    before the `DeclareAttackers` action returns. So the assertions that
//!    regress when the ledger-before-flush reorder is reverted are the
//!    POSITIVE ones read IMMEDIATELY after `declare_attackers` returns
//!    (`attacked_this_turn_gate_is_live_at_the_declaration_flush`, and the
//!    immediate positive in `horrigan_gains_indestructible_only_after_it_attacked`).
//!
//! CR 508.1a (the active player chooses which creatures will attack — the
//! declaration-time fact this gate reads), CR 508.2 (the active player gets
//! priority after attackers are declared — how the fixtures reach the
//! declare-blockers step), CR 611.3a (a static ability's continuous effect
//! isn't locked in; it applies to whatever its text indicates at any moment),
//! CR 613.1 + CR 613.1f (Layer 6 ability-adding effects), CR 701.34a
//! (proliferate — on these counter-free boards nothing is eligible, so the
//! attack/ETB trigger resolves without a prompt), CR 702.12a/b (indestructible
//! is a static ability; such permanents ignore the lethal-damage SBA),
//! CR 704.5b (a player who attempted to draw from an empty library loses —
//! why the libraries are seeded), CR 704.5g (the lethal-damage SBA the grant
//! must survive).

use engine::game::layers::evaluate_layers;
use engine::types::ability::{
    ContinuousModification, FilterProp, StaticCondition, StaticDefinition, TargetFilter,
    TypedFilter,
};
use engine::types::game_state::LayersDirty;

use super::rules::{
    AttackTarget, GameRunner, GameScenario, Keyword, ObjectId, Phase, WaitingFor, Zone, P0, P1,
};

/// Verbatim Oracle text (Scryfall, PIP). The card is seeded in full — the
/// proliferate trigger resolves synchronously on these counter-free boards
/// (`emit_empty_proliferate_action`, CR 701.34a), so no prompt opens. Scenario
/// seeding does not run ETB registration (CR 603.6a), so only the attack half
/// of the trigger ever fires here.
const AGENT_FRANK_HORRIGAN: &str = "Trample\nAgent Frank Horrigan has indestructible as long as it attacked this turn.\nWhenever Agent Frank Horrigan enters or attacks, proliferate twice. (To proliferate, choose any number of permanents and/or players, then give each another counter of each kind already there.)";

/// P0 on the battlefield with Agent Frank Horrigan at PreCombatMain.
///
/// CR 704.5b: both libraries hold a card, so a draw step across the turn
/// boundary (`horrigan_loses_indestructible_on_next_turn`) does not end the
/// game before the gate can be observed to reset.
fn horrigan_scenario() -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Filler A"]);
    scenario.with_library_top(P1, &["Filler B"]);
    let horrigan = scenario
        .add_creature(P0, "Agent Frank Horrigan", 5, 5)
        .from_oracle_text_with_keywords(&["Trample"], AGENT_FRANK_HORRIGAN)
        .id();
    (scenario, horrigan)
}

/// Pass the beginning-of-combat priority window and declare `attacker` against
/// P1 (CR 508.1).
fn declare_attack(runner: &mut GameRunner, attacker: ObjectId) {
    runner.pass_both_players();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("DeclareAttackers should succeed");
}

fn has_indestructible(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].has_keyword(&Keyword::Indestructible)
}

/// CR 508.2: after attackers are declared the active player gets priority, so
/// the declare-blockers step is reached by passing priority (and resolving the
/// attack trigger en route).
fn advance_to_declare_blockers(runner: &mut GameRunner) {
    for _ in 0..12 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ) {
            return;
        }
        runner.pass_both_players();
    }
    panic!(
        "never reached declare blockers; stuck at {:?}",
        runner.state().waiting_for
    );
}

/// CR 508.2 + CR 509.1 + CR 117.4: drive the engine from the declare-attackers
/// step to `phase` by passing priority, answering a declare-blockers window with
/// no blocks.
///
/// `GameRunner::advance_to_phase` cannot be used from inside the
/// declare-attackers step: it opens with `turns::auto_advance`, whose
/// `Phase::DeclareAttackers` arm re-raises the declare-attackers turn-based
/// action (`WaitingFor::DeclareAttackers`, CR 508.1) while that step is still
/// current, and the helper stops as soon as the window it sees is not
/// `Priority`. Passing priority (CR 508.2) is what actually walks the combat
/// steps.
fn advance_through_combat_to(runner: &mut GameRunner, phase: Phase) {
    // A turn has 12 phases/steps (CR 500.1); the bound guards a stuck
    // transition rather than spinning.
    for _ in 0..24 {
        if runner.state().phase == phase {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .declare_blockers(&[])
                    .expect("declaring no blockers should succeed");
            }
            other => panic!("unexpected window while advancing to {phase:?}: {other:?}"),
        }
    }
    panic!(
        "never reached {:?}; stuck in {:?} at {:?}",
        phase,
        runner.state().phase,
        runner.state().waiting_for
    );
}

/// CR 508.1a + CR 611.3a + CR 613.1f — the building-block seam, with no parser
/// dependency: a Layer 6 grant gated on `SourceMatchesFilter(AttackedThisTurn)`
/// is hand-seeded through `CardBuilder::with_static_definition`, so this test
/// discriminates the `commit_attack_declaration` ordering ALONE.
///
/// On the reverted order `commit_attack_declaration` calls `flush_layers`
/// before writing `state.creatures_attacked_this_turn`: the declaration flush
/// evaluates the gate against an empty ledger, leaves the layer cache `Clean`,
/// and nothing re-dirties it before the action returns — so the final assertion
/// fails.
#[test]
fn attacked_this_turn_gate_is_live_at_the_declaration_flush() {
    let gate_static = StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .modifications(vec![ContinuousModification::AddKeyword {
            keyword: Keyword::Indestructible,
        }])
        .condition(StaticCondition::SourceMatchesFilter {
            filter: TargetFilter::Typed(
                TypedFilter::default()
                    .properties(vec![FilterProp::AttackedThisTurn { defender: None }]),
            ),
        });

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gate = scenario
        .add_creature(P0, "Ledger Gate", 2, 2)
        .with_static_definition(gate_static)
        .id();

    let mut runner = scenario.build();
    // `build()` performs no layer pass; force one so the negative below reads a
    // freshly evaluated Layer 6 result rather than an unevaluated board. This is
    // the reach guard that the condition is consulted at all — it holds on both
    // trees, and only the post-declaration positive discriminates.
    evaluate_layers(runner.state_mut());
    assert!(
        !has_indestructible(&runner, gate),
        "reach guard: the typed gate is wired and evaluates FALSE before the creature attacks"
    );

    declare_attack(&mut runner, gate);

    assert!(
        runner.state().creatures_attacked_this_turn.contains(&gate),
        "the per-turn ledger write must have happened during the declaration"
    );
    assert_eq!(
        runner.state().layers_dirty,
        LayersDirty::Clean,
        "PREMISE: the action returned with a Clean layer cache — the keyword read \
         here is the declaration flush's verdict, not a later pass's; if this \
         fails the test's discrimination changed"
    );
    assert!(
        has_indestructible(&runner, gate),
        "CR 508.1a + CR 611.3a + CR 613.1f: the declaration flush must observe \
         the attacked-this-turn ledger"
    );
}

/// CR 508.1a + CR 611.3a + CR 613.1f: the card-level witness. Two
/// revert-failing assertions, one per unit — the pre-attack negative regresses
/// when the grammar/rewrite is reverted (`Unrecognized => true` grants the
/// keyword unconditionally), the immediate post-declaration positive regresses
/// when the `commit_attack_declaration` ledger-before-flush reorder is
/// reverted.
#[test]
fn horrigan_gains_indestructible_only_after_it_attacked() {
    let (scenario, horrigan) = horrigan_scenario();
    let mut runner = scenario.build();

    // U-1 revert probe: `build()` performs no layer pass, so evaluate first.
    evaluate_layers(runner.state_mut());
    assert!(
        !has_indestructible(&runner, horrigan),
        "CR 702.12b: before attacking, the gate is FALSE — no indestructible"
    );

    declare_attack(&mut runner, horrigan);

    // U-2 revert probe: read immediately after the action returns, while the
    // attack trigger still sits on the stack having marked nothing dirty.
    assert!(
        has_indestructible(&runner, horrigan),
        "CR 508.1a: the declaration flush must already see the attacked-this-turn fact"
    );

    // CR 508.1k: the fact is turn-scoped, not combat-scoped — unlike
    // `SourceIsAttacking`, it survives the end of combat.
    advance_through_combat_to(&mut runner, Phase::End);
    assert_eq!(runner.state().phase, Phase::End, "reached the end step");
    assert!(
        has_indestructible(&runner, horrigan),
        "CR 508.1a: 'attacked this turn' persists past end of combat"
    );
}

/// CR 702.12b + CR 704.5g: the granted keyword is the engine's real
/// indestructible — lethal combat damage does not destroy it. Behavioral
/// (not revert-discriminating in either direction); the revert probes live in
/// the sibling tests.
#[test]
fn horrigan_survives_lethal_damage_after_attacking() {
    let (mut scenario, horrigan) = horrigan_scenario();
    let wall = scenario.add_creature(P1, "Blocking Wall", 10, 10).id();
    let mut runner = scenario.build();

    declare_attack(&mut runner, horrigan);
    advance_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(wall, horrigan)])
        .expect("DeclareBlockers should succeed");

    let outcome = runner.combat_damage();
    assert_eq!(
        outcome.state().objects[&wall].damage_marked,
        5,
        "reach guard: the combat damage step ran and Horrigan's 5 power landed"
    );
    assert_eq!(
        outcome.zone_of(horrigan),
        Zone::Battlefield,
        "CR 702.12b + CR 704.5g: 10 damage on a 5/5 with indestructible destroys nothing"
    );
}

/// CR 508.1a: the gate is per-object. Another creature the same player controls
/// attacking does not satisfy Horrigan's own combat history — `SourceMatchesFilter`
/// binds the static's source, not any attacker.
#[test]
fn other_attacker_does_not_grant_horrigan_indestructible() {
    let (mut scenario, horrigan) = horrigan_scenario();
    let bear = scenario.add_creature(P0, "Test Bear", 2, 2).id();
    let mut runner = scenario.build();

    declare_attack(&mut runner, bear);

    assert!(
        runner.state().creatures_attacked_this_turn.contains(&bear),
        "reach guard: the sibling attacker IS in the per-turn ledger"
    );
    assert!(
        !runner
            .state()
            .creatures_attacked_this_turn
            .contains(&horrigan),
        "reach guard: Horrigan did not attack"
    );
    assert!(
        !has_indestructible(&runner, horrigan),
        "CR 508.1a: another creature's attack must not satisfy Horrigan's own gate"
    );
}

/// CR 500.1: "this turn" is turn-scoped. The per-turn ledger is cleared at the
/// start of the next turn and the layer pass re-runs, so the grant drops.
#[test]
fn horrigan_loses_indestructible_on_next_turn() {
    let (scenario, horrigan) = horrigan_scenario();
    let mut runner = scenario.build();

    declare_attack(&mut runner, horrigan);
    assert!(
        has_indestructible(&runner, horrigan),
        "reach guard: the grant was live on the turn Horrigan attacked"
    );

    advance_through_combat_to(&mut runner, Phase::End);
    // From the end step the engine's own helper works: `auto_advance`'s
    // `Phase::End` arm hands out priority (CR 513.1 + CR 117.4).
    runner.advance_to_phase(Phase::PreCombatMain);

    // Five guards that the next turn was actually reached and the game is live
    // (CR 704.5b: an empty-library draw would have ended it before here).
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert_eq!(
        runner.state().active_player,
        P1,
        "the turn passed to the opponent"
    );
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
        "the game is still live: {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().turn_number > 2,
        "a turn boundary was crossed"
    );
    assert!(
        runner.state().creatures_attacked_this_turn.is_empty(),
        "CR 500.1: the per-turn attack ledger cleared at the turn boundary"
    );

    assert!(
        !has_indestructible(&runner, horrigan),
        "CR 500.1 + CR 611.3a: the gate is FALSE again on the next turn"
    );
}
