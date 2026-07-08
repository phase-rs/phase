//! Mm'menon, the Right Hand — Artifacts you control have "{T}: Add {U}. Spend
//! this mana only to cast a spell from anywhere other than your hand."
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
//! Revert-proof: if the `ZoneSpendPolarity::NotFrom` arm of
//! `OnlyForSpellFromZone` were reverted to the inclusion (`From`) reading, the
//! "from hand" spell would become payable and the "from graveyard" spell would
//! become unpayable — every assertion below flips.

use engine::types::identifiers::ObjectId;
use engine::types::mana::{
    ManaPool, ManaRestriction, ManaType, ManaUnit, PaymentContext, SpellMeta, ZoneSpend,
    ZoneSpendPolarity,
};
use engine::types::zones::Zone;

/// Mm'menon, the Right Hand: spend only to cast a spell from anywhere other than
/// your hand.
fn not_from_hand_restriction() -> ManaRestriction {
    ManaRestriction::OnlyForSpellFromZone(ZoneSpend {
        zone: Zone::Hand,
        polarity: ZoneSpendPolarity::NotFrom,
    })
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
