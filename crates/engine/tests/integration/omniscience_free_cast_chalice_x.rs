//! CR 107.3b + CR 601.2b: The free election of an {X} spell under Omniscience
//! locks X to 0 (no mana spent), so its mana value is 0 and Chalice of the Void
//! with zero charge counters counters it. The printed election of the same spell
//! announces X=2 (mana value 2), which Chalice@0 does NOT counter — the spell
//! resolves. This proves the two branches produce different mana values through
//! the real cast pipeline.

use engine::game::scenario::{GameScenario, P0};
use engine::types::game_state::CastingVariant;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const OMNISCIENCE: &str = "You may cast spells from your hand without paying their mana costs.";
// Verbatim Oracle text (card-data.json, 2026-07-20).
const CHALICE: &str =
    "This artifact enters with X charge counters on it.\nWhenever a player casts \
a spell with mana value equal to the number of charge counters on this artifact, counter that \
spell.";

/// Casts an {X} creature under Omniscience + Chalice-of-the-Void-at-zero. `free`
/// elects the free option (X locked to 0); otherwise the printed cost is paid
/// with X=2. Returns the spell's final zone.
fn cast_x_spell_under_chalice(free: bool) -> Zone {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);

    sc.add_creature(P0, "Omniscience", 0, 0)
        .as_enchantment()
        .from_oracle_text(OMNISCIENCE);
    // Placed directly on the battlefield, so the "enters with X charge counters"
    // replacement never fires — Chalice sits at 0 charge counters (Chalice@0),
    // countering only mana-value-0 spells.
    sc.add_creature(P0, "Chalice of the Void", 0, 0)
        .as_artifact()
        .from_oracle_text(CHALICE);

    let spell = sc
        .add_creature_to_hand(P0, "X Wurm", 3, 3)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X],
            generic: 0,
        })
        .id();

    // Fund {2} on both branches so the menu offers free AND printed and the paid
    // X=2 announcement is affordable.
    sc.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Colorless, spell, false, Vec::new()))
            .collect(),
    );

    let mut runner = sc.build();
    let outcome = if free {
        runner.cast(spell).free_cast().resolve()
    } else {
        runner
            .cast(spell)
            .casting_variant(CastingVariant::Normal)
            .x(2)
            .resolve()
    };
    outcome.zone_of(spell)
}

#[test]
fn free_x_spell_is_mana_value_zero_and_countered_by_chalice_at_zero() {
    // CR 107.3b: no alternative cost includes X, so the only legal X is 0 → MV 0 →
    // Chalice@0 counters the spell into the graveyard.
    assert_eq!(
        cast_x_spell_under_chalice(true),
        Zone::Graveyard,
        "a free {{X}} cast has X=0 (MV 0), which Chalice@0 must counter"
    );
}

#[test]
fn paid_x_spell_announces_x_and_dodges_chalice_at_zero() {
    // CR 601.2b: the printed election announces X=2 (MV 2). Chalice@0 counters only
    // MV-0 spells, so this resolves onto the battlefield. Reach-guard: `.x(2)` is
    // consumed only if the ChooseX flow actually surfaced (proving the printed
    // branch spent mana and announced X, not the free branch).
    assert_eq!(
        cast_x_spell_under_chalice(false),
        Zone::Battlefield,
        "the printed X=2 cast has MV 2, which Chalice@0 must NOT counter"
    );
}
