//! Regression for issue #6877: Krark, the Thumbless must still copy "that spell"
//! after an earlier lose trigger has bounced it off the stack.
//!
//! https://github.com/phase-rs/phase/issues/6877

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

const KRARK: &str = "Whenever you cast an instant or sorcery spell, flip a coin. \
    If you lose the flip, return that spell to its owner's hand. \
    If you win the flip, copy that spell, and you may choose new targets for the copy.";

const DRAW_SPELL: &str = "Draw a card.";
const BOLT_SPELL: &str = "~ deals 3 damage to any target.";

fn floating_colorless(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn library_len(state: &engine::types::game_state::GameState, player: PlayerId) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.library.len())
        .unwrap_or(0)
}

fn setup_two_krarks_and_spell(
    seed: u64,
    spell_name: &str,
    spell_oracle: &str,
) -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, seed);
    scenario.at_phase(Phase::PreCombatMain);
    // CR 704.5j: two non-legendary creatures so the legend rule does not
    // collapse the pair.
    scenario.add_creature_from_oracle(P0, "Krark A", 2, 2, KRARK);
    scenario.add_creature_from_oracle(P0, "Krark B", 2, 2, KRARK);
    for i in 0..8 {
        scenario.add_spell_to_library_top(P0, &format!("Library {i}"), true);
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, spell_name, true, spell_oracle)
        .id();
    scenario.with_mana_pool(P0, floating_colorless(10));
    (scenario, spell)
}

/// Two same-controller Krark triggers surface `OrderTriggers` during CastSpell,
/// which `SpellCast::commit` panics on. Drive the same `apply()` pipeline until
/// Priority or OrderTriggers.
fn commit_cast(runner: &mut GameRunner, spell: ObjectId, target: Option<TargetRef>) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast must be accepted");
    for _ in 0..32 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. } | WaitingFor::OrderTriggers { .. }
        ) {
            return;
        }
        if matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ) {
            runner
                .act(GameAction::ChooseTarget {
                    target: target.clone(),
                })
                .expect("declared target must be accepted");
            continue;
        }
        if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
            runner
                .act(GameAction::PassPriority)
                .expect("pool-funded remainder must pay");
            continue;
        }
        panic!(
            "unexpected waiting_for while committing cast: {:?}",
            runner.state().waiting_for
        );
    }
    panic!("cast did not reach Priority or OrderTriggers");
}

/// Drain CR 603.3b ordering, then pass priority until the stack shrinks or a
/// copy-retarget prompt appears. Collects `ActionResult.events` so coin-flip
/// reach-guards can be asserted.
fn resolve_until_stack_shrinks(runner: &mut GameRunner) -> Vec<GameEvent> {
    drain_order_triggers_with_identity(runner.state_mut());
    let initial_stack_len = runner.state().stack.len();
    let mut events = Vec::new();
    for _ in 0..32 {
        drain_order_triggers_with_identity(runner.state_mut());
        if runner.state().stack.len() < initial_stack_len {
            break;
        }
        if matches!(runner.state().waiting_for, WaitingFor::CopyRetarget { .. }) {
            break;
        }
        match runner.act(GameAction::PassPriority) {
            Ok(result) => events.extend(result.events),
            Err(_) => break,
        }
    }
    events
}

fn reseed(runner: &mut GameRunner, seed: u64) {
    runner.state_mut().rng = ChaCha20Rng::seed_from_u64(seed);
}

fn saw_coin(events: &[GameEvent], won: bool) -> bool {
    events
        .iter()
        .any(|event| matches!(event, GameEvent::CoinFlipped { won: w, .. } if *w == won))
}

fn saw_spell_copied(events: &[GameEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, GameEvent::SpellCopied { .. }))
}

/// CR 705.2 + CR 707.10 + CR 400.7: lose then win still copies the bounced spell.
#[test]
fn win_after_bounce_creates_a_copy() {
    let (scenario, spell) = setup_two_krarks_and_spell(42, "Draw Spell", DRAW_SPELL);
    let mut runner = scenario.build();
    commit_cast(&mut runner, spell, None);
    drain_order_triggers_with_identity(runner.state_mut());
    assert!(
        runner.state().stack.len() >= 3,
        "spell plus two Krark triggers must be on the stack"
    );

    reseed(&mut runner, 1);
    let lose_events = resolve_until_stack_shrinks(&mut runner);
    assert!(
        saw_coin(&lose_events, false),
        "first trigger must lose the flip: {lose_events:?}"
    );
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Hand,
        "lose must bounce the original to hand"
    );

    let lib_before = library_len(runner.state(), P0);
    reseed(&mut runner, 0);
    let win_events = resolve_until_stack_shrinks(&mut runner);
    assert!(
        saw_coin(&win_events, true),
        "second trigger must win the flip: {win_events:?}"
    );
    assert!(
        saw_spell_copied(&win_events) || library_len(runner.state(), P0) < lib_before,
        "win after bounce must create a copy (SpellCopied or a resolved draw); events={win_events:?}"
    );
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Hand,
        "original remains in hand after the leftover win"
    );
}

/// Sibling control: winning while the spell is still on the stack still copies.
#[test]
fn win_while_on_stack_still_copies() {
    let (scenario, spell) = setup_two_krarks_and_spell(42, "Draw Spell", DRAW_SPELL);
    let mut runner = scenario.build();
    commit_cast(&mut runner, spell, None);
    drain_order_triggers_with_identity(runner.state_mut());

    let lib_before = library_len(runner.state(), P0);
    reseed(&mut runner, 0);
    let win_events = resolve_until_stack_shrinks(&mut runner);
    assert!(
        saw_coin(&win_events, true),
        "first trigger must win the flip: {win_events:?}"
    );
    assert!(
        saw_spell_copied(&win_events) || library_len(runner.state(), P0) < lib_before,
        "win while on stack must copy"
    );

    reseed(&mut runner, 1);
    let lose_events = resolve_until_stack_shrinks(&mut runner);
    assert!(
        saw_coin(&lose_events, false),
        "second trigger must lose the flip: {lose_events:?}"
    );
    assert_eq!(runner.state().objects[&spell].zone, Zone::Hand);
}

/// CR 707.10c: a targeted leftover win still offers CopyRetarget.
#[test]
fn win_after_bounce_offers_copy_retarget() {
    let (scenario, spell) = setup_two_krarks_and_spell(42, "Lightning Bolt", BOLT_SPELL);
    let mut runner = scenario.build();
    commit_cast(&mut runner, spell, Some(TargetRef::Player(P1)));
    drain_order_triggers_with_identity(runner.state_mut());

    reseed(&mut runner, 1);
    let lose_events = resolve_until_stack_shrinks(&mut runner);
    assert!(saw_coin(&lose_events, false));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Hand);

    reseed(&mut runner, 0);
    let win_events = resolve_until_stack_shrinks(&mut runner);
    assert!(
        saw_coin(&win_events, true),
        "leftover win must flip: {win_events:?}"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::CopyRetarget { .. }),
        "targeted leftover copy must halt on CopyRetarget, got {:?}",
        runner.state().waiting_for
    );
}
