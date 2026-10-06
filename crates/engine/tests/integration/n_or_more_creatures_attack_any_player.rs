//! Subject-led "Whenever one or more / two or more <subject> attack" triggers
//! watch the attacking players their subject names — not only the trigger's
//! controller.
//!
//! CR 508.1a + CR 805.10a: only creatures controlled by an attacking player are
//! declared as attackers. The attacking player is the active player
//! (CR 506.2), or under shared team turns every player on the active team,
//! which makes one combined attack (CR 506.2b + CR 805.10b). So the controller
//! scope written on the subject ("creatures", "creatures you control",
//! "creatures your opponents control") bounds the attacking players the
//! trigger watches. An unscoped "two or more <subject>" counts every attacking
//! creature of that class, read when attackers are declared and never
//! rechecked (CR 603.4 does not apply). CR 603.2: whenever a game event
//! matches the trigger event, the ability triggers. CR 603.2c: a batched "one
//! or more" trigger fires once per declaration.
//!
//! Cards (verbatim Oracle text) and rulings:
//! - Duelist's Heritage — "Whenever one or more creatures attack, you may have
//!   target attacking creature gain double strike until end of turn." Ruling:
//!   triggers whenever any player attacks with one or more creatures, not just
//!   when you do.
//! - Argent Dais — "Whenever two or more creatures attack, put an oil counter on
//!   this artifact." Ruling: triggers whenever two or more creatures attack, not
//!   just when you attack with two or more.
//! - Flummoxed Cyclops — "Whenever two or more creatures your opponents control
//!   attack, this creature can't block this combat." Rulings: in a game using
//!   the shared team turns option, it triggers if two or more players each
//!   attack with only one creature; once two or more creatures your opponents
//!   control have attacked, it can't block even if all but one of those
//!   attackers leave the battlefield before its triggered ability resolves.
//! - Aurelia, the Law Above — "Whenever a player attacks with three or more
//!   creatures, you draw a card." The per-player form names one specific
//!   attacking player (CR 508.3d + CR 805.10c).
//!
//! Before the fix the parser left the attacking-player gate at the
//! controller-scoped default for every subject without an attachment clause, so
//! the unscoped cards fired only on their controller's attacks and Flummoxed
//! Cyclops (opponent-scoped subject, controller-scoped gate) never fired.

use engine::game::combat::can_block_pair;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::format::FormatConfig;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const DUELISTS_HERITAGE: &str = "Whenever one or more creatures attack, you may have target attacking creature gain double strike until end of turn.";
const ARGENT_DAIS: &str = "This artifact enters with two oil counters on it.\nWhenever two or more creatures attack, put an oil counter on this artifact.\n{2}, {T}, Remove two oil counters from this artifact: Exile another target nonland permanent. Its controller draws two cards.";
const FLUMMOXED_CYCLOPS: &str = "Reach\nWhenever two or more creatures your opponents control attack, this creature can't block this combat.";
const CONTROL_SCOPED_TWO_OR_MORE: &str =
    "Whenever two or more creatures you control attack, draw a card.";
const AURELIA_THREE_OR_MORE: &str =
    "Whenever a player attacks with three or more creatures, you draw a card.";
const TWO_OR_MORE_DINOSAURS: &str = "Whenever two or more Dinosaurs attack, draw a card.";

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

/// Drive the declared-attackers trigger window until the stack is empty.
/// Answers trigger ordering with identity, trigger targeting with `target`, and
/// every optional-effect prompt with `accept`. Returns how many optional-effect
/// prompts were answered (a reach-guard for decline paths).
fn drive_attack_triggers(runner: &mut GameRunner, target: Option<ObjectId>, accept: bool) -> usize {
    let mut optional_prompts = 0;
    for _ in 0..60 {
        let action = match &runner.state().waiting_for {
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(
                        target.expect("a targeted trigger needs a chosen target"),
                    )),
                }
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                optional_prompts += 1;
                GameAction::DecideOptionalEffect { accept }
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return optional_prompts;
                }
                GameAction::PassPriority
            }
            other => panic!("unexpected window: {other:?}"),
        };
        runner.act(action).expect("drive attack-trigger window");
    }
    panic!("the stack did not empty within the window budget");
}

/// CR 506.2: hand the turn to `attacker` in a multiplayer game and pass priority
/// until the declare-attackers step (priority advances only after every player
/// passes, so `pass_both_players` is not enough with three players).
fn hand_turn_to(runner: &mut GameRunner, attacker: PlayerId) {
    runner.state_mut().active_player = attacker;
    runner.state_mut().priority_player = attacker;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: attacker };

    for _ in 0..16 {
        if runner.waiting_for_kind() == "DeclareAttackers" {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass should advance toward declare attackers");
    }
    panic!("expected DeclareAttackers");
}

/// Two-player hand-off: make `attacker` the active player and move from
/// precombat main to the declare-attackers step.
fn reach_declare_attackers(runner: &mut GameRunner, attacker: PlayerId) {
    runner.state_mut().active_player = attacker;
    runner.pass_both_players();
}

fn declare(runner: &mut GameRunner, attackers: &[ObjectId], defender: PlayerId) {
    let attacks: Vec<_> = attackers
        .iter()
        .map(|&id| (id, AttackTarget::Player(defender)))
        .collect();
    runner
        .declare_attackers(&attacks)
        .expect("attackers are declared");
}

/// Number of creatures declared as attackers this combat (reach-guard proving
/// the declaration event happened).
fn declared_attackers(runner: &GameRunner) -> usize {
    runner
        .state()
        .combat
        .as_ref()
        .map_or(0, |combat| combat.attackers.len())
}

fn has_double_strike(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].has_keyword(&Keyword::DoubleStrike)
}

fn oil_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Generic("oil".to_string()))
        .copied()
        .unwrap_or(0)
}

fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map_or(0, |p| p.hand.len())
}

/// CR 805.10a: Two-Headed Giant — teams {P0, P1} vs {P2, P3}.
fn two_headed_giant() -> GameScenario {
    GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42)
}

/// CR 805.10a + CR 805.10b: open the active team's one combined declaration
/// with P0 as the active player. Each creature in `valid_attacker_ids` may be
/// controlled by either player on the active team.
fn open_two_headed_giant_declaration(runner: &mut GameRunner, valid_attacker_ids: Vec<ObjectId>) {
    let state = runner.state_mut();
    state.active_player = P0;
    state.priority_player = P0;
    state.phase = Phase::DeclareAttackers;
    state.turn_number = 2;
    state.waiting_for = WaitingFor::DeclareAttackers {
        player: P0,
        valid_attacker_ids,
        valid_attack_targets: vec![AttackTarget::Player(P2), AttackTarget::Player(P3)],
        valid_attack_targets_by_attacker: None,
        attacker_constraints: Default::default(),
    };
}

// --- Duelist's Heritage (unscoped, one or more) ---

/// R1: an opponent attacks; P0's Duelist's Heritage triggers and grants double
/// strike to the opponent's attacker (any attacking creature is a legal target).
#[test]
fn duelists_heritage_fires_on_opponent_attack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Duelist's Heritage", DUELISTS_HERITAGE);
    let opp_attacker = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    reach_declare_attackers(&mut runner, P1);
    declare(&mut runner, &[opp_attacker], P0);
    assert!(!has_double_strike(&runner, opp_attacker));
    drive_attack_triggers(&mut runner, Some(opp_attacker), true);

    // Revert-failing: at BASE the controller-scoped gate rejects P1's attack.
    assert!(
        has_double_strike(&runner, opp_attacker),
        "Duelist's Heritage must trigger on an opponent's attack (CR 603.2)"
    );
}

/// R1-decline: the trigger reaches its optional prompt; declining grants nothing.
#[test]
fn duelists_heritage_decline_grants_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Duelist's Heritage", DUELISTS_HERITAGE);
    let opp_attacker = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    reach_declare_attackers(&mut runner, P1);
    declare(&mut runner, &[opp_attacker], P0);
    let prompts = drive_attack_triggers(&mut runner, Some(opp_attacker), false);

    // Reach-guard: the trigger fired and offered its optional effect.
    assert!(prompts >= 1, "the optional double-strike grant was offered");
    assert!(!has_double_strike(&runner, opp_attacker));
}

/// R1-own: the controller's own attack still triggers.
#[test]
fn duelists_heritage_fires_on_own_attack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Duelist's Heritage", DUELISTS_HERITAGE);
    let own_attacker = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    reach_declare_attackers(&mut runner, P0);
    declare(&mut runner, &[own_attacker], P1);
    drive_attack_triggers(&mut runner, Some(own_attacker), true);

    assert!(has_double_strike(&runner, own_attacker));
}

/// R1-3p: the attacking player (P1), the defending player (P2), and the source
/// controller (P0) are all distinct — the trigger still fires.
#[test]
fn duelists_heritage_fires_when_third_party_attacks() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Duelist's Heritage", DUELISTS_HERITAGE);
    let attacker = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    hand_turn_to(&mut runner, P1);
    declare(&mut runner, &[attacker], P2);
    drive_attack_triggers(&mut runner, Some(attacker), true);

    assert!(
        has_double_strike(&runner, attacker),
        "Duelist's Heritage must trigger when P1 attacks P2"
    );
}

// --- Argent Dais (unscoped, two or more) ---

/// R2: an opponent attacks with two creatures → one oil counter.
#[test]
fn argent_dais_counts_opponent_attack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dais = scenario
        .add_artifact_from_oracle(P0, "Argent Dais", ARGENT_DAIS)
        .id();
    let a = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let b = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    let mut runner = scenario.build();
    let before = oil_counters(&runner, dais);

    reach_declare_attackers(&mut runner, P1);
    declare(&mut runner, &[a, b], P0);
    drive_attack_triggers(&mut runner, None, true);

    // Revert-failing: at BASE the gate rejects P1's attack and the count reads P0.
    assert_eq!(oil_counters(&runner, dais), before + 1);
}

/// R2-one: a single opponent attacker does not meet "two or more".
#[test]
fn argent_dais_ignores_single_opponent_attacker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dais = scenario
        .add_artifact_from_oracle(P0, "Argent Dais", ARGENT_DAIS)
        .id();
    let a = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    let before = oil_counters(&runner, dais);

    reach_declare_attackers(&mut runner, P1);
    declare(&mut runner, &[a], P0);
    drive_attack_triggers(&mut runner, None, true);

    // Reach-guard: the declaration happened (paired positive: R2).
    assert_eq!(declared_attackers(&runner), 1);
    assert_eq!(oil_counters(&runner, dais), before);
}

/// R2-own: the controller's own two-creature attack still counts.
#[test]
fn argent_dais_counts_own_attack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dais = scenario
        .add_artifact_from_oracle(P0, "Argent Dais", ARGENT_DAIS)
        .id();
    let a = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let b = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();
    let mut runner = scenario.build();
    let before = oil_counters(&runner, dais);

    reach_declare_attackers(&mut runner, P0);
    declare(&mut runner, &[a, b], P1);
    drive_attack_triggers(&mut runner, None, true);

    assert_eq!(oil_counters(&runner, dais), before + 1);
}

// --- Event-time count (unscoped, two or more) ---

/// R10: the "two or more" count is part of the trigger event, read when
/// attackers are declared and never rechecked (CR 603.4 applies only to an
/// intervening "if"). An attacker leaving the battlefield before the trigger
/// resolves does not remove it (Flummoxed Cyclops ruling analog).
#[test]
fn argent_dais_count_is_not_rechecked_at_resolution() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dais = scenario
        .add_artifact_from_oracle(P0, "Argent Dais", ARGENT_DAIS)
        .id();
    let a = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let b = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    let mut runner = scenario.build();
    let before = oil_counters(&runner, dais);

    reach_declare_attackers(&mut runner, P1);
    declare(&mut runner, &[a, b], P0);

    // Reach-guard: the trigger fired and sits on the stack with NO
    // resolution-time condition — the count was event-time.
    let stacked = runner
        .state()
        .stack
        .iter()
        .find_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility { condition, .. } if entry.source_id == dais => {
                Some(condition.clone())
            }
            _ => None,
        })
        .expect("the Argent Dais attack trigger must be on the stack");
    assert!(
        stacked.is_none(),
        "the count must not be carried to the stack as a recheck, got {stacked:?}"
    );

    // One attacker leaves the battlefield before the trigger resolves, through
    // the replacement-aware production zone-change pipeline.
    let mut events = Vec::new();
    assert!(
        !move_object_for_test(
            runner.state_mut(),
            ZoneMoveRequest::effect(a, Zone::Graveyard, a),
            &mut events,
        ),
        "the response move must complete without a replacement choice"
    );
    assert_eq!(runner.state().objects[&a].zone, Zone::Graveyard);

    drive_attack_triggers(&mut runner, None, true);

    assert_eq!(
        oil_counters(&runner, dais),
        before + 1,
        "the oil counter still lands: the count was read at declaration"
    );
}

// --- Typed unscoped subject (two or more Dinosaurs) ---

/// R9: the unscoped count reads only ATTACKING objects of the subject CLASS.
/// One attacking Dinosaur plus an attacking non-Dinosaur, with a second
/// Dinosaur on the battlefield that does not attack, is not "two or more
/// Dinosaurs attack".
#[test]
fn typed_unscoped_count_ignores_bystanders_and_other_types() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Mountain"]);
    scenario.add_enchantment_from_oracle(P0, "Test Dinosaur Watcher", TWO_OR_MORE_DINOSAURS);
    let dino_a = scenario
        .add_creature(P1, "Raptor A", 2, 2)
        .with_subtypes(vec!["Dinosaur"])
        .id();
    let _dino_c = scenario
        .add_creature(P1, "Raptor C", 2, 2)
        .with_subtypes(vec!["Dinosaur"])
        .id();
    let bear_b = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    let before = hand_size(&runner, P0);

    reach_declare_attackers(&mut runner, P1);
    declare(&mut runner, &[dino_a, bear_b], P0);
    drive_attack_triggers(&mut runner, None, true);

    // Reach-guard: the two-creature declaration happened (paired positive: R9+).
    assert_eq!(declared_attackers(&runner), 2);
    assert_eq!(
        hand_size(&runner, P0),
        before,
        "one attacking Dinosaur (plus a non-attacking one and an attacking Bear) is not two"
    );
}

/// R9+: two attacking Dinosaurs (an opponent's) satisfy the typed unscoped count.
#[test]
fn typed_unscoped_count_fires_on_two_attacking_dinosaurs() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Mountain"]);
    scenario.add_enchantment_from_oracle(P0, "Test Dinosaur Watcher", TWO_OR_MORE_DINOSAURS);
    let dino_a = scenario
        .add_creature(P1, "Raptor A", 2, 2)
        .with_subtypes(vec!["Dinosaur"])
        .id();
    let dino_c = scenario
        .add_creature(P1, "Raptor C", 2, 2)
        .with_subtypes(vec!["Dinosaur"])
        .id();
    let _bear_b = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    let before = hand_size(&runner, P0);

    reach_declare_attackers(&mut runner, P1);
    declare(&mut runner, &[dino_a, dino_c], P0);
    drive_attack_triggers(&mut runner, None, true);

    assert_eq!(hand_size(&runner, P0), before + 1);
}

// --- Flummoxed Cyclops (opponent-scoped, two or more) ---

/// R3: two opponent attackers → Cyclops can't block this combat.
#[test]
fn flummoxed_cyclops_cant_block_after_two_opponent_attackers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cyclops = scenario
        .add_creature_from_oracle(P0, "Flummoxed Cyclops", 4, 4, FLUMMOXED_CYCLOPS)
        .id();
    let a = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let b = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    let mut runner = scenario.build();

    reach_declare_attackers(&mut runner, P1);
    declare(&mut runner, &[a, b], P0);
    drive_attack_triggers(&mut runner, None, true);

    // Revert-failing: at BASE the trigger never fires (gate scoped to P0, count
    // reads P0's attackers), so Cyclops could still block.
    assert!(
        !can_block_pair(runner.state(), cyclops, a),
        "Flummoxed Cyclops can't block after two opponent creatures attack"
    );
}

/// R3-one: a single opponent attacker does not meet "two or more" — Cyclops
/// can still block it (positive reach-guard on the same blocking check).
#[test]
fn flummoxed_cyclops_can_block_single_opponent_attacker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cyclops = scenario
        .add_creature_from_oracle(P0, "Flummoxed Cyclops", 4, 4, FLUMMOXED_CYCLOPS)
        .id();
    let a = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    reach_declare_attackers(&mut runner, P1);
    declare(&mut runner, &[a], P0);
    drive_attack_triggers(&mut runner, None, true);

    assert_eq!(declared_attackers(&runner), 1);
    assert!(can_block_pair(runner.state(), cyclops, a));
}

// --- "you control" subject (preservation) ---

/// R4: "creatures you control" stays controller-scoped — an opponent's
/// two-creature attack does not draw.
///
/// Sibling negative, NOT revert-failing: at BASE this also stays silent
/// (and `valid_card`'s "you control" would reject the opponent's attackers
/// regardless of the gate). The real "you control" regression guards are the
/// parser preservation tests and R4+.
#[test]
fn control_scoped_two_or_more_silent_on_opponent_attack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Mountain"]);
    scenario.add_creature_from_oracle(P0, "Test Warleader", 2, 2, CONTROL_SCOPED_TWO_OR_MORE);
    let a = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let b = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    let mut runner = scenario.build();
    let before = hand_size(&runner, P0);

    reach_declare_attackers(&mut runner, P1);
    declare(&mut runner, &[a, b], P0);
    drive_attack_triggers(&mut runner, None, true);

    // Reach-guard: the two-creature declaration happened (paired positive: R4+).
    assert_eq!(declared_attackers(&runner), 2);
    assert_eq!(hand_size(&runner, P0), before);
}

/// R4+: the controller's own two-creature attack draws a card.
#[test]
fn control_scoped_two_or_more_fires_on_own_attack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Mountain"]);
    let a = scenario
        .add_creature_from_oracle(P0, "Test Warleader", 2, 2, CONTROL_SCOPED_TWO_OR_MORE)
        .id();
    let b = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    let before = hand_size(&runner, P0);

    reach_declare_attackers(&mut runner, P0);
    declare(&mut runner, &[a, b], P1);
    drive_attack_triggers(&mut runner, None, true);

    assert_eq!(hand_size(&runner, P0), before + 1);
}

// --- Shared team turns (CR 805.10a/805.10b): teams {P0, P1} vs {P2, P3} ---

/// R5: under shared team turns the active team makes one combined attack.
/// P0 (active) declares one of its own creatures and one of its teammate P1's,
/// both attacking P2. Argent Dais (controlled by P2, on the defending team)
/// sees two creatures attack and gets an oil counter.
#[test]
fn argent_dais_counts_two_headed_giant_combined_attack() {
    let mut scenario = two_headed_giant();
    let dais = scenario
        .add_artifact_from_oracle(P2, "Argent Dais", ARGENT_DAIS)
        .id();
    let own = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let teammate = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    let mut runner = scenario.build();
    let before = oil_counters(&runner, dais);

    open_two_headed_giant_declaration(&mut runner, vec![own, teammate]);
    declare(&mut runner, &[own, teammate], P2);
    drive_attack_triggers(&mut runner, None, true);

    // Reach-guard: both team creatures attack in the combined declaration.
    assert_eq!(declared_attackers(&runner), 2);
    // Revert-failing: a count scoped to the first attacker's controller sees 1.
    assert_eq!(
        oil_counters(&runner, dais),
        before + 1,
        "two creatures of the combined attack satisfy \"two or more creatures attack\""
    );
}

/// R5-team: the same combined attack, with Argent Dais controlled by the
/// non-active attacking teammate P1.
#[test]
fn argent_dais_on_attacking_team_counts_teammates_attackers() {
    let mut scenario = two_headed_giant();
    let dais = scenario
        .add_artifact_from_oracle(P1, "Argent Dais", ARGENT_DAIS)
        .id();
    let own = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let teammate = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    let mut runner = scenario.build();
    let before = oil_counters(&runner, dais);

    open_two_headed_giant_declaration(&mut runner, vec![own, teammate]);
    declare(&mut runner, &[own, teammate], P2);
    drive_attack_triggers(&mut runner, None, true);

    assert_eq!(declared_attackers(&runner), 2);
    assert_eq!(oil_counters(&runner, dais), before + 1);
}

/// R5-one: only P0's creature is declared; P1's creature stays on the
/// battlefield undeclared. One creature attacking is not "two or more".
#[test]
fn argent_dais_two_headed_giant_single_attacker_no_counter() {
    let mut scenario = two_headed_giant();
    let dais = scenario
        .add_artifact_from_oracle(P2, "Argent Dais", ARGENT_DAIS)
        .id();
    let own = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let teammate = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    let mut runner = scenario.build();
    let before = oil_counters(&runner, dais);

    open_two_headed_giant_declaration(&mut runner, vec![own, teammate]);
    declare(&mut runner, &[own], P2);
    drive_attack_triggers(&mut runner, None, true);

    // Reach-guard: the declaration happened (paired positive: R5).
    assert_eq!(declared_attackers(&runner), 1);
    assert_eq!(oil_counters(&runner, dais), before);
}

/// R6: Flummoxed Cyclops ruling — under shared team turns it triggers if two
/// or more players each attack with only one creature.
#[test]
fn flummoxed_cyclops_two_headed_giant_one_attacker_each() {
    let mut scenario = two_headed_giant();
    let cyclops = scenario
        .add_creature_from_oracle(P2, "Flummoxed Cyclops", 4, 4, FLUMMOXED_CYCLOPS)
        .id();
    let a = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let b = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    let mut runner = scenario.build();

    open_two_headed_giant_declaration(&mut runner, vec![a, b]);
    declare(&mut runner, &[a, b], P2);
    drive_attack_triggers(&mut runner, None, true);

    assert_eq!(declared_attackers(&runner), 2);
    assert!(
        !can_block_pair(runner.state(), cyclops, a),
        "Flummoxed Cyclops can't block P0's attacker"
    );
    assert!(
        !can_block_pair(runner.state(), cyclops, b),
        "Flummoxed Cyclops can't block P1's attacker"
    );
}

/// R6-one: a single opposing attacker does not meet "two or more" — Cyclops
/// can still block it.
#[test]
fn flummoxed_cyclops_two_headed_giant_single_attacker_can_block() {
    let mut scenario = two_headed_giant();
    let cyclops = scenario
        .add_creature_from_oracle(P2, "Flummoxed Cyclops", 4, 4, FLUMMOXED_CYCLOPS)
        .id();
    let a = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let b = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    let mut runner = scenario.build();

    open_two_headed_giant_declaration(&mut runner, vec![a, b]);
    declare(&mut runner, &[a], P2);
    drive_attack_triggers(&mut runner, None, true);

    assert_eq!(declared_attackers(&runner), 1);
    assert!(can_block_pair(runner.state(), cyclops, a));
}

/// R7: the per-player sibling "a player attacks with three or more creatures"
/// names one specific attacking player (CR 508.3d + CR 805.10c). P0 attacks
/// with two creatures and P1 with one in the combined declaration: no single
/// player attacks with three, so Aurelia's controller draws nothing.
#[test]
fn per_player_attack_count_stays_per_player_in_two_headed_giant() {
    let mut scenario = two_headed_giant();
    scenario.with_library_top(P2, &["Mountain"]);
    scenario.add_creature_from_oracle(P2, "Test Aurelia Watcher", 2, 2, AURELIA_THREE_OR_MORE);
    let a = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let b = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();
    let c = scenario.add_creature(P1, "Balduvian Bears", 2, 2).id();
    let mut runner = scenario.build();
    let before = hand_size(&runner, P2);

    open_two_headed_giant_declaration(&mut runner, vec![a, b, c]);
    declare(&mut runner, &[a, b, c], P2);
    drive_attack_triggers(&mut runner, None, true);

    // Reach-guard: three creatures attack in the combined declaration
    // (paired positive: R7+).
    assert_eq!(declared_attackers(&runner), 3);
    assert_eq!(hand_size(&runner, P2), before);
}

/// R7+: one player (P0) attacks with three creatures — Aurelia's controller
/// draws a card.
#[test]
fn per_player_attack_count_fires_on_one_players_three_attackers() {
    let mut scenario = two_headed_giant();
    scenario.with_library_top(P2, &["Mountain"]);
    scenario.add_creature_from_oracle(P2, "Test Aurelia Watcher", 2, 2, AURELIA_THREE_OR_MORE);
    let a = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let b = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();
    let c = scenario.add_creature(P0, "Balduvian Bears", 2, 2).id();
    let mut runner = scenario.build();
    let before = hand_size(&runner, P2);

    open_two_headed_giant_declaration(&mut runner, vec![a, b, c]);
    declare(&mut runner, &[a, b, c], P2);
    drive_attack_triggers(&mut runner, None, true);

    assert_eq!(hand_size(&runner, P2), before + 1);
}
