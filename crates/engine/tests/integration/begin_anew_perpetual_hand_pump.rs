//! Begin Anew ({G}{G}{W}{W} Sorcery, digital-only Alchemy) — root cause #19
//! (`docs/parser-misparse-backlog.md`) regression suite.
//!
//! Oracle (VERBATIM, verified against Scryfall and `client/public/card-data.json`):
//!   "Destroy all creatures. Creature cards in your hand perpetually get +1/+1."
//!
//! Before this fix the second sentence lowered to
//! `Effect::Pump { power: Fixed(1), toughness: Fixed(1), target: Any }` with a
//! `null` duration — the perpetual routing AND the entire subject filter were
//! dropped, while coverage still reported `supported: true`.
//!
//! It must lower to `Effect::ApplyPerpetual { Typed[Creature] + controller You +
//! InZone{Hand}, ModifyPowerToughness{1,1} }` — AND (CR 115.1) it must declare
//! NO target, so casting surfaces zero prompts and the spell stays castable when
//! the hand holds no creature card.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::PerpetualModification;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const BEGIN_ANEW: &str =
    "Destroy all creatures. Creature cards in your hand perpetually get +1/+1.";

/// `(base_power, base_toughness)` — the fields the perpetual edit writes
/// (`game/game_object.rs`, `ModifyPowerToughness` arm).
fn base_pt(state: &GameState, id: ObjectId) -> (Option<i32>, Option<i32>) {
    let obj = state.objects.get(&id).expect("object must still exist");
    (obj.base_power, obj.base_toughness)
}

/// The recorded perpetual modifications (`GameObject::perpetual_mods`).
fn perpetual_mods(state: &GameState, id: ObjectId) -> &[PerpetualModification] {
    &state
        .objects
        .get(&id)
        .expect("object must still exist")
        .perpetual_mods
}

fn pumped_by_one(mods: &[PerpetualModification]) -> bool {
    mods.iter().any(|m| {
        matches!(
            m,
            PerpetualModification::ModifyPowerToughness {
                power_delta: 1,
                toughness_delta: 1,
            }
        )
    })
}

/// {G}{G}{W}{W} plus slack (Test 2 casts a second spell afterwards).
fn mana() -> Vec<ManaUnit> {
    let mut pool: Vec<ManaUnit> = (0..4)
        .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
        .collect();
    pool.extend((0..4).map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![])));
    pool
}

/// Test 1 — the multi-axis hostile fixture (claims C2 and C11).
///
/// Every axis of the subject filter is given an independent counterexample that
/// differs from the positive subject on exactly ONE property.
#[test]
fn begin_anew_perpetually_buffs_only_your_hand_creature_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana());

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Begin Anew", false, BEGIN_ANEW)
        .id();

    // Positive subject: TWO Creature cards in P0's hand. Two, deliberately: with
    // exactly one matching card the population and a single declared target
    // coincide, so the fixture could not tell "the whole hand population was
    // enumerated" from "one card was picked out of a hidden hand" — the very
    // defect this suite pins. Distinct P/T so each assertion is independent.
    let hand_bear = scenario.add_creature_to_hand(P0, "Bear", 2, 2).id();
    let hand_ogre = scenario.add_creature_to_hand(P0, "Ogre", 4, 1).id();

    // H2 — TYPE axis: a noncreature card in the SAME hand and the SAME zone.
    let hand_land = scenario.add_land_to_hand(P0, "Forest").id();

    // H1 — CONTROLLER axis: a Creature card in the OPPONENT's hand.
    let opp_bear = scenario.add_creature_to_hand(P1, "Opp Bear", 2, 2).id();

    // H3 — ZONE axis, ISOLATED. A P0-controlled CREATURE on the BATTLEFIELD that
    // SURVIVES the first sentence. CR 702.12b: a permanent with indestructible
    // can't be destroyed — enforced for the `DestroyAll` path by the battlefield
    // filter inside `destroy::resolve_all`. It matches the type axis and the
    // controller axis, so the ONLY thing that can exclude it is that
    // `zone_object_ids(state, Zone::Hand)` never enumerates the battlefield —
    // which is exactly the claim. (A vanilla creature here would be in the
    // GRAVEYARD by the time ApplyPerpetual resolves and would not isolate the
    // zone axis.)
    let board_indestructible = scenario
        .add_creature(P0, "Indestructible Bear", 3, 3)
        .indestructible()
        .id();

    // CR 701.8a reach-guard + graveyard negative: a plain vanilla that DOES die.
    let board_vanilla = scenario.add_vanilla(P0, 3, 3);

    let mut runner = scenario.build();

    // Revert baseline, asserted BEFORE the cast.
    assert_eq!(base_pt(runner.state(), hand_bear), (Some(2), Some(2)));
    assert_eq!(base_pt(runner.state(), hand_ogre), (Some(4), Some(1)));
    assert!(perpetual_mods(runner.state(), hand_bear).is_empty());
    assert!(perpetual_mods(runner.state(), hand_ogre).is_empty());

    // C11: NO `.targeting(..)` is declared. If the `ApplyPerpetual` sub-ability
    // surfaces a required target slot (the pre-carve-out behaviour) the harness
    // panics in `pick_slot_target` before reaching any assertion below —
    // reaching the next line IS the no-spurious-prompt assertion.
    let outcome = runner.cast(spell).resolve();

    // C11, explicit form: the pipeline halted at a clean priority window, not at
    // a target/trigger prompt.
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "Begin Anew must resolve to a clean Priority window, not a target prompt: {:?}",
        outcome.final_waiting_for()
    );

    // CR 701.8a: the DestroyAll half really ran — the positive reach-guard that
    // every negative assertion below is paired with.
    outcome.assert_zone(&[board_vanilla], Zone::Graveyard);
    outcome.assert_zone(&[board_indestructible], Zone::Battlefield);

    // Digital-only Alchemy (no CR entry for "perpetually"); the delta itself is a
    // CR 613.4c layer-7c power/toughness modification recorded on the card.
    assert_eq!(
        base_pt(outcome.state(), hand_bear),
        (Some(3), Some(3)),
        "the creature card in the controller's hand must take a PERMANENT base P/T edit"
    );
    assert!(pumped_by_one(perpetual_mods(outcome.state(), hand_bear)));
    // The POPULATION claim: EVERY matching card in the zone, not one chosen card.
    assert_eq!(
        base_pt(outcome.state(), hand_ogre),
        (Some(5), Some(2)),
        "the perpetual grant is a zone POPULATION, so the second matching hand card \
         must be modified too — with a declared target slot only one would be"
    );
    assert!(pumped_by_one(perpetual_mods(outcome.state(), hand_ogre)));

    // H2 — type axis.
    assert!(perpetual_mods(outcome.state(), hand_land).is_empty());
    // H1 — controller axis.
    assert_eq!(base_pt(outcome.state(), opp_bear), (Some(2), Some(2)));
    assert!(perpetual_mods(outcome.state(), opp_bear).is_empty());
    // H3 — zone axis, isolated (same controller, same type, battlefield).
    assert_eq!(
        base_pt(outcome.state(), board_indestructible),
        (Some(3), Some(3)),
        "the surviving battlefield creature must keep its printed 3/3 base"
    );
    assert!(perpetual_mods(outcome.state(), board_indestructible).is_empty());
    // Graveyard negative (the dead vanilla is not in Hand either).
    assert!(perpetual_mods(outcome.state(), board_vanilla).is_empty());
    // H-source: the spell object must NOT be the source-fallback target.
    assert!(perpetual_mods(outcome.state(), spell).is_empty());
}

/// Test 2 — the PERMANENCE discriminator (claim C3). This is what separates
/// `ApplyPerpetual` from *any* `Pump`, however well targeted: a `Pump` is a
/// battlefield-scoped transient continuous effect swept by
/// `prune_end_of_turn_effects` and cannot touch a card sitting in hand at all,
/// so the buffed card could never ENTER the battlefield already enlarged.
#[test]
fn begin_anew_perpetual_buff_survives_the_card_entering_the_battlefield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana());

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Begin Anew", false, BEGIN_ANEW)
        .id();
    let hand_bear = scenario.add_creature_to_hand(P0, "Bear", 2, 2).id();
    // Second matching card so the population is not a one-element set (see
    // Test 1) — it stays in hand and is never cast.
    let hand_ogre = scenario.add_creature_to_hand(P0, "Ogre", 4, 1).id();
    let board_vanilla = scenario.add_vanilla(P0, 3, 3);

    let mut runner = scenario.build();
    assert_eq!(base_pt(runner.state(), hand_bear), (Some(2), Some(2)));

    let outcome = runner.cast(spell).resolve();
    // Reach-guard: the spell really resolved.
    outcome.assert_zone(&[board_vanilla], Zone::Graveyard);
    assert_eq!(base_pt(outcome.state(), hand_bear), (Some(3), Some(3)));
    assert_eq!(base_pt(outcome.state(), hand_ogre), (Some(5), Some(2)));

    // Now cast the buffed card. CR 613.4c: the layer pass derives live P/T from
    // the edited base, so it must ENTER as a 3/3.
    let outcome = runner.cast(hand_bear).resolve();
    outcome.assert_zone(&[hand_bear], Zone::Battlefield);
    let entered = outcome
        .state()
        .objects
        .get(&hand_bear)
        .expect("the creature is on the battlefield");
    assert_eq!(
        (entered.power, entered.toughness),
        (Some(3), Some(3)),
        "a perpetual +1/+1 granted while the card was in hand must still be live \
         after it enters the battlefield (a Pump could never do this)"
    );
    assert!(pumped_by_one(&entered.perpetual_mods));
}

/// Test 3 — the empty-set reach-guard and the castability claim (C5, C12, H4).
///
/// CR 115.1: the perpetual clause declares no target, so the spell must be
/// castable with no creature card anywhere in hand. Pre-carve-out this returns
/// `Err(EngineError::ActionNotAllowed("No legal targets available"))` from
/// `no_legal_target_slots()`, because the `Typed[Creature] + InZone{Hand} + You`
/// filter matches nothing and the ability is not `optional_targeting`.
#[test]
fn begin_anew_with_no_matching_hand_card_is_castable_and_does_not_hit_the_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana());

    // P0's hand holds ONLY Begin Anew — no creature cards in ANY hand.
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Begin Anew", false, BEGIN_ANEW)
        .id();
    let board_vanilla = scenario.add_vanilla(P0, 3, 3);

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).try_resolve().expect(
        "Begin Anew must be castable with no creature card in hand \
         (CR 115.1: the perpetual clause declares no target)",
    );

    // Reach-guard: the spell actually resolved.
    outcome.assert_zone(&[board_vanilla], Zone::Graveyard);
    // C5: the empty matching set must NOT fall back to the source
    // (`ids.push(ability.source_id)` is unreachable on the mass zone path).
    assert!(
        perpetual_mods(outcome.state(), spell).is_empty(),
        "an empty hand population must not fall back to the spell source"
    );
    assert!(perpetual_mods(outcome.state(), board_vanilla).is_empty());
}
