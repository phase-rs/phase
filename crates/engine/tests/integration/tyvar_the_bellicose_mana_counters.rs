use engine::game::interaction::{
    bind_interaction_authority, derive_viewer_interaction, interaction_action_id,
};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::ChosenAttribute;
use engine::types::actions::GameAction;
use engine::types::events::{GameEvent, ManaResolutionShare};
use engine::types::game_state::{
    ActionResult, CastPaymentMode, LoopDetectionMode, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::interaction::{
    InteractionOpportunityResponse, InteractionPresentationSurface, InteractionSessionId,
};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::{CoreType, CounterType};

const TYVAR: &str = concat!(
    "Whenever one or more Elves you control attack, they gain deathtouch until end of turn.\n",
    "Each creature you control has \"Whenever a mana ability of this creature resolves, ",
    "put a number of +1/+1 counters on it equal to the amount of mana this creature produced. ",
    "This ability triggers only once each turn.\""
);

fn tyvar_board(phase: Phase) -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(phase);
    let tyvar = scenario
        .add_creature_from_oracle(P0, "Tyvar the Bellicose", 5, 4, TYVAR)
        .with_subtypes(vec!["Elf", "Warrior"])
        .id();
    (scenario, tyvar)
}

fn llanowar_elves(scenario: &mut GameScenario, controller: PlayerId) -> ObjectId {
    scenario
        .add_creature_from_oracle(controller, "Llanowar Elves", 1, 1, "{T}: Add {G}.")
        .with_subtypes(vec!["Elf", "Druid"])
        .id()
}

fn one_mana(shard: ManaCostShard) -> ManaCost {
    ManaCost::Cost {
        shards: vec![shard],
        generic: 0,
    }
}

fn seed_mana(scenario: &mut GameScenario, colors: &[ManaType]) {
    scenario.with_mana_pool(
        P0,
        colors
            .iter()
            .map(|&color| ManaUnit::new(color, ObjectId(0), false, vec![]))
            .collect(),
    );
}

fn vitalize(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Vitalize", true, "Untap all creatures you control.")
        .with_mana_cost(one_mana(ManaCostShard::Green))
        .id()
}

fn activate(runner: &mut GameRunner, source_id: ObjectId) -> ActionResult {
    runner
        .act(GameAction::ActivateAbility {
            source_id,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate the creature's mana ability")
}

fn counters(runner: &GameRunner, source: ObjectId) -> u32 {
    runner.state().objects[&source]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn assert_top_trigger(runner: &GameRunner, source: ObjectId) {
    assert!(
        matches!(
            runner.state().stack.back().map(|entry| &entry.kind),
            Some(StackEntryKind::TriggeredAbility {
                source_id,
                trigger_event: Some(GameEvent::ManaAbilityProduced { source_id: mana_source, .. }),
                ..
            }) if *source_id == source && *mana_source == source
        ),
        "expected the creature's granted trigger on top of the stack"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

fn resolve_only_trigger(runner: &mut GameRunner, source: ObjectId, expected: u32) {
    assert_eq!(runner.state().stack.len(), 1);
    assert_top_trigger(runner, source);
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(counters(runner, source), expected);
}

fn production_shares(
    events: &[GameEvent],
    source: ObjectId,
) -> Vec<(PlayerId, Vec<ManaType>, ManaResolutionShare)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::ManaAbilityProduced {
                player_id,
                source_id,
                produced,
                resolution,
                ..
            } if *source_id == source => Some((*player_id, produced.clone(), *resolution)),
            _ => None,
        })
        .collect()
}

/// CR 113.7 + CR 611.3a: Each creature has its own grant and turn limit (Tyvar ruling 1).
#[test]
fn tyvar_limits_the_grant_per_creature_per_turn() {
    let (mut scenario, _) = tyvar_board(Phase::End);
    let first = llanowar_elves(&mut scenario, P0);
    let second = llanowar_elves(&mut scenario, P0);
    let opponent = llanowar_elves(&mut scenario, P1);
    let untap = vitalize(&mut scenario);
    let untap_next_turn = vitalize(&mut scenario);
    seed_mana(&mut scenario, &[ManaType::Green]);
    // The initial pool empties before P1's upkeep.
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    let turn = runner.state().turn_number;

    activate(&mut runner, first);
    resolve_only_trigger(&mut runner, first, 1);
    assert_eq!(counters(&runner, second), 0);
    activate(&mut runner, second);
    resolve_only_trigger(&mut runner, second, 1);
    assert_eq!(counters(&runner, first), 1);

    runner
        .cast(untap)
        .resolve()
        .assert_zone(&[untap], Zone::Graveyard);
    assert!(!runner.state().objects[&first].tapped);
    let repeated = activate(&mut runner, first);
    assert_eq!(production_shares(&repeated.events, first).len(), 1);
    assert!(runner.state().objects[&first].tapped);
    assert!(
        runner.state().stack.is_empty(),
        "the same-turn trigger was spent"
    );
    assert_eq!(counters(&runner, first), 1);

    runner
        .act(GameAction::PassPriority)
        .expect("give P1 priority");
    let opposing = activate(&mut runner, opponent);
    assert_eq!(production_shares(&opposing.events, opponent).len(), 1);
    assert!(
        runner.state().stack.is_empty(),
        "P1's creature has no grant"
    );
    assert_eq!(counters(&runner, opponent), 0);

    runner.advance_to_upkeep();
    assert_eq!(runner.state().phase, Phase::Upkeep);
    assert_eq!(runner.state().turn_number, turn + 1);
    if matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1) {
        runner
            .act(GameAction::PassPriority)
            .expect("give P0 priority in P1's upkeep");
    }
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0));
    tap_land(&mut runner, forest);
    runner
        .cast(untap_next_turn)
        .resolve()
        .assert_zone(&[untap_next_turn], Zone::Graveyard);
    assert!(!runner.state().objects[&first].tapped);
    // CR 117.3b: P1 receives priority after the untap spell resolves.
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1));
    runner
        .act(GameAction::PassPriority)
        .expect("give P0 priority");
    activate(&mut runner, first);
    resolve_only_trigger(&mut runner, first, 2);
}

/// CR 605.1a + CR 608.2p: The discard-cost ability can trigger Tyvar’s grant.
#[test]
fn tyvar_triggers_for_a_discard_cost_mana_ability() {
    let (mut scenario, _) = tyvar_board(Phase::PreCombatMain);
    let skirge = scenario
        .add_creature_from_oracle(
            P0,
            "Skirge Familiar",
            3,
            2,
            "Flying\nDiscard a card: Add {B}.",
        )
        .id();
    let discard = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .id();
    let mut runner = scenario.build();

    let prompt = activate(&mut runner, skirge);
    assert!(
        matches!(prompt.waiting_for, WaitingFor::PayCost { ref choices, .. } if choices.as_slice() == [discard])
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![discard],
        })
        .expect("discard the only choice");
    assert_eq!(runner.state().objects[&discard].zone, Zone::Graveyard);
    assert!(!runner.state().objects[&skirge].tapped);
    resolve_only_trigger(&mut runner, skirge, 1);
}

/// CR 603.2c + CR 608.2i: One resolution supplies the full mana quantity.
#[test]
fn tyvar_counts_multiple_mana_from_one_resolution() {
    let (mut scenario, _) = tyvar_board(Phase::PreCombatMain);
    let elder = scenario
        .add_creature_from_oracle(P0, "Fyndhorn Elder", 1, 1, "{T}: Add {G}{G}.")
        .id();
    let mut runner = scenario.build();
    activate(&mut runner, elder);
    resolve_only_trigger(&mut runner, elder, 2);
}

/// CR 608.2c + CR 608.2p: One resolution includes both printed mana instructions.
#[test]
fn tyvar_counts_mana_deposits_from_the_resolution_tail() {
    let (mut scenario, _) = tyvar_board(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(
        P0,
        "March of the Machines",
        "Each noncreature artifact is an artifact creature with power and toughness each equal to its mana value. (Equipment that's a creature can't equip a creature.)",
    );
    let battery = scenario.add_artifact_from_oracle(
        P0, "Black Mana Battery",
        "{2}, {T}: Put a charge counter on this artifact.\n{T}, Remove any number of charge counters from this artifact: Add {B}, then add an additional {B} for each charge counter removed this way.",
    ).with_mana_cost(ManaCost::Cost { shards: vec![], generic: 4 }).id();
    scenario.with_counter(battery, engine::types::parse_counter_type("charge"), 2);
    let mut runner = scenario.build();

    let prompt = runner
        .act(GameAction::ActivateAbility {
            source_id: battery,
            ability_index: 1,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate the animated Battery's second ability");
    assert!(matches!(
        prompt.waiting_for,
        WaitingFor::PayAmountChoice { .. }
    ));
    let result = runner
        .act(GameAction::SubmitPayAmount { amount: 2 })
        .expect("remove two charge counters");
    assert_eq!(
        runner.state().players[0]
            .mana_pool
            .count_color(ManaType::Black),
        3
    );
    assert_eq!(
        production_shares(&result.events, battery),
        vec![(
            P0,
            vec![ManaType::Black],
            ManaResolutionShare::WholeResolution { total: 3 }
        ),]
    );
    resolve_only_trigger(&mut runner, battery, 3);
}

/// CR 106.12b + CR 614.1a: Count what was deposited after the replacement.
#[test]
fn tyvar_counts_replaced_mana_and_preserves_base_production() {
    let (mut scenario, _) = tyvar_board(Phase::PreCombatMain);
    let elf = llanowar_elves(&mut scenario, P0);
    scenario.add_creature_from_oracle(
        P0,
        "Nyxbloom Ancient",
        5,
        5,
        "Trample\nIf you tap a permanent for mana, it produces three times as much of that mana instead.",
    );
    let mut runner = scenario.build();

    let result = activate(&mut runner, elf);
    assert_eq!(
        production_shares(&result.events, elf),
        vec![(
            P0,
            vec![ManaType::Green],
            ManaResolutionShare::WholeResolution { total: 3 }
        ),]
    );
    assert_eq!(
        runner.state().players[0]
            .mana_pool
            .count_color(ManaType::Green),
        3
    );
    resolve_only_trigger(&mut runner, elf, 3);
}

/// CR 101.4 + CR 603.2c: Both recipients get mana from one resolution.
#[test]
fn tyvar_counts_all_yurlok_recipients_in_one_trigger() {
    let (mut scenario, _) = tyvar_board(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let yurlok = scenario.add_creature_from_oracle(
        P0, "Yurlok of Scorch Thrash", 4, 4,
        "Vigilance\nA player losing unspent mana causes that player to lose that much life.\n{1}, {T}: Each player adds {B}{R}{G}.",
    ).id();
    let mut runner = scenario.build();

    let result = activate(&mut runner, yurlok);
    let brg = vec![ManaType::Black, ManaType::Red, ManaType::Green];
    assert_eq!(
        production_shares(&result.events, yurlok),
        vec![
            (
                P0,
                brg.clone(),
                ManaResolutionShare::WholeResolution { total: 6 }
            ),
            (P1, brg, ManaResolutionShare::RecipientOnly),
        ]
    );
    resolve_only_trigger(&mut runner, yurlok, 6);
}

/// CR 608.2p: A mana ability that adds zero mana still resolves and triggers the grant.
#[test]
fn tyvar_spends_the_turn_limit_on_zero_output() {
    let (mut scenario, _) = tyvar_board(Phase::PreCombatMain);
    // A choice-free counted producer isolates zero output from a suspended activation.
    let producer = scenario
        .add_creature(P0, "Gyre Sage", 1, 2)
        .from_oracle_text_with_keywords(
            &["Evolve"],
            "Evolve (Whenever a creature you control enters, if that creature has greater power or toughness than this creature, put a +1/+1 counter on this creature.)\n{T}: Add {G} for each +1/+1 counter on this creature.",
        )
        .with_subtypes(vec!["Elf", "Druid"])
        .id();
    let growth = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Battlegrowth",
            true,
            "Put a +1/+1 counter on target creature.",
        )
        .with_mana_cost(one_mana(ManaCostShard::Green))
        .id();
    let untap = vitalize(&mut scenario);
    seed_mana(&mut scenario, &[ManaType::Green; 2]);
    let mut runner = scenario.build();

    let empty = activate(&mut runner, producer);
    assert_eq!(
        production_shares(&empty.events, producer),
        vec![(
            P0,
            vec![],
            ManaResolutionShare::WholeResolution { total: 0 }
        )]
    );
    assert_eq!(runner.state().players[0].mana_pool.total(), 2);
    resolve_only_trigger(&mut runner, producer, 0);

    runner
        .cast(growth)
        .target_object(producer)
        .resolve()
        .assert_zone(&[growth], Zone::Graveyard);
    assert_eq!(counters(&runner, producer), 1);
    runner
        .cast(untap)
        .resolve()
        .assert_zone(&[untap], Zone::Graveyard);
    assert!(!runner.state().objects[&producer].tapped);

    let positive = activate(&mut runner, producer);
    assert_eq!(
        production_shares(&positive.events, producer),
        vec![(
            P0,
            vec![ManaType::Green],
            ManaResolutionShare::WholeResolution { total: 1 }
        )]
    );
    assert_eq!(
        runner.state().players[0]
            .mana_pool
            .count_color(ManaType::Green),
        1
    );
    assert!(
        runner.state().stack.is_empty(),
        "the zero-output trigger spent the limit"
    );
    assert!(matches!(positive.waiting_for, WaitingFor::Priority { player } if player == P0));
    assert_eq!(counters(&runner, producer), 1);
}

/// CR 608.2p: A mana ability that pauses for a reveal still triggers the grant when it finishes.
#[test]
fn tyvar_triggers_after_a_mana_ability_resumes_from_a_reveal() {
    let (mut scenario, _) = tyvar_board(Phase::PreCombatMain);
    let metalworker = scenario.add_creature_from_oracle(
        P0, "Metalworker", 1, 2,
        "{T}: Reveal any number of artifact cards in your hand. Add {C}{C} for each card revealed this way.",
    ).as_artifact_creature().id();
    let artifact = scenario
        .add_creature_to_hand_from_oracle(P0, "Ornithopter", 0, 2, "Flying")
        .as_artifact_creature()
        .with_subtypes(vec!["Thopter"])
        .id();
    let mut runner = scenario.build();

    let prompt = activate(&mut runner, metalworker);
    assert!(matches!(
        prompt.waiting_for,
        WaitingFor::RevealChoice { player, cards: ref choices, pending_mana_ability: Some(_), .. }
            if player == P0 && choices.as_slice() == [artifact]
    ));
    let result = runner
        .act(GameAction::SelectCards {
            cards: vec![artifact],
        })
        .expect("reveal the artifact card");
    assert_eq!(
        production_shares(&result.events, metalworker),
        vec![(
            P0,
            vec![ManaType::Colorless; 2],
            ManaResolutionShare::WholeResolution { total: 2 },
        )]
    );
    resolve_only_trigger(&mut runner, metalworker, 2);
}

fn vorinclex(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(
            P0,
            "Vorinclex, Voice of Hunger",
            7,
            6,
            "Trample\nWhenever you tap a land for mana, add one mana of any type that land produced.\nWhenever an opponent taps a land for mana, that land doesn't untap during its controller's next untap step.",
        )
        .id()
}

fn tap_land(runner: &mut GameRunner, land: ObjectId) -> ActionResult {
    let (_, _, grouped) = engine::ai_support::legal_actions_full(runner.state());
    let tap = grouped
        .get(&land)
        .into_iter()
        .flatten()
        .find(|action| matches!(action, GameAction::TapLandForMana { .. }))
        .cloned()
        .expect("the land can tap for mana");
    runner.act(tap).expect("tap the land for mana")
}

fn assert_undo_unavailable(runner: &mut GameRunner, land: ObjectId) {
    let undo = GameAction::UntapLandForMana { object_id: land };
    let undo_id = interaction_action_id(&undo);
    let state = runner.state_mut();
    bind_interaction_authority(state, InteractionSessionId("mana-undo".to_string()))
        .expect("bind the human interaction view");
    let view = derive_viewer_interaction(state, state, P0);
    assert!(view.can_submit);
    let choices: Vec<_> = view
        .opportunities
        .iter()
        .flat_map(|opportunity| match &opportunity.response {
            InteractionOpportunityResponse::ExactChoices { choices } => choices,
            InteractionOpportunityResponse::Schema { candidates, .. } => candidates,
        })
        .collect();
    assert!(!choices.is_empty(), "the human player has legal choices");
    assert!(
        !choices.iter().flat_map(|choice| &choice.surfaces).any(|surface| {
            matches!(surface, InteractionPresentationSurface::Action { action_id: Some(id), .. } if id == &undo_id)
        }),
        "the human player is not offered undo for this tap"
    );
    assert!(runner.act(undo).is_err(), "the reducer refuses undo");
}

#[test]
fn tyvar_dryad_arbor_tap_cannot_be_undone_after_the_granted_trigger_fires() {
    let db = crate::support::shared_card_db().expect("integration card fixture");
    for tap in [
        tap_land as fn(&mut GameRunner, ObjectId) -> ActionResult,
        activate,
    ] {
        let (mut scenario, _) = tyvar_board(Phase::PreCombatMain);
        let arbor = scenario.add_real_card(P0, "Dryad Arbor", Zone::Battlefield, db);
        let mut runner = scenario.build();

        tap(&mut runner, arbor);
        assert_top_trigger(&runner, arbor);
        assert_undo_unavailable(&mut runner, arbor);
        resolve_only_trigger(&mut runner, arbor, 1);
        assert!(runner.state().objects[&arbor].tapped);
    }
}

fn manabarbs_board() -> (GameScenario, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let barbs = scenario
        .add_enchantment_from_oracle(
            P1,
            "Manabarbs",
            "Whenever a player taps a land for mana, this enchantment deals 1 damage to that player.",
        )
        .id();
    // A printed Forest has only its intrinsic ability, exercising the tap path
    // that records undo before collecting the resolution's observers.
    let db = crate::support::shared_card_db().expect("integration card fixture");
    let forest = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    (scenario, barbs, forest)
}

fn assert_manabarbs_trigger(runner: &GameRunner, barbs: ObjectId, forest: ObjectId) {
    assert!(matches!(
        runner.state().stack.back().map(|entry| &entry.kind),
        Some(StackEntryKind::TriggeredAbility {
            source_id,
            trigger_event: Some(GameEvent::TappedForMana { source_id: mana_source, .. }),
            ..
        }) if *source_id == barbs && *mana_source == forest
    ));
}

#[test]
fn manabarbs_blocks_undo_of_an_intrinsic_land_tap_at_priority() {
    let (scenario, barbs, forest) = manabarbs_board();
    let mut runner = scenario.build();

    tap_land(&mut runner, forest);
    assert_manabarbs_trigger(&runner, barbs, forest);
    assert_undo_unavailable(&mut runner, forest);
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().players[0].life, 19);
    assert!(runner.state().objects[&forest].tapped);
}

#[test]
fn manabarbs_blocks_undo_while_the_tap_observer_is_deferred_for_payment() {
    let (mut scenario, barbs, forest) = manabarbs_board();
    let spell = vitalize(&mut scenario);
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("announce Vitalize");
    assert!(matches!(result.waiting_for, WaitingFor::ManaPayment { .. }));

    let tapped = tap_land(&mut runner, forest);
    assert!(matches!(tapped.waiting_for, WaitingFor::ManaPayment { .. }));
    assert!(!runner
        .state()
        .stack
        .iter()
        .any(|entry| entry.source_id == barbs));
    assert_undo_unavailable(&mut runner, forest);
    runner
        .act(GameAction::PassPriority)
        .expect("pay for Vitalize");
    assert_manabarbs_trigger(&runner, barbs, forest);
    runner.pass_both_players();
    assert_eq!(runner.state().players[0].life, 19);
    assert!(runner.state().objects[&forest].tapped);
}

fn tyvar_vorinclex_forest() -> (GameScenario, ObjectId, ObjectId) {
    let db = crate::support::shared_card_db().expect("integration card fixture");
    let (mut scenario, _) = tyvar_board(Phase::PreCombatMain);
    let vorinclex = vorinclex(&mut scenario);
    let forest = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    (scenario, vorinclex, forest)
}

#[test]
fn tyvar_vorinclex_forest_tap_cannot_be_undone_during_payment() {
    let (mut scenario, vorinclex, forest) = tyvar_vorinclex_forest();
    let spell = vitalize(&mut scenario);
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    let announced = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("announce Vitalize");
    assert!(matches!(
        announced.waiting_for,
        WaitingFor::ManaPayment { player, .. } if player == P0
    ));

    let tapped = tap_land(&mut runner, forest);
    assert!(matches!(
        tapped.waiting_for,
        WaitingFor::ManaPayment { player, .. } if player == P0
    ));
    assert_eq!(
        runner.state().players[0]
            .mana_pool
            .count_color(ManaType::Green),
        2
    );
    assert!(!runner
        .state()
        .stack
        .iter()
        .any(|entry| entry.source_id == vorinclex));
    assert_undo_unavailable(&mut runner, forest);

    runner
        .act(GameAction::PassPriority)
        .expect("pay for Vitalize");
    assert_eq!(runner.state().stack.len(), 2);
    assert_eq!(runner.state().stack.front().unwrap().source_id, spell);
    assert_top_trigger(&runner, vorinclex);
    assert_eq!(counters(&runner, vorinclex), 0);

    runner.pass_both_players();
    assert_eq!(counters(&runner, vorinclex), 1);
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert!(runner.state().objects[&forest].tapped);
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn tyvar_vorinclex_forest_tap_cannot_be_undone_at_priority() {
    let (scenario, vorinclex, forest) = tyvar_vorinclex_forest();
    let mut runner = scenario.build();

    tap_land(&mut runner, forest);
    assert_eq!(
        runner.state().players[0]
            .mana_pool
            .count_color(ManaType::Green),
        2
    );
    assert_top_trigger(&runner, vorinclex);
    assert_undo_unavailable(&mut runner, forest);
    resolve_only_trigger(&mut runner, vorinclex, 1);
    assert!(runner.state().objects[&forest].tapped);
}

#[test]
fn tyvar_wild_growth_forest_tap_can_be_undone_at_priority() {
    let db = crate::support::shared_card_db().expect("integration card fixture");
    let (mut scenario, _) = tyvar_board(Phase::PreCombatMain);
    let forest = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let growth = scenario.add_real_card(P0, "Wild Growth", Zone::Hand, db);
    seed_mana(&mut scenario, &[ManaType::Green]);
    let mut runner = scenario.build();
    runner
        .cast(growth)
        .target_object(forest)
        .resolve()
        .assert_zone(&[growth], Zone::Battlefield);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);

    tap_land(&mut runner, forest);
    assert!(runner.state().objects[&forest].tapped);
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner.state().players[0]
            .mana_pool
            .count_color(ManaType::Green),
        2
    );

    let undone = runner
        .act(GameAction::UntapLandForMana { object_id: forest })
        .expect("undo the Forest tap and refund both mana producers");
    assert!(matches!(undone.waiting_for, WaitingFor::Priority { player } if player == P0));
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert!(!runner.state().objects[&forest].tapped);
}

/// CR 605.1b + CR 608.2p: Tyvar's grant observes Vorinclex's triggered mana
/// ability and counts its production in either loop detection mode.
#[test]
fn tyvar_counts_a_creatures_triggered_mana_ability_in_both_loop_modes() {
    for mode in [LoopDetectionMode::Interactive, LoopDetectionMode::Off] {
        let (mut scenario, _) = tyvar_board(Phase::PreCombatMain);
        let vorinclex = vorinclex(&mut scenario);
        let forest = scenario.add_basic_land(P0, ManaColor::Green);
        let mut runner = scenario.build();
        runner.state_mut().loop_detection = mode;

        tap_land(&mut runner, forest);
        assert_eq!(
            runner.state().players[0]
                .mana_pool
                .count_color(ManaType::Green),
            2,
            "{mode:?}: the Forest and Vorinclex each added one green mana"
        );
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        resolve_only_trigger(&mut runner, vorinclex, 1);
        assert_eq!(counters(&runner, forest), 0, "{mode:?}");
    }
}

/// CR 605.1b + CR 608.2p: Off keeps main's three green mana from the Forest,
/// Vorinclex, and Caged Sun. Interactive also lets the Sun see Vorinclex's
/// production, which is from a land because of Ashaya.
#[test]
fn off_announces_vorinclex_to_tyvar_without_a_second_caged_sun_firing() {
    for (mode, sun_firings) in [
        (LoopDetectionMode::Off, 1),
        (LoopDetectionMode::Interactive, 2),
    ] {
        let (mut scenario, _) = tyvar_board(Phase::PreCombatMain);
        let vorinclex = vorinclex(&mut scenario);
        scenario.add_creature_from_oracle(
            P0,
            "Ashaya, Soul of the Wild",
            0,
            0,
            "Ashaya's power and toughness are each equal to the number of lands you control.\nNontoken creatures you control are Forest lands in addition to their other types. (They're still affected by summoning sickness.)",
        );
        let sun = scenario
            .add_artifact_from_oracle(
                P0,
                "Caged Sun",
                "As this artifact enters, choose a color.\nCreatures you control of the chosen color get +1/+1.\nWhenever a land's ability causes you to add one or more mana of the chosen color, add an additional one mana of that color.",
            )
            .id();
        let forest = scenario.add_basic_land(P0, ManaColor::Green);
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.loop_detection = mode;
        state
            .objects
            .get_mut(&sun)
            .expect("Caged Sun")
            .chosen_attributes
            .push(ChosenAttribute::Color(ManaColor::Green));
        state.layers_dirty.mark_full();

        let result = tap_land(&mut runner, forest);
        let vorinclex_types = &runner.state().objects[&vorinclex].card_types;
        assert!(vorinclex_types.core_types.contains(&CoreType::Land));
        assert!(vorinclex_types
            .subtypes
            .iter()
            .any(|subtype| subtype == "Forest"));
        assert_eq!(
            production_shares(&result.events, vorinclex).len(),
            1,
            "{mode:?}"
        );
        assert_eq!(
            production_shares(&result.events, sun).len(),
            sun_firings,
            "{mode:?}"
        );
        let pool = &runner.state().players[0].mana_pool;
        assert_eq!(
            pool.count_color(ManaType::Green),
            2 + sun_firings,
            "{mode:?}"
        );
        assert_eq!(pool.total(), 2 + sun_firings, "{mode:?}");
        resolve_only_trigger(&mut runner, vorinclex, 1);
    }
}

fn assert_payment_trigger_resolves_first(mode: CastPaymentMode) {
    let (mut scenario, _) = tyvar_board(Phase::PreCombatMain);
    let elf = llanowar_elves(&mut scenario, P0);
    let spell = vitalize(&mut scenario);
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: mode,
        })
        .expect("cast Vitalize");

    if mode == CastPaymentMode::Manual {
        assert!(matches!(result.waiting_for, WaitingFor::ManaPayment { .. }));
        let activated = activate(&mut runner, elf);
        assert!(matches!(
            activated.waiting_for,
            WaitingFor::ManaPayment { .. }
        ));
        runner
            .act(GameAction::PassPriority)
            .expect("pay for Vitalize");
    }
    assert_eq!(runner.state().stack.len(), 2);
    assert_top_trigger(&runner, elf);
    assert_eq!(runner.state().stack.front().unwrap().source_id, spell);
    assert_eq!(counters(&runner, elf), 0);
    assert!(runner.state().objects[&elf].tapped);

    runner.pass_both_players();
    assert_eq!(counters(&runner, elf), 1);
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert!(
        runner.state().objects[&elf].tapped,
        "Vitalize has yet to untap the elf"
    );

    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    assert!(!runner.state().objects[&elf].tapped);
}

/// CR 601.2g + CR 603.3 + CR 405.5: The grant triggers above the paid spell (Tyvar ruling 2).
#[test]
fn tyvar_trigger_resolves_above_spell_after_manual_payment() {
    assert_payment_trigger_resolves_first(CastPaymentMode::Manual);
}

/// CR 601.2g + CR 603.3 + CR 405.5: Auto-tap also puts the grant above the spell.
#[test]
fn tyvar_trigger_resolves_above_spell_after_auto_tap() {
    assert_payment_trigger_resolves_first(CastPaymentMode::Auto);
}

/// CR 400.7 + CR 611.3a: The returning Tyvar grants a new ability (Tyvar ruling 3).
#[test]
fn tyvar_returning_grants_a_fresh_trigger_this_turn() {
    let (mut scenario, tyvar) = tyvar_board(Phase::PreCombatMain);
    let elf = llanowar_elves(&mut scenario, P0);
    let untap = vitalize(&mut scenario);
    let blink = scenario.add_spell_to_hand_from_oracle(
        P0, "Cloudshift", true,
        "Exile target creature you control, then return that card to the battlefield under your control.",
    ).with_mana_cost(one_mana(ManaCostShard::White)).id();
    seed_mana(&mut scenario, &[ManaType::Green, ManaType::White]);
    let mut runner = scenario.build();
    let turn = runner.state().turn_number;
    let incarnation = runner.state().objects[&tyvar].incarnation;

    activate(&mut runner, elf);
    resolve_only_trigger(&mut runner, elf, 1);
    runner
        .cast(blink)
        .target_object(tyvar)
        .resolve()
        .assert_zone(&[tyvar], Zone::Battlefield);
    assert_ne!(runner.state().objects[&tyvar].incarnation, incarnation);
    runner
        .cast(untap)
        .resolve()
        .assert_zone(&[untap], Zone::Graveyard);
    assert_eq!(runner.state().turn_number, turn);

    activate(&mut runner, elf);
    resolve_only_trigger(&mut runner, elf, 2);
}

#[test]
fn tyvar_mana_event_defaults_legacy_payload_to_recipient_only() {
    let legacy = serde_json::json!({
        "type": "ManaAbilityProduced",
        "data": {
            "player_id": P0,
            "source_id": ObjectId(1),
            "produced": [ManaType::Green]
        }
    });
    let event: GameEvent =
        serde_json::from_value(legacy.clone()).expect("deserialize legacy mana event");
    assert_eq!(
        production_shares(std::slice::from_ref(&event), ObjectId(1)),
        vec![(
            P0,
            vec![ManaType::Green],
            ManaResolutionShare::RecipientOnly
        ),]
    );
    assert_eq!(serde_json::to_value(event).unwrap(), legacy);
}
