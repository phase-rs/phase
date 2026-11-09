//! CR 701.26b + CR 605.3a: a player's own spell or activated ability that will
//! untap a permanent they can tap for mana now holds their priority, because
//! only tapped permanents can be untapped and this is the window to tap it.
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
//! The class around it:
//! - Arbor Elf: "{T}: Untap target Forest."
//! - Dramatic Reversal: "Untap all nonland permanents you control."
//! - Vitalize: "Untap all creatures you control."
//! - Freed from the Real and Pemmin's Aura: "{U}: Untap enchanted creature."
//! - Illusionist's Bracers: "Whenever an ability of equipped creature is
//!   activated, if it isn't a mana ability, copy that ability. You may choose new
//!   targets for the copy."
//!
//! What keeps the pass:
//! - Gilded Goose: "{T}, Sacrifice a Food: Add one mana of any color." asks which
//!   Food as its cost is paid, so a copy of the game can't read whether a second
//!   use exists. With one Food there is none.
//! - Krark-Clan Ironworks: "Sacrifice an artifact: Add {C}{C}." has no {T}.
//! - Lotus Petal: "{T}, Sacrifice this artifact: Add one mana of any color." is
//!   gone once it is tapped.
//! - Snap: "Return target creature to its owner's hand. Untap up to two lands."
//!   chooses its lands as it resolves.
//! - Acrobatic Leap: "Target creature gets +1/+3 and gains flying until end of
//!   turn. Untap it." sits below a pump, which the reach can't bound.
//! - Wilderness Reclamation: "At the beginning of your end step, untap all lands
//!   you control." is a trigger, which goes on the stack with no player act.

use super::support::shared_card_db;
use engine::ai_support::{
    auto_pass_recommended, legal_actions, legal_actions_full, AiDecisionContract,
};
use engine::analysis::decision_template::IterationCount;
use engine::analysis::loop_check::{ShortcutResponse, WinKind};
use engine::analysis::resource::ResourceAxis;
use engine::database::card_db::CardDatabase;
use engine::game::effects::attach::attach_to;
use engine::game::effects::stack_reach::stack_entry_node_reach;
use engine::game::engine::apply_verified_ai_priority_pass;
use engine::game::layers::flush_layers;
use engine::game::mana_abilities::is_mana_ability;
use engine::game::period_confirm::performed_for_tests;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{Effect, TapStateChange, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::parse_counter_type;
use engine::types::game_state::{
    CastPaymentMode, LoopDetectionMode, PriorityPassingMode, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ARBOR_ELF: &str = "{T}: Untap target Forest.";
const WILDERNESS_RECLAMATION: &str =
    "At the beginning of your end step, untap all lands you control.";
const SNAP: &str = "Return target creature to its owner's hand. Untap up to two lands.";
const ACROBATIC_LEAP: &str =
    "Target creature gets +1/+3 and gains flying until end of turn. Untap it.";
const FOOD: &str = "{2}, {T}, Sacrifice this artifact: You gain 3 life.";
const MIRRORED_LOTUS: &str = "Reflect {0} (As this enters, each opponent may pay {0}. When they \
     do, they create a token copy of this except it lacks this ability.)\n{T}, Exile Mirrored \
     Lotus: Add three mana of any one color.";

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

fn copy_retarget_prompt(runner: &GameRunner) -> bool {
    matches!(runner.state().waiting_for, WaitingFor::CopyRetarget { .. })
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

/// Arbor Elf, a tapped Forest and an untapped Forest.
fn arbor_elf_board(db: &CardDatabase) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = main_phase_scenario();
    let elf = scenario
        .add_creature_from_oracle(P0, "Arbor Elf", 1, 1, ARBOR_ELF)
        .id();
    let tapped = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let untapped = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let mut runner = scenario.build();
    clear_summoning_sickness(&mut runner, elf);
    tap_for_mana(&mut runner, tapped);
    (runner, elf, tapped, untapped)
}

/// The declared object decides, not a ready source elsewhere on the board.
#[test]
fn only_the_declared_target_of_an_untap_counts() {
    let Some(db) = shared_card_db() else { return };

    let (mut runner, elf, tapped, untapped) = arbor_elf_board(db);
    let untap = untap_ability_index(&runner, elf);
    activate(&mut runner, elf, untap, &[tapped]);
    assert_eq!(top_source_name(&runner).as_deref(), Some("Arbor Elf"));
    assert!(mana_action(&runner, untapped).is_some());
    assert!(
        recommends_pass(&runner),
        "the declared Forest is tapped, so the untap isn't wasted"
    );

    let (mut runner, elf, _, untapped) = arbor_elf_board(db);
    activate(&mut runner, elf, untap, &[untapped]);
    assert_eq!(top_source_name(&runner).as_deref(), Some("Arbor Elf"));
    assert!(
        !recommends_pass(&runner),
        "the declared Forest can tap for mana first"
    );
}

/// CR 122.1d: "If a permanent with a stun counter on it would become untapped,
/// instead remove a stun counter from it." Arbor Elf targets the untapped
/// Forest. Plain, tapping it first gives its mana twice, so the window holds.
/// Stunned, the untap after that tap only removes the counter, so there's no
/// second tap and the pass stays; played out, the Forest ends tapped.
#[test]
fn a_stunned_target_gets_no_second_tap_from_the_untap() {
    let Some(db) = shared_card_db() else { return };
    for stunned in [false, true] {
        let (mut runner, elf, _, forest) = arbor_elf_board(db);
        if stunned {
            runner
                .state_mut()
                .objects
                .get_mut(&forest)
                .expect("the Forest is on the battlefield")
                .counters
                .insert(parse_counter_type("stun"), 1);
        }
        let untap = untap_ability_index(&runner, elf);
        activate(&mut runner, elf, untap, &[forest]);
        assert_eq!(top_source_name(&runner).as_deref(), Some("Arbor Elf"));
        assert!(mana_action(&runner, forest).is_some() && !is_tapped(&runner, forest));
        assert_eq!(
            recommends_pass(&runner),
            stunned,
            "stunned: {stunned}: whether the untap window passes"
        );

        tap_for_mana(&mut runner, forest);
        for _ in 0..4 {
            if runner.state().stack.is_empty() {
                break;
            }
            runner.act(GameAction::PassPriority).expect("pass");
        }
        let state = runner.state();
        assert!(state.stack.is_empty(), "Arbor Elf's untap resolved");
        assert_eq!(
            state.objects[&forest].tapped, stunned,
            "stunned: {stunned}: whether the Forest stays tapped"
        );
        assert!(
            !state.objects[&forest]
                .counters
                .contains_key(&parse_counter_type("stun")),
            "the untap removed the stun counter instead"
        );
    }
}

/// Dramatic Reversal, its {1}{U} floated first so that automatic payment leaves
/// `permanents` untapped.
fn dramatic_reversal_on_the_stack(permanents: &[&str], db: &CardDatabase) -> GameRunner {
    dramatic_reversal_over(db, |scenario| {
        for permanent in permanents {
            scenario.add_real_card(P0, permanent, Zone::Battlefield, db);
        }
    })
}

fn dramatic_reversal_over(
    db: &CardDatabase,
    add_board: impl FnOnce(&mut GameScenario),
) -> GameRunner {
    let mut scenario = main_phase_scenario();
    let reversal = scenario.add_real_card(P0, "Dramatic Reversal", Zone::Hand, db);
    let islands = [
        scenario.add_real_card(P0, "Island", Zone::Battlefield, db),
        scenario.add_real_card(P0, "Island", Zone::Battlefield, db),
    ];
    add_board(&mut scenario);
    let mut runner = scenario.build();
    for island in islands {
        tap_for_mana(&mut runner, island);
    }
    runner.cast(reversal).commit();
    assert_eq!(
        top_source_name(&runner).as_deref(),
        Some("Dramatic Reversal")
    );
    runner
}

fn permanent_named(runner: &GameRunner, name: &str) -> ObjectId {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .copied()
        .find(|id| state.objects[id].name == name)
        .expect("the permanent is on the battlefield")
}

#[test]
fn a_mass_untap_holds_until_nothing_it_reaches_is_wasted() {
    let Some(db) = shared_card_db() else { return };
    let mut runner = dramatic_reversal_on_the_stack(&["Mind Stone", "Krark-Clan Ironworks"], db);
    let stone = permanent_named(&runner, "Mind Stone");
    assert!(!is_tapped(&runner, stone));
    assert!(!recommends_pass(&runner), "the Stone can tap first");

    tap_for_mana(&mut runner, stone);
    let ironworks = permanent_named(&runner, "Krark-Clan Ironworks");
    assert!(
        !is_tapped(&runner, ironworks),
        "Ironworks is in the mass filter and untapped"
    );
    assert!(
        recommends_pass(&runner),
        "Ironworks has no {{T}} in its cost, so untapping it gives nothing"
    );
}

#[test]
fn a_source_with_no_tap_cost_keeps_the_pass() {
    let Some(db) = shared_card_db() else { return };
    let runner = dramatic_reversal_on_the_stack(&["Krark-Clan Ironworks"], db);
    let ironworks = permanent_named(&runner, "Krark-Clan Ironworks");
    assert!(mana_action(&runner, ironworks).is_some());
    assert!(recommends_pass(&runner));
}

#[test]
fn a_source_that_sacrifices_itself_keeps_the_pass() {
    let Some(db) = shared_card_db() else { return };
    let runner = dramatic_reversal_on_the_stack(&["Lotus Petal"], db);
    let petal = permanent_named(&runner, "Lotus Petal");
    assert!(mana_action(&runner, petal).is_some());
    assert!(recommends_pass(&runner));
}

#[test]
fn a_source_that_exiles_itself_keeps_the_pass() {
    let Some(db) = shared_card_db() else { return };
    let runner = dramatic_reversal_over(db, |scenario| {
        scenario.add_artifact_from_oracle(P0, "Mirrored Lotus", MIRRORED_LOTUS);
    });
    let lotus = permanent_named(&runner, "Mirrored Lotus");
    assert!(mana_action(&runner, lotus).is_some());
    assert!(recommends_pass(&runner));
}

/// Garruk Wildspeaker, verbatim.
const GARRUK_WILDSPEAKER: &str = "[+1]: Untap two target lands.\n[\u{2212}1]: Create a 3/3 green Beast creature token.\n[\u{2212}4]: Creatures you control get +3/+3 and gain trample until end of turn.";
/// Gemstone Mine, verbatim.
const GEMSTONE_MINE: &str = "This land enters with three mining counters on it.\n{T}, Remove a mining counter from this land: Add one mana of any color. If there are no mining counters on this land, sacrifice it.";

/// Garruk Wildspeaker's +1 on the stack, targeting a tapped Forest and an
/// untapped Gemstone Mine with `mining_counters` on it. Returns the runner and
/// the Mine.
fn garruk_over_gemstone_mine(db: &CardDatabase, mining_counters: u32) -> (GameRunner, ObjectId) {
    let mut scenario = main_phase_scenario();
    let garruk = scenario
        .add_planeswalker_from_oracle(P0, "Garruk Wildspeaker", "Garruk", 3, GARRUK_WILDSPEAKER)
        .id();
    let forest = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let mine = scenario
        .add_land_from_oracle(P0, "Gemstone Mine", GEMSTONE_MINE)
        .id();
    // Scenario permanents skip the enters replacement, so the counters are set here.
    scenario.with_counter(mine, parse_counter_type("mining"), mining_counters);
    let mut runner = scenario.build();
    tap_for_mana(&mut runner, forest);
    activate(&mut runner, garruk, 0, &[forest, mine]);
    assert_eq!(
        top_source_name(&runner).as_deref(),
        Some("Garruk Wildspeaker"),
        "the +1 is on the stack"
    );
    (runner, mine)
}

/// Tapping Gemstone Mine removes a mining counter. On its last one the cost
/// can't be paid again, and the mana ability then sacrifices the Mine as it
/// resolves (CR 605.3b + CR 701.21a), so Garruk's untap has nothing to give it
/// and the pass stays. With two counters the Mine keeps one, and untapped it can
/// be used again, so the window holds.
#[test]
fn an_untap_holds_only_for_a_source_that_survives_its_use() {
    let Some(db) = shared_card_db() else { return };
    for (counters, holds) in [(1, false), (2, true)] {
        let (runner, mine) = garruk_over_gemstone_mine(db, counters);
        assert!(
            mana_action(&runner, mine).is_some() && !is_tapped(&runner, mine),
            "{counters} counters: the Mine can tap for mana now"
        );
        assert_eq!(
            !recommends_pass(&runner),
            holds,
            "{counters} counters: whether the untap window holds"
        );
    }
}

/// The two-counter row played out: the player taps the Mine in the held window,
/// and Garruk's untap then untaps it with a counter left to use.
#[test]
fn an_untap_gives_a_surviving_source_its_second_use() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, mine) = garruk_over_gemstone_mine(db, 2);
    assert!(!recommends_pass(&runner));
    tap_for_mana(&mut runner, mine);
    if !p0_has_priority(&runner) {
        let choose = legal_actions(runner.state())
            .into_iter()
            .next()
            .expect("the Mine's color choice offers an answer");
        runner.act(choose).expect("choose a color");
    }
    assert!(is_tapped(&runner, mine), "the Mine was tapped for mana");
    for _ in 0..4 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass");
    }
    let state = runner.state();
    assert!(state.stack.is_empty(), "Garruk's +1 resolved");
    assert_eq!(state.objects[&mine].zone, Zone::Battlefield);
    assert!(!state.objects[&mine].tapped, "the untap reached the Mine");
    assert_eq!(
        state.objects[&mine]
            .counters
            .get(&parse_counter_type("mining"))
            .copied(),
        Some(1),
        "one counter is left for the second use"
    );
}

/// Vitalize, its {G} floated first so that automatic payment leaves the board
/// untapped, over what `add_board` adds and a spare Forest, which can tap for
/// mana and is outside "creatures you control".
fn vitalize_on_the_stack(
    db: &CardDatabase,
    add_board: impl FnOnce(&mut GameScenario) -> Option<ObjectId>,
) -> (GameRunner, Option<ObjectId>, ObjectId) {
    let mut scenario = main_phase_scenario();
    let vitalize = scenario.add_real_card(P0, "Vitalize", Zone::Hand, db);
    let forest = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let spare = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let creature = add_board(&mut scenario);
    let mut runner = scenario.build();
    if let Some(creature) = creature {
        clear_summoning_sickness(&mut runner, creature);
    }
    tap_for_mana(&mut runner, forest);
    runner.cast(vitalize).commit();
    assert_eq!(top_source_name(&runner).as_deref(), Some("Vitalize"));
    (runner, creature, spare)
}

/// Gilded Goose's cost sacrifices a Food, not the Goose, but which Food is asked
/// as the cost is paid, before any of it is, so the repeat check can't read a
/// second use and the pass stays. With this one Food there is no second use.
#[test]
fn a_source_whose_cost_asks_a_choice_keeps_the_pass() {
    let Some(db) = shared_card_db() else { return };
    let (runner, goose, _) = vitalize_on_the_stack(db, |scenario| {
        let goose = scenario.add_real_card(P0, "Gilded Goose", Zone::Battlefield, db);
        scenario
            .add_artifact_from_oracle(P0, "Food", FOOD)
            .with_subtypes(vec!["Food"]);
        Some(goose)
    });
    let goose = goose.expect("the Goose was added");
    assert!(mana_action(&runner, goose).is_some());
    assert!(!is_tapped(&runner, goose));
    assert!(recommends_pass(&runner));
}

#[test]
fn a_mass_untap_reads_its_filter() {
    let Some(db) = shared_card_db() else { return };

    let (runner, _, spare) = vitalize_on_the_stack(db, |_| None);
    assert!(mana_action(&runner, spare).is_some());
    assert!(
        recommends_pass(&runner),
        "a Forest is outside \"creatures you control\""
    );

    let (runner, elves, spare) = vitalize_on_the_stack(db, |scenario| {
        Some(scenario.add_real_card(P0, "Llanowar Elves", Zone::Battlefield, db))
    });
    assert!(mana_action(&runner, spare).is_some());
    assert!(mana_action(&runner, elves.expect("the Elves were added")).is_some());
    assert!(!recommends_pass(&runner));
}

/// Preservation, decided by the node's place below `Bounce` rather than by its
/// binding: the untap keeps the pass, and the lands are chosen as it resolves.
#[test]
fn snap_keeps_the_pass_and_chooses_its_lands_as_it_resolves() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = main_phase_scenario();
    let snap = scenario
        .add_spell_to_hand_from_oracle(P0, "Snap", true, SNAP)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        })
        .id();
    let paying = [
        scenario.add_real_card(P0, "Island", Zone::Battlefield, db),
        scenario.add_real_card(P0, "Island", Zone::Battlefield, db),
    ];
    let third = scenario.add_real_card(P0, "Island", Zone::Battlefield, db);
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let mut runner = scenario.build();
    for island in paying {
        tap_for_mana(&mut runner, island);
    }
    runner.cast(snap).target_object(bears).commit();

    assert_eq!(top_source_name(&runner).as_deref(), Some("Snap"));
    assert!(mana_action(&runner, third).is_some());
    assert!(recommends_pass(&runner));

    client_drive(&mut runner, Rings::NotAsked, |runner| {
        runner.state().stack.is_empty()
    });
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::EffectZoneChoice { .. }
    ));
}

/// CR 707.10: Illusionist's Bracers on Arbor Elf. The copy is mandatory, so
/// there is no prompt; it untaps the Forest, the Forest taps again, and the
/// original untaps it.
#[test]
fn illusionists_bracers_on_arbor_elf_gives_the_forest_a_second_tap() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = main_phase_scenario();
    let elf = scenario
        .add_creature_from_oracle(P0, "Arbor Elf", 1, 1, ARBOR_ELF)
        .id();
    let bracers = scenario.add_real_card(P0, "Illusionist's Bracers", Zone::Battlefield, db);
    let forest = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), bracers, elf);
    flush_layers(runner.state_mut());
    clear_summoning_sickness(&mut runner, elf);

    tap_for_mana(&mut runner, forest);
    let untap = untap_ability_index(&runner, elf);
    activate(&mut runner, elf, untap, &[forest]);

    client_drive(&mut runner, Rings::NotAsked, copy_retarget_prompt);
    client_drive(&mut runner, Rings::NotAsked, original_and_copy);
    assert!(is_tapped(&runner, forest));
    assert!(recommends_pass(&runner));

    client_drive(&mut runner, Rings::NotAsked, left_to_player);
    assert_eq!(runner.state().stack.len(), 1);
    assert!(!is_tapped(&runner, forest), "the copy untapped the Forest");
    assert!(!recommends_pass(&runner));
    tap_for_mana(&mut runner, forest);

    client_drive(&mut runner, Rings::NotAsked, left_to_player);
    assert!(runner.state().stack.is_empty());
    assert!(!is_tapped(&runner, forest), "the original untapped it");
    assert_eq!(mana_in_pool(&runner, ManaType::Green), 2);
}

/// The path a game against the AI takes: the AI's verified pass installs a
/// rechecking session, whose look-ahead authorizes both entries with Monolith
/// tapped. An activated entry resolves alone, so the original still reaches a
/// window of its own with Monolith untapped.
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

    let state = runner.state();
    assert!(p0_has_priority(&runner));
    assert_eq!(
        top_source_name(&runner).as_deref(),
        Some("Wilderness Reclamation")
    );
    assert!(mana_action(&runner, forest).is_some());
    let top = state.stack.back().expect("the trigger is on the stack");
    assert!(matches!(top.kind, StackEntryKind::TriggeredAbility { .. }));
    assert!(
        stack_entry_node_reach(state, top)
            .iter()
            .any(|reach| reach.acted_on.contains(&TargetRef::Object(forest))),
        "the untap reaches the untapped Forest, so the kind decides"
    );
    assert!(recommends_pass(&runner));
}

/// CR 303.4b: "Untap enchanted creature." names the Aura's host, so the host's
/// mana ability is the one the untap would repeat.
#[test]
fn an_auras_untap_of_its_host_holds_while_the_host_can_tap_for_mana() {
    let Some(db) = shared_card_db() else { return };
    for aura in ["Freed from the Real", "Pemmin's Aura"] {
        for host_tapped in [false, true] {
            let mut scenario = main_phase_scenario();
            let elves = scenario.add_real_card(P0, "Llanowar Elves", Zone::Battlefield, db);
            let enchantment = scenario.add_real_card(P0, aura, Zone::Battlefield, db);
            let island = scenario.add_real_card(P0, "Island", Zone::Battlefield, db);
            let mut runner = scenario.build();
            attach_to(runner.state_mut(), enchantment, elves);
            flush_layers(runner.state_mut());
            clear_summoning_sickness(&mut runner, elves);
            runner.state_mut().objects.get_mut(&elves).unwrap().tapped = host_tapped;
            tap_for_mana(&mut runner, island);

            let untap = untap_ability_index(&runner, enchantment);
            activate(&mut runner, enchantment, untap, &[]);
            assert_eq!(top_source_name(&runner).as_deref(), Some(aura));

            assert_eq!(
                mana_action(&runner, elves).is_some(),
                !host_tapped,
                "{aura}: the host's mana action"
            );
            assert_eq!(recommends_pass(&runner), host_tapped, "{aura}");
        }
    }
}

/// Only the untap node's objects count: "Untap it." sits below a pump, so the
/// reach can't name it, and the pump's own answer isn't an untap.
#[test]
fn a_pump_above_an_untap_is_not_read_as_the_untap() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = main_phase_scenario();
    let leap = scenario
        .add_spell_to_hand_from_oracle(P0, "Acrobatic Leap", true, ACROBATIC_LEAP)
        .with_mana_cost(ManaCost::zero())
        .id();
    let elves = scenario.add_real_card(P0, "Llanowar Elves", Zone::Battlefield, db);
    let mut runner = scenario.build();
    clear_summoning_sickness(&mut runner, elves);
    runner.cast(leap).target_object(elves).commit();

    assert!(mana_action(&runner, elves).is_some());
    let state = runner.state();
    let top = state.stack.back().expect("the spell is on the stack");
    let reach = stack_entry_node_reach(state, top);
    assert_eq!(
        reach[0].acted_on,
        vec![TargetRef::Object(elves)],
        "the root node acts on the Elves, so the node filter is what refuses"
    );
    assert!(recommends_pass(&runner));
}
