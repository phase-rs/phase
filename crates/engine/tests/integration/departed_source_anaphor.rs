//! CR 400.7 + CR 608.2c: an untargeted "it" whose referent the engine infers
//! to be the ability's own source names that source only while the ability can
//! still find it. A source that changed zones before the instruction is a new
//! object, so "return it" / "exile it" does nothing to it (CR 400.7), and an
//! optional "you may … it" is not offered (CR 608.2d). Information readers
//! ("a copy of it") read the departed source's last known information instead
//! (CR 608.2h + CR 707.2).
//!
//! Oracle text below is verbatim from `client/public/card-data.json`.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::types::ability::{Effect, EffectKind};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const PERSISTENT_NIGHTMARE: &str = "Skulk (This creature can't be blocked by creatures with greater power.)\nWhen this creature deals combat damage to a player, return it to its owner's hand.";
const SAPRAZZAN_RAIDER: &str = "When this creature becomes blocked, return it to its owner's hand.";
const SLOGURK: &str = "Trample\nWhenever a land card is put into your graveyard from anywhere, put a +1/+1 counter on Slogurk.\nRemove three +1/+1 counters from Slogurk: Return it to its owner's hand.\nWhen Slogurk leaves the battlefield, return up to three target land cards from your graveyard to your hand.";
const GRAZILAXX: &str = "Whenever a creature you control becomes blocked, you may return it to its owner's hand.\nWhenever one or more creatures you control deal combat damage to a player, draw a card.";
const ACADEMY_RECTOR: &str = "When this creature dies, you may exile it. If you do, search your library for an enchantment card, put that card onto the battlefield, then shuffle.";
const NACATL_WAR_PRIDE: &str = "This creature must be blocked by exactly one creature if able.\nWhenever this creature attacks, create X tokens that are copies of it and that are tapped and attacking, where X is the number of creatures defending player controls. Exile the tokens at the beginning of the next end step.";
const ESOTERIC_DUPLICATOR: &str = "Whenever you sacrifice this artifact or another artifact, you may pay {2}. If you do, at the beginning of the next end step, create a token that's a copy of that artifact.\n{2}, Sacrifice this artifact: Draw a card.";
const OBZEDAT: &str = "When Obzedat enters, target opponent loses 2 life and you gain 2 life.\nAt the beginning of your end step, you may exile Obzedat. If you do, return it to the battlefield under its owner's control at the beginning of your next upkeep. It gains haste.";
const CHAMPIONS_FROM_BEYOND: &str = "When this enchantment enters, create X 1/1 colorless Hero creature tokens.\nLight Party — Whenever you attack with four or more creatures, scry 2, then draw a card.\nFull Party — Whenever you attack with eight or more creatures, those creatures get +4/+4 until end of turn.";
const WAKE_TO_SLAUGHTER: &str = "Choose up to two target creature cards in your graveyard. An opponent chooses one of them. Return that card to your hand. Return the other to the battlefield under your control. It gains haste. Exile it at the beginning of the next end step.\nFlashback {4}{B}{R} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";
const MURDER: &str = "Destroy target creature.";
const SHATTER: &str = "Destroy target artifact.";
const CLOUDSHIFT: &str =
    "Exile target creature you control, then return that card to the battlefield under your control.";
const COFFIN_PURGE: &str = "Exile target card from a graveyard.\nFlashback {B} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";
const IVORY_GARGOYLE: &str = "Flying\nWhen this creature dies, return it to the battlefield under its owner's control at the beginning of the next end step and you skip your next draw step.\n{4}{W}: Exile this creature.";

fn scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest"; 10]);
    scenario.with_library_top(P1, &["Forest"; 10]);
    scenario
}

/// How a "you may" prompt is answered (CR 608.2d).
#[derive(Clone, Copy)]
enum Optional {
    Accept,
    Decline,
}

/// Answer the one prompt in front of the game the way a player with no further
/// choices would: no attackers, no blockers, first legal target, keep scried
/// cards, pass priority. A "you may" prompt is answered by `optional` and
/// counted in `offered`.
fn step(runner: &mut GameRunner, optional: Optional, offered: &mut usize) {
    let action = match runner.state().waiting_for.clone() {
        WaitingFor::OrderTriggers { .. } => {
            engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            return;
        }
        WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
            runner
                .choose_first_legal_target()
                .expect("a legal target or an optional slot");
            return;
        }
        WaitingFor::OptionalEffectChoice { .. } => {
            *offered += 1;
            GameAction::DecideOptionalEffect {
                accept: matches!(optional, Optional::Accept),
            }
        }
        WaitingFor::ScryChoice { cards, .. } => GameAction::SelectCards { cards },
        WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        },
        WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
            assignments: vec![],
        },
        WaitingFor::Priority { .. } => GameAction::PassPriority,
        other => panic!("unexpected prompt {other:?}"),
    };
    runner.act(action).expect("the no-choice answer is legal");
}

fn pass_until_with(
    runner: &mut GameRunner,
    optional: Optional,
    offered: &mut usize,
    done: impl Fn(&GameRunner) -> bool,
) {
    for _ in 0..600 {
        if done(runner) {
            return;
        }
        step(runner, optional, offered);
    }
    panic!(
        "never reached the awaited state; turn {} phase {:?} waiting {:?}",
        runner.state().turn_number,
        runner.state().phase,
        runner.state().waiting_for
    );
}

fn pass_until(runner: &mut GameRunner, done: impl Fn(&GameRunner) -> bool) {
    let mut offered = 0;
    pass_until_with(runner, Optional::Decline, &mut offered, done);
    assert_eq!(offered, 0, "no optional prompt is expected while passing");
}

fn settled(runner: &GameRunner) -> bool {
    runner.state().stack.is_empty()
        && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
}

/// Resolve the whole stack, answering "you may" prompts with `optional`.
/// Returns how many optional prompts were offered.
fn resolve_stack(runner: &mut GameRunner, optional: Optional) -> usize {
    let mut offered = 0;
    pass_until_with(runner, optional, &mut offered, settled);
    offered
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn incarnation(runner: &GameRunner, id: ObjectId) -> u64 {
    runner.state().objects[&id].incarnation
}

fn attacking(runner: &GameRunner, id: ObjectId) -> bool {
    runner
        .state()
        .combat
        .as_ref()
        .is_some_and(|combat| combat.attackers.iter().any(|a| a.object_id == id))
}

/// A triggered ability from `source` is on the stack and a player has priority.
fn trigger_from_on_stack(runner: &GameRunner, source: ObjectId) -> bool {
    matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        && runner.state().stack.iter().any(|entry| {
            matches!(
                entry.kind,
                StackEntryKind::TriggeredAbility { source_id, .. } if source_id == source
            )
        })
}

fn add_spell(
    scenario: &mut GameScenario,
    name: &str,
    text: &str,
    shards: Vec<ManaCostShard>,
    generic: u32,
) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, name, true, text)
        .with_mana_cost(ManaCost::Cost { shards, generic })
        .id()
}

fn add_murder(scenario: &mut GameScenario) -> ObjectId {
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Black);
    }
    add_spell(
        scenario,
        "Murder",
        MURDER,
        vec![ManaCostShard::Black, ManaCostShard::Black],
        1,
    )
}

fn add_cloudshift(scenario: &mut GameScenario) -> ObjectId {
    scenario.add_basic_land(P0, ManaColor::White);
    add_spell(
        scenario,
        "Cloudshift",
        CLOUDSHIFT,
        vec![ManaCostShard::White],
        0,
    )
}

/// Cast `spell` at `target` and leave it on the stack above whatever is there.
fn cast_in_response(runner: &mut GameRunner, spell: ObjectId, target: ObjectId) {
    let _ = runner.cast(spell).target_object(target).commit();
    assert_eq!(
        zone(runner, spell),
        Zone::Stack,
        "reach: the response is cast"
    );
}

/// Declare `attackers` against P1 at P0's first declare-attackers step.
fn attack_with(runner: &mut GameRunner, attackers: &[ObjectId]) {
    pass_until(runner, |r| {
        matches!(r.state().waiting_for, WaitingFor::DeclareAttackers { .. })
    });
    let attacks: Vec<_> = attackers
        .iter()
        .map(|&id| (id, AttackTarget::Player(P1)))
        .collect();
    runner
        .declare_attackers(&attacks)
        .expect("the attackers can attack");
}

/// Attack with `attacker`, let P1 block it with `blocker`, and stop with the
/// becomes-blocked trigger of `source` on the stack.
fn attack_and_get_blocked(
    runner: &mut GameRunner,
    attacker: ObjectId,
    blocker: ObjectId,
    source: ObjectId,
) {
    attack_with(runner, &[attacker]);
    pass_until(runner, |r| {
        matches!(r.state().waiting_for, WaitingFor::DeclareBlockers { .. })
    });
    runner
        .declare_blockers(&[(blocker, attacker)])
        .expect("the blocker can block");
    pass_until(runner, |r| trigger_from_on_stack(r, source));
}

// --- Persistent Nightmare: combat-damage trigger, `use_self` (A3, A7) ---

fn nightmare_scenario() -> (GameScenario, ObjectId) {
    let mut scenario = scenario();
    let nightmare = scenario
        .add_creature_from_oracle(P0, "Persistent Nightmare", 1, 1, PERSISTENT_NIGHTMARE)
        .id();
    (scenario, nightmare)
}

fn nightmare_damage_trigger_on_stack(runner: &mut GameRunner, nightmare: ObjectId) {
    attack_with(runner, &[nightmare]);
    pass_until(runner, |r| trigger_from_on_stack(r, nightmare));
    assert_eq!(runner.state().players[1].life, 19, "reach: combat damage");
}

#[test]
fn persistent_nightmare_returns_to_hand_after_combat_damage() {
    let (scenario, nightmare) = nightmare_scenario();
    let mut runner = scenario.build();

    nightmare_damage_trigger_on_stack(&mut runner, nightmare);
    resolve_stack(&mut runner, Optional::Decline);
    assert_eq!(zone(&runner, nightmare), Zone::Hand);
}

#[test]
fn persistent_nightmare_destroyed_in_response_is_not_returned() {
    let (mut scenario, nightmare) = nightmare_scenario();
    let murder = add_murder(&mut scenario);
    let mut runner = scenario.build();

    nightmare_damage_trigger_on_stack(&mut runner, nightmare);
    cast_in_response(&mut runner, murder, nightmare);
    resolve_stack(&mut runner, Optional::Decline);
    // CR 400.7: the graveyard card is a new object "it" no longer names.
    assert_eq!(zone(&runner, nightmare), Zone::Graveyard);
}

#[test]
fn persistent_nightmare_blinked_in_response_stays_on_the_battlefield() {
    let (mut scenario, nightmare) = nightmare_scenario();
    let cloudshift = add_cloudshift(&mut scenario);
    let mut runner = scenario.build();

    nightmare_damage_trigger_on_stack(&mut runner, nightmare);
    let before = incarnation(&runner, nightmare);
    cast_in_response(&mut runner, cloudshift, nightmare);
    resolve_stack(&mut runner, Optional::Decline);
    assert_ne!(
        incarnation(&runner, nightmare),
        before,
        "reach: Cloudshift returned a new object"
    );
    // CR 400.7: a same-id return is a new object.
    assert_eq!(zone(&runner, nightmare), Zone::Battlefield);
}

// --- Academy Rector: TriggeringSource preservation (A4) ---

#[test]
fn academy_rector_exiled_in_response_searches_nothing() {
    let mut scenario = scenario();
    let rector = scenario
        .add_creature_from_oracle(P0, "Academy Rector", 1, 2, ACADEMY_RECTOR)
        .id();
    let enchantment = scenario
        .add_spell_to_library_top(P0, "Library Enchantment", false)
        .as_enchantment()
        .id();
    let murder = add_murder(&mut scenario);
    scenario.add_basic_land(P0, ManaColor::Black);
    let purge = add_spell(
        &mut scenario,
        "Coffin Purge",
        COFFIN_PURGE,
        vec![ManaCostShard::Black],
        0,
    );
    let mut runner = scenario.build();
    let library = runner.state().players[0].library.len();
    assert_eq!(zone(&runner, enchantment), Zone::Library, "reach");

    cast_in_response(&mut runner, murder, rector);
    pass_until(&mut runner, |r| trigger_from_on_stack(r, rector));
    assert_eq!(zone(&runner, rector), Zone::Graveyard, "reach: Rector died");
    cast_in_response(&mut runner, purge, rector);
    let offered = resolve_stack(&mut runner, Optional::Accept);

    assert_eq!(zone(&runner, rector), Zone::Exile, "exiled by Coffin Purge");
    assert_eq!(offered, 0, "CR 608.2d: the exile is impossible");
    assert_eq!(zone(&runner, enchantment), Zone::Library, "no search");
    assert_eq!(runner.state().players[0].library.len(), library);
}

// --- Saprazzan Raider: becomes-blocked hydration (A5) ---

fn raider_scenario() -> (GameScenario, ObjectId, ObjectId) {
    let mut scenario = scenario();
    let raider = scenario
        .add_creature_from_oracle(P0, "Saprazzan Raider", 1, 2, SAPRAZZAN_RAIDER)
        .id();
    let giant = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
    (scenario, raider, giant)
}

#[test]
fn saprazzan_raider_returns_to_hand_when_blocked() {
    let (scenario, raider, giant) = raider_scenario();
    let mut runner = scenario.build();

    attack_and_get_blocked(&mut runner, raider, giant, raider);
    resolve_stack(&mut runner, Optional::Decline);
    assert_eq!(zone(&runner, raider), Zone::Hand);
}

#[test]
fn saprazzan_raider_destroyed_in_response_stays_in_the_graveyard() {
    let (mut scenario, raider, giant) = raider_scenario();
    let murder = add_murder(&mut scenario);
    let mut runner = scenario.build();

    attack_and_get_blocked(&mut runner, raider, giant, raider);
    cast_in_response(&mut runner, murder, raider);
    resolve_stack(&mut runner, Optional::Decline);
    assert_eq!(zone(&runner, raider), Zone::Graveyard);
}

// --- Slogurk, the Overslime: activated ability (A6) ---

fn slogurk_scenario() -> (GameScenario, ObjectId) {
    let mut scenario = scenario();
    let slogurk = scenario
        .add_creature_from_oracle(P0, "Slogurk, the Overslime", 3, 3, SLOGURK)
        .with_plus_counters(3)
        .id();
    (scenario, slogurk)
}

fn activate_slogurk(runner: &mut GameRunner, slogurk: ObjectId) {
    let ability_index = runner.state().objects[&slogurk]
        .abilities
        .iter()
        .position(|a| a.cost.is_some())
        .expect("Slogurk's activated ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: slogurk,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the counters pay the cost");
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach: the ability is on the stack"
    );
}

#[test]
fn slogurk_returns_to_hand() {
    let (scenario, slogurk) = slogurk_scenario();
    let mut runner = scenario.build();

    activate_slogurk(&mut runner, slogurk);
    resolve_stack(&mut runner, Optional::Decline);
    assert_eq!(zone(&runner, slogurk), Zone::Hand);
}

#[test]
fn slogurk_blinked_in_response_stays_on_the_battlefield() {
    let (mut scenario, slogurk) = slogurk_scenario();
    let cloudshift = add_cloudshift(&mut scenario);
    let mut runner = scenario.build();

    activate_slogurk(&mut runner, slogurk);
    let before = incarnation(&runner, slogurk);
    cast_in_response(&mut runner, cloudshift, slogurk);
    resolve_stack(&mut runner, Optional::Decline);
    assert_ne!(incarnation(&runner, slogurk), before, "reach: blinked");
    // CR 400.7: the activated ability captured the incarnation it was activated from.
    assert_eq!(zone(&runner, slogurk), Zone::Battlefield);
}

// --- Conqueror's Galleon: delayed-trigger creation (A8) ---

/// The real Conqueror's Galleon (with its Conqueror's Foothold back face) and a
/// 4/4 to crew it, crewed and attacking with its attack trigger on the stack.
fn galleon_attacking(response: Option<&str>) -> Option<(GameRunner, ObjectId)> {
    let db = shared_card_db()?;
    let mut scenario = scenario();
    let galleon = scenario.add_real_card(P0, "Conqueror's Galleon", Zone::Battlefield, db);
    let crewer = scenario.add_creature(P0, "Crew Ogre", 4, 4).id();
    let shatter = response.map(|text| {
        scenario.add_basic_land(P0, ManaColor::Red);
        scenario.add_basic_land(P0, ManaColor::Red);
        add_spell(&mut scenario, "Shatter", text, vec![ManaCostShard::Red], 1)
    });
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    assert!(
        runner.state().objects[&galleon].back_face.is_some(),
        "reach: the real card carries its back face"
    );

    runner
        .act(GameAction::CrewVehicle {
            vehicle_id: galleon,
            creature_ids: vec![],
        })
        .expect("enter crew selection");
    runner
        .act(GameAction::CrewVehicle {
            vehicle_id: galleon,
            creature_ids: vec![crewer],
        })
        .expect("pay the crew cost");
    resolve_stack(&mut runner, Optional::Decline);

    attack_with(&mut runner, &[galleon]);
    pass_until(&mut runner, |r| trigger_from_on_stack(r, galleon));
    let delayed = runner.state().delayed_triggers.len();
    if let Some(shatter) = shatter {
        cast_in_response(&mut runner, shatter, galleon);
    }
    resolve_stack(&mut runner, Optional::Decline);
    assert_eq!(
        runner.state().delayed_triggers.len(),
        delayed + 1,
        "reach: the attack trigger created its delayed exile"
    );
    // CR 603.7c: the delayed exile fires at end of combat, whether or not "it"
    // can still be found then.
    pass_until(&mut runner, |r| trigger_from_on_stack(r, galleon));
    assert_eq!(
        runner.state().phase,
        Phase::EndCombat,
        "reach: the delayed exile fires at end of combat"
    );
    resolve_stack(&mut runner, Optional::Decline);
    assert_eq!(
        runner.state().delayed_triggers.len(),
        delayed,
        "reach: the delayed exile fired"
    );
    pass_until(&mut runner, |r| r.state().phase == Phase::PostCombatMain);
    Some((runner, galleon))
}

#[test]
fn conquerors_galleon_returns_transformed_at_end_of_combat() {
    let Some((runner, galleon)) = galleon_attacking(None) else {
        return;
    };
    let foothold = &runner.state().objects[&galleon];
    assert_eq!(foothold.zone, Zone::Battlefield);
    assert!(foothold.transformed, "it returns with its back face up");
    assert_eq!(foothold.name, "Conqueror's Foothold");
}

#[test]
fn conquerors_galleon_destroyed_in_response_is_not_returned() {
    let Some((runner, galleon)) = galleon_attacking(Some(SHATTER)) else {
        return;
    };
    // CR 400.7 + CR 603.7e: the delayed trigger's "it" was the Galleon, which
    // had already become a new object in the graveyard.
    let card = &runner.state().objects[&galleon];
    assert_eq!(card.zone, Zone::Graveyard);
    assert!(!card.transformed);
    assert!(
        !runner
            .state()
            .battlefield
            .iter()
            .any(|id| runner.state().objects[id].name == "Conqueror's Foothold"),
        "nothing returned transformed"
    );
}

// --- Nacatl War-Pride: copy reader preserved at LKI (A9) ---

fn war_pride_tokens(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            let object = &runner.state().objects[id];
            object.is_token && object.name == "Nacatl War-Pride"
        })
        .collect()
}

fn war_pride_attack(respond: bool) -> (GameRunner, ObjectId) {
    let mut scenario = scenario();
    let war_pride = scenario
        .add_creature_from_oracle(P0, "Nacatl War-Pride", 3, 3, NACATL_WAR_PRIDE)
        .id();
    scenario.add_creature(P1, "Hill Giant", 3, 3);
    let murder = add_murder(&mut scenario);
    let mut runner = scenario.build();

    attack_with(&mut runner, &[war_pride]);
    pass_until(&mut runner, |r| trigger_from_on_stack(r, war_pride));
    if respond {
        cast_in_response(&mut runner, murder, war_pride);
    }
    resolve_stack(&mut runner, Optional::Decline);
    (runner, war_pride)
}

fn assert_one_attacking_copy(runner: &GameRunner) {
    let tokens = war_pride_tokens(runner);
    assert_eq!(
        tokens.len(),
        1,
        "X = 1 creature the defending player controls"
    );
    let token = &runner.state().objects[&tokens[0]];
    assert!(token.tapped, "tapped");
    assert!(attacking(runner, tokens[0]), "attacking");
}

#[test]
fn nacatl_war_pride_makes_copies_when_it_attacks() {
    let (runner, war_pride) = war_pride_attack(false);
    assert_eq!(zone(&runner, war_pride), Zone::Battlefield);
    assert_one_attacking_copy(&runner);
}

#[test]
fn nacatl_war_pride_destroyed_in_response_still_makes_copies() {
    let (runner, war_pride) = war_pride_attack(true);
    assert_eq!(zone(&runner, war_pride), Zone::Graveyard, "reach: Murder");
    // CR 608.2h + CR 707.2 (ruling): the tokens still enter as copies of it.
    assert_one_attacking_copy(&runner);
}

// --- Jecht, Reluctant Guardian: optional move feasibility (A10) ---

/// The real Jecht, Reluctant Guardian (with its Braska's Final Aeon back face),
/// attacking unblocked with its combat-damage trigger on the stack, and Murder
/// cast on it in response when `respond`.
fn jecht_attacking(respond: bool) -> Option<(GameRunner, ObjectId)> {
    let db = shared_card_db()?;
    let mut scenario = scenario();
    let jecht = scenario.add_real_card(P0, "Jecht, Reluctant Guardian", Zone::Battlefield, db);
    let murder = add_murder(&mut scenario);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    assert!(
        runner.state().objects[&jecht].back_face.is_some(),
        "reach: the real card carries its back face"
    );

    attack_with(&mut runner, &[jecht]);
    pass_until(&mut runner, |r| trigger_from_on_stack(r, jecht));
    assert_eq!(runner.state().players[1].life, 16, "reach: combat damage");
    if respond {
        cast_in_response(&mut runner, murder, jecht);
    }
    Some((runner, jecht))
}

#[test]
fn jecht_exiles_and_returns_transformed() {
    let Some((mut runner, jecht)) = jecht_attacking(false) else {
        return;
    };
    let before = incarnation(&runner, jecht);
    let offered = resolve_stack(&mut runner, Optional::Accept);
    assert_eq!(offered, 1, "the optional exile is offered");
    assert_ne!(incarnation(&runner, jecht), before, "Jecht was exiled");
    // CR 400.7j: "return it" finds the card the exile moved, and it enters
    // transformed; as a Saga it enters with a lore counter (CR 714.3a).
    let aeon = &runner.state().objects[&jecht];
    assert_eq!(aeon.zone, Zone::Battlefield);
    assert!(aeon.transformed, "it returns with its back face up");
    assert_eq!(aeon.name, "Braska's Final Aeon");
    assert_eq!(aeon.counters.get(&CounterType::Lore).copied(), Some(1));
}

#[test]
fn jecht_declined_stays_on_the_battlefield() {
    let Some((mut runner, jecht)) = jecht_attacking(false) else {
        return;
    };
    let before = incarnation(&runner, jecht);
    let offered = resolve_stack(&mut runner, Optional::Decline);
    assert_eq!(offered, 1, "the optional exile is offered");
    assert_eq!(zone(&runner, jecht), Zone::Battlefield);
    assert_eq!(incarnation(&runner, jecht), before);
}

#[test]
fn jecht_destroyed_in_response_offers_no_exile() {
    let Some((mut runner, jecht)) = jecht_attacking(true) else {
        return;
    };
    let offered = resolve_stack(&mut runner, Optional::Accept);
    // CR 608.2d + CR 400.7: "you may exile it" is impossible, so it is not offered.
    assert_eq!(offered, 0);
    assert_eq!(zone(&runner, jecht), Zone::Graveyard);
}

// --- Grazilaxx, Illithid Scholar: hydration + optional Bounce feasibility (A11) ---

fn grazilaxx_blocked(respond: bool) -> (GameRunner, ObjectId) {
    let mut scenario = scenario();
    let grazilaxx = scenario
        .add_creature_from_oracle(P0, "Grazilaxx, Illithid Scholar", 3, 2, GRAZILAXX)
        .id();
    let giant = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
    let murder = add_murder(&mut scenario);
    let mut runner = scenario.build();

    attack_and_get_blocked(&mut runner, grazilaxx, giant, grazilaxx);
    if respond {
        cast_in_response(&mut runner, murder, grazilaxx);
    }
    (runner, grazilaxx)
}

#[test]
fn grazilaxx_blocked_returns_to_hand_when_accepted() {
    let (mut runner, grazilaxx) = grazilaxx_blocked(false);
    let offered = resolve_stack(&mut runner, Optional::Accept);
    assert_eq!(offered, 1, "the optional return is offered");
    assert_eq!(zone(&runner, grazilaxx), Zone::Hand);
}

#[test]
fn grazilaxx_destroyed_in_response_offers_no_return() {
    let (mut runner, grazilaxx) = grazilaxx_blocked(true);
    let offered = resolve_stack(&mut runner, Optional::Accept);
    assert_eq!(offered, 0, "CR 608.2d: the return is impossible");
    assert_eq!(zone(&runner, grazilaxx), Zone::Graveyard);
}

// --- Esoteric Duplicator: delayed copy of its own sacrificed source (A12) ---

#[test]
fn esoteric_duplicator_sacrificed_to_itself_copies_at_end_step() {
    let mut scenario = scenario();
    let duplicator = scenario
        .add_artifact_from_oracle(P0, "Esoteric Duplicator", ESOTERIC_DUPLICATOR)
        .id();
    for _ in 0..4 {
        scenario.add_basic_land(P0, ManaColor::Blue);
    }
    let mut runner = scenario.build();

    let ability_index = runner.state().objects[&duplicator]
        .abilities
        .iter()
        .position(|a| a.cost.is_some())
        .expect("the sacrifice ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: duplicator,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("{2} and the sacrifice are payable");
    assert_eq!(
        zone(&runner, duplicator),
        Zone::Graveyard,
        "reach: sacrificed"
    );
    let delayed = runner.state().delayed_triggers.len();
    let offered = resolve_stack(&mut runner, Optional::Accept);
    assert_eq!(offered, 1, "reach: the {{2}} payment was offered");
    assert_eq!(
        runner.state().delayed_triggers.len(),
        delayed + 1,
        "reach: the delayed copy was created"
    );

    pass_until(&mut runner, |r| r.state().phase == Phase::End);
    resolve_stack(&mut runner, Optional::Decline);
    let copies = runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            let object = &runner.state().objects[*id];
            object.is_token && object.name == "Esoteric Duplicator"
        })
        .count();
    // CR 608.2h + CR 707.2: the copy reads the artifact as it last existed.
    assert_eq!(copies, 1);
}

// --- Obzedat, Ghost Council: delayed own-source return (A13, A13h) ---

fn obzedat_exiled_at_end_step() -> (GameScenario, ObjectId) {
    let mut scenario = scenario();
    let obzedat = scenario
        .add_creature_from_oracle(P0, "Obzedat, Ghost Council", 5, 5, OBZEDAT)
        .id();
    (scenario, obzedat)
}

/// Pass to P0's end step and answer the "you may exile Obzedat" prompt.
fn obzedat_end_step(runner: &mut GameRunner, obzedat: ObjectId, optional: Optional) {
    let delayed = runner.state().delayed_triggers.len();
    pass_until(runner, |r| trigger_from_on_stack(r, obzedat));
    assert_eq!(runner.state().phase, Phase::End, "reach: P0's end step");
    let offered = resolve_stack(runner, optional);
    assert_eq!(offered, 1, "reach: the optional exile is offered");
    match optional {
        Optional::Accept => {
            assert_eq!(zone(runner, obzedat), Zone::Exile, "reach: exiled");
            assert_eq!(runner.state().delayed_triggers.len(), delayed + 1);
        }
        Optional::Decline => {
            assert_eq!(zone(runner, obzedat), Zone::Battlefield);
            assert_eq!(runner.state().delayed_triggers.len(), delayed);
        }
    }
}

/// Pass to P0's next upkeep and resolve what triggers there.
fn through_next_upkeep(runner: &mut GameRunner) {
    let turn = runner.state().turn_number;
    pass_until(runner, |r| {
        r.state().turn_number > turn + 1
            && r.state().active_player == P0
            && r.state().phase == Phase::Upkeep
    });
    resolve_stack(runner, Optional::Decline);
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner.state().players[player.0 as usize].life
}

#[test]
fn obzedat_exiles_itself_and_returns_at_the_next_upkeep() {
    let (scenario, obzedat) = obzedat_exiled_at_end_step();
    let mut runner = scenario.build();

    obzedat_end_step(&mut runner, obzedat, Optional::Accept);
    through_next_upkeep(&mut runner);
    // CR 400.7j + CR 603.7e: "it" is the card the exile moved.
    assert_eq!(zone(&runner, obzedat), Zone::Battlefield);
    assert_eq!(runner.state().objects[&obzedat].controller, P0);
    assert_eq!(life(&runner, P1), 18, "its enters trigger resolved");
}

#[test]
fn obzedat_declined_stays_on_the_battlefield() {
    let (scenario, obzedat) = obzedat_exiled_at_end_step();
    let mut runner = scenario.build();

    obzedat_end_step(&mut runner, obzedat, Optional::Decline);
}

#[test]
fn obzedat_moved_out_of_exile_and_back_does_not_return() {
    let (mut scenario, obzedat) = obzedat_exiled_at_end_step();
    scenario.add_basic_land(P0, ManaColor::Black);
    let purge = add_spell(
        &mut scenario,
        "Coffin Purge",
        COFFIN_PURGE,
        vec![ManaCostShard::Black],
        0,
    );
    let mut runner = scenario.build();

    obzedat_end_step(&mut runner, obzedat, Optional::Accept);
    let exiled = incarnation(&runner, obzedat);
    let delayed = runner.state().delayed_triggers.len();
    // Pull from Eternity's "target face-up exiled card" misparses (its target drops
    // the exile zone), so its move goes through the zone pipeline.
    let mut events = Vec::new();
    move_object_for_test(
        runner.state_mut(),
        ZoneMoveRequest::effect(obzedat, Zone::Graveyard, obzedat),
        &mut events,
    );
    assert_eq!(zone(&runner, obzedat), Zone::Graveyard);
    runner.cast(purge).target_object(obzedat).resolve();
    assert_eq!(zone(&runner, obzedat), Zone::Exile, "reach: re-exiled");
    assert_ne!(incarnation(&runner, obzedat), exiled, "reach: a new object");

    through_next_upkeep(&mut runner);
    assert_eq!(
        runner.state().delayed_triggers.len(),
        delayed - 1,
        "reach: the delayed return fired"
    );
    // CR 400.7 + CR 603.7c: the card in exile is not the one the ability exiled.
    assert_eq!(zone(&runner, obzedat), Zone::Exile);
    assert_eq!(life(&runner, P1), 20);
}

// --- Ivory Gargoyle: dies-trigger own-source referent that became a new object (A15) ---

#[test]
fn ivory_gargoyle_exiled_from_the_graveyard_in_response_does_not_return() {
    let mut scenario = scenario();
    let gargoyle = scenario
        .add_creature_from_oracle(P0, "Ivory Gargoyle", 2, 2, IVORY_GARGOYLE)
        .id();
    let murder = add_murder(&mut scenario);
    scenario.add_basic_land(P0, ManaColor::Black);
    let purge = add_spell(
        &mut scenario,
        "Coffin Purge",
        COFFIN_PURGE,
        vec![ManaCostShard::Black],
        0,
    );
    let mut runner = scenario.build();

    cast_in_response(&mut runner, murder, gargoyle);
    pass_until(&mut runner, |r| trigger_from_on_stack(r, gargoyle));
    assert_eq!(zone(&runner, gargoyle), Zone::Graveyard, "reach: it died");
    let delayed = runner.state().delayed_triggers.len();
    cast_in_response(&mut runner, purge, gargoyle);
    resolve_stack(&mut runner, Optional::Decline);
    assert_eq!(
        zone(&runner, gargoyle),
        Zone::Exile,
        "reach: exiled by Purge"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        delayed + 1,
        "reach: the dies trigger created its delayed return"
    );

    // CR 603.7c: the delayed return fires at the beginning of the next end
    // step, whether or not "it" can still be found then.
    pass_until(&mut runner, |r| trigger_from_on_stack(r, gargoyle));
    assert_eq!(
        runner.state().phase,
        Phase::End,
        "reach: the delayed return fires at the end step"
    );
    resolve_stack(&mut runner, Optional::Decline);
    assert_eq!(
        runner.state().delayed_triggers.len(),
        delayed,
        "reach: the delayed return fired"
    );
    // CR 400.7 + CR 603.7c: the exiled card is a new object, not the card the
    // dies trigger could find, so "return it" returns nothing.
    assert_eq!(zone(&runner, gargoyle), Zone::Exile);
}

// --- Champions from Beyond: attack-batch pin for non-source attackers (A14) ---

fn champions_full_party(blink_first_attacker: bool) -> (GameRunner, Vec<ObjectId>) {
    let mut scenario = scenario();
    scenario
        .add_creature(P0, "Champions from Beyond", 0, 0)
        .as_enchantment()
        .from_oracle_text(CHAMPIONS_FROM_BEYOND);
    let attackers: Vec<ObjectId> = (0..8).map(|_| scenario.add_vanilla(P0, 1, 1)).collect();
    let cloudshift = add_cloudshift(&mut scenario);
    let mut runner = scenario.build();

    attack_with(&mut runner, &attackers);
    pass_until(&mut runner, |r| {
        matches!(r.state().waiting_for, WaitingFor::Priority { .. })
            && r.state().stack.iter().any(|entry| {
                matches!(
                    &entry.kind,
                    StackEntryKind::TriggeredAbility { ability, .. }
                        if matches!(ability.effect, Effect::Pump { .. })
                )
            })
    });
    if blink_first_attacker {
        cast_in_response(&mut runner, cloudshift, attackers[0]);
    }
    resolve_stack(&mut runner, Optional::Decline);
    (runner, attackers)
}

fn power_toughness(runner: &GameRunner, id: ObjectId) -> (i32, i32) {
    let object = &runner.state().objects[&id];
    (object.power.unwrap_or(0), object.toughness.unwrap_or(0))
}

#[test]
fn champions_full_party_pumps_every_attacker_without_a_response() {
    let (runner, attackers) = champions_full_party(false);
    for attacker in attackers {
        assert_eq!(power_toughness(&runner, attacker), (5, 5));
    }
}

#[test]
fn champions_full_party_skips_an_attacker_blinked_in_response() {
    let (runner, attackers) = champions_full_party(true);
    // CR 400.7 + CR 506.4: the returned creature is a new object that is not
    // attacking, so it is not one of "those creatures".
    assert!(
        !attacking(&runner, attackers[0]),
        "reach: blinked out of combat"
    );
    assert_eq!(power_toughness(&runner, attackers[0]), (1, 1));
    for &attacker in &attackers[1..] {
        assert_eq!(power_toughness(&runner, attacker), (5, 5));
    }
}

// --- Wake to Slaughter: "return that card" after an empty choice (T1) ---

/// Cast Wake to Slaughter from hand with no targets (CR 115.6 allows zero for
/// "up to two") and resolve it. Returns the runner, the spell, a creature card
/// left in P0's graveyard, every card set the opponent was offered to choose
/// from, and the events emitted while resolving.
fn wake_to_slaughter_with_no_targets() -> (
    GameRunner,
    ObjectId,
    ObjectId,
    Vec<Vec<ObjectId>>,
    Vec<GameEvent>,
) {
    let mut scenario = scenario();
    scenario.add_basic_land(P0, ManaColor::Black);
    scenario.add_basic_land(P0, ManaColor::Red);
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Black);
    }
    let creature = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .id();
    let wake = add_spell(
        &mut scenario,
        "Wake to Slaughter",
        WAKE_TO_SLAUGHTER,
        vec![ManaCostShard::Black, ManaCostShard::Red],
        3,
    );
    let mut runner = scenario.build();
    // Reach: the opponent's choice has no earlier tracked set and no card in
    // P0's exile to fall back on, so with no targets nothing can be chosen.
    assert!(runner.state().exile.is_empty(), "reach: exile starts empty");
    assert!(
        runner.state().tracked_object_sets.is_empty(),
        "reach: no earlier tracked set"
    );

    let _ = runner.cast(wake).commit();
    assert_eq!(zone(&runner, wake), Zone::Stack, "reach: the spell is cast");
    assert!(
        runner.state().stack.iter().any(|entry| matches!(
            &entry.kind,
            StackEntryKind::Spell { ability: Some(ability), .. }
                if entry.id == wake && ability.targets.is_empty()
        )),
        "reach: the spell is cast with no targets"
    );

    let mut offered = Vec::new();
    let mut events = Vec::new();
    for _ in 0..64 {
        if settled(&runner) {
            return (runner, wake, creature, offered, events);
        }
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::ChooseFromZoneChoice {
                player,
                cards,
                count,
                ..
            } => {
                assert_eq!(player, P1, "an opponent chooses");
                offered.push(cards.clone());
                GameAction::SelectCards {
                    cards: cards.into_iter().take(count).collect(),
                }
            }
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected prompt {other:?}"),
        };
        events.extend(runner.act(action).expect("the answer is legal").events);
    }
    panic!("Wake to Slaughter never finished resolving");
}

fn effect_resolved(events: &[GameEvent], kind: EffectKind) -> bool {
    events
        .iter()
        .any(|event| matches!(event, GameEvent::EffectResolved { kind: k, .. } if *k == kind))
}

#[test]
fn wake_to_slaughter_with_no_targets_returns_nothing_to_hand() {
    let (runner, wake, creature, offered, events) = wake_to_slaughter_with_no_targets();
    // Reach: no card was chosen, and "return that card" still resolved.
    assert!(offered.is_empty(), "nothing to choose from: {offered:?}");
    assert!(effect_resolved(&events, EffectKind::ChooseFromZone));
    assert!(effect_resolved(&events, EffectKind::Bounce));
    // CR 608.2c + CR 609.3: "that card" names no card, so the return does
    // nothing; the spell goes to its owner's graveyard as it finishes resolving.
    assert_eq!(zone(&runner, wake), Zone::Graveyard);
    assert_eq!(zone(&runner, creature), Zone::Graveyard);
}
