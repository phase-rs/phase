//! Nicol Bolas, Dragon-God — "Nicol Bolas has all loyalty abilities of all
//! other planeswalkers on the battlefield." (CR 613.1f Layer-6 ability grant;
//! CR 606.2 loyalty category; CR 606.3 timing + frequency; CR 606.4 payment;
//! CR 607.5 acquired-pair linkage; CR 611.3a live tracking.)
//!
//! The kind-selected loyalty grant REFUSES loudly at parse emission: the
//! donated linked pairs (2024-01-12 Bolas ruling) are unrepresentable in the
//! source-level exile ledger, so the sentence stays a `static_structure` gap
//! rather than over-granting. This file proves the refusal end-to-end against
//! the REAL parse → layer → activation pipeline.
//!
//! Lead conditions:
//!   #1 — grant refused, printed intact: the verbatim Bolas Oracle text
//!        parses with exactly one `static_structure` gap (the grant line)
//!        and zero statics, while the three printed loyalty abilities carry
//!        loyalty costs + sorcery timing.
//!   #2 — grant-absent runtime: Bolas + donor on board, yet Bolas holds
//!        exactly its 3 printed Activated defs (Activated-kind-filtered,
//!        gap-excluding) and the donated index is absent; the printed Bolas
//!        +1 activates in the same harness (reach-guard, separate scenario
//!        — one activation spends the shared CR 606.3 cap).
//!   #3 — absence holds after departure (both controllers): removing the
//!        donor through the production zone-change pipeline leaves Bolas at
//!        exactly the 3 printed Activated defs; an opponent-controlled donor
//!        donates nothing either.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::GameScenario;
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    is_loyalty_ability_cost, AbilityCost, AbilityDefinition, AbilityKind, ActivationRestriction,
    Effect,
};
use engine::types::actions::GameAction;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

/// Verbatim Oracle text of Nicol Bolas, Dragon-God (`\u{2212}` is the U+2212
/// MINUS SIGN of the printed [-3]/[-8] costs).
const BOLAS_ORACLE: &str = "Nicol Bolas has all loyalty abilities of all other planeswalkers on the battlefield.\n[+1]: You draw a card. Each opponent exiles a card from their hand or a permanent they control.\n[\u{2212}3]: Destroy target creature or planeswalker.\n[\u{2212}8]: Each opponent who doesn't control a legendary creature or planeswalker loses the game.";

const DONOR_ORACLE: &str = "+1: You gain 1 life.";

fn loyalty_plus_one_index(abilities: &[AbilityDefinition]) -> Option<usize> {
    abilities
        .iter()
        .position(|a| a.cost == Some(AbilityCost::Loyalty { amount: 1 }))
}

// ─── Condition #1: grant line refused, printed abilities intact ───────────────

/// The full verbatim card REFUSES the grant line loudly: zero statics,
/// exactly one `static_structure` gap whose fragment is the grant line —
/// while the three printed loyalty abilities stay intact below (loyalty
/// costs plus sorcery timing). Re-admitting the loyalty emission arm
/// restores gap=0 and strands the refusal asserts (red).
#[test]
fn full_card_parses_with_no_unimplemented() {
    let parsed = parse_oracle_text(
        BOLAS_ORACLE,
        "Nicol Bolas, Dragon-God",
        &[],
        &["Legendary".to_string(), "Planeswalker".to_string()],
        &["Bolas".to_string()],
    );

    // The grant line refuses: zero statics, exactly one static_structure gap
    // whose fragment is the grant line.
    assert!(
        parsed.statics.is_empty(),
        "the grant line must refuse with zero statics; got {:?}",
        parsed.statics
    );
    let gaps: Vec<&AbilityDefinition> = parsed
        .abilities
        .iter()
        .filter(|ability| matches!(*ability.effect, Effect::Unimplemented { .. }))
        .collect();
    assert_eq!(
        gaps.len(),
        1,
        "exactly one gap ability expected; got {:?}",
        parsed.abilities
    );
    match &*gaps[0].effect {
        Effect::Unimplemented { name, description } => {
            assert_eq!(
                name, "static_structure",
                "the gap must be the static_structure honest gap; got {name:?}"
            );
            assert!(
                description
                    .as_deref()
                    .is_some_and(|fragment| fragment.contains("all loyalty abilities")),
                "the gap fragment must be the grant line; got {description:?}"
            );
        }
        other => panic!("expected the static_structure gap; got {other:?}"),
    }

    // The three printed loyalty abilities: loyalty costs + sorcery timing.
    let loyalty: Vec<&AbilityDefinition> = parsed
        .abilities
        .iter()
        .filter(|a| matches!(a.kind, AbilityKind::Activated))
        .collect();
    assert_eq!(
        loyalty.len(),
        3,
        "Bolas must hold three printed loyalty abilities; got {:?}",
        parsed.abilities
    );
    for def in loyalty {
        assert!(
            def.cost.as_ref().is_some_and(is_loyalty_ability_cost),
            "printed ability must carry a loyalty cost, got {:?}",
            def.cost
        );
        assert!(
            def.activation_restrictions
                .contains(&ActivationRestriction::AsSorcery),
            "printed loyalty ability must carry sorcery timing, got {:?}",
            def.activation_restrictions
        );
    }
}

// ─── Conditions #2 + #3: grant-absent runtime + departure/opponent ──────────

/// CR 606.3 + CR 606.4 + CR 611.3a: grant-ABSENT through the production
/// pipeline — Bolas + donor on board, yet Bolas holds exactly its 3 printed
/// Activated defs (Activated-kind-filtered, gap-excluding) and the donated
/// index is absent. The refused grant line lowers to a Spell-kind
/// `static_structure` gap def that rides base abilities unfiltered, so raw
/// `len()` would count it — hence the kind-filtered comparisons. The lone-
/// Bolas printed-+1 activation is the reach-guard (proving the harness
/// activates loyalty through the public API). Re-admitting the loyalty
/// emission arm re-donates and strands the absence asserts (red).
#[test]
fn bolas_activates_donated_loyalty_ability_through_public_api() {
    // Reach-guard (separate scenario — one activation spends the shared
    // CR 606.3 cap): lone Bolas, printed +1 activates (4→5).
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bolas = scenario
        .add_planeswalker_from_oracle(P0, "Nicol Bolas, Dragon-God", "Bolas", 4, BOLAS_ORACLE)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let lone_activated: Vec<&AbilityDefinition> = runner.state().objects[&bolas]
        .abilities
        .iter()
        .filter(|ability| matches!(ability.kind, AbilityKind::Activated))
        .collect();
    assert_eq!(
        lone_activated.len(),
        3,
        "lone Bolas must hold its three printed Activated defs; got {:?}",
        runner.state().objects[&bolas].abilities
    );
    let printed_plus = loyalty_plus_one_index(&runner.state().objects[&bolas].abilities)
        .expect("lone Bolas must hold its printed +1");
    runner
        .act(GameAction::ActivateAbility {
            source_id: bolas,
            ability_index: printed_plus,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("printed +1 must activate in the same harness");
    assert_eq!(
        runner.state().objects[&bolas].loyalty,
        Some(5),
        "printed +1 pays on Bolas (4→5)"
    );
    assert_eq!(runner.state().stack.len(), 1);

    // Main leg: Bolas + donor from Oracle text, layers evaluated. The grant
    // refuses, so Bolas holds exactly its 3 printed Activated defs and the
    // donor's +1 is absent.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bolas = scenario
        .add_planeswalker_from_oracle(P0, "Nicol Bolas, Dragon-God", "Bolas", 4, BOLAS_ORACLE)
        .id();
    let donor = scenario
        .add_planeswalker_from_oracle(P0, "Donor Walker", "Test", 3, DONOR_ORACLE)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    // The donor still holds its +1 (reach-guard: the donor parsed).
    let donor_def = runner.state().objects[&donor]
        .abilities
        .iter()
        .find(|a| a.cost == Some(AbilityCost::Loyalty { amount: 1 }))
        .cloned()
        .expect("donor must hold its +1");
    // Grant-absent: exactly the 3 printed Activated defs, donated index
    // absent, and the Spell-kind gap def present-but-excluded.
    let activated: Vec<&AbilityDefinition> = runner.state().objects[&bolas]
        .abilities
        .iter()
        .filter(|ability| matches!(ability.kind, AbilityKind::Activated))
        .collect();
    assert_eq!(
        activated.len(),
        3,
        "Bolas must hold exactly its 3 printed Activated defs with a donor present; got {:?}",
        runner.state().objects[&bolas].abilities
    );
    assert!(
        runner.state().objects[&bolas]
            .abilities
            .iter()
            .any(|ability| matches!(ability.kind, AbilityKind::Spell)
                && matches!(*ability.effect, Effect::Unimplemented { .. })),
        "the refused-grant gap def must be present (Spell-kind); got {:?}",
        runner.state().objects[&bolas].abilities
    );
    assert!(
        runner.state().objects[&bolas]
            .abilities
            .iter()
            .all(|a| a != &donor_def),
        "donated def must be absent from Bolas; got {:?}",
        runner.state().objects[&bolas].abilities
    );

    // Absence holds after departure too: the donor leaving through the
    // production zone-change pipeline leaves Bolas at exactly the 3 printed
    // Activated defs, with the Spell-kind gap def still present.
    let mut events = Vec::new();
    assert!(
        !move_object_for_test(
            runner.state_mut(),
            ZoneMoveRequest::effect(donor, Zone::Graveyard, donor),
            &mut events,
        ),
        "donor → graveyard must terminate, not park on a replacement choice"
    );
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let activated_after: Vec<&AbilityDefinition> = runner.state().objects[&bolas]
        .abilities
        .iter()
        .filter(|ability| matches!(ability.kind, AbilityKind::Activated))
        .collect();
    assert_eq!(
        activated_after.len(),
        3,
        "Bolas must hold exactly its 3 printed Activated defs after the donor leaves; got {:?}",
        runner.state().objects[&bolas].abilities
    );
    assert!(
        runner.state().objects[&bolas]
            .abilities
            .iter()
            .any(|ability| matches!(ability.kind, AbilityKind::Spell)
                && matches!(*ability.effect, Effect::Unimplemented { .. })),
        "the refused-grant gap def must still be present (Spell-kind); got {:?}",
        runner.state().objects[&bolas].abilities
    );
    assert!(
        runner.state().objects[&bolas]
            .abilities
            .iter()
            .all(|a| a != &donor_def),
        "donated def must still be absent after the donor leaves; got {:?}",
        runner.state().objects[&bolas].abilities
    );

    // Hostile: opponent-controlled donor — absence must hold for both
    // controllers (the refused source carries no controller axis).
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bolas = scenario
        .add_planeswalker_from_oracle(P0, "Nicol Bolas, Dragon-God", "Bolas", 4, BOLAS_ORACLE)
        .id();
    let opp_donor = scenario
        .add_planeswalker_from_oracle(P1, "Donor Walker", "Test", 3, DONOR_ORACLE)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let opp_donor_def = runner.state().objects[&opp_donor]
        .abilities
        .iter()
        .find(|a| a.cost == Some(AbilityCost::Loyalty { amount: 1 }))
        .cloned()
        .expect("opponent donor must hold its +1");
    let opp_activated: Vec<&AbilityDefinition> = runner.state().objects[&bolas]
        .abilities
        .iter()
        .filter(|ability| matches!(ability.kind, AbilityKind::Activated))
        .collect();
    assert_eq!(
        opp_activated.len(),
        3,
        "Bolas must hold exactly its 3 printed Activated defs with an opponent donor present; got {:?}",
        runner.state().objects[&bolas].abilities
    );
    assert!(
        runner.state().objects[&bolas]
            .abilities
            .iter()
            .all(|a| a != &opp_donor_def),
        "opponent-donor def must be absent from Bolas; got {:?}",
        runner.state().objects[&bolas].abilities
    );
}
