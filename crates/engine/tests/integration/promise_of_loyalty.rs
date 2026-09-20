//! Promise of Loyalty and its keeper-and-dispose siblings — the per-player
//! keeper choice, the vow counter that marks each keeper, and the
//! controller-relative attack prohibition the keepers carry.
//!
//! Every Oracle string here is verbatim from `client/public/card-data.json`.
//! Regenerate any of them with
//! `jq -r '.["promise of loyalty"].oracle_text' client/public/card-data.json`.
//!
//! CR 101.4: every player in scope nominates a keeper, in APNAP order.
//! CR 701.21a: each unchosen permanent is sacrificed by its own controller.
//! CR 122.1: the vow counter is what marks the keeper.
//! CR 109.5 + CR 508.1c: "you" in the granted prohibition means the player who
//! resolved the spell, latched at resolution.
//! CR 611.2b: "for as long as it has a vow counter on it" is re-evaluated, so
//! removing the counter lifts the restriction.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::{
    GameRestriction, ProhibitedActivity, RestrictionExpiry, RestrictionPlayerScope,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Scryfall Oracle text, byte-identical to `client/public/card-data.json`.
const PROMISE_OF_LOYALTY: &str = "Each player puts a vow counter on a creature they control and sacrifices the rest. Each of those creatures can't attack you or planeswalkers you control for as long as it has a vow counter on it.";
/// Razia's Purification, verbatim.
const RAZIAS_PURIFICATION: &str =
    "Each player chooses three permanents they control, then sacrifices the rest.";
/// Single Combat, verbatim.
const SINGLE_COMBAT: &str = "Each player chooses a creature or planeswalker they control, then sacrifices the rest. Players can't cast creature or planeswalker spells until the end of your next turn.";
/// Planetary Annihilation, verbatim.
const PLANETARY_ANNIHILATION: &str = "Each player chooses six lands they control, then sacrifices the rest. Planetary Annihilation deals 6 damage to each creature.";
/// Limited Resources' enters-the-battlefield trigger, verbatim.
const LIMITED_RESOURCES: &str =
    "When this enchantment enters, each player chooses five lands they control and sacrifices the rest.";
const P2: PlayerId = PlayerId(2);

const VOW: fn() -> CounterType = || CounterType::Generic("vow".to_string());

/// Put a fresh 2/2 creature onto the battlefield AFTER a spell has resolved, so
/// it is provably outside any tracked set the resolution published.
fn spawn_creature(state: &mut GameState, player: PlayerId, name: &str) -> ObjectId {
    let id = create_object(
        state,
        CardId(state.next_object_id),
        player,
        name.to_string(),
        Zone::Battlefield,
    );
    let object = state.objects.get_mut(&id).expect("spawned object exists");
    object.card_types.core_types.push(CoreType::Creature);
    object.base_card_types = object.card_types.clone();
    object.base_power = Some(2);
    object.base_toughness = Some(2);
    object.power = Some(2);
    object.toughness = Some(2);
    object.summoning_sick = false;
    state.layers_dirty.mark_full();
    id
}

fn vow_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|object| object.counters.get(&VOW()).copied())
        .unwrap_or(0)
}

/// Drive the game to `player`'s declare-attackers step (CR 508.1), passing
/// every window in between. `GameRunner::advance_to_phase` cannot do this on
/// its own: it stops at the first non-`Priority` window, which is the CASTER's
/// own declare-attackers step, so a test using it would silently assert about
/// the wrong seat's combat.
fn advance_to_declare_attackers_for(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..256 {
        if runner.state().active_player == player
            && matches!(
                runner.state().waiting_for,
                WaitingFor::DeclareAttackers { .. }
            )
        {
            return;
        }
        let stepped = match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority).is_ok(),
            WaitingFor::DeclareAttackers { .. } => runner
                .act(GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                })
                .is_ok(),
            WaitingFor::DeclareBlockers { .. } => runner
                .act(GameAction::DeclareBlockers {
                    assignments: vec![],
                })
                .is_ok(),
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                runner.act(GameAction::OrderTriggers { order }).is_ok()
            }
            _ => false,
        };
        if !stepped {
            break;
        }
    }
    panic!(
        "could not reach {player:?}'s declare-attackers step: phase {:?}, active {:?}, waiting {:?}",
        runner.state().phase,
        runner.state().active_player,
        runner.state().waiting_for
    );
}

/// CR 704.5b: a player who would draw from an empty library loses. These tests
/// cross into a later turn's draw step, so every seat needs a library to draw
/// from — without it the game ends before combat and every combat assertion
/// below would be vacuous.
fn stock_libraries(scenario: &mut GameScenario, players: &[PlayerId]) {
    for player in players {
        for index in 0..8 {
            scenario.add_card_to_library_top(*player, &format!("Filler {index}"));
        }
    }
}

struct PromiseBoard {
    runner: GameRunner,
    p0_keeper: ObjectId,
    p0_doomed: ObjectId,
    p1_keeper: ObjectId,
    p1_doomed: ObjectId,
}

/// Cast Promise of Loyalty with two creatures per seat and answer both keeper
/// prompts. Returns the board after the spell has fully resolved.
fn resolve_promise_of_loyalty() -> PromiseBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let p0_keeper = scenario.add_creature(P0, "Caster Keeper", 2, 2).id();
    let p0_doomed = scenario.add_creature(P0, "Caster Doomed", 2, 2).id();
    let p1_keeper = scenario.add_creature(P1, "Rival Keeper", 2, 2).id();
    let p1_doomed = scenario.add_creature(P1, "Rival Doomed", 2, 2).id();
    stock_libraries(&mut scenario, &[P0, P1]);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Promise of Loyalty", false, PROMISE_OF_LOYALTY)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::KeepExactPermanentsChoice {
                required_count: 1,
                ..
            }
        ),
        "CR 101.4 + CR 609.3: resolution must pause for each seat's exact keeper choice, got {:?}",
        outcome.final_waiting_for()
    );
    drop(outcome);

    for keeper in [p0_keeper, p1_keeper] {
        runner
            .act(GameAction::ChooseKeptPermanents { kept: vec![keeper] })
            .expect("each seat keeps exactly one creature it controls");
    }
    runner.advance_until_stack_empty();

    PromiseBoard {
        runner,
        p0_keeper,
        p0_doomed,
        p1_keeper,
        p1_doomed,
    }
}

/// V1 — CR 701.21a: sentence one keeps one creature per seat and sacrifices
/// every other one, on both seats rather than only the caster's.
///
/// Reverting the `EachPlayerSelf` lowering (or the recognizer that produces it)
/// puts the doomed creatures back on the battlefield.
#[test]
fn promise_of_loyalty_each_player_keeps_one_creature() {
    let PromiseBoard {
        runner,
        p0_keeper,
        p0_doomed,
        p1_keeper,
        p1_doomed,
    } = resolve_promise_of_loyalty();

    for kept in [p0_keeper, p1_keeper] {
        assert_eq!(
            runner.state().objects[&kept].zone,
            Zone::Battlefield,
            "the nominated keeper must survive"
        );
    }
    for sacrificed in [p0_doomed, p1_doomed] {
        assert_eq!(
            runner.state().objects[&sacrificed].zone,
            Zone::Graveyard,
            "CR 701.21a: every unchosen creature is sacrificed"
        );
    }
}

/// V1's hostile fixture — CR 609.3: a seat with at most the printed count of
/// eligible creatures is never prompted; it keeps what it has. Reaching this
/// branch (`step_exact_count`'s `eligible.len() <= count` auto-keep) is what
/// proves the prompt loop is not simply skipped when a seat is empty.
#[test]
fn promise_of_loyalty_auto_keeps_a_seat_that_cannot_choose() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P0 has exactly one creature (nothing to choose), P1 has none at all.
    let only_creature = scenario.add_creature(P0, "Sole Survivor", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Promise of Loyalty", false, PROMISE_OF_LOYALTY)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();
    assert!(
        !matches!(
            outcome.final_waiting_for(),
            WaitingFor::KeepExactPermanentsChoice { .. }
        ),
        "no seat can make a meaningful choice, so no prompt may be raised: {:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(
        outcome.zone_of(only_creature),
        Zone::Battlefield,
        "the sole eligible creature is auto-kept, not sacrificed"
    );
    drop(outcome);
    // Positive reach-guard: the auto-kept creature still went through the
    // keeper pipeline, so it carries the vow counter the head prints.
    assert_eq!(
        vow_counters(&runner, only_creature),
        1,
        "the auto-kept keeper must still be marked"
    );
}

/// V2 — CR 122.1: each keeper gets exactly one vow counter, on BOTH seats.
/// Two seats prove the counter lands on the UNION of keepers rather than on
/// whichever seat chose last.
///
/// Reverting `Effect::PutCounterAll` to `Effect::PutCounter`, or removing the
/// keeper publish from `sacrifice_unchosen`, leaves every keeper at zero.
#[test]
fn promise_of_loyalty_marks_each_keeper_with_one_vow_counter() {
    let PromiseBoard {
        runner,
        p0_keeper,
        p0_doomed,
        p1_keeper,
        p1_doomed,
    } = resolve_promise_of_loyalty();

    assert_eq!(vow_counters(&runner, p0_keeper), 1);
    assert_eq!(vow_counters(&runner, p1_keeper), 1);
    // The sacrificed creatures are in the graveyard and were never marked.
    for sacrificed in [p0_doomed, p1_doomed] {
        assert_eq!(vow_counters(&runner, sacrificed), 0);
    }
}

/// V3 — CR 508.1c + CR 109.5: the keeper can't attack the player who resolved
/// the spell, and "you" means that player specifically.
///
/// The positive reach-guard in the same test is mandatory and non-vacuous: a
/// sibling creature with no vow counter, created after resolution so it is
/// provably outside the published keeper set, attacks the same player legally.
/// Without it the refusal below could be any combat restriction at all.
#[test]
fn promise_of_loyalty_keeper_cannot_attack_the_caster() {
    let PromiseBoard {
        mut runner,
        p1_keeper,
        ..
    } = resolve_promise_of_loyalty();

    let bystander = spawn_creature(runner.state_mut(), P1, "Unmarked Bystander");
    advance_to_declare_attackers_for(&mut runner, P1);

    assert!(
        runner
            .declare_attackers(&[(p1_keeper, AttackTarget::Player(P0))])
            .is_err(),
        "CR 508.1c: a keeper with a vow counter can't attack the spell's controller"
    );
    runner
        .declare_attackers(&[(bystander, AttackTarget::Player(P0))])
        .expect("a creature with no vow counter attacks the same player freely");
}

/// V3's hostile fixture — CR 115.10a: "Just because an object or player is
/// being affected by a spell or ability doesn't make that object or player a
/// target of that spell or ability." Nothing in this card's text says "target",
/// so a HEXPROOF creature is an ordinary member of its controller's keeper
/// pool: it appears in the seat's eligible list, may be chosen, takes the vow
/// counter, and carries the prohibition.
///
/// This is the property that rules `Effect::TargetOnly` out for this class. A
/// targeted lowering could not reach a hexproof creature at all, so reverting
/// the recognizer to one would leave this seat's hexproof creature un-marked —
/// or refuse the choice outright.
///
/// Two paired positives keep the row non-vacuous: the caster's own seat is
/// prompted and sacrifices its unchosen creature (so the instruction provably
/// ran), and an unmarked bystander created AFTER resolution attacks the caster
/// freely in the same window the hexproof keeper is refused.
#[test]
fn hexproof_creature_is_eligible_and_choosable_as_a_keeper() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let p0_keeper = scenario.add_creature(P0, "Caster Keeper", 2, 2).id();
    let p0_doomed = scenario.add_creature(P0, "Caster Doomed", 2, 2).id();
    // The opponent's pool is a hexproof creature and a plain one, so the seat
    // has a real choice to make and can make the hexproof one.
    let p1_hexproof = scenario
        .add_creature(P1, "Warded Keeper", 2, 2)
        .hexproof()
        .id();
    let p1_doomed = scenario.add_creature(P1, "Rival Doomed", 2, 2).id();
    stock_libraries(&mut scenario, &[P0, P1]);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Promise of Loyalty", false, PROMISE_OF_LOYALTY)
        .id();

    let mut runner = scenario.build();
    drop(runner.cast(spell).resolve());
    runner
        .act(GameAction::ChooseKeptPermanents {
            kept: vec![p0_keeper],
        })
        .expect("the caster keeps one creature");

    // CR 115.10a: hexproof does not remove the creature from the choice pool.
    let WaitingFor::KeepExactPermanentsChoice {
        player, eligible, ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the opponent must be prompted for its own keeper: {:?}",
            runner.state().waiting_for
        )
    };
    assert_eq!(player, P1);
    assert!(
        eligible.contains(&p1_hexproof),
        "a hexproof creature is chosen, not targeted, so it must be eligible: {eligible:?}"
    );
    runner
        .act(GameAction::ChooseKeptPermanents {
            kept: vec![p1_hexproof],
        })
        .expect("the opponent may keep its hexproof creature");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&p1_hexproof].zone,
        Zone::Battlefield,
        "the hexproof keeper survives"
    );
    for sacrificed in [p0_doomed, p1_doomed] {
        assert_eq!(
            runner.state().objects[&sacrificed].zone,
            Zone::Graveyard,
            "CR 701.21a: the unchosen creatures are still sacrificed on both seats"
        );
    }
    // CR 122.1: the counter reaches the hexproof keeper — the marking step does
    // not target either.
    assert_eq!(
        vow_counters(&runner, p1_hexproof),
        1,
        "the hexproof keeper must be marked like any other"
    );

    // CR 508.1c: and it carries the prohibition, with the paired positive in
    // the same declare-attackers window.
    let bystander = spawn_creature(runner.state_mut(), P1, "Unmarked Bystander");
    advance_to_declare_attackers_for(&mut runner, P1);
    assert!(
        runner
            .declare_attackers(&[(p1_hexproof, AttackTarget::Player(P0))])
            .is_err(),
        "the hexproof keeper is bound by the prohibition it received"
    );
    runner
        .declare_attackers(&[(bystander, AttackTarget::Player(P0))])
        .expect("a creature with no vow counter attacks the same player freely");
}

/// V4 — CR 611.2b: the duration is re-evaluated, so removing the last vow
/// counter lifts the restriction; removing one of two does not.
#[test]
fn promise_of_loyalty_keeper_attacks_after_vow_counter_removed() {
    // Partial removal first: two counters, remove one, restriction persists.
    {
        let PromiseBoard {
            mut runner,
            p1_keeper,
            ..
        } = resolve_promise_of_loyalty();
        *runner
            .state_mut()
            .objects
            .get_mut(&p1_keeper)
            .expect("keeper exists")
            .counters
            .entry(VOW())
            .or_insert(0) += 1;
        assert_eq!(vow_counters(&runner, p1_keeper), 2);
        runner
            .state_mut()
            .objects
            .get_mut(&p1_keeper)
            .expect("keeper exists")
            .counters
            .insert(VOW(), 1);
        advance_to_declare_attackers_for(&mut runner, P1);
        assert!(
            runner
                .declare_attackers(&[(p1_keeper, AttackTarget::Player(P0))])
                .is_err(),
            "one remaining vow counter still satisfies the for-as-long-as condition"
        );
    }

    // Full removal: the restriction retires.
    let PromiseBoard {
        mut runner,
        p1_keeper,
        ..
    } = resolve_promise_of_loyalty();
    runner
        .state_mut()
        .objects
        .get_mut(&p1_keeper)
        .expect("keeper exists")
        .counters
        .remove(&VOW());
    advance_to_declare_attackers_for(&mut runner, P1);
    runner
        .declare_attackers(&[(p1_keeper, AttackTarget::Player(P0))])
        .expect("CR 611.2b: with no vow counter left, the prohibition is gone");
}

/// V5 — CR 608.2c: "those creatures" names the set the keeper instruction
/// fixed. A vow counter placed later on a creature that was never nominated
/// does NOT bind it — the printed ruling this row exists for.
///
/// Paired positive: the actual keeper in the same game IS still refused, so a
/// blanket "no restriction installed" cannot green this row.
#[test]
fn vow_counter_on_unchosen_creature_does_not_restrict_it() {
    let PromiseBoard {
        mut runner,
        p1_keeper,
        ..
    } = resolve_promise_of_loyalty();

    let latecomer = spawn_creature(runner.state_mut(), P1, "Latecomer");
    runner
        .state_mut()
        .objects
        .get_mut(&latecomer)
        .expect("latecomer exists")
        .counters
        .insert(VOW(), 1);

    advance_to_declare_attackers_for(&mut runner, P1);
    assert!(
        runner
            .declare_attackers(&[(p1_keeper, AttackTarget::Player(P0))])
            .is_err(),
        "paired positive: the nominated keeper is still restricted"
    );
    runner
        .declare_attackers(&[(latecomer, AttackTarget::Player(P0))])
        .expect("a counter moved onto a never-nominated creature must not bind it");
}

/// V6 — CR 109.5: "you" is latched to the player who resolved the spell, and
/// stays latched through a control change. The keeper still cannot attack the
/// original caster after changing hands, and CAN attack a different opponent —
/// the paired positive that proves the prohibition is scoped rather than total.
#[test]
fn keeper_still_cannot_attack_original_caster_after_control_change() {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature(P0, "Caster Keeper", 2, 2);
    scenario.add_creature(P0, "Caster Doomed", 2, 2);
    let p1_keeper = scenario.add_creature(P1, "Rival Keeper", 2, 2).id();
    scenario.add_creature(P1, "Rival Doomed", 2, 2);
    scenario.add_creature(P2, "Third Seat Keeper", 2, 2);
    stock_libraries(&mut scenario, &[P0, P1, P2]);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Promise of Loyalty", false, PROMISE_OF_LOYALTY)
        .id();

    let mut runner = scenario.build();
    let p0_keeper = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].name == "Caster Keeper")
        .expect("the caster's keeper exists");
    let p2_keeper = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].name == "Third Seat Keeper")
        .expect("the third seat's keeper exists");
    drop(runner.cast(spell).resolve());
    for keeper in [p0_keeper, p1_keeper, p2_keeper] {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::KeepExactPermanentsChoice { .. }
        ) {
            runner
                .act(GameAction::ChooseKeptPermanents { kept: vec![keeper] })
                .expect("each seat keeps one creature");
        }
    }
    runner.advance_until_stack_empty();
    assert_eq!(vow_counters(&runner, p1_keeper), 1);

    // The keeper changes hands: control moves to the third seat. Both the base
    // and the effective controller move, because `evaluate_layers` recomputes
    // the effective controller from the base one (CR 613.1b, layer 2) — setting
    // only the effective field would be silently undone on the next layer pass
    // and every assertion below would then be about the wrong seat.
    {
        let keeper = runner
            .state_mut()
            .objects
            .get_mut(&p1_keeper)
            .expect("keeper exists");
        keeper.base_controller = Some(P2);
        keeper.controller = P2;
    }
    runner.state_mut().layers_dirty.mark_full();

    // Advance to the new controller's combat.
    advance_to_declare_attackers_for(&mut runner, P2);

    // Non-vacuous: the refusal must be the attack restriction, not a control or
    // timing error that any illegal declaration would also produce. The paired
    // legal declaration on the very next line, with the SAME attacker in the
    // SAME window, is what rules that class of false pass out.
    let refusal = runner
        .declare_attackers(&[(p1_keeper, AttackTarget::Player(P0))])
        .expect_err("CR 109.5: 'you' stays latched to the original caster across a control change");
    assert!(
        !format!("{refusal:?}").contains("not controlled by the active player"),
        "the refusal must come from the prohibition, not from a control mismatch: {refusal:?}"
    );
    runner
        .declare_attackers(&[(p1_keeper, AttackTarget::Player(P1))])
        .expect("the prohibition defends the caster only, not every player");
}

/// V7 — the polarity-and-cardinality inversion. At BASE_SHA these two cards
/// lowered to `Effect::TargetOnly` followed by a sacrifice of the TRACKED set —
/// i.e. they sacrificed the keeper, and dropped the printed count. Regenerate
/// that baseline with
/// `jq -c '.["razia'\''s purification"].abilities[0].effect.type' client/public/card-data.json`.
#[test]
fn razias_purification_sacrifices_the_unchosen_not_the_keeper() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let kept: Vec<ObjectId> = (0..3)
        .map(|i| scenario.add_creature(P0, &format!("Kept {i}"), 1, 1).id())
        .collect();
    let doomed = scenario.add_creature(P0, "Doomed", 1, 1).id();
    let rival_kept: Vec<ObjectId> = (0..3)
        .map(|i| {
            scenario
                .add_creature(P1, &format!("Rival Kept {i}"), 1, 1)
                .id()
        })
        .collect();
    let rival_doomed = scenario.add_creature(P1, "Rival Doomed", 1, 1).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Razia's Purification", false, RAZIAS_PURIFICATION)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::KeepExactPermanentsChoice {
                required_count: 3,
                ..
            }
        ),
        "the PRINTED three must reach the prompt, not a dropped-to-one default: {:?}",
        outcome.final_waiting_for()
    );
    drop(outcome);
    for seat_keepers in [&kept, &rival_kept] {
        runner
            .act(GameAction::ChooseKeptPermanents {
                kept: seat_keepers.to_vec(),
            })
            .expect("each seat keeps exactly three permanents");
    }
    runner.advance_until_stack_empty();

    for survivor in kept.iter().chain(rival_kept.iter()) {
        assert_eq!(
            runner.state().objects[survivor].zone,
            Zone::Battlefield,
            "the CHOSEN permanents survive"
        );
    }
    for sacrificed in [doomed, rival_doomed] {
        assert_eq!(
            runner.state().objects[&sacrificed].zone,
            Zone::Graveyard,
            "the UNCHOSEN permanents are the ones sacrificed"
        );
    }
}

/// V7's hostile fixture — CR 609.3: "If an effect attempts to do something
/// impossible, it does only as much as possible." A seat that controls FEWER
/// permanents than the printed count keeps every one of them and sacrifices
/// nothing, and is never prompted — `step_exact_count`'s
/// `eligible.len() <= count` auto-keep arm.
///
/// `promise_of_loyalty_auto_keeps_a_seat_that_cannot_choose` reaches the same
/// arm only at the degenerate count of one, where "fewer than the count" and
/// "nothing to choose between" are the same board. Razia's printed three
/// separates them: this seat's two permanents are a genuine multi-permanent
/// pool that still cannot satisfy the instruction.
///
/// Positive reach-guard in the same game: the caster's four-permanent seat IS
/// prompted for the printed three and does lose its unchosen permanent, so the
/// clamped seat's survival is not the spell failing to resolve.
#[test]
fn razias_purification_clamps_a_seat_with_fewer_permanents_than_the_count() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The caster can satisfy the printed three and has one to spare.
    let caster_pool: Vec<ObjectId> = (0..4)
        .map(|i| scenario.add_creature(P0, &format!("Caster {i}"), 1, 1).id())
        .collect();
    // The opponent controls two permanents — more than one, fewer than three.
    let clamped_pool: Vec<ObjectId> = (0..2)
        .map(|i| {
            scenario
                .add_creature(P1, &format!("Clamped {i}"), 1, 1)
                .id()
        })
        .collect();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Razia's Purification", false, RAZIAS_PURIFICATION)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::KeepExactPermanentsChoice {
                player: P0,
                required_count: 3,
                ..
            }
        ),
        "the seat that CAN satisfy the printed three must be prompted for three: {:?}",
        outcome.final_waiting_for()
    );
    drop(outcome);
    runner
        .act(GameAction::ChooseKeptPermanents {
            kept: caster_pool[..3].to_vec(),
        })
        .expect("the caster keeps exactly three permanents");

    // CR 609.3: the short seat is never asked — there is no choice to make, and
    // asking would demand a three-permanent answer it cannot give.
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::KeepExactPermanentsChoice { .. }
        ),
        "the seat with fewer permanents than the count must not be prompted: {:?}",
        runner.state().waiting_for
    );
    runner.advance_until_stack_empty();

    for kept in &clamped_pool {
        assert_eq!(
            runner.state().objects[kept].zone,
            Zone::Battlefield,
            "CR 609.3: both of the short seat's permanents are kept"
        );
    }
    for kept in &caster_pool[..3] {
        assert_eq!(runner.state().objects[kept].zone, Zone::Battlefield);
    }
    assert_eq!(
        runner.state().objects[&caster_pool[3]].zone,
        Zone::Graveyard,
        "the instruction provably ran: the caster's unchosen permanent is sacrificed"
    );
}

/// V7's sibling on the Or-domain: Single Combat's keeper may be a creature OR a
/// planeswalker, and its trailing casting restriction must survive the splice
/// (T1). The spell is driven through the real cast-and-resolve pipeline, and the
/// restriction it installs is read back off `state.restrictions`.
///
/// Asserted on the installed restriction rather than on a refused cast.
/// `casting.rs::is_blocked_by_cant_cast_spells_for` returns `false` for a
/// `RestrictionExpiry::UntilEndOfNextTurnOf` expiry, and the assertion below
/// shows that is the expiry this clause lowers to, so no cast attempted in the
/// creating turn can observe this ban. A cast-refusal assertion in this window
/// cannot discriminate: with the recognizer's remainder splice disabled the
/// parse carries no `AddRestriction` and `state.restrictions` is `[]`, yet a
/// cast-refusal assertion here still passed — so that refusal did not come from
/// the prohibition.
#[test]
fn single_combat_keeps_one_and_installs_the_creature_cast_ban() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let keeper = scenario.add_creature(P0, "Chosen Champion", 2, 2).id();
    let doomed = scenario.add_creature(P0, "Doomed Champion", 2, 2).id();
    let rival_keeper = scenario.add_creature(P1, "Rival Champion", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Single Combat", false, SINGLE_COMBAT)
        .id();

    let mut runner = scenario.build();
    drop(runner.cast(spell).resolve());
    for kept in [keeper, rival_keeper] {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::KeepExactPermanentsChoice { .. }
        ) {
            runner
                .act(GameAction::ChooseKeptPermanents { kept: vec![kept] })
                .expect("each seat keeps one creature or planeswalker");
        }
    }
    runner.advance_until_stack_empty();

    assert_eq!(runner.state().objects[&keeper].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&doomed].zone, Zone::Graveyard);

    // T1: the trailing sentence survived the remainder splice and reached the
    // resolver. With the splice disabled `state.restrictions` is empty here.
    let installed: Vec<&GameRestriction> = runner
        .state()
        .restrictions
        .iter()
        .filter(|restriction| {
            matches!(
                restriction,
                GameRestriction::ProhibitActivity {
                    activity: ProhibitedActivity::CastSpells { .. },
                    ..
                }
            )
        })
        .collect();
    assert_eq!(
        installed.len(),
        1,
        "exactly one cast prohibition — the splice must install the clause once, \
         not zero times and not twice: {:?}",
        runner.state().restrictions
    );
    let GameRestriction::ProhibitActivity {
        affected_players,
        expiry,
        activity: ProhibitedActivity::CastSpells { spell_filter },
        ..
    } = installed[0]
    else {
        unreachable!("filtered above")
    };
    // The printed subject is "Players" — every seat, including the caster's own,
    // rather than the caster's opponents.
    assert_eq!(*affected_players, RestrictionPlayerScope::AllPlayers);
    // "until the end of your next turn" is anchored on the spell's controller.
    assert!(
        matches!(expiry, RestrictionExpiry::UntilEndOfNextTurnOf { player } if *player == P0),
        "the expiry must be anchored on the caster: {expiry:?}"
    );
    // The printed ban names two card types, not every spell. A `None` filter
    // here is read as a total ban: `is_blocked_by_cant_cast_spells_for` matches
    // its `None` arm unconditionally.
    assert!(
        spell_filter.is_some(),
        "the prohibition must carry the creature-or-planeswalker spell filter"
    );
}

/// V8 — the `" and "` connector no longer drops the disposal tail. Limited
/// Resources' enters-the-battlefield trigger keeps five lands per seat.
#[test]
fn limited_resources_sacrifices_lands_beyond_five() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let lands: Vec<ObjectId> = (0..6)
        .map(|_| scenario.add_basic_land(P0, engine::types::mana::ManaColor::White))
        .collect();
    // Cast it, so the enters-the-battlefield trigger genuinely fires: a
    // permanent placed directly on the battlefield by the scenario builder
    // never generates its own ETB event.
    let enchantment = scenario
        .add_spell_to_hand_from_oracle(P0, "Limited Resources", false, LIMITED_RESOURCES)
        .as_enchantment()
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(enchantment).resolve();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::KeepExactPermanentsChoice {
                required_count: 5,
                ..
            }
        ),
        "the ' and ' connector must still deliver the disposal tail, with the \
         printed five: {:?}",
        outcome.final_waiting_for()
    );
    drop(outcome);
    runner
        .act(GameAction::ChooseKeptPermanents {
            kept: lands[..5].to_vec(),
        })
        .expect("the seat keeps exactly five lands");
    runner.advance_until_stack_empty();

    let surviving = lands
        .iter()
        .filter(|id| runner.state().objects[id].zone == Zone::Battlefield)
        .count();
    assert_eq!(
        surviving, 5,
        "the printed five lands survive and the rest are sacrificed"
    );
}

/// V9's runtime half — "each opponent" binds to the opponents only: the
/// controller's own board is untouched.
///
/// SYNTHETIC CARD, deliberately. The printed member of this axis is No One Will
/// Hear Your Cries, whose entry point is an Archenemy `SetInMotion` trigger the
/// scenario runner has no driver for. The clause text below is byte-identical
/// to that card's trigger body, and it reaches the SAME recognizer output; the
/// printed card's own lowering (`player_scope: Opponent`, and a keeper filter
/// with NO controller — the wrong-seat defect this change fixes) is asserted on
/// the verbatim card in
/// `parser::oracle_effect::tests::keeper_dispose_trigger_entry_points_scope_to_the_printed_players`.
#[test]
fn each_opponent_keeper_choice_leaves_the_controllers_board_untouched() {
    const EACH_OPPONENT_CLAUSE: &str =
        "Each opponent chooses a creature they control, then sacrifices the rest.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let controller_a = scenario.add_creature(P0, "Controller A", 2, 2).id();
    let controller_b = scenario.add_creature(P0, "Controller B", 2, 2).id();
    let rival_keeper = scenario.add_creature(P1, "Rival Keeper", 2, 2).id();
    let rival_doomed = scenario.add_creature(P1, "Rival Doomed", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic Each-Opponent Keeper",
            false,
            EACH_OPPONENT_CLAUSE,
        )
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::KeepExactPermanentsChoice {
                player: P1,
                required_count: 1,
                ..
            }
        ),
        "only the opponent may be prompted: {:?}",
        outcome.final_waiting_for()
    );
    drop(outcome);
    runner
        .act(GameAction::ChooseKeptPermanents {
            kept: vec![rival_keeper],
        })
        .expect("the opponent keeps one creature they control");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&rival_doomed].zone,
        Zone::Graveyard,
        "the opponent's unchosen creature is sacrificed"
    );
    assert_eq!(
        runner.state().objects[&rival_keeper].zone,
        Zone::Battlefield
    );
    for untouched in [controller_a, controller_b] {
        assert_eq!(
            runner.state().objects[&untouched].zone,
            Zone::Battlefield,
            "PlayerFilter::Opponent must exclude the controller's own board"
        );
    }
}

/// T2 — Planetary Annihilation's trailing damage survives the remainder splice
/// and lands AFTER the sacrifice, on the creatures that are still there.
#[test]
fn planetary_annihilation_deals_six_to_each_surviving_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let lands: Vec<ObjectId> = (0..7)
        .map(|_| scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green))
        .collect();
    let survivor = scenario.add_creature(P1, "Tough Survivor", 8, 8).id();
    let casualty = scenario.add_creature(P1, "Fragile Casualty", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Planetary Annihilation", false, PLANETARY_ANNIHILATION)
        .id();

    let mut runner = scenario.build();
    drop(runner.cast(spell).resolve());
    if matches!(
        runner.state().waiting_for,
        WaitingFor::KeepExactPermanentsChoice { .. }
    ) {
        runner
            .act(GameAction::ChooseKeptPermanents {
                kept: lands[..6].to_vec(),
            })
            .expect("the seat keeps exactly six lands");
    }
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&casualty].zone,
        Zone::Graveyard,
        "the trailing damage must still be dealt"
    );
    assert_eq!(
        runner.state().objects[&survivor].damage_marked,
        6,
        "each creature takes the printed six damage"
    );
}
