//! Issue #4948 — Samwise Gamgee's "Sacrifice three Foods: Return target
//! historic card from your graveyard to your hand" could offer one of the
//! just-sacrificed Food tokens as its OWN target. This engine pays a
//! non-self sacrifice activation cost BEFORE choosing the ability's target
//! (a documented CR 601.2c-vs-601.2h/602.2b ordering shortcut — see issue
//! #1301's `exclude_cost_paid_object_that_left_battlefield`), reversed from
//! CR 602.2b's real "choose targets, THEN pay costs" order. A sacrificed
//! Food is itself Historic (it's an artifact) and lands in the graveyard the
//! instant the cost is paid, so — before this fix — it transiently
//! qualified as a legal "target historic card from your graveyard" candidate
//! for the very same activation. CR 704.5d then makes the token cease to
//! exist before the ability resolves, so if it was chosen the ability
//! silently fizzled (CR 608.2b): "targeted Object N, then did nothing when
//! it resolved" — the exact reported symptom. See the maintainer's own
//! root-cause analysis on the issue (`mike-theDude`, 2026-07-10) confirming
//! the cost-before-target ordering risk.
//!
//! Root cause of the leak: `exclude_cost_paid_object_that_left_battlefield`
//! (`game/ability_utils.rs`) already existed for exactly this problem class
//! (issue #1301, Cauldron of Essence), but only tracked a SINGLE cost-paid
//! object (`ResolvedAbility.cost_paid_object`, stamped from `chosen.first()`
//! at each non-self Sacrifice/Discard/Exile cost-payment site). Samwise
//! sacrifices THREE Foods, so only one of the three was ever excluded — the
//! other two remained legal. The fix generalizes this to a
//! `Vec<ObjectId>` (`cost_paid_object_ids`, via the new
//! `ResolvedAbility::add_cost_paid_object_ids_recursive`), populated
//! alongside the existing singular stamp at all three non-self cost-payment
//! sites (`handle_sacrifice_for_cost`, `handle_discard_for_cost`,
//! `finish_exile_selection_for_cost`), so the ONE shared exclusion filter —
//! consulted by both `begin_deferred_target_selection` and
//! `push_activated_ability_to_stack`'s inline target build — now excludes
//! every object the cost consumed, not just the first.
//!
//! The second test below (`discard_two_cost_excludes_both_discarded_cards`)
//! pins a sibling defect an independent review of this fix's first (later
//! reworked) draft found and reproduced live: a multi-object non-self
//! DISCARD cost had the exact same single-object leak, but with a worse
//! consequence than Samwise's silent fizzle — since a discarded non-token
//! card does NOT cease to exist (CR 704.5d is token-specific), the leaked
//! second discarded card was a *legal, resolvable* target, letting a
//! "Discard two creature cards: Return target creature card from your
//! graveyard to your hand"-shaped ability return one of its own just-paid
//! cards for free. The same generalized fix closes this too, since both
//! defects shared the identical single-object root cause and exclusion seam.
//!
//! https://github.com/phase-rs/phase/issues/4948

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SAMWISE_GAMGEE_ORACLE: &str = "Whenever another nontoken creature you control enters, create a Food token. (It's an artifact with \"{2}, {T}, Sacrifice this token: You gain 3 life.\")\nSacrifice three Foods: Return target historic card from your graveyard to your hand. (Artifacts, legendaries, and Sagas are historic.)";

fn food_tokens(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            runner
                .state()
                .objects
                .get(id)
                .is_some_and(|o| o.is_token && o.card_types.subtypes.iter().any(|s| s == "Food"))
        })
        .collect()
}

fn sacrifice_ability_index(runner: &GameRunner, samwise: ObjectId) -> usize {
    runner
        .state()
        .objects
        .get(&samwise)
        .expect("samwise on battlefield")
        .abilities
        .iter()
        .position(|a| a.cost.is_some())
        .expect("Samwise Gamgee has a costed activated ability")
}

#[test]
fn samwise_gamgee_sacrifice_excludes_just_sacrificed_foods_from_own_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let samwise = scenario
        .add_creature_from_oracle(P0, "Samwise Gamgee", 2, 2, SAMWISE_GAMGEE_ORACLE)
        .id();

    // Two real, pre-existing Historic cards in the graveyard. A SECOND
    // legal target is required so target selection is genuinely ambiguous
    // and the engine pauses on `WaitingFor::TargetSelection` instead of
    // auto-resolving a lone legal target inline (see the documented
    // single-legal-target auto-resolve behavior at
    // `casting_costs.rs`'s `deferred target selection: TWO legal damage
    // targets so ... genuinely pauses` test sentinel) — with the
    // cost_paid_object_ids fix correctly excluding all three just-
    // sacrificed Foods, a single pre-existing graveyard card would leave
    // exactly one legal target and never pause at all.
    let old_relic = scenario
        .add_creature_to_graveyard(P0, "Old Relic Golem", 3, 3)
        .as_legendary()
        .id();
    let decoy_relic = scenario
        .add_creature_to_graveyard(P0, "Decoy Relic Golem", 3, 3)
        .as_legendary()
        .id();

    // Cast three 0-cost nontoken creatures to fire Samwise's own ETB
    // trigger three times through the REAL trigger/token-creation pipeline
    // (not a debug token helper) — mirrors issue #1016's test pattern.
    let mut hobbits = Vec::new();
    for i in 0..3 {
        let h = scenario
            .add_creature_to_hand(P0, &format!("Visiting Hobbit {i}"), 1, 1)
            .with_mana_cost(ManaCost::generic(0))
            .id();
        hobbits.push(h);
    }

    let mut runner = scenario.build();
    for h in hobbits {
        runner.cast(h).resolve();
        runner.advance_until_stack_empty();
    }

    let foods = food_tokens(&runner);
    assert_eq!(
        foods.len(),
        3,
        "Samwise's ETB must have created exactly 3 Food tokens; battlefield={:?}",
        runner.state().battlefield
    );

    let ability_index = sacrifice_ability_index(&runner, samwise);
    runner
        .act(GameAction::ActivateAbility {
            source_id: samwise,
            ability_index,
        })
        .expect("announce Samwise Gamgee's sacrifice ability");

    let mut target_checked = false;
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: foods.clone(),
                    })
                    .expect("sacrifice all three Foods to pay the activation cost");
            }
            WaitingFor::TargetSelection { target_slots, .. } => {
                let legal = &target_slots[0].legal_targets;
                for &food in &foods {
                    assert!(
                        !legal.contains(&TargetRef::Object(food)),
                        "issue #4948: a just-sacrificed Food ({food:?}) must never be a legal \
                         target for this SAME activation's own \"target historic card from your \
                         graveyard\" — it wasn't in the graveyard yet when targets are declared \
                         under CR 602.2b's real order; legal_targets={legal:?}"
                    );
                }
                assert!(
                    legal.contains(&TargetRef::Object(old_relic))
                        && legal.contains(&TargetRef::Object(decoy_relic)),
                    "both pre-existing legendary graveyard cards must remain legal targets; \
                     legal_targets={legal:?}"
                );
                target_checked = true;
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(old_relic)),
                    })
                    .expect("choose the real historic graveyard card as the target");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            other if runner.state().stack.is_empty() => {
                panic!("unexpected waiting state: {other:?}");
            }
            _ => {
                runner.act(GameAction::PassPriority).unwrap();
            }
        }
    }

    assert!(
        target_checked,
        "the target-selection exclusion assertion must have run"
    );
    assert_eq!(
        runner.state().objects.get(&old_relic).map(|o| o.zone),
        Some(Zone::Hand),
        "Old Relic Golem must have actually resolved and been returned to hand — not fizzled"
    );
}

const TEST_RELIC_ORACLE: &str =
    "Discard two creature cards: Return target creature card from your graveyard to your hand.";

fn discard_ability_index(runner: &GameRunner, relic: ObjectId) -> usize {
    runner
        .state()
        .objects
        .get(&relic)
        .expect("relic on battlefield")
        .abilities
        .iter()
        .position(|a| a.cost.is_some())
        .expect("Test Relic has a costed activated ability")
}

/// Sibling defect surfaced by an independent review of this fix's first
/// draft (see the module doc): a multi-object non-self DISCARD cost has the
/// same single-cost-paid-object leak sacrifice did, but a card discarded to
/// pay a cost does not cease to exist (unlike a sacrificed token), so the
/// leaked second discarded card is a legal, resolvable target — letting the
/// ability return one of its own just-discarded cards for free. Not the
/// literal card in #4948, but the same root cause and the same fix.
#[test]
fn discard_two_cost_excludes_both_discarded_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let relic = scenario
        .add_creature(P0, "Test Relic", 3, 3)
        .as_artifact()
        .from_oracle_text(TEST_RELIC_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();

    // Two real, pre-existing creature cards in the graveyard — a SECOND
    // legal target is required so target selection is genuinely ambiguous
    // and the engine pauses on `WaitingFor::TargetSelection` instead of
    // auto-resolving a lone legal target inline (see the matching comment
    // on the sacrifice test above).
    let old_bear = scenario
        .add_creature_to_graveyard(P0, "Old Graveyard Bear", 2, 2)
        .id();
    let decoy_bear = scenario
        .add_creature_to_graveyard(P0, "Decoy Graveyard Bear", 2, 2)
        .id();

    let discard_a = scenario.add_creature_to_hand(P0, "Hand Bear A", 2, 2).id();
    let discard_b = scenario.add_creature_to_hand(P0, "Hand Bear B", 2, 2).id();

    let mut runner = scenario.build();
    let ability_index = discard_ability_index(&runner, relic);
    runner
        .act(GameAction::ActivateAbility {
            source_id: relic,
            ability_index,
        })
        .expect("announce Test Relic's discard ability");

    let mut target_checked = false;
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost {
                kind: PayCostKind::Discard,
                ..
            } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![discard_a, discard_b],
                    })
                    .expect("discard both creature cards to pay the activation cost");
            }
            WaitingFor::TargetSelection { target_slots, .. } => {
                let legal = &target_slots[0].legal_targets;
                for &discarded in &[discard_a, discard_b] {
                    assert!(
                        !legal.contains(&TargetRef::Object(discarded)),
                        "a just-discarded card ({discarded:?}) must never be a legal target for \
                         this SAME activation's own \"target creature card from your graveyard\" \
                         — it wasn't in the graveyard yet when targets are declared under CR \
                         602.2b's real order; legal_targets={legal:?}"
                    );
                }
                assert!(
                    legal.contains(&TargetRef::Object(old_bear))
                        && legal.contains(&TargetRef::Object(decoy_bear)),
                    "both pre-existing graveyard creature cards must remain legal targets; \
                     legal_targets={legal:?}"
                );
                target_checked = true;
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(old_bear)),
                    })
                    .expect("choose the real graveyard creature card as the target");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            other if runner.state().stack.is_empty() => {
                panic!("unexpected waiting state: {other:?}");
            }
            _ => {
                runner.act(GameAction::PassPriority).unwrap();
            }
        }
    }

    assert!(
        target_checked,
        "the target-selection exclusion assertion must have run"
    );
    assert_eq!(
        runner.state().objects.get(&old_bear).map(|o| o.zone),
        Some(Zone::Hand),
        "Old Graveyard Bear must have actually resolved and been returned to hand"
    );
    for &discarded in &[discard_a, discard_b] {
        assert_eq!(
            runner.state().objects.get(&discarded).map(|o| o.zone),
            Some(Zone::Graveyard),
            "a just-discarded card must stay discarded, not come back for free \
             (discarded={discarded:?})"
        );
    }
}
