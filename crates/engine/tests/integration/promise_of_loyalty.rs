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
    AbilityDefinition, AbilityKind, Effect, EffectKind, GameRestriction, ProhibitedActivity,
    ReplacementDefinition, RestrictionExpiry, RestrictionPlayerScope, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::{EtbTapState, Zone};

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
/// Winding Constrictor, verbatim. Regenerate with
/// `jq -r '.["winding constrictor"].oracle_text' client/public/card-data.json`.
/// A real counter-doubling multiplier, controlled by the same player as the
/// vow-counter recipient, that is itself among the sacrificed permanents —
/// the exact shape CR 608.2c + CR 614.1 require the keeper mark to be placed
/// BEFORE the sacrifice for.
const WINDING_CONSTRICTOR: &str = "If one or more counters would be put on an artifact or creature you control, that many plus one of each of those kinds of counters are put on that permanent instead.\nIf you would get one or more counters, you get that many plus one of each of those kinds of counters instead.";
/// Doubling Season, verbatim. A second, non-commuting counter multiplier,
/// used together with Winding Constrictor to force a CR 616.1
/// replacement-ordering choice on the printed vow counter. Recipient-scoped
/// ("a permanent you control") rather than actor-scoped, so it applies
/// correctly under the same `controller: You` filter Winding Constrictor uses
/// regardless of which player's `Effect::ChooseAndSacrificeRest` instance
/// actually performs the placement — unlike an actor-scoped multiplier
/// (Vorinclex's "if you would put"), which resolves its "you" against
/// `add_object_counters_then`'s single per-batch `actor` parameter, a
/// pre-existing convention `resolve_add_all` shares (both stamp the whole
/// ability's controller, not each recipient's own controller) and which is
/// therefore out of scope for this fix.
const DOUBLING_SEASON_ORACLE: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.\nIf an effect would put one or more counters on a permanent you control, it puts twice that many of those counters on that permanent instead.";
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
/// counter lifts the restriction; removing one of two does not. The full-removal
/// case also cross-checks that the OTHER seat's still-marked keeper is
/// unaffected, ruling out a shared expiry across the whole printed instruction.
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
        p0_keeper,
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

    // Cross-seat control: the caster's own keeper ("Caster Keeper") still carries
    // its own vow counter, untouched by p1_keeper's removal above. CR 506.2: an
    // attacking creature must be controlled by the active player, and the
    // nonactive player is the defending player, so a creature can never legally
    // attack its own controller. Moving p0_keeper to P1's control (the
    // active player for this window) is what makes declaring it against P0
    // legal at all — `game/combat.rs::attacker_can_attack_target` refuses any
    // `AttackTarget::Player` on the active team, and P0 is never on that team —
    // so the vow restriction's effect becomes observable in the SAME window
    // p1_keeper's restriction just expired. A regression that gave the whole
    // printed instruction one shared expiry (instead of a separate duration
    // check per marked creature) would wrongly lift this restriction too.
    //
    // Mutating `transient_duration_holds`'s `None` arm to collapse every
    // sibling TCE under one `source_id` onto the max `ObjectId` (one shared
    // expiry for the whole instruction, since `register_transient_effect`'s
    // `ParentTarget` arm gives every per-keeper TCE that same `source_id`)
    // reddens the `expect_err` below: the attack against P0 succeeds instead
    // of erroring.
    {
        let keeper = runner
            .state_mut()
            .objects
            .get_mut(&p0_keeper)
            .expect("keeper exists");
        keeper.base_controller = Some(P1);
        keeper.controller = P1;
    }
    runner.state_mut().layers_dirty.mark_full();

    advance_to_declare_attackers_for(&mut runner, P1);
    let refusal = runner
        .declare_attackers(&[(p0_keeper, AttackTarget::Player(P0))])
        .expect_err("the caster's own keeper still has its own vow counter");
    assert!(
        format!("{refusal:?}").contains("CR 508.1c/d attack restriction"),
        "the refusal must come from the per-target restriction loop, not the \
         creature-level checks in validate_attackers_with_cap: {refusal:?}"
    );
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
/// i.e. they sacrificed the keeper, and dropped the printed count.
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

// ---------------------------------------------------------------------------
// CR 608.2c — the keeper mark precedes the sacrifice.
// ---------------------------------------------------------------------------

/// CR 616.1: a redirect replacement forcing a CR 616.1 ordering choice when
/// paired with another applicable replacement on the same object's departure.
/// Mirrors `cost_zone_pipeline.rs`'s `redirect_self_moved_to` — no shared
/// helper exists between the two integration test files.
fn redirect_self_moved_to(destination: Zone, redirected_to: Zone) -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(destination)
        .valid_card(TargetFilter::SelfRef)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                destination: redirected_to,
                origin: None,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        ))
}

/// V-F1a — CR 608.2c + CR 614.1: the vow counter is placed BEFORE the
/// sacrifice. P0 keeps a vanilla 2/2 while a REAL Winding Constrictor
/// (verbatim Oracle) is among the sacrificed creatures; because the mark now
/// precedes the sweep, the Constrictor is still on the battlefield when the
/// counter lands and its "plus one" replacement applies, so the keeper ends
/// up with TWO vow counters instead of the printed one.
///
/// Reverting the ordering (placing the counters after the sweep, as
/// `PutCounterAll` used to) reddens this to 1 — the Constrictor is already
/// gone by the time the counter would land.
///
/// P1's keeper stays at exactly ONE: Winding Constrictor's replacement is
/// `controller: You`-scoped to P0, so it never reaches P1's board.
#[test]
fn promise_of_loyalty_marks_the_keeper_before_the_sacrifice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let p0_keeper = scenario.add_creature(P0, "Vanilla Keeper", 2, 2).id();
    let p0_constrictor = scenario
        .add_creature_from_oracle(P0, "Winding Constrictor", 2, 3, WINDING_CONSTRICTOR)
        .id();
    let p1_keeper = scenario.add_creature(P1, "Rival Keeper", 2, 2).id();
    let p1_doomed = scenario.add_creature(P1, "Rival Doomed", 2, 2).id();
    stock_libraries(&mut scenario, &[P0, P1]);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Promise of Loyalty", false, PROMISE_OF_LOYALTY)
        .id();

    let mut runner = scenario.build();
    drop(runner.cast(spell).resolve());
    for (player, keeper) in [(P0, p0_keeper), (P1, p1_keeper)] {
        runner
            .act(GameAction::ChooseKeptPermanents { kept: vec![keeper] })
            .unwrap_or_else(|e| panic!("{player:?} keeps its own creature: {e:?}"));
    }
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&p0_constrictor].zone,
        Zone::Graveyard,
        "the multiplier itself must be among the sacrificed permanents"
    );
    assert_eq!(
        vow_counters(&runner, p0_keeper),
        2,
        "CR 608.2c + CR 614.1: the mark precedes the sacrifice, so Winding \
         Constrictor's replacement is still live when the counter is placed"
    );
    assert_eq!(
        vow_counters(&runner, p1_keeper),
        1,
        "Winding Constrictor's replacement is controller-scoped to P0 and \
         must not reach P1's board"
    );
    assert_eq!(
        runner.state().objects[&p1_doomed].zone,
        Zone::Graveyard,
        "P1's unchosen creature is still swept"
    );
}

/// Paired positive control for V-F1a: when P0 keeps the Constrictor ITSELF,
/// the count is 2 whether the fix is present or not (the Constrictor never
/// left the battlefield), so this row must stay green across the revert and
/// is what proves `promise_of_loyalty_marks_the_keeper_before_the_sacrifice`
/// is measuring the ordering, not something else about the multiplier.
#[test]
fn promise_of_loyalty_marks_a_keeper_that_is_its_own_multiplier() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let p0_constrictor = scenario
        .add_creature_from_oracle(P0, "Winding Constrictor", 2, 3, WINDING_CONSTRICTOR)
        .id();
    let p0_doomed = scenario.add_creature(P0, "Caster Doomed", 2, 2).id();
    stock_libraries(&mut scenario, &[P0]);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Promise of Loyalty", false, PROMISE_OF_LOYALTY)
        .id();

    let mut runner = scenario.build();
    drop(runner.cast(spell).resolve());
    runner
        .act(GameAction::ChooseKeptPermanents {
            kept: vec![p0_constrictor],
        })
        .expect("P0 keeps the Constrictor itself");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&p0_constrictor].zone,
        Zone::Battlefield
    );
    assert_eq!(runner.state().objects[&p0_doomed].zone, Zone::Graveyard);
    assert_eq!(
        vow_counters(&runner, p0_constrictor),
        2,
        "the kept multiplier doubles-plus-one its own vow counter regardless \
         of ordering — the positive control for V-F1a"
    );
}

/// V-F1c — CR 616.1: a replacement-ordering choice raised INSIDE the keeper
/// marking step still resumes into the sacrifice. Two non-commuting
/// multipliers (Winding Constrictor's "plus one", Doubling Season's "twice")
/// both apply to the printed vow counter, so the marking step itself must
/// pause on `WaitingFor::ReplacementChoice` BEFORE either non-keeper leaves
/// the battlefield; after answering, both non-keepers are in the graveyard
/// and the keeper holds the chosen arithmetic.
///
/// The one-multiplier board (`promise_of_loyalty_marks_the_keeper_before_the_sacrifice`)
/// pauses not at all — the paired reach-guard proving the pause here is
/// genuinely load-bearing rather than a property of every fixture.
#[test]
fn promise_of_loyalty_keeper_mark_survives_a_replacement_order_choice() {
    for (index, expected_vow_counters) in [(0usize, 3u32), (1usize, 4u32)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);

        let keeper = scenario.add_creature(P0, "Vanilla Keeper", 2, 2).id();
        let constrictor = scenario
            .add_creature_from_oracle(P0, "Winding Constrictor", 2, 3, WINDING_CONSTRICTOR)
            .id();
        let doubling_season = scenario
            .add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING_SEASON_ORACLE)
            .id();
        stock_libraries(&mut scenario, &[P0]);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Promise of Loyalty", false, PROMISE_OF_LOYALTY)
            .id();

        let mut runner = scenario.build();
        drop(runner.cast(spell).resolve());
        runner
            .act(GameAction::ChooseKeptPermanents { kept: vec![keeper] })
            .expect("P0 keeps the vanilla creature");

        // CR 616.1: both multipliers are still on the battlefield when the
        // ordering choice for the vow counter opens.
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ),
            "the keeper mark's two competing multipliers must raise a CR \
             616.1 ordering choice, got {:?}",
            runner.state().waiting_for
        );
        for multiplier in [constrictor, doubling_season] {
            assert_eq!(
                runner.state().objects[&multiplier].zone,
                Zone::Battlefield,
                "no non-keeper may leave the battlefield before the \
                 ordering choice is answered"
            );
        }

        runner
            .act(GameAction::ChooseReplacement { index })
            .expect("ordering the two vow-counter multipliers must be legal");
        runner.advance_until_stack_empty();

        assert_eq!(
            runner.state().objects[&constrictor].zone,
            Zone::Graveyard,
            "the sacrifice must still run after the ordering choice resumes"
        );
        // Doubling Season is an enchantment: Promise of Loyalty's sacrifice
        // sweep is creature-scoped and never reaches it.
        assert_eq!(
            runner.state().objects[&doubling_season].zone,
            Zone::Battlefield
        );
        assert_eq!(
            vow_counters(&runner, keeper),
            expected_vow_counters,
            "index {index}: the keeper's count must reflect the CHOSEN order"
        );
    }
}

/// Three boards shared by `V-F1g` / `MO-2` / `MO-3` (unpaused, single
/// counter-placement pause, and a double pause that also pauses the sacrifice
/// sweep itself) and by `V-F1h-E` / `MO-1`.
///
/// P0 casts with an EMPTY board (auto-keeps nothing, no prompt — CR 609.3)
/// so the fixture stays single-seat for combat: P1 is the seat that
/// nominates a keeper, so "the keeper can't attack YOU (=P0, the caster)" is
/// a real cross-seat restriction rather than a creature refusing to attack
/// its own controller.
///
/// `double_pause`: when `true`, one of P1's non-keepers additionally carries
/// two competing move-redirect replacements, so answering the counter-pause's
/// ordering choice runs straight into a SECOND `WaitingFor::ReplacementChoice`
/// for that creature's own departure — the double-pause board.
struct PauseBoard {
    runner: GameRunner,
    keeper: ObjectId,
    bystander_a: ObjectId,
    bystander_b: ObjectId,
    /// Every `GameEvent` emitted by the cast and by each subsequent
    /// `GameAction` this builder submitted to reach the settled board — the
    /// union `MO-1` / `V-F1h-E` count over.
    events: Vec<GameEvent>,
}

fn build_pause_board(multipliers: bool, double_pause: bool) -> PauseBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P0 casts with no creatures — its own APNAP keeper step auto-resolves
    // with nothing kept (CR 609.3) and never prompts.
    let keeper = scenario.add_creature(P1, "Vanilla Keeper", 2, 2).id();
    let bystander_a = if double_pause {
        scenario
            .add_creature(P1, "Doubly Redirected Bystander", 2, 2)
            .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Exile))
            .with_replacement_definition(redirect_self_moved_to(Zone::Graveyard, Zone::Hand))
            .id()
    } else {
        scenario.add_creature(P1, "Plain Bystander A", 2, 2).id()
    };
    let bystander_b = scenario.add_creature(P1, "Plain Bystander B", 2, 2).id();
    if multipliers {
        scenario.add_creature_from_oracle(P1, "Winding Constrictor", 2, 3, WINDING_CONSTRICTOR);
        scenario.add_enchantment_from_oracle(P1, "Doubling Season", DOUBLING_SEASON_ORACLE);
    }
    stock_libraries(&mut scenario, &[P0, P1]);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Promise of Loyalty", false, PROMISE_OF_LOYALTY)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();
    let mut events: Vec<GameEvent> = outcome.events().to_vec();
    drop(outcome);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::KeepExactPermanentsChoice { .. }
        ),
        "P1 must be prompted directly — P0's empty board must not prompt: {:?}",
        runner.state().waiting_for
    );
    let result = runner
        .act(GameAction::ChooseKeptPermanents { kept: vec![keeper] })
        .expect("P1 keeps the vanilla creature");
    events.extend(result.events);

    if multipliers {
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ),
            "the counter-placement pause must be raised: {:?}",
            runner.state().waiting_for
        );
        // MO-4: `chain_tracked_set_id` must already be bound (by
        // `publish_fresh_tracked_set`, ahead of the counter step) at the
        // moment of the pause, and must not change across it — a rebind
        // would invalidate the `ParentTarget` binding `V-F1g` depends on.
        // MEASURED (temporary instrumentation, run and reverted byte-exact):
        // `Some(TrackedSetId(_))` immediately after `publish_fresh_tracked_set`,
        // at this pause, and again inside `continue_player_scope_sacrifice` —
        // the SAME id all three times; the continuation never re-publishes.
        let bound_before_pause = runner.state().chain_tracked_set_id;
        assert!(
            bound_before_pause.is_some(),
            "chain_tracked_set_id must already be bound at the pause"
        );
        let result = runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("ordering the counter multipliers must be legal");
        events.extend(result.events);
    }

    if double_pause {
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ),
            "the sacrifice's own departure pause must be raised: {:?}",
            runner.state().waiting_for
        );
        let result = runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("ordering the bystander's competing redirects must be legal");
        events.extend(result.events);
    }

    PauseBoard {
        runner,
        keeper,
        bystander_a,
        bystander_b,
        events,
    }
}

/// V-F1g + MO-2 + MO-3 — sentence two still binds to the keepers after the
/// anaphor's label flip (`ChooseAndSacrificeRest` is not a member of
/// `publishes_tracked_set_from_resolution`, so the grant's `affected` is
/// `ParentTarget`, not `TrackedSet`), on all three pause shapes.
///
/// Reverting the funnel's publish-before-counters order makes
/// `state.chain_tracked_set_id` wrong or unset at install time, so the
/// `ParentTarget` arm's `unwrap_or_default()` installs on an empty set —
/// nothing is refused, and the keeper attacks freely. RED on that mutation.
///
/// MO-2, MEASURED on the double-pause board (temporary instrumentation, run
/// and reverted byte-exact): the sacrifice stage's own `Completed` arm in
/// `game/engine_replacement.rs`'s `ReplacementResult::Execute` arm is what
/// calls `effects::drain_pending_continuation` and installs sentence two —
/// the general continuation stage's OWN call to the same function never
/// fires for this resolution, because the sacrifice-stage arm already
/// drained the continuation by the time control would reach it.
#[test]
fn promise_of_loyalty_sentence_two_binds_to_the_keepers_on_every_pause_path() {
    for (label, multipliers, double_pause) in [
        ("unpaused", false, false),
        ("single-pause", true, false),
        ("double-pause", true, true),
    ] {
        let PauseBoard {
            mut runner,
            keeper,
            bystander_a,
            bystander_b,
            events: _,
        } = build_pause_board(multipliers, double_pause);
        runner.advance_until_stack_empty();

        assert_eq!(
            runner.state().objects[&bystander_b].zone,
            Zone::Graveyard,
            "{label}: every ordinary non-keeper must still be swept"
        );
        // `bystander_a` carries two competing move-redirects in the
        // `double_pause` board (that is what raises its own second
        // `WaitingFor::ReplacementChoice`), so ITS zone is whichever
        // redirect answering `index: 0` chose (MEASURED: Exile) rather than
        // Graveyard — the sacrifice still ran, it was just redirected away.
        assert_eq!(
            runner.state().objects[&bystander_a].zone,
            if double_pause {
                Zone::Exile
            } else {
                Zone::Graveyard
            },
            "{label}: bystander_a must still have left the battlefield"
        );
        assert_eq!(
            vow_counters(&runner, keeper),
            // MEASURED on THIS fixture (index 0, answered in `build_pause_board`):
            // the resulting count on a base of 1 vow counter is 4. The
            // candidate-order → arithmetic mapping is a property of this
            // specific board (it differs from
            // `promise_of_loyalty_keeper_mark_survives_a_replacement_order_choice`'s
            // board, which measures 3 at index 0) — this row does not assume
            // the two fixtures share a mapping, only asserts what THIS one
            // measures.
            if multipliers { 4 } else { 1 },
            "{label}: the keeper must still be marked"
        );

        let unmarked = spawn_creature(runner.state_mut(), P1, "Unmarked Bystander");
        advance_to_declare_attackers_for(&mut runner, P1);
        assert!(
            runner
                .declare_attackers(&[(keeper, AttackTarget::Player(P0))])
                .is_err(),
            "{label}: the keeper must still be refused the attack on the caster \
             after the label flip"
        );
        runner
            .declare_attackers(&[(unmarked, AttackTarget::Player(P0))])
            .unwrap_or_else(|e| {
                panic!(
                    "{label}: a creature with no vow counter must attack the caster freely: {e:?}"
                )
            });
    }
}

/// V-F1h-E + MO-1 — the counter queue's own completion does not add an
/// OBSERVABLE THIRD `EffectResolved{ChooseAndSacrificeRest}` push beyond the
/// pre-existing baseline, on every pause shape.
///
/// MEASURED: a real interactive exact-keeper choice emits TWO such events
/// even at BASE_SHA — `step_exact_count` pushes one when it first raises
/// `WaitingFor::KeepExactPermanentsChoice`, and
/// `perform_player_scope_sacrifices`'s completion tail pushes a second when
/// the sacrifice actually finishes. BASELINE = 2 on
/// every board, including "unpaused": `add_object_counters_then`'s inline
/// path never constructs a completion frame at all, so neither mode is even
/// consulted there.
///
/// `add_object_counters_then`'s `Suppress` mode is what keeps the
/// PAUSED boards at that same baseline instead of adding a THIRD push for the
/// counter queue's own completion. Positive control, run and reverted byte-
/// exact: flipping `Suppress` to `Emit` in `counters.rs::add_object_counters_then`
/// measures 2 / 3 / 3 (unpaused / single-pause / double-pause) — proving (a)
/// the instrument moves, and (b) the extra push happens ONCE per
/// `add_object_counters_then` call regardless of how many NESTED pauses that
/// one post-action itself takes (double-pause does not compound to 4).
fn count_choose_and_sacrifice_resolved(events: &[GameEvent], source_id: ObjectId) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::ChooseAndSacrificeRest,
                    source_id: event_source,
                    ..
                } if *event_source == source_id
            )
        })
        .count()
}

#[test]
fn promise_of_loyalty_adds_no_extra_effect_resolved_on_any_pause_path() {
    for (label, multipliers, double_pause) in [
        ("unpaused", false, false),
        ("single-pause", true, false),
        ("double-pause", true, true),
    ] {
        let PauseBoard {
            mut runner,
            keeper: _,
            bystander_a: _,
            bystander_b: _,
            events: mut all_events,
        } = build_pause_board(multipliers, double_pause);

        while !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            let result = runner
                .act(GameAction::PassPriority)
                .unwrap_or_else(|e| panic!("{label}: could not settle the stack: {e:?}"));
            all_events.extend(result.events);
        }

        // Reach guard: at least one OTHER `EffectResolved` kind exists in the
        // same stream, so the filter below is not vacuously matching nothing.
        assert!(
            all_events.iter().any(|event| matches!(
                event,
                GameEvent::EffectResolved { kind, .. } if *kind != EffectKind::ChooseAndSacrificeRest
            )),
            "{label}: reach guard — an unrelated EffectResolved kind must be present"
        );

        let spell_source = all_events
            .iter()
            .find_map(|event| match event {
                GameEvent::EffectResolved {
                    kind: EffectKind::ChooseAndSacrificeRest,
                    source_id,
                    ..
                } => Some(*source_id),
                _ => None,
            })
            .unwrap_or_else(|| {
                panic!("{label}: at least one EffectResolved{{ChooseAndSacrificeRest}} must exist")
            });

        assert_eq!(
            count_choose_and_sacrifice_resolved(&all_events, spell_source),
            2,
            "{label}: the counter queue's completion must not add a third \
             EffectResolved{{ChooseAndSacrificeRest}} beyond the pre-existing \
             two-push baseline"
        );
    }
}
