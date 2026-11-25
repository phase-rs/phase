//! "defending player" of an attacking creature whose planeswalker relation was retired, or that
//! was removed from combat, through the real pipeline (CR 506.2 + CR 506.4 + CR 506.4c +
//! CR 508.5 + CR 608.2b).
//!
//! In a two-player game P0's Fiend Binder attacks P1's Jace Beleren, and its attack trigger
//! ("tap target creature defending player controls") targets P1's Grizzly Bears. With the trigger
//! on the stack, P0 casts Word of Seizing on Jace: Jace changes control, so it is removed from
//! combat (CR 506.4) and the Binder keeps attacking, attacking nothing (CR 506.4c). The Binder's
//! defending player is still P1: CR 506.2 names the nonactive player in a two-player game, and
//! CR 508.5's last clause keeps the player it was attacking before it was removed from combat. So
//! at the CR 608.2b re-check the Bears are still a legal target and become tapped, whether the
//! Binder is still attacking, has died (Murder), has been removed from combat (Reconnaissance),
//! or was removed and then died. Every card is its verbatim Oracle text.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{StackEntryKind, WaitingFor, ZoneChangeRecord};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const FIEND_BINDER: &str =
    "Whenever this creature attacks, tap target creature defending player controls.";
const WORD: &str = "Split second (As long as this spell is on the stack, players can't cast spells or activate abilities that aren't mana abilities.)\nUntap target permanent and gain control of it until end of turn. It gains haste until end of turn.";
const JACE: &str = "[+2]: Each player draws a card.\n[\u{2212}1]: Target player draws a card.\n[\u{2212}10]: Target player mills twenty cards.";
const MURDER: &str = "Destroy target creature.";
const RECONNAISSANCE: &str = "{0}: Remove target attacking creature you control from combat and untap it. (If you activate during end of combat, the creature will untap after it deals combat damage.)";

/// What P0 does while the Binder's trigger is on the stack.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Response {
    /// Nothing: the paired positive.
    Nothing,
    /// Word of Seizing on Jace (the attacked planeswalker) or on the Bears (the trigger's target).
    WordOn(WordTarget),
    /// Word of Seizing on Jace, then Murder on the Binder.
    WordThenMurder,
    /// Word of Seizing on Jace, then Reconnaissance on the Binder.
    WordThenRecon,
    /// Word of Seizing on Jace, then Reconnaissance on the Binder, then Murder on the Binder.
    WordThenReconThenMurder,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WordTarget {
    Jace,
    Bears,
}

struct Board {
    r: GameRunner,
    binder: ObjectId,
    jace: ObjectId,
    bears: ObjectId,
    word: ObjectId,
    murder: ObjectId,
    recon: ObjectId,
}

fn build() -> Board {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let binder = sc
        .add_creature_from_oracle(P0, "Fiend Binder", 3, 2, FIEND_BINDER)
        .id();
    let word = sc
        .add_spell_to_hand_from_oracle(P0, "Word of Seizing", true, WORD)
        .from_oracle_text_with_keywords(&["Split second"], WORD)
        .id();
    let murder = sc
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .id();
    let recon = sc
        .add_enchantment_from_oracle(P0, "Reconnaissance", RECONNAISSANCE)
        .id();
    let jace = sc
        .add_planeswalker_from_oracle(P1, "Jace Beleren", "Jace", 3, JACE)
        .as_planeswalker_with_loyalty("Jace", 3)
        .id();
    let bears = sc.add_creature(P1, "Grizzly Bears", 2, 2).id();
    Board {
        r: sc.build(),
        binder,
        jace,
        bears,
        word,
        murder,
        recon,
    }
}

/// Answer trigger ordering and the Binder trigger's target prompt (the Bears).
fn drain_orders(r: &mut GameRunner, target: ObjectId) {
    for _ in 0..16 {
        match r.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                r.act(GameAction::OrderTriggers { order }).unwrap();
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                r.act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(target)),
                })
                .unwrap();
            }
            _ => return,
        }
    }
    panic!("attack-trigger prompts did not settle");
}

/// The targets of the Binder's attack trigger on the stack, if it is there.
fn binder_trigger_targets(r: &GameRunner, binder: ObjectId) -> Option<Vec<TargetRef>> {
    r.state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == binder)
        .find_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility { ability, .. } => Some(ability.targets.clone()),
            _ => None,
        })
}

fn pass_to_p0(r: &mut GameRunner) {
    for _ in 0..3 {
        match r.state().waiting_for {
            WaitingFor::Priority { player: P0 } => return,
            WaitingFor::Priority { .. } => {
                r.act(GameAction::PassPriority).unwrap();
            }
            ref other => panic!("expected response priority: {other:?}"),
        }
    }
    panic!("P0 did not get response priority");
}

/// Cast `spell` on `target` and resolve just it, leaving the Binder's trigger on the stack.
fn resolve_response(r: &mut GameRunner, spell: ObjectId, target: ObjectId) {
    pass_to_p0(r);
    let mut cast = r.cast(spell).target_object(target).commit();
    for _ in 0..24 {
        if !cast
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == spell)
        {
            return;
        }
        cast.act(GameAction::PassPriority)
            .expect("response resolves");
    }
    panic!("response remained on stack");
}

/// Activate Reconnaissance on `binder` and resolve just that ability.
fn remove_with_recon(r: &mut GameRunner, recon: ObjectId, binder: ObjectId) {
    pass_to_p0(r);
    r.act(GameAction::ActivateAbility {
        source_id: recon,
        ability_index: 0,
        payment_mode: Default::default(),
    })
    .unwrap();
    if matches!(r.state().waiting_for, WaitingFor::TargetSelection { .. }) {
        r.act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(binder)),
        })
        .unwrap();
    }
    for _ in 0..24 {
        if !r.state().stack.iter().any(|entry| entry.source_id == recon) {
            return;
        }
        r.act(GameAction::PassPriority).unwrap();
    }
    panic!("Reconnaissance's ability remained on the stack");
}

/// Pass priority until the stack is empty, collecting the events.
fn settle_to_empty_stack(r: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..40 {
        match r.state().waiting_for {
            WaitingFor::Priority { .. } if r.state().stack.is_empty() => return events,
            WaitingFor::Priority { .. } => {
                events.extend(r.act(GameAction::PassPriority).unwrap().events);
            }
            ref other => panic!("unexpected prompt while settling: {other:?}"),
        }
    }
    panic!("the stack did not empty");
}

fn tapped_by_trigger(events: &[GameEvent], bears: ObjectId) -> bool {
    events
        .iter()
        .any(|event| matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == bears))
}

fn binder_departure_row(r: &GameRunner, binder: ObjectId) -> &ZoneChangeRecord {
    r.state()
        .zone_changes_this_turn
        .iter()
        .rev()
        .find(|row| row.object_id == binder && row.from_zone == Some(Zone::Battlefield))
        .expect("the Binder's departure row")
}

fn binder_is_listed(r: &GameRunner, binder: ObjectId) -> bool {
    r.state()
        .combat
        .as_ref()
        .unwrap()
        .attackers
        .iter()
        .any(|info| info.object_id == binder)
}

/// Assert the Word-on-Jace preconditions: Jace changed control, its relation is retired, and the
/// Binder is still an attacking creature recorded as attacking P1's planeswalker (CR 506.4c).
fn assert_relation_retired(b: &Board) {
    let state = b.r.state();
    assert_eq!(state.objects[&b.jace].controller, P0);
    let combat = state.combat.as_ref().unwrap();
    assert!(!combat.attacked_permanents.contains_key(&b.binder));
    let info = combat
        .attackers
        .iter()
        .find(|info| info.object_id == b.binder)
        .expect("CR 506.4c: the Binder is still attacking");
    assert_eq!(info.defending_player, P1);
}

fn assert_removed_record(b: &Board) -> ObjectIncarnationRef {
    let state = b.r.state();
    assert_eq!(state.objects[&b.binder].zone, Zone::Battlefield);
    assert!(!binder_is_listed(&b.r, b.binder));
    let incarnation = ObjectIncarnationRef {
        object_id: b.binder,
        incarnation: state.objects[&b.binder].incarnation,
    };
    let records = &state.combat.as_ref().unwrap().removed_attacker_defenders;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].attacker, incarnation);
    assert_eq!(records[0].defending_player, P1);
    incarnation
}

/// Drive the attack, the response and resolution. Returns the board and the events of the
/// trigger's resolution.
fn run(response: Response) -> (Board, Vec<GameEvent>) {
    let mut b = build();
    assert!(!b.r.state().objects[&b.bears].tapped);
    b.r.advance_to_combat();
    b.r.declare_attackers(&[(b.binder, AttackTarget::Planeswalker(b.jace))])
        .expect("the attack is legal");
    drain_orders(&mut b.r, b.bears);
    assert_eq!(
        binder_trigger_targets(&b.r, b.binder),
        Some(vec![TargetRef::Object(b.bears)]),
        "the Binder's trigger is on the stack targeting the Bears"
    );

    match response {
        Response::Nothing => {}
        Response::WordOn(WordTarget::Bears) => {
            resolve_response(&mut b.r, b.word, b.bears);
            assert_eq!(b.r.state().objects[&b.bears].controller, P0);
            assert!(
                b.r.state()
                    .combat
                    .as_ref()
                    .unwrap()
                    .attacked_permanents
                    .contains_key(&b.binder),
                "Jace's relation stays active"
            );
        }
        Response::WordOn(WordTarget::Jace) => {
            resolve_response(&mut b.r, b.word, b.jace);
            assert_relation_retired(&b);
        }
        Response::WordThenMurder => {
            resolve_response(&mut b.r, b.word, b.jace);
            assert_relation_retired(&b);
            resolve_response(&mut b.r, b.murder, b.binder);
            assert_eq!(b.r.state().objects[&b.binder].zone, Zone::Graveyard);
            let row = binder_departure_row(&b.r, b.binder);
            assert!(row.combat_status.attacking);
            assert_eq!(row.combat_status.defending_player, Some(P1));
        }
        Response::WordThenRecon => {
            resolve_response(&mut b.r, b.word, b.jace);
            assert_relation_retired(&b);
            remove_with_recon(&mut b.r, b.recon, b.binder);
            assert_removed_record(&b);
        }
        Response::WordThenReconThenMurder => {
            resolve_response(&mut b.r, b.word, b.jace);
            assert_relation_retired(&b);
            remove_with_recon(&mut b.r, b.recon, b.binder);
            let removed = assert_removed_record(&b);
            resolve_response(&mut b.r, b.murder, b.binder);
            assert_eq!(b.r.state().objects[&b.binder].zone, Zone::Graveyard);
            let row = binder_departure_row(&b.r, b.binder);
            assert!(!row.combat_status.attacking);
            assert_eq!(row.combat_status.defending_player, None);
            assert_eq!(
                row.trigger_source_context
                    .as_ref()
                    .expect("the row's source context")
                    .identity
                    .reference,
                removed
            );
            let records =
                &b.r.state()
                    .combat
                    .as_ref()
                    .unwrap()
                    .removed_attacker_defenders;
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].attacker, removed);
            assert_eq!(records[0].defending_player, P1);
        }
    }
    assert!(
        binder_trigger_targets(&b.r, b.binder).is_some(),
        "the Binder's trigger is still on the stack after the responses"
    );

    let events = settle_to_empty_stack(&mut b.r);
    assert!(b.r.state().stack.is_empty());
    assert!(matches!(
        b.r.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    (b, events)
}

fn assert_bears_tapped(response: Response) {
    let (b, events) = run(response);
    assert!(
        tapped_by_trigger(&events, b.bears),
        "the Bears are tapped by the Binder's trigger"
    );
    assert!(b.r.state().objects[&b.bears].tapped);
}

/// D1 (CR 506.2 + CR 506.4c + CR 508.5): P0 steals the attacked Jace; the Binder's defending
/// player is still P1, so the Bears stay a legal target (CR 608.2b).
#[test]
fn stolen_attacked_planeswalker_keeps_its_defender_for_fiend_binder() {
    assert_bears_tapped(Response::WordOn(WordTarget::Jace));
}

/// Paired positive: with no response the trigger taps the Bears.
#[test]
fn fiend_binder_taps_with_the_planeswalker_still_attacked() {
    assert_bears_tapped(Response::Nothing);
}

/// Hostile sibling (CR 608.2b): the Bears change control while Jace stays attacked; the Bears are
/// no longer controlled by the defending player P1, so the trigger does not tap them.
#[test]
fn fiend_binder_fizzles_when_its_target_leaves_the_defenders_control() {
    let (b, events) = run(Response::WordOn(WordTarget::Bears));
    assert!(!tapped_by_trigger(&events, b.bears));
    assert!(!b.r.state().objects[&b.bears].tapped);
}

/// D3 (CR 508.5 last clause + CR 608.2h): after the steal, Murder destroys the Binder; its
/// departure row records P1 as its defending player.
#[test]
fn destroyed_fiend_binder_keeps_the_defender_it_had_as_it_left() {
    assert_bears_tapped(Response::WordThenMurder);
}

/// D4 (CR 506.4 + CR 508.5 last clause): after the steal, Reconnaissance removes the Binder from
/// combat; its defending player is the one it had when removed, P1.
#[test]
fn fiend_binder_removed_from_combat_keeps_the_defender_it_had_when_removed() {
    assert_bears_tapped(Response::WordThenRecon);
}

/// D5 (CR 506.4 + CR 508.5 last clause): after the steal, Reconnaissance removes the Binder, then
/// Murder destroys it; its departure row records no defender (it was no longer attacking), and
/// the removal record of that incarnation answers P1.
#[test]
fn fiend_binder_removed_then_destroyed_keeps_the_defender_it_had_when_removed() {
    assert_bears_tapped(Response::WordThenReconThenMurder);
}
