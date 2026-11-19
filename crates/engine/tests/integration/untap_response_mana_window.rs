//! CR 701.26b + CR 605.3a: a player's own activated ability that will untap
//! its own untapped mana source holds their priority, because only tapped
//! permanents can be untapped and this is the window to tap it.
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
//! What keeps the pass: a stunned source (CR 122.1d), a phased-out source
//! (CR 702.26b), a source whose only mana ability removes it or was removed
//! from it (Humble), an untap of another permanent (Arbor Elf, Freed from the Real), an
//! ability that does more than untap (Eater of the Dead, Harsh Deceiver), and
//! a trigger (Wilderness Reclamation: "At the beginning of your end step, untap
//! all lands you control.", Hardbristle Bandit: "Whenever you commit a crime,
//! untap this creature."), which goes on the stack with no player act.

use super::support::shared_card_db;
use engine::ai_support::{
    auto_pass_recommended, auto_pass_recommended_for_viewer, legal_actions, legal_actions_full,
    AiDecisionContract,
};
use engine::analysis::decision_template::IterationCount;
use engine::analysis::loop_check::{ShortcutResponse, WinKind};
use engine::analysis::resource::ResourceAxis;
use engine::database::card_db::CardDatabase;
use engine::game::effects::attach::attach_to;
use engine::game::engine::apply_verified_ai_priority_pass;
use engine::game::layers::{evaluate_layers, flush_layers};
use engine::game::mana_abilities::is_mana_ability;
use engine::game::perf_counters;
use engine::game::period_confirm::performed_for_tests;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zones::move_to_zone;
use engine::types::ability::{Effect, TapStateChange, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::parse_counter_type;
use engine::types::game_state::{
    AutoPassMode, AutoPassRequest, CastPaymentMode, LoopDetectionMode, PriorityPassingMode,
    StackEntryKind, StackResolutionPolicy, TurnBoundary, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ARBOR_ELF: &str = "{T}: Untap target Forest.";
const WILDERNESS_RECLAMATION: &str =
    "At the beginning of your end step, untap all lands you control.";

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

/// Ashaya, Soul of the Wild, verbatim.
const ASHAYA: &str = "Ashaya's power and toughness are each equal to the number of lands you control.\nNontoken creatures you control are Forest lands in addition to their other types. (They're still affected by summoning sickness.)";

/// Manabarbs, verbatim.
const MANABARBS: &str =
    "Whenever a player taps a land for mana, this enchantment deals 1 damage to that player.";

/// Wild Growth, verbatim.
const WILD_GROWTH: &str =
    "Enchant land\nWhenever enchanted land is tapped for mana, its controller adds an additional {G}.";

/// Devoted Druid under Ashaya, its own untap on top, a Manabarbs on each side
/// when `manabarbs`, and Wild Growth on the Druid when `wild_growth`. Returns
/// whether the window holds.
fn ashaya_druid_untap_holds(manabarbs: bool, wild_growth: bool) -> bool {
    let mut scenario = main_phase_scenario();
    scenario.add_creature_from_oracle(P0, "Ashaya, Soul of the Wild", 0, 0, ASHAYA);
    let druid = scenario
        .add_creature_from_oracle(P0, "Devoted Druid", 0, 2, DEVOTED_DRUID)
        .id();
    if manabarbs {
        scenario.add_enchantment_from_oracle(P0, "Manabarbs", MANABARBS);
        scenario.add_enchantment_from_oracle(P1, "Manabarbs", MANABARBS);
    }
    let growth = wild_growth.then(|| {
        scenario
            .add_enchantment_from_oracle(P0, "Wild Growth", WILD_GROWTH)
            .id()
    });
    let mut runner = scenario.build();
    flush_layers(runner.state_mut());
    if let Some(growth) = growth {
        attach_to(runner.state_mut(), growth, druid);
        flush_layers(runner.state_mut());
    }
    clear_summoning_sickness(&mut runner, druid);
    assert!(
        runner.state().objects[&druid]
            .card_types
            .core_types
            .contains(&CoreType::Land),
        "Ashaya makes the Druid a land"
    );
    let untap = untap_ability_index(&runner, druid);
    activate(&mut runner, druid, untap, &[]);
    assert_eq!(top_source_name(&runner).as_deref(), Some("Devoted Druid"));
    assert!(mana_action(&runner, druid).is_some() && !is_tapped(&runner, druid));
    !recommends_pass(&runner)
}

/// Devoted Druid under Ashaya is a Forest. Its own untap holds with the board's
/// mana triggers around it: Wild Growth on the Druid, or a Manabarbs on each
/// side.
#[test]
fn a_land_source_holds_with_mana_triggers_around_it() {
    for (manabarbs, wild_growth) in [(false, false), (false, true), (true, false)] {
        assert!(
            ashaya_druid_untap_holds(manabarbs, wild_growth),
            "Manabarbs: {manabarbs}, Wild Growth: {wild_growth}"
        );
    }
}

/// The recommendation P0's client receives (`auto_pass_recommended_for_viewer`).
fn viewer_recommends_pass(runner: &GameRunner) -> bool {
    let state = runner.state();
    auto_pass_recommended_for_viewer(state, P0, &legal_actions(state))
}

/// CR 401.2: the recommendation P0's client receives is the same whatever the
/// hidden top card of P0's library is. Basalt Monolith's untap holds over a
/// land and over a creature. Harsh Deceiver's "{2}: Reveal the top card of your
/// library. If it's a land card, untap this creature" keeps the pass over
/// both, though only the land would untap it.
#[test]
fn the_hold_reads_no_hidden_card() {
    let Some(db) = shared_card_db() else { return };
    for top_card in ["Forest", "Llanowar Elves"] {
        let mut scenario = main_phase_scenario();
        let monolith = scenario.add_real_card(P0, "Basalt Monolith", Zone::Battlefield, db);
        scenario.add_real_card(P0, top_card, Zone::Library, db);
        let mountains: Vec<_> = (0..3)
            .map(|_| scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db))
            .collect();
        let mut runner = scenario.build();
        for mountain in mountains {
            tap_for_mana(&mut runner, mountain);
        }
        let untap = untap_ability_index(&runner, monolith);
        activate(&mut runner, monolith, untap, &[]);
        assert!(
            !viewer_recommends_pass(&runner),
            "Monolith over a {top_card}: the window holds"
        );

        let mut scenario = main_phase_scenario();
        scenario.add_enchantment_from_oracle(P0, "Cryptolith Rite", CRYPTOLITH_RITE);
        let deceiver = scenario
            .add_creature_from_oracle(P0, "Harsh Deceiver", 1, 4, HARSH_DECEIVER)
            .id();
        scenario.add_real_card(P0, top_card, Zone::Library, db);
        let islands: Vec<_> = (0..2)
            .map(|_| scenario.add_real_card(P0, "Island", Zone::Battlefield, db))
            .collect();
        let mut runner = scenario.build();
        clear_summoning_sickness(&mut runner, deceiver);
        for island in islands {
            tap_for_mana(&mut runner, island);
        }
        let reveal = runner.state().objects[&deceiver]
            .abilities
            .iter()
            .enumerate()
            .filter(|(_, ability)| !is_mana_ability(ability))
            .nth(1)
            .map(|(index, _)| index)
            .expect("the reveal ability");
        activate(&mut runner, deceiver, reveal, &[]);
        assert_eq!(top_source_name(&runner).as_deref(), Some("Harsh Deceiver"));
        assert!(
            viewer_recommends_pass(&runner),
            "Deceiver over a {top_card}: the pass stays"
        );
    }
}

/// Staff of Compleation, verbatim.
const STAFF_OF_COMPLEATION: &str = "{T}, Pay 1 life: Destroy target permanent you own.\n{T}, Pay 2 life: Add one mana of any color.\n{T}, Pay 3 life: Proliferate.\n{T}, Pay 4 life: Draw a card.\n{5}: Untap this artifact.";

/// CR 607.2a: Myr Welder activates its "{T}: Exile target artifact card from a
/// graveyard." on each of `cards` in a graveyard, so it has their activated
/// abilities ("This creature has all activated abilities of all cards exiled
/// with it."), and is readied to tap after each.
fn imprint_with_welder(runner: &mut GameRunner, welder: ObjectId, cards: &[ObjectId]) {
    clear_summoning_sickness(runner, welder);
    for &card in cards {
        let exile = runner.state().objects[&welder]
            .abilities
            .iter()
            .position(|ability| {
                matches!(
                    &*ability.effect,
                    Effect::ChangeZone {
                        destination: Zone::Exile,
                        ..
                    }
                )
            })
            .expect("Myr Welder has its printed exile ability");
        runner.activate(welder, exile).target_object(card).resolve();
        let state = runner.state();
        assert_eq!(state.objects[&card].zone, Zone::Exile, "reach: exiled");
        assert!(
            state
                .exile_links
                .iter()
                .any(|link| link.exiled_id == card && link.source_id == welder),
            "reach: exiled with the Welder"
        );
        runner.state_mut().objects.get_mut(&welder).unwrap().tapped = false;
    }
    evaluate_layers(runner.state_mut());
}

/// A costed source's untap on top: Staff of Compleation ("{T}, Pay 2 life: Add
/// one mana of any color.") with its own "{5}: Untap this artifact.", or Myr
/// Welder with Soliton's "{U}: Untap this creature." and Celestial Prism's
/// "{2}, {T}: Add one mana of any color." exiled with it, floating the Prism's
/// {2} beyond the untap's {U}.
fn costed_source_untap(db: &CardDatabase, prism: bool) -> GameRunner {
    let mut scenario = main_phase_scenario();
    let (source, exiled, lands) = if prism {
        let welder = scenario.add_real_card(P0, "Myr Welder", Zone::Battlefield, db);
        let exiled = vec![
            scenario.add_real_card(P0, "Soliton", Zone::Graveyard, db),
            scenario.add_real_card(P0, "Celestial Prism", Zone::Graveyard, db),
        ];
        let islands: Vec<_> = (0..3)
            .map(|_| scenario.add_real_card(P0, "Island", Zone::Battlefield, db))
            .collect();
        (welder, exiled, islands)
    } else {
        let staff = scenario
            .add_artifact_from_oracle(P0, "Staff of Compleation", STAFF_OF_COMPLEATION)
            .id();
        let mountains: Vec<_> = (0..5)
            .map(|_| scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db))
            .collect();
        (staff, Vec::new(), mountains)
    };
    let mut runner = scenario.build();
    if prism {
        imprint_with_welder(&mut runner, source, &exiled);
    }
    for land in lands {
        tap_for_mana(&mut runner, land);
    }
    let untap = untap_ability_index(&runner, source);
    activate(&mut runner, source, untap, &[]);
    assert!(mana_action(&runner, source).is_some() && !is_tapped(&runner, source));
    runner
}

/// The hold reads a costed source without running its mana ability or a
/// payment: the recommendation P0's client receives holds for the Staff's "Pay
/// 2 life" and for the Prism's {2}, neither of which it settles, taking no
/// legality, readiness or auto-payment copy of the game.
#[test]
fn a_costed_source_is_read_without_running_it() {
    let Some(db) = shared_card_db() else { return };
    for prism in [false, true] {
        let runner = costed_source_untap(db, prism);
        let state = runner.state();
        let actions = legal_actions(state);
        perf_counters::reset();
        assert!(
            !auto_pass_recommended_for_viewer(state, P0, &actions),
            "Prism: {prism}: the window holds"
        );
        let work = perf_counters::snapshot();
        assert_eq!(
            (
                work.state_clone_for_legality,
                work.grouped_mana_readiness_state_clones,
                work.strict_fast_path_mana_readiness_state_clones,
                work.auto_payment_owned_state_clones,
                work.priority_cast_probe_state_clones,
                work.auto_tap_source_cache_builds,
                work.post_apply_uncached_source_collections,
                work.mana_aura_trigger_scans,
            ),
            (0, 0, 0, 0, 0, 0, 0, 0),
            "Prism: {prism}: no copy of the game"
        );
        assert_eq!(
            perf_counters::bounded_reader_snapshot().state_clones,
            0,
            "Prism: {prism}: no auto-payment run"
        );
    }
}

/// The Immortal Sun, verbatim.
const THE_IMMORTAL_SUN: &str = "Players can't activate planeswalkers' loyalty abilities.\nAt the beginning of your draw step, draw an additional card.\nSpells you cast cost {1} less to cast.\nCreatures you control get +1/+1.";

/// Myr Welder with Soliton and Springleaf Drum exiled with it, Soliton's "{U}:
/// Untap this creature." on the stack. `crowded` adds The Immortal Sun and
/// thirty creatures. Returns whether the recommendation P0's client receives
/// holds, the probe counters and the engine's counters for that read, and the
/// probe counters for the live listing of the Welder's mana actions.
fn welder_drum_hold_work(
    db: &CardDatabase,
    crowded: bool,
) -> (
    bool,
    perf_counters::BoundedReaderCounters,
    perf_counters::PerfCounterSnapshot,
    perf_counters::BoundedReaderCounters,
) {
    let mut scenario = main_phase_scenario();
    let welder = scenario.add_real_card(P0, "Myr Welder", Zone::Battlefield, db);
    let exiled = [
        scenario.add_real_card(P0, "Soliton", Zone::Graveyard, db),
        scenario.add_real_card(P0, "Springleaf Drum", Zone::Graveyard, db),
    ];
    let island = scenario.add_real_card(P0, "Island", Zone::Battlefield, db);
    if crowded {
        scenario.add_artifact_from_oracle(P0, "The Immortal Sun", THE_IMMORTAL_SUN);
        for _ in 0..30 {
            scenario.add_creature(P0, "Bear", 2, 2);
        }
    }
    let mut runner = scenario.build();
    imprint_with_welder(&mut runner, welder, &exiled);
    tap_for_mana(&mut runner, island);
    let untap = untap_ability_index(&runner, welder);
    activate(&mut runner, welder, untap, &[]);
    assert_eq!(top_source_name(&runner).as_deref(), Some("Myr Welder"));
    let state = runner.state();
    let actions = legal_actions(state);
    perf_counters::reset();
    let holds = !auto_pass_recommended_for_viewer(state, P0, &actions);
    let hold_probe = perf_counters::bounded_reader_snapshot();
    let hold_work = perf_counters::snapshot();
    // The reach control: the live listing checks the borrowed Drum's cost.
    perf_counters::reset();
    let _ = mana_action(&runner, welder);
    let listing_probe = perf_counters::bounded_reader_snapshot();
    (holds, hold_probe, hold_work, listing_probe)
}

/// The hold reads only its source: `untap_would_repeat_mana` takes the source
/// object, not the game. Through the recommendation P0's client receives, Myr
/// Welder borrowing Springleaf Drum's "Tap an untapped creature you control"
/// holds, with The Immortal Sun's loyalty prohibition and thirty creatures on
/// the battlefield or without them. The read takes no auto-payment copy, visits
/// no object for the Drum's "tap an untapped creature" cost, and every engine
/// counter matches the bare board's. The live listing of the Welder's mana
/// actions does visit the battlefield for that cost, so the counter reaches it.
#[test]
fn the_hold_reads_only_its_source_on_a_crowded_board() {
    let Some(db) = shared_card_db() else { return };
    let (bare_holds, bare_probe, bare, _) = welder_drum_hold_work(db, false);
    let (crowded_holds, crowded_probe, crowded, crowded_listing) = welder_drum_hold_work(db, true);
    assert!(bare_holds && crowded_holds, "the window holds");
    assert_eq!(
        (
            bare_probe.state_clones,
            crowded_probe.state_clones,
            bare_probe.tap_creature_cost_visits,
            crowded_probe.tap_creature_cost_visits,
        ),
        (0, 0, 0, 0),
        "no auto-payment run and no creature-cost scan"
    );
    assert_eq!(crowded, bare, "the board adds no work to the read");
    assert!(
        crowded_listing.tap_creature_cost_visits > 30,
        "reach: the live listing walks the battlefield for the Drum's cost"
    );
}

/// CR 702.26b: Teferi's Protection ("All permanents you control phase out.")
/// resolves above Basalt Monolith's untap, with Monolith untapped. Monolith is
/// treated as though it doesn't exist, so it offers no mana action, and the
/// recommendation P0's client receives is to pass, with the untap still on the
/// stack.
#[test]
fn a_phased_out_source_keeps_the_pass() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = main_phase_scenario();
    let monolith = scenario.add_real_card(P0, "Basalt Monolith", Zone::Battlefield, db);
    let protection = scenario.add_real_card(P0, "Teferi's Protection", Zone::Hand, db);
    let mountains: Vec<_> = (0..3)
        .map(|_| scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db))
        .collect();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&protection)
        .expect("Teferi's Protection is in hand")
        .mana_cost = ManaCost::zero();
    for mountain in mountains {
        tap_for_mana(&mut runner, mountain);
    }
    let untap = untap_ability_index(&runner, monolith);
    activate(&mut runner, monolith, untap, &[]);
    assert!(mana_action(&runner, monolith).is_some());
    assert!(!viewer_recommends_pass(&runner), "the window holds first");

    runner.cast(protection).commit();
    for _ in 0..4 {
        if runner.state().stack.len() == 1 {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass");
    }
    assert_eq!(
        top_source_name(&runner).as_deref(),
        Some("Basalt Monolith"),
        "the untap is still on the stack"
    );
    assert!(runner.state().objects[&monolith].is_phased_out());
    assert!(mana_action(&runner, monolith).is_none());
    assert!(viewer_recommends_pass(&runner));
}

/// Guildless Commons, verbatim.
const GUILDLESS_COMMONS: &str = "This land enters tapped.\nWhen this land enters, return a land you control to its owner's hand.\n{T}: Add {C}{C}.";
/// Inspiration, verbatim.
const INSPIRATION: &str = "Target player draws two cards.";

/// The opponent's Resolve All asks only the untap question. P0 plays Guildless
/// Commons ("When this land enters, return a land you control to its owner's
/// hand.") beside an untapped Forest; P0's own recommendation over the trigger
/// is to hold, for the Forest, by the departure reason. P0 passes anyway, P1
/// casts Inspiration above the trigger and asks to resolve the stack, through
/// the public action (a session P0 isn't part of) or as a stored request with
/// no session. Another player's Resolve All doesn't read the departure reason:
/// the trigger resolves without stopping at P0's window.
#[test]
fn another_players_resolve_all_reads_no_departure_hold() {
    let Some(db) = shared_card_db() else { return };
    for stored in [false, true] {
        let mut scenario = main_phase_scenario();
        scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
        let commons = scenario
            .add_land_from_oracle(P0, "Guildless Commons", GUILDLESS_COMMONS)
            .id();
        let inspiration = scenario
            .add_spell_to_hand_from_oracle(P1, "Inspiration", true, INSPIRATION)
            .with_mana_cost(ManaCost::zero())
            .id();
        for _ in 0..2 {
            scenario.add_real_card(P1, "Island", Zone::Library, db);
        }
        let mut runner = scenario.build();
        move_to_zone(runner.state_mut(), commons, Zone::Hand, &mut Vec::new());
        let card_id = runner.state().objects[&commons].card_id;
        runner
            .act(GameAction::PlayLand {
                object_id: commons,
                card_id,
            })
            .expect("P0 plays Guildless Commons");
        assert_eq!(
            top_source_name(&runner).as_deref(),
            Some("Guildless Commons")
        );
        assert!(
            !recommends_pass(&runner),
            "reach: P0's own window over the trigger is held for the Forest"
        );
        runner.act(GameAction::PassPriority).expect("P0 passes");
        assert_eq!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P1 }
        );
        runner.cast(inspiration).target_player(P1).commit();
        assert_eq!(runner.state().stack.len(), 2);
        if stored {
            runner.state_mut().auto_pass.insert(
                P1,
                AutoPassMode::UntilStackEmpty {
                    initial_stack_len: 2,
                    policy: StackResolutionPolicy::Committed,
                },
            );
            runner.act(GameAction::PassPriority).expect("P1 passes");
        } else {
            runner
                .act(GameAction::SetAutoPass {
                    mode: AutoPassRequest::UntilStackEmpty,
                })
                .expect("P1 asks to resolve the stack");
        }

        assert!(
            !runner.state().stack.iter().any(|entry| runner
                .state()
                .objects
                .get(&entry.source_id)
                .is_some_and(|o| o.name == "Guildless Commons")),
            "stored: {stored}: the Commons trigger resolved under P1's Resolve All"
        );
    }
}

/// Humble, verbatim.
const HUMBLE: &str =
    "Until end of turn, target creature loses all abilities and has base power and toughness 0/1.";

/// CR 305.6 + CR 613.1f + CR 113.7a: Soliton is a Forest under Ashaya, so it
/// has the intrinsic "{T}: Add {G}", and its own "{U}: Untap this creature." on
/// the stack holds. Humble resolving above the untap removes that mana ability
/// in layer 6, while the untap on the stack survives: the source has no mana to
/// give again, and the pass stays. (The live listing still offers the {G} after
/// Humble through an older subtype fallback, filed separately.)
#[test]
fn an_ability_removed_by_a_layer_keeps_the_pass() {
    let Some(db) = shared_card_db() else { return };
    for humble in [false, true] {
        let mut scenario = main_phase_scenario();
        scenario.add_creature_from_oracle(P0, "Ashaya, Soul of the Wild", 0, 0, ASHAYA);
        let soliton = scenario.add_real_card(P0, "Soliton", Zone::Battlefield, db);
        let island = scenario.add_real_card(P0, "Island", Zone::Battlefield, db);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Humble", true, HUMBLE)
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = scenario.build();
        flush_layers(runner.state_mut());
        clear_summoning_sickness(&mut runner, soliton);
        tap_for_mana(&mut runner, island);
        let untap = untap_ability_index(&runner, soliton);
        activate(&mut runner, soliton, untap, &[]);
        assert!(
            mana_action(&runner, soliton).is_some(),
            "Soliton taps for {{G}}"
        );
        if humble {
            runner.cast(spell).target_object(soliton).commit();
            for _ in 0..4 {
                if runner.state().stack.len() == 1 {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass");
            }
            assert_eq!(top_source_name(&runner).as_deref(), Some("Soliton"));
            assert!(
                runner.state().objects[&soliton].abilities.is_empty(),
                "Humble removed Soliton's abilities, its intrinsic mana among them"
            );
        }
        assert_eq!(
            viewer_recommends_pass(&runner),
            humble,
            "Humble: {humble}: whether the untap window passes"
        );
    }
}

/// CR 701.21a: Myr Welder with Soliton and Lotus Petal exiled with it can tap
/// for mana by sacrificing itself, and with Mirrored Lotus by exiling itself,
/// which leaves nothing for Soliton's untap to untap, so the pass stays. With Sol Ring instead, the same untap holds. The
/// borrowed mana action is offered either way.
#[test]
fn a_mana_ability_that_consumes_its_source_keeps_the_pass() {
    let Some(db) = shared_card_db() else { return };
    for (name, holds) in [
        ("Lotus Petal", false),
        ("Mirrored Lotus", false),
        ("Sol Ring", true),
    ] {
        let mut scenario = main_phase_scenario();
        let welder = scenario.add_real_card(P0, "Myr Welder", Zone::Battlefield, db);
        let exiled = [
            scenario.add_real_card(P0, "Soliton", Zone::Graveyard, db),
            scenario.add_real_card(P0, name, Zone::Graveyard, db),
        ];
        let island = scenario.add_real_card(P0, "Island", Zone::Battlefield, db);
        let mut runner = scenario.build();
        imprint_with_welder(&mut runner, welder, &exiled);
        tap_for_mana(&mut runner, island);
        let untap = untap_ability_index(&runner, welder);
        activate(&mut runner, welder, untap, &[]);
        assert_eq!(top_source_name(&runner).as_deref(), Some("Myr Welder"));
        assert!(
            mana_action(&runner, welder).is_some(),
            "{name}: the borrowed mana action is offered"
        );
        assert_eq!(
            !viewer_recommends_pass(&runner),
            holds,
            "{name}: whether the untap window holds"
        );
    }
}

/// The engine's own auto-pass reads the same hold. P0 taps Staff of Compleation
/// ("{T}, Pay 2 life"), activates its "{5}: Untap this artifact." and copies it
/// with Rings of Brighthearth, then sets auto-pass to the end of the turn over
/// the copy, where the Staff is tapped. The copy untaps the Staff, and
/// auto-pass stops at the original's window.
#[test]
fn auto_pass_stops_for_a_costed_source() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = main_phase_scenario();
    let staff = scenario
        .add_artifact_from_oracle(P0, "Staff of Compleation", STAFF_OF_COMPLEATION)
        .id();
    scenario.add_real_card(P0, "Rings of Brighthearth", Zone::Battlefield, db);
    let mountains: Vec<_> = (0..6)
        .map(|_| scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db))
        .collect();
    let mut runner = scenario.build();
    for mountain in mountains {
        tap_for_mana(&mut runner, mountain);
    }
    tap_for_mana(&mut runner, staff);
    if !p0_has_priority(&runner) {
        let choose = legal_actions(runner.state())
            .into_iter()
            .next()
            .expect("the Staff's color choice offers an answer");
        runner.act(choose).expect("choose a color");
    }
    let untap = untap_ability_index(&runner, staff);
    activate(&mut runner, staff, untap, &[]);
    client_drive(&mut runner, Rings::Pay, original_and_copy);
    assert!(is_tapped(&runner, staff));

    runner
        .act(GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilTurnBoundary {
                until: TurnBoundary::EndOfCurrentTurn,
            },
        })
        .expect("P0 sets auto-pass");
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    );
    runner.act(GameAction::PassPriority).expect("P1 passes");
    assert!(p0_has_priority(&runner), "auto-pass stops at the original");
    assert_eq!(runner.state().stack.len(), 1);
    assert!(!is_tapped(&runner, staff), "the copy untapped the Staff");
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

/// Grim Monolith, verbatim.
const GRIM_MONOLITH: &str = "This artifact doesn't untap during your untap step.\n{T}: Add {C}{C}{C}.\n{4}: Untap this artifact.";

/// CR 117.3d: the opponent's Resolve All is their consent, not P0's. Grim
/// Monolith's untap and its Rings copy are on the stack, and the opponent asks
/// to resolve the stack. The copy untaps Monolith, and P0, who has no other
/// action ({4} is more than Monolith's {C}{C}{C}), still gets the window over
/// the original to tap it. Both routes: the request through the public action,
/// which installs a session P0 isn't part of, and a stored request with no
/// session, as a restored game carries it.
#[test]
fn another_players_resolve_all_leaves_the_untap_window_held() {
    let Some(db) = shared_card_db() else { return };
    for stored in [false, true] {
        let mut scenario = main_phase_scenario();
        let monolith = scenario
            .add_artifact_from_oracle(P0, "Grim Monolith", GRIM_MONOLITH)
            .id();
        scenario.add_real_card(P0, "Rings of Brighthearth", Zone::Battlefield, db);
        let mountains: Vec<_> = (0..3)
            .map(|_| scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db))
            .collect();
        let mut runner = scenario.build();
        tap_for_mana(&mut runner, monolith);
        tap_for_mana(&mut runner, mountains[0]);
        let untap = untap_ability_index(&runner, monolith);
        activate(&mut runner, monolith, untap, &[]);
        client_drive(&mut runner, Rings::Pay, original_and_copy);
        assert!(is_tapped(&runner, monolith));
        assert!(recommends_pass(&runner));

        if stored {
            runner.state_mut().auto_pass.insert(
                P1,
                AutoPassMode::UntilStackEmpty {
                    initial_stack_len: 2,
                    policy: StackResolutionPolicy::Committed,
                },
            );
            runner.act(GameAction::PassPriority).expect("P0 passes");
            assert!(runner.state().stack_resolution_session.is_none());
        } else {
            runner.act(GameAction::PassPriority).expect("P0 passes");
            assert_eq!(
                runner.state().waiting_for,
                WaitingFor::Priority { player: P1 }
            );
            runner
                .act(GameAction::SetAutoPass {
                    mode: AutoPassRequest::UntilStackEmpty,
                })
                .expect("the opponent asks to resolve the stack");
        }

        assert!(
            p0_has_priority(&runner),
            "stored: {stored}: P0 gets the window over the original"
        );
        assert_eq!(runner.state().stack.len(), 1, "stored: {stored}");
        assert!(!is_tapped(&runner, monolith), "the copy untapped Monolith");
        assert!(!recommends_pass(&runner));
    }
}

/// Eater of the Dead, verbatim.
const EATER_OF_THE_DEAD: &str = "{0}: If this creature is tapped, exile target creature card from a graveyard and untap this creature.";
/// Cryptolith Rite, verbatim.
const CRYPTOLITH_RITE: &str = "Creatures you control have \"{T}: Add one mana of any color.\"";

/// `source`'s one activated ability that isn't a mana ability: its printed
/// one, beside the mana ability Cryptolith Rite grants.
fn printed_ability_index(runner: &GameRunner, source: ObjectId) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| !is_mana_ability(ability))
        .expect("the source has its printed ability")
}

/// Harsh Deceiver, verbatim.
const HARSH_DECEIVER: &str = "{1}: Look at the top card of your library.\n{2}: Reveal the top card of your library. If it's a land card, untap this creature and it gets +1/+1 until end of turn. Activate only once each turn.";

/// Only an ability that does nothing but untap its own source holds. Each
/// creature here has Cryptolith Rite's "{T}: Add one mana of any color." and is
/// untapped with its ability on the stack. Eater of the Dead's "If this
/// creature is tapped, exile target creature card from a graveyard and untap
/// this creature." keeps the pass, though tapping it would meet the condition.
/// Harsh Deceiver's untap turns on the hidden top card of its library
/// (CR 401.2), and the recommendation is the same with a land or a nonland on
/// top.
#[test]
fn an_ability_that_does_more_than_untap_keeps_the_pass() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = main_phase_scenario();
    scenario.add_enchantment_from_oracle(P0, "Cryptolith Rite", CRYPTOLITH_RITE);
    let eater = scenario
        .add_creature_from_oracle(P0, "Eater of the Dead", 3, 4, EATER_OF_THE_DEAD)
        .id();
    let corpse = scenario.add_real_card(P1, "Llanowar Elves", Zone::Graveyard, db);
    let mut runner = scenario.build();
    clear_summoning_sickness(&mut runner, eater);
    let exile = printed_ability_index(&runner, eater);
    activate(&mut runner, eater, exile, &[corpse]);
    assert_eq!(
        top_source_name(&runner).as_deref(),
        Some("Eater of the Dead")
    );
    assert!(mana_action(&runner, eater).is_some() && !is_tapped(&runner, eater));
    assert!(recommends_pass(&runner), "Eater: its ability exiles too");

    for top_card in ["Forest", "Llanowar Elves"] {
        let mut scenario = main_phase_scenario();
        scenario.add_enchantment_from_oracle(P0, "Cryptolith Rite", CRYPTOLITH_RITE);
        let deceiver = scenario
            .add_creature_from_oracle(P0, "Harsh Deceiver", 1, 4, HARSH_DECEIVER)
            .id();
        scenario.add_real_card(P0, top_card, Zone::Library, db);
        let islands: Vec<_> = (0..2)
            .map(|_| scenario.add_real_card(P0, "Island", Zone::Battlefield, db))
            .collect();
        let mut runner = scenario.build();
        clear_summoning_sickness(&mut runner, deceiver);
        for island in islands {
            tap_for_mana(&mut runner, island);
        }
        // Its second printed ability, after "{1}: Look at the top card".
        let reveal = runner.state().objects[&deceiver]
            .abilities
            .iter()
            .enumerate()
            .filter(|(_, ability)| !is_mana_ability(ability))
            .nth(1)
            .map(|(index, _)| index)
            .expect("the reveal ability");
        activate(&mut runner, deceiver, reveal, &[]);
        assert_eq!(top_source_name(&runner).as_deref(), Some("Harsh Deceiver"));
        assert!(mana_action(&runner, deceiver).is_some() && !is_tapped(&runner, deceiver));
        assert!(
            recommends_pass(&runner),
            "Deceiver over a {top_card}: the hidden card doesn't decide"
        );
    }
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

/// CR 603.3 + CR 700.13: the same self-untap as an activated ability and as a
/// trigger, each over an untapped mana source. Basalt Monolith's "{3}: Untap
/// this artifact.", which P0 activated, holds P0's window. Hardbristle Bandit's
/// "Whenever you commit a crime, untap this creature." triggers when P0 casts
/// Inspiration ("Target player draws two cards.") targeting P1, and goes on the stack with no player act, so the
/// window passes, with the Bandit's "{T}: Add one mana of any color." offered.
#[test]
fn an_activated_self_untap_holds_and_a_triggered_one_passes() {
    let Some(db) = shared_card_db() else { return };
    let (monolith_runner, _) = untapped_monolith_with_its_untap_on_the_stack(db, false);
    let top = monolith_runner.state().stack.back().expect("the untap");
    assert!(matches!(top.kind, StackEntryKind::ActivatedAbility { .. }));
    assert!(
        !recommends_pass(&monolith_runner) && !viewer_recommends_pass(&monolith_runner),
        "Monolith's activated untap holds"
    );

    let mut scenario = main_phase_scenario();
    let bandit = scenario.add_real_card(P0, "Hardbristle Bandit", Zone::Battlefield, db);
    let inspiration = scenario.add_real_card(P0, "Inspiration", Zone::Hand, db);
    let islands: Vec<_> = (0..4)
        .map(|_| scenario.add_real_card(P0, "Island", Zone::Battlefield, db))
        .collect();
    for _ in 0..2 {
        scenario.add_real_card(P1, "Island", Zone::Library, db);
    }
    let mut runner = scenario.build();
    clear_summoning_sickness(&mut runner, bandit);
    // Inspiration's {3}{U} comes from the Islands, floated first.
    for island in islands {
        tap_for_mana(&mut runner, island);
    }
    runner.cast(inspiration).target_player(P1).commit();
    assert!(p0_has_priority(&runner));
    assert_eq!(
        top_source_name(&runner).as_deref(),
        Some("Hardbristle Bandit")
    );
    let top = runner.state().stack.back().expect("the trigger");
    assert!(matches!(top.kind, StackEntryKind::TriggeredAbility { .. }));
    assert!(mana_action(&runner, bandit).is_some() && !is_tapped(&runner, bandit));
    assert!(
        recommends_pass(&runner) && viewer_recommends_pass(&runner),
        "the Bandit's triggered untap keeps the pass"
    );
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
