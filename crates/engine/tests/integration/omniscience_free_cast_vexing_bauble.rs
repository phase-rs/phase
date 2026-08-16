//! CR 118.9 + CR 118.9a: Electing the Omniscience free-cast option is the
//! no-mana-spent branch. Vexing Bauble ("Whenever a player casts a spell, if no
//! mana was spent to cast it, counter that spell.") therefore counters a spell
//! cast for free, but NOT the same spell cast for its printed cost (mana spent).
//! This is the production-path proof that the free election spends zero mana and
//! the printed election spends mana — the exact dodge the plan requires.

use engine::game::scenario::{GameScenario, P0};
use engine::types::game_state::CastingVariant;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const OMNISCIENCE: &str = "You may cast spells from your hand without paying their mana costs.";
// Verbatim MH3 Oracle text (card-data.json, 2026-07-20).
const VEXING_BAUBLE: &str = "Whenever a player casts a spell, if no mana was spent to cast it, \
counter that spell.\n{1}, {T}, Sacrifice this artifact: Draw a card.";

/// Casts a {1}{G} bear under Omniscience + Vexing Bauble on a board that can
/// afford the printed cost. `free` elects the free `HandPermission` option;
/// otherwise the printed `Normal` option is paid. Returns the bear's final zone.
fn cast_bear_under_bauble(free: bool) -> Zone {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);

    // Omniscience: the Unlimited CastFromHandFree permission.
    sc.add_creature(P0, "Omniscience", 0, 0)
        .as_enchantment()
        .from_oracle_text(OMNISCIENCE);
    // Vexing Bauble: counters any spell cast with no mana spent.
    sc.add_creature(P0, "Vexing Bauble", 0, 0)
        .as_artifact()
        .from_oracle_text(VEXING_BAUBLE);

    let bear = sc
        .add_creature_to_hand(P0, "Test Bear", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();

    // Fund the printed {1}{G} on BOTH branches so the menu offers free AND
    // printed — the election (not affordability) is the only difference.
    sc.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Green, bear, false, Vec::new()),
            ManaUnit::new(ManaType::Colorless, bear, false, Vec::new()),
        ],
    );

    let mut runner = sc.build();
    let outcome = if free {
        runner.cast(bear).free_cast().resolve()
    } else {
        runner
            .cast(bear)
            .casting_variant(CastingVariant::Normal)
            .resolve()
    };
    outcome.zone_of(bear)
}

#[test]
fn free_election_is_countered_by_vexing_bauble() {
    // CR 118.9a + CR 107.3b: the free election spends no mana, so Vexing Bauble's
    // "if no mana was spent" trigger counters the spell into the graveyard.
    assert_eq!(
        cast_bear_under_bauble(true),
        Zone::Graveyard,
        "a free (no-mana-spent) cast under Omniscience must be countered by Vexing Bauble"
    );
}

#[test]
fn printed_election_dodges_vexing_bauble() {
    // Reach-guard + discriminator: paying the printed {1}{G} spends mana, so the
    // trigger's intervening-if fails and the spell resolves onto the battlefield.
    // If the "free" branch had also spent no mana this would be vacuous — it does
    // not, because THIS branch spends mana and resolves.
    assert_eq!(
        cast_bear_under_bauble(false),
        Zone::Battlefield,
        "casting for the printed cost spends mana, so Vexing Bauble must NOT counter it"
    );
}
