//! Pipeline-level regression for the Cascade (and the wider `WasCast`
//! cast-origin intervening-`if`) class of cast-triggered abilities.
//!
//! CR 702.85a: Cascade is a triggered ability that functions only while the
//! spell with cascade is on the stack. CR 603.4: an intervening-`if` clause is
//! re-checked when the triggered ability *resolves* (`stack.rs`). Cascade's
//! synthesized trigger carries a redundant `WasCast` intervening-`if`; the
//! source spell is still on the stack at resolution, so the re-check must read
//! its live `cast_from_zone`.
//!
//! The existing `cascade.rs` unit tests call `cascade::resolve()` directly and
//! therefore bypass the `resolve_top` intervening-`if` gate — which is why the
//! regression (`clear_post_collection_transients` wiping `cast_from_zone` for a
//! spell on the stack) shipped invisibly. This test drives the full `apply()`
//! pipeline through to resolution so the gate is exercised.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{Effect, EffectKind};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastOfferKind, CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// CR 702.85a + CR 603.4: a sorcery with Cascade cast through the real
/// pipeline must, when its trigger resolves above it on the stack, exile from
/// the top of the library until it hits a lower-MV nonland card and offer to
/// cast it. On the regression the `WasCast` re-check read a wiped
/// `cast_from_zone` and silently dropped the trigger.
#[test]
fn cascade_trigger_resolves_through_pipeline_and_exiles_hit() {
    // {2}{R} = MV 3.
    let cascade_cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 2,
    };
    // {R} = MV 1 (nonland, below the cascade spell's MV 3 → a cascade hit).
    let hit_cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 0,
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // A pool covering {2}{R}: one Red pip + two generic, paid here as three Red
    // units (the colored pip plus two generics).
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
        ],
    );

    // Bare Oracle "Cascade" parses to `Keyword::Cascade`; cast-trigger synthesis
    // fires for a sorcery (no type restriction).
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Cascade Sorcery", false, "Cascade")
        .with_mana_cost(cascade_cost)
        .id();
    // The cascade hit on top of P0's library: a nonland instant of MV 1.
    let hit = scenario
        .add_spell_to_library_top(P0, "Cheap Hit", true)
        .with_mana_cost(hit_cost)
        .id();

    let mut runner = scenario.build();

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast");

    // The cascade trigger sits above the spell on the stack and resolves first.
    runner.act(GameAction::PassPriority).expect("p0 pass");
    let result = runner.act(GameAction::PassPriority).expect("p1 pass");

    // PRIMARY: on the regression the trigger is dropped, so `hit` stays in the
    // library; with the fix cascade exiles it.
    assert_eq!(
        runner.state().objects[&hit].zone,
        Zone::Exile,
        "cascade must exile the hit card; if it is still in the library the \
         WasCast intervening-if was dropped at resolution"
    );

    // SECONDARY: the cascade `EffectResolved` event is only emitted if the
    // trigger actually resolved.
    let cascade_kind = EffectKind::from(&Effect::Cascade);
    assert!(
        result
            .events
            .iter()
            .any(|e| matches!(e, GameEvent::EffectResolved { kind, .. } if *kind == cascade_kind)),
        "the resolving action must emit an EffectResolved {{ kind: Cascade }} event; events = {:?}",
        result.events
    );

    // OPTIONAL: the cascade cast-offer for the hit card is still pending (we do
    // not answer it, which keeps `hit` in Exile for the primary assertion).
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CastOffer {
                kind: CastOfferKind::Cascade { hit_card, .. },
                ..
            } if hit_card == hit
        ),
        "cascade must leave a CastOffer for the exiled hit; waiting_for = {:?}",
        runner.state().waiting_for
    );
}
