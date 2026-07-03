//! Regression for issue #4357 — Molecule Man must offer miracle {0} when the
//! controller draws a nonland card as their first draw of the turn, and a
//! accepted miracle cast must use the granted zero cost at cast time.
//!
//! https://github.com/phase-rs/phase/issues/4357

use engine::game::effects::draw::resolve as resolve_draw;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, CastingVariant, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const MOLECULE_MAN: &str = "Nonland cards in your hand have miracle {0}. (You may cast a card for its miracle cost when you draw it if it's the first card you drew this turn.)";

fn draw_one_for_controller(runner: &mut engine::game::scenario::GameRunner) {
    let draw = ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
        Vec::new(),
        ObjectId(0),
        P0,
    );
    let mut events = Vec::new();
    resolve_draw(runner.state_mut(), &draw, &mut events).expect("draw resolves");
}

#[test]
fn molecule_man_offers_miracle_on_first_nonland_draw() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Molecule Man", 5, 5)
        .from_oracle_text(MOLECULE_MAN);
    scenario
        .add_spell_to_library_top(P0, "Shock", false)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 1,
        });
    scenario.add_card_to_library_top(P0, "Island");

    let mut runner = scenario.build();

    draw_one_for_controller(&mut runner);

    assert_eq!(
        runner.state().pending_miracle_offers.len(),
        1,
        "first draw of a nonland must queue a miracle offer under Molecule Man"
    );
    assert!(
        runner.state().pending_miracle_offers[0]
            .cost
            .is_without_paying_mana(),
        "miracle {{0}} must be a zero-cost alternative, got {:?}",
        runner.state().pending_miracle_offers[0].cost
    );
}

/// CR 702.94a + CR 118.9a: Accepting a granted miracle cast must use the
/// effective hand keyword at cast preparation time, not only when enqueueing
/// the reveal offer.
#[test]
fn molecule_man_accepted_miracle_cast_uses_granted_zero_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Molecule Man", 5, 5)
        .from_oracle_text(MOLECULE_MAN);
    let drawn_spell = scenario
        .add_spell_to_library_top(P0, "GrantedMiracleSpell", false)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 5,
        })
        .with_ability(Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        })
        .id();

    let mut runner = scenario.build();
    draw_one_for_controller(&mut runner);

    let offer = runner.state().pending_miracle_offers[0].clone();
    assert_eq!(offer.object_id, drawn_spell);
    let card_id = runner.state().objects[&drawn_spell].card_id;

    runner.state_mut().waiting_for = WaitingFor::MiracleReveal {
        player: P0,
        object_id: drawn_spell,
        cost: offer.cost,
    };
    runner.state_mut().pending_miracle_offers.clear();

    runner
        .act(GameAction::CastSpellAsMiracle {
            object_id: drawn_spell,
            card_id,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("miracle reveal accept should succeed");

    runner.act(GameAction::PassPriority).expect("P0 pass");
    runner.act(GameAction::PassPriority).expect("P1 pass");

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::CastOffer { .. }),
        "miracle trigger should surface a cast offer, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::CastSpellAsMiracle {
            object_id: drawn_spell,
            card_id,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("miracle cast should succeed without paying printed cost");

    let entry = runner.state().stack.last().expect("spell on stack");
    match &entry.kind {
        StackEntryKind::Spell {
            casting_variant, ..
        } => {
            assert_eq!(
                *casting_variant,
                CastingVariant::Miracle,
                "stack entry should record CastingVariant::Miracle"
            );
        }
        other => panic!("expected Spell on stack, got {other:?}"),
    }
    assert!(
        runner.state().players[0].mana_pool.mana.is_empty(),
        "granted miracle {{0}} must not require mana payment"
    );
}
