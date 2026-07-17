//! Regression for issue #4966: Waterbender Ascension's activated ability
//! ("Waterbend {4}: Target creature can't be blocked this turn.") never
//! actually made the targeted creature unblockable, because the engine
//! wrongly refused to let the player activate it in the first place.
//!
//! Oracle text (verified against `data/mtgish-cards.json`, card
//! "Waterbender Ascension"):
//! "Whenever a creature you control deals combat damage to a player, put a
//! quest counter on this enchantment. Then if it has four or more quest
//! counters on it, draw a card.
//! Waterbend {4}: Target creature can't be blocked this turn. (While paying
//! a waterbend cost, you can tap your artifacts and creatures to help. Each
//! one pays for {1}.)"
//!
//! Root cause: `AbilityCost::Waterbend`'s `is_payable` affordability
//! pre-check (`crates/engine/src/game/cost_payability.rs`) — consulted by
//! `casting.rs` before an activated ability is even offered as a legal
//! action — delegated to the plain `can_pay_cost_after_auto_tap` helper,
//! which only considers real mana-producing sources (lands, mana rocks) and
//! has no notion of Waterbend's own tap-artifacts-or-creatures-to-help
//! mechanic (CR 601.2b, the entire point of the keyword). A player with
//! zero floating/land mana for the generic leg but plenty of untapped
//! eligible creatures to tap was therefore told the ability wasn't payable
//! at all — the exact "remains blockable" symptom reported, since the
//! ability could never be activated to begin with. Fixed by delegating to
//! `can_feasibly_pay_mana_cost_with_tap_payment_mode` (already used by the
//! spell-cast "additional cost: you may waterbend N" path), which falls
//! back to the plain auto-tap check first, so pool/land-funded payment is
//! unaffected.
//!
//! No prior test paired a Waterbend-cost *activated* ability with a real
//! target (creature) selection driven through the full
//! `ActivateAbility -> pay cost -> choose target -> resolve` pipeline. This
//! test closes that gap end-to-end and asserts the concrete, observable
//! effect: the targeted creature must actually become unblockable, both
//! when the cost is pool-funded and when it's paid via real tap-to-help.

use engine::game::combat::{can_block_pair, AttackTarget};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;

const WATERBENDER_ASCENSION_ORACLE: &str = "Whenever a creature you control deals combat damage to a player, put a quest counter on this enchantment. Then if it has four or more quest counters on it, draw a card.\nWaterbend {4}: Target creature can't be blocked this turn.";

#[test]
fn waterbend_activated_ability_makes_targeted_creature_unblockable() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ascension = scenario
        .add_creature(P0, "Waterbender Ascension", 0, 0)
        .as_enchantment()
        .from_oracle_text(WATERBENDER_ASCENSION_ORACLE)
        .with_mana_cost(ManaCost::NoCost)
        .id();

    let attacker = scenario.add_creature(P0, "Swift Raider", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Guard", 2, 2).id();

    // Fund the Waterbend {4} generic leg straight from the mana pool. This
    // doesn't exercise the affordability bug itself (pool-funded payment
    // was never broken -- see the sibling `waterbend_tap_to_help_...` test
    // for that), but it's a faithful, minimal baseline that the ability's
    // targeting and effect application work at all before layering the
    // tap-to-help payment path on top.
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Colorless, ObjectId(9_999), false, vec![]); 4],
    );

    let mut runner = scenario.build();

    assert!(
        can_block_pair(runner.state(), blocker, attacker),
        "sanity check: the blocker must be a legal block target before the \
         ability resolves"
    );

    runner
        .activate(ascension, 0)
        .target_object(attacker)
        .resolve();

    assert!(
        !can_block_pair(runner.state(), blocker, attacker),
        "issue #4966: \"Waterbend {{4}}: Target creature can't be blocked \
         this turn.\" must make the targeted creature unblockable"
    );

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("DeclareAttackers must succeed");

    assert!(
        runner.declare_blockers(&[(blocker, attacker)]).is_err(),
        "declaring the unblockable creature as blocked must be rejected"
    );
}

/// Same regression, but paying the Waterbend {4} cost via the actual
/// tap-to-help mechanic (`GameAction::TapForConvoke`) instead of a pre-funded
/// mana pool -- the mechanic the reminder text and issue are actually about.
/// `AbilityActivation::resolve()`'s sugar only knows how to finalize
/// `ManaPayment` via a bare `PassPriority` (see its doc comment), so this
/// drives the lower-level `GameAction` sequence directly, mirroring
/// `secret_of_bloodbending_control_window.rs`'s hand-rolled driver loop.
#[test]
fn waterbend_tap_to_help_makes_targeted_creature_unblockable() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ascension = scenario
        .add_creature(P0, "Waterbender Ascension", 0, 0)
        .as_enchantment()
        .from_oracle_text(WATERBENDER_ASCENSION_ORACLE)
        .with_mana_cost(ManaCost::NoCost)
        .id();

    let attacker = scenario.add_creature(P0, "Swift Raider", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Guard", 2, 2).id();

    // Four otherwise-uninvolved creatures to tap for the Waterbend {4} cost --
    // no floating mana at all, so the ManaPayment window can ONLY be finished
    // by tapping.
    let helpers: Vec<ObjectId> = (0..4)
        .map(|i| scenario.add_creature(P0, &format!("Helper {i}"), 1, 1).id())
        .collect();

    let mut runner = scenario.build();

    assert!(
        can_block_pair(runner.state(), blocker, attacker),
        "sanity check: the blocker must be a legal block target before the \
         ability resolves"
    );

    runner
        .act(GameAction::ActivateAbility {
            source_id: ascension,
            ability_index: 0,
        })
        .expect("ActivateAbility must be accepted");

    let mut tapped_for_cost = 0;
    let mut target_chosen = false;
    for _ in 0..64 {
        match &runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => {
                if tapped_for_cost < helpers.len() {
                    runner
                        .act(GameAction::TapForConvoke {
                            object_id: helpers[tapped_for_cost],
                            mana_type: ManaType::Colorless,
                        })
                        .expect("TapForConvoke must be accepted");
                    tapped_for_cost += 1;
                } else {
                    runner
                        .act(GameAction::PassPriority)
                        .expect("finalize Waterbend mana payment");
                }
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(engine::types::ability::TargetRef::Object(attacker)),
                    })
                    .expect("ChooseTarget must be accepted");
                target_chosen = true;
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected WaitingFor::{other:?} while paying Waterbend via tap"),
        }
    }

    assert_eq!(tapped_for_cost, 4, "must tap all 4 helpers to pay {{4}}");
    assert!(target_chosen, "target selection must have been reached");
    for helper in &helpers {
        assert!(
            runner.state().objects[helper].tapped,
            "each helper creature must be tapped after paying its Waterbend leg"
        );
    }

    // Pass priority on both players so the ability resolves off the stack.
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority");
    runner
        .act(GameAction::PassPriority)
        .expect("P1 passes priority to resolve the ability");

    assert!(
        !can_block_pair(runner.state(), blocker, attacker),
        "issue #4966: \"Waterbend {{4}}: Target creature can't be blocked \
         this turn.\" must make the targeted creature unblockable even when \
         the cost is paid via tap-to-help, not a pre-funded mana pool"
    );
}
