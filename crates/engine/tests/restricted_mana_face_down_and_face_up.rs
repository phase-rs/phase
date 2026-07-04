//! Spend-restriction cluster: face-down casts and turn-face-up / door-unlock
//! special actions on produced mana.
//!
//! Cards in the cluster:
//!   - Creeping Peeper — "{T}: Add {U}. Spend this mana only to cast an
//!     enchantment spell, unlock a door, or turn a permanent face up."
//!   - Overgrown Zealot — "{T}: Add two mana of any one color. Spend this mana
//!     only to turn permanents face up."
//!   - Tin Street Gossip — "{T}: Add {R}{G}. Spend this mana only to cast
//!     face-down spells or to turn creatures face up."
//!
//! CR 106.6 (restricted mana spend) + CR 708.4 (face-down spell) + CR 116.2b /
//! CR 702.37e (turn-face-up special action) + CR 116.2m / CR 709.5e (door
//! unlock).
//!
//! These tests drive the mana-payment route — `ManaPool::spend_for` with a
//! `PaymentContext` — proving the produced unit is CONSUMED for a legal spend
//! and WITHHELD for an illegal one.
//!
//! Two of the three restriction halves are LIVE on a production payment path and
//! two are HONEST-DEFERRED — be precise about which is which:
//!
//! - LIVE: the spell-type half (`OnlyForSpellType`, Creeping Peeper's
//!   enchantment branch) and the door-unlock half
//!   (`OnlyForSpecialAction(UnlockDoor)`). Both reach `spend_for` through real
//!   production sites — `can_pay_for_spell` / `pay_cost_*` for casts and
//!   `pay_special_action_mana_cost` for door unlock — so the `spend_for`
//!   assertion exercises the same `ManaRestriction::allows` decision a full
//!   `apply()` cast / unlock makes.
//!
//! - HONEST-DEFERRED: the face-down-cast half (`OnlyForFaceDownSpell`, Tin
//!   Street Gossip) and the turn-face-up half
//!   (`OnlyForSpecialAction(TurnFaceUp)`, Overgrown Zealot). NEITHER is reachable
//!   on a production payment path: CR 708.4 face-down play
//!   (`GameAction::PlayFaceDown` → `game::morph::play_face_down`) and CR 116.2b
//!   turn-face-up (`game::morph::turn_face_up`) both move/flip the permanent via
//!   the zone pipeline and charge NO mana, so no site ever CASTS A SPELL FACE
//!   DOWN nor emits `PaymentContext::SpecialAction(TurnFaceUp)`. The
//!   `OnlyForFaceDownSpell` gate is therefore fail-closed (under-permitting, not
//!   over-permitting): `SpellMeta.is_face_down` is sourced from the cast's
//!   face-down intent (`build_spell_meta`, hardcoded `false` today), not from
//!   `obj.face_down`, so the gate ALSO correctly REJECTS exile-concealment casts
//!   (foretell/hideaway) whose `obj.face_down = true` but which are cast face up
//!   (CR 702.143c). The tests below assert that contract directly: the gate
//!   REJECTS every production payment context, and the genuine face-down-cast
//!   positive is checked only at the restriction level (not via a production
//!   payment, which never sets `is_face_down = true`), matching the honest-
//!   deferred treatment.
//!
//! Revert-proof: each assertion flips if the corresponding gate is reverted —
//! see the per-test notes.

use engine::types::identifiers::ObjectId;
use engine::types::mana::{
    ManaPool, ManaRestriction, ManaType, ManaUnit, PaymentContext, SpecialAction, SpellMeta,
};

fn spell(types: &[&str], is_face_down: bool) -> SpellMeta {
    SpellMeta {
        types: types.iter().map(|s| s.to_string()).collect(),
        is_face_down,
        ..SpellMeta::default()
    }
}

/// Tin Street Gossip: "spend this mana only to cast face-down spells" — the
/// `OnlyForFaceDownSpell` half. This gate is fail-closed on every production
/// payment path: no site CASTS A SPELL FACE DOWN (CR 708.4 morph cast cost,
/// CR 702.37c, is unimplemented), and `SpellMeta.is_face_down` is sourced from
/// the cast's face-down intent (`build_spell_meta`, hardcoded `false` today),
/// never from `obj.face_down` — so a normal face-up cast AND an exile-concealment
/// cast (foretell/hideaway, whose `obj.face_down = true` but which is cast face
/// up, CR 702.143c) both report `is_face_down = false` and are correctly
/// rejected. This test asserts the gate REJECTS every production payment context
/// and confirms the genuine face-down-cast positive only at the restriction
/// level (it is unreachable on any production payment path today).
///
/// Revert-proof: if `allows_spell` for `OnlyForFaceDownSpell` were changed to
/// ignore `meta.is_face_down` (e.g. return `true`), the face-up `Spell`
/// rejection (A1) would flip — the unit would be wrongly consumed.
#[test]
fn face_down_spell_mana_rejects_every_production_context() {
    let source = ObjectId(1);
    let make_pool = || {
        let mut pool = ManaPool::default();
        pool.add(ManaUnit::new(
            ManaType::Red,
            source,
            false,
            vec![ManaRestriction::OnlyForFaceDownSpell],
        ));
        pool
    };

    // ILLEGAL (A1): a normal face-up creature cast — the production `Spell`
    // context never carries `is_face_down = true`, so the unit is withheld.
    let face_up = spell(&["Creature"], false);
    let mut pool = make_pool();
    assert!(
        pool.spend_for(ManaType::Red, &PaymentContext::Spell(&face_up))
            .is_none(),
        "face-down-only mana must not pay a normal face-up cast"
    );
    assert_eq!(pool.total(), 1, "the unit must remain unspent");

    // ILLEGAL: an unrelated door-unlock special action — the unit is withheld.
    let mut pool = make_pool();
    assert!(
        pool.spend_for(
            ManaType::Red,
            &PaymentContext::SpecialAction(SpecialAction::UnlockDoor)
        )
        .is_none(),
        "face-down-only mana must not pay a door-unlock special action"
    );
    assert_eq!(pool.total(), 1);

    // ILLEGAL: an ability activation — the unit is withheld.
    let mut pool = make_pool();
    assert!(
        pool.spend_for(
            ManaType::Red,
            &PaymentContext::Activation {
                source_types: &["Creature".to_string()],
                source_subtypes: &[],
                ability_tag: None,
            }
        )
        .is_none(),
        "face-down-only mana must not pay an ability activation"
    );
    assert_eq!(pool.total(), 1);

    // The genuine face-down CAST (CR 708.4 / CR 702.37c) would be the only legal
    // context; confirm the gate accepts it at the restriction level. This is the
    // future face-down-cast path and is unreachable on any production payment
    // path today (no site sets `is_face_down = true`).
    assert!(ManaRestriction::OnlyForFaceDownSpell
        .allows(&PaymentContext::Spell(&spell(&["Creature"], true))));
}

/// Creeping Peeper: "spend this mana only to cast an enchantment spell, unlock a
/// door, or turn a permanent face up" — the runtime
/// `Any([SpellType("Enchantment"), OnlyForSpecialAction(UnlockDoor),
/// OnlyForSpecialAction(TurnFaceUp)])`. Drives `spend_for`: an enchantment cast
/// consumes the {U}; a non-enchantment cast withholds it.
///
/// Revert-proof: if the `SpellType("Enchantment")` branch were dropped from the
/// disjunction, the enchantment cast would no longer be payable and its
/// assertion would flip.
#[test]
fn creeping_peeper_mana_consumes_for_enchantment_not_creature() {
    let source = ObjectId(2);
    let restriction = ManaRestriction::OnlyForAny(vec![
        ManaRestriction::OnlyForSpellType("Enchantment".to_string()),
        ManaRestriction::OnlyForSpecialAction(SpecialAction::UnlockDoor),
        ManaRestriction::OnlyForSpecialAction(SpecialAction::TurnFaceUp),
    ]);
    let make_pool = || {
        let mut pool = ManaPool::default();
        pool.add(ManaUnit::new(
            ManaType::Blue,
            source,
            false,
            vec![restriction.clone()],
        ));
        pool
    };

    // LEGAL: an enchantment cast — the {U} is consumed.
    let enchantment = spell(&["Enchantment"], false);
    let mut pool = make_pool();
    let spent = pool.spend_for(ManaType::Blue, &PaymentContext::Spell(&enchantment));
    assert!(
        spent.is_some(),
        "Creeping Peeper's {{U}} must pay an enchantment spell"
    );
    assert_eq!(pool.total(), 0, "the {{U}} must be consumed");

    // ILLEGAL: a (non-enchantment) creature cast — the {U} is withheld.
    let creature = spell(&["Creature"], false);
    let mut pool = make_pool();
    let spent = pool.spend_for(ManaType::Blue, &PaymentContext::Spell(&creature));
    assert!(
        spent.is_none(),
        "Creeping Peeper's {{U}} must not pay a non-enchantment spell"
    );
    assert_eq!(pool.total(), 1, "the {{U}} must remain unspent");
}

/// Creeping Peeper's {U} pays the door-unlock special action (CR 116.2m), the
/// branch a Room's unlock cost routes through
/// (`PaymentContext::SpecialAction(UnlockDoor)`).
///
/// Revert-proof: if the `OnlyForSpecialAction(UnlockDoor)` branch were dropped,
/// this assertion would flip — the unit would no longer pay an unlock.
#[test]
fn creeping_peeper_mana_pays_door_unlock_special_action() {
    let source = ObjectId(3);
    let mut pool = ManaPool::default();
    pool.add(ManaUnit::new(
        ManaType::Blue,
        source,
        false,
        vec![ManaRestriction::OnlyForAny(vec![
            ManaRestriction::OnlyForSpellType("Enchantment".to_string()),
            ManaRestriction::OnlyForSpecialAction(SpecialAction::UnlockDoor),
            ManaRestriction::OnlyForSpecialAction(SpecialAction::TurnFaceUp),
        ])],
    ));
    let spent = pool.spend_for(
        ManaType::Blue,
        &PaymentContext::SpecialAction(SpecialAction::UnlockDoor),
    );
    assert!(
        spent.is_some(),
        "Creeping Peeper's {{U}} must pay a door-unlock special action"
    );
    assert_eq!(pool.total(), 0, "the {{U}} must be consumed");
}

/// Overgrown Zealot: "spend this mana only to turn permanents face up" — the
/// `OnlyForSpecialAction(TurnFaceUp)` gate. This special action charges no mana
/// in this engine yet (`game::morph::turn_face_up` flips the permanent for
/// free), so no payment site emits `PaymentContext::SpecialAction(TurnFaceUp)`.
/// The runtime is therefore conservative: the mana is never spendable on any
/// payment context that actually occurs (spell / activation / effect /
/// door-unlock), and is never silently over-permitted.
///
/// This documents the honest-deferred contract: the turn-face-up gate is
/// representable and correctly REJECTS every wrong context, but its positive
/// case awaits routing the morph cost through the special-action payment path.
/// If a future change starts emitting `PaymentContext::SpecialAction(TurnFaceUp)`
/// at a real spend site, the positive assertion below flips from withheld to
/// consumed and this test must gain a positive-payment arm.
#[test]
fn overgrown_zealot_turn_face_up_mana_rejects_every_live_context() {
    let source = ObjectId(4);
    let make_pool = || {
        let mut pool = ManaPool::default();
        // Overgrown Zealot adds two mana of any one color.
        pool.add(ManaUnit::new(
            ManaType::Green,
            source,
            false,
            vec![ManaRestriction::OnlyForSpecialAction(
                SpecialAction::TurnFaceUp,
            )],
        ));
        pool
    };

    // ILLEGAL: a spell cast (even a face-down one) — the unit is withheld.
    let face_down = spell(&["Creature"], true);
    let mut pool = make_pool();
    assert!(
        pool.spend_for(ManaType::Green, &PaymentContext::Spell(&face_down))
            .is_none(),
        "turn-face-up mana must not pay a spell cast"
    );
    assert_eq!(pool.total(), 1);

    // ILLEGAL: an unrelated door-unlock special action — the unit is withheld.
    let mut pool = make_pool();
    assert!(
        pool.spend_for(
            ManaType::Green,
            &PaymentContext::SpecialAction(SpecialAction::UnlockDoor)
        )
        .is_none(),
        "turn-face-up mana must not pay a door unlock"
    );
    assert_eq!(pool.total(), 1);

    // The matching special action would be the only legal context (CR 116.2b);
    // confirm the gate accepts it at the restriction level so the eventual
    // payment wiring is a no-op for this enum.
    assert!(
        ManaRestriction::OnlyForSpecialAction(SpecialAction::TurnFaceUp)
            .allows(&PaymentContext::SpecialAction(SpecialAction::TurnFaceUp))
    );
}
