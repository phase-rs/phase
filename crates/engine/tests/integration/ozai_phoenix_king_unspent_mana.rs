use engine::game::keywords::has_keyword;
use engine::game::layers::flush_layers;
use engine::game::mana_payment;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::turns::advance_phase;
use engine::types::ability::{ContinuousModification, Duration, TargetFilter};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType};
use engine::types::phase::Phase;

const OZAI_ORACLE: &str = "Trample, firebending 4, haste\nIf you would lose unspent mana, that mana becomes red instead.\nOzai has flying and indestructible as long as you have six or more unspent mana.";
const OPT_ORACLE: &str = "Scry 1.\nDraw a card.";

fn produce_blue(
    runner: &mut GameRunner,
    source: ObjectId,
    player: engine::types::player::PlayerId,
    count: usize,
) {
    for _ in 0..count {
        let mut events = Vec::new();
        mana_payment::produce_mana(
            runner.state_mut(),
            source,
            ManaType::Blue,
            player,
            false,
            &mut events,
        );
    }
}

fn has_keyword_after_layers(runner: &mut GameRunner, object: ObjectId, keyword: &Keyword) -> bool {
    flush_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&object], keyword)
}

#[test]
fn ozai_unspent_mana_gate_tracks_live_controller_and_cast_payment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Drawn Card"]);

    let ozai = scenario
        .add_creature(P0, "Ozai, the Phoenix King", 4, 4)
        .from_oracle_text_with_keywords(&["trample", "firebending", "haste"], OZAI_ORACLE)
        .id();
    let p1_mana_source = scenario.add_creature(P1, "P1 Mana Source", 1, 1).id();
    let thief = scenario
        .add_creature(P1, "Control Effect Source", 1, 1)
        .id();
    let opt = scenario
        .add_spell_to_hand_from_oracle(P0, "Opt", true, OPT_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();

    produce_blue(&mut runner, p1_mana_source, P1, 6);
    assert!(
        !has_keyword_after_layers(&mut runner, ozai, &Keyword::Flying),
        "P1's mana must not enable P0's Ozai"
    );
    assert!(
        !has_keyword_after_layers(&mut runner, ozai, &Keyword::Indestructible),
        "the negative controller fixture must reach Ozai's parsed gate"
    );

    produce_blue(&mut runner, ozai, P0, 6);
    assert!(
        has_keyword_after_layers(&mut runner, ozai, &Keyword::Flying),
        "six unspent mana enables flying"
    );
    assert!(
        has_keyword_after_layers(&mut runner, ozai, &Keyword::Indestructible),
        "six unspent mana enables indestructible"
    );
    assert!(has_keyword_after_layers(
        &mut runner,
        ozai,
        &Keyword::Trample
    ));
    assert!(has_keyword_after_layers(&mut runner, ozai, &Keyword::Haste));

    let outcome = runner.cast(opt).resolve();
    outcome.assert_hand_drawn(P0, 1);

    assert!(
        !has_keyword_after_layers(&mut runner, ozai, &Keyword::Flying),
        "paying one mana through the casting pipeline drops Ozai below six"
    );
    assert!(
        !has_keyword_after_layers(&mut runner, ozai, &Keyword::Indestructible),
        "both conditional grants must be invalidated at five mana"
    );
    assert!(
        has_keyword_after_layers(&mut runner, ozai, &Keyword::Trample),
        "printed trample must survive conditional-grant invalidation"
    );
    assert!(
        has_keyword_after_layers(&mut runner, ozai, &Keyword::Haste),
        "printed haste must survive conditional-grant invalidation"
    );

    runner.state_mut().add_transient_continuous_effect(
        thief,
        P1,
        Duration::Permanent,
        TargetFilter::SpecificObject { id: ozai },
        vec![ContinuousModification::ChangeController],
        None,
    );
    flush_layers(runner.state_mut());
    assert_eq!(runner.state().objects[&ozai].controller, P1);
    assert!(
        has_keyword(&runner.state().objects[&ozai], &Keyword::Flying),
        "one layer flush must evaluate Ozai's layer-6 gate with P1 as controller"
    );
    assert!(
        has_keyword(&runner.state().objects[&ozai], &Keyword::Indestructible,),
        "P1's six mana enables the stolen Ozai"
    );
}

#[test]
fn ozai_recolors_unspent_mana_when_entering_cleanup() {
    // CR 500.5 + CR 614.1a: Ozai replaces the loss of its controller's
    // unspent mana at the cleanup-step boundary, preserving the six-mana gate.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let ozai = scenario
        .add_creature(P0, "Ozai, the Phoenix King", 4, 4)
        .from_oracle_text_with_keywords(&["trample", "firebending", "haste"], OZAI_ORACLE)
        .id();
    let mut runner = scenario.build();

    produce_blue(&mut runner, ozai, P0, 6);
    assert!(has_keyword_after_layers(
        &mut runner,
        ozai,
        &Keyword::Flying
    ));
    assert!(has_keyword_after_layers(
        &mut runner,
        ozai,
        &Keyword::Indestructible
    ));

    advance_phase(runner.state_mut(), &mut Vec::new());

    assert_eq!(runner.state().phase, Phase::Cleanup);
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 6);
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Red),
        6,
        "Ozai turns would-be-lost mana red rather than letting it empty"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Blue),
        0
    );
    assert!(has_keyword_after_layers(
        &mut runner,
        ozai,
        &Keyword::Flying
    ));
    assert!(has_keyword_after_layers(
        &mut runner,
        ozai,
        &Keyword::Indestructible
    ));
}
