//! Real-card regression for the reducer-backed prospective fetchland route.

use engine::ai_support::{
    certify_fetch_then_cast, validated_candidate_actions_for_semantic_owner, CandidateAction,
    TacticalClass,
};
use engine::game::game_object::ObjectIncarnationRefExt;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, ManaChoice, WaitingFor};
use engine::types::identifiers::{ObjectId, ObjectIdentityBinding, ObjectIncarnationRef};
use engine::types::mana::ManaType;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn fetchland_state() -> (
    engine::types::game_state::GameState,
    ObjectId,
    ObjectId,
    ObjectId,
    ObjectId,
) {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let misty = scenario.add_real_card(P0, "Misty Rainforest", Zone::Battlefield, db);
    // Phantom Monster needs three generic mana in addition to the fetched
    // blue source. These preserve the Forest-first baseline while making the
    // Island branch's normal payment continuation reducer-certifiable.
    for _ in 0..3 {
        scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db);
    }
    let forest = scenario.add_real_card(P0, "Forest", Zone::Library, db);
    let island = scenario.add_real_card(P0, "Island", Zone::Library, db);
    // These earlier hand cards ensure certification is not limited by hand
    // order: the beneficial Phantom route must remain reachable after other
    // castable alternatives have been considered.
    scenario.add_real_card(P0, "Goblin Piker", Zone::Hand, db);
    scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    let phantom = scenario.add_real_card(P0, "Phantom Monster", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    (runner.state().clone(), misty, forest, island, phantom)
}

fn misty_candidate(
    state: &engine::types::game_state::GameState,
    misty: ObjectId,
) -> CandidateAction {
    validated_candidate_actions_for_semantic_owner(state, P0)
        .into_iter()
        .find(|candidate| {
            matches!(
                candidate.action,
                GameAction::ActivateAbility { source_id, .. } if source_id == misty
            )
        })
        .expect("Misty Rainforest's real activated ability must be a validated root candidate")
}

#[test]
fn prospective_fetchland_certificate_prefers_island_and_commits_phantom_monster() {
    let (state, misty, forest, island, phantom) = fetchland_state();
    let fetch = misty_candidate(&state, misty);
    let cast = ObjectIdentityBinding::new(
        ObjectIncarnationRef::from_object(&state.objects[&phantom]),
        Zone::Hand,
    );

    let GameAction::ActivateAbility { ability_index, .. } = &fetch.action else {
        panic!("the validated Misty root must be an activation");
    };

    // The generic deterministic tutor route takes the first actual search
    // card, Forest, which cannot pay Phantom Monster's printed {3}{U} cost.
    let mut generic = GameRunner::from_state(state.clone());
    generic
        .activate(misty, *ability_index)
        .search_first_legal()
        .resolve();
    assert_eq!(
        generic.state().objects[&forest].zone,
        Zone::Battlefield,
        "the generic SearchChoice policy takes the first legal Forest"
    );
    assert_eq!(
        generic.state().objects[&island].zone,
        Zone::Library,
        "the generic SearchChoice policy leaves Island unfetched"
    );

    // The scored certificate follows that same frozen root through the reducer,
    // evaluates each real SearchChoice, and picks Island because it can commit
    // Phantom Monster to the stack.
    let casts: Vec<_> = state.players[P0.0 as usize]
        .hand
        .iter()
        .map(|object_id| {
            ObjectIdentityBinding::new(
                ObjectIncarnationRef::from_object(&state.objects[object_id]),
                Zone::Hand,
            )
        })
        .collect();
    assert_eq!(casts.last(), Some(&cast));
    let (prompt, _) = certify_fetch_then_cast(&state, &fetch, &casts, |terminal, selected_cast| {
        if terminal.objects[&island].zone == Zone::Battlefield
            && terminal.objects[&phantom].zone == Zone::Stack
            && *selected_cast == cast
        {
            1.0
        } else {
            0.0
        }
    })
    .expect("the real fetch-then-cast route must be reducer-certifiable");
    let mut real = GameRunner::from_state(state.clone());
    real.activate(misty, *ability_index).resolve();
    let scored_pick = prompt
        .action_for(real.state(), P0)
        .expect("the certified token must match the real SearchChoice");
    let follow_up = prompt.follow_up();
    assert_eq!(
        scored_pick,
        GameAction::SelectCards {
            cards: vec![island]
        }
    );
    real.act(scored_pick)
        .expect("certified Island selection applies");
    let cast_action = follow_up
        .action_for(real.state(), P0)
        .expect("the exact post-search state unlocks the certified Phantom Monster cast");
    assert!(matches!(cast_action, GameAction::CastSpell { object_id, .. } if object_id == phantom));
    real.act(cast_action)
        .expect("ordinary cast commits Phantom Monster");
    assert_eq!(real.state().objects[&phantom].zone, Zone::Stack);
}

/// Misty Rainforest, three Lotus Petals and an Island to fetch, with
/// Leatherback Baloth ({G}{G}{G}) in hand. After the Island arrives only the
/// Petals ("{T}, Sacrifice this artifact: Add one mana of any color.") can pay
/// the green, so the cast stops for its sacrificial payment and the
/// prospective payment search has to tap Petals and choose colors.
fn sacrificial_payment_state() -> (
    engine::types::game_state::GameState,
    ObjectId,
    ObjectId,
    Vec<ObjectId>,
    ObjectId,
) {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let misty = scenario.add_real_card(P0, "Misty Rainforest", Zone::Battlefield, db);
    let petals = (0..3)
        .map(|_| scenario.add_real_card(P0, "Lotus Petal", Zone::Battlefield, db))
        .collect();
    let island = scenario.add_real_card(P0, "Island", Zone::Library, db);
    let baloth = scenario.add_real_card(P0, "Leatherback Baloth", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    (runner.state().clone(), misty, island, petals, baloth)
}

#[test]
fn prospective_payment_search_spends_one_shared_application_allowance() {
    let (state, misty, island, petals, baloth) = sacrificial_payment_state();
    let fetch = misty_candidate(&state, misty);
    let GameAction::ActivateAbility { ability_index, .. } = &fetch.action else {
        panic!("the validated Misty root must be an activation");
    };

    // Reach guard: after the real fetch, the Baloth cast stops for its
    // sacrificial payment, which is the board the payment search explores.
    let mut real = GameRunner::from_state(state.clone());
    real.activate(misty, *ability_index).resolve();
    real.act(GameAction::SelectCards {
        cards: vec![island],
    })
    .expect("Island is a legal Misty Rainforest search choice");
    assert_eq!(
        real.state().objects[&island].zone,
        Zone::Battlefield,
        "the certify route skips a fetch whose card did not reach the battlefield"
    );
    let cast = validated_candidate_actions_for_semantic_owner(real.state(), P0)
        .into_iter()
        .find(
            |c| matches!(c.action, GameAction::CastSpell { object_id, .. } if object_id == baloth),
        )
        .expect("Leatherback Baloth is castable with the Island and three Petals");
    assert_eq!(
        cast.metadata.tactical_class,
        TacticalClass::Spell,
        "cast_bound_card_terminal only casts a Spell-class candidate"
    );
    assert!(matches!(
        cast.action,
        GameAction::CastSpell {
            payment_mode: CastPaymentMode::AutoExceptSacrificialMana,
            ..
        }
    ));
    real.act(cast.action).expect("the Baloth cast begins");
    let WaitingFor::ManaSourceSelection { options, .. } = &real.state().waiting_for else {
        panic!(
            "the cast must stop for the sacrificial Petal payment, got {:?}",
            real.state().waiting_for
        );
    };
    assert_eq!(options.len(), petals.len());

    // Positive control (CR 601.2g + CR 605.3a): the cast is payable. Each Petal
    // makes green and the payment finalizes, so a `None` below is the search
    // bound, not an unpayable cost.
    for _ in 0..16 {
        let candidates = validated_candidate_actions_for_semantic_owner(real.state(), P0);
        let step = match &real.state().waiting_for {
            WaitingFor::ManaSourceSelection { .. } => candidates
                .into_iter()
                .find(|c| matches!(c.action, GameAction::ActivateManaSource { .. })),
            WaitingFor::ChooseManaColor { .. } => candidates.into_iter().find(|c| {
                matches!(c.action, GameAction::ChooseManaColor { choice: ManaChoice::SingleColor(ManaType::Green), .. })
            }),
            WaitingFor::ManaPayment { .. } => candidates
                .iter()
                .find(|c| matches!(c.action, GameAction::ActivateAbility { source_id, .. } if petals.contains(&source_id)))
                .or_else(|| candidates.iter().find(|c| matches!(c.action, GameAction::PassPriority)))
                .cloned(),
            _ => break,
        };
        let step = step.expect("every payment prompt offers the step that pays Baloth");
        real.act(step.action).expect("each payment step is legal");
    }
    assert_eq!(real.state().objects[&baloth].zone, Zone::Stack);
    assert!(matches!(real.state().waiting_for, WaitingFor::Priority { player } if player == P0));

    // Regression: the first Petal's first color (White) leads to a subtree
    // with no way to pay {G}{G}{G} and more than 64 applications. The search
    // must stop at its shared allowance and fail closed. Before the fix it
    // decremented the exhausted counter (overflow panic in the test profile,
    // an unbounded walk in release).
    let casts = vec![ObjectIdentityBinding::new(
        ObjectIncarnationRef::from_object(&state.objects[&baloth]),
        Zone::Hand,
    )];
    let mut scored = 0usize;
    let certificate = certify_fetch_then_cast(&state, &fetch, &casts, |_, _| {
        scored += 1;
        1.0
    });
    assert!(certificate.is_none());
    assert_eq!(scored, 0, "no payment-search terminal is certified");
}
