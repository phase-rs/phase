//! Clone-copy Marath modal-damage LKI source-name regression.
//!
//! CR 113.7a + CR 602.2b + CR 608.2h + CR 400.7 + CR 702.16e: the damage of a
//! modal activated ability reads its departed source through the exit snapshot
//! of the source incarnation the ability is pinned to
//! (`deal_damage::damage_context_from_ability`). A Clone
//! copying Marath, Will of the Wild IS named "Marath, Will of the Wild" on
//! the battlefield; after it is Murdered in response to its own modal damage
//! activation, the live graveyard object reads printed "Clone". A Runed Halo
//! naming "Marath, Will of the Wild" must still prevent the departed copy's
//! damage via its exit-LKI name; a read with no pinned incarnation sees live
//! "Clone" and lets it through.
//!
//! Oracle texts (verbatim):
//! - Marath, Will of the Wild: "Marath enters with a number of +1/+1 counters on it equal to the amount of mana spent to cast it.\n{X}, Remove X +1/+1 counters from Marath: Choose one —\n• Put X +1/+1 counters on target creature. X can't be 0.\n• Marath deals X damage to any target. X can't be 0.\n• Create an X/X green Elemental creature token. X can't be 0."
//! - Clone: "You may have this creature enter as a copy of any creature on the battlefield."
//! - Runed Halo: "As this enchantment enters, choose a card name.\nYou have protection from the chosen card name. (You can't be targeted, dealt damage, or enchanted by anything with that name.)"
//! - Murder: "Destroy target creature."
//! - Jade Monolith: "{1}: The next time a source of your choice would deal damage to target creature this turn, that source deals that damage to you instead."
//!
//! Choreography: P0 casts Runed Halo through its real as-enters prompt
//! before anything targets, so protection is legally established with no
//! mid-game chosen-name mutation. P0 is therefore an ILLEGAL target for the
//! modal damage (CR 702.16b) — the test pins that rejection — so P0's
//! Clone-copy-Marath announces X=2 at P1's unprotected Grizzly Bears, P0
//! redirects the hit to themself with Jade Monolith (choosing the still
//! alive copy as the source), and P0 Murders the copy in response. The
//! modal resolves: redirected to the Halo-protected P0 and prevented via
//! the exit-LKI name. (P0,P1) is (20,20) with the incarnation-pinned read and
//! (18,20) when that read is given `None`; the unprotected control (Halo
//! naming "Grizzly Bears") takes the redirected 2.

use std::sync::Arc;

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
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
const JADE_MONOLITH_ORACLE: &str = "{1}: The next time a source of your choice would deal damage to target creature this turn, that source deals that damage to you instead.";

fn pool_mana(color: ManaType) -> ManaUnit {
    ManaUnit::new(color, ObjectId(0), false, vec![])
}

/// Runs the Clone-copy-Marath modal-damage sequence with P0's Halo naming
/// `halo_name` (cast through its real as-enters prompt) and the X=2 hit
/// redirected from P1's unprotected Bears to P0. Returns `(P0 life, P1 life)`
/// at `Priority` with an empty stack.
fn run_clone_marath_modal_redirect(halo_name: &str) -> (i32, i32) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // 1. P1 controls Marath and an unprotected Grizzly Bears. P0 controls
    // Jade Monolith. P0 hand: Runed Halo (cast through its real prompt in
    // step 2), Clone, Murder.
    let marath = scenario
        .add_creature_from_oracle(P1, "Marath, Will of the Wild", 0, 0, MARATH_ORACLE)
        .id();
    // The 0/0 body needs counters to survive state-based actions until the
    // Clone copies it; counters are not copiable, so the copy starts clean.
    scenario.with_counter(marath, CounterType::Plus1Plus1, 3);
    let monolith = scenario
        .add_artifact_from_oracle(P0, "Jade Monolith", JADE_MONOLITH_ORACLE)
        .id();
    let bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let halo = scenario
        .add_spell_to_hand_from_oracle(P0, "Runed Halo", false, RUNED_HALO_ORACLE)
        .as_enchantment()
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::White],
            generic: 0,
        })
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
    // Halo {W}{W} + Clone {3}{U} + Murder {1}{B}{B} + X=2 + one Monolith
    // {1} activation: 7 generic + U + WW + BB.
    scenario.with_mana_pool(
        P0,
        vec![
            pool_mana(ManaType::Colorless),
            pool_mana(ManaType::Colorless),
            pool_mana(ManaType::Colorless),
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
    runner.state_mut().all_card_names = Arc::from([
        "Marath, Will of the Wild".to_string(),
        "Grizzly Bears".to_string(),
    ]);

    // 2. P0 casts Runed Halo through its REAL as-enters name prompt (never a
    // mid-game mutation) and names `halo_name` before anything targets.
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
    let WaitingFor::NamedChoice { player, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "Runed Halo must prompt for a card name, got {}",
            runner.waiting_for_kind()
        );
    };
    assert_eq!(player, P0, "the Halo controller names the card (CR 614.1c)");
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

    // 3. P0 casts Clone; assert the copy prompt unconditionally; copy P1's
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

    // 4. Provision 2 +1/+1 counters on the battlefield copy through the
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

    // 5. The Clone-copy entered this turn; clear summoning sickness per repo
    // convention so the copied activated ability is activatable (CR 302.6).
    runner
        .state_mut()
        .objects
        .get_mut(&clone)
        .unwrap()
        .summoning_sick = false;

    // 6. P0 activates the copy's modal ability; assert the mode prompt
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

    // 7. Marath's cost carries X, so the modal funnel diverts into the
    // payment step: assert ChooseXValue unconditionally, then announce X=2
    // (X=0 deals 0 under both regimes and cannot discriminate).
    let WaitingFor::ChooseXValue { player, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "the X-cost activation must prompt for X, got {}",
            runner.waiting_for_kind()
        );
    };
    assert_eq!(
        player, P0,
        "CR 601.2b + CR 602.2b: the activator announces X"
    );
    runner
        .act(GameAction::ChooseX { value: 2 })
        .expect("announcing X=2 must succeed");

    // 8. Target P1's unprotected Grizzly Bears with the X=2 damage: P0 themself
    // is an ILLEGAL target (CR 702.16b — Halo protection was legally
    // established in step 2), which the rejection below pins, so only a
    // redirect can route the hit to P0 — no fizzle can zero the board.
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
    // Only the Marath-named Halo protects P0 from this ability; the control
    // row's "Grizzly Bears" Halo leaves P0 a legal target.
    if halo_name == "Marath, Will of the Wild" {
        assert!(
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(P0)),
                })
                .is_err(),
            "CR 702.16b: the Halo-protected P0 must be an illegal target"
        );
    }
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bears)),
        })
        .expect("targeting the unprotected Bears with the X damage must succeed");
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
        "the modal ability is on the stack before the Monolith activation"
    );

    // 9. P0 activates Jade Monolith while the copy is still alive: the hit's
    // original recipient is the Bears, and the to-be-departed copy is the
    // named source. The redirect resolves first, so the modal's damage —
    // whatever its source reads at resolution — reroutes to P0.
    runner
        .act(GameAction::ActivateAbility {
            source_id: monolith,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activating Jade Monolith must succeed");
    let WaitingFor::TargetSelection { player, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "the Monolith must prompt for its creature, got {}",
            runner.waiting_for_kind()
        );
    };
    assert_eq!(
        player, P0,
        "CR 601.2c: the Monolith controller chooses the creature"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bears)),
        })
        .expect("the Monolith must target the Bears");
    for _ in 0..16 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DamageSourceChoice { .. }
        ) {
            break;
        }
        assert!(
            !runner.state().stack.is_empty(),
            "the Monolith must reach its source choice"
        );
        runner
            .act(GameAction::PassPriority)
            .expect("pass toward the Monolith source choice");
    }
    let WaitingFor::DamageSourceChoice {
        player, options, ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the Monolith must prompt for its source, got {}",
            runner.waiting_for_kind()
        );
    };
    assert_eq!(player, P0, "P0 chooses the redirected source");
    assert!(
        options.contains(&clone),
        "the Clone-copy must be a choosable damage source, got {options:?}"
    );
    runner
        .act(GameAction::ChooseDamageSource { source: clone })
        .expect("naming the Clone-copy as the Monolith source must succeed");
    // Resolve the Monolith ability only: pass while two entries stand, so
    // the modal underneath survives for the Murder response.
    for _ in 0..8 {
        if runner.state().stack.len() == 1 {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass toward the Monolith resolution");
            }
            other => panic!("unexpected prompt while resolving Monolith: {other:?}"),
        }
    }
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the modal ability is on the stack before Murder"
    );

    // 10. P0 casts Murder targeting the Clone-copy in response; it resolves
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

    // 11. The modal ability resolves: the Monolith redirect reroutes the X=2
    // from the Bears to P0, and the Halo prevents it when the departed
    // source reads "Marath, Will of the Wild" (its pinned incarnation's exit
    // snapshot) but not when it reads live "Clone". Replacement
    // candidates are always exactly the applicable ones, so answering the
    // first each time cannot mis-order: prevention is inapplicable until
    // the redirect makes P0 the recipient.
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must succeed");
            }
            WaitingFor::ReplacementChoice { .. } => {
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("applying the next replacement must succeed");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("ordering triggers must succeed");
            }
            other => panic!("unexpected prompt while draining: {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "the stack is empty after resolution"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must complete at Priority, got {}",
        runner.waiting_for_kind()
    );
    // The redirect must have rerouted the hit: a Bears that absorbed the X=2
    // would be dead (2/2), leaving P0 at 20 without any prevention — the
    // vacuous (20,20) this row must exclude.
    assert!(
        runner.state().battlefield.contains(&bears),
        "the Bears must survive: the Monolith redirected the hit away"
    );
    assert_eq!(
        runner.state().objects[&bears].damage_marked,
        0,
        "the Bears must take no damage: the full X=2 rerouted to P0"
    );
    (
        runner.state().players[0].life,
        runner.state().players[1].life,
    )
}

/// CR 602.2b + CR 702.16e: with Halo naming "Marath, Will of the Wild"
/// (established through its real as-enters prompt before anything targeted),
/// the departed Clone-copy's modal damage — announced at the unprotected
/// Bears, redirected to P0 — is prevented via its exit-LKI name.
///
/// Production dependency: `deal_damage::damage_context_from_ability` hands
/// the ability's pinned source incarnation (`announced_source_incarnation`,
/// else `source_incarnation`) to `damage_context_for_subject`, which reads the
/// departed copy's exit snapshot for that exact incarnation (CR 608.2h +
/// CR 400.7). Passing `None` there instead makes this row fail with P0 at 18:
/// the source then reads as the card now in the graveyard ("Clone"), which
/// Halo's chosen name does not cover.
#[test]
fn clone_copy_marath_modal_redirect_prevents_via_exit_lki() {
    assert_eq!(
        run_clone_marath_modal_redirect("Marath, Will of the Wild"),
        (20, 20)
    );
}

/// Unprotected control: Halo naming "Grizzly Bears" cannot prevent the
/// Marath-sourced hit, so the redirected 2 lands on P0 — proving the
/// redirect path was live and prevention did the work in the main row.
#[test]
fn marath_modal_redirect_unprotected_control_takes_two() {
    assert_eq!(run_clone_marath_modal_redirect("Grizzly Bears"), (18, 20));
}
