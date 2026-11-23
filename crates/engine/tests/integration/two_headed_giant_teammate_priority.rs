//! CR 805.5a: a player may act when their TEAM has priority, not only the
//! team's representative. In Two-Headed Giant the engine parks priority on the
//! team representative (P0 / P2); the teammate (P1 / P3) must still be able to
//! play lands, cast spells, and activate abilities on the team's turn.

use engine::ai_support;
use engine::game::casting::can_foretell_card;
use engine::game::combat::{AttackerInfo, CombatState};
use engine::game::engine as game_engine;
use engine::game::game_object::RoomDoor;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
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

fn teammate_ninjutsu_board() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
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
    let db = crate::support::shared_card_db().expect("integration card fixture must load");
    let shock = sc.add_real_card(P2, "Sudden Shock", Zone::Hand, db);
    sc.with_mana_pool(
        P2,
        vec![
            ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
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
    (runner, ninja, attacker, shock)
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
    let (mut runner, ninja, attacker, shock) = teammate_ninjutsu_board();
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

    // A printed split-second spell, cast through the pipeline, suppresses the
    // offer while it is on the stack; the combat-window positive above is the
    // paired reach-guard proving the gate body runs.
    runner
        .pass_holding_team()
        .expect("active team passes to P2");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P2
        ),
        "P2 must hold priority to cast, got {:?}",
        runner.state().waiting_for
    );
    let _committed = runner.cast(shock).target_player(P0).commit();
    runner
        .pass_holding_team()
        .expect("opposing team passes back");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ),
        "priority must return to the active representative, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.stack_names(), ["Sudden Shock"]);

    // Reach guards: the printed spell carries split second and is live on the
    // stack in the combat window.
    assert!(
        runner.state().objects[&shock]
            .keywords
            .iter()
            .any(|k| matches!(k, Keyword::SplitSecond)),
        "premise: the cast Sudden Shock carries split second"
    );
    assert!(
        engine::game::keywords::stack_has_split_second(runner.state()),
        "premise: split second is live on the stack"
    );
    assert_eq!(runner.state().phase, Phase::DeclareBlockers);

    // Negative (offer): CR 702.61a + CR 702.49a — ninjutsu is an activated
    // ability that isn't a mana ability, so it can't be offered now.
    assert!(
        !offers_ninjutsu_for(&runner, P1, ninja),
        "CR 702.61a + CR 702.49a: ninjutsu must not be offered while Sudden Shock is on the stack"
    );
    // Negative (reducer): the activation is refused and mutates nothing.
    let err = game_engine::apply(
        runner.state_mut(),
        P1,
        GameAction::ActivateNinjutsu {
            ninjutsu_object_id: ninja,
            creature_to_return: attacker,
        },
    )
    .expect_err("CR 702.61a: ninjutsu can't be activated while Sudden Shock is on the stack");
    assert!(
        format!("{err:?}").contains("split second"),
        "unexpected rejection (want the split-second gate): {err:?}"
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
    assert_eq!(runner.stack_names(), ["Sudden Shock"]);

    // The active team passes; all four have passed, so the Shock resolves.
    runner
        .pass_holding_team()
        .expect("active team passes; all four have passed");
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&shock].zone, Zone::Graveyard);
    // CR 117.4: only the top object resolves — the step does not end.
    assert_eq!(runner.state().phase, Phase::DeclareBlockers);
    // CR 117.3b: the active player receives priority after the resolution.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ),
        "priority must return to the active player, got {:?}",
        runner.state().waiting_for
    );

    // Positive (restored): after a split-second spell resolves, ninjutsu is
    // offered again in the same step.
    assert!(
        offers_ninjutsu_for(&runner, P1, ninja),
        "the ninjutsu offer must return after Sudden Shock resolves"
    );

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

/// CR 805.5b: `pass_holding_team` skips already-passed teammates (never the
/// holder) and keeps the remaining passes' events.
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

    // Hostile: the team still holds priority with every member's pass recorded
    // (CR 117.5: a trigger put on the stack hands priority back without
    // clearing them). The holder still holds priority, so its pass is
    // submitted (CR 117.3d) — a no-op here would leave every drain loop
    // spinning on an unchanged window. The team's pass completes and priority
    // leaves for the opposing team, without an error.
    let sc = team_turn_board();
    let mut runner = sc.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    runner.state_mut().priority_passes.insert(P0);
    runner.state_mut().priority_passes.insert(P1);
    let events = runner
        .pass_holding_team()
        .expect("a fully recorded team still passes without error");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, GameEvent::PriorityPassed { .. })),
        "the holder's pass must be submitted, got {events:?}"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player != P0 && player != P1
        ),
        "the window must leave the holding team, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 805.8: a whole-team controller's pass completes the team's pass — the
/// helper stops once the window leaves the team instead of submitting stale
/// passes into the next window, and keeps the events it collected.
#[test]
fn pass_holding_team_stops_when_a_controller_pass_leaves_the_team() {
    let sc = team_turn_board();
    let mut runner = sc.build();
    // P2 controls the active team's decisions (CR 723): both members' passes
    // submit as P2, and P2's first pass records the whole team. Sync
    // re-derives the authorized submitter onto `priority_player`, as a
    // live controlled turn carries it.
    runner.state_mut().active_player = P0;
    runner.state_mut().turn_decision_controller = Some(P2);
    engine::game::public_state::sync_waiting_for(
        runner.state_mut(),
        &WaitingFor::Priority { player: P0 },
    );

    let events = runner
        .pass_holding_team()
        .expect("a controller-mapped window must complete");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, GameEvent::PriorityPassed { .. })),
        "the completing pass events must be kept, got {events:?}"
    );
    assert!(runner.state().priority_passes.contains(&P0));
    assert!(runner.state().priority_passes.contains(&P1));
    // The window advanced to the opposing team WITHOUT passing either head:
    // the helper stopped instead of submitting stale passes there.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P2
        ),
        "the team pass must advance to P2, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        !runner.state().priority_passes.contains(&P2),
        "P2 must not be passed by the helper's stale submission"
    );
    assert!(
        !runner.state().priority_passes.contains(&P3),
        "P3 must not be passed by the helper's stale submission"
    );
}

/// CR 805.5b: an AI teammate's verified pass authorizes only that teammate.
/// P1's verified pass over an opposing spell must leave the human
/// representative P0 unpassed on the parked window and still able to
/// respond — the session records the semantic owner, never the
/// representative. Regression: recording the representative let P1's pass
/// authorize P0's automatic pass, so P0 never got to answer the Bolt.
#[test]
fn ai_teammate_verified_pass_leaves_representative_unpassed_and_able_to_respond() {
    let db = crate::support::shared_card_db().expect("integration card fixture must load");
    let mut sc = team_turn_board();
    let counterspell = sc.add_real_card(P0, "Counterspell", Zone::Hand, db);
    sc.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
        ],
    );
    let bolt = sc.add_bolt_to_hand(P2);
    sc.with_mana_pool(
        P2,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
    );
    let victim = sc.add_creature(P0, "Target", 0, 8).id();
    let mut runner = sc.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    // Hand priority to the opposing team so P2 can cast; the cast returns
    // priority to the active team's representative with the Bolt on the stack.
    runner
        .pass_holding_team()
        .expect("active team passes to P2");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P2
        ),
        "P2 must hold priority to cast, got {:?}",
        runner.state().waiting_for
    );
    let _committed = runner.cast(bolt).target_object(victim).commit();
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.controller == P2),
        "P2's Bolt must be on the stack"
    );
    // The cast leaves priority with the caster's team; pass it back so the
    // window parks on P0 with the Bolt still on the stack.
    runner
        .pass_holding_team()
        .expect("opposing team passes back");
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.controller == P2),
        "the Bolt must survive the opposing team's passes"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ),
        "priority must return to the active representative, got {:?}",
        runner.state().waiting_for
    );

    // P1 (AI) takes the production verified-pass boundary.
    let contract = ai_support::AiDecisionContract::issue(runner.state(), P1);
    game_engine::apply_verified_ai_priority_pass(
        runner.state_mut(),
        P1,
        &contract,
        GameAction::PassPriority,
    )
    .expect("teammate verified pass must be accepted");

    // The pass is P1's alone: P0 stays unpassed on the parked window, and the
    // session records the semantic owner — never the representative.
    assert!(runner.state().priority_passes.contains(&P1));
    assert!(
        !runner.state().priority_passes.contains(&P0),
        "P1's verified pass must not pass for P0"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ),
        "the window must stay parked on P0, got {:?}",
        runner.state().waiting_for
    );
    let session = runner
        .state()
        .stack_resolution_session
        .as_ref()
        .expect("the verified pass installs a rechecking session");
    assert!(
        session.verified_pass_owners.contains(&P1),
        "the session must record P1's authorization"
    );
    assert!(
        !session.verified_pass_owners.contains(&P0),
        "the session must not record P0 as verified"
    );

    // P0 can still respond: Counterspell over the Bolt is offered to P0.
    let (actions, _, _) = ai_support::legal_actions_for_viewer(runner.state(), P0);
    assert!(
        actions.iter().any(|action| matches!(
            action,
            GameAction::CastSpell { object_id, .. } if *object_id == counterspell
        )),
        "P0 must be offered Counterspell in response, got {actions:?}"
    );
}

/// CR 709.5e + CR 805.5a: a teammate may unlock their own Room's door on the
/// team's turn while priority parks on the representative — the candidate
/// offer and the reducer validate agree. Regression: the offer went
/// team-aware while the validator still required the exact active player,
/// so the teammate's unlock was offered and then refused.
#[test]
fn teammate_room_unlock_offered_and_applies_on_team_priority() {
    let db = crate::support::shared_card_db().expect("integration card fixture must load");
    let mut sc = team_turn_board();
    // Greenhouse {2}{G} // Rickety Gazebo {3}{G}: unlock the {2}{G} left door.
    let room = sc.add_real_card(P1, "Greenhouse", Zone::Battlefield, db);
    sc.with_mana_pool(
        P1,
        vec![
            ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let mut runner = sc.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    // Premise: fully locked on entry (neither half cast).
    assert!(
        !runner.state().objects[&room]
            .room_unlocks
            .unwrap_or_default()
            .is_unlocked(RoomDoor::Left),
        "premise: the left door starts locked"
    );
    // Offered to the teammate on the parked window.
    let (actions, _, _) = ai_support::legal_actions_for_viewer(runner.state(), P1);
    assert!(
        actions.iter().any(|action| matches!(
            action,
            GameAction::UnlockRoomDoor { object_id, door }
                if *object_id == room && *door == RoomDoor::Left
        )),
        "P1 must be offered their Room's left door, got {actions:?}"
    );
    // Hostile (custody gate): the representative cannot unlock P1's Room.
    let rep_err = game_engine::apply(
        runner.state_mut(),
        P0,
        GameAction::UnlockRoomDoor {
            object_id: room,
            door: RoomDoor::Left,
        },
    )
    .expect_err("P0 must not unlock P1's Room");
    assert!(
        format!("{rep_err:?}").contains("controller"),
        "unexpected rejection (want the custody gate): {rep_err:?}"
    );

    // Applies as the teammate through the authenticated boundary.
    let before = runner.state().players[1].mana_pool.total();
    game_engine::apply(
        runner.state_mut(),
        P1,
        GameAction::UnlockRoomDoor {
            object_id: room,
            door: RoomDoor::Left,
        },
    )
    .expect("teammate room unlock must be accepted on team priority");
    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ManaPayment { .. } => {
                game_engine::apply(runner.state_mut(), P1, GameAction::PassPriority)
                    .expect("mana payment finalizes");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.pass_holding_team().expect("settle passes");
            }
            other => panic!("unexpected prompt while unlocking: {other:?}"),
        }
    }
    assert!(
        runner.state().objects[&room]
            .room_unlocks
            .unwrap_or_default()
            .is_unlocked(RoomDoor::Left),
        "the left door must unlock"
    );
    assert_eq!(
        before - runner.state().players[1].mana_pool.total(),
        3,
        "CR 709.5e: the unlock pays the half's {{2}}{{G}}"
    );
}

// ===========================================================================
// Phase-2 (F3, PR #10984 r2): per-seat auto-pass + Resolve All ownership.
// Appended region: imports, VM-P1 helpers, rows VM-S1..VM-A1. Everything
// above this line is read-only for phase 2.
// ===========================================================================

use engine::types::ability::TargetRef;
use engine::types::actions::{ResolveAllConsentDecision, ResolveAllScope};
use engine::types::game_state::{
    AutoPassMode, AutoPassRequest, CastPaymentMode, StackResolutionPolicy, TurnBoundary,
};
use engine::types::phase::{PhaseStop, PhaseStopScope};
use std::collections::BTreeSet;

/// Staged object ids for the VM-P1 board.
struct BoltIds {
    /// P2's cast Bolt (on the stack after setup).
    bolt: ObjectId,
    /// P0's spare Bolt (clear-casts in S2/C1).
    p0_bolt: ObjectId,
    /// P1's spare Bolt (clear-cast in F2).
    p1_bolt: ObjectId,
    /// P1's Counterspell (deliberate cast in D1).
    p1_counterspell: ObjectId,
}

/// Stages the union of what the phase-2 rows need: P2 holds two printed
/// Bolts + 2 red (one to cast, one spare so P2 keeps a meaningful action
/// afterwards); P0 holds a Bolt + red; P1 holds a Bolt + red and
/// Counterspell + blue blue. Unused holdings are inert: the runner consults
/// preferences, never mere holdings. Returned pre-build for rows with
/// custom drive orders (R2); most rows use [`bolt_setup`] instead.
fn staged_bolt_scenario() -> (GameScenario, BoltIds) {
    let db = crate::support::shared_card_db().expect("integration card fixture must load");
    let mut sc = team_turn_board();
    let bolt = sc.add_real_card(P2, "Lightning Bolt", Zone::Hand, db);
    sc.add_real_card(P2, "Lightning Bolt", Zone::Hand, db);
    sc.with_mana_pool(
        P2,
        vec![
            ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
        ],
    );
    let p0_bolt = sc.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    sc.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
    );
    let p1_bolt = sc.add_real_card(P1, "Lightning Bolt", Zone::Hand, db);
    let p1_counterspell = sc.add_real_card(P1, "Counterspell", Zone::Hand, db);
    sc.with_mana_pool(
        P1,
        vec![
            ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
        ],
    );
    (
        sc,
        BoltIds {
            bolt,
            p0_bolt,
            p1_bolt,
            p1_counterspell,
        },
    )
}

/// Parks a fresh runner on P0's team window.
fn park_p0(runner: &mut GameRunner) {
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
}

/// VM-P1 shared board: P2 casts a printed Bolt at P0 through the pipeline,
/// then the opposing team hands priority back. Returns with `Priority{P0}`,
/// `passes == {P2, P3}`, `stack == [Bolt]`.
fn bolt_setup() -> (GameRunner, BoltIds) {
    let (sc, ids) = staged_bolt_scenario();
    let mut runner = sc.build();
    park_p0(&mut runner);
    runner
        .pass_holding_team()
        .expect("active team passes to P2");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P2
        ),
        "P2 must hold priority to cast, got {:?}",
        runner.state().waiting_for
    );
    let _committed = runner.cast(ids.bolt).target_player(P0).commit();
    assert_eq!(runner.stack_names(), ["Lightning Bolt"]);
    runner
        .pass_holding_team()
        .expect("opposing team passes back");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ),
        "priority must return to the active representative, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(passes_set(&runner), BTreeSet::from([P2, P3]));
    (runner, ids)
}

/// Recorded passes as an exact set.
fn passes_set(runner: &GameRunner) -> BTreeSet<PlayerId> {
    runner.state().priority_passes.iter().copied().collect()
}

/// Begins a teammate's instant cast on a parked window through an explicit
/// apply (the fluent builder submits as the waiting player, so it cannot
/// cast a teammate's spell). The cast parks on the caster's targeting step;
/// [`teammate_cast_finish`] answers it.
fn teammate_cast_begin(runner: &mut GameRunner, caster: PlayerId, spell: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    game_engine::apply(
        runner.state_mut(),
        caster,
        GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        },
    )
    .expect("teammate cast must be accepted on team priority");
}

/// Answers a begun teammate cast's targeting with `target` and finalizes a
/// pool payment if one surfaces.
fn teammate_cast_finish(runner: &mut GameRunner, caster: PlayerId, target: TargetRef) {
    for _ in 0..4 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                game_engine::apply(
                    runner.state_mut(),
                    caster,
                    GameAction::ChooseTarget {
                        target: Some(target.clone()),
                    },
                )
                .expect("teammate target must be accepted");
            }
            WaitingFor::ManaPayment { .. } => {
                game_engine::apply(runner.state_mut(), caster, GameAction::PassPriority)
                    .expect("pool payment finalizes");
            }
            _ => break,
        }
    }
}

/// Drives a teammate's instant cast on a parked window to commit.
fn teammate_cast(runner: &mut GameRunner, caster: PlayerId, spell: ObjectId, target: TargetRef) {
    teammate_cast_begin(runner, caster, spell);
    teammate_cast_finish(runner, caster, target);
}

/// VM-S1 (F3/D2.2): a teammate's direct auto-pass stores under the teammate's
/// own key, and the boundary runner consumes the teammate's own pass. CR
/// 805.5b: per-seat decision bookkeeping on a shared team turn.
#[test]
fn teammate_direct_auto_pass_stores_own_key_and_passes_own_seat() {
    let sc = team_turn_board();
    let mut runner = sc.build();
    park_p0(&mut runner);

    game_engine::apply(
        runner.state_mut(),
        P1,
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilTurnBoundary {
                until: TurnBoundary::EndOfCurrentTurn,
            },
        },
    )
    .expect("teammate auto-pass install must be accepted");
    // Reach: accepted on the parked window with an empty stack.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ),
        "the window must stay parked on P0, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "reach: the board stays bare"
    );
    // The preference is P1's alone.
    assert_eq!(
        runner.state().auto_pass.get(&P1),
        Some(&AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        }),
        "P1's install must store under P1"
    );
    assert!(
        !runner.state().auto_pass.contains_key(&P0),
        "P1's install must not store under P0"
    );
    // ... and the runner consumed P1's own pass.
    assert!(
        runner.state().priority_passes.contains(&P1),
        "P1's install must pass for P1"
    );
    assert!(
        !runner.state().priority_passes.contains(&P0),
        "P1's install must not pass for P0"
    );
    // Hostile: P0 still holds a live window with its own actions.
    let (actions, _, _) = ai_support::legal_actions_for_viewer(runner.state(), P0);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, GameAction::PassPriority)),
        "P0 must still be offered its own pass, got {actions:?}"
    );
}

/// VM-S2 (F3/D2.2): a teammate's UntilStackEmpty on a nonempty stack
/// preserves the representative's live session and merges the teammate's own
/// key into the teardown baseline. CR 805.5b.
#[test]
fn teammate_until_stack_empty_preserves_session_and_merges_baseline() {
    let (mut runner, ids) = bolt_setup();
    // P0 clear-casts Bolt 2 at P2: passes clear, the stack grows to two, the
    // window stays with the caster.
    let life_p2_before = runner.state().players[2].life;
    let _committed = runner.cast(ids.p0_bolt).target_player(P2).commit();
    assert!(
        runner.state().priority_passes.is_empty(),
        "reach: the cast clears passes"
    );
    assert_eq!(
        runner.state().stack.len(),
        2,
        "reach: two Bolts on the stack"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ),
        "reach: the caster keeps the window, got {:?}",
        runner.state().waiting_for
    );

    // P0 installs: canonical session {P0} + P0's immediate own pass.
    game_engine::apply(
        runner.state_mut(),
        P0,
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .expect("P0 install must be accepted");
    // Reach pre-P1-install: session live, window P0, passes == {P0}.
    let session = runner
        .state()
        .stack_resolution_session
        .as_ref()
        .expect("reach: P0 install opens the session");
    assert_eq!(session.representatives, BTreeSet::from([P0]));
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ),
        "reach: pre-P1-install window P0, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(passes_set(&runner), BTreeSet::from([P0]));

    // P1 installs its own UntilStackEmpty.
    game_engine::apply(
        runner.state_mut(),
        P1,
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .expect("P1 install must be accepted");
    // Post: the runner drove P1 from its own preference and stopped at P2's
    // meaningful action with the session intact.
    assert_eq!(runner.state().stack.len(), 2, "nothing resolves on install");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P2
        ),
        "post: window P2, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(passes_set(&runner), BTreeSet::from([P0, P1]));
    let session = runner
        .state()
        .stack_resolution_session
        .as_ref()
        .expect("the session survives P1's install");
    assert_eq!(session.representatives, BTreeSet::from([P0]));
    assert!(
        matches!(
            runner.state().auto_pass.get(&P1),
            Some(AutoPassMode::UntilStackEmpty {
                initial_stack_len: 2,
                ..
            })
        ),
        "the live map holds P1's own key, got {:?}",
        runner.state().auto_pass.get(&P1)
    );
    assert!(
        session.auto_pass_overlay.baseline.contains_key(&P1),
        "the baseline merged P1's key"
    );
    assert_eq!(
        runner.state().players[2].life,
        life_p2_before,
        "reach: no Bolt resolved yet"
    );
}

/// VM-R1 (F3/D2.3): the runner drives an unpassed member from its own
/// preference. P0 passes manually; P1's inserted UntilStackEmpty carries the
/// round home and the Bolt resolves. CR 805.5b + CR 117.4.
#[test]
fn runner_drives_unpassed_teammate_by_own_preference() {
    let (mut runner, _) = bolt_setup();
    // Reach: the helper parked P0 with the opposing team passed and [Bolt].
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ),
        "reach: window P0, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(passes_set(&runner), BTreeSet::from([P2, P3]));
    assert_eq!(runner.stack_names(), ["Lightning Bolt"]);
    // Direct insert: installing through apply would record P1's pass during
    // setup, but the beat needs P1 unpassed.
    runner.state_mut().auto_pass.insert(
        P1,
        AutoPassMode::UntilStackEmpty {
            initial_stack_len: 1,
            policy: StackResolutionPolicy::Committed,
        },
    );
    assert!(
        matches!(
            runner.state().auto_pass.get(&P1),
            Some(AutoPassMode::UntilStackEmpty {
                initial_stack_len: 1,
                ..
            })
        ),
        "reach: P1's own key live"
    );
    assert!(
        !runner.state().priority_passes.contains(&P1),
        "reach: P1 unpassed"
    );
    let life_p0_before = runner.state().players[0].life;

    // Beat: P0's manual pass. The runner must drive P1 from P1's preference,
    // not re-pass P0. (Both passes being accepted is the behavioral proof the
    // divergence latch gates nothing here.)
    game_engine::apply(runner.state_mut(), P0, GameAction::PassPriority).expect("P0 passes");
    // The round completed: empty stack and Bolt damage on P0.
    assert!(
        runner.state().stack.is_empty(),
        "the Bolt must resolve, stack: {:?}",
        runner.stack_names()
    );
    assert_eq!(
        runner.state().players[0].life,
        life_p0_before - 3,
        "Bolt deals 3 to P0"
    );
}

/// VM-R2 (F3/D2.3): no cross-seat Exit shortcut. P3's standing preference
/// never passes P0's window: the handoff back records only its own seats.
/// Team 0 stays bare (no holdings, no preferences) so the base code's
/// session seam would drive P0 — the revert-failing beat.
#[test]
fn opposing_teammate_preference_never_passes_this_window() {
    let db = crate::support::shared_card_db().expect("integration card fixture must load");
    let mut sc = team_turn_board();
    let bolt = sc.add_real_card(P2, "Lightning Bolt", Zone::Hand, db);
    sc.with_mana_pool(
        P2,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
    );
    let mut runner = sc.build();
    park_p0(&mut runner);
    runner
        .pass_holding_team()
        .expect("active team passes to P2");
    let _committed = runner.cast(bolt).target_player(P0).commit();
    // P3 installs its own UntilStackEmpty on the nonempty stack.
    game_engine::apply(
        runner.state_mut(),
        P3,
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .expect("P3 install must be accepted");

    // Beat: the handoff back to team 0.
    runner.pass_holding_team().expect("handoff back");
    // Discriminating assert first: the base session seam records P0 here.
    assert_eq!(
        passes_set(&runner),
        BTreeSet::from([P2, P3]),
        "only the handoff's own seats pass"
    );
    assert!(
        !runner.state().priority_passes.contains(&P0),
        "P0 must stay unpassed"
    );
    assert_eq!(runner.stack_names(), ["Lightning Bolt"]);
    // Reach: P3's own key live, window P0, team 0 bare of preferences.
    assert!(
        matches!(
            runner.state().auto_pass.get(&P3),
            Some(AutoPassMode::UntilStackEmpty {
                initial_stack_len: 1,
                ..
            })
        ),
        "reach: P3's own key live, got {:?}",
        runner.state().auto_pass
    );
    assert_eq!(
        runner.state().auto_pass.len(),
        1,
        "reach: bare-team premises"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ),
        "reach: window P0, got {:?}",
        runner.state().waiting_for
    );

    // Tail: P1 installs its own UntilStackEmpty; the runner drives P1 from
    // it while P0 stays unpassed.
    game_engine::apply(
        runner.state_mut(),
        P1,
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .expect("P1 install must be accepted");
    assert!(
        runner.state().auto_pass.contains_key(&P1),
        "tail: P1's own key live"
    );
    assert!(
        runner.state().priority_passes.contains(&P1),
        "tail: the runner drove P1 from its own preference"
    );
    assert!(
        !runner.state().priority_passes.contains(&P0),
        "tail: P0 still unpassed"
    );
}

/// VM-R3 (F3/D2.3): the runner continues across a round boundary while its
/// passes grow, and terminates at the next team's bare seats. P3's pass
/// completes the round and advances the phase; P0/P1's boundary preferences
/// re-pass their team on the fresh window; P2's window stops the drive.
#[test]
fn runner_continues_across_round_boundary_stops_at_next_team() {
    let sc = team_turn_board();
    let mut runner = sc.build();
    park_p0(&mut runner);
    // Direct inserts: installing through apply would self-record passes.
    runner.state_mut().auto_pass.insert(
        P0,
        AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        },
    );
    runner.state_mut().auto_pass.insert(
        P1,
        AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::MyNextTurnStart,
        },
    );
    runner.pass_holding_team().expect("team 0 passes");
    game_engine::apply(runner.state_mut(), P2, GameAction::PassPriority).expect("P2 passes");
    // Reach pre: {P0,P1,P2} with the window parked on P2.
    assert_eq!(passes_set(&runner), BTreeSet::from([P0, P1, P2]));
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P2
        ),
        "reach: window P2, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().phase, Phase::PreCombatMain);

    // Beat: P3's pass completes the round and advances the phase.
    game_engine::apply(runner.state_mut(), P3, GameAction::PassPriority).expect("P3 passes");
    assert_eq!(
        passes_set(&runner),
        BTreeSet::from([P0, P1]),
        "the runner re-passed team 0 on the fresh window"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P2
        ),
        "the drive terminates at P2's bare window, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().phase,
        Phase::BeginCombat,
        "the completed round advanced the phase"
    );
    // Reach post: preferences live, no phase stops.
    assert!(
        runner.state().auto_pass.contains_key(&P0) && runner.state().auto_pass.contains_key(&P1),
        "reach: post prefs live"
    );
    assert!(
        runner.state().phase_stops.is_empty(),
        "reach: phase stops empty"
    );
}

/// VM-F2 (F3/D2.4): the resolution decrement tests the key itself — a
/// teammate's own baseline advances while the representative's overlay key
/// is skipped. CR 805.5b: overlay entries are exactly the session
/// representatives' keys.
#[test]
fn resolution_decrement_advances_teammate_key_skips_overlay() {
    let (mut runner, ids) = bolt_setup();
    // P1 clear-casts Bolt 2 at P3 through explicit teammate applies.
    teammate_cast(&mut runner, P1, ids.p1_bolt, TargetRef::Player(P3));
    // Reach: [B1, B2], Priority{P1}, passes cleared.
    assert_eq!(runner.stack_names(), ["Lightning Bolt", "Lightning Bolt"]);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P1
        ),
        "reach: the caster holds the window, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().priority_passes.is_empty(),
        "reach: the cast clears passes"
    );
    // P1 + P0 install their own UntilStackEmpty preferences. P1 installs
    // first, while the window is still its team's: after P0's install the
    // runner parks the window on P2, where P1 may no longer submit.
    game_engine::apply(
        runner.state_mut(),
        P1,
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .expect("P1 install must be accepted");
    game_engine::apply(
        runner.state_mut(),
        P0,
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .expect("P0 install must be accepted");
    assert!(
        runner.state().stack_resolution_session.is_some(),
        "reach: live session"
    );
    let life_p3_before = runner.state().players[3].life;

    // Complete the round manually: P2, P3 pass; B2 resolves; the cascade
    // stops at P2's meaningful action.
    game_engine::apply(runner.state_mut(), P2, GameAction::PassPriority).expect("P2 passes");
    game_engine::apply(runner.state_mut(), P3, GameAction::PassPriority).expect("P3 passes");
    assert_eq!(runner.stack_names(), ["Lightning Bolt"]);
    assert_eq!(
        runner.state().players[3].life,
        life_p3_before - 3,
        "B2 dealt its damage"
    );
    assert!(
        runner.state().stack_resolution_session.is_some(),
        "reach: live session"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P2
        ),
        "reach: the cascade stops at P2, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(passes_set(&runner), BTreeSet::from([P0, P1]));
    // The teammate's key decremented 2 -> 1; the overlay key skipped.
    assert!(
        matches!(
            runner.state().auto_pass.get(&P1),
            Some(AutoPassMode::UntilStackEmpty {
                initial_stack_len: 1,
                ..
            })
        ),
        "P1's baseline must decrement to 1, got {:?}",
        runner.state().auto_pass.get(&P1)
    );
    assert!(
        matches!(
            runner.state().auto_pass.get(&P0),
            Some(AutoPassMode::UntilStackEmpty {
                initial_stack_len: 2,
                ..
            })
        ),
        "reach: P0's overlay key stays at 2, got {:?}",
        runner.state().auto_pass.get(&P0)
    );
}

/// VM-C1 (F3/D2.5): a teammate's cancel removes only its own preference
/// (live map + baseline) while the representative's session runs on. CR
/// 805.5b: cancel revokes the acting seat's preference; teardown is
/// team-level, by self-membership.
#[test]
fn teammate_cancel_removes_only_own_key_session_survives() {
    let (mut runner, ids) = bolt_setup();
    // P0 clear-casts Bolt 2 at P2: passes {}, len 2, window P0.
    let _committed = runner.cast(ids.p0_bolt).target_player(P2).commit();
    assert!(
        runner.state().priority_passes.is_empty(),
        "reach: the cast clears passes"
    );
    assert_eq!(runner.state().stack.len(), 2);
    // P0 installs: passes {P0}, session {P0}, window P0 retained.
    game_engine::apply(
        runner.state_mut(),
        P0,
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .expect("P0 install must be accepted");
    assert_eq!(passes_set(&runner), BTreeSet::from([P0]));
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ),
        "reach: pre-P1-install window P0, got {:?}",
        runner.state().waiting_for
    );
    // P1 installs its turn boundary. A boundary preference breaks over
    // another seat's spell, so P1 then passes manually through the session.
    game_engine::apply(
        runner.state_mut(),
        P1,
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilTurnBoundary {
                until: TurnBoundary::EndOfCurrentTurn,
            },
        },
    )
    .expect("P1 install must be accepted");
    game_engine::apply(runner.state_mut(), P1, GameAction::PassPriority)
        .expect("P1 passes manually through the session");
    // Reach pre-cancel: session + P1 + window P2 + passes {P0,P1}.
    assert!(
        runner.state().stack_resolution_session.is_some(),
        "reach: session live pre-cancel"
    );
    assert!(
        runner.state().auto_pass.contains_key(&P1),
        "reach: P1's key live pre-cancel"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P2
        ),
        "reach: window P2 pre-cancel, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(passes_set(&runner), BTreeSet::from([P0, P1]));

    // Beat: P1 cancels its own preference.
    game_engine::apply(runner.state_mut(), P1, GameAction::CancelAutoPass)
        .expect("P1 cancel must be accepted");
    let session = runner
        .state()
        .stack_resolution_session
        .as_ref()
        .expect("the session survives P1's cancel");
    assert_eq!(session.representatives, BTreeSet::from([P0]));
    assert!(
        !runner.state().auto_pass.contains_key(&P1),
        "P1's live key must be removed"
    );
    assert!(
        !session.auto_pass_overlay.baseline.contains_key(&P1),
        "P1's baseline key must be removed"
    );
    // Post: passes preserved, window P2.
    assert_eq!(passes_set(&runner), BTreeSet::from([P0, P1]));
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P2
        ),
        "post: window P2, got {:?}",
        runner.state().waiting_for
    );
}

/// VM-C2 (F3/D2.5+D2.7): a pending-consent cancel mirrors the actor's own
/// key, and the decline rollback restores every seat but that one.
#[test]
fn pending_consent_cancel_and_decline_restore_only_other_seats() {
    let (mut runner, _) = bolt_setup();
    // Direct inserts: installing through apply would self-record passes.
    runner.state_mut().auto_pass.insert(
        P0,
        AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        },
    );
    runner.state_mut().auto_pass.insert(
        P1,
        AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::MyNextTurnStart,
        },
    );
    // P0 opens Shared Resolve All: the baseline snapshots {P0, P1} and P2
    // is queued for consent.
    game_engine::apply(
        runner.state_mut(),
        P0,
        GameAction::BeginResolveAll {
            max_resolutions: 0,
            scope: ResolveAllScope::Shared,
        },
    )
    .expect("P0 opens Shared consent");
    // Reach pre: consent queued at P2 with the full baseline.
    let epoch = match runner.state().waiting_for.clone() {
        WaitingFor::ResolveAllConsent {
            epoch,
            representative,
        } => {
            assert_eq!(representative, P2, "reach: P2 queued for consent");
            epoch
        }
        other => panic!("reach: ResolveAllConsent{{P2}}, got {other:?}"),
    };
    {
        let run = runner
            .state()
            .resolve_all_consent_run
            .as_ref()
            .expect("reach: run live");
        let baseline = run
            .auto_pass_baseline
            .as_ref()
            .expect("reach: baseline snapshotted");
        assert_eq!(baseline.len(), 2, "reach: baseline holds both seats");
        assert!(baseline.contains_key(&P0) && baseline.contains_key(&P1));
    }
    // P1 cancels its own preference mid-consent.
    game_engine::apply(runner.state_mut(), P1, GameAction::CancelAutoPass)
        .expect("P1 cancels mid-consent");
    // P2 declines: rollback restores the baseline minus P1.
    game_engine::apply(
        runner.state_mut(),
        P2,
        GameAction::RespondResolveAllConsent {
            epoch,
            decision: ResolveAllConsentDecision::Decline,
        },
    )
    .expect("P2 declines");
    // Only P0's preference survives, exactly.
    assert_eq!(
        runner.state().auto_pass.len(),
        1,
        "only one preference survives the rollback, got {:?}",
        runner.state().auto_pass
    );
    assert_eq!(
        runner.state().auto_pass.get(&P0),
        Some(&AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        })
    );
    // Reach post: the priority snapshot restored the helper window.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ),
        "post: window P0, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(passes_set(&runner), BTreeSet::from([P2, P3]));
}

/// VM-D1 (F3/D2.5): a teammate's deliberate cast revokes only its own
/// preference; the representative's survives. No session live. CR 805.5b.
#[test]
fn teammate_deliberate_cast_revokes_only_own_preference() {
    let (mut runner, ids) = bolt_setup();
    // Direct inserts: P0-EOC + P1-MNTS.
    runner.state_mut().auto_pass.insert(
        P0,
        AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        },
    );
    runner.state_mut().auto_pass.insert(
        P1,
        AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::MyNextTurnStart,
        },
    );
    // Reach pre: both keys live on the helper window.
    assert!(runner.state().auto_pass.contains_key(&P0));
    assert!(runner.state().auto_pass.contains_key(&P1));
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ),
        "reach: window P0, got {:?}",
        runner.state().waiting_for
    );

    // Beat: P1 casts Counterspell at the Bolt.
    teammate_cast(
        &mut runner,
        P1,
        ids.p1_counterspell,
        TargetRef::Object(ids.bolt),
    );
    // Post: [Bolt, Counterspell], Priority{P1}, passes cleared.
    assert_eq!(runner.stack_names(), ["Lightning Bolt", "Counterspell"]);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P1
        ),
        "post: the caster holds the window, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().priority_passes.is_empty(),
        "post: passes cleared"
    );
    // Only P0's preference survives the deliberate cast.
    assert_eq!(
        runner.state().auto_pass.get(&P0),
        Some(&AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        }),
        "P0's preference must survive P1's cast"
    );
    assert!(
        !runner.state().auto_pass.contains_key(&P1),
        "P1's own preference must be revoked by its cast"
    );
}

/// VM-D1b (F3/D2.5 + mirror): a teammate's deliberate cast with a live
/// session mirror-scrubs only the teammate's key instead of tearing the
/// session down — so when the new top fence-teardowns the cohort, the
/// restore resurrects nothing. (Deviation from the plan's tap beat:
/// teammate mana taps are rejected by the engine's mana custody rule and
/// PlayLand needs an empty stack, so the cast is the only viable deliberate
/// beat; the mirror seam under test is unchanged.)
#[test]
fn teammate_deliberate_cast_with_live_session_mirror_scrubs() {
    let (mut runner, ids) = bolt_setup();
    runner.state_mut().auto_pass.insert(
        P1,
        AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::MyNextTurnStart,
        },
    );
    game_engine::apply(
        runner.state_mut(),
        P0,
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .expect("P0 install must be accepted");
    // Reach pre: session {P0}, both keys live, P1 merged into the baseline.
    let session = runner
        .state()
        .stack_resolution_session
        .as_ref()
        .expect("reach: P0 install opens the session");
    assert_eq!(session.representatives, BTreeSet::from([P0]));
    assert!(runner.state().auto_pass.contains_key(&P0));
    assert!(runner.state().auto_pass.contains_key(&P1));
    assert!(session.auto_pass_overlay.baseline.contains_key(&P1));

    // Beat: P1 deliberately casts Bolt 2 at P3.
    teammate_cast_begin(&mut runner, P1, ids.p1_bolt);
    // Mid (discriminating): the cast parked on P1's targeting step with the
    // session live and P1 mirror-scrubbed — the targeting apply below would
    // remove P1's key in both versions, so the seam is observed here.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { player, .. } if player == P1
        ),
        "mid: the cast parks on P1 targeting, got {:?}",
        runner.state().waiting_for
    );
    let session = runner
        .state()
        .stack_resolution_session
        .as_ref()
        .expect("mid: no deliberate teardown — the session lives");
    assert_eq!(session.representatives, BTreeSet::from([P0]));
    assert_eq!(
        runner.state().auto_pass.len(),
        1,
        "mid: only P0's overlay survives the mirror scrub, got {:?}",
        runner.state().auto_pass
    );
    assert!(
        !runner.state().auto_pass.contains_key(&P1),
        "mid: P1's live key is mirror-scrubbed"
    );
    assert!(
        session.auto_pass_overlay.baseline.is_empty(),
        "mid: P1's baseline key is mirror-scrubbed"
    );
    teammate_cast_finish(&mut runner, P1, TargetRef::Player(P3));
    // Post: [B1, B2], Priority{P1}, passes cleared.
    assert_eq!(runner.stack_names(), ["Lightning Bolt", "Lightning Bolt"]);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P1
        ),
        "post: the caster holds the window, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().priority_passes.is_empty(),
        "post: passes cleared"
    );
    // The new top fence-tore the cohort down (reach: the restore ran, so the
    // emptiness below is non-vacuous) and resurrected nothing: P1's key was
    // mirror-scrubbed from both representations instead of restored.
    assert!(
        runner.state().stack_resolution_session.is_none(),
        "reach: the fence teardown ran"
    );
    assert!(
        runner.state().auto_pass.is_empty(),
        "no preference may survive the mirror-scrubbed restore, got {:?}",
        runner.state().auto_pass
    );
    assert!(
        !runner.state().auto_pass.contains_key(&P1),
        "P1's revoked key must not be resurrected"
    );
}

/// VM-B1 (F3/D2.6): a teammate's unlimited Own Resolve All is exactly their
/// own UntilStackEmpty install — no consent run, no session. CR 805.5b.
#[test]
fn teammate_unlimited_own_begin_is_own_until_stack_empty() {
    let (mut runner, _) = bolt_setup();
    game_engine::apply(
        runner.state_mut(),
        P1,
        GameAction::BeginResolveAll {
            max_resolutions: 0,
            scope: ResolveAllScope::Own,
        },
    )
    .expect("P1 begins Own");
    assert!(
        matches!(
            runner.state().auto_pass.get(&P1),
            Some(AutoPassMode::UntilStackEmpty {
                initial_stack_len: 1,
                ..
            })
        ),
        "P1's Begin must install P1's own USE, got {:?}",
        runner.state().auto_pass.get(&P1)
    );
    assert!(
        runner.state().priority_passes.contains(&P1),
        "P1's Begin must pass for P1"
    );
    assert!(
        runner.state().resolve_all_consent_run.is_none(),
        "no consent run for a teammate Own entry"
    );
    assert!(
        runner.state().stack_resolution_session.is_none(),
        "no session for a teammate Own entry"
    );
    assert!(
        !runner.state().priority_passes.contains(&P0),
        "P0 must stay unpassed"
    );
    // Reach: the Bolt still awaits, window retained on P0.
    assert_eq!(runner.stack_names(), ["Lightning Bolt"]);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player } if player == P0
        ),
        "reach: window P0, got {:?}",
        runner.state().waiting_for
    );
}

/// VM-B2/B3 (F3/D2.6): a teammate's Shared or limited Resolve All is refused
/// before any mutation; the window is left healthy enough to finish the
/// round. CR 805.5b: one seat never binds another's passes.
#[test]
fn teammate_shared_or_limited_begin_refused_without_mutation() {
    for (scope, max_resolutions) in [
        (ResolveAllScope::Shared, 0u32),
        (ResolveAllScope::Own, 3u32),
    ] {
        let (mut runner, _) = bolt_setup();
        game_engine::apply(runner.state_mut(), P0, GameAction::PassPriority).expect("P0 passes");
        assert_eq!(
            passes_set(&runner),
            BTreeSet::from([P2, P3, P0]),
            "reach: {scope:?}/{max_resolutions}"
        );
        let err = game_engine::apply(
            runner.state_mut(),
            P1,
            GameAction::BeginResolveAll {
                max_resolutions,
                scope,
            },
        )
        .expect_err("teammate Shared/limited Begin must be refused");
        assert!(
            format!("{err:?}").contains("ActionNotAllowed"),
            "unexpected refusal (want ActionNotAllowed): {err:?}"
        );
        // Refused pre-mutation: pass state, preferences, run, window.
        assert_eq!(passes_set(&runner), BTreeSet::from([P2, P3, P0]));
        assert!(
            runner.state().auto_pass.is_empty(),
            "no preference may be stored by a refused Begin"
        );
        assert!(runner.state().resolve_all_consent_run.is_none());
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { player } if player == P0
            ),
            "window P0, got {:?}",
            runner.state().waiting_for
        );
        // Tail: P1's manual pass completes the round; the Bolt resolves.
        let life_p0_before = runner.state().players[0].life;
        game_engine::apply(runner.state_mut(), P1, GameAction::PassPriority).expect("P1 passes");
        assert!(
            runner.state().stack.is_empty(),
            "the Bolt must resolve, stack: {:?}",
            runner.stack_names()
        );
        assert_eq!(
            runner.state().players[0].life,
            life_p0_before - 3,
            "Bolt deals 3 to P0"
        );
    }
}

/// VM-A1 (F3/D2.7): a Declare prompt's auto-pass stores under the prompt
/// seat — the non-representative blocker's key, never the representative's.
/// The representative's attacker prompt stores under itself, unchanged.
#[test]
fn declare_prompts_store_under_prompt_seat() {
    let mut sc = team_turn_board();
    let raider = sc.add_creature(P2, "P2 Raider", 2, 2).id();
    let guardian = sc.add_creature(P1, "P1 Guardian", 1, 3).id();
    let mut runner = sc.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    // Stops keep both imperative prompts parked: a turn boundary would
    // otherwise auto-submit the attacker declaration, and an attacker-less
    // board would auto-submit the blockers.
    runner.state_mut().phase_stops.insert(
        P1,
        vec![PhaseStop {
            phase: Phase::DeclareBlockers,
            scope: PhaseStopScope::AllTurns,
        }],
    );
    runner.state_mut().phase_stops.insert(
        P0,
        vec![PhaseStop {
            phase: Phase::DeclareAttackers,
            scope: PhaseStopScope::AllTurns,
        }],
    );

    // Blockers half: imperative DeclareBlockers{P1}.
    runner.state_mut().phase = Phase::DeclareBlockers;
    runner.state_mut().waiting_for = WaitingFor::DeclareBlockers {
        player: P1,
        valid_blocker_ids: vec![guardian],
        valid_block_targets: Default::default(),
        block_requirements: Default::default(),
        blocker_constraints: Default::default(),
        must_be_blocked_targets: Default::default(),
        block_capacities: Default::default(),
    };
    game_engine::apply(
        runner.state_mut(),
        P1,
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilTurnBoundary {
                until: TurnBoundary::EndOfCurrentTurn,
            },
        },
    )
    .expect("blocker auto-pass must be accepted");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { player, .. } if player == P1
        ),
        "reach: the blocker prompt stands, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().auto_pass.get(&P1),
        Some(&AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        }),
        "the blocker prompt must store under P1"
    );
    assert!(
        !runner.state().auto_pass.contains_key(&P0),
        "the blocker prompt must not store under P0"
    );
    let err = game_engine::apply(
        runner.state_mut(),
        P1,
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .expect_err("UntilStackEmpty must be refused while declaring blockers");
    assert!(
        format!("{err:?}").contains("ActionNotAllowed"),
        "unexpected refusal (want ActionNotAllowed): {err:?}"
    );

    // Attackers half: imperative DeclareAttackers{P0} (representative path).
    runner.state_mut().phase = Phase::DeclareAttackers;
    runner.state_mut().waiting_for = WaitingFor::DeclareAttackers {
        player: P0,
        valid_attacker_ids: vec![raider],
        valid_attack_targets: vec![],
        valid_attack_targets_by_attacker: None,
        attacker_constraints: Default::default(),
    };
    game_engine::apply(
        runner.state_mut(),
        P0,
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilTurnBoundary {
                until: TurnBoundary::EndOfCurrentTurn,
            },
        },
    )
    .expect("attacker auto-pass must be accepted");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { player, .. } if player == P0
        ),
        "reach: the attacker prompt stands, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().auto_pass.get(&P0),
        Some(&AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        }),
        "the attacker prompt must store under P0"
    );
    let err = game_engine::apply(
        runner.state_mut(),
        P0,
        GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        },
    )
    .expect_err("UntilStackEmpty must be refused while declaring attackers");
    assert!(
        format!("{err:?}").contains("ActionNotAllowed"),
        "unexpected refusal (want ActionNotAllowed): {err:?}"
    );
}
