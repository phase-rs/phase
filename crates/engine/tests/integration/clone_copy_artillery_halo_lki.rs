//! Clone-copy Orcish Artillery LKI source-name regression (F1).
//!
//! CR 113.7a + CR 602.2a + CR 608.2h + CR 400.7 + CR 702.16e: an activated
//! ability pins its announcement-time source incarnation. A Clone copying
//! Orcish Artillery IS named "Orcish Artillery" on the battlefield; after it
//! is Murdered in response to its own activation, the live graveyard object
//! reads printed "Clone". The untargeted "3 damage to you" must still be
//! prevented by a Runed Halo naming "Orcish Artillery" (exit-LKI name), while
//! a Halo naming anything else prevents nothing.
//!
//! Oracle texts (verbatim):
//! - Orcish Artillery: "{T}: This creature deals 2 damage to any target and 3 damage to you."
//! - Clone: "You may have this creature enter as a copy of any creature on the battlefield."
//! - Runed Halo: "As this enchantment enters, choose a card name.\nYou have protection from the chosen card name. (You can't be targeted, dealt damage, or enchanted by anything with that name.)"
//! - Murder: "Destroy target creature."
//!
//! Choreography (b): Halo shields the controller P0; the copied ability
//! targets unprotected P1, so announcement is legal and no fizzle can zero
//! the board; assertions ride the non-targeted 3-to-you self-hit. P1's −2 is
//! the paired positive reach-guard proving the ability resolved (a fizzle
//! would zero both deltas). No duplication with the printed-Artillery rows in
//! `runed_halo_chosen_card_name_protection.rs`: this file adds only the
//! Clone-copy LKI discrimination.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{ChoiceType, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use std::sync::Arc;

const ORCISH_ARTILLERY_ORACLE: &str =
    "{T}: This creature deals 2 damage to any target and 3 damage to you.";
const CLONE_ORACLE: &str =
    "You may have this creature enter as a copy of any creature on the battlefield.";
const RUNED_HALO_ORACLE: &str = "As this enchantment enters, choose a card name.\nYou have protection from the chosen card name. (You can't be targeted, dealt damage, or enchanted by anything with that name.)";
const MURDER_ORACLE: &str = "Destroy target creature.";

fn pool_mana(color: ManaType) -> ManaUnit {
    ManaUnit::new(color, ObjectId(0), false, vec![])
}

/// Runs the Clone-copy-Artillery-into-Murder sequence with Halo naming
/// `halo_name`. Returns `(P0 life, P1 life)` at `Priority` with an empty
/// stack.
fn run_clone_artillery_halo(halo_name: &str) -> (i32, i32) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // 1. P1 controls Orcish Artillery. P0 hand: Clone, Runed Halo, Murder.
    let artillery = scenario
        .add_creature_from_oracle(P1, "Orcish Artillery", 1, 3, ORCISH_ARTILLERY_ORACLE)
        .id();
    let clone = scenario
        .add_spell_to_hand_from_oracle(P0, "Clone", false, CLONE_ORACLE)
        .as_creature()
        .with_subtypes(vec!["Shapeshifter"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 3,
        })
        .id();
    let halo = scenario
        .add_spell_to_hand_from_oracle(P0, "Runed Halo", false, RUNED_HALO_ORACLE)
        .as_enchantment()
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::White],
            generic: 0,
        })
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", false, MURDER_ORACLE)
        .as_instant()
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 1,
        })
        .id();
    // Clone {3}{U} + Halo {W}{W} + Murder {1}{B}{B}: 4 generic + U + WW + BB.
    scenario.with_mana_pool(
        P0,
        vec![
            pool_mana(ManaType::Colorless),
            pool_mana(ManaType::Colorless),
            pool_mana(ManaType::Colorless),
            pool_mana(ManaType::Colorless),
            pool_mana(ManaType::Blue),
            pool_mana(ManaType::White),
            pool_mana(ManaType::White),
            pool_mana(ManaType::Black),
            pool_mana(ManaType::Black),
        ],
    );
    let mut runner = scenario.build();

    // 2. P0 casts Clone; assert the copy prompt unconditionally; copy P1's
    // Artillery. CR 707.9: the entering creature becomes a copy.
    let card_id = runner.state().objects[&clone].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: clone,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Clone must succeed");
    runner.advance_until_stack_empty();
    let WaitingFor::ReplacementChoice { player, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "Clone entry must pause for the copy replacement, got {}",
            runner.waiting_for_kind()
        );
    };
    assert_eq!(
        player, P0,
        "CR 616.1: the Clone controller answers the copy replacement"
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accepting the copy replacement must succeed");
    let mut saw_copy_target = false;
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { player, .. } => {
                assert_eq!(
                    player, P0,
                    "CR 608.2d: the Clone controller answers the 'you may' pause"
                );
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accepting the copy must succeed");
            }
            WaitingFor::CopyTargetChoice {
                player,
                valid_targets,
                ..
            } => {
                assert_eq!(
                    player, P0,
                    "CR 707.9: the Clone controller chooses what to copy"
                );
                assert!(
                    valid_targets.contains(&artillery),
                    "the copy choice must offer P1's Orcish Artillery, got {valid_targets:?}"
                );
                saw_copy_target = true;
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(artillery)),
                    })
                    .expect("copying P1's Orcish Artillery must succeed");
                break;
            }
            _ => panic!(
                "unexpected prompt while Clone enters: {}",
                runner.waiting_for_kind()
            ),
        }
    }
    assert!(
        saw_copy_target,
        "Clone must prompt for its copy target (CR 707.9)"
    );
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&clone].zone,
        Zone::Battlefield,
        "Clone must resolve onto the battlefield"
    );
    assert_eq!(
        runner.state().objects[&clone].name,
        "Orcish Artillery",
        "reach-guard: Clone entered copying P1's Orcish Artillery"
    );

    // 3. P0 casts Runed Halo; drive the REAL as-enters name prompt (never the
    // direct-mutation helper) and answer it with `halo_name`.
    let card_id = runner.state().objects[&halo].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: halo,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Runed Halo must succeed");
    runner.advance_until_stack_empty();
    assert!(
        !runner.state().battlefield.contains(&halo),
        "CR 614.1c: Runed Halo must NOT be on the battlefield while its as-enters choice is pending"
    );
    let WaitingFor::NamedChoice {
        player,
        choice_type,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "Runed Halo must prompt for a card name, got {}",
            runner.waiting_for_kind()
        );
    };
    assert_eq!(player, P0, "the Halo controller names the card (CR 614.1c)");
    assert!(
        matches!(choice_type, ChoiceType::CardName { .. }),
        "expected ChoiceType::CardName, got {choice_type:?}"
    );
    runner.state_mut().all_card_names = Arc::from([halo_name.to_string()]);
    runner
        .act(GameAction::ChooseOption {
            choice: halo_name.to_string(),
        })
        .expect("choosing the Halo name must succeed");
    runner.advance_until_stack_empty();
    assert!(
        runner.state().battlefield.contains(&halo),
        "Runed Halo must be on the battlefield after its choice resolves"
    );
    assert_eq!(
        runner.state().objects[&halo].chosen_card_name(),
        Some(halo_name),
        "the chosen card name must persist on Runed Halo"
    );

    // 4. The Clone-copy entered this turn; clear summoning sickness per repo
    // convention (cf. `unsick` in activation_cost_target_settlement.rs) so the
    // copied {T} ability is activatable (CR 302.6).
    runner
        .state_mut()
        .objects
        .get_mut(&clone)
        .unwrap()
        .summoning_sick = false;

    // 5. P0 activates the copy's {T} ability, targeting unprotected P1, so
    // announcement is legal and no fizzle can follow.
    let index = runner.state().objects[&clone]
        .abilities
        .iter()
        .position(|a| a.cost.is_some())
        .expect("the Clone-copy must carry Orcish Artillery's tap ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: clone,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activating the Clone-copy must succeed");
    let WaitingFor::TargetSelection { player, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "the copied ability must prompt for its target, got {}",
            runner.waiting_for_kind()
        );
    };
    assert_eq!(
        player, P0,
        "CR 601.2c: the ability controller chooses the target"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("targeting P1 with the 2 damage must succeed");
    assert!(
        runner.state().objects[&clone].tapped,
        "the tap cost is paid"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the copied ability is on the stack before Murder"
    );

    // 6. P0 casts Murder targeting the Clone-copy in response; it resolves
    // first and the copy dies.
    let card_id = runner.state().objects[&murder].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: murder,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Murder must succeed");
    let WaitingFor::TargetSelection { player, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "Murder must prompt for its target, got {}",
            runner.waiting_for_kind()
        );
    };
    assert_eq!(
        player, P0,
        "CR 601.2c: the Murder controller chooses the target"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(clone)),
        })
        .expect("Murder must target the Clone-copy");
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes on Murder");
    runner
        .act(GameAction::PassPriority)
        .expect("P1 passes on Murder");
    assert_eq!(
        runner.state().objects[&clone].zone,
        Zone::Graveyard,
        "reach-guard: Murder destroyed the Clone-copy"
    );
    assert_eq!(
        runner.state().objects[&clone].name,
        "Clone",
        "bug-precondition guard: the live graveyard object is printed Clone again"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the copied ability remains on the stack after Murder resolves"
    );

    // 7. The copied ability resolves: 2 to P1, 3 to its controller P0.
    runner.advance_until_stack_empty();
    assert!(
        runner.state().stack.is_empty(),
        "the stack is empty after resolution"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must complete at Priority, got {}",
        runner.waiting_for_kind()
    );
    (
        runner.state().players[0].life,
        runner.state().players[1].life,
    )
}

/// CR 702.16e + CR 608.2b: with Halo naming "Orcish Artillery", the 3-to-you
/// self-hit from the departed Clone-copy is prevented via its exit-LKI name
/// while the targeted 2 damage to P1 still lands (reach-guard: the ability
/// resolved rather than fizzling). Fails with P0 at 17 when the
/// activation-time latch is reverted.
#[test]
fn clone_copy_artillery_halo_names_artillery_prevents_self_hit() {
    assert_eq!(run_clone_artillery_halo("Orcish Artillery"), (20, 18));
}

/// Control: a Halo naming anything else prevents nothing, so both deltas
/// land. Passes identically with and without the fix, proving the pipeline
/// fires and the named row's 0 is prevention rather than a fizzle.
#[test]
fn clone_copy_artillery_halo_other_name_prevents_nothing() {
    assert_eq!(run_clone_artillery_halo("Grizzly Bears"), (17, 18));
}
