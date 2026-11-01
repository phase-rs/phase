//! Immediate chosen-player attack conditions (Matt R7-F5).
//!
//! The attack-endpoint bridge maps every indexed `ChosenPlayer` to
//! `PlayerScope::Target`, but only delayed-trigger creation stamps the
//! matching target (Faramir's path). An IMMEDIATE Choose→conditional has no
//! stamp: ordinary choice completion writes only `chosen_players`, so
//! `Target` consults no player (no declared targets) or an unrelated
//! declared target — and under `Not` the false leaf selects the wrong
//! branch. The fix refuses the lowering outside the matching stamped
//! delayed context, so the immediate guard fails closed to a visible
//! `unparsed_condition` marker (never a silent misfire, never a silently
//! unconditional body).
//!
//! Both probes use the scope-free "that player" V1 anaphor: it mints
//! `ChosenPlayer { index: 0 }` with no scope gate, so pre-fix it sails
//! through the bridge into the misfire. (The scope-gated "they" pronoun
//! declines without scope and already gaps pre-fix — orthogonal.)
//!
//! Ledger entries are seeded directly: the finding is about WHICH player the
//! condition consults, not about ledger recording (covered by the combat-path
//! tests).

use engine::game::scenario::{GameRunner, GameScenario};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);

const PROBE: &str =
    "Choose an opponent. You draw a card if that player didn't attack you that turn.";
const PROBE_TARGETED: &str =
    "Choose an opponent. Target player draws a card if that player didn't attack you that turn.";

fn free_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![],
        generic: 0,
    }
}

fn stock_libraries(scenario: &mut GameScenario) {
    for _ in 0..8 {
        scenario.add_spell_to_library_top(P0, "Pad", false);
        scenario.add_spell_to_library_top(P1, "Pad", false);
        scenario.add_spell_to_library_top(P2, "Pad", false);
    }
}

fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| o.controller == player && o.zone == Zone::Hand)
        .count()
}

/// True when parsing `text` fails the immediate guard closed to a visible
/// `unparsed_condition` marker anywhere in the chain (mirrors the choiceless
/// probe's walker in oracle_tests).
fn reports_unparsed_condition(text: &str) -> bool {
    let parsed = parse_oracle_text(text, "Probe", &[], &["Sorcery".to_string()], &[]);
    for ability in &parsed.abilities {
        let mut cursor = Some(ability);
        while let Some(link) = cursor {
            if matches!(
                link.effect.as_ref(),
                Effect::Unimplemented { name, .. } if name == "unparsed_condition"
            ) {
                return true;
            }
            cursor = link.sub_ability.as_deref();
        }
    }
    false
}

/// Seed "attacker hit defender directly this turn" on the retrospective
/// ledger the attack predicate reads.
fn seed_direct_attack(runner: &mut GameRunner, attacker: PlayerId, defender: PlayerId) {
    runner
        .state_mut()
        .attacked_players_directly_this_turn
        .entry(attacker)
        .or_default()
        .insert(defender);
}

/// Answer the Choose-opponent prompt with `who`, returning after asserting
/// the prompt's reach guards.
fn choose_opponent(runner: &mut GameRunner, who: PlayerId) {
    let WaitingFor::NamedChoice { options, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "choosing the opponent must pause on NamedChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    let slot = who.0.to_string();
    assert!(
        options.contains(&slot),
        "reach guard: {who:?} must be a legal choice, got {options:?}"
    );
    runner
        .act(GameAction::ChooseOption { choice: slot })
        .expect("the choice must succeed");
}

/// CR 109.4 + CR 608.2c (Matt R7-F5): an immediate Choose→conditional with
/// NO declared targets consults no player — `Target` reads nothing, the
/// leaf is false for the wrong reason, and `Not` draws. P1 attacked P0, so
/// a working condition would withhold the draw; pre-fix the spell draws
/// anyway. Post-fix the lowering is refused: no draw, with a visible
/// `unparsed_condition` marker.
#[test]
fn immediate_chosen_player_condition_with_no_target_fails_closed() {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    let probe = scenario
        .add_spell_to_hand_from_oracle(P0, "Probe", false, PROBE)
        .with_mana_cost(free_cost())
        .id();
    let mut runner = scenario.build();

    seed_direct_attack(&mut runner, P1, P0);
    assert!(
        runner
            .state()
            .has_attacked_player_directly_this_turn(P1, P0),
        "reach guard: the ledger must record P1's attack on P0"
    );

    let hand_before = hand_size(&runner, P0);
    runner.cast(probe).resolve();
    choose_opponent(&mut runner, P1);
    runner.advance_until_stack_empty();

    // P0's hand starts as just the probe: cast consumes it (-1), so the
    // post-fix count is `hand_before - 1` (no draw). Pre-fix the misfire
    // draws back up to `hand_before`.
    assert_eq!(
        hand_size(&runner, P0),
        hand_before - 1,
        "pre-fix the condition consults no player and the Not draws; post-fix \
         the refused guard draws nothing"
    );
    assert!(
        reports_unparsed_condition(PROBE),
        "the refused lowering must stay visible as unparsed_condition — never \
         a silent misfire, never a silently unconditional body"
    );
}

/// CR 109.4 + CR 608.2c (Matt R7-F5, distinct players): the same immediate
/// composition WITH a declared player target consults the UNRELATED target.
/// Chosen P1 attacked but declared P2 is clean: pre-fix the condition reads
/// P2's clean record (leaf false → `Not` true) and draws — crediting P1
/// with P2's record. Post-fix the lowering is refused: no draw, with a
/// visible `unparsed_condition` marker.
#[test]
fn immediate_chosen_player_condition_ignores_unrelated_declared_target() {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    let probe = scenario
        .add_spell_to_hand_from_oracle(P0, "Targeted Probe", false, PROBE_TARGETED)
        .with_mana_cost(free_cost())
        .id();
    let mut runner = scenario.build();

    // Chosen P1 attacked; declared P2 never did. The condition must consult
    // the chosen player — pre-fix it consults P2 instead.
    seed_direct_attack(&mut runner, P1, P0);
    assert!(
        !runner
            .state()
            .has_attacked_player_directly_this_turn(P2, P0),
        "reach guard: P2's record must be clean"
    );

    let hand_before = hand_size(&runner, P2);
    runner.cast(probe).target_player(P2).resolve();
    choose_opponent(&mut runner, P1);
    runner.advance_until_stack_empty();

    assert_eq!(
        hand_size(&runner, P2),
        hand_before,
        "pre-fix the condition consults P2's clean record and draws; post-fix \
         the refused guard draws nothing"
    );
    assert!(
        reports_unparsed_condition(PROBE_TARGETED),
        "the refused lowering must stay visible as unparsed_condition"
    );
}
