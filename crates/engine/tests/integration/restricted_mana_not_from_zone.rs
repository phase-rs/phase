//! Mm'menon, the Right Hand — positive spell-only `NotFrom(Hand)` restriction;
//! and Karolina Dean, Runaway — a narrow prohibition on casts from hand that
//! leaves every non-cast payment context unrestricted.
//!
//! CR 106.6 (restricted mana spend) + CR 400.7 (cast-from zone identity).
//!
//! These tests drive the runtime spend-eligibility decision two ways:
//!   1. `ManaRestriction::allows_spell` — the single authority every payment site
//!      flows through (`PaymentContext::Spell` → `allows_spell`).
//!   2. `ManaPool::spend_for` with `PaymentContext::Spell` — the real mana-payment
//!      route, proving a `NotFrom`-restricted unit is CONSUMED for a spell cast
//!      from a non-hand zone and WITHHELD for a spell cast from hand.
//!
//! Revert-proof: reverting either the polarity axis or Karolina's dedicated
//! prohibition makes the corresponding hand/non-hand and non-cast assertions
//! flip.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::zones::create_object;
use engine::types::actions::GameAction;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{
    ActivationManaColorConstraint, ManaCost, ManaPool, ManaRestriction, ManaType, ManaUnit,
    PaymentContext, SpecialAction, SpellMeta, ZoneSpend, ZoneSpendPolarity,
};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Mm'menon, the Right Hand: spend only to cast a spell from anywhere other than
/// your hand.
fn not_from_hand_restriction() -> ManaRestriction {
    ManaRestriction::OnlyForSpellFromZone(ZoneSpend {
        zone: Zone::Hand,
        polarity: ZoneSpendPolarity::NotFrom,
    })
}

/// Karolina Dean: this mana cannot pay for the one forbidden cast class, but
/// remains unrestricted for non-cast payments.
fn cannot_cast_from_hand_restriction() -> ManaRestriction {
    ManaRestriction::CannotCastSpellFromZone(Zone::Hand)
}

fn spell_cast_from(zone: Zone) -> SpellMeta {
    SpellMeta {
        types: vec!["Artifact".to_string()],
        cast_from_zone: Some(zone),
        ..SpellMeta::default()
    }
}

#[test]
fn allows_spell_cast_from_non_hand_zone() {
    let r = not_from_hand_restriction();
    // Any cast-from zone except hand qualifies.
    assert!(r.allows_spell(&spell_cast_from(Zone::Graveyard)));
    assert!(r.allows_spell(&spell_cast_from(Zone::Exile)));
    assert!(r.allows_spell(&spell_cast_from(Zone::Library)));
}

#[test]
fn rejects_spell_cast_from_hand() {
    // A normal cast from hand is exactly what this restriction forbids.
    assert!(!not_from_hand_restriction().allows_spell(&spell_cast_from(Zone::Hand)));
}

#[test]
fn rejects_spell_with_unknown_origin() {
    // CR 400.7: a payment site with no associated cast-from zone is ineligible
    // (conservative — never auto-authorize when origin is unknown).
    assert!(!not_from_hand_restriction().allows_spell(&SpellMeta::default()));
}

#[test]
fn never_allows_ability_activation() {
    // CR 106.6: zone-gated spend is spell-casting only.
    assert!(!not_from_hand_restriction().allows_activation(&["Artifact".to_string()], &[], None));
}

/// Drive the REAL mana-payment route: `ManaPool::spend_for` with
/// `PaymentContext::Spell`. A `NotFrom`-restricted unit must be consumed for a
/// non-hand cast and withheld for a hand cast.
#[test]
fn spend_for_consumes_for_non_hand_and_withholds_for_hand() {
    let source = ObjectId(1);
    let make_pool = || {
        let mut pool = ManaPool::default();
        pool.add(ManaUnit::new(
            ManaType::Blue,
            source,
            false,
            vec![not_from_hand_restriction()],
        ));
        pool
    };

    // Eligible: cast from graveyard (non-hand) — the unit is consumed.
    let from_gy = spell_cast_from(Zone::Graveyard);
    let mut pool = make_pool();
    let spent = pool.spend_for(ManaType::Blue, &PaymentContext::Spell(&from_gy));
    assert!(
        spent.is_some(),
        "NotFrom-restricted mana must pay a spell cast from a non-hand zone"
    );
    assert_eq!(pool.total(), 0, "the unit must be consumed");

    // Ineligible: cast from hand — the unit is withheld, pool intact.
    let from_hand = spell_cast_from(Zone::Hand);
    let mut pool = make_pool();
    let spent = pool.spend_for(ManaType::Blue, &PaymentContext::Spell(&from_hand));
    assert!(
        spent.is_none(),
        "NotFrom-restricted mana must not pay a spell cast from hand"
    );
    assert_eq!(pool.total(), 1, "the unit must remain unspent");
}

#[test]
fn karolina_restriction_is_a_narrow_cast_prohibition() {
    let source = ObjectId(1);
    let make_pool = || {
        let mut pool = ManaPool::default();
        pool.add(ManaUnit::new(
            ManaType::White,
            source,
            false,
            vec![cannot_cast_from_hand_restriction()],
        ));
        pool
    };

    let mut hand_pool = make_pool();
    assert!(
        hand_pool
            .spend_for(
                ManaType::White,
                &PaymentContext::Spell(&spell_cast_from(Zone::Hand)),
            )
            .is_none(),
        "Karolina's mana must be withheld from a spell cast from hand"
    );
    assert_eq!(hand_pool.total(), 1);

    for origin in [Zone::Graveyard, Zone::Exile] {
        let mut pool = make_pool();
        assert!(
            pool.spend_for(
                ManaType::White,
                &PaymentContext::Spell(&spell_cast_from(origin)),
            )
            .is_some(),
            "Karolina's mana must pay for a spell cast from {origin:?}"
        );
        assert_eq!(pool.total(), 0, "eligible mana must be consumed");
    }

    let mut unknown_pool = make_pool();
    assert!(
        unknown_pool
            .spend_for(
                ManaType::White,
                &PaymentContext::Spell(&SpellMeta::default()),
            )
            .is_none(),
        "unknown spell origins must fail closed"
    );

    let source_types = ["Creature".to_string()];
    let source_subtypes = ["Human".to_string()];
    let activation = PaymentContext::Activation {
        source_types: &source_types,
        source_subtypes: &source_subtypes,
        ability_tag: None,
        mana_color_constraint: ActivationManaColorConstraint::Unrestricted,
    };
    let mut activation_pool = make_pool();
    assert!(activation_pool
        .spend_for(ManaType::White, &activation)
        .is_some());

    let mut effect_pool = make_pool();
    assert!(effect_pool
        .spend_for(ManaType::White, &PaymentContext::Effect)
        .is_some());

    for action in [
        SpecialAction::CompanionToHand,
        SpecialAction::UnlockDoor,
        SpecialAction::Plot,
        SpecialAction::TurnFaceUp,
        SpecialAction::RollPlanarDie,
        SpecialAction::EndContinuousEffect,
    ] {
        let mut pool = make_pool();
        assert!(
            pool.spend_for(ManaType::White, &PaymentContext::SpecialAction(action))
                .is_some(),
            "Karolina's prohibition must not reject {action:?}"
        );
    }
}

#[test]
fn karolina_restriction_drives_the_production_cast_payment_pipeline() {
    const KAROLINA_ORACLE: &str = "Flying\nAt the beginning of your first main phase, add {W}{U}{B}{R}{G}. This mana can't be spent to cast spells from your hand.";

    let build_game = |zone, commander| -> (GameRunner, ObjectId) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::Upkeep);
        scenario.with_library_top(P0, &["P0 Card"; 40]);
        scenario.with_library_top(engine::types::player::PlayerId(1), &["P1 Card"; 40]);
        scenario
            .add_creature(P0, "Karolina Dean, Runaway", 4, 4)
            .from_oracle_text(KAROLINA_ORACLE);
        let mut game = scenario.build();
        let state = game.state_mut();
        state.format_config.command_zone = commander;

        let spell = create_object(
            state,
            CardId(if commander { 9_102 } else { 9_101 }),
            P0,
            "Restricted Mana Cast Probe".to_string(),
            zone,
        );
        let object = state.objects.get_mut(&spell).unwrap();
        object.card_types.core_types.push(CoreType::Creature);
        object.mana_cost = ManaCost::generic(1);
        if commander {
            object.card_types.supertypes.push(Supertype::Legendary);
            object.is_commander = true;
        }

        game.advance_to_phase(Phase::PreCombatMain);
        for _ in 0..4 {
            if game.state().players[P0.0 as usize].mana_pool.total() == 5 {
                break;
            }
            game.act(GameAction::PassPriority)
                .expect("Karolina's first-main-phase trigger must resolve through priority");
        }

        let pool = &game.state().players[P0.0 as usize].mana_pool;
        assert_eq!(pool.total(), 5, "Karolina's trigger must produce WUBRG");
        for color in [
            ManaType::White,
            ManaType::Blue,
            ManaType::Black,
            ManaType::Red,
            ManaType::Green,
        ] {
            assert_eq!(pool.count_color(color), 1);
        }
        assert!(pool.mana.iter().all(|unit| {
            unit.restrictions == vec![ManaRestriction::CannotCastSpellFromZone(Zone::Hand)]
        }));
        (game, spell)
    };

    let (mut hand_game, hand_spell) = build_game(Zone::Hand, false);
    let error = match hand_game.cast(hand_spell).try_resolve() {
        Ok(_) => panic!("Karolina's mana must not fund a spell cast from hand"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("Cannot pay mana cost"));
    assert_eq!(hand_game.state().objects[&hand_spell].zone, Zone::Hand);
    assert_eq!(hand_game.state().players[0].mana_pool.total(), 5);

    // The command zone is a naturally castable non-hand origin, so this half
    // reaches the same production payment path without granting a test-only
    // graveyard or exile permission.
    let (mut command_game, command_spell) = build_game(Zone::Command, true);
    let outcome = command_game.cast(command_spell).resolve();
    outcome.assert_zone(&[command_spell], Zone::Battlefield);
    assert_eq!(outcome.mana_pool_total(P0), 4);
}

/// Guard against the inclusion polarity regressing: the positive `From` reading
/// must still gate on the named zone (graveyard payable, hand not), proving the
/// polarity axis discriminates both directions from one variant.
#[test]
fn from_polarity_still_gates_inclusively() {
    let from_gy_only = ManaRestriction::OnlyForSpellFromZone(ZoneSpend {
        zone: Zone::Graveyard,
        polarity: ZoneSpendPolarity::From,
    });
    assert!(from_gy_only.allows_spell(&spell_cast_from(Zone::Graveyard)));
    assert!(!from_gy_only.allows_spell(&spell_cast_from(Zone::Hand)));
}
