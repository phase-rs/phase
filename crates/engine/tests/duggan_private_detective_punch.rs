//! Runtime proof for Duggan, Private Detective (cluster 58, Universes Beyond — WHO).
//!
//! Duggan's activated ability — "The Most Important Punch in History — {1}{G}, {T}:
//! Duggan deals damage equal to twice its power to another target creature.
//! Activate only once." — was UNSUPPORTED on upstream/main because the CR 207.2d
//! flavor-word label ("The Most Important Punch in History", 6 words) exceeded the
//! 4-word ability-word cap on the activated-cost path, so the cost parsed as
//! `Composite([Unimplemented(...), Tap])`. Widening the cost-label strip to the
//! flavor-word cap lets the cost parse, after which the rest of the card runs on
//! existing primitives (Maro-style CDA, EntersOrAttacks trigger, OnlyOnce limit,
//! `Multiply(2, Power{Source})` damage).
//!
//! This test drives the real activation pipeline: with a 4-card hand the CDA makes
//! Duggan 4/4 (CR 208.2a), the once-per-game punch resolves dealing 2 x 4 = 8
//! damage to the targeted opponent creature (CR 208.1), and the OnlyOnce counter
//! records the single activation (CR 602.5b). Revert-proof: if the cost-label fix
//! is reverted the cost is `Unimplemented`, the ability cannot be activated, and
//! `activate(..).resolve()` panics before reaching the damage assertion.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const DUGGAN_ORACLE: &str =
    "Duggan's power and toughness are each equal to the number of cards in \
your hand.\nWhenever Duggan enters or attacks, investigate.\nThe Most Important Punch in History \
\u{2014} {1}{G}, {T}: Duggan deals damage equal to twice its power to another target creature. \
Activate only once.";

#[test]
fn duggan_punch_deals_twice_power_and_records_once_per_game() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P0 hand: 4 cards → the CDA sets Duggan to 4/4 (CR 208.2a). Card identities
    // are irrelevant; only the hand size feeds `HandSize { Controller }`.
    scenario.with_cards_in_hand(P0, &["Memo A", "Memo B", "Memo C", "Memo D"]);
    // Fund {1}{G} from the pool — the activation driver does not model source
    // auto-tap, so two green mana cover the {G} pip and the {1} generic.
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
        ],
    );

    // Opponent creature with toughness 10 survives 8 damage, so `damage_marked`
    // stays observable (SBA does not destroy it). Its own power (2) differs from
    // the expected 8 so a wrong amount referent cannot coincidentally pass.
    let target = scenario.add_vanilla(P1, 2, 10);
    // Duggan base 0/0; the CDA overrides P/T to the controller's hand size.
    let duggan = scenario
        .add_creature_from_oracle(P0, "Duggan, Private Detective", 0, 0, DUGGAN_ORACLE)
        .id();

    let mut runner = scenario.build();

    // CDA proof: force a layer pass and confirm Duggan is 4/4 from the 4-card hand.
    {
        let st = runner.state_mut();
        st.layers_dirty.mark_full();
        evaluate_layers(st);
    }
    assert_eq!(
        runner.state().objects[&duggan].power,
        Some(4),
        "CR 208.2a: Duggan's power equals your hand size (4 cards → 4)"
    );
    assert_eq!(
        runner.state().objects[&duggan].toughness,
        Some(4),
        "CR 208.2a: Duggan's toughness equals your hand size (4 cards → 4)"
    );

    // Activate the once-per-game punch targeting the opponent creature. This only
    // succeeds because the {1}{G}, {T} cost now parses (the fix under test).
    let outcome = runner.activate(duggan, 0).target_object(target).resolve();

    // CR 208.1 + Multiply(2, Power{Source}): 2 × 4 = 8 damage marked on the target.
    assert_eq!(
        outcome.state().objects[&target].damage_marked,
        8,
        "Duggan deals twice its power (2 × 4 = 8) to the targeted creature, \
         not 0 (dropped referent) and not the target's own power (2)"
    );

    // CR 602.5b: "Activate only once" recorded the single per-game activation.
    assert_eq!(
        outcome
            .state()
            .activated_abilities_this_game
            .get(&(duggan, 0))
            .copied(),
        Some(1),
        "Activate only once must record exactly one per-game activation (CR 602.5b)"
    );
}
