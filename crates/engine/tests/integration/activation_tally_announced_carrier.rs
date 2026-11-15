//! Resolution tally over the announced carrier (CR 608.2b + CR 602.5c +
//! CR 608.2c).
//!
//! Dragon Whelp's rider — "If this ability has been activated four or more
//! times this turn, sacrifice this creature at the beginning of the next end
//! step" — tallies `AbilityUseTally::Activated` from the CR 602.2a
//! announcement journal. When Dress Down ("Creatures lose all abilities.")
//! resolves between announcement and resolution, the live board no longer
//! carries the ability text: a tally that re-derives the usage key from the
//! live slot reads count 0 and never arms. Reading the carrier's
//! `announced_usage_key` instead (falling back to the live derivation only
//! when no announcement was captured) keeps the count at four and the
//! sacrifice arms.
//!
//! The sub-node propagation (`apply_parent_chain_context`) carries the same
//! announced key down the chain with a no-clobber guard, so a tally
//! evaluated on a chained sub-node reads the parent acquisition's budget.
//!
//! Verbatim Oracle texts (Scryfall): Dragon Whelp — "Flying / {R}: This
//! creature gets +1/+0 until end of turn. If this ability has been activated
//! four or more times this turn, sacrifice this creature at the beginning of
//! the next end step." Dress Down — "Flash / When this enchantment enters,
//! draw a card. / Creatures lose all abilities. / At the beginning of the
//! end step, sacrifice this enchantment."
//!
//! Revert detectors: restoring the live re-derivation leaves the Whelp on
//! the battlefield after four activations under Dress Down (the main test
//! fails); the three-activation hostile pins the `n = 4` boundary so a fix
//! that never arms cannot satisfy the pair.

use engine::game::restrictions::ability_usage_key;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::CastPaymentMode;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DRAGON_WHELP_ORACLE: &str = "Flying\n{R}: This creature gets +1/+0 until end of turn. If this ability has been activated four or more times this turn, sacrifice this creature at the beginning of the next end step.";
const DRESS_DOWN_ORACLE: &str = "Flash\nWhen this enchantment enters, draw a card.\nCreatures lose all abilities.\nAt the beginning of the end step, sacrifice this enchantment.";

/// Dress Down staged in hand (an Enchantment core type, so it enters on
/// resolution) plus a Whelp, mana for four `{R}` activations and one
/// `{1}{U}` flash cast, and a stocked library for the enter-draw.
fn dress_down_board() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Island", "Island", "Island", "Island"]);
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
            .chain((0..2).map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![])))
            .collect(),
    );
    let whelp = scenario
        .add_creature_from_oracle(P0, "Dragon Whelp", 2, 3, DRAGON_WHELP_ORACLE)
        .id();
    let dress_down = scenario
        .add_spell_to_hand_from_oracle(P0, "Dress Down", false, DRESS_DOWN_ORACLE)
        .as_enchantment()
        .id();
    (scenario.build(), whelp, dress_down)
}

/// Announce (but do not resolve) `n` Whelp activations, leaving all of them
/// on the stack above an empty-resolution baseline.
fn announce_n(runner: &mut GameRunner, whelp: ObjectId, n: usize) {
    for nth in 1..=n {
        runner
            .act(GameAction::ActivateAbility {
                source_id: whelp,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap_or_else(|e| panic!("activation {nth} must be payable: {e:?}"));
    }
    assert_eq!(
        runner.state().stack.len(),
        n,
        "all {n} announcements must sit on the stack unresolved"
    );
}

/// Cross combat and the end step, resolving whatever fires (mirrors the
/// issue-8388 harness: priority passes cannot cross DeclareAttackers, and
/// only `advance_until_stack_empty` resolves a fired end-step trigger).
fn cross_to_resolved_end_step(runner: &mut GameRunner) {
    runner.advance_to_combat();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        })
        .expect("declare no attackers to cross combat");
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().phase,
        Phase::End,
        "the delayed sacrifice is timed to the next end step (CR 603.7a)"
    );
}

/// CR 608.2b + CR 602.5c: four announced activations are tallied through
/// Dress Down's text removal — the announced carrier, not the live board,
/// names the acquisition, so the rider still arms and the Whelp is
/// sacrificed at the next end step.
#[test]
fn whelp_tally_reads_announced_key_through_dress_down() {
    let (mut runner, whelp, dress_down) = dress_down_board();
    announce_n(&mut runner, whelp, 4);

    // Reach-guard: all four announcements journaled under one key before
    // Dress Down arrives.
    let key = ability_usage_key(runner.state(), whelp, 0).expect("the slot resolves");
    assert_eq!(
        runner
            .state()
            .activated_abilities_this_turn
            .get(&key)
            .copied(),
        Some(4),
        "four announcements must be recorded on the CR 602.2a ledger"
    );

    // Dress Down (flash) cast on top, then resolve the whole stack: Dress
    // Down strips the text first, then each Whelp activation resolves with
    // its announced identity intact.
    runner.cast(dress_down).resolve();
    runner.advance_until_stack_empty();
    assert!(
        runner.state().objects[&whelp].abilities.is_empty(),
        "reach-guard: Dress Down must have removed the Whelp's abilities, got {:?}",
        runner.state().objects[&whelp].abilities
    );
    assert!(
        !runner.state().delayed_triggers.is_empty(),
        "the fourth resolution must arm the delayed end-step sacrifice even with the text gone"
    );
    assert_eq!(
        runner.state().objects[&whelp].zone,
        Zone::Battlefield,
        "CR 603.7a: the sacrifice is delayed, so the Whelp is still around now"
    );

    cross_to_resolved_end_step(&mut runner);

    assert_eq!(
        runner.state().objects[&whelp].zone,
        Zone::Graveyard,
        "the armed delayed trigger must sacrifice the Whelp at the end step"
    );
}

/// Boundary hostile (paired with the main test): three activations stay
/// below the printed four-or-more threshold, so Dress Down plus resolution
/// arms nothing and the Whelp survives its end step.
#[test]
fn three_activations_then_dress_down_spares_the_whelp() {
    let (mut runner, whelp, dress_down) = dress_down_board();
    announce_n(&mut runner, whelp, 3);

    runner.cast(dress_down).resolve();
    runner.advance_until_stack_empty();

    // Reach-guard: the ability really resolved three times through the text
    // loss (the tally path was reached, not bypassed).
    let key = ability_usage_key(runner.state(), whelp, 0);
    assert!(
        key.is_none(),
        "with Dress Down live the slot no longer resolves — the announced carrier, not a live re-derivation, is the only identity left"
    );
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "three activations is below the printed four-or-more threshold, so the \
         delayed sacrifice must never be installed; got {:?}",
        runner.state().delayed_triggers
    );

    cross_to_resolved_end_step(&mut runner);

    assert_eq!(
        runner.state().objects[&whelp].zone,
        Zone::Battlefield,
        "the Whelp must survive below its threshold"
    );
}
