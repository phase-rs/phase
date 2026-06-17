//! Nix — "Counter target spell if no mana was spent to cast it."
//!
//! Parser regression: the trailing intervening-if must lower to a
//! `QuantityCheck` over `ManaSpentToCast { scope: AbilityTarget }`, not
//! remain as swallowed `Condition_If` text.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCondition, CastManaObjectScope, CastManaSpentMetric, Comparator, Effect, QuantityExpr,
    QuantityRef,
};
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastingVariant, StackEntry, StackEntryKind};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const NIX_ORACLE: &str = "Counter target spell if no mana was spent to cast it.";

#[test]
fn nix_parses_counter_with_target_no_mana_spent_condition() {
    let parsed = parse_oracle_text(NIX_ORACLE, "Nix", &[], &["Instant".to_string()], &[]);
    let ability = parsed
        .abilities
        .first()
        .expect("Nix must parse a spell ability");
    assert!(matches!(ability.effect.as_ref(), Effect::Counter { .. }));
    assert!(matches!(
        ability.condition.as_ref(),
        Some(AbilityCondition::QuantityCheck {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::ManaSpentToCast {
                    scope: CastManaObjectScope::AbilityTarget,
                    metric: CastManaSpentMetric::Total,
                },
            },
            comparator: Comparator::EQ,
            rhs: QuantityExpr::Fixed { value: 0 },
        })
    ));
}

/// Place an opponent **creature** spell on the stack as a fixture, recording the
/// amount of mana spent to cast it. Returns the stack object's id.
///
/// A creature (permanent) spell gives a clean, unambiguous counter signal:
/// - countered (CR 701.6a) → its owner's graveyard;
/// - resolved normally (CR 608.3a) → the battlefield.
///
/// An instant/sorcery fixture would be useless here, because a *resolved*
/// instant also ends in the graveyard (CR 608.2m) — indistinguishable from a
/// countered one.
///
/// CR 601.2a + CR 106.6: the cast pipeline records the mana actually spent on
/// the spell object (`mana_spent_to_cast_amount`); Nix reads that tally from the
/// *targeted* spell via `CastManaObjectScope::AbilityTarget`.
fn put_creature_spell_on_stack(
    runner: &mut engine::game::scenario::GameRunner,
    controller: PlayerId,
    name: &str,
    card_id: CardId,
    mana_spent_to_cast: u32,
) -> ObjectId {
    let id = create_object(
        runner.state_mut(),
        card_id,
        controller,
        name.to_string(),
        Zone::Stack,
    );
    {
        let obj = runner.state_mut().objects.get_mut(&id).unwrap();
        obj.card_types.core_types = vec![CoreType::Creature];
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        // CR 106.6 / CR 601.2h: the spell's recorded mana-spent tally — the
        // exact value Nix's intervening-if reads via `AbilityTarget`.
        obj.mana_spent_to_cast = mana_spent_to_cast > 0;
        obj.mana_spent_to_cast_amount = mana_spent_to_cast;
    }
    // `ability: None` — a vanilla permanent spell with no on-resolution effect;
    // it simply enters the battlefield when it resolves.
    runner.state_mut().stack.push_back(StackEntry {
        id,
        source_id: id,
        controller,
        kind: StackEntryKind::Spell {
            card_id,
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: mana_spent_to_cast,
        },
    });
    id
}

/// Drive Nix through the cast pipeline at a target creature spell and report the
/// target's final zone after both spells leave the stack.
///
/// Nix is always cast paying its printed {U} (so Nix's *own* recorded
/// mana-spent tally is 1). Only the targeted spell's tally varies. This isolates
/// the scope under test: a `SelfObject`/controller-scoped read would always see
/// Nix's `1` and never satisfy `== 0`, so the counter would never fire and the
/// creature would always reach the battlefield.
fn nix_at_target_with_mana_spent(target_mana_spent: u32) -> Zone {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mut nix_builder = scenario.add_spell_to_hand_from_oracle(P0, "Nix", true, NIX_ORACLE);
    nix_builder.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Blue],
    });
    let nix = nix_builder.id();
    // CR 601.2g: fund exactly Nix's {U} so the pipeline records 1 mana spent to
    // cast Nix itself — the value a mis-scoped read would pick up.
    scenario.with_mana_pool(P0, vec![ManaUnit::new(ManaType::Blue, nix, false, vec![])]);

    let mut runner = scenario.build();

    let target = put_creature_spell_on_stack(
        &mut runner,
        P1,
        "Opponent Bear",
        CardId(777),
        target_mana_spent,
    );

    let outcome = runner.cast(nix).target_objects(&[target]).resolve();
    outcome.zone_of(target)
}

/// Discriminating runtime test for `CastManaObjectScope::AbilityTarget`.
///
/// CR 115.1 + CR 601.2h + CR 608.2c: Nix's "if no mana was spent to cast it"
/// must read the *targeted* spell's mana tally, not Nix's own. The two cases
/// below straddle the `== 0` boundary on the **target's** tally while Nix's own
/// tally is held fixed at 1:
///
/// - target cast for free (0 mana) → countered → graveyard.
/// - target cast paying 2 mana → not countered → battlefield.
///
/// Were the scope `SelfObject` (Nix itself, tally 1), both cases would read 1,
/// `1 == 0` would be false, the counter would never fire, and BOTH targets
/// would reach the battlefield — so the first assertion below would fail. That
/// makes this test red on a scope revert.
#[test]
fn nix_counter_gates_on_targeted_spell_mana_not_nix_own() {
    // CR 701.6a: countered because the *target's* tally is 0 (read via AbilityTarget).
    assert_eq!(
        nix_at_target_with_mana_spent(0),
        Zone::Graveyard,
        "Nix must counter a target spell cast with no mana spent"
    );
    // CR 608.3a: not countered (target's tally is 2 ≠ 0), so the creature resolves.
    assert_eq!(
        nix_at_target_with_mana_spent(2),
        Zone::Battlefield,
        "Nix must NOT counter a target spell cast paying mana — it resolves to the battlefield"
    );
}
