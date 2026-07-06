//! CR 202.3d + CR 709.4/709.4b: A split card's mana value is the COMBINED value
//! of both halves in every zone EXCEPT the stack, where it is the chosen half.
//!
//! This is the NEGATIVE / on-stack half of the split-card mana-value fix: casting
//! one half of Assault // Battery ({R} Assault) puts a spell on the stack whose
//! `effective_mana_value()` must be the CHOSEN half (1), NOT the combined value
//! (5). The off-stack combined-value cases live in the inline `game_object`
//! tests; this drives the real cast pipeline so the `zone != Zone::Stack` gate is
//! exercised against a genuinely on-stack object (which still carries a
//! `back_face` with `layout_kind == Split` after the front-face path).
//!
//! Reverting the `zone != Zone::Stack` gate makes the on-stack spell report the
//! combined MV 5 and this test fails.

use engine::game::casting::spell_objects_available_to_cast;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{
    CastPermissionConstraint, CastingPermission, Comparator, QuantityExpr,
};
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

#[test]
fn split_spell_on_stack_reports_chosen_half_mana_value() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Assault // Battery: Assault {R} (MV 1) is the front half. Off the stack the
    // combined MV is 5; on the stack the chosen half (Assault) MV is 1.
    let assault = scenario.add_real_card(P0, "Assault", Zone::Hand, db);

    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, assault, false, vec![])],
    );

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Sanity: off the stack (in hand) the combined MV is 5.
    assert_eq!(
        runner
            .state()
            .objects
            .get(&assault)
            .unwrap()
            .effective_mana_value(),
        5,
        "in hand, Assault // Battery reports the combined MV 5"
    );

    // Cast Assault (the {R} front half) targeting the opponent (any target).
    // Assault // Battery is a spell//spell split, so casting requires an explicit
    // face choice; `modal_back_face(false)` selects the front (Assault) half.
    let commit = runner
        .cast(assault)
        .modal_back_face(false)
        .target_player(P1)
        .commit();

    let state = commit.state();
    let stack_entry = state
        .stack
        .last()
        .expect("Assault should be on the stack after casting");
    let spell = state
        .objects
        .get(&stack_entry.source_id)
        .expect("stack spell object exists");

    assert_eq!(spell.zone, Zone::Stack, "the cast half is on the stack");
    assert_eq!(spell.name, "Assault", "the front half was cast");
    assert_eq!(
        spell.effective_mana_value(),
        1,
        "on the stack, a split spell's mana value is the CHOSEN half (1), not the \
         combined value (5) — proves the zone != Stack gate"
    );
}

/// A `ManaValue { LE, Fixed(ceiling) }` exile-cast permission granted to `P0`,
/// modeling an impulse-draw "you may cast a card with mana value N or less from
/// exile" effect. Off-stack legality routes through the `resulting_mv.is_none()`
/// arm of `cast_permission_constraint_allows_cast`.
fn exile_cast_permission_mv_le(ceiling: i32) -> CastingPermission {
    CastingPermission::ExileWithAltCost {
        cost: ManaCost::zero(),
        cast_transformed: false,
        constraint: Some(CastPermissionConstraint::ManaValue {
            comparator: Comparator::LE,
            value: QuantityExpr::Fixed { value: ceiling },
        }),
        granted_to: Some(P0),
        resolution_cleanup: None,
        duration: None,
        graveyard_replacement: None,
        mana_spend_permission: None,
        enters_with_counter: None,
        // Added on main after this test was written; no modifications on entry.
        enters_with_modifications: Vec::new(),
    }
}

/// CR 202.3d + CR 709.4b: An impulse-exiled split card's cast-permission legality
/// must be evaluated against its COMBINED mana value (both halves), because the
/// object is off the stack. Assault // Battery combines to MV 5.
///
/// This drives the real off-stack legality surface
/// (`spell_objects_available_to_cast` → `exile_object_castable_by_permission` →
/// `has_exile_cast_permission` → `exile_alt_cost_permission_supports_cast` →
/// `cast_permission_constraint_allows_cast`, `resulting_mv.is_none()` arm).
///
/// Revert-failing discriminator: a permission ceiling of MV <= 4 must DENY the
/// spell (combined 5 > 4). If the legality gate read the front-only MV
/// (`obj.mana_cost.mana_value()` = 1 for Assault), it would WRONGLY admit the
/// card at MV <= 4. Reverting casting.rs to the front-only read flips
/// `denies_at_four` and fails this test. MV <= 5 must ALLOW it (5 <= 5).
#[test]
fn exile_cast_permission_uses_combined_split_mana_value() {
    let Some(db) = load_db() else {
        return;
    };

    // Ceiling 4: combined MV 5 must be DENIED (front-only MV 1 would wrongly pass).
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let assault_denied = scenario.add_real_card(P0, "Assault", Zone::Exile, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner
        .state_mut()
        .objects
        .get_mut(&assault_denied)
        .unwrap()
        .casting_permissions
        .push(exile_cast_permission_mv_le(4));

    // Sanity: off the stack (in exile) the combined MV is 5.
    assert_eq!(
        runner
            .state()
            .objects
            .get(&assault_denied)
            .unwrap()
            .effective_mana_value(),
        5,
        "in exile, Assault // Battery reports the combined MV 5"
    );

    let denies_at_four =
        !spell_objects_available_to_cast(runner.state(), P0).contains(&assault_denied);
    assert!(
        denies_at_four,
        "an exile-cast permission of MV <= 4 must DENY Assault // Battery (combined \
         MV 5); the front-only MV (1) would wrongly admit it — this asserts the \
         cast-permission legality gate uses effective_mana_value()"
    );

    // Ceiling 5: combined MV 5 must be ALLOWED (5 <= 5).
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let assault_allowed = scenario.add_real_card(P0, "Assault", Zone::Exile, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner
        .state_mut()
        .objects
        .get_mut(&assault_allowed)
        .unwrap()
        .casting_permissions
        .push(exile_cast_permission_mv_le(5));

    assert!(
        spell_objects_available_to_cast(runner.state(), P0).contains(&assault_allowed),
        "an exile-cast permission of MV <= 5 must ALLOW Assault // Battery (combined MV 5)"
    );
}

/// CR 202.3d + CR 702.102b + CR 709.4d: A FUSED split spell on the stack combines
/// the characteristics of BOTH halves — colors AND mana value. Per CR 202.3d, "the
/// mana value of ... a fused split spell on the stack is determined from the
/// combined mana costs of its halves", so the on-stack `zone == Stack` chosen-half
/// gate must NOT apply to a fused spell (the `fused_split_spell` marker overrides
/// it).
///
/// Assault // Battery has no Fuse ability, so it cannot be fused (casting a plain
/// spell//spell split routes through `ModalFaceChoice`, not the Fuse
/// `CastingVariantChoice`). Breaking // Entering is the Fuse fixture card: Breaking
/// {U}{B} (Blue, Black, MV 2) + Entering {4}{B}{R} (Black, Red, MV 6) fuse to
/// colors {Blue, Black, Red} and combined MV 8.
///
/// Revert-failing discriminator: without the `fused_split_spell` marker feeding
/// `effective_mana_value`, the on-stack fused spell reports only the front half's
/// MV (2) and the combined-MV assertion fails.
#[test]
fn fused_split_spell_on_stack_reports_both_halves_colors_and_mana_value() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let breaking = scenario.add_real_card(P0, "Breaking", Zone::Hand, db);
    // Entering ({4}{B}{R}) reanimates a creature card from a graveyard; seed one.
    let milled_creature = scenario.add_real_card(P1, "Grizzly Bears", Zone::Graveyard, db);
    // Fused cost: {U}{B} + {4}{B}{R} = U, B, B, R + 4 generic = 8 mana.
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Blue, breaking, false, vec![]),
            ManaUnit::new(ManaType::Black, breaking, false, vec![]),
            ManaUnit::new(ManaType::Black, breaking, false, vec![]),
            ManaUnit::new(ManaType::Red, breaking, false, vec![]),
            ManaUnit::new(ManaType::Colorless, breaking, false, vec![]),
            ManaUnit::new(ManaType::Colorless, breaking, false, vec![]),
            ManaUnit::new(ManaType::Colorless, breaking, false, vec![]),
            ManaUnit::new(ManaType::Colorless, breaking, false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Breaking (left half) mills; Entering (right half) targets a graveyard creature.
    let commit = runner
        .cast(breaking)
        .casting_variant(engine::types::game_state::CastingVariant::Fuse)
        .target_player(P1)
        .target_object(milled_creature)
        .commit();

    let state = commit.state();
    let stack_entry = state
        .stack
        .last()
        .expect("fused Breaking // Entering should be on the stack");
    let spell = state
        .objects
        .get(&stack_entry.source_id)
        .expect("stack spell object exists");

    assert_eq!(spell.zone, Zone::Stack, "the fused spell is on the stack");
    // CR 702.102b + CR 709.4d: fused spell carries both halves' colors.
    for color in [ManaColor::Blue, ManaColor::Black, ManaColor::Red] {
        assert!(
            spell.color.contains(&color),
            "fused Breaking // Entering must carry {color:?} from its combined halves; \
             got {:?}",
            spell.color
        );
    }
    // CR 202.3d + CR 709.4d: a fused split spell's on-stack mana value is the
    // COMBINED value of both halves ({U}{B} = 2 + {4}{B}{R} = 6 → 8), NOT the
    // chosen/front half alone (2). This asserts the fused_split_spell marker
    // overrides the on-stack chosen-half gate in effective_mana_value.
    assert!(
        spell.fused_split_spell,
        "the fused stack object must be marked fused_split_spell"
    );
    assert_eq!(
        spell.effective_mana_value(),
        8,
        "fused Breaking // Entering on the stack reports the COMBINED MV 8, not the \
         front half's MV 2"
    );

    // CR 202.3d + CR 702.102b: the spell-cast history records the fused spell's
    // COMBINED mana value (8), not the front half (2), so per-turn / per-game
    // "cast a spell with mana value N" history filters see the fused value. The
    // fuse marker is set before payment, so `record_spell_cast_from_zone` (which
    // routes through `spell_mana_value`) captures the combined value.
    let history_mv = state
        .spells_cast_this_turn_by_player
        .get(&P0)
        .and_then(|records| records.last())
        .map(|record| record.mana_value)
        .expect("the fused cast is recorded in spell-cast history");
    assert_eq!(
        history_mv, 8,
        "spell-cast history records fused Breaking // Entering with combined MV 8, not front MV 2"
    );
}
