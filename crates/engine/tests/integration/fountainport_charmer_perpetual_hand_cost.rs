//! Fountainport Charmer ({1}{G} Creature — Frog Bard, digital-only Alchemy) —
//! the `ModifyCost` sibling of root cause #19's zone-population `ApplyPerpetual`.
//!
//! Oracle (VERBATIM, verified against `client/public/card-data.json`):
//!   "Offspring {2}\n
//!    When Fountainport Charmer enters, creature cards in your hand perpetually
//!    gain \"This spell costs {1} less to cast.\""
//!
//! This card ships today. Its ETB lowers (via
//! `oracle_effect::try_parse_typed_cards_in_hand_perpetual_gain_cost`) to
//! `ApplyPerpetual { Typed[Card, Creature] + controller You + InZone{Hand},
//! ModifyCost{Reduce, {1}} }` — the SAME filter shape as the `+N/+M` arm, so it
//! rides both engine seams this change touches:
//!
//! * `triggers::extract_target_filter_from_effect`'s zone-population carve-out,
//!   which suppresses the stack-time slot. Before it, the controller was forced
//!   to pick ONE card out of their own hidden hand and only that card got the
//!   reduction — and with no matching card the trigger was DROPPED entirely
//!   (`TriggerDispatchDisposition::DroppedTargetUnresolved`).
//! * `effects::perpetual`'s mass zone branch, which now answers the filter's
//!   "your" predicate through `filter::matches_target_filter_in_owner_zone` for
//!   every `filter::is_owner_scoped_zone` (CR 109.4 + CR 400.3).
//!
//! `crates/engine/src/game/casting_tests.rs`'s
//! `perpetual_mass_hand_cost_reduces_only_matching_cards` hand-builds a
//! `ResolvedAbility` with empty `targets`, so it bypasses
//! `extract_target_filter_from_effect` entirely and cannot see either seam. This
//! file drives the REAL cast pipeline instead.
//!
//! Scope note: the EMPTY-population case (the trigger must still reach the stack
//! when no card matches) is deliberately NOT tested here. Through the cast
//! pipeline a dropped trigger and a resolved no-op trigger are observationally
//! identical — no event, no state delta — so such a test would be vacuous. That
//! claim is pinned at the trigger-dispatch seam instead, by
//! `game::triggers::tests::trigger_hosted_zone_perpetual_is_not_dropped_when_hand_is_empty`,
//! which asserts `stack.len() == 1` and keys on the SAME
//! `apply_perpetual_targets_zone_population` predicate over the SAME filter
//! shape (the `PerpetualModification` variant is irrelevant to that predicate).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::types::ability::PerpetualModification;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::CostModifyMode;
use engine::types::zones::Zone;

const FOUNTAINPORT_CHARMER: &str = "Offspring {2}\nWhen Fountainport Charmer enters, creature \
                                    cards in your hand perpetually gain \"This spell costs {1} \
                                    less to cast.\"";

/// True when the object carries the {1} self-spell cost reduction this card
/// grants (`game/game_object.rs`, the `ModifyCost` arm of
/// `apply_perpetual_modification`).
fn reduced_by_one(state: &GameState, id: ObjectId) -> bool {
    state
        .objects
        .get(&id)
        .expect("object must still exist")
        .perpetual_mods
        .iter()
        .any(|m| {
            matches!(
                m,
                PerpetualModification::ModifyCost {
                    mode: CostModifyMode::Reduce,
                    amount,
                } if *amount == ManaCost::generic(1)
            )
        })
}

/// Which player's hand holds `id`. CR 400.3 makes this the OWNER's, which is the
/// point of the owner-scoping axes below — so it is derived from the actual
/// per-player zone lists rather than read off `obj.owner`.
fn hand_index(state: &GameState, id: ObjectId) -> usize {
    state
        .players
        .iter()
        .position(|p| p.hand.contains(&id))
        .expect("card must be in some player's hand")
}

/// {1}{G} with slack.
fn mana() -> Vec<ManaUnit> {
    let mut pool: Vec<ManaUnit> = (0..2)
        .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
        .collect();
    pool.extend((0..2).map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![])));
    pool
}

/// The cast-pipeline witness for the `ModifyCost` sibling.
///
/// Three claims, each of which flips if part of this change is reverted:
///
/// 1. **Zero target prompts.** No `.target_objects(..)` is declared. If the ETB
///    trigger surfaces a required target slot (the pre-carve-out behaviour) the
///    shared resolution driver panics before any assertion below, so reaching
///    them IS the no-prompt assertion; `final_waiting_for()` restates it.
/// 2. **The whole hand POPULATION is reduced**, not one chosen card — pinned by
///    a SECOND matching hand card that was never a declared target.
/// 3. **The population is scoped by OWNERSHIP, not last-known control**
///    (CR 109.4 + CR 400.3). `stolen` and `loaned` are the two directions; they
///    fail if the mass branch uses plain `matches_target_filter`.
#[test]
fn fountainport_charmer_reduces_the_whole_hand_population_with_no_target_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana());

    let charmer = scenario
        .add_creature_to_hand_from_oracle(P0, "Fountainport Charmer", 2, 3, FOUNTAINPORT_CHARMER)
        .id();

    // Positive subject: TWO creature cards in P0's hand. Two, deliberately —
    // with exactly one matching card the population and a single declared
    // target coincide and the fixture could not discriminate them.
    let hand_bear = scenario.add_creature_to_hand(P0, "Bear", 2, 2).id();
    let hand_ogre = scenario.add_creature_to_hand(P0, "Ogre", 4, 1).id();
    // TYPE axis: a noncreature card in the SAME hand.
    let hand_land = scenario.add_land_to_hand(P0, "Forest").id();
    // PLAIN controller/owner axis: a creature card owned AND last controlled by
    // the opponent, in the opponent's hand.
    let opp_bear = scenario.add_creature_to_hand(P1, "Opp Bear", 2, 2).id();

    // The two LKI-divergent cards. Both start on the battlefield so that leaving
    // it records an LKI snapshot (`apply_zone_exit_cleanup`).
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

    // Both bounce THIS step. `state.lki_cache` is step-scoped, so the divergence
    // is still live when the ETB trigger resolves below.
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), stolen, Zone::Hand, &mut events);
    move_to_zone(runner.state_mut(), loaned, Zone::Hand, &mut events);

    // Reach-guards on the FIXTURE: without these the owner assertions could pass
    // for the trivial reason that no LKI was ever recorded.
    {
        let state = runner.state();
        assert_eq!(
            hand_index(state, stolen),
            0,
            "CR 400.3: the stolen card must be in its OWNER's (P0's) hand"
        );
        assert_eq!(
            hand_index(state, loaned),
            1,
            "CR 400.3: the loaned card must be in its OWNER's (P1's) hand"
        );
        assert_eq!(
            state
                .lki_cache
                .get(&stolen)
                .expect("a battlefield -> hand move records LKI")
                .controller,
            P1,
            "reach-guard: the stolen card's LAST KNOWN controller must be the thief"
        );
        assert_eq!(
            state
                .lki_cache
                .get(&loaned)
                .expect("a battlefield -> hand move records LKI")
                .controller,
            P0,
            "reach-guard: the loaned card's LAST KNOWN controller must be the caster"
        );
    }

    // Revert baseline, asserted BEFORE the cast.
    for id in [hand_bear, hand_ogre, hand_land, opp_bear, stolen, loaned] {
        assert!(
            !reduced_by_one(runner.state(), id),
            "no card may carry the reduction before the trigger resolves"
        );
    }

    // Claim 1: cast with NO declared target intent at all.
    let outcome = runner.cast(charmer).resolve();

    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "Fountainport Charmer's ETB declares no target, so the cast must end at a \
         clean Priority window rather than a hidden-hand pick: {:?}",
        outcome.final_waiting_for()
    );

    // Reach-guard: the creature really resolved and its ETB really fired.
    outcome.assert_zone(&[charmer], Zone::Battlefield);

    // Claim 2: BOTH matching hand cards took the grant.
    assert!(
        reduced_by_one(outcome.state(), hand_bear),
        "the first matching creature card in the caster's hand must be reduced"
    );
    assert!(
        reduced_by_one(outcome.state(), hand_ogre),
        "the SECOND matching hand card must be reduced too — the grant is a zone \
         POPULATION; with a declared target slot only one card would be"
    );

    // Claim 3, POSITIVE direction: a card you OWN that an opponent last
    // controlled is still in YOUR hand and must be reduced. Fails under plain
    // `matches_target_filter`, whose control predicate reads the LKI thief.
    outcome.assert_zone(&[stolen], Zone::Hand);
    assert!(
        reduced_by_one(outcome.state(), stolen),
        "CR 400.3 + CR 108.3: a card you own that an opponent last controlled is in \
         YOUR hand and must take the grant"
    );

    // Claim 3, NEGATIVE direction: a card an OPPONENT owns is in THEIR hand even
    // though you controlled it last. Fails under plain `matches_target_filter`,
    // which would match `ControllerRef::You` against the stale LKI controller and
    // hand the caster's cost reduction to a card in the opponent's hand.
    outcome.assert_zone(&[loaned], Zone::Hand);
    assert!(
        !reduced_by_one(outcome.state(), loaned),
        "CR 400.3: a card an opponent OWNS is in THEIR hand even though you \
         controlled it last — it must NOT take the grant"
    );

    // Type axis, and the plain controller/owner axis.
    assert!(!reduced_by_one(outcome.state(), hand_land), "type axis");
    assert!(
        !reduced_by_one(outcome.state(), opp_bear),
        "controller/owner axis"
    );
    // The source must not be the source-fallback recipient of its own grant.
    assert!(
        !reduced_by_one(outcome.state(), charmer),
        "the mass zone path must never fall back to the trigger source"
    );
}
