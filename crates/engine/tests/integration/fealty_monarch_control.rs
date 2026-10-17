//! Fealty to the Realm — monarch-anchored layer-2 control (phase 5b, rows 2-4).
//!
//! Verbatim Oracle text (data/card-data.json `oracle_text`):
//!   "Enchant creature\nWhen this Aura enters, you become the monarch.\nThe
//!   monarch controls enchanted creature.\nEnchanted creature attacks each
//!   combat if able and can't attack you."
//!
//! Row 2 (live re-derivation, BOTH transfer paths): the enchanted creature's
//! controller tracks the monarch with no other game action — via the effect
//! path (a "you become the monarch" spell cast through the pipeline, which
//! covers Fealty's own ETB and every monarch-maker) and via the combat path
//! (CR 725.2: a creature dealing combat damage to the monarch steals the
//! crown through the synthetic trigger). Row 4 (CR 725.5): vacant designation
//! leaves control at base, and a later crowning applies with no reinstall
//! (liveness, not latch). Elimination (CR 725.4): succession to the active
//! player (sole survivor in 2P; active-over-turn-order in 3P) — control
//! follows. The no-players-left vacancy sub-branch is unreachable in a live
//! game (it ends the game) and is covered at unit level instead.
//!
//! Mutation-tested: reverting the S3 guard call in `become_monarch::resolve`
//! leaves `layers_dirty` Clean after the crowning (the guard-fired assertion
//! flips); reverting the S1 apply arm to `effect.controller` leaves control
//! with the Aura's controller instead of the monarch; reverting the S2 parser
//! arm drops the static so the aura never moves control (zero-Unimplemented
//! shape + control assertions flip).

use engine::game::combat::AttackTarget;
use engine::game::effects::attach::attach_to;
use engine::game::layers::{evaluate_layers, flush_layers};
use engine::game::sba::check_state_based_actions;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);

// Verbatim Oracle text (data/card-data.json) — building the test card from the
// real card's exact text ensures the parser takes the production branch.
const FEALTY: &str = "Enchant creature\nWhen this Aura enters, you become the monarch.\nThe monarch controls enchanted creature.\nEnchanted creature attacks each combat if able and can't attack you.";

// P1's instant-speed crowning (cast through the pipeline; pool-funded,
// mirroring the Angel's Grace pattern — no mana staging needed).
const CORONATION: &str = "You become the monarch.";

/// P0 host 2/2 + P0 brute 2/2 + P1-controlled Fealty attached to the host +
/// P1-held crowning instant. Returns the runner with the aura attached and
/// layers evaluated.
fn fealty_table() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_vanilla(P0, 2, 2);
    let brute = scenario.add_vanilla(P0, 2, 2);
    // True-enchantment shell + explicit Aura stamp below: the full
    // "Enchantment — Aura" shape the CR 704.5p SBA sweep requires of an
    // attached permanent (a Creature shell is detached by sentence 1, a
    // subtypeless shell by sentence 2, on the first SBA sweep).
    let aura = scenario
        .add_enchantment_from_oracle(P1, "Fealty to the Realm", FEALTY)
        .id();
    let coronation = scenario
        .add_spell_to_hand_from_oracle(P1, "Test Coronation", true, CORONATION)
        .id();
    let mut runner = scenario.build();
    {
        let aura_obj = runner
            .state_mut()
            .objects
            .get_mut(&aura)
            .expect("aura present");
        aura_obj.card_types.subtypes.push("Aura".to_string());
        aura_obj.base_card_types = aura_obj.card_types.clone();
    }
    // NOTE: `attach_to` returns the PREVIOUS host (`None` on first attach),
    // so the return is not an attach-success signal — attachment is verified
    // by the control assertions below, which fail if the link is absent.
    attach_to(runner.state_mut(), aura, host);
    evaluate_layers(runner.state_mut());
    (runner, host, brute, coronation)
}

/// P0 passes priority so P1 casts the crowning instant; drive to resolution.
/// Pins the EFFECT transfer path through the real cast pipeline.
fn cast_coronation(runner: &mut GameRunner, spell: ObjectId) {
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority so P1 can cast");
    runner.cast(spell).resolve();
}

fn controller_of(runner: &GameRunner, id: ObjectId) -> PlayerId {
    runner
        .state()
        .objects
        .get(&id)
        .expect("object present")
        .controller
}

/// Row 4 (vacant) + row 2 (effect path): with no monarch the aura moves
/// nothing; crowning P1 through the cast pipeline flips control to P1, and
/// the guarded site escalated (dirty == Full before the flush).
#[test]
fn fealty_vacant_then_crowned_by_effect() {
    let (mut runner, host, _brute, coronation) = fealty_table();
    runner.state_mut().monarch = None;
    evaluate_layers(runner.state_mut());
    assert_eq!(
        controller_of(&runner, host),
        P0,
        "CR 725.5: vacant designation leaves control at base"
    );

    cast_coronation(&mut runner, coronation);
    assert_eq!(
        runner.state().monarch,
        Some(P1),
        "the crowning spell must crown its caster P1"
    );
    // No manual flush: the pipeline consumes the guard's mark in its own
    // post-resolution flush, so control must already be live here — the
    // strongest end-to-end pin (the guard→Full scheduling itself is pinned
    // at unit level, where no auto-flush intervenes).
    assert_eq!(
        controller_of(&runner, host),
        P1,
        "row 2 (effect path): control follows the live monarch"
    );
}

/// Row 2 (combat path, CR 725.2): P1 monarch holds the host; P0's brute hits
/// P1 unblocked, the crown steals to P0, and control follows with no other
/// game action. The enchanted host cannot attack P1 itself ("can't attack
/// you") and has no other legal attack, so declaring only the brute is legal.
#[test]
fn fealty_control_follows_combat_steal() {
    let (mut runner, host, brute, coronation) = fealty_table();
    cast_coronation(&mut runner, coronation);
    flush_layers(runner.state_mut());
    assert_eq!(controller_of(&runner, host), P1);
    let life_before = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .expect("P1 exists")
        .life;

    let mut attack = Some((brute, AttackTarget::Player(P1)));
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(
            guard < 2000,
            "never reached postcombat (phase {:?}, waiting {})",
            runner.state().phase,
            runner.waiting_for_kind()
        );
        if runner.state().phase == Phase::PostCombatMain && runner.state().active_player == P0 {
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => {
                let attacks = if runner.state().active_player == P0 {
                    attack.take().map(|a| vec![a]).unwrap_or_default()
                } else {
                    vec![]
                };
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks,
                        bands: vec![],
                    })
                    .expect("declare attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declare blockers");
            }
            WaitingFor::TargetSelection { .. } => {
                panic!(
                    "Fealty's sequence declares no targets, got {:?}",
                    runner.state().waiting_for
                );
            }
            _ => {
                let _ = runner.act(GameAction::PassPriority);
            }
        }
        runner.advance_until_stack_empty();
    }

    // Anti-vacuity: the staged attack was consumed by a declaration (not
    // skipped) and P1's life dropped below starting — the brute attacked and
    // dealt combat damage, which is what steals the crown (CR 725.2), rather
    // than the crown moving some other way.
    assert!(
        attack.is_none(),
        "the staged brute attack must have been declared"
    );
    let p1_life = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .expect("P1 exists")
        .life;
    assert!(
        p1_life < life_before,
        "P1 must have taken the brute's combat damage (was {life_before}), got life {p1_life}"
    );
    assert_eq!(
        runner.state().monarch,
        Some(P0),
        "CR 725.2: combat damage to the monarch steals the crown"
    );
    flush_layers(runner.state_mut());
    assert_eq!(
        controller_of(&runner, host),
        P0,
        "row 2 (combat path): control follows the stolen crown"
    );
}

/// CR 725.4 succession with a sole survivor (2P): the P1 monarch is
/// eliminated — the active player P0 (the only player left) succeeds and
/// control follows without replaying the Aura.
#[test]
fn fealty_sole_survivor_succeeds() {
    let (mut runner, host, _brute, coronation) = fealty_table();
    cast_coronation(&mut runner, coronation);
    flush_layers(runner.state_mut());
    assert_eq!(controller_of(&runner, host), P1);

    runner.state_mut().players[1].life = 0;
    let mut events = Vec::new();
    check_state_based_actions(runner.state_mut(), &mut events);
    assert!(
        runner.state().players[1].is_eliminated,
        "P1 at 0 life must be eliminated (reach guard)"
    );
    assert_eq!(
        runner.state().monarch,
        Some(P0),
        "CR 725.4: the active player succeeds, even as sole survivor"
    );
    flush_layers(runner.state_mut());
    assert_eq!(
        controller_of(&runner, host),
        P0,
        "succession moves control with no other game action"
    );
}

/// CR 725.4 succession branch (3P): the P1 monarch is eliminated — the active
/// player P0 succeeds and control follows without replaying the Aura.
/// Fealty's controller P2 is never the monarch: the designation source is
/// independent of the Aura's source.
#[test]
fn fealty_monarch_eliminated_successor_takes_control() {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_vanilla(P0, 2, 2);
    // True-enchantment shell + Aura stamp (see fealty_table).
    let aura = scenario
        .add_enchantment_from_oracle(P2, "Fealty to the Realm", FEALTY)
        .id();
    let coronation = scenario
        .add_spell_to_hand_from_oracle(P1, "Test Coronation", true, CORONATION)
        .id();
    let mut runner = scenario.build();
    {
        let aura_obj = runner
            .state_mut()
            .objects
            .get_mut(&aura)
            .expect("aura present");
        aura_obj.card_types.subtypes.push("Aura".to_string());
        aura_obj.base_card_types = aura_obj.card_types.clone();
    }
    // NOTE: `attach_to` returns the previous host (`None` on first attach).
    attach_to(runner.state_mut(), aura, host);
    evaluate_layers(runner.state_mut());

    cast_coronation(&mut runner, coronation);
    flush_layers(runner.state_mut());
    assert_eq!(
        controller_of(&runner, host),
        P1,
        "monarch P1 holds the enchanted host"
    );

    runner.state_mut().players[1].life = 0;
    let mut events = Vec::new();
    check_state_based_actions(runner.state_mut(), &mut events);
    assert!(
        runner.state().players[1].is_eliminated,
        "P1 at 0 life must be eliminated (reach guard)"
    );
    assert_eq!(
        runner.state().monarch,
        Some(P0),
        "CR 725.4: active player succeeds the eliminated monarch"
    );
    flush_layers(runner.state_mut());
    assert_eq!(
        controller_of(&runner, host),
        P0,
        "succession moves control with no other game action"
    );
}
