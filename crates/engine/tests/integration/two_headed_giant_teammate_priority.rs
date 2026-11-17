//! CR 805.5a: a player may act when their TEAM has priority, not only the
//! team's representative. In Two-Headed Giant the engine parks priority on the
//! team representative (P0 / P2); the teammate (P1 / P3) must still be able to
//! play lands, cast spells, and activate abilities on the team's turn.

use engine::ai_support;
use engine::game::casting::can_foretell_card;
use engine::game::combat::{AttackerInfo, CombatState};
use engine::game::engine as game_engine;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Effect, ResolvedAbility};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{StackEntry, StackEntryKind, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::PlayerId;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

fn team_turn_board() -> GameScenario {
    let mut sc = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
    sc.at_phase(Phase::PreCombatMain);
    sc
}

/// The teammate sees their own legal actions while the representative holds
/// the team's priority window.
#[test]
fn teammate_has_legal_actions_on_team_priority() {
    let mut sc = team_turn_board();
    let land = sc.add_land_to_hand(P1, "Forest").id();
    let mut runner = sc.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    let (actions, _, _) = ai_support::legal_actions_for_viewer(runner.state(), P1);
    assert!(
        actions.iter().any(|a| matches!(
            a,
            GameAction::PlayLand { object_id, .. } if *object_id == land
        )),
        "P1 must be offered their own land on the team's turn, got {actions:?}"
    );
}

/// The teammate's land play is accepted through the authenticated boundary
/// while the representative holds priority.
#[test]
fn teammate_land_play_accepted_on_team_priority() {
    let mut sc = team_turn_board();
    let land = sc.add_land_to_hand(P1, "Forest").id();
    let mut runner = sc.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    let card_id = runner.state().objects[&land].card_id;
    game_engine::apply(
        runner.state_mut(),
        P1,
        GameAction::PlayLand {
            object_id: land,
            card_id,
        },
    )
    .expect("teammate land play must be accepted on team priority");
    assert_eq!(
        runner.state().objects[&land].zone,
        engine::types::zones::Zone::Battlefield
    );
}

/// The teammate can cast an instant while the representative holds priority.
#[test]
fn teammate_instant_cast_accepted_on_team_priority() {
    use engine::types::ability::TargetRef;
    use engine::types::game_state::{CastPaymentMode, StackEntryKind};

    let mut sc = team_turn_board();
    let bolt = sc.add_bolt_to_hand(P1);
    let target = sc.add_creature(P2, "Target", 0, 8).id();
    sc.with_mana_pool(
        P1,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
    );
    let mut runner = sc.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    let card_id = runner.state().objects[&bolt].card_id;
    game_engine::apply(
        runner.state_mut(),
        P1,
        GameAction::CastSpell {
            object_id: bolt,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        },
    )
    .expect("teammate cast must be accepted on team priority");
    // The cast parks on the teammate's own targeting step, not the
    // representative's.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { player, .. } if player == P1
        ),
        "teammate cast must park on P1 targeting, got {:?}",
        runner.state().waiting_for
    );
    game_engine::apply(
        runner.state_mut(),
        P1,
        GameAction::SelectTargets {
            targets: vec![TargetRef::Object(target)],
        },
    )
    .expect("teammate targeting accepted");
    assert!(
        runner.state().stack.iter().any(|entry| entry.id == bolt
            && matches!(entry.kind, StackEntryKind::Spell { .. })
            && entry.controller == P1),
        "teammate spell must reach the stack under P1 control"
    );
}

/// CR 805.5b: one teammate passing does not pass for the team. Both living
/// members must pass before priority leaves the team.
#[test]
fn team_pass_requires_both_teammates() {
    let sc = team_turn_board();
    let mut runner = sc.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    // First teammate passes: the team still holds priority.
    game_engine::apply(runner.state_mut(), P0, GameAction::PassPriority)
        .expect("representative pass accepted");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0 || player == P1
        ),
        "team must retain priority until both members pass, got {:?}",
        runner.state().waiting_for
    );

    // Second teammate passes: now the opposing team receives priority.
    game_engine::apply(runner.state_mut(), P1, GameAction::PassPriority)
        .expect("teammate pass accepted");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P2 || player == P3
        ),
        "opposing team must receive priority after both teammates pass, got {:?}",
        runner.state().waiting_for
    );
}

/// Scryfall-verified Oracle for Ninja of the Deep Hours (J25 #108). NOTE: the
/// current Oracle uses "Whenever this creature deals combat damage to a
/// player" (the plan quoted the older "Whenever Ninja of the Deep Hours
/// deals ..."). The verified text below is used.
const NINJA_ORACLE: &str = "Ninjutsu {1}{U} ({1}{U}, Return an unblocked attacker you control to hand: Put this card onto the battlefield from your hand tapped and attacking.)\nWhenever this creature deals combat damage to a player, you may draw a card.";

fn teammate_ninjutsu_board() -> (GameRunner, ObjectId, ObjectId) {
    let mut sc = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
    sc.at_phase(Phase::DeclareBlockers);
    let ninja = sc
        .add_creature_to_hand_from_oracle(P1, "Ninja of the Deep Hours", 2, 2, NINJA_ORACLE)
        .id();
    let attacker = sc.add_creature(P1, "P1 Saboteur", 1, 1).id();
    sc.with_mana_pool(
        P1,
        vec![
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let mut runner = sc.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    runner.state_mut().combat = Some(CombatState {
        attackers: vec![AttackerInfo::attacking_player(attacker, P2)],
        ..Default::default()
    });
    (runner, ninja, attacker)
}

fn offers_ninjutsu_for(runner: &GameRunner, viewer: PlayerId, ninja: ObjectId) -> bool {
    let (actions, _, _) = ai_support::legal_actions_for_viewer(runner.state(), viewer);
    actions.iter().any(|a| {
        matches!(
            a,
            GameAction::ActivateNinjutsu {
                ninjutsu_object_id,
                ..
            } if *ninjutsu_object_id == ninja
        )
    })
}

/// CR 702.49a + CR 805.5a: the non-representative teammate may activate
/// ninjutsu from hand on the team's combat window, exactly as the
/// representative may.
#[test]
fn teammate_ninjutsu_offered_and_applies_on_team_priority() {
    let (mut runner, ninja, attacker) = teammate_ninjutsu_board();
    assert!(
        runner.state().objects[&ninja]
            .base_keywords
            .iter()
            .any(|keyword| matches!(keyword, Keyword::Ninjutsu(_))),
        "premise: the parsed Ninja carries its printed ninjutsu keyword"
    );

    // Positive: the teammate is offered their own hand ninja.
    assert!(
        offers_ninjutsu_for(&runner, P1, ninja),
        "P1 must be offered their hand ninja on the team's combat window"
    );
    // Hostile (custody gate): the viewer enumeration is seat-agnostic — P0 can
    // SEE the team's offered actions — but P0 cannot ACTIVATE P1's ninja:
    // `activate_ninjutsu` rejects a card outside the actor's hand before any
    // cost or mutation (and separately rejects a non-owner).
    let rep_err = game_engine::apply(
        runner.state_mut(),
        P0,
        GameAction::ActivateNinjutsu {
            ninjutsu_object_id: ninja,
            creature_to_return: attacker,
        },
    )
    .expect_err("P0 must not activate P1's hand ninja");
    assert!(
        format!("{rep_err:?}").contains("not in hand or command zone"),
        "unexpected rejection (want the custody gate): {rep_err:?}"
    );
    assert_eq!(
        runner.state().objects[&ninja].zone,
        Zone::Hand,
        "the refused activation must leave the ninja in hand"
    );
    assert_eq!(
        runner.state().objects[&attacker].zone,
        Zone::Battlefield,
        "the refused activation must leave the attacker in combat"
    );

    // Combat-window scoping: outside DeclareBlockers/CombatDamage the timing
    // gate still withholds the offer — then restores it (reach-guard that the
    // gate body still runs across the phase flip).
    runner.state_mut().phase = Phase::PreCombatMain;
    assert!(
        !offers_ninjutsu_for(&runner, P1, ninja),
        "ninjutsu must not be offered outside its timing window"
    );
    runner.state_mut().phase = Phase::DeclareBlockers;
    assert!(
        offers_ninjutsu_for(&runner, P1, ninja),
        "reach-guard: restoring the combat window restores the offer"
    );

    // Split second suppresses the offer; the positive above is the paired
    // reach-guard proving the gate body runs.
    {
        let state = runner.state_mut();
        let split_obj = engine::game::zones::create_object(
            state,
            CardId(7_007),
            P2,
            "Split Second Shell".to_string(),
            Zone::Battlefield,
        );
        let target = state.objects.get_mut(&split_obj).expect("shell exists");
        target.keywords.push(Keyword::SplitSecond);
        target.base_keywords.push(Keyword::SplitSecond);
        state.stack.push_back(StackEntry {
            id: split_obj,
            source_id: split_obj,
            controller: P2,
            kind: StackEntryKind::ActivatedAbility {
                source_id: split_obj,
                ability: Box::new(ResolvedAbility::new(Effect::NoOp, vec![], split_obj, P2)),
            },
        });
    }
    assert!(
        !offers_ninjutsu_for(&runner, P1, ninja),
        "split second must suppress the ninjutsu offer"
    );
    runner.state_mut().stack.pop_back();

    // Apply through the authenticated boundary as the teammate.
    game_engine::apply(
        runner.state_mut(),
        P1,
        GameAction::ActivateNinjutsu {
            ninjutsu_object_id: ninja,
            creature_to_return: attacker,
        },
    )
    .expect("teammate ninjutsu activation must be accepted on team priority");
    assert_eq!(
        runner.state().objects[&attacker].zone,
        Zone::Hand,
        "the returned attacker is back in hand"
    );
    let ninja_obj = &runner.state().objects[&ninja];
    assert_eq!(ninja_obj.zone, Zone::Battlefield);
    assert!(ninja_obj.tapped, "the ninja enters tapped");
    let combat = runner
        .state()
        .combat
        .as_ref()
        .expect("combat is still active");
    assert!(
        combat.attackers.iter().any(|a| a.object_id == ninja),
        "the ninja enters attacking"
    );
    assert!(
        !combat.attackers.iter().any(|a| a.object_id == attacker),
        "the returned attacker leaves combat"
    );
}

/// Scryfall-verified Oracle for Doomskar (KHM #009): "Destroy all
/// creatures.\nForetell {1}{W}{W} (During your turn, you may pay {2} and
/// exile this card from your hand face down. Cast it on a later turn for its
/// foretell cost.)" — identical to the in-repo foretell fixture.
const DOOMSKAR_ORACLE: &str = "Destroy all creatures.\nForetell {1}{W}{W} (During your turn, you may pay {2} and exile this card from your hand face down. Cast it on a later turn for its foretell cost.)";

/// CR 702.143a + CR 805.5a: a teammate may foretell on the team's turn.
#[test]
fn teammate_doomskar_foretell_accepted_on_team_priority() {
    let mut sc = team_turn_board();
    let doomskar = sc
        .add_spell_to_hand_from_oracle(P1, "Doomskar", false, DOOMSKAR_ORACLE)
        .id();
    let rep_doomskar = sc
        .add_spell_to_hand_from_oracle(P0, "Doomskar", false, DOOMSKAR_ORACLE)
        .id();
    let foe_doomskar = sc
        .add_spell_to_hand_from_oracle(P2, "Doomskar", false, DOOMSKAR_ORACLE)
        .id();
    let vanilla = sc.add_creature_to_hand(P1, "Vanilla Bear", 2, 2).id();
    for seat in [P0, P1, P2] {
        sc.with_mana_pool(
            seat,
            vec![
                ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
                ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ],
        );
    }
    let mut runner = sc.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    for (id, name) in [(doomskar, "P1"), (rep_doomskar, "P0"), (foe_doomskar, "P2")] {
        assert!(
            runner.state().objects[&id]
                .base_keywords
                .iter()
                .any(|keyword| matches!(keyword, Keyword::Foretell(_))),
            "premise: the parsed {name} Doomskar carries its printed foretell keyword"
        );
    }

    // Positive: the teammate may foretell on the team's turn ...
    assert!(
        can_foretell_card(runner.state(), P1, doomskar),
        "P1 must be able to foretell on the team's turn"
    );
    // ... exactly as the representative may (reach-guard: the gate body runs,
    // not vacuously true for one seat).
    assert!(
        can_foretell_card(runner.state(), P0, rep_doomskar),
        "reach-guard: the representative may still foretell on its own turn"
    );
    // Hostile (team gate — P2 is funded, so only membership refuses): the
    // opponent may not foretell on P0's team turn.
    assert!(
        !can_foretell_card(runner.state(), P2, foe_doomskar),
        "P2 must not foretell on the opposing team's turn"
    );
    // Hostile (keyword gate): a card without foretell is not foretellable.
    assert!(
        !can_foretell_card(runner.state(), P1, vanilla),
        "a non-foretell card must not be foretellable"
    );

    let card_id = runner.state().objects[&doomskar].card_id;
    game_engine::apply(
        runner.state_mut(),
        P1,
        GameAction::Foretell {
            object_id: doomskar,
            card_id,
        },
    )
    .expect("teammate foretell must be accepted on team priority");
    let obj = &runner.state().objects[&doomskar];
    assert_eq!(obj.zone, Zone::Exile);
    assert!(obj.foretold, "the card is foretold in exile");
    assert!(obj.face_down, "the foretold card is exiled face down");
    assert!(
        !obj.casting_permissions.is_empty(),
        "the foretell permission is stamped"
    );
}

/// CR 805.5b: `pass_holding_team` skips already-passed seats and keeps the
/// remaining passes' events.
#[test]
fn pass_holding_team_skips_passed_members_and_keeps_events() {
    let sc = team_turn_board();
    let mut runner = sc.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    // Pre-pass the representative through the authenticated boundary.
    game_engine::apply(runner.state_mut(), P0, GameAction::PassPriority)
        .expect("representative pass accepted");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the team must retain priority until every member passes, got {:?}",
        runner.state().waiting_for
    );
    let events = runner
        .pass_holding_team()
        .expect("the helper passes the rest of the holding team");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, GameEvent::PriorityPassed { .. })),
        "the teammate's pass events must be retained, got {events:?}"
    );

    // Hostile: every member already passed — Ok with empty events, not an
    // `AlreadyPassed`-class error.
    let sc = team_turn_board();
    let mut runner = sc.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    runner.state_mut().priority_passes.insert(P0);
    runner.state_mut().priority_passes.insert(P1);
    let events = runner
        .pass_holding_team()
        .expect("an all-passed call must be a no-op Ok");
    assert!(
        events.is_empty(),
        "no passes are owed, so no events may be produced, got {events:?}"
    );
}
