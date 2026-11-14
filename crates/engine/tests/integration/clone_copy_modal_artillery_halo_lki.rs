//! Clone-copy Marath modal-damage LKI source-name regression (F1 modal pin).
//!
//! CR 113.7a + CR 602.2a + CR 602.2b + CR 608.2h + CR 400.7 + CR 702.16e: a
//! modal activated ability pins its announcement-time source incarnation over
//! the whole chained ability graph at the `engine_modes.rs` seam. A Clone
//! copying Marath, Will of the Wild IS named "Marath, Will of the Wild" on
//! the battlefield; after it is Murdered in response to its own modal damage
//! activation, the live graveyard object reads printed "Clone". A Runed Halo
//! naming "Marath, Will of the Wild" must still prevent the departed copy's
//! damage via its exit-LKI name, while the unpinned (`None` modal latch)
//! regime reads live "Clone" and lets it through.
//!
//! Oracle texts (verbatim):
//! - Marath, Will of the Wild: "Marath enters with a number of +1/+1 counters on it equal to the amount of mana spent to cast it.\n{X}, Remove X +1/+1 counters from Marath: Choose one —\n• Put X +1/+1 counters on target creature. X can't be 0.\n• Marath deals X damage to any target. X can't be 0.\n• Create an X/X green Elemental creature token. X can't be 0."
//! - Clone: "You may have this creature enter as a copy of any creature on the battlefield."
//! - Runed Halo: "As this enchantment enters, choose a card name.\nYou have protection from the chosen card name. (You can't be targeted, dealt damage, or enchanted by anything with that name.)"
//! - Murder: "Destroy target creature."
//!
//! Choreography: P1 controls Marath (the 0/0 body is kept alive by 3 setup
//! counters) and a setup-placed Runed Halo whose as-enters name is still
//! unchosen, so P1 is a legal target at announcement. P0 Clones the Marath,
//! provisions 2 +1/+1 counters on the copy, announces the damage mode with
//! X=2 targeting P1, Murders the copy in response, and only then is the Halo
//! named "Marath, Will of the Wild" (a real pre-activation naming would make
//! P1 an illegal target at announcement, and the stack is non-empty
//! afterwards so no cast can name it there) before the stack drains.
//! (P0,P1) is (20,20) with the modal pin and (20,18) with the modal pin
//! neutralized back to today's `None` modal state.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{ChosenAttribute, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MARATH_ORACLE: &str = "Marath enters with a number of +1/+1 counters on it equal to the amount of mana spent to cast it.\n{X}, Remove X +1/+1 counters from Marath: Choose one \u{2014}\n\u{2022} Put X +1/+1 counters on target creature. X can't be 0.\n\u{2022} Marath deals X damage to any target. X can't be 0.\n\u{2022} Create an X/X green Elemental creature token. X can't be 0.";
const CLONE_ORACLE: &str =
    "You may have this creature enter as a copy of any creature on the battlefield.";
const RUNED_HALO_ORACLE: &str = "As this enchantment enters, choose a card name.\nYou have protection from the chosen card name. (You can't be targeted, dealt damage, or enchanted by anything with that name.)";
const MURDER_ORACLE: &str = "Destroy target creature.";

fn pool_mana(color: ManaType) -> ManaUnit {
    ManaUnit::new(color, ObjectId(0), false, vec![])
}

/// Runs the Clone-copy-Marath modal-damage sequence with the Halo named
/// "Marath, Will of the Wild" after the Murder. Returns `(P0 life, P1 life)`
/// at `Priority` with an empty stack.
fn run_clone_marath_modal_halo() -> (i32, i32) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // 1. P1 controls Marath and a setup-placed Runed Halo (name unchosen, so
    // no protection yet). P0 hand: Clone, Murder.
    let marath = scenario
        .add_creature_from_oracle(P1, "Marath, Will of the Wild", 0, 0, MARATH_ORACLE)
        .id();
    // The 0/0 body needs counters to survive state-based actions until the
    // Clone copies it; counters are not copiable, so the copy starts clean.
    scenario.with_counter(marath, CounterType::Plus1Plus1, 3);
    let halo = scenario
        .add_enchantment_from_oracle(P1, "Runed Halo", RUNED_HALO_ORACLE)
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
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", false, MURDER_ORACLE)
        .as_instant()
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 1,
        })
        .id();
    // Clone {3}{U} + Murder {1}{B}{B} + X=2: 6 generic + U + BB.
    scenario.with_mana_pool(
        P0,
        vec![
            pool_mana(ManaType::Colorless),
            pool_mana(ManaType::Colorless),
            pool_mana(ManaType::Colorless),
            pool_mana(ManaType::Colorless),
            pool_mana(ManaType::Colorless),
            pool_mana(ManaType::Colorless),
            pool_mana(ManaType::Blue),
            pool_mana(ManaType::Black),
            pool_mana(ManaType::Black),
        ],
    );
    let mut runner = scenario.build();

    // 2. P0 casts Clone; assert the copy prompt unconditionally; copy P1's
    // Marath. CR 707.9: the entering creature becomes a copy.
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
                    valid_targets.contains(&marath),
                    "the copy choice must offer P1's Marath, got {valid_targets:?}"
                );
                saw_copy_target = true;
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(marath)),
                    })
                    .expect("copying P1's Marath must succeed");
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
        "Marath, Will of the Wild",
        "reach-guard: Clone entered copying P1's Marath"
    );

    // 3. Provision 2 +1/+1 counters on the battlefield copy through the
    // `state_mut` escape hatch: the copy object exists only after entry, so
    // no scenario-level provisioning can reach it. This mirrors
    // `GameScenario::with_counter`'s object-map write exactly.
    *runner
        .state_mut()
        .objects
        .get_mut(&clone)
        .expect("the Clone-copy must exist")
        .counters
        .entry(CounterType::Plus1Plus1)
        .or_insert(0) += 2;
    // Reach-guard: the copy carries 6 counters — 4 from Clone-as-Marath's own
    // enters-with-counters ETB (mana spent to cast Clone, {3}{U}) plus the 2
    // provisioned above. The counter-removal cost below needs X=2 payable;
    // this pins the deterministic counter state before activation.
    assert_eq!(
        runner.state().objects[&clone]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&6),
        "the Clone-copy must carry 4 ETB counters plus the 2 provisioned"
    );

    // 4. The Clone-copy entered this turn; clear summoning sickness per repo
    // convention so the copied activated ability is activatable (CR 302.6).
    runner
        .state_mut()
        .objects
        .get_mut(&clone)
        .unwrap()
        .summoning_sick = false;

    // 5. P0 activates the copy's modal ability; assert the mode prompt
    // unconditionally, then choose the damage mode (index 1).
    let index = runner.state().objects[&clone]
        .abilities
        .iter()
        .position(|a| a.cost.is_some())
        .expect("the Clone-copy must carry Marath's modal ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: clone,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activating the Clone-copy must succeed");
    let WaitingFor::AbilityModeChoice {
        player,
        is_activated,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the modal activation must prompt for its mode, got {}",
            runner.waiting_for_kind()
        );
    };
    assert_eq!(
        player, P0,
        "CR 602.2b: the ability controller answers the mode prompt"
    );
    assert!(
        is_activated,
        "the mode prompt must be the activated-ability route"
    );
    runner
        .act(GameAction::SelectModes { indices: vec![1] })
        .expect("choosing the damage mode must succeed");

    // 6. Marath's cost carries X, so the modal funnel diverts into the
    // payment step: assert ChooseXValue unconditionally, then announce X=2
    // (X=0 deals 0 under both regimes and cannot discriminate).
    let WaitingFor::ChooseXValue { player, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "the X-cost activation must prompt for X, got {}",
            runner.waiting_for_kind()
        );
    };
    assert_eq!(player, P0, "CR 601.2f: the activator announces X");
    runner
        .act(GameAction::ChooseX { value: 2 })
        .expect("announcing X=2 must succeed");

    // 7. Target unprotected-for-now P1 with the X=2 damage, so announcement
    // is legal and no fizzle can zero the board.
    let WaitingFor::TargetSelection { player, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "the damage mode must prompt for its target, got {}",
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
        .expect("targeting P1 with the X damage must succeed");
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => break,
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("auto mana payment must continue");
            }
            other => panic!("unexpected prompt while paying: {other:?}"),
        }
    }
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the modal ability is on the stack before Murder"
    );

    // 8. P0 casts Murder targeting the Clone-copy in response; it resolves
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
        "the modal ability remains on the stack after Murder resolves"
    );

    // 9. Name "Marath, Will of the Wild" on P1's Halo now that targeting is
    // committed: P1 gains protection from the departed copy's exit-LKI name.
    runner
        .state_mut()
        .objects
        .get_mut(&halo)
        .expect("Runed Halo must exist")
        .chosen_attributes
        .push(ChosenAttribute::CardName(
            "Marath, Will of the Wild".to_string(),
        ));
    assert_eq!(
        runner.state().objects[&halo].chosen_card_name(),
        Some("Marath, Will of the Wild"),
        "the chosen card name must persist on Runed Halo"
    );

    // 10. The modal ability resolves: prevented with the modal pin (exit-LKI
    // name "Marath, Will of the Wild"), dealt without it (live "Clone").
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

/// CR 602.2b + CR 702.16e: with Halo naming "Marath, Will of the Wild", the
/// departed Clone-copy's modal damage is prevented via its exit-LKI name.
/// Fails with P1 at 18 when the modal pin is neutralized back to today's
/// `None` modal state.
#[test]
fn clone_copy_marath_modal_names_marath_prevents_departed_hit() {
    assert_eq!(run_clone_marath_modal_halo(), (20, 20));
}
