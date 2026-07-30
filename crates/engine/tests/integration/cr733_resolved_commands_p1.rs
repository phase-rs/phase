//! P1 runtime coverage for resolved-command mana provenance.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::visibility::filter_state_for_viewer;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::resolved_commands::{
    ManaPaymentRecipient, RulesExecutionNodeKind, RulesExecutionNodeRef,
};

const DIMIR_SIGNET_ORACLE: &str = "{1}, {T}: Add {U}{B}.";

fn make_artifact(runner: &mut GameRunner, id: ObjectId) {
    let object = runner.state_mut().objects.get_mut(&id).unwrap();
    object.card_types.core_types = vec![CoreType::Artifact];
    object.base_card_types = object.card_types.clone();
    object.power = None;
    object.toughness = None;
    object.base_power = None;
    object.base_toughness = None;
}

/// P1 must observe the engine's real mana-ability path, not a hand-built
/// journal: auto-tapping a basic land pays the Signet's cost, then the Signet
/// produces two new units. The consumed land unit must retain its exact pip,
/// producer node, and recipient identity.
#[test]
fn real_mana_activation_records_exact_produced_and_spent_units() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario.add_basic_land(P0, ManaColor::White);
    let signet = scenario
        .add_creature_from_oracle(P0, "Dimir Signet", 0, 0, DIMIR_SIGNET_ORACLE)
        .id();

    let mut runner = scenario.build();
    make_artifact(&mut runner, signet);
    runner
        .act(GameAction::ActivateAbility {
            source_id: signet,
            ability_index: 0,
        })
        .expect("the real Signet mana ability must activate");

    let state = runner.state();
    let journal = &state.resolved_rules_journal;
    let spent = journal
        .spent_mana()
        .first()
        .expect("the Signet's generic cost consumes the land's exact mana unit");
    assert_ne!(spent.unit.pip_id.0, 0, "consumed mana must be stamped");
    assert_eq!(spent.unit.source_id, land);
    assert!(matches!(
        spent.producer,
        RulesExecutionNodeRef::ActivatedMana(_)
    ));
    assert_eq!(
        spent.recipient,
        ManaPaymentRecipient::Object(ObjectIncarnationRef::from_object(&state.objects[&signet])),
        "the payment recipient is the exact Signet incarnation"
    );
    assert!(
        journal.produced_mana().iter().any(|record| {
            record.unit.pip_id == spent.unit.pip_id
                && record.unit == spent.unit
                && record.producer == spent.producer
        }),
        "every spent pip has exactly the produced record from its producer node"
    );

    let payment = journal
        .nodes()
        .iter()
        .find(|node| node.identity == spent.payment)
        .expect("spent unit's payment node exists");
    assert_eq!(payment.depends_on, vec![spent.producer]);
    assert!(matches!(
        &payment.kind,
        RulesExecutionNodeKind::Payment { .. }
    ));
    let land_node = journal
        .nodes()
        .iter()
        .find(|node| node.identity == spent.producer)
        .expect("land producer node exists");
    assert!(
        matches!(
            &land_node.kind,
            RulesExecutionNodeKind::ActivatedMana { source }
                if *source == ObjectIncarnationRef::from_object(&state.objects[&land])
        ),
        "the nested land activation keeps its exact source identity"
    );
    let signet_node = journal
        .nodes()
        .iter()
        .find(|node| {
            matches!(
                &node.kind,
                RulesExecutionNodeKind::ActivatedMana { source }
                    if *source == ObjectIncarnationRef::from_object(&state.objects[&signet])
            )
        })
        .expect("the Signet activation has its own node");
    assert_eq!(land_node.caused_by, Some(signet_node.identity));
}

/// P1 retention policy: the journal is bounded to one turn. A turn transition
/// cannot begin with a payment in flight, so truncating at the boundary is
/// safe until the CR 733 settlement consumer defines the real window.
#[test]
fn provenance_journal_is_truncated_at_the_turn_boundary() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();
    let _ = runner.state_mut().add_mana_to_pool(
        P0,
        ManaUnit::new(ManaType::Green, ObjectId(99), false, Vec::new()),
    );
    assert!(
        !runner
            .state()
            .resolved_rules_journal
            .produced_mana()
            .is_empty(),
        "journal must have provenance before the boundary"
    );
    runner.state_mut().players[0].mana_pool.clear();

    let start_turn = runner.state().turn_number;
    let mut guard = 0;
    while runner.state().turn_number == start_turn {
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority must advance an empty-stack game");
        guard += 1;
        assert!(
            guard < 200,
            "turn must roll over within a bounded pass count"
        );
    }
    assert!(
        runner
            .state()
            .resolved_rules_journal
            .produced_mana()
            .is_empty()
            && runner.state().resolved_rules_journal.nodes().is_empty(),
        "the turn boundary must truncate the provenance journal"
    );
}

/// A pre-provenance save deserializes `next_pip_id` to 0 (serde default). The
/// allocator must self-heal rather than mint the ManaPipId(0) sentinel, which
/// the resolved-mana appliers fail closed on (this panicked the phase-ai
/// community scenarios in PR #6331's first CI run).
#[test]
fn legacy_zero_pip_allocator_self_heals_instead_of_minting_the_sentinel() {
    let mut state = GameState::new_two_player(11);
    state.next_pip_id = 0;
    let inserted = state
        .add_mana_to_pool(
            P0,
            ManaUnit::new(ManaType::Red, ObjectId(7), false, Vec::new()),
        )
        .expect("insert into a known player's pool must succeed");
    assert_ne!(
        inserted.pip_id.0, 0,
        "a legacy zero allocator must never stamp the unstamped sentinel"
    );
}

#[test]
fn provenance_journal_is_not_exposed_in_a_viewer_projection() {
    let mut state = GameState::new_two_player(11);
    let _ = state.add_mana_to_pool(
        P0,
        ManaUnit::new(ManaType::Green, ObjectId(99), false, Vec::new()),
    );
    assert!(
        !state.resolved_rules_journal.produced_mana().is_empty(),
        "authoritative state has provenance to redact"
    );

    let opponent_view = filter_state_for_viewer(&state, PlayerId(1));
    assert!(opponent_view.resolved_rules_journal.nodes().is_empty());
    assert!(opponent_view
        .resolved_rules_journal
        .produced_mana()
        .is_empty());
    assert!(opponent_view.resolved_rules_journal.spent_mana().is_empty());
}
