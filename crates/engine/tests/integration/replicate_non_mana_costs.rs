use engine::game::engine::{apply, EngineError};
use engine::game::mana_sources::activatable_mana_source_selections;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, AdditionalCostOrigin, ChosenAttribute, QuantityModification,
    ReplacementDefinition, TapCreaturesSelectionMode, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, ConvokeMode, DeferredLifeCostResume, ManaChoice, PayCostKind,
    PersistedGameState, PersistedRestoreFinalization, ShardChoice, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{parse_mtgjson_mana_cost, ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::ReplacementEvent;

const EXTERMINATE: &str = "Replicate—Tap an untapped Dalek you control. (When you cast this spell, copy it for each time you paid its replicate cost. You may choose new targets for the copies.)\nDestroy target creature. Its controller loses 3 life.";
const PSIONIC_RITUAL: &str = "Replicate—Tap an untapped Horror you control. (When you cast this spell, copy it for each time you paid its replicate cost. You may choose new targets for the copies.)\nExile target instant or sorcery card from a graveyard and copy it. You may cast the copy without paying its mana cost.\nExile Psionic Ritual.";
const REITERATING_BOLT: &str = "Replicate—Pay {E}{E}{E}. (When you cast this spell, copy it for each time you paid its replicate cost. You may choose new targets for the copies.)\nReiterating Bolt deals 3 damage to target creature or planeswalker.";
const WORT: &str = "When Wort enters, create two 1/1 red and green Goblin Warrior creature tokens.\nEach red or green instant or sorcery spell you cast has conspire. (As you cast the spell, you may tap two untapped creatures you control that share a color with it. When you do, copy it and you may choose new targets for the copy.)";
const HUGH: &str = "Artifact spells you cast have replicate {2}{U}. (Whenever you cast an artifact spell, copy it for each time you paid its replicate cost. Each copy becomes a token.)";
const THREEFOLD_SIGNAL: &str = "When this artifact enters, scry 3.\nEach spell you cast that's exactly three colors has replicate {3}. (When you cast it, copy it for each time you paid its replicate cost. You may choose new targets for the copies. A copy of a permanent spell becomes a token.)";
const AETHER_HUB: &str = "When this land enters, you get {E} (an energy counter).\n{T}: Add {C}.\n{T}, Pay {E}: Add one mana of any color.";
const CAETUS: &str = "Noncreature spells you cast have convoke. (Your creatures can help cast those spells. Each creature you tap while casting a noncreature spell pays for {1} or one mana of that creature's color.)\nAt the beginning of your end step, untap up to four target creatures.";
const ASHNODS_ALTAR: &str = "Sacrifice a creature: Add {C}{C}.";
const GRAND_ARCHITECT: &str = "Other blue creatures you control get +1/+1.\n{U}: Target artifact creature becomes blue until end of turn.\nTap an untapped blue creature you control: Add {C}{C}. Spend this mana only to cast artifact spells or activate abilities of artifacts.";
const PILI_PALA: &str = "Flying\n{2}, {Q}: Add one mana of any color. ({Q} is the untap symbol.)";
const MASKWOOD_NEXUS: &str = "Creatures you control are every creature type. The same is true for creature spells you control and creature cards you own that aren't on the battlefield.\n{3}, {T}: Create a 2/2 blue Shapeshifter creature token with changeling. (It is every creature type.)";
const PARADISE_MANTLE: &str =
    "Equipped creature has \"{T}: Add one mana of any color.\"\nEquip {1}";
const DALEK_SQUADRON: &str = "Menace\nMyriad (Whenever this creature attacks, for each opponent other than defending player, you may create a token copy that's tapped and attacking that player or a planeswalker they control. Exile the tokens at end of combat.)";
const KRARK_CLAN_IRONWORKS: &str = "Sacrifice an artifact: Add {C}{C}.";
const HUDDLE_UP: &str = "Assist (Another player can pay up to {2} of this spell's cost.)\nTwo target players each draw a card.";
const DJINN_ILLUMINATUS: &str = "({U/R} can be paid with either {U} or {R}.)\nFlying\nEach instant and sorcery spell you cast has replicate. The replicate cost is equal to its mana cost. (When you cast it, copy it for each time you paid its replicate cost. You may choose new targets for the copies.)";

const KILLIAN: &str = "Lifelink\nMenace (This creature can't be blocked except by two or more creatures.)\nSpells you cast that target a creature cost {2} less to cast.";

const PYROMATICS: &str = "Replicate {1}{R} (When you cast this spell, copy it for each time you paid its replicate cost. You may choose new targets for the copies.)\nPyromatics deals 1 damage to any target.";

const PAINTER: &str = "As this creature enters, choose a color.\nAll cards that aren't on the battlefield, spells, and permanents are the chosen color in addition to their other colors.";

fn act(runner: &mut GameRunner, action: GameAction) -> Vec<GameEvent> {
    runner
        .act(action.clone())
        .unwrap_or_else(|error| panic!("{action:?} failed: {error:?}"))
        .events
}

fn cast(runner: &mut GameRunner, spell: ObjectId, payment_mode: CastPaymentMode) {
    let card_id = runner.state().objects[&spell].card_id;
    act(
        runner,
        GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode,
        },
    );
}

fn offer(runner: &GameRunner, origin: AdditionalCostOrigin, paid: u32) {
    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::OptionalCostChoice { player: P0, origin: actual, times_kicked, .. }
                if *actual == origin && *times_kicked == paid
        ),
        "expected {origin:?} offer after {paid} payments, got {:?}",
        runner.state().waiting_for
    );
}

fn decide(runner: &mut GameRunner, pay: bool) {
    act(
        runner,
        GameAction::DecideOptionalCost {
            index: usize::from(!pay),
        },
    );
}

fn select(cards: &[ObjectId]) -> GameAction {
    GameAction::SelectCards {
        cards: cards.to_vec(),
    }
}

fn tap_choices(runner: &GameRunner, expected: usize) -> &[ObjectId] {
    let WaitingFor::PayCost {
        player: P0,
        kind:
            PayCostKind::TapCreatures {
                mode: TapCreaturesSelectionMode::Fixed,
            },
        choices,
        count,
        min_count,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected fixed tap payment, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!((*min_count, *count), (expected, expected));
    choices
}

fn target(id: ObjectId) -> GameAction {
    GameAction::ChooseTarget {
        target: Some(TargetRef::Object(id)),
    }
}

fn target_prompt(runner: &GameRunner) {
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { player: P0, .. }
    ));
}

fn pass(runner: &mut GameRunner) -> Vec<GameEvent> {
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    act(runner, GameAction::PassPriority)
}

fn pass_to_prompt(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..20 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            return events;
        }
        assert!(
            !runner.state().stack.is_empty(),
            "expected a resolving instruction"
        );
        events.extend(pass(runner));
    }
    panic!("the stack did not reach its prompt");
}

fn finish(runner: &mut GameRunner) {
    for _ in 0..20 {
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        if runner.state().stack.is_empty() {
            return;
        }
        pass(runner);
    }
    panic!("stack did not empty");
}

fn copy_prompt(runner: &GameRunner, original_target: ObjectId) -> ObjectId {
    let WaitingFor::CopyRetarget {
        player: P0,
        copy_id,
        target_slots,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected copy retarget, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(target_slots.len(), 1);
    assert_eq!(
        target_slots[0].current,
        Some(TargetRef::Object(original_target))
    );
    *copy_id
}

fn records(runner: &GameRunner, spell: ObjectId) -> Vec<(u32, u32)> {
    runner.state().objects[&spell]
        .additional_cost_payments
        .iter()
        .filter(|record| record.origin == AdditionalCostOrigin::Replicate)
        .map(|record| (record.origin_ordinal, record.count))
        .collect()
}

fn round_trip(runner: &GameRunner) -> GameRunner {
    let json = serde_json::to_string(&PersistedGameState::capture(runner.state().clone())).unwrap();
    let state = serde_json::from_str::<PersistedGameState>(&json)
        .unwrap()
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .unwrap()
        .finalize_after_rehydration(|_| Ok(()))
        .unwrap();
    GameRunner::from_state(state)
}

fn pool(scenario: &mut GameScenario, mana: ManaType, count: usize) {
    scenario.with_mana_pool(
        P0,
        (0..count)
            .map(|_| ManaUnit::new(mana, ObjectId(0), false, vec![]))
            .collect(),
    );
}

struct DalekBoard {
    runner: GameRunner,
    spell: ObjectId,
    mana_dalek: ObjectId,
    sick_dalek: ObjectId,
    tapped_dalek: ObjectId,
    opponent_dalek: ObjectId,
    original_target: ObjectId,
    new_target: ObjectId,
    swamp: Option<ObjectId>,
    helpers: Vec<ObjectId>,
}

fn dalek_board(spare_mana: bool, conspire_helpers: Option<usize>) -> DalekBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_artifact_from_oracle(P0, "Maskwood Nexus", MASKWOOD_NEXUS);
    let mana_dalek = scenario
        .add_creature_from_oracle(P0, "Leaden Myr", 1, 1, "{T}: Add {B}.")
        .as_artifact_creature()
        .with_subtypes(vec!["Myr"])
        .with_mana_cost(parse_mtgjson_mana_cost("{2}"))
        .id();
    let sick_dalek = scenario
        .add_creature_from_oracle(P0, "Dalek Squadron", 3, 3, DALEK_SQUADRON)
        .as_artifact_creature()
        .with_subtypes(vec!["Dalek"])
        .with_mana_cost(parse_mtgjson_mana_cost("{2}{B}"))
        .with_summoning_sickness()
        .id();
    let tapped_dalek = scenario
        .add_creature_from_oracle(P0, "Dalek Squadron", 3, 3, DALEK_SQUADRON)
        .as_artifact_creature()
        .with_subtypes(vec!["Dalek"])
        .with_mana_cost(parse_mtgjson_mana_cost("{2}{B}"))
        .id();
    let opponent_dalek = scenario
        .add_creature_from_oracle(P1, "Dalek Squadron", 3, 3, DALEK_SQUADRON)
        .as_artifact_creature()
        .with_subtypes(vec!["Dalek"])
        .with_mana_cost(parse_mtgjson_mana_cost("{2}{B}"))
        .id();
    let original_target = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let new_target = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let swamp = spare_mana.then(|| scenario.add_basic_land(P0, ManaColor::Black));
    pool(&mut scenario, ManaType::Colorless, 2);
    let mut helpers = Vec::new();
    let conspire = conspire_helpers.map(|count| {
        let wort = scenario
            .add_creature_from_oracle(P0, "Wort, the Raidmother", 3, 3, WORT)
            .with_mana_cost(parse_mtgjson_mana_cost("{4}{R/G}{R/G}"))
            .id();
        let painter = scenario
            .add_creature_from_oracle(P1, "Painter's Servant", 1, 3, PAINTER)
            .as_artifact_creature()
            .with_mana_cost(parse_mtgjson_mana_cost("{2}"))
            .id();
        for _ in 0..count {
            helpers.push(
                scenario
                    .add_creature(P0, "Mons's Goblin Raiders", 1, 1)
                    .with_mana_cost(parse_mtgjson_mana_cost("{R}"))
                    .id(),
            );
        }
        (wort, painter)
    });
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Exterminate!", false, EXTERMINATE)
        .with_mana_cost(parse_mtgjson_mana_cost("{2}{B}"))
        .id();
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = ["Dalek", "Myr", "Goblin", "Warrior", "Bear"]
        .into_iter()
        .map(str::to_string)
        .collect();
    for id in [tapped_dalek, new_target] {
        runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
    }
    if let Some((wort, painter)) = conspire {
        runner.state_mut().objects.get_mut(&wort).unwrap().tapped = true;
        runner
            .state_mut()
            .objects
            .get_mut(&painter)
            .unwrap()
            .chosen_attributes
            .push(ChosenAttribute::Color(ManaColor::Red));
    }
    DalekBoard {
        runner,
        spell,
        mana_dalek,
        sick_dalek,
        tapped_dalek,
        opponent_dalek,
        original_target,
        new_target,
        swamp,
        helpers,
    }
}

#[test]
fn exterminate_tap_cost_display_round_trips_through_client_adapters() {
    use engine::game::derived_views::{ClientGameState, ClientGameStateRef};
    use engine::game::visibility::{filter_state_for_viewer, HandDisclosure};
    use engine::types::game_state::GameState;
    use serde_json::{json, Value};

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(
            P0,
            "Dalek Drone",
            3,
            3,
            "Flying, menace\nExterminate! — When this creature enters, destroy target creature an opponent controls. That player loses 3 life.",
        )
        .with_subtypes(vec!["Dalek"]);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Exterminate!", false, EXTERMINATE)
        .with_mana_cost(parse_mtgjson_mana_cost("{2}{B}"))
        .id();
    pool(&mut scenario, ManaType::Colorless, 2);
    pool(&mut scenario, ManaType::Black, 1);
    let mut runner = scenario.build();
    cast(&mut runner, spell, CastPaymentMode::Manual);
    offer(&runner, AdditionalCostOrigin::Replicate, 0);

    let assert_display = |wire: &Value| {
        let display = json!({"type": "ControlledSubtype", "subtype": "Dalek"});
        let keywords = wire["objects"][spell.0.to_string()]["keywords"]
            .as_array()
            .expect("the caster sees their spell's keywords");
        assert!(
            keywords
                .iter()
                .any(|keyword| keyword["Replicate"]["tap_display"] == display),
            "expected the Dalek display descriptor on Replicate, got {keywords:?}"
        );
        assert_eq!(
            wire["waiting_for"]["data"]["cost"]["data"]["cost"]["tap_display"],
            display
        );
    };

    let state = runner.state();
    let local = serde_json::to_value(ClientGameStateRef::wrap_with_hand_disclosure(
        state,
        P0,
        HandDisclosure::Rules,
    ))
    .unwrap();
    assert_display(&local["state"]);
    let local: ClientGameState = serde_json::from_value(local).unwrap();
    assert_display(&serde_json::to_value(local.state).unwrap());

    let remote = serde_json::to_value(filter_state_for_viewer(state, P0)).unwrap();
    assert_display(&remote);
    let remote: GameState = serde_json::from_value(remote).unwrap();
    assert_display(&serde_json::to_value(remote).unwrap());
}

fn reject_tap(runner: &mut GameRunner, id: ObjectId) {
    assert!(runner.act(select(&[id])).is_err());
    assert!(!tap_choices(runner, 1).is_empty());
}

fn assert_spell_target(runner: &GameRunner, spell: ObjectId, target: ObjectId) {
    let entry = runner
        .state()
        .stack
        .iter()
        .find(|entry| {
            entry.source_id == spell && matches!(entry.kind, StackEntryKind::Spell { .. })
        })
        .expect("spell is on the stack");
    assert_eq!(
        entry.ability().expect("spell ability").targets,
        [TargetRef::Object(target)]
    );
}

// CR 601.2b and CR 302.6: selection precedes targeting and summoning sickness
// does not prevent paying the written tap cost.
#[test]
fn exterminate_reserves_distinct_daleks_then_copies_and_retargets() {
    for mode in [CastPaymentMode::Auto, CastPaymentMode::Manual] {
        let DalekBoard {
            mut runner,
            spell,
            mana_dalek,
            sick_dalek,
            tapped_dalek,
            opponent_dalek,
            original_target,
            new_target,
            swamp,
            ..
        } = dalek_board(true, None);
        let mana_selections =
            |runner: &GameRunner| activatable_mana_source_selections(runner.state(), P0);
        assert!(runner.state().objects[&sick_dalek].summoning_sick);
        cast(&mut runner, spell, mode);
        offer(&runner, AdditionalCostOrigin::Replicate, 0);
        decide(&mut runner, true);
        let choices = tap_choices(&runner, 1);
        assert_eq!(choices.len(), 2);
        assert!(choices.contains(&mana_dalek) && choices.contains(&sick_dalek));
        for invalid in [tapped_dalek, opponent_dalek, new_target] {
            reject_tap(&mut runner, invalid);
        }
        assert!(runner.act(select(&[mana_dalek, mana_dalek])).is_err());
        act(&mut runner, select(&[mana_dalek]));
        assert!(!runner.state().objects[&mana_dalek].tapped);
        assert!(!mana_selections(&runner)
            .iter()
            .any(|selection| selection.source.object_id == mana_dalek));
        offer(&runner, AdditionalCostOrigin::Replicate, 1);
        decide(&mut runner, true);
        assert_eq!(tap_choices(&runner, 1), [sick_dalek]);
        reject_tap(&mut runner, mana_dalek);
        act(&mut runner, select(&[sick_dalek]));
        target_prompt(&runner);
        assert!(!runner.state().objects[&sick_dalek].tapped);
        assert!(runner.act(target(swamp.unwrap())).is_err());
        target_prompt(&runner);
        let mut payment_events = act(&mut runner, target(original_target));
        if mode == CastPaymentMode::Manual {
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::ManaPayment { player: P0, .. }
            ));
            assert!(!runner.state().objects[&mana_dalek].tapped);
            assert_eq!(runner.state().players[0].mana_pool.total(), 2);
            let selection = mana_selections(&runner)
                .into_iter()
                .find(|selection| Some(selection.source.object_id) == swamp)
                .expect("spare black mana");
            act(&mut runner, GameAction::TapLandForMana { selection });
            payment_events.extend(act(&mut runner, GameAction::PassPriority));
        }
        for dalek in [mana_dalek, sick_dalek] {
            assert_eq!(
                payment_events
                    .iter()
                    .filter(|event| matches!(
                        event, GameEvent::PermanentTapped { object_id, .. } if *object_id == dalek
                    ))
                    .count(),
                1
            );
        }
        assert!(payment_events.iter().any(|event| matches!(
            event, GameEvent::SpellCast { object_id, cast_mana_spent: Some(3), .. }
                if *object_id == spell
        )));
        assert_eq!(records(&runner, spell), [(0, 1), (0, 1)]);
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert!(runner.state().objects[&swamp.unwrap()].tapped);
        assert!(runner.state().objects[&mana_dalek].tapped);
        assert!(runner.state().objects[&sick_dalek].tapped);
        assert_eq!(
            runner.state().stack.len(),
            2,
            "one trigger for the paid instance"
        );

        let mut copy_events = pass_to_prompt(&mut runner);
        let first = copy_prompt(&runner, original_target);
        copy_events.extend(act(&mut runner, GameAction::KeepAllCopyTargets));
        assert_spell_target(&runner, first, original_target);
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            copy_events.extend(pass_to_prompt(&mut runner));
        }
        let second = copy_prompt(&runner, original_target);
        assert_ne!(first, second);
        assert!(runner.act(target(swamp.unwrap())).is_err());
        assert_eq!(copy_prompt(&runner, original_target), second);
        copy_events.extend(act(&mut runner, target(new_target)));
        assert!(!copy_events
            .iter()
            .any(|event| matches!(event, GameEvent::SpellCast { .. })));
        assert_spell_target(&runner, spell, original_target);
        assert_spell_target(&runner, second, new_target);
        finish(&mut runner);
        assert_eq!(
            runner.state().objects[&original_target].zone,
            Zone::Graveyard
        );
        assert_eq!(runner.state().objects[&new_target].zone, Zone::Graveyard);
        assert_eq!(runner.state().players[0].life, 17);
        assert_eq!(runner.state().players[1].life, 17);
    }
}

// CR 601.2g + CR 605.3a + CR 107.6 + CR 601.2h: a reserved Dalek may tap
// for mana and untap again before it pays Replicate.
#[test]
fn exterminate_reserved_pili_pala_can_tap_and_untap_for_mana() {
    for untap in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_artifact_from_oracle(P0, "Maskwood Nexus", MASKWOOD_NEXUS);
        let architect = scenario
            .add_creature_from_oracle(P0, "Grand Architect", 1, 3, GRAND_ARCHITECT)
            .with_color(vec![ManaColor::Blue])
            .id();
        let pili = scenario
            .add_creature_from_oracle(P0, "Pili-Pala", 1, 1, PILI_PALA)
            .as_artifact_creature()
            .id();
        let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Exterminate!", false, EXTERMINATE)
            .with_mana_cost(parse_mtgjson_mana_cost("{2}{B}"))
            .id();
        scenario.with_mana_pool(
            P0,
            [
                ManaType::Blue,
                ManaType::Black,
                ManaType::Black,
                ManaType::Black,
            ]
            .map(|mana| ManaUnit::new(mana, ObjectId(0), false, vec![]))
            .to_vec(),
        );
        let mut runner = scenario.build();
        // Supply the creature-type catalog normally installed by deck loading.
        runner.state_mut().all_creature_types = ["Dalek", "Scarecrow", "Vedalken", "Artificer"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let activate = |source_id, ability_index| GameAction::ActivateAbility {
            source_id,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        };
        act(&mut runner, activate(architect, 0));
        // Pili-Pala is the only artifact creature, so the activation chooses it.
        finish(&mut runner);

        cast(&mut runner, spell, CastPaymentMode::Manual);
        offer(&runner, AdditionalCostOrigin::Replicate, 0);
        decide(&mut runner, true);
        assert!(tap_choices(&runner, 1).contains(&pili));
        act(&mut runner, select(&[pili]));
        offer(&runner, AdditionalCostOrigin::Replicate, 1);
        decide(&mut runner, false);
        act(&mut runner, target(victim));
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ManaPayment { .. }
        ));
        act(&mut runner, activate(architect, 1));
        assert!(tap_choices(&runner, 1).contains(&pili));
        let mut events = act(&mut runner, select(&[pili]));
        runner = round_trip(&runner);
        assert!(runner.state().objects[&pili].tapped);
        assert_eq!(runner.state().players[0].mana_pool.total(), 5);
        let restricted = |runner: &GameRunner| {
            runner.state().players[0]
                .mana_pool
                .units()
                .filter(|unit| !unit.restrictions.is_empty())
                .count()
        };
        assert_eq!(restricted(&runner), 2);
        if !untap {
            assert!(matches!(
                runner.act(GameAction::PassPriority),
                Err(EngineError::ActionNotAllowed(message))
                    if message.contains("can no longer pay it")
            ));
            assert!(records(&runner, spell).is_empty());
            continue;
        }

        events.extend(act(&mut runner, activate(pili, 0)));
        events.extend(act(
            &mut runner,
            GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Black),
                count: 1,
            },
        ));
        assert!(!runner.state().objects[&pili].tapped);
        assert_eq!(runner.state().players[0].mana_pool.total(), 4);
        assert_eq!(restricted(&runner), 0);
        events.extend(act(&mut runner, GameAction::PassPriority));
        let pili_changes: Vec<_> = events
            .iter()
            .filter_map(|event| {
                if matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == pili) {
                    Some(true)
                } else if matches!(event, GameEvent::PermanentUntapped { object_id, .. } if *object_id == pili) {
                    Some(false)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(pili_changes, [true, false, true]);
        assert_eq!(records(&runner, spell), [(0, 1)]);
        assert!(runner.state().objects[&pili].tapped);
        assert_eq!(runner.state().players[0].mana_pool.total(), 1);
    }
}

#[test]
fn exterminate_can_reserve_a_dalek_beyond_the_preview_budget() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_artifact_from_oracle(P0, "Maskwood Nexus", MASKWOOD_NEXUS);
    // Seed the battlefield with the tokens Nexus produces. Their age does
    // not affect payment, which uses no tap symbol (CR 302.6).
    let tokens: Vec<_> = (0..70)
        .map(|_| {
            scenario
                .add_creature(P0, "Shapeshifter", 2, 2)
                .with_subtypes(vec!["Shapeshifter"])
                .with_color(vec![ManaColor::Blue])
                .with_keyword(engine::types::keywords::Keyword::Changeling)
                .id()
        })
        .collect();
    let myr = scenario
        .add_creature_from_oracle(P0, "Leaden Myr", 1, 1, "{T}: Add {B}.")
        .as_artifact_creature()
        .with_subtypes(vec!["Myr"])
        .id();
    let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Exterminate!", false, EXTERMINATE)
        .with_mana_cost(parse_mtgjson_mana_cost("{2}{B}"))
        .id();
    pool(&mut scenario, ManaType::Colorless, 2);
    let mut runner = scenario.build();
    for id in tokens {
        runner.state_mut().objects.get_mut(&id).unwrap().is_token = true;
    }
    runner.state_mut().all_creature_types =
        ["Dalek", "Myr"].into_iter().map(str::to_string).collect();
    cast(&mut runner, spell, CastPaymentMode::Auto);
    offer(&runner, AdditionalCostOrigin::Replicate, 0);
    decide(&mut runner, true);
    let choices = tap_choices(&runner, 1);
    assert_eq!(choices.len(), 71);
    assert_eq!(
        choices[70], myr,
        "the only black source is offered after the probe budget"
    );
    let chosen = choices[69];
    let choices = choices.to_vec();
    let pending = runner
        .state()
        .waiting_for
        .pending_cast_ref()
        .unwrap()
        .clone();
    let mana = runner.state().players[0].mana_pool.clone();
    reject_tap(&mut runner, myr);
    assert_eq!(tap_choices(&runner, 1), choices);
    assert_eq!(
        runner.state().waiting_for.pending_cast_ref(),
        Some(&pending)
    );
    assert!(records(&runner, spell).is_empty());
    assert!(!runner.state().objects[&myr].tapped);
    assert_eq!(runner.state().players[0].mana_pool, mana);
    act(&mut runner, select(&[chosen]));
    offer(&runner, AdditionalCostOrigin::Replicate, 1);
    decide(&mut runner, false);
    target_prompt(&runner);
    act(&mut runner, target(victim));
    // CR 601.2h: The selected creature pays only when the spell commits.
    assert!(runner.state().objects[&chosen].tapped);
    assert_eq!(records(&runner, spell), [(0, 1)]);
    pass_to_prompt(&mut runner);
    let copy = copy_prompt(&runner, victim);
    act(&mut runner, GameAction::KeepAllCopyTargets);
    assert_spell_target(&runner, copy, victim);
    finish(&mut runner);
}

#[test]
fn exterminate_offers_pili_pala_when_its_tap_untap_pays_for_the_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_artifact_from_oracle(P0, "Maskwood Nexus", MASKWOOD_NEXUS);
    let pili = scenario
        .add_creature_from_oracle(P0, "Pili-Pala", 1, 1, PILI_PALA)
        .as_artifact_creature()
        .with_subtypes(vec!["Scarecrow"])
        .id();
    let mantle = scenario
        .add_artifact_from_oracle(P0, "Paradise Mantle", PARADISE_MANTLE)
        .with_subtypes(vec!["Equipment"])
        .id();
    let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Exterminate!", false, EXTERMINATE)
        .with_mana_cost(parse_mtgjson_mana_cost("{2}{B}"))
        .id();
    pool(&mut scenario, ManaType::Colorless, 4);
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = ["Dalek", "Scarecrow"]
        .into_iter()
        .map(str::to_string)
        .collect();
    engine::game::effects::attach::attach_to(runner.state_mut(), mantle, pili);

    cast(&mut runner, spell, CastPaymentMode::Auto);
    offer(&runner, AdditionalCostOrigin::Replicate, 0);
    decide(&mut runner, true);
    assert_eq!(tap_choices(&runner, 1), [pili]);
    act(&mut runner, select(&[pili]));
    target_prompt(&runner);
    act(&mut runner, target(victim));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    let activate = |ability_index| GameAction::ActivateAbility {
        source_id: pili,
        ability_index,
        payment_mode: CastPaymentMode::Auto,
    };
    // CR 601.2g-h + CR 107.6: Tap with the Mantle grant, then untap for mana
    // so Pili-Pala can still pay Replicate at commit.
    for index in [1, 0] {
        act(&mut runner, activate(index));
        act(
            &mut runner,
            GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Black),
                count: 1,
            },
        );
    }
    assert!(!runner.state().objects[&pili].tapped);
    act(&mut runner, GameAction::PassPriority);
    assert!(runner.state().objects[&pili].tapped);
    assert_eq!(records(&runner, spell), [(0, 1)]);
    pass_to_prompt(&mut runner);
    let copy = copy_prompt(&runner, victim);
    act(&mut runner, GameAction::KeepAllCopyTargets);
    assert_spell_target(&runner, copy, victim);
    finish(&mut runner);
}

#[test]
fn exterminate_can_reserve_pili_pala_after_it_attacks() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_artifact_from_oracle(P0, "Maskwood Nexus", MASKWOOD_NEXUS);
    let pili = scenario
        .add_creature_from_oracle(P0, "Pili-Pala", 1, 1, PILI_PALA)
        .as_artifact_creature()
        .with_subtypes(vec!["Scarecrow"])
        .with_mana_cost(parse_mtgjson_mana_cost("{2}"))
        .id();
    let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Exterminate!", false, EXTERMINATE)
        .with_mana_cost(parse_mtgjson_mana_cost("{2}{B}"))
        .id();
    scenario.add_basic_land(P0, ManaColor::Black);
    for _ in 0..4 {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = ["Dalek", "Scarecrow"]
        .into_iter()
        .map(str::to_string)
        .collect();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(pili, engine::game::combat::AttackTarget::Player(P1))])
        .unwrap();
    for _ in 0..16 {
        if runner.state().phase == Phase::PostCombatMain {
            break;
        }
        match runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                pass(&mut runner);
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).unwrap();
            }
            ref other => panic!("unexpected combat prompt {other:?}"),
        }
    }
    assert_eq!(runner.state().phase, Phase::PostCombatMain);
    assert!(runner.state().objects[&pili].tapped);
    for selection in activatable_mana_source_selections(runner.state(), P0) {
        if selection.source.object_id != pili {
            act(&mut runner, GameAction::TapLandForMana { selection });
        }
    }
    cast(&mut runner, spell, CastPaymentMode::Auto);
    offer(&runner, AdditionalCostOrigin::Replicate, 0);
    decide(&mut runner, true);
    assert_eq!(tap_choices(&runner, 1), [pili]);
    act(&mut runner, select(&[pili]));
    act(&mut runner, target(victim));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    assert!(
        runner.act(GameAction::PassPriority).is_err(),
        "a still-tapped Pili-Pala cannot pay"
    );
    assert!(runner.state().objects[&pili].tapped);
    act(
        &mut runner,
        GameAction::ActivateAbility {
            source_id: pili,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        },
    );
    act(
        &mut runner,
        GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Black),
            count: 1,
        },
    );
    assert!(!runner.state().objects[&pili].tapped);
    act(&mut runner, GameAction::PassPriority);
    assert!(runner.state().objects[&pili].tapped);
    assert_eq!(records(&runner, spell), [(0, 1)]);
    pass_to_prompt(&mut runner);
    let copy = copy_prompt(&runner, victim);
    act(&mut runner, GameAction::KeepAllCopyTargets);
    assert_spell_target(&runner, copy, victim);
}

#[test]
fn exterminate_restores_a_stale_reservation_but_only_cancellation_can_finish_it() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let nexus = scenario
        .add_artifact_from_oracle(P0, "Maskwood Nexus", MASKWOOD_NEXUS)
        .id();
    let ironworks = scenario
        .add_artifact_from_oracle(P0, "Krark-Clan Ironworks", KRARK_CLAN_IRONWORKS)
        .id();
    let bears = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    let victim = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Exterminate!", false, EXTERMINATE)
        .with_mana_cost(parse_mtgjson_mana_cost("{2}{B}"))
        .id();
    pool(&mut scenario, ManaType::Black, 3);
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types =
        ["Dalek", "Bear"].into_iter().map(str::to_string).collect();
    cast(&mut runner, spell, CastPaymentMode::Manual);
    offer(&runner, AdditionalCostOrigin::Replicate, 0);
    decide(&mut runner, true);
    assert_eq!(tap_choices(&runner, 1), [bears]);
    act(&mut runner, select(&[bears]));
    target_prompt(&runner);
    act(&mut runner, target(victim));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    act(
        &mut runner,
        GameAction::ActivateAbility {
            source_id: ironworks,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        },
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost {
            player: P0,
            kind: PayCostKind::Sacrifice,
            ..
        }
    ));
    act(&mut runner, select(&[nexus]));
    assert_eq!(runner.state().objects[&nexus].zone, Zone::Graveyard);
    let mut restored = round_trip(&runner);
    assert!(matches!(
        restored.state().waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    // CR 601.2h: the Bears can no longer pay the selected Dalek cost.
    assert!(matches!(
        restored.act(GameAction::PassPriority),
        Err(EngineError::ActionNotAllowed(message)) if message.contains("can no longer pay it")
    ));
    act(&mut restored, GameAction::CancelCast);
    assert_eq!(restored.state().objects[&spell].zone, Zone::Hand);
    assert!(!restored.state().objects[&bears].tapped);
    assert!(restored.state().stack.is_empty());
    assert!(matches!(
        restored.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
}

#[test]
fn pyromatics_can_declare_replicate_before_killians_target_discount() {
    for creature_target in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let killian = scenario
            .add_creature_from_oracle(P0, "Killian, Ink Duelist", 2, 2, KILLIAN)
            .with_mana_cost(parse_mtgjson_mana_cost("{W}{B}"))
            .id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Pyromatics", true, PYROMATICS)
            .with_mana_cost(parse_mtgjson_mana_cost("{1}{R}"))
            .id();
        pool(&mut scenario, ManaType::Red, 2);
        let mut runner = scenario.build();
        let mana = runner.state().players[0].mana_pool.clone();
        cast(&mut runner, spell, CastPaymentMode::Auto);
        offer(&runner, AdditionalCostOrigin::Replicate, 0);
        decide(&mut runner, true);
        // A conservative declaration can offer another payment; settlement
        // must still price the selected targets and enforce the locked cost.
        if matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalCostChoice { .. }
        ) {
            decide(&mut runner, false);
        }
        if !creature_target {
            target_prompt(&runner);
            let result = runner.act(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(P1)),
            });
            if let Ok(result) = result {
                assert!(!result.events.iter().any(|event| matches!(
                    event, GameEvent::SpellCast { object_id, .. } if *object_id == spell
                )));
                if matches!(
                    runner.state().waiting_for,
                    WaitingFor::ManaPayment { player: P0, .. }
                ) {
                    assert!(runner.act(GameAction::PassPriority).is_err());
                } else {
                    assert!(matches!(
                        runner.state().waiting_for,
                        WaitingFor::Priority { player: P0 }
                    ));
                }
            }
            assert!(records(&runner, spell).is_empty());
            if let Some(pending) = runner
                .state()
                .waiting_for
                .pending_cast_ref()
                .or(runner.state().pending_cast.as_deref())
            {
                assert_eq!(pending.object_id, spell);
                act(&mut runner, GameAction::CancelCast);
            }
            assert_eq!(runner.state().objects[&spell].zone, Zone::Hand);
            assert!(runner.state().players[0].hand.contains(&spell));
            assert_eq!(runner.state().players[0].mana_pool, mana);
            assert!(records(&runner, spell).is_empty());
            assert!(runner.state().stack.is_empty());
            assert!(runner.state().pending_cast.is_none());
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { player: P0 }
            ));
            continue;
        }
        act(&mut runner, target(killian));
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_eq!(records(&runner, spell), [(0, 1)]);
        pass_to_prompt(&mut runner);
        let copy = copy_prompt(&runner, killian);
        act(&mut runner, GameAction::KeepAllCopyTargets);
        assert_spell_target(&runner, copy, killian);
    }
}

#[test]
fn huddle_up_can_assist_the_total_with_replicate_from_djinn_illuminatus() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Djinn Illuminatus", 3, 5, DJINN_ILLUMINATUS)
        .with_mana_cost(parse_mtgjson_mana_cost("{5}{U/R}{U/R}"));
    let spell = scenario
        .add_spell_to_hand(P0, "Huddle Up", false)
        .from_oracle_text_with_keywords(&["Assist"], HUDDLE_UP)
        .with_mana_cost(parse_mtgjson_mana_cost("{2}{U}"))
        .id();
    for player in [P0, P1] {
        scenario.with_library_top(player, &["Island", "Island", "Island"]);
    }
    pool(&mut scenario, ManaType::Blue, 2);
    scenario.with_mana_pool(
        P1,
        vec![ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]); 4],
    );
    let mut runner = scenario.build();
    cast(&mut runner, spell, CastPaymentMode::Manual);
    offer(&runner, AdditionalCostOrigin::Replicate, 0);
    decide(&mut runner, true);
    for player in [P0, P1] {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ) {
            act(
                &mut runner,
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(player)),
                },
            );
        }
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::AssistChoosePlayer { player: P0, .. }
    ));
    act(
        &mut runner,
        GameAction::ChooseAssistPlayer { player: Some(P1) },
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::AssistPayment { .. }
    ));
    // CR 702.132a: Assist can cover the generic mana in the entire total.
    act(&mut runner, GameAction::CommitAssistPayment { generic: 4 });
    if matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ) {
        act(&mut runner, GameAction::PassPriority);
    }
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert_eq!(runner.state().players[1].mana_pool.total(), 0);
    assert_eq!(records(&runner, spell), [(0, 1)]);
    pass(&mut runner);
    pass(&mut runner);
    let WaitingFor::CopyRetarget { copy_id, .. } = runner.state().waiting_for else {
        panic!(
            "expected Huddle Up copy, got {:?}",
            runner.state().waiting_for
        );
    };
    act(&mut runner, GameAction::KeepAllCopyTargets);
    assert!(runner.state().stack.iter().any(|entry| entry.id == copy_id));
    finish(&mut runner);
    for player in [P0, P1] {
        assert_eq!(runner.state().players[player.0 as usize].library.len(), 1);
    }
}

#[test]
fn exterminate_revalidates_a_stale_singleton_choice() {
    let mut board = dalek_board(true, None);
    let runner = &mut board.runner;
    cast(runner, board.spell, CastPaymentMode::Auto);
    offer(runner, AdditionalCostOrigin::Replicate, 0);
    decide(runner, true);
    assert!(tap_choices(runner, 1).contains(&board.mana_dalek));
    let pending = runner
        .state()
        .waiting_for
        .pending_cast_ref()
        .unwrap()
        .clone();
    // Simulate the advertised choice going stale without changing the prompt.
    runner
        .state_mut()
        .objects
        .get_mut(&board.mana_dalek)
        .unwrap()
        .tapped = true;
    reject_tap(runner, board.mana_dalek);
    assert_eq!(
        runner.state().waiting_for.pending_cast_ref(),
        Some(&pending)
    );
    act(runner, select(&[board.sick_dalek]));
    target_prompt(runner);
    act(runner, target(board.original_target));
    assert_eq!(records(runner, board.spell), [(0, 1)]);
}

#[test]
fn exterminate_cannot_reserve_its_only_black_mana_source() {
    for mode in [CastPaymentMode::Auto, CastPaymentMode::Manual] {
        let DalekBoard {
            mut runner,
            spell,
            mana_dalek,
            sick_dalek,
            original_target,
            ..
        } = dalek_board(false, None);
        cast(&mut runner, spell, mode);
        offer(&runner, AdditionalCostOrigin::Replicate, 0);
        decide(&mut runner, true);
        assert_eq!(tap_choices(&runner, 1), [sick_dalek]);
        reject_tap(&mut runner, mana_dalek);
        assert!(!runner.state().objects[&mana_dalek].tapped);
        act(&mut runner, select(&[sick_dalek]));
        target_prompt(&runner);
        act(&mut runner, target(original_target));
        if mode == CastPaymentMode::Manual {
            let selection = activatable_mana_source_selections(runner.state(), P0)
                .into_iter()
                .find(|selection| selection.source.object_id == mana_dalek)
                .expect("unreserved Dalek funds the base cost");
            act(
                &mut runner,
                GameAction::ActivateAbility {
                    source_id: mana_dalek,
                    ability_index: selection.ability_index.expect("Dalek's mana ability"),
                    payment_mode: CastPaymentMode::Auto,
                },
            );
            act(&mut runner, GameAction::PassPriority);
        }
        assert_eq!(records(&runner, spell), [(0, 1)]);
        assert!(runner.state().objects[&mana_dalek].tapped);
        assert!(runner.state().objects[&sick_dalek].tapped);
    }
}

#[test]
fn exterminate_can_cast_without_a_dalek_to_spare() {
    let DalekBoard {
        mut runner,
        spell,
        mana_dalek,
        sick_dalek,
        original_target,
        ..
    } = dalek_board(false, None);
    runner
        .state_mut()
        .objects
        .get_mut(&sick_dalek)
        .unwrap()
        .tapped = true;
    cast(&mut runner, spell, CastPaymentMode::Auto);
    target_prompt(&runner);
    act(&mut runner, target(original_target));
    assert!(records(&runner, spell).is_empty());
    assert!(runner.state().objects[&mana_dalek].tapped);
    assert_eq!(runner.state().stack.len(), 1);
    finish(&mut runner);
    assert_eq!(
        runner.state().objects[&original_target].zone,
        Zone::Graveyard
    );
    assert_eq!(runner.state().players[1].life, 17);
}

#[test]
fn exterminate_can_convoke_with_an_unreserved_helper() {
    for spare in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Exterminate!", false, EXTERMINATE)
            .with_mana_cost(parse_mtgjson_mana_cost("{2}{B}"))
            .id();
        let caetus = scenario
            .add_creature_from_oracle(P0, "Caetus, Sea Tyrant of Segovia", 3, 3, CAETUS)
            .id();
        let dalek = scenario
            .add_creature(P0, "Dalek", 2, 2)
            .with_subtypes(vec!["Dalek"])
            .id();
        let helper = spare.then(|| scenario.add_creature(P0, "Extra helper", 1, 1).id());
        let victim = scenario.add_creature(P1, "Victim", 2, 2).id();
        pool(&mut scenario, ManaType::Black, 2);
        let mut runner = scenario.build();
        runner.state_mut().objects.get_mut(&caetus).unwrap().tapped = true;
        cast(&mut runner, spell, CastPaymentMode::Manual);
        if spare {
            offer(&runner, AdditionalCostOrigin::Replicate, 0);
            decide(&mut runner, true);
            assert_eq!(tap_choices(&runner, 1), [dalek]);
            act(&mut runner, select(&[dalek]));
        }
        target_prompt(&runner);
        act(&mut runner, target(victim));
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ManaPayment {
                player: P0,
                convoke_mode: Some(ConvokeMode::Convoke)
            }
        ));
        let convoke = |object_id| GameAction::TapForConvoke {
            object_id,
            mana_type: ManaType::Colorless,
        };
        if spare {
            assert!(!runner.state().objects[&dalek].tapped);
            assert!(!engine::ai_support::legal_actions(runner.state()).contains(&convoke(dalek)));
            assert!(runner.act(convoke(dalek)).is_err());
        }
        act(&mut runner, convoke(helper.unwrap_or(dalek)));
        act(&mut runner, GameAction::PassPriority);
        assert!(runner.state().objects[&dalek].tapped);
        assert_eq!(
            records(&runner, spell),
            if spare { vec![(0, 1)] } else { vec![] }
        );
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    }
}

#[test]
fn exterminate_cannot_sacrifice_its_reserved_dalek_for_cast_mana() {
    for spare in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Exterminate!", false, EXTERMINATE)
            .with_mana_cost(parse_mtgjson_mana_cost("{2}{B}"))
            .id();
        let altar = scenario
            .add_artifact_from_oracle(P0, "Ashnod's Altar", ASHNODS_ALTAR)
            .id();
        let dalek = scenario
            .add_creature(P0, "Dalek", 2, 2)
            .with_subtypes(vec!["Dalek"])
            .id();
        let helper = spare.then(|| scenario.add_creature(P0, "Spare creature", 1, 1).id());
        let victim = scenario.add_creature(P1, "Victim", 2, 2).id();
        pool(&mut scenario, ManaType::Black, 1);
        let mut runner = scenario.build();
        cast(&mut runner, spell, CastPaymentMode::Manual);
        if spare {
            offer(&runner, AdditionalCostOrigin::Replicate, 0);
            decide(&mut runner, true);
            assert_eq!(tap_choices(&runner, 1), [dalek]);
            act(&mut runner, select(&[dalek]));
        }
        target_prompt(&runner);
        act(&mut runner, target(victim));
        act(
            &mut runner,
            GameAction::ActivateAbility {
                source_id: altar,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            },
        );
        let sacrifice = helper.unwrap_or(dalek);
        assert!(matches!(&runner.state().waiting_for,
            WaitingFor::PayCost { player: P0, choices, count: 1, .. } if choices == &[sacrifice]
        ));
        if spare {
            assert!(!runner.state().objects[&dalek].tapped);
            assert!(runner.act(select(&[dalek])).is_err());
        }
        act(&mut runner, select(&[sacrifice]));
        act(&mut runner, GameAction::PassPriority);
        assert_eq!(runner.state().objects[&sacrifice].zone, Zone::Graveyard);
        assert_eq!(
            records(&runner, spell),
            if spare { vec![(0, 1)] } else { vec![] }
        );
        if spare {
            assert_eq!(runner.state().objects[&dalek].zone, Zone::Battlefield);
            assert!(runner.state().objects[&dalek].tapped);
        }
    }
}

#[test]
fn exterminate_conspire_uses_only_unreserved_helpers() {
    for helper_count in [1, 2] {
        let DalekBoard {
            mut runner,
            spell,
            mana_dalek,
            sick_dalek,
            original_target,
            helpers,
            ..
        } = dalek_board(true, Some(helper_count));
        let wort = runner
            .state()
            .objects
            .values()
            .find(|object| object.name == "Wort, the Raidmother")
            .expect("Wort grants Conspire");
        assert_eq!(wort.base_color, [ManaColor::Red, ManaColor::Green]);
        assert_eq!(
            runner.state().objects[&spell].base_color,
            [ManaColor::Black]
        );
        assert!(wort.tapped, "only the designated helpers can pay Conspire");
        cast(&mut runner, spell, CastPaymentMode::Auto);
        offer(&runner, AdditionalCostOrigin::Replicate, 0);
        decide(&mut runner, true);
        act(&mut runner, select(&[mana_dalek]));
        offer(&runner, AdditionalCostOrigin::Replicate, 1);
        decide(&mut runner, true);
        act(&mut runner, select(&[sick_dalek]));
        offer(&runner, AdditionalCostOrigin::Replicate, 2);
        decide(&mut runner, false);
        if helper_count == 2 {
            offer(&runner, AdditionalCostOrigin::Conspire, 0);
            decide(&mut runner, true);
            let choices = tap_choices(&runner, 2);
            assert_eq!(choices.len(), 2);
            assert!(helpers.iter().all(|id| choices.contains(id)));
            assert!(runner.act(select(&[helpers[0], mana_dalek])).is_err());
            act(&mut runner, select(&helpers));
        } else {
            target_prompt(&runner);
            assert!(runner
                .act(GameAction::DecideOptionalCost { index: 0 })
                .is_err());
        }
        target_prompt(&runner);
        act(&mut runner, target(original_target));
        if helper_count == 2 {
            assert!(matches!(
                &runner.state().waiting_for,
                WaitingFor::OrderTriggers { player: P0, triggers } if triggers.len() == 2
            ));
            act(&mut runner, GameAction::OrderTriggers { order: vec![1, 0] });
        } else {
            assert!(
                matches!(
                    runner.state().waiting_for,
                    WaitingFor::Priority { player: P0 }
                ),
                "no Conspire trigger when its separate cost was not paid"
            );
        }
        assert_eq!(records(&runner, spell), [(0, 1), (0, 1)]);
        assert_eq!(
            runner.state().stack.len(),
            if helper_count == 2 { 3 } else { 2 }
        );
        assert!(helpers
            .iter()
            .all(|id| runner.state().objects[id].tapped == (helper_count == 2)));
    }
}

fn dalek_payment_before_second_selection(board: &mut DalekBoard, mode: CastPaymentMode) {
    cast(&mut board.runner, board.spell, mode);
    offer(&board.runner, AdditionalCostOrigin::Replicate, 0);
    decide(&mut board.runner, true);
    act(&mut board.runner, select(&[board.mana_dalek]));
    offer(&board.runner, AdditionalCostOrigin::Replicate, 1);
    decide(&mut board.runner, true);
    assert_eq!(tap_choices(&board.runner, 1), [board.sick_dalek]);
}

#[test]
fn exterminate_restore_preserves_the_reservation_and_cancel_releases_it() {
    for restore in [false, true] {
        let mut board = dalek_board(true, None);
        let pre_pool = board.runner.state().players[0].mana_pool.total();
        dalek_payment_before_second_selection(&mut board, CastPaymentMode::Manual);
        if restore {
            board.runner = round_trip(&board.runner);
        }
        assert_eq!(tap_choices(&board.runner, 1), [board.sick_dalek]);
        reject_tap(&mut board.runner, board.mana_dalek);
        assert!(apply(board.runner.state_mut(), P1, select(&[board.sick_dalek])).is_err());
        assert!(apply(board.runner.state_mut(), P1, GameAction::CancelCast).is_err());
        act(&mut board.runner, GameAction::CancelCast);
        assert_eq!(board.runner.state().objects[&board.spell].zone, Zone::Hand);
        assert!(board.runner.state().stack.is_empty());
        assert!(records(&board.runner, board.spell).is_empty());
        assert_eq!(board.runner.state().players[0].mana_pool.total(), pre_pool);
        assert_eq!(board.runner.state().players[0].energy, 0);
        assert!(!board.runner.state().objects[&board.mana_dalek].tapped);
        assert!(!board.runner.state().objects[&board.sick_dalek].tapped);

        cast(&mut board.runner, board.spell, CastPaymentMode::Auto);
        decide(&mut board.runner, true);
        assert!(tap_choices(&board.runner, 1).contains(&board.mana_dalek));
        act(&mut board.runner, select(&[board.mana_dalek]));
        decide(&mut board.runner, false);
        target_prompt(&board.runner);
        act(&mut board.runner, target(board.original_target));
        assert_eq!(records(&board.runner, board.spell), [(0, 1)]);
        assert_eq!(board.runner.state().stack.len(), 2);
        assert!(board.runner.state().objects[&board.mana_dalek].tapped);
        assert!(!board.runner.state().objects[&board.sick_dalek].tapped);
    }
}

#[test]
fn exterminate_restored_second_selection_completes_like_an_uninterrupted_cast() {
    for restore in [false, true] {
        let mut board = dalek_board(true, None);
        dalek_payment_before_second_selection(&mut board, CastPaymentMode::Auto);
        if restore {
            board.runner = round_trip(&board.runner);
        }
        assert_eq!(tap_choices(&board.runner, 1), [board.sick_dalek]);
        reject_tap(&mut board.runner, board.mana_dalek);
        act(&mut board.runner, select(&[board.sick_dalek]));
        target_prompt(&board.runner);
        act(&mut board.runner, target(board.original_target));
        assert_eq!(records(&board.runner, board.spell), [(0, 1), (0, 1)]);
        assert!(board.runner.state().objects[&board.mana_dalek].tapped);
        assert!(board.runner.state().objects[&board.sick_dalek].tapped);
        assert_eq!(board.runner.state().players[0].mana_pool.total(), 0);
        assert_spell_target(&board.runner, board.spell, board.original_target);
        assert_eq!(board.runner.state().stack.len(), 2);
    }
}

#[test]
fn exterminate_restore_rejects_a_missing_previously_selected_dalek() {
    let mut board = dalek_board(true, None);
    dalek_payment_before_second_selection(&mut board, CastPaymentMode::Manual);
    let mut valid = round_trip(&board.runner);
    assert_eq!(tap_choices(&valid, 1), [board.sick_dalek]);
    act(&mut valid, select(&[board.sick_dalek]));
    target_prompt(&valid);

    let mut forged = board.runner.state().clone();
    let pending = forged
        .waiting_for
        .pending_cast_mut()
        .expect("second fixed selection");
    let missing = pending
        .deferred_tapped_permanents
        .pop()
        .expect("prior reservation");
    assert_eq!(missing.object_ids, [board.mana_dalek]);
    let json = serde_json::to_string(&PersistedGameState::capture(forged)).unwrap();
    let decoded =
        serde_json::from_str::<PersistedGameState>(&json).expect("wire shape still decodes");
    assert!(decoded
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .and_then(|prepared| prepared.finalize_after_rehydration(|_| Ok(())))
        .is_err());
}

fn bolt_scenario(oracle: &str, mana_cost: &str) -> (GameScenario, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let name = if oracle == REITERATING_BOLT && mana_cost == "{1}{R}" {
        "Reiterating Bolt"
    } else {
        "Synthetic energy Replicate spell"
    };
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, name, false, oracle)
        .with_mana_cost(parse_mtgjson_mana_cost(mana_cost))
        .id();
    let creature = scenario.add_creature(P1, "Ancient Brontodon", 9, 9).id();
    let walker = scenario
        .add_planeswalker_from_oracle(P1, "Jace", "Jace", 6, "")
        .id();
    pool(&mut scenario, ManaType::Red, 2);
    (scenario, spell, creature, walker)
}

fn bolt_board(energy: u32) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let (scenario, spell, creature, walker) = bolt_scenario(REITERATING_BOLT, "{1}{R}");
    let mut runner = scenario.build();
    runner.state_mut().players[0].energy = energy;
    (runner, spell, creature, walker)
}

// CR 107.14 and CR 601.2h: choosing to pay reserves energy, and settlement
// spends the actual counters exactly once.
#[test]
fn reiterating_bolt_energy_budget_and_copy_count_survive_restore() {
    for (energy, repeats, restore) in [
        (5, 1, false),
        (6, 2, false),
        (2, 0, false),
        (5, 1, true),
        (6, 2, true),
    ] {
        let (mut runner, spell, creature, walker) = bolt_board(energy);
        cast(&mut runner, spell, CastPaymentMode::Auto);
        for paid in 0..repeats {
            offer(&runner, AdditionalCostOrigin::Replicate, paid);
            decide(&mut runner, true);
            assert_eq!(runner.state().players[0].energy, energy);
            if restore && paid == 0 {
                runner = round_trip(&runner);
            }
        }
        target_prompt(&runner);
        assert!(!engine::ai_support::legal_actions(runner.state())
            .iter()
            .any(|action| matches!(action, GameAction::DecideOptionalCost { index: 0 })));
        assert_eq!(runner.state().players[0].energy, energy);
        let events = act(&mut runner, target(creature));
        assert!(events.iter().any(|event| matches!(event,
            GameEvent::SpellCast { object_id, cast_mana_spent: Some(2), .. } if *object_id == spell)));
        assert_eq!(records(&runner, spell), vec![(0, 1); repeats as usize]);
        assert_eq!(runner.state().players[0].energy, energy - repeats * 3);
        assert_eq!(runner.state().stack.len(), if repeats == 0 { 1 } else { 2 });
        for generated in 0..repeats {
            let mut events = pass_to_prompt(&mut runner);
            let copy = copy_prompt(&runner, creature);
            assert_ne!(copy, spell);
            let recipient = if generated == 0 { walker } else { creature };
            if generated == 0 {
                events.extend(act(&mut runner, target(walker)));
            } else {
                events.extend(act(&mut runner, GameAction::KeepAllCopyTargets));
            }
            assert!(!events
                .iter()
                .any(|event| matches!(event, GameEvent::SpellCast { .. })));
            assert_spell_target(&runner, copy, recipient);
        }
        finish(&mut runner);
        assert_eq!(
            runner.state().objects[&creature].damage_marked,
            if repeats == 2 { 6 } else { 3 }
        );
        let loyalty = &runner.state().objects[&walker].counters;
        assert_eq!(
            loyalty[&engine::types::counter::CounterType::Loyalty],
            if repeats > 0 { 3 } else { 6 }
        );
        assert_eq!(runner.state().players[0].energy, energy - repeats * 3);
    }
}

#[test]
fn reiterating_bolt_rejects_acceptance_when_an_outstanding_offer_loses_its_budget() {
    let (mut runner, spell, creature, _) = bolt_board(6);
    cast(&mut runner, spell, CastPaymentMode::Auto);
    offer(&runner, AdditionalCostOrigin::Replicate, 0);
    decide(&mut runner, true);
    offer(&runner, AdditionalCostOrigin::Replicate, 1);
    let pending = runner
        .state()
        .waiting_for
        .pending_cast_ref()
        .unwrap()
        .clone();
    assert_eq!(pending.unpaid_replicate_energy, 3);
    assert_eq!(pending.ability.context.additional_cost_payments.len(), 1);

    // Simulate the live budget changing after the second offer was issued.
    runner.state_mut().players[0].energy = 5;
    assert!(matches!(
        runner.act(GameAction::DecideOptionalCost { index: 0 }),
        Err(EngineError::ActionNotAllowed(message)) if message == "This replicate cost cannot be paid"
    ));
    offer(&runner, AdditionalCostOrigin::Replicate, 1);
    assert_eq!(
        runner.state().waiting_for.pending_cast_ref(),
        Some(&pending)
    );
    assert_eq!(runner.state().players[0].energy, 5);

    decide(&mut runner, false);
    target_prompt(&runner);
    act(&mut runner, target(creature));
    assert_eq!(records(&runner, spell), [(0, 1)]);
    assert_eq!(runner.state().players[0].energy, 2);
}

// CR 601.2b + CR 107.14 + CR 118.3: the optional election must leave enough
// energy for the mandatory part of the same spell's total cost.
// CR 601.2h + CR 107.4f + CR 616.1: paying the Phyrexian shard pauses
// the commit while the Replicate energy obligation is still outstanding.
#[test]
fn reiterating_bolt_settles_replicate_on_uninterrupted_phyrexian_payment() {
    // Synthetic exception: no printed energy Replicate spell has a Phyrexian
    // shard. Reiterating Bolt and Dismember supply the nearest printed shapes.
    // Combining them exercises settlement after interactive life payment.
    let (scenario, spell, creature, _) = bolt_scenario(REITERATING_BOLT, "{1}{R}{B/P}");
    let mut runner = scenario.build();
    start_bolt_phyrexian_payment(&mut runner, spell, creature);
    let events = act(
        &mut runner,
        GameAction::SubmitPhyrexianChoices {
            choices: vec![ShardChoice::PayLife],
        },
    );
    assert_eq!(runner.state().players[0].life, 18);
    assert_bolt_replicate_settled(&mut runner, spell, creature, &events);
}

#[test]
fn reiterating_bolt_settles_replicate_at_deferred_sacrifice_commit() {
    // Synthetic exception: no printed energy Replicate spell requires a
    // sacrifice. Reiterating Bolt and Village Rites supply the nearest shapes.
    // Combining them exercises settlement before a deferred sacrifice resumes.
    let oracle = format!(
        "As an additional cost to cast this spell, sacrifice a creature.\n{REITERATING_BOLT}"
    );
    let (mut scenario, spell, creature, _) = bolt_scenario(&oracle, "{1}{R}");
    let memnite = scenario
        .add_creature(P0, "Memnite", 1, 1)
        .as_artifact_creature()
        .id();
    let mut runner = scenario.build();
    runner.state_mut().players[0].energy = 5;
    cast(&mut runner, spell, CastPaymentMode::Manual);
    offer(&runner, AdditionalCostOrigin::Replicate, 0);
    decide(&mut runner, true);
    target_prompt(&runner);
    act(&mut runner, target(creature));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost {
            kind: PayCostKind::Sacrifice,
            ..
        }
    ));
    act(&mut runner, select(&[memnite]));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    assert!(runner.state().battlefield.contains(&memnite));
    assert_eq!(runner.state().players[0].energy, 5);
    let events = act(&mut runner, GameAction::PassPriority);
    assert!(events
        .iter()
        .any(|event| matches!(event, GameEvent::PermanentSacrificed { object_id, player_id: P0 } if *object_id == memnite)));
    assert_bolt_replicate_settled(&mut runner, spell, creature, &events);
}

#[test]
fn reiterating_bolt_settles_replicate_once_after_a_replacement_pause() {
    // Synthetic exception: no printed energy Replicate spell has a Phyrexian
    // shard. Reiterating Bolt and Dismember supply the nearest printed shapes.
    // Combining them exercises settlement after interactive life payment.
    let (mut scenario, spell, creature, _) = bolt_scenario(REITERATING_BOLT, "{1}{R}{B/P}");
    // Synthetic exception: no printed life-loss replacement adds one to the
    // same event in this shape. Competing replacements exercise resumable
    // settlement. Bloodletter of Aclazotz is the analogue for doubling life loss.
    // Forsaken Wastes adds a separate one-life loss. The synthetic +1 modifier
    // folds that extra loss into the same event so the two replacements can compete.
    scenario
        .add_creature(P0, "Life-loss replacements", 0, 1)
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::LoseLife)
                .quantity_modification(QuantityModification::DOUBLE)
                .description("Double".to_string()),
        )
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::LoseLife)
                .quantity_modification(QuantityModification::Plus { value: 1 })
                .description("Plus one".to_string()),
        );
    let mut runner = scenario.build();
    start_bolt_phyrexian_payment(&mut runner, spell, creature);
    let mut events = act(
        &mut runner,
        GameAction::SubmitPhyrexianChoices {
            choices: vec![ShardChoice::PayLife],
        },
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    let Some(DeferredLifeCostResume::Cast {
        pending: Some(pending),
        ..
    }) = &runner.state().pending_deferred_life_cost_resume
    else {
        panic!("expected the cast to be parked on the replacement");
    };
    assert_eq!(pending.unpaid_replicate_energy, 3);
    assert_eq!(runner.state().players[0].energy, 5);
    events.extend(act(&mut runner, GameAction::ChooseReplacement { index: 0 }));
    assert_bolt_replicate_settled(&mut runner, spell, creature, &events);
}

fn start_bolt_phyrexian_payment(runner: &mut GameRunner, spell: ObjectId, creature: ObjectId) {
    runner.state_mut().players[0].energy = 5;
    cast(runner, spell, CastPaymentMode::Manual);
    offer(runner, AdditionalCostOrigin::Replicate, 0);
    decide(runner, true);
    target_prompt(runner);
    act(runner, target(creature));
    act(runner, GameAction::PassPriority);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PhyrexianPayment { .. }
    ));
}

fn assert_bolt_replicate_settled(
    runner: &mut GameRunner,
    spell: ObjectId,
    creature: ObjectId,
    events: &[GameEvent],
) {
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().players[0].energy, 2);
    assert_eq!(runner.state().players[0].energy_paid_or_lost_this_turn, 3);
    assert_eq!(records(runner, spell), [(0, 1)]);
    assert_eq!(runner.state().stack.len(), 2);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::EnergyChanged {
                    player: P0,
                    delta: -3
                }
            ))
            .count(),
        1
    );
    pass_to_prompt(runner);
    let copy = copy_prompt(runner, creature);
    assert_ne!(copy, spell);
    act(runner, GameAction::KeepAllCopyTargets);
    assert_eq!(runner.state().stack.len(), 2);
    assert_spell_target(runner, copy, creature);
    finish(runner);
    assert_eq!(runner.state().objects[&creature].damage_marked, 6);
}

#[test]
fn reiterating_bolt_leaves_energy_for_the_mana_ability_that_funds_it() {
    for (energy, pay, cancel) in [(3, false, false), (4, true, false), (4, true, true)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Reiterating Bolt", false, REITERATING_BOLT)
            .with_mana_cost(parse_mtgjson_mana_cost("{1}{R}"))
            .id();
        let creature = scenario.add_creature(P1, "Resilient creature", 1, 10).id();
        scenario.add_planeswalker_from_oracle(P1, "Jace", "Jace", 6, "");
        let hub = scenario
            .add_land_from_oracle(P0, "Aether Hub", AETHER_HUB)
            .id();
        pool(&mut scenario, ManaType::Colorless, 1);
        let mut runner = scenario.build();
        runner.state_mut().players[0].energy = energy;
        let mana_ability = runner.state().objects[&hub]
            .abilities
            .iter()
            .position(|ability| {
                let Some(AbilityCost::Composite { costs }) = &ability.cost else {
                    return false;
                };
                costs
                    .iter()
                    .any(|cost| matches!(cost, AbilityCost::PayEnergy { .. }))
            })
            .expect("only the energy ability can produce red");
        cast(&mut runner, spell, CastPaymentMode::Manual);
        if pay {
            offer(&runner, AdditionalCostOrigin::Replicate, 0);
            decide(&mut runner, true);
        }
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::TargetSelection { player: P0, .. }
            ),
            "with {energy} energy and replication election {pay}, got {}",
            runner.waiting_for_kind()
        );
        assert_eq!(runner.state().players[0].energy, energy);
        act(&mut runner, target(creature));
        act(
            &mut runner,
            GameAction::ActivateAbility {
                source_id: hub,
                ability_index: mana_ability,
                payment_mode: CastPaymentMode::Auto,
            },
        );
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseManaColor { player: P0, .. }
        ));
        act(
            &mut runner,
            GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Red),
                count: 1,
            },
        );
        assert_eq!(runner.state().players[0].energy, energy - 1);
        assert!(runner.state().objects[&hub].tapped);
        if cancel {
            // CR 733.1: cancel the spell and keep the completed mana ability.
            act(&mut runner, GameAction::CancelCast);
            assert_eq!(runner.state().objects[&spell].zone, Zone::Hand);
            assert!(runner.state().objects[&hub].tapped);
            assert_eq!(runner.state().players[0].energy, energy - 1);
            assert_eq!(runner.state().players[0].energy_paid_or_lost_this_turn, 1);
            assert_eq!(runner.state().players[0].mana_pool.total(), 2);
            assert!(runner.state().stack.is_empty());
            assert!(runner.state().pending_cast.is_none());
            cast(&mut runner, spell, CastPaymentMode::Auto);
            offer(&runner, AdditionalCostOrigin::Replicate, 0);
            decide(&mut runner, true);
            target_prompt(&runner);
            act(&mut runner, target(creature));
            assert_eq!(records(&runner, spell), [(0, 1)]);
            assert_eq!(runner.state().players[0].energy, 0);
            continue;
        }
        act(&mut runner, GameAction::PassPriority);
        assert_eq!(runner.state().players[0].energy, if pay { 0 } else { 2 });
        assert_eq!(
            records(&runner, spell),
            if pay { vec![(0, 1)] } else { vec![] }
        );
        assert_eq!(runner.state().stack.len(), if pay { 2 } else { 1 });
    }
}

fn answer_ritual_copy_offer(
    runner: &mut GameRunner,
    card: ObjectId,
    accept: bool,
) -> Vec<GameEvent> {
    assert_eq!(runner.state().objects[&card].zone, Zone::Exile);
    let action = match &runner.state().waiting_for {
        WaitingFor::ChooseFromZoneChoice {
            player: P0,
            cards,
            up_to: true,
            ..
        } => {
            assert_eq!(cards, &[card]);
            select(if accept {
                std::slice::from_ref(&card)
            } else {
                &[]
            })
        }
        WaitingFor::OptionalEffectChoice { player: P0, .. } => {
            GameAction::DecideOptionalEffect { accept }
        }
        other => panic!("expected a cast-copy offer for the exiled spell, got {other:?}"),
    };
    act(runner, action)
}

#[test]
fn psionic_ritual_retargets_to_a_second_graveyard_and_casts_only_one_copy() {
    for pay in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let ritual = scenario
            .add_spell_to_hand_from_oracle(P0, "Psionic Ritual", false, PSIONIC_RITUAL)
            .with_mana_cost(parse_mtgjson_mana_cost("{4}{U}{U}"))
            .id();
        let horror = scenario
            .add_creature(P0, "Friendly Horror", 2, 2)
            .with_subtypes(vec!["Horror"])
            .id();
        let rival = scenario
            .add_creature(P1, "Opposing Horror", 2, 2)
            .with_subtypes(vec!["Horror"])
            .id();
        let bear = scenario
            .add_creature(P0, "Bear", 2, 2)
            .with_subtypes(vec!["Bear"])
            .id();
        let corpse = scenario
            .add_creature_to_graveyard(P1, "Walking Corpse", 2, 2)
            .id();
        let lava_spike = scenario
            .add_spell_to_graveyard(P1, "Lava Spike", false)
            .with_mana_cost(parse_mtgjson_mana_cost("{R}"))
            .from_oracle_text("Lava Spike deals 3 damage to target player or planeswalker.")
            .id();
        let dark_ritual = scenario
            .add_spell_to_graveyard(P0, "Dark Ritual", true)
            .with_mana_cost(parse_mtgjson_mana_cost("{B}"))
            .from_oracle_text("Add {B}{B}{B}.")
            .id();
        pool(&mut scenario, ManaType::Blue, 6);
        let mut runner = scenario.build();
        cast(&mut runner, ritual, CastPaymentMode::Auto);
        if pay {
            offer(&runner, AdditionalCostOrigin::Replicate, 0);
            decide(&mut runner, true);
            assert_eq!(tap_choices(&runner, 1), [horror]);
            reject_tap(&mut runner, rival);
            reject_tap(&mut runner, bear);
            act(&mut runner, select(&[horror]));
        } else if matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalCostChoice { .. }
        ) {
            offer(&runner, AdditionalCostOrigin::Replicate, 0);
            decide(&mut runner, false);
        }
        target_prompt(&runner);
        assert!(runner.act(target(corpse)).is_err());
        act(&mut runner, target(lava_spike));
        assert_eq!(
            records(&runner, ritual),
            if pay { vec![(0, 1)] } else { vec![] }
        );
        assert_eq!(runner.state().objects[&horror].tapped, pay);
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_spell_target(&runner, ritual, lava_spike);
        if pay {
            pass_to_prompt(&mut runner);
            let copy = copy_prompt(&runner, lava_spike);
            act(&mut runner, target(dark_ritual));
            assert_spell_target(&runner, copy, dark_ritual);
            pass_to_prompt(&mut runner);
            let events = answer_ritual_copy_offer(&mut runner, dark_ritual, true);
            let casts: Vec<_> = events
                .iter()
                .filter_map(|event| {
                    let GameEvent::SpellCast {
                        object_id,
                        cast_mana_spent,
                        ..
                    } = event
                    else {
                        return None;
                    };
                    Some((*object_id, *cast_mana_spent))
                })
                .collect();
            assert_eq!(casts.len(), 1);
            assert_ne!(casts[0].0, dark_ritual);
            assert_eq!(runner.state().objects[&casts[0].0].name, "Dark Ritual");
            assert_eq!(casts[0].1, Some(0));
        }
        pass_to_prompt(&mut runner);
        let events = answer_ritual_copy_offer(&mut runner, lava_spike, false);
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::SpellCast { .. })));
        finish(&mut runner);
        assert_eq!(runner.state().objects[&ritual].zone, Zone::Exile);
        assert_eq!(runner.state().objects[&lava_spike].zone, Zone::Exile);
        assert_eq!(
            runner.state().objects[&dark_ritual].zone,
            if pay { Zone::Exile } else { Zone::Graveyard }
        );
        assert_eq!(runner.state().objects[&corpse].zone, Zone::Graveyard);
        assert_eq!(
            runner.state().players[0].mana_pool.total(),
            if pay { 3 } else { 0 }
        );
    }
}

#[test]
fn fixed_replicate_grants_keep_their_price_and_copy_the_recipient() {
    for (grant_name, oracle, price) in [
        ("Hugh, First of Many", HUGH, "{2}{U}"),
        ("Threefold Signal", THREEFOLD_SIGNAL, "{3}"),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        if grant_name == "Threefold Signal" {
            scenario.add_artifact_from_oracle(P0, grant_name, oracle);
        } else {
            scenario.add_creature_from_oracle(P0, grant_name, 3, 4, oracle);
        }
        let recipient = scenario
            .add_creature_to_hand_from_oracle(P0, "Tower Gargoyle", 4, 4, "Flying")
            .as_artifact_creature()
            .with_mana_cost(parse_mtgjson_mana_cost("{1}{W}{U}{B}"))
            .with_color(vec![ManaColor::White, ManaColor::Blue, ManaColor::Black])
            .id();
        scenario.with_mana_pool(
            P0,
            [
                ManaType::White,
                ManaType::Blue,
                ManaType::Black,
                ManaType::Blue,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
            ]
            .into_iter()
            .map(|mana| ManaUnit::new(mana, ObjectId(0), false, vec![]))
            .collect(),
        );
        let mut runner = scenario.build();
        cast(&mut runner, recipient, CastPaymentMode::Auto);
        offer(&runner, AdditionalCostOrigin::Replicate, 0);
        assert!(matches!(
            &runner.state().waiting_for,
            WaitingFor::OptionalCostChoice {
                cost: engine::types::ability::AdditionalCost::Optional {
                    cost: engine::types::ability::AbilityCost::Mana { cost }, ..
                },
                ..
            } if *cost == parse_mtgjson_mana_cost(price)
        ));
        decide(&mut runner, true);
        if matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalCostChoice { .. }
        ) {
            decide(&mut runner, false);
        }
        assert_eq!(records(&runner, recipient), [(0, 1)]);
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_eq!(runner.state().stack.len(), 2);
        finish(&mut runner);
        assert_eq!(
            runner
                .state()
                .battlefield
                .iter()
                .filter(|id| {
                    let object = &runner.state().objects[id];
                    object.name == "Tower Gargoyle" && object.is_token
                })
                .count(),
            1
        );
        assert_eq!(runner.state().objects[&recipient].zone, Zone::Battlefield);
    }
}

#[test]
fn reiterating_bolt_cancel_from_target_or_manual_mana_restores_and_can_replay() {
    for restore in [false, true] {
        let (mut runner, spell, creature, _) = bolt_board(5);
        cast(&mut runner, spell, CastPaymentMode::Manual);
        decide(&mut runner, true);
        target_prompt(&runner);
        if restore {
            runner = round_trip(&runner);
        }
        assert!(apply(runner.state_mut(), P1, target(creature)).is_err());
        assert!(runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(P1))
            })
            .is_err());
        target_prompt(&runner);
        if restore {
            act(&mut runner, target(creature));
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::ManaPayment { player: P0, .. }
            ));
        }
        act(&mut runner, GameAction::CancelCast);
        assert_eq!(runner.state().objects[&spell].zone, Zone::Hand);
        assert!(runner.state().stack.is_empty());
        assert!(records(&runner, spell).is_empty());
        assert_eq!(runner.state().players[0].mana_pool.total(), 2);
        assert_eq!(runner.state().players[0].energy, 5);
        cast(&mut runner, spell, CastPaymentMode::Auto);
        offer(&runner, AdditionalCostOrigin::Replicate, 0);
        decide(&mut runner, true);
        target_prompt(&runner);
        act(&mut runner, target(creature));
        assert_eq!(records(&runner, spell), [(0, 1)]);
        assert_eq!(runner.state().players[0].energy, 2);
        assert_eq!(runner.state().stack.len(), 2);
    }
}
