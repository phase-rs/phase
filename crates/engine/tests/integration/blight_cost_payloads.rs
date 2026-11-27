use engine::ai_support::candidate_actions;
use engine::database::CardDatabase;
use engine::game::effects::attach::attach_to;
use engine::game::engine::apply;
use engine::game::layers::evaluate_layers;
use engine::game::printed_cards::{printed_ref_from_face, rehydrate_game_from_card_db};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AbilityCost, AdditionalCost, BlightCount};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, PersistedGameState, PersistedRestoreError, PersistedRestoreFinalization,
    ReplacementChoiceKind, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{parse_mtgjson_mana_cost, ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::proposed_event::ProposedEvent;
use engine::types::zones::Zone;
use std::sync::OnceLock;

const P2: PlayerId = PlayerId(2);
const SOUL: &str = "As an additional cost to cast this spell, blight X. X can't be greater than the greatest toughness among creatures you control. (Put X -1/-1 counters on a creature you control.)\nSoul Immolation deals X damage to each opponent and each creature they control.";
const AUNTIE: &str = "Ward—Blight 2. (To blight 2, a player puts two -1/-1 counters on a creature they control.)\nWhenever one or more -1/-1 counters are put on a creature, draw a card if you control that creature. If you don't control it, its controller loses 1 life.";
const GNARLBARK: &str = "At the beginning of your end step, draw a card and blight 1. (Put a -1/-1 counter on a creature you control.)";
const PYROMANCER: &str = "{T}: This creature deals 1 damage to any target.";
const UNSUMMON: &str = "Return target creature to its owner's hand.";

fn mana(kinds: &[ManaType]) -> Vec<ManaUnit> {
    kinds
        .iter()
        .map(|kind| ManaUnit::new(*kind, ObjectId(0), false, vec![]))
        .collect()
}

fn minus_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Minus1Minus1)
        .copied()
        .unwrap_or(0)
}

fn select(id: ObjectId) -> GameAction {
    GameAction::SelectCards { cards: vec![id] }
}

fn candidate(runner: &GameRunner, action: GameAction) -> GameAction {
    let choice = candidate_actions(runner.state())
        .into_iter()
        .find(|choice| choice.action == action)
        .unwrap_or_else(|| {
            panic!(
                "missing AI choice {action:?} at {:?}",
                runner.state().waiting_for
            )
        });
    assert_eq!(
        choice.metadata.actor,
        runner.state().waiting_for.acting_player()
    );
    choice.action
}

fn announce(
    runner: &mut GameRunner,
    spell: ObjectId,
    payment_mode: CastPaymentMode,
) -> Result<(), engine::game::engine::EngineError> {
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: runner.state().objects[&spell].card_id,
            targets: vec![],
            payment_mode,
        })
        .map(|_| ())
}

fn card_db() -> &'static CardDatabase {
    static DB: OnceLock<CardDatabase> = OnceLock::new();
    DB.get_or_init(|| CardDatabase::from_integration_fixture_cached().unwrap())
}

fn attach_printed_ref(runner: &mut GameRunner, id: ObjectId) {
    let card = runner.state_mut().objects.get_mut(&id).unwrap();
    let face = card_db().get_face_by_name(&card.name).unwrap();
    card.printed_ref = Some(printed_ref_from_face(face).expect("a stable printed face identity"));
}

fn decode_checkpoint(runner: &GameRunner) -> PersistedGameState {
    let save = PersistedGameState::capture(runner.state().clone());
    serde_json::from_slice(&serde_json::to_vec(&save).unwrap()).unwrap()
}

fn checkpoint(runner: &GameRunner) -> GameRunner {
    let restored = decode_checkpoint(runner)
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .unwrap()
        .finalize_after_rehydration(|state| {
            rehydrate_game_from_card_db(state, card_db());
            Ok(())
        })
        .unwrap();
    GameRunner::from_state(restored)
}

fn soul(scenario: &mut GameScenario) -> ObjectId {
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        mana(&[
            ManaType::Red,
            ManaType::Red,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]),
    );
    scenario
        .add_spell_to_hand_from_oracle(P0, "Soul Immolation", false, SOUL)
        .with_mana_cost(parse_mtgjson_mana_cost("{3}{R}{R}"))
        .id()
}

fn assert_x(runner: &GameRunner, maximum: u32) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ChooseXValue { player: P0, min: 0, max, .. } if max == maximum),
        "{:?}",
        runner.state().waiting_for
    );
}

fn assert_blight(runner: &GameRunner, amount: u32, eligible: &[ObjectId]) {
    let WaitingFor::BlightChoice {
        player,
        counters,
        creatures,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected cast Blight choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!((*player, *counters), (P0, amount));
    assert_eq!(creatures, eligible);
}

fn printed_text(name: &str) -> &str {
    card_db()
        .get_face_by_name(name)
        .unwrap()
        .oracle_text
        .as_deref()
        .unwrap()
}

fn add_counter_replacements(scenario: &mut GameScenario, payer: PlayerId) -> [ObjectId; 2] {
    let vorinclex = "Vorinclex, Monstrous Raider";
    let constrictor = "Winding Constrictor";
    [
        scenario
            .add_creature(payer, vorinclex, 6, 6)
            .as_legendary()
            .from_oracle_text_with_keywords(&["Trample", "Haste"], printed_text(vorinclex))
            .id(),
        scenario
            .add_creature_from_oracle(payer, constrictor, 2, 3, printed_text(constrictor))
            .id(),
    ]
}

fn add_solemnity(scenario: &mut GameScenario) {
    scenario.add_enchantment_from_oracle(P0, "Solemnity", printed_text("Solemnity"));
}

fn add_control_magic(scenario: &mut GameScenario, payer: PlayerId) -> ObjectId {
    scenario
        .add_enchantment_from_oracle(payer, "Control Magic", printed_text("Control Magic"))
        .with_subtypes(vec!["Aura"])
        .id()
}

fn attach_control_magic(runner: &mut GameRunner, aura: ObjectId, recipient: ObjectId) {
    attach_to(runner.state_mut(), aura, recipient);
    evaluate_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&recipient].controller,
        runner.state().objects[&aura].controller
    );
}

fn counter_order_index(runner: &GameRunner, payer: PlayerId, first: &str) -> usize {
    let WaitingFor::ReplacementChoice {
        player,
        kind,
        candidates,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("expected a replacement-order choice for the Blight placement");
    };
    assert_eq!(
        (*player, *kind, candidates.len()),
        (payer, ReplacementChoiceKind::Order, 2)
    );
    let names: Vec<_> = candidates
        .iter()
        .map(|candidate| runner.state().objects[&candidate.source_id].name.as_str())
        .collect();
    assert!(names.contains(&"Vorinclex, Monstrous Raider"));
    assert!(names.contains(&"Winding Constrictor"));
    names.iter().position(|name| *name == first).unwrap()
}

#[test]
fn soul_immolation_locks_derived_cap_separately_from_recipient() {
    for mode in [CastPaymentMode::Auto, CastPaymentMode::Manual] {
        let mut scenario = GameScenario::new_n_player(3, 123);
        let spell = soul(&mut scenario);
        let response = scenario
            .add_spell_to_hand_from_oracle(P0, "Unsummon", true, UNSUMMON)
            .with_mana_cost(parse_mtgjson_mana_cost("{U}"))
            .id();
        scenario.add_basic_land(P0, ManaColor::Blue);
        let cap = scenario
            .add_creature_from_oracle(
                P0,
                "Melira's Keepers",
                4,
                4,
                printed_text("Melira's Keepers"),
            )
            .id();
        let recipient = scenario.add_vanilla(P0, 1, 1);
        let first = scenario.add_vanilla(P1, 1, 8);
        let second = scenario.add_vanilla(P2, 1, 7);
        scenario.add_enchantment_from_oracle(
            P0,
            "Glorious Anthem",
            "Creatures you control get +1/+1.",
        );
        let mut runner = scenario.build();
        attach_printed_ref(&mut runner, spell);
        announce(&mut runner, spell, mode).unwrap();
        assert_x(&runner, 5);
        let mut resumed = checkpoint(&runner);
        let manual = mode == CastPaymentMode::Manual;
        let mut visited_mana_payment = [false; 2];
        for (path, runner) in [&mut runner, &mut resumed].into_iter().enumerate() {
            assert_x(runner, 5);
            assert!(runner.act(GameAction::ChooseX { value: 6 }).is_err());
            assert_x(runner, 5);
            runner
                .act(candidate(runner, GameAction::ChooseX { value: 5 }))
                .unwrap();
            if matches!(
                runner.state().waiting_for,
                WaitingFor::ManaPayment { player: P0, .. }
            ) {
                assert!(manual);
                runner.act(GameAction::PassPriority).unwrap();
                visited_mana_payment[path] = true;
            }
            assert_blight(runner, 5, &[recipient]);
        }
        resumed = checkpoint(&resumed);
        for (path, runner) in [&mut runner, &mut resumed].into_iter().enumerate() {
            for cards in [
                vec![cap],
                vec![recipient, cap],
                vec![recipient, recipient],
                vec![],
            ] {
                assert!(runner.act(GameAction::SelectCards { cards }).is_err());
                assert_blight(runner, 5, &[recipient]);
            }
            let mut events = runner
                .act(candidate(runner, select(recipient)))
                .unwrap()
                .events;
            if matches!(
                runner.state().waiting_for,
                WaitingFor::ManaPayment { player: P0, .. }
            ) {
                assert!(manual && !visited_mana_payment[path]);
                events.extend(runner.act(GameAction::PassPriority).unwrap().events);
                visited_mana_payment[path] = true;
            }
            assert_eq!(visited_mana_payment[path], manual);
            assert_eq!(events.iter().filter(|event| matches!(event,
                GameEvent::CounterAdded { object_id, counter_type: CounterType::Minus1Minus1, count: 5, actor: P0 } if *object_id == recipient
            )).count(), 1);
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            let entry = runner.state().stack.back().unwrap();
            assert_eq!(entry.source_id, spell);
            assert!(
                matches!(&entry.kind, StackEntryKind::Spell { ability: Some(ability), .. } if ability.chosen_x == Some(5))
            );
            assert!(runner
                .state()
                .objects
                .get(&recipient)
                .is_none_or(|obj| obj.zone != Zone::Battlefield));
            runner.cast(response).target_object(cap).commit();
            runner.pass_holding_team().unwrap();
            runner.pass_holding_team().unwrap();
            runner.pass_holding_team().unwrap();
            assert!(runner
                .state()
                .objects
                .get(&cap)
                .is_none_or(|obj| obj.zone != Zone::Battlefield));
            assert_eq!(runner.state().stack.back().unwrap().source_id, spell);
            runner.advance_until_stack_empty();
            assert!(runner.state().stack.is_empty());
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert_eq!((runner.life(P1), runner.life(P2)), (15, 15));
            assert_eq!(runner.state().objects[&first].damage_marked, 5);
            assert_eq!(runner.state().objects[&second].damage_marked, 5);
        }
    }
}

#[test]
fn soul_immolation_zero_still_requires_a_creature() {
    let mut scenario = GameScenario::new();
    let spell = soul(&mut scenario);
    let mut empty = scenario.build();
    let before = empty.state().players[0].mana_pool.total();
    let _ = announce(&mut empty, spell, CastPaymentMode::Auto);
    assert_eq!(empty.state().players[0].mana_pool.total(), before);
    assert_eq!(empty.state().objects[&spell].zone, Zone::Hand);
    assert!(empty.state().stack.is_empty());
    assert!(
        matches!(empty.state().waiting_for, WaitingFor::Priority { .. }),
        "empty board must reject the cast before announcing X, got {:?}",
        empty.state().waiting_for
    );

    let mut scenario = GameScenario::new();
    let spell = soul(&mut scenario);
    let creature = scenario.add_vanilla(P0, 0, 1);
    let mut runner = scenario.build();
    announce(&mut runner, spell, CastPaymentMode::Auto).unwrap();
    assert_x(&runner, 1);
    runner.act(GameAction::ChooseX { value: 0 }).unwrap();
    assert_blight(&runner, 0, &[creature]);
    let result = runner.act(select(creature)).unwrap();
    assert!(!result
        .events
        .iter()
        .any(|event| matches!(event, GameEvent::CounterAdded { .. })));
    assert_eq!(runner.state().stack.back().unwrap().source_id, spell);
    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P1), 20);
    assert_eq!(minus_counters(&runner, creature), 0);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn soul_immolation_recipient_owner_concession_rolls_back() {
    let mut scenario = GameScenario::new_n_player(3, 206);
    let spell = soul(&mut scenario);
    let recipient = scenario.add_creature(P2, "Borrowed creature", 0, 7).id();
    let aura = add_control_magic(&mut scenario, P0);
    let [vorinclex, constrictor] = add_counter_replacements(&mut scenario, P0);
    let mut runner = scenario.build();
    attach_control_magic(&mut runner, aura, recipient);
    let before = runner.state().players[0].mana_pool.total();
    announce(&mut runner, spell, CastPaymentMode::Auto).unwrap();
    assert_x(&runner, 7);
    runner.act(GameAction::ChooseX { value: 2 }).unwrap();
    assert_blight(&runner, 2, &[recipient, vorinclex, constrictor]);
    runner.act(select(recipient)).unwrap();
    counter_order_index(&runner, P0, "Vorinclex, Monstrous Raider");

    // CR 800.4a + CR 733.1: the owner leaves with the recipient and the
    // surviving caster must reverse the incomplete cast.
    let result = runner.act(GameAction::Concede { player_id: P2 }).unwrap();
    assert!(!result.events.iter().any(|event| matches!(event,
        GameEvent::CounterAdded { object_id, .. } if *object_id == recipient
    )));
    assert!(runner.state().players[2].is_eliminated);
    assert_eq!(runner.state().objects[&recipient].zone, Zone::Exile);
    assert_eq!(runner.state().players[0].mana_pool.total(), before);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Hand);
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().pending_cast.is_none());
    assert!(runner.state().pending_replacement.is_none());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
}

#[test]
fn soul_immolation_caster_concession_retires_parked_blight() {
    let mut scenario = GameScenario::new_n_player(3, 206);
    let spell = soul(&mut scenario);
    let recipient = scenario.add_creature(P1, "Borrowed creature", 0, 7).id();
    let aura = add_control_magic(&mut scenario, P0);
    let [vorinclex, constrictor] = add_counter_replacements(&mut scenario, P0);
    let mut runner = scenario.build();
    attach_control_magic(&mut runner, aura, recipient);
    announce(&mut runner, spell, CastPaymentMode::Auto).unwrap();
    runner.act(GameAction::ChooseX { value: 2 }).unwrap();
    assert_blight(&runner, 2, &[recipient, vorinclex, constrictor]);
    runner.act(select(recipient)).unwrap();
    counter_order_index(&runner, P0, "Vorinclex, Monstrous Raider");

    // CR 800.4a + CR 701.68a: the caster leaves and loses control of the
    // borrowed creature, so the abandoned Blight payment cannot place counters.
    let result = runner.act(GameAction::Concede { player_id: P0 }).unwrap();
    assert_eq!(minus_counters(&runner, recipient), 0);
    assert!(!result.events.iter().any(|event| matches!(event,
        GameEvent::CounterAdded { object_id, .. } if *object_id == recipient
    )));
    assert!(runner.state().players[0].is_eliminated);
    assert_eq!(runner.state().objects[&recipient].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&recipient].controller, P1);
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().pending_cast.is_none());
    assert!(runner.state().pending_replacement.is_none());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 | P2 }
    ));
}

#[test]
fn soul_immolation_all_prohibited_recipient_offers_only_zero() {
    let mut scenario = GameScenario::new();
    let spell = soul(&mut scenario);
    let recipient = scenario.add_creature(P0, "Protected creature", 0, 5).id();
    add_solemnity(&mut scenario);
    let mut runner = scenario.build();
    announce(&mut runner, spell, CastPaymentMode::Auto).unwrap();
    assert_x(&runner, 0);
    assert!(runner.act(GameAction::ChooseX { value: 1 }).is_err());
    runner.act(GameAction::ChooseX { value: 0 }).unwrap();
    assert_blight(&runner, 0, &[recipient]);
    runner.act(select(recipient)).unwrap();
    assert_eq!(runner.state().stack.back().unwrap().source_id, spell);
}

#[test]
fn legacy_numeric_blight_continues_as_fixed_after_card_rehydration() {
    for amount in [0, 1, 3] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_mana_pool(
            P0,
            mana(&[
                ManaType::Red,
                ManaType::Red,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
            ]),
        );
        // Reproduce the legacy parsed cost carried by an already announced
        // cast, with zero as the lower-bound wire control.
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Soul Immolation", false, SOUL)
            .with_mana_cost(parse_mtgjson_mana_cost("{3}{R}{R}"))
            .with_additional_cost(AdditionalCost::Required(AbilityCost::Blight {
                count: BlightCount::Fixed(amount),
            }))
            .id();
        let recipient = scenario.add_vanilla(P0, 0, 4);
        let mut runner = scenario.build();
        attach_printed_ref(&mut runner, spell);
        announce(&mut runner, spell, CastPaymentMode::Auto).unwrap();
        assert_blight(&runner, amount, &[recipient]);
        let bare_cost = serde_json::json!({"type":"Blight","count":amount});
        assert_eq!(
            serde_json::from_value::<AbilityCost>(bare_cost.clone()).unwrap(),
            AbilityCost::Blight {
                count: BlightCount::Fixed(amount)
            }
        );
        assert_eq!(
            serde_json::to_value(AbilityCost::Blight {
                count: BlightCount::Fixed(amount)
            })
            .unwrap(),
            bare_cost
        );
        let mut resumed = checkpoint(&runner);
        for runner in [&mut runner, &mut resumed] {
            assert_blight(runner, amount, &[recipient]);
            runner.act(select(recipient)).unwrap();
            assert_eq!(runner.state().stack.back().unwrap().source_id, spell);
            assert_eq!(minus_counters(runner, recipient), amount);
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
        }
    }
    let malformed = serde_json::json!({"type":"Blight","count":{"type":"AnnouncedX"}});
    assert!(serde_json::from_value::<AbilityCost>(malformed).is_err());
}

#[test]
fn gristle_glutton_pays_fixed_blight_in_a_printed_activation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let glutton = scenario
        .add_creature_from_oracle(
            P0,
            "Gristle Glutton",
            1,
            3,
            "{T}, Blight 1: Discard a card. If you do, draw a card. (To blight 1, put a -1/-1 counter on a creature you control.)",
        )
        .id();
    let recipient = scenario.add_vanilla(P0, 2, 2);
    let discard = scenario.add_card_to_hand(P0, "Mountain");
    let kept = scenario.add_card_to_hand(P0, "Island");
    let draw = scenario.add_card_to_library_top(P0, "Forest");
    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: glutton,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    let WaitingFor::BlightChoice {
        player,
        counters,
        creatures,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("expected the printed ability to ask for its fixed Blight payment");
    };
    assert_eq!((*player, *counters), (P0, 1));
    assert!(creatures.contains(&recipient));
    runner.act(select(recipient)).unwrap();
    assert_eq!(minus_counters(&runner, recipient), 1);
    assert!(runner.state().objects[&glutton].tapped);
    assert!(
        matches!(runner.state().stack.back().map(|entry| &entry.kind),
            Some(StackEntryKind::ActivatedAbility { source_id, .. }) if *source_id == glutton
        )
    );
    runner.advance_until_stack_empty();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DiscardChoice {
            player: P0,
            count: 1,
            ..
        }
    ));
    runner.act(select(discard)).unwrap();
    assert!(runner.state().players[0].graveyard.contains(&discard));
    assert!(runner.state().players[0].hand.contains(&draw));
    assert!(runner.state().players[0].hand.contains(&kept));
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

fn auntie(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature(P0, "Auntie Ool, Cursewretch", 4, 4)
        .as_legendary()
        .from_oracle_text_with_keywords(&["Ward"], AUNTIE)
        .id()
}

struct WardBoard {
    runner: GameRunner,
    auntie: ObjectId,
    targeting_entry: ObjectId,
    lower_entry: ObjectId,
    recipient: Option<ObjectId>,
}

fn ward(
    ability: bool,
    prohibited: bool,
    has_recipient: bool,
    order_replacements: bool,
) -> WardBoard {
    let mut scenario = GameScenario::new_n_player(3, 206);
    scenario.at_phase(Phase::PreCombatMain);
    let auntie = auntie(&mut scenario);
    let recipient = has_recipient.then(|| scenario.add_creature(P2, "Blight recipient", 0, 7).id());
    let aura = recipient.map(|_| add_control_magic(&mut scenario, P1));
    let replacements = order_replacements.then(|| add_counter_replacements(&mut scenario, P1));
    if prohibited {
        add_solemnity(&mut scenario);
    }
    let target_source = if ability {
        scenario
            .add_creature_from_oracle(P1, "Prodigal Pyromancer", 1, 1, PYROMANCER)
            .id()
    } else {
        scenario
            .add_spell_to_hand_from_oracle(
                P1,
                "Lightning Bolt",
                true,
                "Lightning Bolt deals 3 damage to any target.",
            )
            .with_mana_cost(parse_mtgjson_mana_cost("{R}"))
            .id()
    };
    let lower = scenario
        .add_spell_to_hand_from_oracle(P1, "Shock", true, "Shock deals 2 damage to any target.")
        .with_mana_cost(parse_mtgjson_mana_cost("{R}"))
        .id();
    scenario.with_mana_pool(P1, mana(&[ManaType::Red, ManaType::Red]));
    let mut runner = scenario.build();
    for id in [auntie, target_source, lower] {
        attach_printed_ref(&mut runner, id);
    }
    if let (Some(aura), Some(recipient)) = (aura, recipient) {
        attach_printed_ref(&mut runner, aura);
        attach_control_magic(&mut runner, aura, recipient);
    }
    if let Some(replacements) = replacements {
        for id in replacements {
            attach_printed_ref(&mut runner, id);
        }
    }
    runner.act(GameAction::PassPriority).unwrap();
    runner.cast(lower).target_player(P2).commit();
    let lower_entry = runner.state().stack.back().unwrap().id;
    if ability {
        let outcome = runner
            .activate(target_source, 0)
            .target_object(auntie)
            .stop_at_optional()
            .resolve();
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::UnlessPayment { player: P1, .. }
        ));
    } else {
        runner.cast(target_source).target_object(auntie).commit();
    }
    let targeting_entry = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.source_id == target_source)
        .unwrap();
    assert!(if ability {
        matches!(
            targeting_entry.kind,
            StackEntryKind::ActivatedAbility { .. }
        )
    } else {
        matches!(targeting_entry.kind, StackEntryKind::Spell { .. })
    });
    let targeting_entry = targeting_entry.id;
    assert_ne!(targeting_entry, lower_entry);
    if !ability {
        let trigger = runner.state().stack.back().unwrap();
        assert_eq!(trigger.source_id, auntie);
        assert!(matches!(
            trigger.kind,
            StackEntryKind::TriggeredAbility { .. }
        ));
        for _ in 0..3 {
            runner.act(GameAction::PassPriority).unwrap();
        }
    }
    WardBoard {
        runner,
        auntie,
        targeting_entry,
        lower_entry,
        recipient,
    }
}

fn on_stack(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().stack.iter().any(|entry| entry.id == id)
}

fn ward_pick(runner: &mut GameRunner, recipient: ObjectId) {
    let WaitingFor::UnlessPayment {
        player: P1,
        cost: AbilityCost::Blight {
            count: BlightCount::Fixed(2),
        },
        trigger_event,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("expected the original Ward trigger");
    };
    let original_event = trigger_event.clone();
    assert!(matches!(
        original_event,
        Some(GameEvent::BecomesTarget {
            source_controller: P1,
            ..
        })
    ));
    runner.act(GameAction::PayUnlessCost { pay: true }).unwrap();
    let WaitingFor::UnlessBlightChoice {
        player,
        counters,
        creatures,
        trigger_event,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected Ward Blight prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!((*player, *counters), (P1, 2));
    assert!(creatures.contains(&recipient));
    assert_eq!(trigger_event, &original_event);
}

#[test]
fn auntie_ward_spell_and_ability_settle_exactly_one_stack_object() {
    for ability in [false, true] {
        for selection in [None, Some(false), Some(true)] {
            let pay = selection == Some(true);
            let WardBoard {
                mut runner,
                auntie,
                targeting_entry,
                lower_entry,
                recipient,
            } = ward(ability, false, true, false);
            let recipient = recipient.unwrap();
            if selection.is_some() {
                ward_pick(&mut runner, recipient);
                assert!(apply(runner.state_mut(), P2, select(recipient)).is_err());
                assert!(runner.act(select(auntie)).is_err());
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::UnlessBlightChoice { player: P1, .. }
                ));
            } else {
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::UnlessPayment { player: P1, .. }
                ));
            }
            let mut resumed = checkpoint(&runner);
            for runner in [&mut runner, &mut resumed] {
                if pay {
                    runner.act(candidate(runner, select(recipient))).unwrap();
                    assert_eq!(minus_counters(runner, recipient), 2);
                    assert_eq!(
                        runner
                            .state()
                            .stack
                            .iter()
                            .filter(|entry| entry.source_id == auntie)
                            .count(),
                        1
                    );
                } else {
                    runner
                        .act(candidate(runner, GameAction::PayUnlessCost { pay: false }))
                        .unwrap();
                    assert_eq!(minus_counters(runner, recipient), 0);
                }
                assert_eq!(on_stack(runner, targeting_entry), pay);
                assert!(on_stack(runner, lower_entry));
                runner.advance_until_stack_empty();
                assert!(runner.state().stack.is_empty());
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
                assert_eq!(runner.life(P1), if pay { 19 } else { 20 });
                assert_eq!(runner.life(P2), 18);
                assert_eq!(
                    runner.state().objects[&auntie].damage_marked,
                    if pay {
                        if ability {
                            1
                        } else {
                            3
                        }
                    } else {
                        0
                    }
                );
            }
        }
    }
}

#[test]
fn auntie_ward_cannot_pay_with_no_eligible_creature() {
    for prohibited in [false, true] {
        let WardBoard {
            runner,
            targeting_entry,
            lower_entry,
            ..
        } = ward(false, prohibited, prohibited, false);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(!on_stack(&runner, targeting_entry));
        assert!(on_stack(&runner, lower_entry));
    }
}

#[test]
fn auntie_ward_checkpoint_rejects_forged_recipient_and_placement_count() {
    let WardBoard {
        mut runner,
        auntie,
        recipient,
        ..
    } = ward(false, false, true, true);
    let recipient = recipient.unwrap();
    ward_pick(&mut runner, recipient);
    let mut forged = checkpoint(&runner);
    assert!(matches!(
        forged.state().waiting_for,
        WaitingFor::UnlessBlightChoice { player: P1, .. }
    ));

    let WaitingFor::UnlessBlightChoice { creatures, .. } = &mut forged.state_mut().waiting_for
    else {
        panic!("the forged snapshot must originate from the real Ward prompt");
    };
    *creatures = vec![auntie];
    let restore = decode_checkpoint(&forged)
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated);
    assert!(
        matches!(restore, Err(PersistedRestoreError::InvalidPaymentTransaction(message))
        if message.contains("invalid parked Blight selection"))
    );

    runner.act(select(recipient)).unwrap();
    let mut forged = checkpoint(&runner);
    assert!(matches!(
        forged.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P1, .. }
    ));
    let pending = forged.state_mut().pending_replacement.as_mut().unwrap();
    let ProposedEvent::AddCounter { count, .. } = &mut pending.proposed else {
        panic!("the parked payment must propose the original Ward placement");
    };
    assert_eq!(*count, 2);
    *count = 100;
    let restore = decode_checkpoint(&forged)
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated);
    assert!(
        matches!(restore, Err(PersistedRestoreError::InvalidPaymentTransaction(message))
        if message.contains("invalid parked unless Blight counter placement"))
    );
}

#[test]
fn auntie_ward_clears_both_payment_pauses_when_the_payer_departs() {
    for in_replacement in [false, true] {
        let WardBoard {
            mut runner,
            targeting_entry,
            recipient,
            ..
        } = ward(false, false, true, true);
        let recipient = recipient.unwrap();
        ward_pick(&mut runner, recipient);
        if in_replacement {
            runner.act(select(recipient)).unwrap();
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { player: P1, .. }
            ));
        }
        runner.act(GameAction::Concede { player_id: P1 }).unwrap();
        assert!(runner.state().players[1].is_eliminated);
        assert!(!on_stack(&runner, targeting_entry));
        assert!(runner.state().stack.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 | P2 }
        ));
        assert!(runner
            .state()
            .objects
            .get(&recipient)
            .is_none_or(|object| !object.counters.contains_key(&CounterType::Minus1Minus1)));
    }
}

#[test]
fn auntie_ward_replacement_checkpoint_settles_once() {
    for (first, expected) in [
        ("Vorinclex, Monstrous Raider", 5),
        ("Winding Constrictor", 6),
    ] {
        let WardBoard {
            mut runner,
            auntie,
            targeting_entry,
            lower_entry,
            recipient,
        } = ward(true, false, true, true);
        let recipient = recipient.unwrap();
        ward_pick(&mut runner, recipient);
        runner.act(select(recipient)).unwrap();
        let index = counter_order_index(&runner, P1, first);
        assert!(on_stack(&runner, targeting_entry));
        assert_eq!(minus_counters(&runner, recipient), 0);
        let mut resumed = checkpoint(&runner);
        for runner in [&mut runner, &mut resumed] {
            let result = runner.act(GameAction::ChooseReplacement { index }).unwrap();
            // CR 118.11 + CR 616.1: both orders pay the cost with a modified count.
            assert_eq!(minus_counters(runner, recipient), expected);
            assert_eq!(
                result
                    .events
                    .iter()
                    .filter(|event| matches!(event,
                        GameEvent::CounterAdded { object_id, .. } if *object_id == recipient
                    ))
                    .count(),
                1
            );
            assert!(on_stack(runner, targeting_entry));
            assert!(on_stack(runner, lower_entry));
            assert_eq!(
                runner
                    .state()
                    .stack
                    .iter()
                    .filter(|entry| entry.source_id == auntie)
                    .count(),
                1
            );
            runner.advance_until_stack_empty();
            assert_eq!(runner.life(P1), 19);
            assert_eq!(runner.life(P2), 18);
        }
    }
}

#[test]
fn auntie_ward_recipient_owner_concession_counters_targeting_ability() {
    let WardBoard {
        mut runner,
        auntie,
        targeting_entry,
        lower_entry,
        recipient,
    } = ward(true, false, true, true);
    let recipient = recipient.unwrap();
    ward_pick(&mut runner, recipient);
    runner.act(select(recipient)).unwrap();
    counter_order_index(&runner, P1, "Vorinclex, Monstrous Raider");
    assert!(on_stack(&runner, targeting_entry));

    // CR 800.4a + CR 702.21a: the recipient leaves with its owner. Ward
    // counters the targeting ability because the placement was abandoned.
    let result = runner.act(GameAction::Concede { player_id: P2 }).unwrap();
    assert!(!result.events.iter().any(|event| matches!(event,
        GameEvent::CounterAdded { object_id, .. } if *object_id == recipient
    )));
    assert!(runner.state().players[2].is_eliminated);
    assert_eq!(runner.state().objects[&recipient].zone, Zone::Exile);
    assert!(!on_stack(&runner, targeting_entry));
    assert!(on_stack(&runner, lower_entry));
    assert!(!runner
        .state()
        .stack
        .iter()
        .any(|entry| entry.source_id == auntie));
    assert!(runner.state().pending_cost_move_resume.is_none());
    assert!(runner.state().pending_replacement.is_none());
}

#[test]
fn auntie_draws_once_when_printed_blight_places_two_counters_on_its_creature() {
    let mut scenario = GameScenario::new();
    let soul = soul(&mut scenario);
    let auntie = auntie(&mut scenario);
    let drawn = scenario.add_card_to_library_top(P0, "Mountain");
    let untouched = scenario.add_card_to_library_top(P1, "Island");
    let mut runner = scenario.build();
    announce(&mut runner, soul, CastPaymentMode::Auto).unwrap();
    assert_x(&runner, 4);
    runner.act(GameAction::ChooseX { value: 2 }).unwrap();
    assert_blight(&runner, 2, &[auntie]);
    runner.act(select(auntie)).unwrap();
    assert_eq!(minus_counters(&runner, auntie), 2);
    assert_eq!(runner.state().stack.iter().filter(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { source_id, .. } if source_id == auntie)).count(), 1);
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&drawn].zone, Zone::Hand);
    assert_eq!(runner.state().players[0].hand.len(), 1);
    assert_eq!(runner.state().objects[&untouched].zone, Zone::Library);
    assert_eq!(runner.life(P0), 20);
    assert_eq!(runner.life(P1), 18);
}

#[test]
fn auntie_uses_last_controller_when_blight_kills_a_borrowed_creature() {
    let mut scenario = GameScenario::new_n_player(3, 419);
    scenario.at_phase(Phase::PreCombatMain);
    let auntie = auntie(&mut scenario);
    let recipient = scenario
        .add_creature(P2, "Borrowed creature", 1, 1)
        .controlled_by(P1)
        .id();
    scenario.add_vanilla(P1, 2, 3);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Soul Immolation", false, SOUL)
        .with_mana_cost(parse_mtgjson_mana_cost("{3}{R}{R}"))
        .id();
    scenario.with_mana_pool(
        P1,
        mana(&[
            ManaType::Red,
            ManaType::Red,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]),
    );
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    announce(&mut runner, spell, CastPaymentMode::Auto).unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseXValue {
            player: P1,
            max: 3,
            ..
        }
    ));
    runner.act(GameAction::ChooseX { value: 2 }).unwrap();
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::BlightChoice { player: P1, creatures, counters: 2, .. } if creatures.contains(&recipient))
    );
    runner.act(select(recipient)).unwrap();
    assert!(runner.state().players[2].graveyard.contains(&recipient));
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .filter(|entry| entry.source_id == auntie)
            .count(),
        1
    );
    for _ in 0..3 {
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert_eq!(
        (runner.life(P0), runner.life(P1), runner.life(P2)),
        (20, 19, 20)
    );
    assert_eq!(runner.state().stack.back().unwrap().source_id, spell);
    runner.advance_until_stack_empty();
    assert_eq!(
        (runner.life(P0), runner.life(P1), runner.life(P2)),
        (18, 19, 18)
    );
    assert!(runner.state().players[0].hand.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn auntie_ignores_plus_one_counters_from_snakeskin_veil() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    auntie(&mut scenario);
    let recipient = scenario.add_vanilla(P0, 0, 4);
    let veil = scenario.add_spell_to_hand_from_oracle(
        P0, "Snakeskin Veil", true,
        "Put a +1/+1 counter on target creature you control. It gains hexproof until end of turn. (It can't be the target of spells or abilities your opponents control.)",
    ).with_mana_cost(parse_mtgjson_mana_cost("{G}")).id();
    scenario.with_mana_pool(P0, mana(&[ManaType::Green]));
    let mut runner = scenario.build();
    runner.cast(veil).target_object(recipient).commit();
    assert_eq!(runner.state().stack.back().unwrap().source_id, veil);
    runner.act(GameAction::PassPriority).unwrap();
    let result = runner.act(GameAction::PassPriority).unwrap();
    assert!(result.events.iter().any(|event| matches!(event,
        GameEvent::CounterAdded { counter_type: CounterType::Plus1Plus1, object_id, count: 1, .. } if *object_id == recipient
    )));
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn auntie_ward_still_collects_payment_after_its_source_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let auntie = auntie(&mut scenario);
    let recipient = scenario.add_vanilla(P1, 0, 4);
    let response = scenario
        .add_spell_to_hand_from_oracle(P0, "Unsummon", true, UNSUMMON)
        .with_mana_cost(parse_mtgjson_mana_cost("{U}"))
        .id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(
            P1,
            "Lightning Bolt",
            true,
            "Lightning Bolt deals 3 damage to any target.",
        )
        .with_mana_cost(parse_mtgjson_mana_cost("{R}"))
        .id();
    scenario.with_mana_pool(P0, mana(&[ManaType::Blue]));
    scenario.with_mana_pool(P1, mana(&[ManaType::Red]));
    let mut runner = scenario.build();
    runner.act(GameAction::PassPriority).unwrap();
    runner.cast(bolt).target_object(auntie).commit();
    let targeting = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.source_id == bolt)
        .unwrap()
        .id;
    assert_eq!(runner.state().stack.back().unwrap().source_id, auntie);
    if matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ) {
        runner.act(GameAction::PassPriority).unwrap();
    }
    runner.cast(response).target_object(auntie).commit();
    runner.advance_until_stack_empty();
    assert!(runner
        .state()
        .objects
        .get(&auntie)
        .is_none_or(|obj| obj.zone != Zone::Battlefield));
    ward_pick(&mut runner, recipient);
    runner.act(select(recipient)).unwrap();
    assert!(on_stack(&runner, targeting));
    assert_eq!(minus_counters(&runner, recipient), 2);
    assert!(!runner
        .state()
        .stack
        .iter()
        .any(|entry| entry.source_id == auntie));
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
}

#[test]
fn gnarlbark_end_step_draws_before_blight_and_without_a_recipient() {
    for remove in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PostCombatMain);
        let gnarlbark = scenario
            .add_creature_from_oracle(P0, "Sinister Gnarlbark", 0, 4, GNARLBARK)
            .id();
        let opposing = scenario.add_vanilla(P1, 1, 4);
        let drawn = scenario.add_card_to_library_top(P0, "Mountain");
        let response = scenario
            .add_spell_to_hand_from_oracle(P0, "Unsummon", true, UNSUMMON)
            .with_mana_cost(parse_mtgjson_mana_cost("{U}"))
            .id();
        scenario.add_basic_land(P0, ManaColor::Blue);
        let mut runner = scenario.build();
        runner.advance_to_end_step();
        assert!(
            matches!(runner.state().stack.back().map(|entry| &entry.kind),
                Some(StackEntryKind::TriggeredAbility { source_id, .. }) if *source_id == gnarlbark
            )
        );
        assert_eq!(runner.state().objects[&drawn].zone, Zone::Library);
        if remove {
            runner.cast(response).target_object(gnarlbark).commit();
        }
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&drawn].zone, Zone::Hand);
        if remove {
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
        } else {
            let WaitingFor::EffectZoneChoice {
                player,
                cards,
                count,
                ..
            } = &runner.state().waiting_for
            else {
                panic!(
                    "expected Blight after draw, got {:?}",
                    runner.state().waiting_for
                );
            };
            assert_eq!((*player, *count), (P0, 1));
            assert_eq!(cards, &[gnarlbark]);
            assert_eq!(minus_counters(&runner, gnarlbark), 0);
            runner.act(select(gnarlbark)).unwrap();
            assert_eq!(minus_counters(&runner, gnarlbark), 1);
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
        }
        assert!(runner.state().stack.is_empty());
        assert_eq!(minus_counters(&runner, opposing), 0);
    }
}

#[test]
fn gnarlbark_does_not_trigger_at_the_opponents_end_step() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let gnarlbark = scenario
        .add_creature_from_oracle(P0, "Sinister Gnarlbark", 0, 4, GNARLBARK)
        .id();
    let drawn = scenario.add_card_to_library_top(P0, "Mountain");
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    runner.advance_to_end_step();
    assert_eq!(runner.state().phase, Phase::End);
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&drawn].zone, Zone::Library);
    assert_eq!(minus_counters(&runner, gnarlbark), 0);
}

#[test]
fn gnarlbark_blights_after_a_replaced_draw_finishes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let gnarlbark = scenario
        .add_creature_from_oracle(P0, "Sinister Gnarlbark", 0, 4, GNARLBARK)
        .id();
    let mossdog = scenario
        .add_creature_to_graveyard(P0, "Greater Mossdog", 3, 3)
        .from_oracle_text_with_keywords(
            &["Dredge"],
            "Dredge 3 (If you would draw a card, you may mill three cards instead. If you do, return this card from your graveyard to your hand.)",
        )
        .id();
    scenario.with_library_top(P0, &["Forest", "Swamp", "Mountain", "Plains"]);
    let mut runner = scenario.build();
    runner.advance_to_end_step();
    assert_eq!(runner.state().stack.back().unwrap().source_id, gnarlbark);
    runner.advance_until_stack_empty();
    let WaitingFor::ReplacementChoice {
        player,
        kind,
        candidates,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("expected Greater Mossdog to offer dredge before Blight");
    };
    assert_eq!(*player, P0);
    assert_eq!(*kind, ReplacementChoiceKind::OptionalBranch);
    assert_eq!(candidates.len(), 2);
    assert!(candidates.iter().all(|choice| choice.source_id == mossdog));
    assert_eq!(runner.state().objects[&mossdog].zone, Zone::Graveyard);
    assert_eq!(minus_counters(&runner, gnarlbark), 0);
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .unwrap();
    assert_eq!(runner.state().objects[&mossdog].zone, Zone::Hand);
    assert_eq!(runner.state().players[0].library.len(), 1);
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::EffectZoneChoice {
        player: P0, cards, ..
    } if cards == &[gnarlbark])
    );
    runner.act(select(gnarlbark)).unwrap();
    assert_eq!(minus_counters(&runner, gnarlbark), 1);
    assert_eq!(runner.state().players[0].hand.len(), 1);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}
