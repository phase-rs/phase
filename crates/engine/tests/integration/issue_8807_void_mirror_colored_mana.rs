//! CR 603.4 + CR 106.1a + CR 601.2h, issue #8807: Void Mirror's intervening-if
//! reads the COLOR axis of the payment record, not the amount.
//!
//! > Whenever a player casts a spell, if no colored mana was spent to cast it,
//! > counter that spell.
//!
//! The clause never parsed, so the trigger degraded to an unconditional
//! "whenever a player casts a spell, counter that spell" and the mirror
//! countered everything. Both branches below spend the SAME amount of mana on
//! the SAME card for the SAME cost — only the color of the mana differs — so
//! the pair discriminates the color axis from the amount axis (which is Vexing
//! Bauble's clause, covered by `omniscience_free_cast_vexing_bauble`).

use engine::game::scenario::{GameScenario, P0};
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// Verbatim MH2 Oracle text (Scryfall, 2026-09-11).
const VOID_MIRROR: &str =
    "Whenever a player casts a spell, if no colored mana was spent to cast it, counter that spell.";

/// Casts a generic-cost `{2}` creature under Void Mirror, paying the whole cost
/// with two mana of `mana_type`. CR 107.4b: generic mana in costs can be paid
/// with any type of mana, so the card, the cost and the amount spent are
/// identical across branches. Returns the creature's final zone.
fn cast_generic_two_drop_paying_with(mana_type: ManaType) -> Zone {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);

    sc.add_creature(P0, "Void Mirror", 0, 0)
        .as_artifact()
        .from_oracle_text(VOID_MIRROR);

    let bear = sc
        .add_creature_to_hand(P0, "Test Bear", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: Vec::new(),
            generic: 2,
        })
        .id();

    sc.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(mana_type, bear, false, Vec::new()),
            ManaUnit::new(mana_type, bear, false, Vec::new()),
        ],
    );

    let mut runner = sc.build();
    runner.cast(bear).resolve().zone_of(bear)
}

#[test]
fn colorless_payment_is_countered_by_void_mirror() {
    // CR 106.1a + CR 106.1b: colorless is a type of mana but not a color, so a
    // cost paid entirely with colorless mana spends no colored mana and the
    // intervening-if holds.
    assert_eq!(
        cast_generic_two_drop_paying_with(ManaType::Colorless),
        Zone::Graveyard,
        "a cost paid entirely with colorless mana spends no colored mana, \
         so Void Mirror must counter the spell"
    );
}

#[test]
fn colored_payment_dodges_void_mirror() {
    // Discriminator for the reported bug: before the fix the clause was dropped
    // and this spell was countered too. It is not a vacuous negative — the
    // sibling test above proves the trigger still fires and still counters when
    // the condition genuinely holds.
    assert_eq!(
        cast_generic_two_drop_paying_with(ManaType::Green),
        Zone::Battlefield,
        "paying the same generic cost with green mana spends colored mana, \
         so Void Mirror's intervening-if must fail and the spell must resolve"
    );
}
