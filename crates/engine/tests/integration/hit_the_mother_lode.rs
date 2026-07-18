//! Runtime cast-pipeline regressions for Hit the Mother Lode (LCI #153):
//!
//!   {4}{R}{R}{R} Sorcery
//!   "Discover 10. If the discovered card's mana value is less than 10, create
//!    a number of tapped Treasure tokens equal to the difference."
//!
//! These drive `apply()` end-to-end (CastSpell → PassPriority → the discover
//! CastOffer accept/decline) so the token count binds and resolves against the
//! real discovered-card referent, never a shape-only AST assertion.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::{CastChoice, GameAction};
use engine::types::game_state::{CastOfferKind, CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// {4}{R}{R}{R} = MV 7 discover source.
fn discover_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Red, ManaCostShard::Red, ManaCostShard::Red],
        generic: 4,
    }
}

const ORACLE: &str = "Discover 10. If the discovered card's mana value is less than 10, create a number of tapped Treasure tokens equal to the difference.";

/// Seed a red-heavy pool that covers {4}{R}{R}{R}.
fn seed_pool(scenario: &mut GameScenario) {
    let units: Vec<ManaUnit> = (0..7)
        .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
        .collect();
    scenario.with_mana_pool(P0, units);
}

/// All Treasure tokens controlled by `player` on the battlefield.
fn treasures(state: &GameState, player: PlayerId) -> Vec<ObjectId> {
    state
        .objects
        .iter()
        .filter(|(_, o)| {
            o.zone == Zone::Battlefield
                && o.controller == player
                && o.card_types.subtypes.iter().any(|s| s == "Treasure")
        })
        .map(|(id, _)| *id)
        .collect()
}

/// Cast the discover spell and pass priority so it resolves. Returns the runner.
fn cast_and_resolve(scenario: GameScenario, spell: ObjectId) -> engine::game::scenario::GameRunner {
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Hit the Mother Lode");
    runner.act(GameAction::PassPriority).expect("p0 pass");
    runner.act(GameAction::PassPriority).expect("p1 pass");
    runner
}

/// CR 701.57c + CR 608.2c: discover hits an MV-3 card; the caster keeps it (to
/// hand). The follow-up creates exactly |3 - 10| = 7 Treasures, and CR 111.10
/// permits the creating effect to add the tapped characteristic. Reverting the difference bind (7 → placeholder → 0)
/// or the tapped flag flips this assertion.
#[test]
fn discover_hit_mv3_to_hand_creates_seven_tapped_treasures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    seed_pool(&mut scenario);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Hit the Mother Lode", false, ORACLE)
        .with_mana_cost(discover_cost())
        .id();
    let hit = scenario
        .add_spell_to_library_top(P0, "MV3 Hit", false)
        .with_mana_cost(ManaCost::generic(3))
        .id();

    let mut runner = cast_and_resolve(scenario, spell);

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CastOffer {
                kind: CastOfferKind::Discover { hit_card, .. },
                ..
            } if hit_card == hit
        ),
        "expected a discover CastOffer for the MV3 hit; got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::DiscoverChoice {
            choice: CastChoice::Decline,
        })
        .expect("decline discover (keep in hand)");

    let created = treasures(runner.state(), P0);
    assert_eq!(
        created.len(),
        7,
        "MV-3 discovery must create 10 - 3 = 7 Treasures; state {:?}",
        runner.state().waiting_for
    );
    assert!(
        created.iter().all(|id| runner.state().objects[id].tapped),
        "every created Treasure must enter tapped (CR 111.10)"
    );
    assert!(
        treasures(runner.state(), P1).is_empty(),
        "the opponent gets no Treasures"
    );
    assert_eq!(
        runner.state().objects[&hit].zone,
        Zone::Hand,
        "the kept discovered card goes to the caster's hand"
    );
}

/// CR 608.2g + CR 701.57c: the caster CASTS the discovered MV-3 card for free —
/// it leaves the exile/library and lands on the stack — and the 7 tapped
/// Treasures are still created after the discover finishes.
#[test]
fn discover_hit_cast_still_creates_seven_treasures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    seed_pool(&mut scenario);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Hit the Mother Lode", false, ORACLE)
        .with_mana_cost(discover_cost())
        .id();
    let hit = scenario
        .add_spell_to_library_top(P0, "MV3 Hit", false)
        .as_creature()
        .with_mana_cost(ManaCost::generic(3))
        .id();

    let mut runner = cast_and_resolve(scenario, spell);
    runner
        .act(GameAction::DiscoverChoice {
            choice: CastChoice::Cast,
        })
        .expect("cast the discovered card for free");

    // CR 608.2g: the accepted discovered card becomes the topmost stack object
    // during the spell's resolution; accepting the offer must not leave it in
    // exile or put it into hand.
    assert_eq!(
        runner.state().objects[&hit].zone,
        Zone::Stack,
        "the accepted discovered card is cast onto the stack"
    );
    assert_eq!(
        treasures(runner.state(), P0).len(),
        7,
        "casting the discovered card still yields 7 Treasures after discover finishes"
    );
}

/// LCI ruling: an {X} card is discovered with X = 0, so an {X}{R} card has mana
/// value 1 — the follow-up creates 10 - 1 = 9 Treasures.
#[test]
fn discover_hit_x_spell_uses_x_zero_mana_value() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    seed_pool(&mut scenario);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Hit the Mother Lode", false, ORACLE)
        .with_mana_cost(discover_cost())
        .id();
    // {X}{R}: mana value 1 with X counted as 0 (CR 202.3e / LCI ruling).
    let hit = scenario
        .add_spell_to_library_top(P0, "X Spell", false)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Red],
            generic: 0,
        })
        .id();

    let mut runner = cast_and_resolve(scenario, spell);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CastOffer {
                kind: CastOfferKind::Discover { hit_card, .. },
                ..
            } if hit_card == hit
        ),
        "expected discover CastOffer for the {{X}}{{R}} hit; got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DiscoverChoice {
            choice: CastChoice::Decline,
        })
        .expect("decline discover");

    assert_eq!(
        treasures(runner.state(), P0).len(),
        9,
        "an {{X}}{{R}} (MV 1) discovery must create 10 - 1 = 9 Treasures"
    );
}

/// CR 701.57c: a discovered card whose mana value is EXACTLY 10 is a legal hit
/// (discover N exiles until MV <= N), but the follow-up condition
/// `MV < 10` is false — 0 Treasures. The referent IS present (MV-0 vs. absent
/// discrimination), so this is a genuine false condition, not the missing-referent
/// guard.
#[test]
fn discover_hit_mv10_creates_no_treasures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    seed_pool(&mut scenario);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Hit the Mother Lode", false, ORACLE)
        .with_mana_cost(discover_cost())
        .id();
    let hit = scenario
        .add_spell_to_library_top(P0, "MV10 Hit", false)
        .with_mana_cost(ManaCost::generic(10))
        .id();

    let mut runner = cast_and_resolve(scenario, spell);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CastOffer {
                kind: CastOfferKind::Discover { hit_card, .. },
                ..
            } if hit_card == hit
        ),
        "an MV-10 card is still a legal discover hit; got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DiscoverChoice {
            choice: CastChoice::Decline,
        })
        .expect("decline discover");

    assert_eq!(
        treasures(runner.state(), P0).len(),
        0,
        "MV == 10 fails the `< 10` condition, so no Treasures are created"
    );
}

/// CR 701.57c: an all-land library yields NO discovered card. "The discovered
/// card's mana value" has no referent, so the missing-referent guard makes the
/// condition false — 0 Treasures. Without the guard, the absent referent
/// resolves to mana value 0, `0 < 10` is true, and |0 - 10| = 10 Treasures would
/// be wrongly created. Paired with the MV-3 hit test (which DOES create 7), this
/// is a non-vacuous discriminator for the presence guard.
#[test]
fn discover_no_hit_all_lands_creates_no_treasures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    seed_pool(&mut scenario);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Hit the Mother Lode", false, ORACLE)
        .with_mana_cost(discover_cost())
        .id();
    // Library holds only lands — discover finds no nonland hit.
    for name in ["Land A", "Land B", "Land C"] {
        scenario.add_spell_to_library_top(P0, name, false).as_land();
    }

    let runner = cast_and_resolve(scenario, spell);

    // No hit means no CastOffer; the spell has fully resolved back to priority.
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::CastOffer {
                kind: CastOfferKind::Discover { .. },
                ..
            }
        ),
        "an all-land discover must not present a cast offer; got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        treasures(runner.state(), P0).len(),
        0,
        "no discovered card means no referent — 0 Treasures (missing-referent guard)"
    );
}
