//! CR 702.138b–c: only a spell cast using Escape gets the Escape entry amount.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{CopyTargetPurpose, Effect, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, CastingVariant, PayCostKind, PersistedGameState, PersistedRestoreFinalization,
    ReplacementChoiceKind, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::keywords::KeywordKind;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::proposed_event::ProposedEvent;
use engine::types::statics::CastFrequency;
use engine::types::zones::{ExileCostSourceZone, Zone};

use crate::blitz_em_dash_graveyard_cast::MULDROTHA;
use crate::support::shared_card_db;

const POLUKRANOS_NAME: &str = "Polukranos, Unchained";
const POLUKRANOS_ORACLE: &str = "Polukranos enters with six +1/+1 counters on it. It escapes with twelve +1/+1 counters on it instead.\n\
If damage would be dealt to Polukranos while it has a +1/+1 counter on it, prevent that damage and remove that many +1/+1 counters from it.\n\
{1}{B}{G}: Polukranos fights another target creature.\n\
Escape—{4}{B}{G}, Exile six other cards from your graveyard.";

fn golgari_mana_scenario(generic: usize) -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_mana_pool(
        P0,
        [ManaType::Black, ManaType::Green]
            .into_iter()
            .chain(std::iter::repeat_n(ManaType::Colorless, generic))
            .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
            .collect(),
    );
    scenario
}

fn polukranos_cost() -> ManaCost {
    ManaCost::Cost {
        generic: 2,
        shards: vec![ManaCostShard::Black, ManaCostShard::Green],
    }
}

fn cast_spell(runner: &mut GameRunner, object_id: ObjectId) -> WaitingFor {
    cast_spell_with_targets(runner, object_id, vec![])
}

fn cast_spell_with_targets(
    runner: &mut GameRunner,
    object_id: ObjectId,
    targets: Vec<ObjectId>,
) -> WaitingFor {
    let card_id = runner.state().objects[&object_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id,
            card_id,
            targets,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast must be accepted")
        .waiting_for
}

fn pay_escape_exile(runner: &mut GameRunner, prompt: WaitingFor, filler: &[ObjectId]) {
    let WaitingFor::PayCost {
        player,
        kind:
            PayCostKind::ExileFromZone {
                zone: ExileCostSourceZone::Graveyard,
            },
        count,
        choices,
        ..
    } = prompt
    else {
        panic!("expected Escape's graveyard exile cost, got {prompt:?}");
    };
    assert_eq!(player, P0);
    assert_eq!(count, filler.len());
    assert_eq!(choices.len(), filler.len());
    assert!(filler.iter().all(|id| choices.contains(id)));
    runner
        .act(GameAction::SelectCards {
            cards: filler.to_vec(),
        })
        .expect("the eligible graveyard cards must pay Escape");
    for id in filler {
        assert_eq!(runner.state().objects[id].zone, Zone::Exile);
    }
}

fn finalized_spell(
    runner: &mut GameRunner,
    id: ObjectId,
    origin: Zone,
    variant: CastingVariant,
    mana_spent: u32,
) -> ObjectIncarnationRef {
    if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
        runner
            .act(GameAction::PassPriority)
            .expect("the mana pool must cover the spell cost");
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "expected finalized cast, got {:?}",
        runner.state().waiting_for,
    );
    let entry = runner.state().stack.last().expect("finalized spell");
    assert_eq!(entry.source_id, id);
    let StackEntryKind::Spell {
        casting_variant,
        actual_mana_spent,
        ..
    } = &entry.kind
    else {
        panic!("expected a spell, got {entry:?}");
    };
    assert_eq!(*casting_variant, variant);
    assert_eq!(*actual_mana_spent, mana_spent);
    let object = &runner.state().objects[&id];
    assert_eq!(object.zone, Zone::Stack);
    assert_eq!(object.cast_from_zone, Some(origin));
    ObjectIncarnationRef::of(id, object.incarnation)
}

fn pass_to_resolution(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..2 {
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "expected priority before resolution, got {:?}",
            runner.state().waiting_for,
        );
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("the priority holder must be able to pass")
                .events,
        );
    }
    events
}

fn assert_entry_incarnation(
    runner: &GameRunner,
    departure: ObjectIncarnationRef,
    from: Zone,
) -> ObjectIncarnationRef {
    let state = runner.state();
    let object = &state.objects[&departure.object_id];
    assert_eq!(object.zone, Zone::Battlefield);
    assert_ne!(object.incarnation, departure.incarnation);
    let arrival = state
        .zone_changes_this_turn
        .iter()
        .find(|event| {
            event.object_id == departure.object_id
                && event.from_zone == Some(from)
                && event.to_zone == Zone::Battlefield
                && event.entered_incarnation == Some(object.incarnation)
        })
        .expect("the exact battlefield incarnation must have an arrival record");
    let source = &arrival
        .trigger_source_context
        .as_ref()
        .expect("the entry must record its departure")
        .identity;
    assert_eq!(source.reference, departure);
    assert_eq!(source.expected_zone, from);
    let entrant = ObjectIncarnationRef::of(departure.object_id, object.incarnation);
    entrant
}

fn assert_entry_counters(
    runner: &GameRunner,
    departure: ObjectIncarnationRef,
    from: Zone,
    expected: u32,
) -> ObjectIncarnationRef {
    let entrant = assert_entry_incarnation(runner, departure, from);
    let object = &runner.state().objects[&entrant.object_id];
    assert_eq!(
        object
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or_default(),
        expected
    );
    entrant
}

fn add_filler(scenario: &mut GameScenario, count: usize) -> Vec<ObjectId> {
    (0..count)
        .map(|index| {
            scenario
                .add_land_to_graveyard(P0, &format!("Filler {index}"))
                .id()
        })
        .collect()
}

#[test]
fn polukranos_cast_from_hand_enters_with_six_counters() {
    let mut scenario = golgari_mana_scenario(2);
    let polukranos = scenario
        .add_creature_to_hand(P0, POLUKRANOS_NAME, 0, 0)
        .as_legendary()
        .with_subtypes(vec!["Zombie", "Hydra"])
        .with_mana_cost(polukranos_cost())
        .from_oracle_text(POLUKRANOS_ORACLE)
        .id();
    let mut runner = scenario.build();

    cast_spell(&mut runner, polukranos);
    let departure = finalized_spell(
        &mut runner,
        polukranos,
        Zone::Hand,
        CastingVariant::Normal,
        4,
    );
    pass_to_resolution(&mut runner);
    assert_entry_counters(&runner, departure, Zone::Stack, 6);
}

#[test]
fn polukranos_cast_with_escape_enters_with_twelve_counters() {
    let mut scenario = golgari_mana_scenario(4);
    let polukranos = scenario
        .add_creature_to_graveyard(P0, POLUKRANOS_NAME, 0, 0)
        .as_legendary()
        .with_subtypes(vec!["Zombie", "Hydra"])
        .with_mana_cost(polukranos_cost())
        .from_oracle_text(POLUKRANOS_ORACLE)
        .id();
    let filler = add_filler(&mut scenario, 6);
    let mut runner = scenario.build();

    let prompt = cast_spell(&mut runner, polukranos);
    pay_escape_exile(&mut runner, prompt, &filler);
    let departure = finalized_spell(
        &mut runner,
        polukranos,
        Zone::Graveyard,
        CastingVariant::Escape,
        6,
    );
    pass_to_resolution(&mut runner);
    assert_entry_counters(&runner, departure, Zone::Stack, 12);
}

fn mana_cost(generic: u32, colors: &[ManaCostShard]) -> ManaCost {
    ManaCost::Cost {
        generic,
        shards: colors.to_vec(),
    }
}

fn mana_pool(colors: &[ManaType], generic: usize) -> Vec<ManaUnit> {
    colors
        .iter()
        .copied()
        .chain(std::iter::repeat_n(ManaType::Colorless, generic))
        .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
        .collect()
}

fn polukranos_in_graveyard(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_graveyard(P0, POLUKRANOS_NAME, 0, 0)
        .as_legendary()
        .with_subtypes(vec!["Zombie", "Hydra"])
        .with_mana_cost(polukranos_cost())
        .from_oracle_text(POLUKRANOS_ORACLE)
        .id()
}

fn add_muldrotha(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature(P0, "Muldrotha, the Gravetide", 6, 6)
        .as_legendary()
        .with_subtypes(vec!["Elemental", "Avatar"])
        .with_mana_cost(mana_cost(
            3,
            &[
                ManaCostShard::Black,
                ManaCostShard::Green,
                ManaCostShard::Blue,
            ],
        ))
        .from_oracle_text(MULDROTHA)
        .id()
}

fn choose_graveyard_cast(
    runner: &mut GameRunner,
    spell: ObjectId,
    muldrotha: ObjectId,
    filler: &[ObjectId],
    escape_cost: ManaCost,
    escape: bool,
) -> CastingVariant {
    let printed_cost = runner.state().objects[&spell].mana_cost.clone();
    let prompt = cast_spell(runner, spell);
    let WaitingFor::CastingVariantChoice {
        player,
        object_id,
        options,
        ..
    } = prompt
    else {
        panic!("expected both graveyard cast options, got {prompt:?}");
    };
    assert_eq!(player, P0);
    assert_eq!(object_id, spell);
    assert_eq!(options.len(), 2, "{options:?}");

    let muldrotha_variant = CastingVariant::GraveyardPermission {
        source: muldrotha,
        frequency: CastFrequency::OncePerTurnPerPermanentType,
        slot_type: Some(CoreType::Creature),
        graveyard_destination_replacement: None,
    };
    let escape_option = options
        .iter()
        .find(|option| option.variant == CastingVariant::Escape)
        .expect("the Escape option must be offered");
    assert_eq!(escape_option.mana_cost, escape_cost);
    let muldrotha_option = options
        .iter()
        .find(|option| option.variant == muldrotha_variant)
        .expect("Muldrotha must offer its creature slot at the printed cost");
    assert_eq!(muldrotha_option.mana_cost, printed_cost);
    assert!(muldrotha_option.additional_cost.is_none());

    let variant = if escape {
        CastingVariant::Escape
    } else {
        muldrotha_variant
    };
    let index = options
        .iter()
        .position(|option| option.variant == variant)
        .expect("the selected casting permission must be offered");
    let prompt = runner
        .act(GameAction::ChooseCastingVariant { index })
        .expect("the selected graveyard permission must be payable")
        .waiting_for;
    if escape {
        pay_escape_exile(runner, prompt, filler);
    }
    variant
}

fn assert_graveyard_payment(
    runner: &GameRunner,
    muldrotha: ObjectId,
    filler: &[ObjectId],
    escaped: bool,
) {
    assert_eq!(
        runner
            .state()
            .graveyard_cast_permissions_used_per_type
            .contains(&(muldrotha, CoreType::Creature)),
        !escaped
    );
    let expected = if escaped {
        Zone::Exile
    } else {
        Zone::Graveyard
    };
    for id in filler {
        assert_eq!(runner.state().objects[id].zone, expected);
    }
}

fn checked_reload(runner: &GameRunner, stack_pin: ObjectIncarnationRef) -> GameRunner {
    let serialized = serde_json::to_value(PersistedGameState::capture(runner.state().clone()))
        .expect("the paused game must serialize");
    assert_eq!(
        serialized["state"]["resolving_stack_object"],
        serde_json::to_value(stack_pin).unwrap()
    );
    let restored = serde_json::from_value::<PersistedGameState>(serialized)
        .expect("the saved game must decode")
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .expect("the paused state must be admissible")
        .finalize_after_rehydration(|state| {
            engine::game::rehydrate_game_from_card_db_with_finalization(
                state,
                shared_card_db().expect("the integration card database"),
                engine::game::CardDbRehydrationFinalization::Defer,
            );
            Ok(())
        })
        .expect("the restored state must be publishable");
    let serialized = serde_json::to_value(PersistedGameState::capture(restored.clone()))
        .expect("the restored game must serialize");
    assert_eq!(
        serialized["state"]["resolving_stack_object"],
        serde_json::to_value(stack_pin).unwrap()
    );
    GameRunner::from_state(restored)
}

fn assert_resolving_spell(
    runner: &GameRunner,
    stack_pin: ObjectIncarnationRef,
    variant: CastingVariant,
) {
    let entry = runner
        .state()
        .resolving_stack_entry
        .as_ref()
        .expect("the paused spell is still resolving");
    assert_eq!(entry.id, stack_pin.object_id);
    assert!(matches!(
        entry.kind,
        StackEntryKind::Spell { casting_variant, .. } if casting_variant == variant
    ));
}

#[test]
fn polukranos_reanimated_without_casting_enters_with_six_counters() {
    let mut scenario = GameScenario::new();
    scenario
        .at_phase(Phase::PreCombatMain)
        .with_mana_pool(P0, mana_pool(&[ManaType::Black], 3));
    let polukranos = polukranos_in_graveyard(&mut scenario);
    let zombify = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Zombify",
            false,
            "Return target creature card from your graveyard to the battlefield.",
        )
        .with_mana_cost(mana_cost(3, &[ManaCostShard::Black]))
        .id();
    let mut runner = scenario.build();
    let departure =
        ObjectIncarnationRef::of(polukranos, runner.state().objects[&polukranos].incarnation);
    let target = TargetRef::Object(polukranos);
    cast_spell_with_targets(&mut runner, zombify, vec![polukranos]);
    finalized_spell(&mut runner, zombify, Zone::Hand, CastingVariant::Normal, 4);
    let StackEntryKind::Spell {
        ability: Some(ability),
        ..
    } = &runner.state().stack.last().unwrap().kind
    else {
        panic!("Zombify must carry its selected target");
    };
    assert_eq!(ability.targets, vec![target]);
    let target_object = &runner.state().objects[&polukranos];
    assert_eq!(target_object.zone, Zone::Graveyard);
    assert_eq!(target_object.incarnation, departure.incarnation);
    pass_to_resolution(&mut runner);
    assert_entry_counters(&runner, departure, Zone::Graveyard, 6);
}

#[test]
fn polukranos_escape_adds_renatas_independent_entry_counter() {
    let mut scenario = golgari_mana_scenario(4);
    scenario.add_creature(P0, "Renata, Called to the Hunt", 0, 3)
        .as_legendary()
        .as_enchantment()
        .as_creature()
        .with_subtypes(vec!["Demigod"])
        .with_mana_cost(mana_cost(2, &[ManaCostShard::Green, ManaCostShard::Green]))
        .from_oracle_text("Renata's power is equal to your devotion to green. (Each {G} in the mana costs of permanents you control counts toward your devotion to green.)\nEach other creature you control enters with an additional +1/+1 counter on it.");
    let polukranos = polukranos_in_graveyard(&mut scenario);
    let filler = add_filler(&mut scenario, 6);
    let mut runner = scenario.build();

    let prompt = cast_spell(&mut runner, polukranos);
    pay_escape_exile(&mut runner, prompt, &filler);
    let pin = finalized_spell(
        &mut runner,
        polukranos,
        Zone::Graveyard,
        CastingVariant::Escape,
        6,
    );
    pass_to_resolution(&mut runner);
    assert_entry_counters(&runner, pin, Zone::Stack, 13);
}

fn entry_order_option(
    runner: &GameRunner,
    pin: ObjectIncarnationRef,
    expected_candidates: &[ObjectId],
    answer: ObjectId,
) -> usize {
    assert_resolving_spell(runner, pin, CastingVariant::Escape);
    let entrant = &runner.state().objects[&pin.object_id];
    assert_eq!(entrant.zone, Zone::Stack);
    assert_eq!(entrant.incarnation, pin.incarnation);
    assert!(matches!(
        runner.state().pending_replacement.as_ref().map(|pending| &pending.proposed),
        Some(ProposedEvent::ZoneChange { object_id, from: Zone::Stack, to: Zone::Battlefield, .. })
            if *object_id == pin.object_id
    ));
    let WaitingFor::ReplacementChoice {
        player,
        kind: ReplacementChoiceKind::Order,
        candidates,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected entry replacement order, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(
        candidates.len(),
        expected_candidates.len(),
        "{candidates:?}"
    );
    assert!(
        expected_candidates
            .iter()
            .all(|id| candidates.iter().any(|row| row.source_id == *id)),
        "{candidates:?}"
    );
    candidates
        .iter()
        .position(|row| row.source_id == answer)
        .expect("the selected replacement is offered")
}

#[test]
fn polukranos_escape_preserves_one_entry_candidate_across_replacement_order_reload() {
    let mut scenario = golgari_mana_scenario(4);
    let archelos = scenario.add_creature_from_oracle(
        P0, "Archelos, Lagoon Mystic", 2, 4,
        "As long as Archelos is tapped, other permanents enter tapped.\nAs long as Archelos is untapped, other permanents enter untapped.",
    ).id();
    let imposing = scenario
        .add_creature_from_oracle(
            P1,
            "Imposing Sovereign",
            2,
            1,
            "Creatures your opponents control enter tapped.",
        )
        .id();
    let polukranos = polukranos_in_graveyard(&mut scenario);
    let filler = add_filler(&mut scenario, 6);
    let mut runner = scenario.build();

    let prompt = cast_spell(&mut runner, polukranos);
    pay_escape_exile(&mut runner, prompt, &filler);
    let pin = finalized_spell(
        &mut runner,
        polukranos,
        Zone::Graveyard,
        CastingVariant::Escape,
        6,
    );
    pass_to_resolution(&mut runner);
    let offered = [polukranos, archelos, imposing];
    entry_order_option(&runner, pin, &offered, polukranos);
    let mut runner = checked_reload(&runner, pin);
    let index = entry_order_option(&runner, pin, &offered, polukranos);
    runner.act(GameAction::ChooseReplacement { index }).unwrap();
    let index = entry_order_option(&runner, pin, &[archelos, imposing], imposing);
    runner.act(GameAction::ChooseReplacement { index }).unwrap();
    assert_entry_counters(&runner, pin, Zone::Stack, 12);
    assert!(
        !runner.state().objects[&polukranos].tapped,
        "Archelos must make the last tap-state write"
    );
}

#[test]
fn polukranos_uses_the_entry_amount_for_the_selected_graveyard_authority() {
    for escape in [false, true] {
        let mut scenario = golgari_mana_scenario(4);
        let muldrotha = add_muldrotha(&mut scenario);
        let polukranos = polukranos_in_graveyard(&mut scenario);
        let filler = add_filler(&mut scenario, 6);
        let mut runner = scenario.build();

        let paid = choose_graveyard_cast(
            &mut runner,
            polukranos,
            muldrotha,
            &filler,
            mana_cost(4, &[ManaCostShard::Black, ManaCostShard::Green]),
            escape,
        );
        let pin = finalized_spell(
            &mut runner,
            polukranos,
            Zone::Graveyard,
            paid,
            if escape { 6 } else { 4 },
        );
        assert_graveyard_payment(&runner, muldrotha, &filler, escape);
        pass_to_resolution(&mut runner);
        assert_entry_counters(&runner, pin, Zone::Stack, if escape { 12 } else { 6 });
    }
}

#[test]
fn voracious_typhon_gets_counters_only_when_it_uses_escape() {
    for escape in [false, true] {
        let mut scenario = GameScenario::new();
        scenario
            .at_phase(Phase::PreCombatMain)
            .with_mana_pool(P0, mana_pool(&[ManaType::Green, ManaType::Green], 5));
        let muldrotha = add_muldrotha(&mut scenario);
        let typhon = scenario.add_creature_to_graveyard(P0, "Voracious Typhon", 4, 4)
            .with_subtypes(vec!["Snake", "Beast"])
            .with_mana_cost(mana_cost(2, &[ManaCostShard::Green, ManaCostShard::Green]))
            .from_oracle_text("Escape—{5}{G}{G}, Exile four other cards from your graveyard. (You may cast this card from your graveyard for its escape cost.)\nThis creature escapes with three +1/+1 counters on it.")
            .id();
        let filler = add_filler(&mut scenario, 4);
        let mut runner = scenario.build();

        let paid = choose_graveyard_cast(
            &mut runner,
            typhon,
            muldrotha,
            &filler,
            mana_cost(5, &[ManaCostShard::Green, ManaCostShard::Green]),
            escape,
        );
        let pin = finalized_spell(
            &mut runner,
            typhon,
            Zone::Graveyard,
            paid,
            if escape { 7 } else { 4 },
        );
        assert_graveyard_payment(&runner, muldrotha, &filler, escape);
        pass_to_resolution(&mut runner);
        assert_entry_counters(&runner, pin, Zone::Stack, if escape { 3 } else { 0 });
    }
}

#[test]
fn tizerus_charger_offers_its_counter_choice_only_when_it_escapes() {
    let flying = CounterType::Keyword(KeywordKind::Flying);
    for escape in [false, true] {
        let mut scenario = GameScenario::new();
        scenario
            .at_phase(Phase::PreCombatMain)
            .with_mana_pool(P0, mana_pool(&[ManaType::Black], 4));
        let muldrotha = add_muldrotha(&mut scenario);
        let charger = scenario.add_creature_to_graveyard(P0, "Tizerus Charger", 3, 2)
            .with_subtypes(vec!["Pegasus"])
            .with_mana_cost(mana_cost(2, &[ManaCostShard::Black]))
            .from_oracle_text("Escape—{4}{B}, Exile five other cards from your graveyard. (You may cast this card from your graveyard for its escape cost.)\nThis creature escapes with your choice of a +1/+1 counter or a flying counter on it.")
            .id();
        let filler = add_filler(&mut scenario, 5);
        let mut runner = scenario.build();

        let paid = choose_graveyard_cast(
            &mut runner,
            charger,
            muldrotha,
            &filler,
            mana_cost(4, &[ManaCostShard::Black]),
            escape,
        );
        let pin = finalized_spell(
            &mut runner,
            charger,
            Zone::Graveyard,
            paid,
            if escape { 5 } else { 3 },
        );
        assert_graveyard_payment(&runner, muldrotha, &filler, escape);
        pass_to_resolution(&mut runner);

        if escape {
            let WaitingFor::ChooseOneOfBranch {
                player,
                source_id,
                branches,
                ..
            } = &runner.state().waiting_for
            else {
                panic!(
                    "Tizerus Charger must ask for a counter when it escapes, got {:?}",
                    runner.state().waiting_for
                );
            };
            assert_eq!(*player, P0);
            assert_eq!(*source_id, charger);
            assert_eq!(branches.len(), 2);
            let index = branches.iter().position(|branch| matches!(
                branch.effect.as_ref(),
                Effect::PutCounter { counter_type, target: TargetFilter::SelfRef, .. } if counter_type == &flying
            )).expect("the flying counter must be offered");
            runner.act(GameAction::ChooseBranch { index }).unwrap();
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert_entry_counters(&runner, pin, Zone::Stack, 0);
        assert_eq!(
            runner.state().objects[&charger]
                .counters
                .get(&flying)
                .copied()
                .unwrap_or_default(),
            u32::from(escape)
        );
    }
}

fn add_polukranos_donor(scenario: &mut GameScenario) -> ObjectId {
    scenario.add_enchantment_from_oracle(
        P1,
        "Leyline of Anticipation",
        "If this card is in your opening hand, you may begin the game with it on the battlefield.\nYou may cast spells as though they had flash.",
    );
    scenario.with_mana_pool(P1, mana_pool(&[ManaType::Black, ManaType::Green], 2));
    scenario
        .add_creature_to_hand_from_oracle(P1, POLUKRANOS_NAME, 0, 0, POLUKRANOS_ORACLE)
        .as_legendary()
        .with_subtypes(vec!["Zombie", "Hydra"])
        .with_mana_cost(polukranos_cost())
        .id()
}

fn cast_polukranos_donor(runner: &mut GameRunner, donor: ObjectId) -> ObjectIncarnationRef {
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0));
    runner.act(GameAction::PassPriority).unwrap();
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1));
    cast_spell(runner, donor);
    let stack_pin = finalized_spell(runner, donor, Zone::Hand, CastingVariant::Normal, 4);
    pass_to_resolution(runner);
    let pin = assert_entry_counters(runner, stack_pin, Zone::Stack, 6);
    assert_polukranos_donor(runner, pin);
    assert!(matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0));
    pin
}

fn assert_polukranos_donor(runner: &GameRunner, donor: ObjectIncarnationRef) {
    let object = &runner.state().objects[&donor.object_id];
    assert_eq!(object.incarnation, donor.incarnation);
    assert_eq!(object.zone, Zone::Battlefield);
    assert_eq!((object.owner, object.controller), (P1, P1));
    assert_eq!(object.counters.get(&CounterType::Plus1Plus1), Some(&6));
    assert!(!object.is_token);
}

fn assert_clone_prompt(
    runner: &GameRunner,
    stack_pin: ObjectIncarnationRef,
    donor: ObjectIncarnationRef,
) -> ObjectIncarnationRef {
    let WaitingFor::CopyTargetChoice {
        player,
        source_id,
        purpose,
        valid_targets,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected Clone's copy choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*source_id, stack_pin.object_id);
    assert_eq!(*purpose, CopyTargetPurpose::BecomeCopy);
    assert!(
        valid_targets.contains(&donor.object_id),
        "{valid_targets:?}"
    );
    assert_entry_incarnation(runner, stack_pin, Zone::Stack)
}

#[test]
fn clone_uses_its_own_escape_payment_after_restoring_the_copy_choice() {
    for escape in [true, false] {
        let mut scenario = GameScenario::new();
        scenario
            .at_phase(Phase::PreCombatMain)
            .with_mana_pool(P0, mana_pool(&[ManaType::Blue], 3));
        let donor = add_polukranos_donor(&mut scenario);
        let muldrotha = add_muldrotha(&mut scenario);
        scenario.add_enchantment_from_oracle(
            P0,
            "Underworld Breach",
            "Each nonland card in your graveyard has escape. The escape cost is equal to the card's mana cost plus exile three other cards from your graveyard. (You may cast cards from your graveyard for their escape cost.)\nAt the beginning of the end step, sacrifice this enchantment.",
        );
        let clone_cost = mana_cost(3, &[ManaCostShard::Blue]);
        let clone = scenario
            .add_creature_to_graveyard(P0, "Clone", 0, 0)
            .with_subtypes(vec!["Shapeshifter"])
            .with_mana_cost(clone_cost.clone())
            .from_oracle_text(
                "You may have this creature enter as a copy of any creature on the battlefield.",
            )
            .id();
        let filler = add_filler(&mut scenario, 3);
        let mut runner = scenario.build();

        let donor_pin = cast_polukranos_donor(&mut runner, donor);
        let paid =
            choose_graveyard_cast(&mut runner, clone, muldrotha, &filler, clone_cost, escape);
        let stack_pin = finalized_spell(&mut runner, clone, Zone::Graveyard, paid.clone(), 4);
        assert_graveyard_payment(&runner, muldrotha, &filler, escape);
        pass_to_resolution(&mut runner);

        let WaitingFor::ReplacementChoice {
            player,
            kind: ReplacementChoiceKind::OptionalBranch,
            candidates,
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "expected Clone's optional replacement, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*player, P0);
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.source_id)
                .collect::<Vec<_>>(),
            vec![clone, clone],
        );
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .unwrap();
        let battlefield_pin = assert_clone_prompt(&runner, stack_pin, donor_pin);
        assert_resolving_spell(&runner, stack_pin, paid.clone());
        assert_eq!(
            runner.state().objects[&clone].cast_origin_zone(),
            Some(Zone::Graveyard)
        );

        let mut runner = checked_reload(&runner, stack_pin);
        assert_eq!(
            assert_clone_prompt(&runner, stack_pin, donor_pin),
            battlefield_pin
        );
        assert_resolving_spell(&runner, stack_pin, paid);
        assert_eq!(
            runner.state().objects[&clone].cast_origin_zone(),
            Some(Zone::Graveyard)
        );
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(donor)),
            })
            .expect("Clone must copy the offered Polukranos");
        assert_eq!(
            assert_entry_counters(&runner, stack_pin, Zone::Stack, if escape { 12 } else { 6 }),
            battlefield_pin
        );
        let copy = &runner.state().objects[&clone];
        assert_eq!(copy.name, POLUKRANOS_NAME);
        assert_eq!((copy.owner, copy.controller), (P0, P0));
        assert!(!copy.is_token);
        assert_polukranos_donor(&runner, donor_pin);
    }
}

#[test]
fn copied_polukranos_spell_keeps_the_cost_choice_but_enters_with_six() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_mana_pool(
        P0,
        mana_pool(
            &[
                ManaType::Black,
                ManaType::Green,
                ManaType::Green,
                ManaType::Blue,
            ],
            4,
        ),
    );
    let polukranos = polukranos_in_graveyard(&mut scenario);
    let filler = add_filler(&mut scenario, 6);
    let double_major = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Double Major",
            true,
            "Copy target creature spell you control, except it isn't legendary if the spell is legendary. (A copy of a creature spell becomes a token.)",
        )
        .with_mana_cost(mana_cost(0, &[ManaCostShard::Green, ManaCostShard::Blue]))
        .id();
    let mut runner = scenario.build();

    let prompt = cast_spell(&mut runner, polukranos);
    pay_escape_exile(&mut runner, prompt, &filler);
    let original = finalized_spell(
        &mut runner,
        polukranos,
        Zone::Graveyard,
        CastingVariant::Escape,
        6,
    );
    let target = TargetRef::Object(polukranos);
    cast_spell_with_targets(&mut runner, double_major, vec![polukranos]);
    finalized_spell(
        &mut runner,
        double_major,
        Zone::Hand,
        CastingVariant::Normal,
        2,
    );
    let ability = runner
        .state()
        .stack
        .last()
        .and_then(|entry| entry.ability())
        .expect("Double Major must carry its selected creature spell");
    assert_eq!(ability.targets, vec![target]);
    let target_object = &runner.state().objects[&polukranos];
    assert_eq!(target_object.zone, Zone::Stack);
    assert_eq!(target_object.incarnation, original.incarnation);
    let events = pass_to_resolution(&mut runner);
    let copies: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::SpellCopied {
                original_id,
                object_id,
                controller: P0,
                ..
            } if *original_id == polukranos => Some(*object_id),
            _ => None,
        })
        .collect();
    let [copy_id] = copies.as_slice() else {
        panic!("expected one copy of the escaped spell, got {copies:?}");
    };
    assert_ne!(*copy_id, polukranos);
    let state = runner.state();
    let [copied, cast] = [*copy_id, polukranos].map(|id| &state.objects[&id]);
    assert_eq!(copied.zone, Zone::Stack);
    assert_eq!(cast.zone, Zone::Stack);
    let copy = ObjectIncarnationRef::of(*copy_id, copied.incarnation);
    assert_eq!(
        ObjectIncarnationRef::of(polukranos, cast.incarnation),
        original
    );
    assert_eq!(copied.cast_origin_zone(), None);
    assert_eq!(cast.cast_origin_zone(), Some(Zone::Graveyard));
    assert_eq!(
        state.stack.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        vec![polukranos, *copy_id]
    );
    assert!(state.stack.iter().all(|entry| matches!(
        entry.kind,
        StackEntryKind::Spell {
            casting_variant: CastingVariant::Escape,
            ..
        }
    )));

    pass_to_resolution(&mut runner);
    assert_entry_counters(&runner, copy, Zone::Stack, 6);
    assert_eq!(runner.state().objects[&polukranos].zone, Zone::Stack);
    pass_to_resolution(&mut runner);
    assert_entry_counters(&runner, original, Zone::Stack, 12);
    assert_entry_counters(&runner, copy, Zone::Stack, 6);
    let copied = &runner.state().objects[copy_id];
    let cast = &runner.state().objects[&polukranos];
    assert!(copied.is_token);
    assert!(!cast.is_token);
    assert!(!copied.card_types.supertypes.contains(&Supertype::Legendary));
    assert!(cast.card_types.supertypes.contains(&Supertype::Legendary));
}

#[test]
fn rite_of_replication_creates_a_polukranos_token_with_six_counters() {
    let mut scenario = GameScenario::new();
    scenario
        .at_phase(Phase::PreCombatMain)
        .with_mana_pool(P0, mana_pool(&[ManaType::Blue, ManaType::Blue], 2));
    let donor = add_polukranos_donor(&mut scenario);
    let rite = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Rite of Replication",
            false,
            "Kicker {5} (You may pay an additional {5} as you cast this spell.)\nCreate a token that's a copy of target creature. If this spell was kicked, create five of those tokens instead.",
        )
        .with_mana_cost(mana_cost(2, &[ManaCostShard::Blue, ManaCostShard::Blue]))
        .id();
    let mut runner = scenario.build();

    let donor_pin = cast_polukranos_donor(&mut runner, donor);
    let _ = runner
        .cast(rite)
        .target_object(donor)
        .decline_optional()
        .commit();
    finalized_spell(&mut runner, rite, Zone::Hand, CastingVariant::Normal, 4);
    assert!(runner.state().objects[&rite].kickers_paid.is_empty());
    let events = pass_to_resolution(&mut runner);
    let tokens: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::TokenCreated {
                object_id,
                source_id,
                name,
            } if *source_id == rite => {
                assert_eq!(name, POLUKRANOS_NAME);
                Some(*object_id)
            }
            _ => None,
        })
        .collect();
    let [token_id] = tokens.as_slice() else {
        panic!("unkicked Rite must create one token, got {tokens:?}");
    };
    assert_ne!(*token_id, donor);
    let token = &runner.state().objects[token_id];
    assert_eq!(token.zone, Zone::Battlefield);
    assert_eq!((token.owner, token.controller), (P0, P0));
    assert!(token.is_token);
    assert_eq!(token.counters.get(&CounterType::Plus1Plus1), Some(&6));
    assert_polukranos_donor(&runner, donor_pin);
}
