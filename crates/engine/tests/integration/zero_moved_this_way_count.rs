//! CR 609.3 + CR 608.2c: a `ChangeZone` that moves nothing records "zero moved"
//! (`last_effect_count = Some(0)`), so a following "this way" / "that many"
//! step reads 0 instead of a count an earlier step of the chain left behind.
//! Every row drives a printed card through `GameRunner` / `GameAction`. Each
//! row but one reaches an empty-target guard in `effects::change_zone::resolve`
//! and has a positive control that really moves something; the Vengeful
//! Regrowth row is the production sibling-legality control (it keeps a legal
//! target, so it reaches no empty-target guard). The zero-result branch with a
//! legal sibling (`ParentTargetSlot`) has no printed card with a count
//! follower: the printed `ParentTargetSlot` producers are Goblin Welder
//! (Unimplemented step), Cloak and Dagger, Entwined, and Fiery Annihilation;
//! none of them reads "that many" / "this way".
//!
//! Printed cards (Oracle text verbatim, data/card-data.json):
//!   Myr Incubator — "{6}, {T}, Sacrifice this artifact: Search your library
//!     for any number of artifact cards, exile them, then create that many 1/1
//!     colorless Myr artifact creature tokens. Then shuffle."
//!   Ornithopter — "Flying"
//!   Vengeful Regrowth — "Return up to three target land cards from your
//!     graveyard to the battlefield tapped. Create that many 4/2 green Plant
//!     Warrior creature tokens with reach. / Flashback {6}{G}{G}"
//!   Meandering Towershell — "Islandwalk / Whenever this creature attacks,
//!     exile it. Return it to the battlefield under your control tapped and
//!     attacking at the beginning of the declare attackers step on your next
//!     turn."
//!   Unsummon — "Return target creature to its owner's hand."
//!   Cling to Dust — "Exile target card from a graveyard. If it was a creature
//!     card, you gain 3 life. Otherwise, you draw a card. / Escape—{3}{B}, Exile
//!     five other cards from your graveyard. (...)"
//!   All-Fates Stalker — "When this creature enters, exile up to one target
//!     non-Assassin creature until this creature leaves the battlefield. /
//!     Warp {1}{W}"
//!   Cloudshift — "Exile target creature you control, then return that card to
//!     the battlefield under your control."
//!
//! The former hand-built `ParentTargetSlot` row is gone: the only printed
//! `ChangeZone` with a `ParentTargetSlot` target is Goblin Welder, whose chain
//! carries an `Unimplemented` "unbound_subject" step and no count follower.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::EffectKind;
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::events::GameEvent;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn main_phase() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

fn add_mana(runner: &mut GameRunner, player: PlayerId, colored: &[ManaType], generic: usize) {
    let units = colored
        .iter()
        .copied()
        .chain(std::iter::repeat_n(ManaType::Colorless, generic));
    for color in units {
        let _ = runner
            .state_mut()
            .add_mana_to_pool(player, ManaUnit::new(color, ObjectId(0), false, vec![]));
    }
}

fn tokens_named(runner: &GameRunner, name: &str) -> usize {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter(|id| {
            let object = &state.objects[id];
            object.is_token && object.name == name
        })
        .count()
}

/// A `ChangeZone` from `source` resolved and reported that it moved nothing.
fn change_zone_resolved_empty(events: &[GameEvent], source: ObjectId) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::ChangeZone,
                source_id,
                subject: None,
            } if *source_id == source
        )
    })
}

/// Pass priority (placing triggers in identity order) until `done` holds at a
/// priority window.
fn pass_until(
    runner: &mut GameRunner,
    events: &mut Vec<GameEvent>,
    done: impl Fn(&GameRunner) -> bool,
) {
    for _ in 0..128 {
        match runner.state().waiting_for {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if done(runner) {
                    return;
                }
                let result = runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must be accepted");
                events.extend(result.events);
            }
            ref other => panic!("pass_until stopped at an unexpected prompt: {other:?}"),
        }
    }
    panic!("pass_until never reached its goal");
}

fn trigger_from_on_stack(runner: &GameRunner, source: ObjectId) -> bool {
    runner.state().stack.iter().any(|entry| {
        matches!(entry.kind, StackEntryKind::TriggeredAbility { .. }) && entry.source_id == source
    })
}

// ---------------------------------------------------------------------------
// CR 701.23b + CR 401.2: Library + Any fail-to-find guard.
// ---------------------------------------------------------------------------

/// Activate Myr Incubator and resolve it, returning the activation's events.
fn incubate(runner: &mut GameRunner, incubator: ObjectId) -> Vec<GameEvent> {
    add_mana(runner, P0, &[], 6);
    let outcome = runner.activate(incubator, 0).search_first_legal().resolve();
    outcome.events().to_vec()
}

/// P0's library holds no artifact, so the search finds nothing and the
/// put-step moves nothing; in particular it does not reach into P1's library,
/// which does hold an artifact. "That many" Myr is 0.
#[test]
fn myr_incubator_finding_nothing_moves_nothing_and_creates_no_myr() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = main_phase();
    scenario.with_library_top(P0, &["Forest", "Island"]);
    let incubator = scenario.add_real_card(P0, "Myr Incubator", Zone::Battlefield, db);
    let opposing = scenario.add_real_card(P1, "Ornithopter", Zone::Library, db);
    let mut runner = scenario.build();

    let events = incubate(&mut runner, incubator);
    assert_eq!(
        runner.state().objects[&incubator].zone,
        Zone::Graveyard,
        "reach: the sacrifice cost was paid"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "no choice is offered: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().objects[&opposing].zone,
        Zone::Library,
        "P1's artifact stays in P1's library"
    );
    assert!(change_zone_resolved_empty(&events, incubator));
    assert_eq!(tokens_named(&runner, "Myr"), 0);
    assert_eq!(runner.state().last_effect_count, Some(0));
    assert!(!runner.state().cost_payment_failed_flag);
}

/// Positive control: the found artifact is exiled and one Myr is created.
#[test]
fn myr_incubator_exiles_the_found_artifact_and_creates_that_many_myr() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = main_phase();
    let incubator = scenario.add_real_card(P0, "Myr Incubator", Zone::Battlefield, db);
    let found = scenario.add_real_card(P0, "Ornithopter", Zone::Library, db);
    let mut runner = scenario.build();

    incubate(&mut runner, incubator);
    assert_eq!(runner.state().objects[&found].zone, Zone::Exile);
    assert_eq!(tokens_named(&runner, "Myr"), 1);
}

// ---------------------------------------------------------------------------
// CR 115.6: optional targeting with zero targets chosen.
// ---------------------------------------------------------------------------

fn regrowth_board() -> Option<(GameRunner, ObjectId, [ObjectId; 2])> {
    let db = shared_card_db()?;
    let mut scenario = main_phase();
    let regrowth = scenario.add_real_card(P0, "Vengeful Regrowth", Zone::Hand, db);
    let lands = [
        scenario.add_real_card(P0, "Forest", Zone::Graveyard, db),
        scenario.add_real_card(P0, "Forest", Zone::Graveyard, db),
    ];
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, &[ManaType::Green, ManaType::Green], 4);
    Some((runner, regrowth, lands))
}

/// "Up to three" with no target chosen: no land returns and "that many"
/// Plant Warriors is 0.
#[test]
fn vengeful_regrowth_with_no_targets_returns_nothing_and_creates_no_plants() {
    let Some((mut runner, regrowth, lands)) = regrowth_board() else {
        return;
    };
    let outcome = runner.cast(regrowth).resolve();
    let events = outcome.events().to_vec();
    assert_eq!(
        runner.state().objects[&regrowth].zone,
        Zone::Graveyard,
        "reach: the spell resolved"
    );
    for land in lands {
        assert_eq!(runner.state().objects[&land].zone, Zone::Graveyard);
    }
    assert!(change_zone_resolved_empty(&events, regrowth));
    assert_eq!(tokens_named(&runner, "Plant Warrior"), 0);
    assert_eq!(runner.state().last_effect_count, Some(0));
    assert!(!runner.state().cost_payment_failed_flag);
}

/// Positive control: two lands targeted, two returned, two Plant Warriors.
#[test]
fn vengeful_regrowth_returns_its_targets_and_creates_that_many_plants() {
    let Some((mut runner, regrowth, lands)) = regrowth_board() else {
        return;
    };
    runner.cast(regrowth).target_objects(&lands).resolve();
    for land in lands {
        assert_eq!(runner.state().objects[&land].zone, Zone::Battlefield);
    }
    assert_eq!(tokens_named(&runner, "Plant Warrior"), 2);
}

/// CR 608.2b: a target that became illegal as the spell resolves is not
/// affected, while a sibling target that is still legal is. Both lands are
/// declared through the production cast path; in response, Cling to Dust is cast
/// through the production cast path and exiles the first land from the
/// graveyard, so one land returns and "that many" Plant Warriors is 1
/// (not the 2 targets declared). A sibling-legality control, not a zero-moved
/// row: the target list stays non-empty, so it reaches no empty-target guard.
#[test]
fn vengeful_regrowth_with_one_target_invalidated_returns_the_legal_sibling_only() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = main_phase();
    let regrowth = scenario.add_real_card(P0, "Vengeful Regrowth", Zone::Hand, db);
    let cling = scenario.add_real_card(P0, "Cling to Dust", Zone::Hand, db);
    // Cling to Dust draws for a non-creature card; keep P0 off an empty library.
    scenario.with_library_top(P0, &["Island", "Swamp"]);
    let lands = [
        scenario.add_real_card(P0, "Forest", Zone::Graveyard, db),
        scenario.add_real_card(P0, "Forest", Zone::Graveyard, db),
    ];
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, &[ManaType::Green, ManaType::Green], 4);
    add_mana(&mut runner, P0, &[ManaType::Black], 0);

    let commit = runner.cast(regrowth).target_objects(&lands).commit();
    let mut events = commit.events().to_vec();
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == regrowth),
        "reach: Vengeful Regrowth is on the stack with both lands declared"
    );
    // In response: Cling to Dust exiles the first land from the graveyard.
    let response = runner.cast(cling).target_object(lands[0]).commit();
    events.extend(response.events().iter().cloned());
    pass_until(&mut runner, &mut events, |r| {
        r.state().objects[&lands[0]].zone == Zone::Exile
    });
    assert_eq!(
        runner.state().objects[&lands[0]].zone,
        Zone::Exile,
        "reach: the first target left the graveyard while the spell was on the stack"
    );
    pass_until(&mut runner, &mut events, |r| {
        !r.state()
            .stack
            .iter()
            .any(|entry| entry.source_id == regrowth)
    });
    assert_eq!(
        runner.state().objects[&lands[0]].zone,
        Zone::Exile,
        "the illegal target is untouched"
    );
    assert_eq!(runner.state().objects[&lands[1]].zone, Zone::Battlefield);
    assert_eq!(tokens_named(&runner, "Plant Warrior"), 1);
    assert_eq!(runner.state().last_effect_count, Some(1));
}

// ---------------------------------------------------------------------------
// CR 400.7: an untargeted "it" whose referent was the source, after the source
// changed zones (inferred-source guard).
// ---------------------------------------------------------------------------

/// Attack with Meandering Towershell; with `bounce`, Unsummon returns it to
/// hand in response to its attack trigger. Returns the board after the
/// trigger has resolved, and that resolution's events.
fn towershell_attack(bounce: bool) -> Option<(GameRunner, ObjectId, Vec<GameEvent>)> {
    let db = shared_card_db()?;
    let mut scenario = main_phase();
    let towershell = scenario.add_real_card(P0, "Meandering Towershell", Zone::Battlefield, db);
    let unsummon = scenario.add_real_card(P0, "Unsummon", Zone::Hand, db);
    let mut runner = scenario.build();
    runner.advance_to_combat();
    let mut events = Vec::new();
    for _ in 0..16 {
        match runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => break,
            WaitingFor::Priority { .. } => {
                let result = runner.act(GameAction::PassPriority).unwrap();
                events.extend(result.events);
            }
            ref other => panic!("unexpected prompt before attacks: {other:?}"),
        }
    }
    let result = runner
        .declare_attackers(&[(towershell, AttackTarget::Player(P1))])
        .expect("Towershell attacks");
    events.extend(result.events);
    if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
        drain_order_triggers_with_identity(runner.state_mut());
    }
    assert!(
        trigger_from_on_stack(&runner, towershell),
        "reach: the attack trigger is on the stack"
    );
    if bounce {
        add_mana(&mut runner, P0, &[ManaType::Blue], 0);
        let commit = runner.cast(unsummon).target_object(towershell).commit();
        events.extend(commit.events().iter().cloned());
        pass_until(&mut runner, &mut events, |r| {
            !r.state()
                .stack
                .iter()
                .any(|entry| entry.source_id == unsummon)
        });
        assert_eq!(
            runner.state().objects[&towershell].zone,
            Zone::Hand,
            "reach: Unsummon returned Towershell before its trigger resolved"
        );
    }
    let start = events.len();
    pass_until(&mut runner, &mut events, |r| {
        !trigger_from_on_stack(r, towershell)
    });
    let trigger_events = events.split_off(start);
    Some((runner, towershell, trigger_events))
}

#[test]
fn towershell_bounced_in_response_to_its_attack_trigger_moves_nothing() {
    let Some((runner, towershell, events)) = towershell_attack(true) else {
        return;
    };
    assert_eq!(runner.state().objects[&towershell].zone, Zone::Hand);
    assert!(change_zone_resolved_empty(&events, towershell));
    assert_eq!(runner.state().last_effect_count, Some(0));
    assert!(!runner.state().cost_payment_failed_flag);
    // CR 603.7a + CR 603.7c: the "return it" delayed trigger is still
    // created; its object is no longer in exile, so it will do nothing.
}

/// Positive control: no bounce, the trigger exiles Towershell (one moved)
/// and sets up its return.
#[test]
fn towershell_attack_trigger_exiles_it() {
    let Some((runner, towershell, _events)) = towershell_attack(false) else {
        return;
    };
    assert_eq!(runner.state().objects[&towershell].zone, Zone::Exile);
    assert_eq!(runner.state().last_effect_count, Some(1));
    assert!(!runner.state().delayed_triggers.is_empty());
}

// ---------------------------------------------------------------------------
// CR 400.7: a Warp delayed exile (SelfRef) after the source was blinked.
// ---------------------------------------------------------------------------

/// Cast All-Fates Stalker for its warp cost (its enter trigger exiles
/// nothing); with `blink`, Cloudshift it. Then run to the end step until the
/// warp exile trigger has resolved; returns that resolution's events.
fn warped_stalker(blink: bool) -> Option<(GameRunner, ObjectId, Vec<GameEvent>)> {
    let db = shared_card_db()?;
    let mut scenario = main_phase();
    let stalker = scenario.add_real_card(P0, "All-Fates Stalker", Zone::Hand, db);
    let cloudshift = scenario.add_real_card(P0, "Cloudshift", Zone::Hand, db);
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, &[ManaType::White], 1);
    runner
        .cast(stalker)
        .alternative_cast(AlternativeCastDecision::Alternative)
        .resolve();
    assert_eq!(
        runner.state().objects[&stalker].zone,
        Zone::Battlefield,
        "reach: the warped Stalker entered"
    );
    assert!(
        !runner.state().delayed_triggers.is_empty(),
        "reach: the warp exile is scheduled"
    );
    if blink {
        add_mana(&mut runner, P0, &[ManaType::White], 0);
        runner.cast(cloudshift).target_object(stalker).resolve();
        assert_eq!(
            runner.state().objects[&stalker].zone,
            Zone::Battlefield,
            "reach: Cloudshift returned the Stalker as a new object"
        );
    }
    let mut events = Vec::new();
    for _ in 0..256 {
        if runner.state().phase == Phase::End && trigger_from_on_stack(&runner, stalker) {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                let result = runner.act(GameAction::PassPriority).unwrap();
                events.extend(result.events);
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::DeclareAttackers { .. } => {
                let result = runner.declare_attackers(&[]).unwrap();
                events.extend(result.events);
            }
            other => panic!("unexpected prompt before the end step: {other:?}"),
        }
    }
    assert!(
        trigger_from_on_stack(&runner, stalker),
        "reach: the warp exile trigger is on the stack at the end step"
    );
    let start = events.len();
    pass_until(&mut runner, &mut events, |r| {
        !trigger_from_on_stack(r, stalker)
    });
    let trigger_events = events.split_off(start);
    Some((runner, stalker, trigger_events))
}

#[test]
fn warp_exile_after_a_blink_leaves_the_new_stalker_alone() {
    let Some((runner, stalker, events)) = warped_stalker(true) else {
        return;
    };
    assert_eq!(runner.state().objects[&stalker].zone, Zone::Battlefield);
    assert!(change_zone_resolved_empty(&events, stalker));
    assert_eq!(runner.state().last_effect_count, Some(0));
    assert!(!runner.state().cost_payment_failed_flag);
}

/// Positive control: without the blink the warp exile exiles the Stalker.
#[test]
fn warp_exile_exiles_the_unblinked_stalker() {
    let Some((runner, stalker, _events)) = warped_stalker(false) else {
        return;
    };
    assert_eq!(runner.state().objects[&stalker].zone, Zone::Exile);
    assert_eq!(runner.state().last_effect_count, Some(1));
}
