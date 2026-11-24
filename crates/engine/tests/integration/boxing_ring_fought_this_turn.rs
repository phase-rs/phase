//! Boxing Ring's "Activate only if you control a creature that fought this
//! turn" (CR 602.5 + CR 701.14a/b): the per-turn fight ledger and
//! `FilterProp::FoughtThisTurn`.

use engine::analysis::resource::loop_states_equal_modulo_resources;
use engine::game::engine::{apply, EngineError};
use engine::game::game_object::ObjectIncarnationRefExt;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const BOXING_RING: &str = "Whenever a creature you control enters, it fights up to one target creature you don't control with the same mana value.\n{T}: Create a Treasure token. Activate only if you control a creature that fought this turn.";
const PREY_UPON: &str = "Target creature you control fights target creature you don't control. (Each deals damage equal to its power to the other.)";
const LIGHTNING_BOLT: &str = "Lightning Bolt deals 3 damage to any target.";
const CLOUDSHIFT: &str =
    "Exile target creature you control, then return that card to the battlefield under your control.";
const KOGLA: &str = "When Kogla enters, it fights up to one target creature you don't control.\nWhenever Kogla attacks, destroy target artifact or enchantment defending player controls.\n{1}{G}: Return target Human you control to its owner's hand. Kogla gains indestructible until end of turn.";
const SLIP_OUT_THE_BACK: &str = "Put a +1/+1 counter on target creature. It phases out. (Treat it and anything attached to it as though they don't exist until its controller's next turn.)";

struct Board {
    runner: GameRunner,
    ring: ObjectId,
    mine: ObjectId,
    theirs: ObjectId,
    prey: ObjectId,
    /// The `instants` passed to `board`, in P0's hand, in order.
    instants: Vec<ObjectId>,
}

/// Boxing Ring, P0's `mine`, P1's `theirs`, and Prey Upon plus `instants` in
/// P0's hand.
fn board(mine_pt: (i32, i32), theirs_pt: (i32, i32), instants: &[(&str, &str)]) -> Board {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let ring = s
        .add_artifact_from_oracle(P0, "Boxing Ring", BOXING_RING)
        .id();
    let mine = s.add_vanilla(P0, mine_pt.0, mine_pt.1);
    let theirs = s.add_vanilla(P1, theirs_pt.0, theirs_pt.1);
    let prey = s
        .add_spell_to_hand_from_oracle(P0, "Prey Upon", false, PREY_UPON)
        .id();
    let instants = instants
        .iter()
        .map(|(name, oracle)| s.add_spell_to_hand_from_oracle(P0, name, true, oracle).id())
        .collect();
    s.with_library_top(P0, &["Card A", "Card B"]);
    s.with_library_top(P1, &["Card C", "Card D"]);
    Board {
        runner: s.build(),
        ring,
        mine,
        theirs,
        prey,
        instants,
    }
}

fn prey_upon(b: &mut Board) {
    b.runner
        .cast(b.prey)
        .target_objects(&[b.mine, b.theirs])
        .resolve();
}

fn incarnation(r: &GameRunner, id: ObjectId) -> ObjectIncarnationRef {
    ObjectIncarnationRef::from_object(&r.state().objects[&id])
}

fn treasures(r: &GameRunner) -> usize {
    r.state()
        .objects
        .values()
        .filter(|o| {
            o.zone == Zone::Battlefield
                && o.controller == P0
                && o.card_types.subtypes.iter().any(|t| t == "Treasure")
        })
        .count()
}

fn assert_on_battlefield_under_p0(r: &GameRunner, id: ObjectId) {
    let obj = &r.state().objects[&id];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert_eq!(obj.controller, P0);
}

/// CR 602.5: the activation is refused and nothing was paid or created.
fn assert_refused(b: &mut Board) {
    let refused = b.runner.act(GameAction::ActivateAbility {
        source_id: b.ring,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    });
    assert!(
        matches!(&refused, Err(EngineError::ActionNotAllowed(m))
            if m.starts_with("Activation restriction not satisfied")),
        "{refused:?}"
    );
    assert!(!b.runner.state().objects[&b.ring].tapped);
    assert_eq!(treasures(&b.runner), 0);
}

fn assert_allowed(b: &mut Board) {
    if let Err(e) = b.runner.activate(b.ring, 0).try_resolve() {
        panic!("activation refused: {e:?}");
    }
    assert_eq!(treasures(&b.runner), 1);
    assert!(b.runner.state().stack.is_empty());
    assert!(matches!(
        b.runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// Passes priority (declaring no attackers or blockers) until P0's next
/// precombat main phase.
fn pass_to_own_next_main(r: &mut GameRunner) {
    let start = r.state().turn_number;
    for _ in 0..600 {
        let st = r.state();
        if st.turn_number >= start + 2 && st.active_player == P0 && st.phase == Phase::PreCombatMain
        {
            return;
        }
        let action = match r.state().waiting_for.clone() {
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
        r.act(action).expect("the turn cycle advances");
    }
    panic!("did not reach P0's next main phase");
}

#[test]
fn refused_before_any_fight_allowed_after_your_creature_fought() {
    let mut b = board((3, 3), (1, 1), &[]);
    assert_refused(&mut b);

    prey_upon(&mut b);
    assert_eq!(b.runner.state().objects[&b.theirs].zone, Zone::Graveyard);
    assert_allowed(&mut b);
}

#[test]
fn only_a_fighter_you_control_counts() {
    let mut b = board((1, 1), (3, 3), &[]);
    prey_upon(&mut b);

    assert_eq!(b.runner.state().objects[&b.mine].zone, Zone::Graveyard);
    let survivor = incarnation(&b.runner, b.theirs);
    assert!(b
        .runner
        .state()
        .creatures_fought_this_turn
        .contains(&survivor));
    assert_refused(&mut b);
}

/// CR 120.8: a 0-power fighter deals no damage, yet it still fought.
#[test]
fn zero_power_fighters_still_fought() {
    let mut b = board((0, 2), (0, 2), &[]);
    prey_upon(&mut b);

    assert_eq!(b.runner.state().objects[&b.mine].damage_marked, 0);
    assert_eq!(b.runner.state().objects[&b.theirs].damage_marked, 0);
    assert_allowed(&mut b);
}

/// CR 701.14b + CR 608.2b: one fighter became an illegal target, so neither
/// fights.
#[test]
fn a_fight_with_an_illegal_target_records_nothing() {
    let mut b = board((2, 2), (2, 2), &[("Lightning Bolt", LIGHTNING_BOLT)]);
    let bolt = b.instants[0];
    let mut prey = b
        .runner
        .cast(b.prey)
        .target_objects(&[b.mine, b.theirs])
        .commit();
    prey.cast(bolt).target_objects(&[b.theirs]).resolve();
    prey.resolve();

    let state = b.runner.state();
    assert_eq!(state.objects[&b.prey].zone, Zone::Graveyard);
    assert_eq!(state.objects[&b.theirs].zone, Zone::Graveyard);
    assert_on_battlefield_under_p0(&b.runner, b.mine);
    assert_eq!(state.objects[&b.mine].damage_marked, 0);
    assert!(state.creatures_fought_this_turn.is_empty());
    assert_refused(&mut b);
}

/// CR 400.7: a fighter that left and returned is a new object that never
/// fought.
#[test]
fn a_fighter_that_changed_zones_is_a_new_object() {
    let mut b = board((3, 3), (1, 1), &[("Cloudshift", CLOUDSHIFT)]);
    let cloudshift = b.instants[0];
    prey_upon(&mut b);
    let fought = incarnation(&b.runner, b.mine);
    assert!(b
        .runner
        .state()
        .creatures_fought_this_turn
        .contains(&fought));

    b.runner
        .cast(cloudshift)
        .target_objects(&[b.mine])
        .resolve();
    b.runner.advance_until_stack_empty();

    assert_on_battlefield_under_p0(&b.runner, b.mine);
    assert_ne!(incarnation(&b.runner, b.mine), fought);
    assert_refused(&mut b);
}

#[test]
fn next_turn_resets_the_fight_ledger() {
    let mut b = board((3, 3), (1, 1), &[]);
    prey_upon(&mut b);
    assert!(!b.runner.state().creatures_fought_this_turn.is_empty());

    pass_to_own_next_main(&mut b.runner);

    assert_on_battlefield_under_p0(&b.runner, b.mine);
    assert_refused(&mut b);
}

/// CR 701.14b: Kogla phased out in response to its own enter trigger, so
/// neither creature fights.
#[test]
fn an_ineligible_fighter_records_nothing() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let kogla = s
        .add_creature_to_hand_from_oracle(P0, "Kogla, the Titan Ape", 7, 6, KOGLA)
        .id();
    let slip = s
        .add_spell_to_hand_from_oracle(P0, "Slip Out the Back", true, SLIP_OUT_THE_BACK)
        .id();
    let theirs = s.add_vanilla(P1, 2, 2);
    let mut r = s.build();

    r.cast(kogla).commit();
    for _ in 0..10 {
        if r.state().objects[&kogla].zone == Zone::Battlefield && !r.state().stack.is_empty() {
            break;
        }
        if matches!(
            r.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ) {
            r.act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(theirs)),
            })
            .expect("Kogla's trigger targets their creature");
            continue;
        }
        r.act(GameAction::PassPriority).expect("priority passes");
    }
    let trigger = r
        .state()
        .stack
        .last()
        .expect("Kogla's trigger is on the stack");
    assert_eq!(trigger.source_id, kogla);
    assert_eq!(
        trigger.ability().map(|a| a.targets.clone()),
        Some(vec![TargetRef::Object(theirs)])
    );

    r.cast(slip).target_objects(&[kogla]).resolve();

    let state = r.state();
    assert!(state.creatures_fought_this_turn.is_empty());
    assert_on_battlefield_under_p0(&r, kogla);
    assert!(state.objects[&kogla].is_phased_out());
    assert_eq!(state.objects[&theirs].zone, Zone::Battlefield);
    assert_eq!(state.objects[&theirs].damage_marked, 0);
    assert!(state.stack.is_empty());
}

/// `frame` without `fighter`'s fight entry.
fn without(frame: &GameState, fighter: ObjectIncarnationRef) -> GameState {
    let mut state = frame.clone();
    assert!(state.creatures_fought_this_turn.remove(&fighter));
    state
}

/// CR 602.5: whether Boxing Ring's activation may begin in `frame`.
fn ring_allowed(frame: &GameState, ring: ObjectId) -> bool {
    let mut state = frame.clone();
    match apply(
        &mut state,
        P0,
        GameAction::ActivateAbility {
            source_id: ring,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        },
    ) {
        Ok(_) => true,
        Err(EngineError::ActionNotAllowed(m))
            if m.starts_with("Activation restriction not satisfied") =>
        {
            false
        }
        Err(e) => panic!("{e:?}"),
    }
}

/// CR 701.14a + CR 400.7 + CR 602.5: a sacrificed fighter is a new object Boxing Ring can no
/// longer see, so its entry is not loop position unless a carried trigger still names it, while
/// a live fighter's entry is.
#[test]
fn spent_fight_history_is_not_position_but_an_observable_fighter_is() {
    let db = shared_card_db().expect("the card fixture");
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let ring = s
        .add_artifact_from_oracle(P0, "Boxing Ring", BOXING_RING)
        .id();
    let bears = s.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let memnite = s.add_real_card(P0, "Memnite", Zone::Battlefield, db);
    let altar = s.add_real_card(P0, "Ashnod's Altar", Zone::Battlefield, db);
    s.add_real_card(P0, "Blood Artist", Zone::Battlefield, db);
    let preys = [(); 2].map(|()| s.add_real_card(P0, "Prey Upon", Zone::Hand, db));
    for _ in 0..2 {
        s.add_basic_land(P0, ManaColor::Green);
    }
    let myr = s.add_real_card(P1, "Darksteel Myr", Zone::Battlefield, db);
    s.with_library_top(P0, &["Card A", "Card B"]);
    s.with_library_top(P1, &["Card C", "Card D"]);
    let mut r = s.build();
    for (prey, mine) in preys.into_iter().zip([bears, memnite]) {
        r.cast(prey).target_objects(&[mine, myr]).resolve();
    }
    let observable = incarnation(&r, bears);
    let spent = incarnation(&r, memnite);
    assert!(r.state().creatures_fought_this_turn.contains(&spent));
    r.act(GameAction::ActivateAbility {
        source_id: altar,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("Ashnod's Altar activates");
    for _ in 0..10 {
        let action = match &r.state().waiting_for {
            WaitingFor::PayCost { .. } => GameAction::SelectCards {
                cards: vec![memnite],
            },
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(P1)),
                }
            }
            _ => break,
        };
        r.act(action).expect("the prompt is answered");
    }
    assert!(!r.state().stack.is_empty(), "{:?}", r.state().waiting_for);
    let carried = r.state().clone();
    r.advance_until_stack_empty();
    let settled = r.state().clone();

    let table = [spent, observable].map(|fighter| {
        let state = without(&settled, fighter);
        (
            ring_allowed(&state, ring),
            loop_states_equal_modulo_resources(&settled, &state),
        )
    });
    assert_eq!(table, [(true, true), (false, false)]);
    assert!(!loop_states_equal_modulo_resources(
        &carried,
        &without(&carried, spent)
    ));
}
