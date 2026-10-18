//! Nicol Bolas, Dragon-God — "Nicol Bolas has all loyalty abilities of all
//! other planeswalkers on the battlefield." (CR 613.1f Layer-6 ability grant;
//! CR 606.2 loyalty category; CR 606.3 timing + frequency; CR 606.4 payment;
//! CR 611.3a live tracking.)
//!
//! Exercises the grant end-to-end against the REAL parse → layer → activation
//! pipeline:
//!   * `ContinuousModification::GrantAllActivatedAbilitiesOf { And[Typed
//!     (planeswalker + InZone{Battlefield}), Not{SelfRef}], kind:
//!     Some(Loyalty) }` → `expand_granted_activated_abilities` → `GrantAbility`
//!     on Bolas.
//!   * The donated def activates through the public
//!     `GameAction::ActivateAbility` route with Bolas paying loyalty and the
//!     effect resolving.
//!
//! Lead conditions:
//!   #1 — full-card gap=0: the verbatim Bolas Oracle text parses with NO
//!        `Unimplemented` clause, the grant static carries the typed source,
//!        and the three printed loyalty abilities carry loyalty costs +
//!        sorcery timing.
//!   #2 — donated activation: Bolas activates the donor's +1 through the
//!        public API (loyalty 4→5 on Bolas, donor unchanged, life +1 on
//!        resolution); the printed Bolas +1 activates in the same harness
//!        (reach-guard, separate scenario — one activation spends the shared
//!        CR 606.3 cap).
//!   #3 — live tracking: removing the donor through the production zone-change
//!        pipeline drops the donation on the next layer pass (printed defs
//!        remain).

use engine::game::layers::evaluate_layers;
use engine::game::scenario::GameScenario;
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    is_loyalty_ability_cost, AbilityCost, AbilityDefinition, AbilityKind, ActivationRestriction,
    ContinuousModification, FilterProp, TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::TypeFilter;

const P0: PlayerId = PlayerId(0);

/// Verbatim Oracle text of Nicol Bolas, Dragon-God (`\u{2212}` is the U+2212
/// MINUS SIGN of the printed [-3]/[-8] costs).
const BOLAS_ORACLE: &str = "Nicol Bolas has all loyalty abilities of all other planeswalkers on the battlefield.\n[+1]: You draw a card. Each opponent exiles a card from their hand or a permanent they control.\n[\u{2212}3]: Destroy target creature or planeswalker.\n[\u{2212}8]: Each opponent who doesn't control a legendary creature or planeswalker loses the game.";

const DONOR_ORACLE: &str = "+1: You gain 1 life.";

fn loyalty_plus_one_index(abilities: &[AbilityDefinition]) -> Option<usize> {
    abilities
        .iter()
        .position(|a| a.cost == Some(AbilityCost::Loyalty { amount: 1 }))
}

/// CR 613.1f: the provider set "all other planeswalkers on the battlefield"
/// exactly as the parser lowers it (verified by the parse-shape test below):
/// battlefield-pinned planeswalkers AND NOT the host itself (the English
/// "other").
fn bolas_grant_source() -> TargetFilter {
    TargetFilter::And {
        filters: vec![
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Planeswalker).properties(vec![
                FilterProp::InZone {
                    zone: Zone::Battlefield,
                },
            ])),
            TargetFilter::Not {
                filter: Box::new(TargetFilter::SelfRef),
            },
        ],
    }
}

// ─── Condition #1: full-card gap=0 ───────────────────────────────────────────

/// The full verbatim card parses with NO `Unimplemented` clause: the grant
/// line lowers to the typed loyalty-grant static (affected SelfRef, the
/// `bolas_grant_source` provider set) and the three printed loyalty abilities
/// carry loyalty costs + sorcery timing. Reverting any grammar arm strands the
/// static-count assertion (red: the `static_structure` honest gap returns).
#[test]
fn full_card_parses_with_no_unimplemented() {
    let parsed = parse_oracle_text(
        BOLAS_ORACLE,
        "Nicol Bolas, Dragon-God",
        &[],
        &["Legendary".to_string(), "Planeswalker".to_string()],
        &["Bolas".to_string()],
    );

    assert!(
        !format!("{parsed:?}").contains("Unimplemented"),
        "no clause may remain Unimplemented (gap=0): {parsed:?}"
    );

    // The grant static, exactly as the runtime helper mirrors it.
    assert_eq!(
        parsed.statics.len(),
        1,
        "the grant line must lower to exactly one static; got {:?}",
        parsed.statics
    );
    assert_eq!(
        parsed.statics[0].affected,
        Some(TargetFilter::SelfRef),
        "the grant static must affect its host; got {:?}",
        parsed.statics[0].affected
    );
    assert_eq!(
        parsed.statics[0].modifications,
        vec![ContinuousModification::GrantAllActivatedAbilitiesOf {
            source: bolas_grant_source(),
            cap: None,
            kind: Some(engine::types::events::ActivatedAbilityKind::Loyalty),
        }],
        "the grant static must carry the typed loyalty grant; got {:?}",
        parsed.statics[0].modifications
    );

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

// ─── Conditions #2 + #3: donated activation + live tracking ─────────────────

/// CR 606.3 + CR 606.4 + CR 611.3a: Bolas activates the donor's +1 through the
/// public game API — loyalty 4→5 on Bolas, the donor unchanged, the gain-life
/// effect resolving (+1 life). Removing the donor through the production
/// zone-change pipeline then drops the donation on the next layer pass while
/// the printed defs remain. Reverting the emission arm strands the donation
/// setup assertion (red: donated ability absent).
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
    assert_eq!(
        runner.state().objects[&bolas].abilities.len(),
        3,
        "lone Bolas must hold its three printed defs; got {:?}",
        runner.state().objects[&bolas].abilities
    );
    let printed_plus = loyalty_plus_one_index(&runner.state().objects[&bolas].abilities)
        .expect("lone Bolas must hold its printed +1");
    runner
        .act(GameAction::ActivateAbility {
            source_id: bolas,
            ability_index: printed_plus,
        })
        .expect("printed +1 must activate in the same harness");
    assert_eq!(
        runner.state().objects[&bolas].loyalty,
        Some(5),
        "printed +1 pays on Bolas (4→5)"
    );
    assert_eq!(runner.state().stack.len(), 1);

    // Main leg: Bolas + donor from Oracle text, layers evaluated.
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

    // The donor's +1, wholesale donated onto Bolas.
    let donor_def = runner.state().objects[&donor]
        .abilities
        .iter()
        .find(|a| a.cost == Some(AbilityCost::Loyalty { amount: 1 }))
        .cloned()
        .expect("donor must hold its +1");
    let donated_index = runner.state().objects[&bolas]
        .abilities
        .iter()
        .position(|a| a == &donor_def)
        .expect("Bolas must hold the donated +1");
    let life_before = runner.life(P0);
    runner
        .act(GameAction::ActivateAbility {
            source_id: bolas,
            ability_index: donated_index,
        })
        .expect("donated +1 must activate through the public API");
    assert_eq!(
        runner.state().objects[&bolas].loyalty,
        Some(5),
        "Bolas pays the donated +1 (4→5)"
    );
    assert_eq!(
        runner.state().objects[&donor].loyalty,
        Some(3),
        "donor loyalty unchanged by Bolas's activation"
    );
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.life(P0),
        life_before + 1,
        "the donated gain-life effect must resolve"
    );

    // Live tracking: the donor leaving drops the donation.
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
    assert!(
        runner.state().objects[&bolas]
            .abilities
            .iter()
            .all(|a| a != &donor_def),
        "donated def must drop once the donor leaves; got {:?}",
        runner.state().objects[&bolas].abilities
    );
    assert_eq!(
        runner.state().objects[&bolas].abilities.len(),
        3,
        "Bolas's printed defs must remain; got {:?}",
        runner.state().objects[&bolas].abilities
    );
}
