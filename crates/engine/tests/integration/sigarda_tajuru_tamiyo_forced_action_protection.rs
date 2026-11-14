//! CR 701.9a (discard) + CR 701.21a (sacrifice) + CR 609.3 + CR 109.5:
//! "Spells and abilities your opponents control can't cause you to
//! <action list>." — a player-level protection static shared by three cards.
//!
//! CARD TEXT (verified against `data/mtgjson/AtomicCards.json`):
//!   Sigarda, Host of Herons — "Flying, hexproof\nSpells and abilities your
//!     opponents control can't cause you to sacrifice permanents."
//!   Tajuru Preserver — "Spells and abilities your opponents control can't
//!     cause you to sacrifice permanents."
//!   Tamiyo, Collector of Tales — "Spells and abilities your opponents
//!     control can't cause you to discard cards or sacrifice permanents."
//!     (plus two loyalty abilities, unrelated to this static and already
//!     supported before this change).
//!
//! This engine models the shared clause as `StaticMode::CantCauseForcedAction
//! { cause: ProhibitionScope, actions: Vec<CostCategory> }`, enforced in
//! `game/static_abilities.rs::forced_action_muzzled` and consulted from the
//! `Effect::Sacrifice` resolver (`game/effects/sacrifice.rs`) and the
//! `Effect::Discard`/`Effect::DiscardCard` resolver (`game/effects/discard.rs`).
//!
//! Test 1 (Sigarda/Tajuru shape): an opponent's forced-sacrifice spell is a
//! no-op against the protected player.
//! Test 2: the protected player's OWN sacrifice effect still works normally —
//! the protection only blocks OPPONENT-controlled spells/abilities.
//! Test 3 (Tamiyo shape): an opponent's forced-discard spell is a no-op
//! against the protected player.

use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

/// Tajuru Preserver / Sigarda, Host of Herons's static line (verbatim).
const SACRIFICE_PROTECTION_ORACLE: &str =
    "Spells and abilities your opponents control can't cause you to sacrifice permanents.";

/// Tamiyo, Collector of Tales's first static line (verbatim) — the loyalty
/// abilities that follow it on the real card are unrelated to this static and
/// are not needed to exercise it, so this test isolates the line on a plain
/// creature stand-in (mirrors the "Synthetic Sacrifice"/"Synthetic Edict"
/// convention already used by other integration tests in this suite for
/// isolating a single clause).
const DISCARD_AND_SACRIFICE_PROTECTION_ORACLE: &str = "Spells and abilities your opponents \
control can't cause you to discard cards or sacrifice permanents.";

const EDICT_ORACLE: &str = "Target player sacrifices a creature of their choice.";
const DISCARD_ORACLE: &str = "Target player discards a card.";
const SELF_SACRIFICE_ORACLE: &str = "Sacrifice a creature.";

/// Add `count` units of `ty` mana to `player`'s pool — deterministic payment
/// without modelling lands (mirrors `undying_malice_edict_sacrifice_5942.rs`).
fn add_mana(runner: &mut GameRunner, player: PlayerId, ty: ManaType, count: usize) {
    let unit_source = ObjectId(0);
    let target = runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("player exists");
    for _ in 0..count {
        target
            .mana_pool
            .add(ManaUnit::new(ty, unit_source, false, vec![]));
    }
}

/// CR 701.21a + CR 609.3: an opponent's "Target player sacrifices a creature
/// of their choice" is a no-op against a player protected by
/// `CantCauseForcedAction { actions: [SacrificesPermanent] }` — the whole
/// forced sacrifice fails for that player specifically (CR 609.3: an effect
/// that can't do something does only as much as possible), so every one of
/// their permanents survives, regardless of which one would otherwise have
/// been eligible.
#[test]
fn opponent_edict_is_muzzled_by_sacrifice_protection() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ward = scenario
        .add_creature_from_oracle(P0, "Tajuru Preserver", 3, 2, SACRIFICE_PROTECTION_ORACLE)
        .id();
    let victim = scenario.add_creature(P0, "Fodder Bear", 2, 2).id();

    let edict = scenario
        .add_spell_to_hand_from_oracle(P1, "Synthetic Edict", true, EDICT_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        })
        .id();

    let mut runner = scenario.build();
    add_mana(&mut runner, P1, ManaType::Black, 2);

    // Hand priority to P1 so they may cast their edict (CR 117.3c).
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    // Pre-stage a choice of `victim` for the `EffectZoneChoice` pause that
    // would occur if the muzzle did NOT empty the eligible pool (reach guard:
    // without this, an unmuzzled sacrifice would merely pause unresolved and
    // the assertions below would pass vacuously regardless of the muzzle).
    let outcome = runner
        .cast(edict)
        .target_player(P0)
        .effect_zone(&[victim])
        .resolve();

    // Non-vacuity: BOTH the protected permanent and the otherwise-eligible
    // "Fodder Bear" survive — the muzzle blocks the whole forced sacrifice for
    // the protected player, not just the object carrying the static.
    outcome.assert_zone(&[ward, victim], Zone::Battlefield);
    assert!(
        outcome.state().players[0].graveyard.is_empty(),
        "the protected player must sacrifice nothing"
    );
}

/// CR 701.21a: the protection only stops OPPONENT-controlled spells/abilities
/// — the protected player's OWN sacrifice effect must still function
/// normally. Mutation-test companion to the muzzle test above: this assertion
/// would also fail if `forced_action_muzzled` were (incorrectly) applied
/// without checking the causing ability's controller.
#[test]
fn own_sacrifice_effect_still_works_under_sacrifice_protection() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ward = scenario
        .add_creature_from_oracle(P0, "Tajuru Preserver", 3, 2, SACRIFICE_PROTECTION_ORACLE)
        .id();
    let fodder = scenario.add_creature(P0, "Fodder Bear", 2, 2).id();

    let synthetic_sacrifice = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic Sacrifice", true, SELF_SACRIFICE_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    add_mana(&mut runner, P0, ManaType::Black, 1);

    // Two eligible creatures (the protected permanent itself is a legal
    // choice too), so the sacrifice pauses on `EffectZoneChoice`; choose the
    // non-warded creature to keep the assertions unambiguous.
    let outcome = runner
        .cast(synthetic_sacrifice)
        .effect_zone(&[fodder])
        .resolve();

    outcome.assert_zone(&[fodder], Zone::Graveyard);
    outcome.assert_zone(&[ward], Zone::Battlefield);
}

/// CR 701.9a + CR 609.3: Tamiyo, Collector of Tales shape — an opponent's
/// "Target player discards a card" is a no-op against a player protected by
/// `CantCauseForcedAction { actions: [Discards, SacrificesPermanent] }`. The
/// discard pauses on `WaitingFor::DiscardChoice` when unmuzzled (see
/// `issue_7470_hidden_strings_optional_frame_leak.rs`); under the muzzle it
/// must resolve immediately with no discard and no pause.
#[test]
fn opponent_discard_is_muzzled_by_discard_protection() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _ward = scenario.add_creature_from_oracle(
        P0,
        "Ward Host",
        2,
        2,
        DISCARD_AND_SACRIFICE_PROTECTION_ORACLE,
    );
    let card_a = scenario.add_card_to_hand(P0, "Victim Card A");
    let card_b = scenario.add_card_to_hand(P0, "Victim Card B");

    let discard_spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Synthetic Discard", true, DISCARD_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    add_mana(&mut runner, P1, ManaType::Black, 1);

    // Hand priority to P1 so they may cast their discard spell (CR 117.3c).
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    // Pre-stage a choice of `card_a` for the `DiscardChoice` pause that would
    // occur if the muzzle did NOT prevent the discard (reach guard: without
    // this, an unmuzzled discard would merely pause unresolved and the
    // assertions below would pass vacuously regardless of the muzzle).
    let outcome = runner
        .cast(discard_spell)
        .target_player(P0)
        .discard(&[card_a])
        .resolve();

    // Non-vacuity: both hand cards remain — the muzzle blocks the whole
    // forced discard, not just one candidate card.
    outcome.assert_zone(&[card_a, card_b], Zone::Hand);
    assert!(
        outcome.state().players[0].graveyard.is_empty(),
        "the protected player must discard nothing"
    );
}
