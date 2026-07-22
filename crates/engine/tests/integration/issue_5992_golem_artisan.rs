//! Issue #5992 — Golem Artisan's "gains your choice of flying, trample, or
//! haste" activated ability.
//!
//! Verbatim Scryfall Oracle (Golem Artisan, {5}, Commander Legends, a 3/3
//! Artifact Creature — Golem):
//!
//!   {2}: Target artifact creature gets +1/+1 until end of turn.
//!   {2}: Target artifact creature gains your choice of flying, trample, or haste
//!        until end of turn.
//!
//! The second ability is a THREE-way Oxford-comma keyword choice. On `main`,
//! `parse_keyword_choice_grant` only handled a binary "X or Y" split, so this
//! 3-way list failed to parse and the ability fell through to `Unimplemented` —
//! no branch ever surfaced. The fix routes the choice list through the nom-based
//! `split_choice_list_items` splitter so an N-ary list parses into N branches.
//!
//! These are REAL end-to-end runtime tests: they activate the ability, drive the
//! surfaced `ChooseOneOfBranch` prompt to completion, and assert the chosen
//! keyword actually lands on the targeted artifact creature after
//! `evaluate_layers`. `drive_resolution` has no `ChooseOneOfBranch` arm and
//! legitimately halts there — that halt IS the positive proof the 3-way prompt
//! was offered.
//!
//! There is NO "you control" restriction on the target (the real card targets
//! ANY artifact creature). The second test proves this holds through the real
//! targeting-legality pipeline by targeting an OPPONENT-controlled artifact
//! creature.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const GOLEM_ARTISAN_ORACLE: &str = "{2}: Target artifact creature gets +1/+1 until \
     end of turn.\n{2}: Target artifact creature gains your choice of flying, \
     trample, or haste until end of turn.";

/// The choice ability is the SECOND printed ability (index 1); index 0 is the
/// +1/+1 pump.
const CHOICE_ABILITY_INDEX: usize = 1;

/// Two colorless mana — enough to pay the ability's `{2}` activation cost.
fn two_generic() -> Vec<ManaUnit> {
    vec![
        ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
    ]
}

/// Make an already-created creature also an artifact (an ARTIFACT CREATURE),
/// keeping its Creature type. The `CardBuilder::as_artifact` helper strips
/// Creature, so push the core type directly onto both the computed and base
/// type lines (base survives `evaluate_layers`).
fn make_artifact_creature(runner: &mut GameRunner, id: ObjectId) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.base_card_types.core_types.push(CoreType::Artifact);
}

/// After the ability has resolved (state halted at `ChooseOneOfBranch`), choose
/// the branch whose description names `keyword`, then drive priority until the
/// stack empties and layers settle.
fn choose_branch_and_settle(runner: &mut GameRunner, keyword: &str) {
    let index = match &runner.state().waiting_for {
        WaitingFor::ChooseOneOfBranch {
            branch_descriptions,
            branches,
            ..
        } => {
            assert_eq!(
                branches.len(),
                3,
                "the 3-way 'flying, trample, or haste' choice must offer 3 branches, got {:?}",
                branch_descriptions
            );
            branch_descriptions
                .iter()
                .position(|d| d.contains(keyword))
                .unwrap_or_else(|| {
                    panic!("expected a '{keyword}' branch among {branch_descriptions:?}")
                })
        }
        other => {
            panic!("expected a ChooseOneOfBranch prompt (the 3-way keyword choice), got {other:?}")
        }
    };

    runner
        .act(GameAction::ChooseBranch { index })
        .expect("choosing the keyword branch must succeed");

    for _ in 0..16 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() && runner.state().deferred_triggers.is_empty() {
                    evaluate_layers(runner.state_mut());
                    return;
                }
                runner.pass_both_players();
            }
            other => panic!("unexpected waiting state after choosing the branch: {other:?}"),
        }
    }

    panic!("stack/deferred_triggers did not settle within 16 priority passes");
}

/// Self-target: Golem Artisan activates its choice ability targeting ITSELF and
/// gains the chosen keyword (Trample) end-to-end.
#[test]
fn golem_artisan_three_way_choice_grants_trample_to_self() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for pid in [P0, P1] {
        scenario.with_library_top(pid, &["Lib A", "Lib B", "Lib C", "Lib D"]);
    }
    let golem = scenario
        .add_creature_from_oracle(P0, "Golem Artisan", 3, 3, GOLEM_ARTISAN_ORACLE)
        .id();
    scenario.with_mana_pool(P0, two_generic());

    let mut runner = scenario.build();
    make_artifact_creature(&mut runner, golem);

    assert!(
        !runner.state().objects[&golem].has_keyword(&Keyword::Trample),
        "precondition: Golem Artisan has no trample before the ability"
    );

    runner
        .activate(golem, CHOICE_ABILITY_INDEX)
        .target_object(golem)
        .resolve();

    choose_branch_and_settle(&mut runner, "Trample");

    assert!(
        runner
            .state()
            .objects
            .get(&golem)
            .unwrap()
            .has_keyword(&Keyword::Trample),
        "the chosen keyword (trample) must be granted to the targeted artifact creature"
    );
    assert!(
        !runner.state().objects[&golem].has_keyword(&Keyword::Flying),
        "the unchosen keyword (flying) must NOT be granted"
    );
    assert!(
        !runner.state().objects[&golem].has_keyword(&Keyword::Haste),
        "the unchosen keyword (haste) must NOT be granted"
    );
}

/// GAP-FIX: opponent-target. The real card has NO "you control" restriction.
/// Golem Artisan (P0) activates its choice ability targeting an OPPONENT-owned
/// (P1) artifact creature; the targeting must be LEGAL and the chosen keyword
/// (Haste) must land on the opponent's creature. This exercises the real
/// targeting-legality pipeline, not just the parsed `TargetFilter` shape.
#[test]
fn golem_artisan_three_way_choice_can_target_opponent_artifact_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for pid in [P0, P1] {
        scenario.with_library_top(pid, &["Lib A", "Lib B", "Lib C", "Lib D"]);
    }
    let golem = scenario
        .add_creature_from_oracle(P0, "Golem Artisan", 3, 3, GOLEM_ARTISAN_ORACLE)
        .id();
    // A vanilla artifact creature under the OPPONENT's control.
    let opponent_golem = scenario.add_creature(P1, "Opposing Construct", 2, 2).id();
    scenario.with_mana_pool(P0, two_generic());

    let mut runner = scenario.build();
    make_artifact_creature(&mut runner, golem);
    make_artifact_creature(&mut runner, opponent_golem);

    assert!(
        !runner.state().objects[&opponent_golem].has_keyword(&Keyword::Haste),
        "precondition: the opponent's artifact creature has no haste"
    );

    // Activating targeting the OPPONENT's artifact creature must be accepted
    // (no "you control" restriction). If targeting were illegal, `.resolve()`
    // would fail to reach the ChooseOneOfBranch prompt and the helper panics.
    runner
        .activate(golem, CHOICE_ABILITY_INDEX)
        .target_object(opponent_golem)
        .resolve();

    choose_branch_and_settle(&mut runner, "Haste");

    assert!(
        runner
            .state()
            .objects
            .get(&opponent_golem)
            .unwrap()
            .has_keyword(&Keyword::Haste),
        "the chosen keyword (haste) must land on the OPPONENT's targeted artifact creature"
    );
    // Golem Artisan itself was not the target — it stays vanilla.
    assert!(
        !runner.state().objects[&golem].has_keyword(&Keyword::Haste),
        "the untargeted source must not gain the keyword"
    );
}
