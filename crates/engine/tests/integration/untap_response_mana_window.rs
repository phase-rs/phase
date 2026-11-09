//! CR 701.26b + CR 605.3a: a player's own activated ability that will untap
//! its own source while they can tap that source for mana now holds their
//! priority, because only tapped permanents can be untapped and this is the
//! window to tap it.
//!
//! The defining line, with Basalt Monolith ("{T}: Add {C}{C}{C}." and "{3}: Untap
//! this artifact.") and Rings of Brighthearth ("Whenever you activate an ability,
//! if it isn't a mana ability, you may pay {2}. If you do, copy that ability. You
//! may choose new targets for the copy."):
//!
//! 1. Tap Monolith for {C}{C}{C}, and pay {3} to activate its untap.
//! 2. Pay {2} for Rings. The copy goes on the stack above the original.
//! 3. The copy resolves and untaps Monolith.
//! 4. With the original still on the stack, tap Monolith again (CR 605.3a).
//! 5. The original resolves and untaps Monolith again.
//!
//! Every priority window of the acting player is driven the way the client
//! drives it: the window is passed for the player exactly when the engine's
//! `auto_pass_recommended` says so. Step 4 therefore fails if the engine
//! recommends passing the window, and the loop detector can't see a cycle the
//! player never got to play.
//!
//! The class around it: Grim Monolith ("{4}: Untap this artifact."), Devoted
//! Druid ("Put a -1/-1 counter on this creature: Untap this creature."), Staff
//! of Compleation ("{5}: Untap this artifact."), and Illusionist's Bracers
//! ("Whenever an ability of equipped creature is activated, if it isn't a mana
//! ability, copy that ability.") copying such an untap.
//!
//! What keeps the pass: a stunned source (CR 122.1d), a first use that ends the
//! game, an untap of another permanent (Arbor Elf, Freed from the Real), and
//! a trigger (Wilderness Reclamation: "At the beginning of your end step, untap
//! all lands you control."), which goes on the stack with no player act.

use super::support::shared_card_db;
use engine::ai_support::{
    auto_pass_recommended, legal_actions, legal_actions_full, AiDecisionContract,
};
use engine::analysis::decision_template::IterationCount;
use engine::analysis::loop_check::{ShortcutResponse, WinKind};
use engine::analysis::resource::ResourceAxis;
use engine::database::card_db::CardDatabase;
use engine::game::effects::attach::attach_to;
use engine::game::engine::apply_verified_ai_priority_pass;
use engine::game::layers::flush_layers;
use engine::game::mana_abilities::is_mana_ability;
use engine::game::perf_counters;
use engine::game::period_confirm::performed_for_tests;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{Effect, TapStateChange, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::parse_counter_type;
use engine::types::game_state::{
    CastPaymentMode, LoopDetectionMode, PriorityPassingMode, StackEntry, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ARBOR_ELF: &str = "{T}: Untap target Forest.";
const WILDERNESS_RECLAMATION: &str =
    "At the beginning of your end step, untap all lands you control.";
/// Treasure's mana ability, as the engine's token has it.
const TREASURE: &str = "{T}, Sacrifice this artifact: Add one mana of any color.";

fn recommends_pass(runner: &GameRunner) -> bool {
    let state = runner.state();
    auto_pass_recommended(state, &legal_actions(state))
}

fn p0_has_priority(runner: &GameRunner) -> bool {
    matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0)
}

fn top_source_name(runner: &GameRunner) -> Option<String> {
    let state = runner.state();
    let top = state.stack.back()?;
    state.objects.get(&top.source_id).map(|o| o.name.clone())
}

/// `source`'s mana action, from the list the client reads.
fn mana_action(runner: &GameRunner, source: ObjectId) -> Option<GameAction> {
    let state = runner.state();
    let (_, _, grouped) = legal_actions_full(state);
    grouped
        .get(&source)?
        .iter()
        .find(|action| match action {
            GameAction::TapLandForMana { .. } => true,
            GameAction::ActivateAbility {
                source_id,
                ability_index,
                ..
            } => is_mana_ability(&state.objects[source_id].abilities[*ability_index]),
            _ => false,
        })
        .cloned()
}

fn tap_for_mana(runner: &mut GameRunner, source: ObjectId) {
    let action = mana_action(runner, source).expect("the source's mana action is listed");
    runner.act(action).expect("tap for mana");
}

fn is_tapped(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].tapped
}

fn mana_in_pool(runner: &GameRunner, mana: ManaType) -> usize {
    runner.state().players[0].mana_pool.count_color(mana)
}

fn untap_ability_index(runner: &GameRunner, source: ObjectId) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| {
            matches!(
                &*ability.effect,
                Effect::SetTapState {
                    state: TapStateChange::Untap,
                    ..
                }
            )
        })
        .expect("the source has an untap ability")
}

/// Activates through the public actions: declares `targets` when asked, and
/// pays automatically.
fn activate(runner: &mut GameRunner, source: ObjectId, ability_index: usize, targets: &[ObjectId]) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate");
    for _ in 0..3 {
        let action = match &runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => GameAction::SelectTargets {
                targets: targets.iter().copied().map(TargetRef::Object).collect(),
            },
            WaitingFor::ManaPayment { .. } => GameAction::PassPriority,
            _ => return,
        };
        runner.act(action).expect("finish the activation");
    }
}

/// The player's answer to Rings of Brighthearth's "you may pay {2}".
#[derive(Clone, Copy)]
enum Rings {
    Pay,
    Decline,
    /// Bracers' copy is mandatory: asking is a failure.
    NotAsked,
}

/// The answer the drive gives to any prompt, or to a window that isn't P0's.
fn drive_action(runner: &GameRunner, rings: Rings) -> GameAction {
    match &runner.state().waiting_for {
        WaitingFor::Priority { .. } | WaitingFor::ManaPayment { .. } => GameAction::PassPriority,
        WaitingFor::OptionalEffectChoice { .. } => match rings {
            Rings::Pay => GameAction::DecideOptionalEffect { accept: true },
            Rings::Decline => GameAction::DecideOptionalEffect { accept: false },
            Rings::NotAsked => panic!("nothing here should be optional"),
        },
        WaitingFor::CopyRetarget { .. } => GameAction::KeepAllCopyTargets,
        other => panic!("unexpected {other:?}"),
    }
}

/// Plays on as the client does until `stop` says so: P0's windows pass exactly
/// when the engine recommends it, and a window it doesn't recommend passing,
/// short of `stop`, is a failure.
fn client_drive(runner: &mut GameRunner, rings: Rings, stop: fn(&GameRunner) -> bool) {
    for _ in 0..64 {
        if stop(runner) {
            return;
        }
        if p0_has_priority(runner) {
            assert!(
                recommends_pass(runner),
                "the client stopped short: {:?}",
                top_source_name(runner)
            );
        }
        let action = drive_action(runner, rings);
        runner.act(action).expect("drive");
    }
    panic!("the drive never reached its stop");
}

/// A window the client leaves to the player, or the loop offer.
fn left_to_player(runner: &GameRunner) -> bool {
    matches!(runner.state().waiting_for, WaitingFor::LoopShortcut { .. })
        || (p0_has_priority(runner)
            && (runner.state().stack.is_empty() || !recommends_pass(runner)))
}

/// P0's window with an activated ability and its copy on the stack.
fn original_and_copy(runner: &GameRunner) -> bool {
    let stack = &runner.state().stack;
    p0_has_priority(runner)
        && stack.len() == 2
        && stack
            .iter()
            .all(|entry| matches!(entry.kind, StackEntryKind::ActivatedAbility { .. }))
}

fn optional_effect_prompt(runner: &GameRunner) -> bool {
    matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    )
}

struct BasaltRings {
    runner: GameRunner,
    monolith: ObjectId,
    lands: Vec<ObjectId>,
    untap: usize,
}

fn basalt_rings(lands: usize, db: &CardDatabase) -> BasaltRings {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let monolith = scenario.add_real_card(P0, "Basalt Monolith", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Rings of Brighthearth", Zone::Battlefield, db);
    let lands = (0..lands)
        .map(|_| scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db))
        .collect();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    let untap = untap_ability_index(&runner, monolith);
    BasaltRings {
        runner,
        monolith,
        lands,
        untap,
    }
}

impl BasaltRings {
    fn monolith_tapped(&self) -> bool {
        is_tapped(&self.runner, self.monolith)
    }

    /// Taps Monolith for {C}{C}{C}, then pays {3} to activate its untap.
    fn tap_and_activate_untap(&mut self) {
        tap_for_mana(&mut self.runner, self.monolith);
        activate(&mut self.runner, self.monolith, self.untap, &[]);
    }

    /// Steps 1 to 5 of the line above, with every window passed as the client
    /// passes it. Stops at the offer or at the empty stack.
    fn play_one_cycle(&mut self) {
        self.tap_and_activate_untap();
        client_drive(&mut self.runner, Rings::Pay, original_and_copy);
        // Monolith is tapped, so there is nothing to tap yet.
        assert!(self.monolith_tapped());
        assert!(
            recommends_pass(&self.runner),
            "the untap isn't wasted while Monolith is tapped"
        );

        client_drive(&mut self.runner, Rings::Pay, left_to_player);
        assert_eq!(
            self.runner.state().stack.len(),
            1,
            "the client must stop with the original on the stack"
        );
        assert!(!self.monolith_tapped(), "the copy untapped Monolith");
        assert!(!recommends_pass(&self.runner));
        tap_for_mana(&mut self.runner, self.monolith);
        assert_eq!(mana_in_pool(&self.runner, ManaType::Colorless), 3);

        client_drive(&mut self.runner, Rings::Pay, left_to_player);
    }
}

fn main_phase_scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

fn clear_summoning_sickness(runner: &mut GameRunner, id: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&id)
        .expect("the object exists")
        .summoning_sick = false;
}

#[test]
fn the_monolith_loop_plays_by_hand_when_the_client_follows_the_recommendation() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = basalt_rings(2, db);
    rig.play_one_cycle();

    assert!(rig.runner.state().stack.is_empty());
    assert!(!rig.monolith_tapped(), "the original untapped Monolith");
    assert_eq!(mana_in_pool(&rig.runner, ManaType::Colorless), 3);
}

/// CR 732.2a: the optional {2}, the copy and the mid-stack mana spend all fit
/// the live detector, so one cycle played by hand is an offer.
#[test]
fn one_played_cycle_is_offered_as_a_mana_loop() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = basalt_rings(2, db);
    rig.play_one_cycle();

    match &rig.runner.state().waiting_for {
        WaitingFor::LoopShortcut {
            proposer,
            certificate,
            ..
        } => {
            assert_eq!(*proposer, P0);
            assert_eq!(
                certificate.unbounded,
                vec![ResourceAxis::Mana(ManaType::Colorless)]
            );
            assert_eq!(certificate.win_kind, Some(WinKind::Advantage));
        }
        other => panic!("expected the loop offer, got {other:?}"),
    }
    assert!(matches!(
        performed_for_tests(rig.runner.state()),
        Some(Ok(_))
    ));

    rig.runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(1),
            template: None,
        })
        .expect("declare the shortcut");
    rig.runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("the opponent accepts");
    assert!(rig
        .runner
        .state()
        .unbounded_resources
        .get(&P0)
        .is_some_and(|axes| axes
            .iter()
            .any(|axis| matches!(axis, ResourceAxis::Mana(_)))));
}

/// The route that already worked: Full Control refuses every pass, so the same
/// board reaches both windows and the offer without the hold.
#[test]
fn full_control_reaches_the_same_windows_and_the_offer() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = basalt_rings(2, db);
    rig.runner
        .state_mut()
        .priority_passing_modes
        .insert(P0, PriorityPassingMode::FullControl);
    rig.tap_and_activate_untap();

    let mut held_before_the_copy_resolved = false;
    let mut tapped_with_the_original_on_the_stack = false;
    for _ in 0..64 {
        if left_to_player(&rig.runner) && rig.runner.state().stack.is_empty() {
            break;
        }
        if p0_has_priority(&rig.runner) {
            assert!(!recommends_pass(&rig.runner));
            held_before_the_copy_resolved |=
                original_and_copy(&rig.runner) && rig.monolith_tapped();
            if !rig.monolith_tapped() {
                assert_eq!(rig.runner.state().stack.len(), 1);
                tap_for_mana(&mut rig.runner, rig.monolith);
                tapped_with_the_original_on_the_stack = true;
                continue;
            }
        }
        let action = drive_action(&rig.runner, Rings::Pay);
        rig.runner.act(action).expect("drive");
    }

    assert!(held_before_the_copy_resolved);
    assert!(tapped_with_the_original_on_the_stack);
    assert!(matches!(
        rig.runner.state().waiting_for,
        WaitingFor::LoopShortcut { .. }
    ));
}

/// Without the copy, Monolith stays tapped until the only untap resolves: one
/// untap per {3} and one tap per {C}{C}{C} is no progress, and nothing stops.
#[test]
fn declining_rings_passes_the_original_and_is_no_loop() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = basalt_rings(2, db);
    rig.tap_and_activate_untap();

    client_drive(&mut rig.runner, Rings::Decline, optional_effect_prompt);
    client_drive(&mut rig.runner, Rings::Decline, |runner| {
        !optional_effect_prompt(runner) && p0_has_priority(runner)
    });
    assert_eq!(rig.runner.state().stack.len(), 1);
    assert!(rig.monolith_tapped());
    assert!(recommends_pass(&rig.runner));

    client_drive(&mut rig.runner, Rings::Decline, left_to_player);
    assert!(rig.runner.state().stack.is_empty());
    assert!(p0_has_priority(&rig.runner), "no offer");
    assert!(!rig.monolith_tapped());
    assert_eq!(mana_in_pool(&rig.runner, ManaType::Colorless), 0);
}

/// With the cost floated, Monolith is untapped when activated, so the copy's
/// untap would be wasted too.
#[test]
fn both_windows_hold_when_the_source_is_untapped_at_activation() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = basalt_rings(5, db);
    for land in rig.lands.clone() {
        tap_for_mana(&mut rig.runner, land);
    }
    activate(&mut rig.runner, rig.monolith, rig.untap, &[]);

    client_drive(&mut rig.runner, Rings::Pay, original_and_copy);
    assert!(!rig.monolith_tapped());
    assert!(!recommends_pass(&rig.runner), "the copy's untap is wasted");
    tap_for_mana(&mut rig.runner, rig.monolith);
    assert!(
        recommends_pass(&rig.runner),
        "with Monolith tapped the copy can untap it"
    );

    client_drive(&mut rig.runner, Rings::Pay, left_to_player);
    assert_eq!(rig.runner.state().stack.len(), 1);
    assert!(!rig.monolith_tapped());
    assert!(!recommends_pass(&rig.runner), "the original's untap, too");
}

/// Basalt Monolith and three Mountains, Monolith untapped, its untap activated
/// with the Mountains' mana floated first, so Monolith can tap for mana in the
/// window.
/// `stun` puts a stun counter on Monolith first (Nesting Grounds can move one
/// there).
fn untapped_monolith_with_its_untap_on_the_stack(
    db: &CardDatabase,
    stun: bool,
) -> (GameRunner, ObjectId) {
    let mut scenario = main_phase_scenario();
    let monolith = scenario.add_real_card(P0, "Basalt Monolith", Zone::Battlefield, db);
    let mountains: Vec<_> = (0..3)
        .map(|_| scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db))
        .collect();
    let mut runner = scenario.build();
    for mountain in mountains {
        tap_for_mana(&mut runner, mountain);
    }
    if stun {
        runner
            .state_mut()
            .objects
            .get_mut(&monolith)
            .expect("Monolith is on the battlefield")
            .counters
            .insert(parse_counter_type("stun"), 1);
    }
    let untap = untap_ability_index(&runner, monolith);
    activate(&mut runner, monolith, untap, &[]);
    assert_eq!(top_source_name(&runner).as_deref(), Some("Basalt Monolith"));
    assert!(mana_action(&runner, monolith).is_some() && !is_tapped(&runner, monolith));
    (runner, monolith)
}

/// CR 122.1d: "If a permanent with a stun counter on it would become untapped,
/// instead remove a stun counter from it." Plain, tapping Monolith first gives
/// its mana twice, so the window holds. Stunned, the untap after that tap only
/// removes the counter, so there's no second tap and the pass stays; played out,
/// Monolith ends tapped with the counter gone.
#[test]
fn a_stunned_source_gets_no_second_tap_from_its_untap() {
    let Some(db) = shared_card_db() else { return };
    for stunned in [false, true] {
        let (mut runner, monolith) = untapped_monolith_with_its_untap_on_the_stack(db, stunned);
        assert_eq!(
            recommends_pass(&runner),
            stunned,
            "stunned: {stunned}: whether the untap window passes"
        );

        tap_for_mana(&mut runner, monolith);
        for _ in 0..4 {
            if runner.state().stack.is_empty() {
                break;
            }
            runner.act(GameAction::PassPriority).expect("pass");
        }
        let state = runner.state();
        assert!(state.stack.is_empty(), "Monolith's untap resolved");
        assert_eq!(
            state.objects[&monolith].tapped, stunned,
            "stunned: {stunned}: whether Monolith stays tapped"
        );
        assert!(
            !state.objects[&monolith]
                .counters
                .contains_key(&parse_counter_type("stun")),
            "the untap removed the stun counter instead"
        );
    }
}

/// Ghostly Flicker, verbatim.
const GHOSTLY_FLICKER: &str = "Exile two target artifacts, creatures, and/or lands you control, \
     then return those cards to the battlefield under your control.";

/// CR 400.7: once Monolith is flickered it is a new object, and the untap on
/// the stack no longer refers to it, so the pass stays. Played out, the untap
/// leaves the returned Monolith tapped. Without the flicker the same window
/// holds.
#[test]
fn a_flickered_source_is_not_the_untaps_object() {
    let Some(db) = shared_card_db() else { return };
    for flicker in [false, true] {
        let mut scenario = main_phase_scenario();
        let monolith = scenario.add_real_card(P0, "Basalt Monolith", Zone::Battlefield, db);
        let mountains: Vec<_> = (0..3)
            .map(|_| scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db))
            .collect();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Ghostly Flicker", true, GHOSTLY_FLICKER)
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = scenario.build();
        for &mountain in &mountains {
            tap_for_mana(&mut runner, mountain);
        }
        let untap = untap_ability_index(&runner, monolith);
        activate(&mut runner, monolith, untap, &[]);
        if flicker {
            let incarnation = runner.state().objects[&monolith].incarnation;
            runner
                .cast(spell)
                .target_objects(&[monolith, mountains[0]])
                .commit();
            for _ in 0..4 {
                if runner.state().stack.len() == 1 {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass");
            }
            assert_eq!(runner.state().stack.len(), 1, "Ghostly Flicker resolved");
            assert_ne!(
                runner.state().objects[&monolith].incarnation,
                incarnation,
                "Monolith came back as a new object"
            );
        }
        assert_eq!(top_source_name(&runner).as_deref(), Some("Basalt Monolith"));
        assert!(mana_action(&runner, monolith).is_some() && !is_tapped(&runner, monolith));
        assert_eq!(
            recommends_pass(&runner),
            flicker,
            "flickered: {flicker}: whether the untap window passes"
        );

        tap_for_mana(&mut runner, monolith);
        for _ in 0..4 {
            if runner.state().stack.is_empty() {
                break;
            }
            runner.act(GameAction::PassPriority).expect("pass");
        }
        assert!(runner.state().stack.is_empty(), "the untap resolved");
        assert_eq!(
            is_tapped(&runner, monolith),
            flicker,
            "flickered: {flicker}: whether Monolith stays tapped"
        );
    }
}

/// The hold reads the top entry and its source only: with forty more queued
/// untaps below it and sixty Treasures beside it, it copies the game once, for
/// Monolith's one {T} mana row.
#[test]
fn the_untap_hold_reads_only_the_top_and_its_source() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = main_phase_scenario();
    let monolith = scenario.add_real_card(P0, "Basalt Monolith", Zone::Battlefield, db);
    let mountains: Vec<_> = (0..3)
        .map(|_| scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db))
        .collect();
    for _ in 0..60 {
        scenario.add_artifact_from_oracle(P0, "Treasure", TREASURE);
    }
    let mut runner = scenario.build();
    for mountain in mountains {
        tap_for_mana(&mut runner, mountain);
    }
    let untap = untap_ability_index(&runner, monolith);
    activate(&mut runner, monolith, untap, &[]);
    let queued = runner.state().stack.back().expect("the untap").clone();
    for id in 0..40 {
        runner.state_mut().stack.push_front(StackEntry {
            id: ObjectId(90_000 + id),
            ..queued.clone()
        });
    }

    let state = runner.state();
    let actions = legal_actions(state);
    perf_counters::reset_untap_repeat_probe_counters();
    assert!(!auto_pass_recommended(state, &actions));
    assert_eq!(perf_counters::untap_repeat_probe_snapshot().state_clones, 1);
}

/// Staff of Compleation, verbatim.
const STAFF_OF_COMPLEATION: &str = "{T}, Pay 1 life: Destroy target permanent you own.\n{T}, Pay 2 life: Add one mana of any color.\n{T}, Pay 3 life: Proliferate.\n{T}, Pay 4 life: Draw a card.\n{5}: Untap this artifact.";

/// The Staff's "{T}, Pay 2 life: Add one mana of any color." CR 119.4 +
/// CR 704.5a: paying 2 life at 2 life is legal, and the player then loses the
/// game; with 0 life the cost can't be paid again either, so the untap has no
/// second use to wait for and the pass stays. At 3 life the first use leaves 1,
/// too little to pay again, so the pass stays too. At 4 it leaves 2, and the
/// window holds.
#[test]
fn a_use_that_ends_the_game_leaves_no_second_one() {
    let Some(db) = shared_card_db() else { return };
    for (life, holds) in [(2, false), (3, false), (4, true)] {
        let mut scenario = main_phase_scenario();
        let staff = scenario
            .add_artifact_from_oracle(P0, "Staff of Compleation", STAFF_OF_COMPLEATION)
            .id();
        let mountains: Vec<_> = (0..5)
            .map(|_| scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db))
            .collect();
        let mut runner = scenario.build();
        runner.state_mut().players[0].life = life;
        for mountain in mountains {
            tap_for_mana(&mut runner, mountain);
        }
        let untap = untap_ability_index(&runner, staff);
        activate(&mut runner, staff, untap, &[]);
        assert_eq!(
            top_source_name(&runner).as_deref(),
            Some("Staff of Compleation")
        );
        assert!(
            mana_action(&runner, staff).is_some(),
            "{life} life: the Staff can tap for mana now"
        );
        assert_eq!(
            !recommends_pass(&runner),
            holds,
            "{life} life: whether the untap window holds"
        );
    }
}

/// Devoted Druid, verbatim.
const DEVOTED_DRUID: &str =
    "{T}: Add {G}.\nPut a -1/-1 counter on this creature: Untap this creature.";

/// CR 707.10: Illusionist's Bracers on Devoted Druid. The copy is mandatory, so
/// there is no prompt; it untaps the Druid, the Druid taps again, and the
/// original untaps it.
#[test]
fn illusionists_bracers_on_devoted_druid_gives_the_druid_a_second_tap() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = main_phase_scenario();
    let druid = scenario
        .add_creature_from_oracle(P0, "Devoted Druid", 0, 2, DEVOTED_DRUID)
        .id();
    let bracers = scenario.add_real_card(P0, "Illusionist's Bracers", Zone::Battlefield, db);
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), bracers, druid);
    flush_layers(runner.state_mut());
    clear_summoning_sickness(&mut runner, druid);

    tap_for_mana(&mut runner, druid);
    let untap = untap_ability_index(&runner, druid);
    activate(&mut runner, druid, untap, &[]);

    client_drive(&mut runner, Rings::NotAsked, original_and_copy);
    assert!(is_tapped(&runner, druid));
    assert!(recommends_pass(&runner));

    client_drive(&mut runner, Rings::NotAsked, left_to_player);
    assert_eq!(runner.state().stack.len(), 1);
    assert!(!is_tapped(&runner, druid), "the copy untapped the Druid");
    assert!(!recommends_pass(&runner));
    tap_for_mana(&mut runner, druid);

    client_drive(&mut runner, Rings::NotAsked, left_to_player);
    assert!(runner.state().stack.is_empty());
    assert!(!is_tapped(&runner, druid), "the original untapped it");
    assert_eq!(mana_in_pool(&runner, ManaType::Green), 2);
}

/// The path a game against the AI takes: the AI's verified pass installs a
/// rechecking session. An activated entry resolves alone, so the look-ahead
/// authorizes one entry, and the original still reaches a window of its own
/// with Monolith untapped.
#[test]
fn an_ai_opponents_pass_leaves_the_original_its_window() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = basalt_rings(2, db);
    rig.tap_and_activate_untap();
    client_drive(&mut rig.runner, Rings::Pay, original_and_copy);
    assert!(rig.monolith_tapped());
    assert!(recommends_pass(&rig.runner));

    rig.runner.act(GameAction::PassPriority).expect("P0 passes");
    let state = rig.runner.state_mut();
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P1 });
    assert_eq!(state.stack.len(), 2);
    let contract = AiDecisionContract::issue(state, P1);
    apply_verified_ai_priority_pass(state, P1, &contract, GameAction::PassPriority)
        .expect("a verified AI priority pass is legal");

    assert!(p0_has_priority(&rig.runner));
    assert_eq!(rig.runner.state().stack.len(), 1);
    assert!(rig.runner.state().stack_resolution_session.is_some());
    assert!(!rig.monolith_tapped(), "the copy untapped Monolith");
    assert!(!recommends_pass(&rig.runner));
}

/// CR 603.2 + CR 603.3: a trigger goes on the stack with no player act, so
/// holding for it would stop every end step.
#[test]
fn wilderness_reclamation_keeps_the_pass() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Wilderness Reclamation", WILDERNESS_RECLAMATION);
    let forest = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let mut runner = scenario.build();
    for _ in 0..4 {
        if !runner.state().stack.is_empty() {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass");
    }

    assert!(p0_has_priority(&runner));
    assert_eq!(
        top_source_name(&runner).as_deref(),
        Some("Wilderness Reclamation")
    );
    assert!(mana_action(&runner, forest).is_some());
    let top = runner
        .state()
        .stack
        .back()
        .expect("the trigger is on the stack");
    assert!(matches!(top.kind, StackEntryKind::TriggeredAbility { .. }));
    assert!(recommends_pass(&runner));
}

/// Only an untap of the ability's own source holds. Arbor Elf's "{T}: Untap
/// target Forest." on an untapped Forest, and Freed from the Real's "{U}: Untap
/// enchanted creature." on untapped Llanowar Elves, keep the pass, as on main.
#[test]
fn an_untap_of_another_permanent_keeps_the_pass() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = main_phase_scenario();
    let elf = scenario
        .add_creature_from_oracle(P0, "Arbor Elf", 1, 1, ARBOR_ELF)
        .id();
    let forest = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let mut runner = scenario.build();
    clear_summoning_sickness(&mut runner, elf);
    let untap = untap_ability_index(&runner, elf);
    activate(&mut runner, elf, untap, &[forest]);
    assert_eq!(top_source_name(&runner).as_deref(), Some("Arbor Elf"));
    assert!(mana_action(&runner, forest).is_some() && !is_tapped(&runner, forest));
    assert!(
        recommends_pass(&runner),
        "Arbor Elf untaps another permanent"
    );

    let mut scenario = main_phase_scenario();
    let elves = scenario.add_real_card(P0, "Llanowar Elves", Zone::Battlefield, db);
    let aura = scenario.add_real_card(P0, "Freed from the Real", Zone::Battlefield, db);
    let island = scenario.add_real_card(P0, "Island", Zone::Battlefield, db);
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), aura, elves);
    flush_layers(runner.state_mut());
    clear_summoning_sickness(&mut runner, elves);
    tap_for_mana(&mut runner, island);
    let untap = untap_ability_index(&runner, aura);
    activate(&mut runner, aura, untap, &[]);
    assert_eq!(
        top_source_name(&runner).as_deref(),
        Some("Freed from the Real")
    );
    assert!(mana_action(&runner, elves).is_some() && !is_tapped(&runner, elves));
    assert!(recommends_pass(&runner), "the Aura untaps its host");
}
