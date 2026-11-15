//! Tests relocated from `src/parser/oracle_static/tests.rs`.

use engine::types::keywords::Keyword;
use engine::types::phase::Phase;

/// #5257 Rayami runtime discriminator: the keyword grant is CONDITIONAL and
/// re-evaluated (CR 611.3a). With no matching exiled card, Rayami must NOT have
/// flying; once an exiled creature card with a blood counter that itself has
/// flying is present, Rayami gains flying — and only that keyword.
///
/// Discriminator: on `main` the condition is `Unrecognized`, so Rayami has
/// flying (and every other listed keyword) unconditionally and the negative
/// assertion flips.
#[test]
fn rayami_flying_grant_is_conditional_on_matching_exiled_card() {
    use engine::game::scenario::{GameScenario, P0};
    use engine::game::zones::create_object;
    use engine::types::actions::GameAction;
    use engine::types::card_type::CoreType;
    use engine::types::counter::CounterType;
    use engine::types::identifiers::CardId;
    use engine::types::zones::Zone;

    const RAYAMI: &str = "As long as an exiled creature card with a blood counter on it has flying, Rayami has flying. The same is true for first strike, double strike, deathtouch, haste, hexproof, indestructible, lifelink, menace, protection, reach, trample, and vigilance.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let rayami_id = scenario
        .add_creature_from_oracle(P0, "Rayami, First of the Fallen", 4, 4, RAYAMI)
        .id();
    let mut runner = scenario.build();
    // Force layer evaluation (SBAs run, continuous effects recomputed).
    runner.act(GameAction::PassPriority).ok();

    assert!(
        !runner.state().objects[&rayami_id].has_keyword(&Keyword::Flying),
        "Rayami must not have flying with no matching exiled card present"
    );

    // Positive reach-guard: an exiled creature card with a blood counter that has
    // flying. Rayami now gains flying — proving the grant path actually fires, so
    // the negative above is a genuine conditional rather than flying being
    // unreachable. (Seed the object directly, then re-run the continuous-effect
    // engine — a raw state edit bypasses the normal change tracking.)
    {
        let state = runner.state_mut();
        let exiled = create_object(
            state,
            CardId(900),
            P0,
            "Exiled Bat".to_string(),
            Zone::Exile,
        );
        let obj = state.objects.get_mut(&exiled).expect("exiled card exists");
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.keywords.push(Keyword::Flying);
        *obj.counters
            .entry(CounterType::Generic("blood".to_string()))
            .or_insert(0) += 1;
    }
    engine::game::layers::evaluate_layers(runner.state_mut());

    assert!(
        runner.state().objects[&rayami_id].has_keyword(&Keyword::Flying),
        "Rayami must gain flying while an exiled flying creature card with a blood counter is present"
    );
    assert!(
        !runner.state().objects[&rayami_id].has_keyword(&Keyword::Vigilance),
        "Rayami must not gain vigilance — no exiled card has vigilance"
    );
}
