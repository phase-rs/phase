//! Feed the Bog ({1}{B} Sorcery, digital-only Alchemy) — root cause #19
//! (`docs/parser-misparse-backlog.md`), the SECOND structural shape of the class.
//!
//! Oracle (VERBATIM, verified against `client/public/card-data.json`):
//!   "Replicate {1}{B}\n
//!    Creature cards in your graveyard perpetually get +1/+1. Then return target
//!    creature card with mana value 3 or less from your graveyard to the
//!    battlefield."
//!
//! `begin_anew_perpetual_hand_pump.rs` covers the perpetual as a SUB of an
//! untargeted parent, and `game/effects/perpetual.rs`'s chain-inheritance test
//! covers the perpetual as a SUB of a TARGETED parent. This file covers the
//! remaining shape: `ApplyPerpetual` as the **PARENT**, with a targeted
//! `ChangeZone` sub — and it is the shape where the targeting carve-out changes a
//! SHIPPED card's declared target-slot COUNT. Before the carve-out, Feed the Bog
//! demanded TWO graveyard picks (one for the suppressed `ApplyPerpetual` slot,
//! one for the `ChangeZone`); after it, exactly one.
//!
//! `assign_targets_recursive` (`game/ability_utils.rs`) gates each node's slot on
//! the same `extract_target_filter_from_effect` authority, so suppressing the
//! parent's filter is what makes the parent take no target and hands the single
//! declared target to the sub — and is also what leaves the parent's
//! `ability.targets` EMPTY, the precondition for `perpetual_target_object_ids`
//! reaching its mass zone branch instead of short-circuiting on one chosen card.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::types::ability::PerpetualModification;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const FEED_THE_BOG: &str = "Replicate {1}{B}\nCreature cards in your graveyard perpetually get \
                            +1/+1. Then return target creature card with mana value 3 or less \
                            from your graveyard to the battlefield.";

fn base_pt(state: &GameState, id: ObjectId) -> (Option<i32>, Option<i32>) {
    let obj = state.objects.get(&id).expect("object must still exist");
    (obj.base_power, obj.base_toughness)
}

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

/// Which player's graveyard holds `id`. CR 400.3 makes this the OWNER's, which
/// is the whole point of the owner-scoping test below — so the index is derived
/// from the actual per-player zone lists rather than read off `obj.owner`.
fn graveyard_index(state: &GameState, id: ObjectId) -> usize {
    state
        .players
        .iter()
        .position(|p| p.graveyard.contains(&id))
        .expect("card must be in some player's graveyard")
}

/// {1}{B} with slack.
fn mana() -> Vec<ManaUnit> {
    (0..4)
        .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
        .collect()
}

/// The `ApplyPerpetual`-as-PARENT witness.
///
/// Two independent claims, both of which flip if the targeting carve-out is
/// reverted:
///
/// 1. **Exactly ONE object target slot** — the `ChangeZone`'s. Only ONE object
///    intent is declared. The driver answers one slot per declared object, in
///    written order (CR 601.2c); a second required slot makes `pick_slot_target`
///    panic with "could not satisfy required target slot 1". Reaching the
///    assertions below therefore IS the "exactly one prompt" assertion, and with
///    the carve-out reverted the parent's `ApplyPerpetual` claims slot 0 and the
///    `ChangeZone` slot 1 has nothing left to consume.
/// 2. **Both matching graveyard cards take the perpetual grant** — the mass zone
///    branch really enumerated the POPULATION. With a slot filled,
///    `perpetual_target_object_ids` short-circuits on `ability.targets` and only
///    the one chosen card would be modified.
#[test]
fn feed_the_bog_declares_one_target_and_perpetually_buffs_the_whole_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana());

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Feed the Bog", false, FEED_THE_BOG)
        .id();

    // Positive subject: TWO creature cards in P0's graveyard, DISTINCT base P/T
    // so each assertion is independent. Two, deliberately — with one matching
    // card the population and a single declared target coincide and the fixture
    // could not discriminate. Both have mana value 0, so both are also legal
    // `ChangeZone` targets ("mana value 3 or less").
    let gy_bear = scenario.add_creature_to_graveyard(P0, "Bear", 2, 2).id();
    let gy_ogre = scenario.add_creature_to_graveyard(P0, "Ogre", 4, 1).id();

    // TYPE axis: a noncreature card in the SAME graveyard.
    let gy_shock = scenario.add_spell_to_graveyard(P0, "Shock", true).id();
    // CONTROLLER axis: a creature card in the OPPONENT's graveyard.
    let opp_bear = scenario
        .add_creature_to_graveyard(P1, "Opp Bear", 2, 2)
        .id();
    // ZONE axis: a P0 creature already on the battlefield.
    let board_bear = scenario.add_creature(P0, "Board Bear", 3, 3).id();

    let mut runner = scenario.build();

    // Revert baseline, asserted BEFORE the cast.
    assert_eq!(base_pt(runner.state(), gy_bear), (Some(2), Some(2)));
    assert_eq!(base_pt(runner.state(), gy_ogre), (Some(4), Some(1)));
    assert!(perpetual_mods(runner.state(), gy_bear).is_empty());
    assert!(perpetual_mods(runner.state(), gy_ogre).is_empty());

    // Claim 1: exactly ONE object intent for the whole spell.
    let outcome = runner.cast(spell).target_objects(&[gy_bear]).resolve();

    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "Feed the Bog must resolve to a clean Priority window, not a further prompt: {:?}",
        outcome.final_waiting_for()
    );

    // Reach-guard AND slot-identity proof: the single declared target was
    // consumed by the `ChangeZone` sub, which really ran.
    outcome.assert_zone(&[gy_bear], Zone::Battlefield);

    // The perpetual ran BEFORE the return ("Then return ..."), so the returned
    // card was still in the graveyard when the population was enumerated and
    // enters the battlefield already enlarged (CR 613.4c derives live P/T from
    // the edited base).
    assert_eq!(
        base_pt(outcome.state(), gy_bear),
        (Some(3), Some(3)),
        "the returned card must carry the perpetual edit onto the battlefield"
    );
    assert!(pumped_by_one(perpetual_mods(outcome.state(), gy_bear)));
    let entered = outcome
        .state()
        .objects
        .get(&gy_bear)
        .expect("the returned creature is on the battlefield");
    assert_eq!((entered.power, entered.toughness), (Some(3), Some(3)));

    // Claim 2, the POPULATION claim: the SECOND matching graveyard card was
    // never a declared target, stayed in the graveyard, and still took the
    // grant. This is the assertion that fails if the carve-out is reverted and
    // the parent claims a target slot of its own.
    outcome.assert_zone(&[gy_ogre], Zone::Graveyard);
    assert_eq!(
        base_pt(outcome.state(), gy_ogre),
        (Some(5), Some(2)),
        "the perpetual grant is a zone POPULATION, so the second matching graveyard \
         card must be modified too — with a declared target slot only one would be"
    );
    assert!(pumped_by_one(perpetual_mods(outcome.state(), gy_ogre)));

    // Type axis.
    assert!(perpetual_mods(outcome.state(), gy_shock).is_empty());
    // Controller axis.
    assert_eq!(base_pt(outcome.state(), opp_bear), (Some(2), Some(2)));
    assert!(perpetual_mods(outcome.state(), opp_bear).is_empty());
    // Zone axis.
    assert_eq!(base_pt(outcome.state(), board_bear), (Some(3), Some(3)));
    assert!(perpetual_mods(outcome.state(), board_bear).is_empty());
    // The spell source must not be the source-fallback recipient.
    assert!(perpetual_mods(outcome.state(), spell).is_empty());
}

/// "Your graveyard" is scoped by OWNERSHIP, not by last-known control.
///
/// CR 400.3: an object that would go to any library, graveyard or hand other
/// than its owner's goes to its OWNER's corresponding zone. CR 108.3: ownership
/// is fixed for the whole game. CR 109.4: an object outside the battlefield and
/// stack has no controller at all — so the only player-scoping a graveyard
/// population can legitimately use is ownership.
///
/// The engine's `effective_controller` (`game/filter.rs`) answers a control
/// predicate for a non-battlefield object out of `state.lki_cache`, i.e. with
/// the LAST KNOWN controller. This test builds exactly the state where that
/// diverges from ownership — two creatures that changed hands and then died in
/// the same step — and pins that the perpetual grant follows the OWNER:
///
/// * `stolen` is OWNED by P0, was CONTROLLED by P1, and died into P0's
///   graveyard. It IS in "your graveyard" and MUST take the grant. This
///   assertion fails under plain `matches_target_filter`, whose control
///   predicate reads the LKI thief (P1).
/// * `loaned` is OWNED by P1, was CONTROLLED by P0, and died into P1's
///   graveyard. It is NOT in "your graveyard" and MUST NOT take the grant.
///   This assertion fails under plain `matches_target_filter`, which would
///   match `ControllerRef::You` against the stale LKI controller and hand the
///   caster's grant to a card in the opponent's graveyard — nothing else on
///   this path is player-scoped, since `zone_object_ids` sweeps EVERY player's
///   graveyard (`game/targeting.rs`) and `FilterProp::InZone` only compares
///   `obj.zone` (`game/filter.rs`).
///
/// The shipped scoping lives at the RESOLVER seam, not in the parsed filter:
/// `game/effects/perpetual.rs`'s mass zone branch answers the predicate through
/// `filter::matches_target_filter_in_owner_zone` for every
/// `filter::is_owner_scoped_zone`. The subject filter keeps `parse_type_phrase`'s
/// plain `controller: Some(ControllerRef::You)` shape.
///
/// `opp_bear` (owned AND last controlled by P1) is the plain controller-axis
/// negative; it is already covered above but repeated here so this fixture is
/// self-contained even if the player scoping is dropped entirely.
#[test]
fn feed_the_bog_scopes_the_graveyard_population_by_owner_not_last_known_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana());

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Feed the Bog", false, FEED_THE_BOG)
        .id();

    // The declared `ChangeZone` target: an ordinary P0-owned graveyard creature
    // with no LKI at all.
    let gy_bear = scenario.add_creature_to_graveyard(P0, "Bear", 2, 2).id();
    // Plain controller-axis negative: owned and controlled by P1 throughout.
    let opp_bear = scenario
        .add_creature_to_graveyard(P1, "Opp Bear", 2, 2)
        .id();

    // Both start on the battlefield so that the move to the graveyard records an
    // LKI snapshot (`apply_zone_exit_cleanup` snapshots on a battlefield exit).
    let stolen = scenario.add_creature(P0, "Stolen Ogre", 4, 1).id();
    let loaned = scenario.add_creature(P1, "Loaned Golem", 3, 3).id();

    let mut runner = scenario.build();

    // The theft, and its mirror image.
    runner
        .state_mut()
        .objects
        .get_mut(&stolen)
        .expect("stolen creature exists")
        .controller = P1;
    runner
        .state_mut()
        .objects
        .get_mut(&loaned)
        .expect("loaned creature exists")
        .controller = P0;

    // Both die THIS step. `state.lki_cache` is step-scoped, so the divergence is
    // still live when the spell resolves below.
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), stolen, Zone::Graveyard, &mut events);
    move_to_zone(runner.state_mut(), loaned, Zone::Graveyard, &mut events);

    // Reach-guards: the fixture really is the divergent state this test is about.
    // Without these the owner assertion below could pass for the trivial reason
    // that no LKI was ever recorded.
    {
        let state = runner.state();
        assert_eq!(
            graveyard_index(state, stolen),
            0,
            "CR 400.3: the stolen card must be in its OWNER's (P0's) graveyard"
        );
        assert_eq!(
            graveyard_index(state, loaned),
            1,
            "CR 400.3: the loaned card must be in its OWNER's (P1's) graveyard"
        );
        assert_eq!(
            state
                .lki_cache
                .get(&stolen)
                .expect("a battlefield -> graveyard move records LKI")
                .controller,
            P1,
            "reach-guard: the stolen card's LAST KNOWN controller must be the thief"
        );
        assert_eq!(
            state
                .lki_cache
                .get(&loaned)
                .expect("a battlefield -> graveyard move records LKI")
                .controller,
            P0,
            "reach-guard: the loaned card's LAST KNOWN controller must be the caster"
        );
        assert_eq!(base_pt(state, stolen), (Some(4), Some(1)));
        assert_eq!(base_pt(state, loaned), (Some(3), Some(3)));
    }

    let outcome = runner.cast(spell).target_objects(&[gy_bear]).resolve();

    // Positive reach-guard: the spell really resolved and the `ChangeZone` sub
    // consumed the single declared target.
    outcome.assert_zone(&[gy_bear], Zone::Battlefield);
    assert_eq!(base_pt(outcome.state(), gy_bear), (Some(3), Some(3)));

    // OWNER-SCOPED POSITIVE. Fails under plain `matches_target_filter`, whose
    // control predicate reads the LKI thief.
    outcome.assert_zone(&[stolen], Zone::Graveyard);
    assert_eq!(
        base_pt(outcome.state(), stolen),
        (Some(5), Some(2)),
        "CR 400.3 + CR 108.3: a card you OWN that an opponent last controlled is \
         still in YOUR graveyard and must take the grant"
    );
    assert!(pumped_by_one(perpetual_mods(outcome.state(), stolen)));

    // OWNER-SCOPED NEGATIVE. Fails if the mass branch stops dispatching through
    // `matches_target_filter_in_owner_zone`.
    outcome.assert_zone(&[loaned], Zone::Graveyard);
    assert_eq!(
        base_pt(outcome.state(), loaned),
        (Some(3), Some(3)),
        "CR 400.3: a card an OPPONENT owns is in THEIR graveyard even though you \
         controlled it last — it must NOT take the grant"
    );
    assert!(perpetual_mods(outcome.state(), loaned).is_empty());

    // Plain controller-axis negative, restated for self-containment.
    assert_eq!(base_pt(outcome.state(), opp_bear), (Some(2), Some(2)));
    assert!(perpetual_mods(outcome.state(), opp_bear).is_empty());

    // The spell must still not be the source-fallback recipient.
    assert!(perpetual_mods(outcome.state(), spell).is_empty());
}
