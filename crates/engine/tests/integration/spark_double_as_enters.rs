//! Spark Double preserves its printed entry counter while applying a copied
//! as-enters choice.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SPARK_DOUBLE: &str = "You may have this creature enter as a copy of a creature or planeswalker you control, except it enters with an additional +1/+1 counter on it if it's a creature, it enters with an additional loyalty counter on it if it's a planeswalker, and it isn't legendary.";
const PAINTERS_SERVANT: &str = "As this creature enters, choose a color.\nAll cards that aren't on the battlefield, spells, and permanents are the chosen color in addition to their other colors.";

#[test]
fn spark_double_copying_painters_servant_keeps_its_counter_and_prompts_for_color_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let painter = scenario
        .add_creature_from_oracle(P0, "Painter's Servant", 1, 3, PAINTERS_SERVANT)
        .id();
    let spark = scenario
        .add_creature_to_hand_from_oracle(P0, "Spark Double", 0, 0, SPARK_DOUBLE)
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spark].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spark,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Spark Double");
    runner.advance_until_stack_empty();

    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        panic!(
            "Spark Double must offer its optional enter-as-copy replacement, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(candidates.len(), 2);
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept Spark Double's copy replacement");

    let WaitingFor::CopyTargetChoice { valid_targets, .. } = &runner.state().waiting_for else {
        panic!(
            "Spark Double must offer a copy target, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(valid_targets.contains(&painter));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(painter)),
        })
        .expect("copy Painter's Servant");

    let WaitingFor::NamedChoice { options, .. } = &runner.state().waiting_for else {
        panic!(
            "the copied Painter's Servant must offer its as-enters color choice, got {:?}",
            runner.state().waiting_for
        );
    };
    let blue = options
        .iter()
        .find(|option| option.eq_ignore_ascii_case("blue"))
        .expect("Painter's Servant offers blue")
        .clone();
    runner
        .act(GameAction::ChooseOption { choice: blue })
        .expect("choose blue for the copied Painter's Servant");

    let entered = &runner.state().objects[&spark];
    assert_eq!(entered.zone, Zone::Battlefield);
    assert_eq!(entered.name, "Painter's Servant");
    assert_eq!(entered.chosen_color(), Some(ManaColor::Blue));
    assert_eq!(
        entered.counters.get(&CounterType::Plus1Plus1).copied(),
        Some(1),
        "CR 614.12 + CR 707.9: Spark Double's printed entry replacement must add exactly one counter while the copied as-enters choice applies once"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

const CLEVER_IMPERSONATOR: &str =
    "You may have this creature enter as a copy of any nonland permanent on the battlefield.";
const JACE_BELEREN: &str = "[+2]: Each player draws a card.\n[−1]: Target player draws a card.\n[−10]: Target player mills twenty cards.";
const LAEZEL: &str = "If you would put one or more counters on a creature or planeswalker you control or on yourself, put that many plus one of each of those kinds of counters on that permanent or player instead.\nChoose a Background (You can have a Background as a second commander.)";
const SCALES: &str = "If one or more +1/+1 counters would be put on a creature you control, that many plus one +1/+1 counters are put on it instead.";
const SEASON: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.\nIf an effect would put one or more counters on a permanent you control, it puts twice that many of those counters on that permanent instead.";
const TRISKELION: &str = "This creature enters with three +1/+1 counters on it.\nRemove a +1/+1 counter from this creature: It deals 1 damage to any target.";

use engine::game::scenario::{GameRunner, P1};
use engine::types::card::PrintedLoyalty;
use engine::types::identifiers::ObjectId;

fn copy_spell(scenario: &mut GameScenario, clever: bool) -> ObjectId {
    let (name, oracle) = if clever {
        ("Clever Impersonator", CLEVER_IMPERSONATOR)
    } else {
        ("Spark Double", SPARK_DOUBLE)
    };
    scenario
        .add_creature_to_hand_from_oracle(P0, name, 0, 0, oracle)
        .id()
}

fn jace(scenario: &mut GameScenario, owner: engine::types::player::PlayerId) -> ObjectId {
    scenario
        .add_planeswalker_from_oracle(owner, "Jace Beleren", "Jace", 3, JACE_BELEREN)
        .as_planeswalker_with_loyalty("Jace", 3)
        .as_legendary()
        .id()
}

fn begin_copy(runner: &mut GameRunner, spell: ObjectId, donor: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast copier");
    runner.advance_until_stack_empty();
    let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected optional copy replacement, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(candidates.len(), 2);
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept copy");
    let WaitingFor::CopyTargetChoice {
        player,
        valid_targets,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected copy-source choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert!(valid_targets.contains(&donor));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(donor)),
        })
        .expect("choose copy source");
}

fn choose_counter_replacement(runner: &mut GameRunner, source: ObjectId) {
    let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected counter ordering prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    let index = candidates
        .iter()
        .position(|candidate| candidate.source_id == source)
        .expect("requested replacement is offered");
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("order counter replacements");
}

fn assert_entry_complete(runner: &mut GameRunner) {
    runner.advance_until_stack_empty();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "entry did not reach priority: {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());
    assert!(
        runner.state().resolution_stack.is_empty(),
        "entry stranded a resolution frame"
    );
    assert!(
        runner.state().resolving_stack_entry.is_none(),
        "spell carrier did not retire"
    );
}

fn loyalty_case(clever: bool, laezel: bool, season: bool, expected: u32) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let donor = jace(&mut scenario, if clever { P1 } else { P0 });
    if laezel {
        scenario.add_creature_from_oracle(P0, "Lae'zel, Vlaakith's Champion", 3, 3, LAEZEL);
    }
    if season {
        scenario.add_enchantment_from_oracle(P0, "Doubling Season", SEASON);
    }
    let spell = copy_spell(&mut scenario, clever);
    let mut runner = scenario.build();
    {
        let donor = runner.state_mut().objects.get_mut(&donor).unwrap();
        donor.printed_loyalty = Some(PrintedLoyalty::Fixed(3));
        donor.base_printed_loyalty = Some(PrintedLoyalty::Fixed(3));
        donor.counters.insert(CounterType::Loyalty, 9);
        donor.loyalty = Some(9);
    }
    begin_copy(&mut runner, spell, donor);
    let entered = &runner.state().objects[&spell];
    assert_eq!(entered.name, "Jace Beleren");
    assert_eq!(entered.zone, Zone::Battlefield);
    assert_eq!(
        (
            entered.counters.get(&CounterType::Loyalty).copied(),
            entered.loyalty
        ),
        (Some(expected), Some(expected))
    );
    assert_eq!(
        runner.state().objects[&donor]
            .counters
            .get(&CounterType::Loyalty),
        Some(&9)
    );
    assert_entry_complete(&mut runner);
}

#[test]
fn spark_double_loyalty_uses_printed_three_not_donors_current_nine() {
    loyalty_case(false, false, false, 4);
}

#[test]
fn spark_double_clever_impersonator_gets_intrinsic_loyalty() {
    loyalty_case(true, false, false, 3);
}

#[test]
fn spark_double_laezel_modifies_the_merged_loyalty_once() {
    loyalty_case(false, true, false, 5);
}

#[test]
fn spark_double_season_doubles_the_merged_loyalty() {
    loyalty_case(false, false, true, 8);
}

fn ordered_bears_case(scales_first: bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let donor = scenario
        .add_creature_from_oracle(P0, "Grizzly Bears", 2, 2, "")
        .id();
    let scales = scenario
        .add_enchantment_from_oracle(P0, "Hardened Scales", SCALES)
        .id();
    let season = scenario
        .add_enchantment_from_oracle(P0, "Doubling Season", SEASON)
        .id();
    let spell = copy_spell(&mut scenario, false);
    let mut runner = scenario.build();
    begin_copy(&mut runner, spell, donor);
    choose_counter_replacement(&mut runner, if scales_first { scales } else { season });
    let expected = if scales_first { 4 } else { 3 };
    assert_eq!(
        runner.state().objects[&spell]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&expected)
    );
    assert_entry_complete(&mut runner);
}

#[test]
fn spark_double_bears_scales_before_season_keeps_counter_prompt() {
    ordered_bears_case(true);
}

#[test]
fn spark_double_bears_season_before_scales_keeps_counter_prompt() {
    ordered_bears_case(false);
}

#[test]
fn spark_double_creature_exception_does_not_add_loyalty() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let donor = scenario
        .add_creature_from_oracle(P0, "Grizzly Bears", 2, 2, "")
        .id();
    let spell = copy_spell(&mut scenario, false);
    let mut runner = scenario.build();
    begin_copy(&mut runner, spell, donor);
    let object = &runner.state().objects[&spell];
    assert_eq!(object.name, "Grizzly Bears");
    assert_eq!((object.power, object.toughness), (Some(3), Some(3)));
    assert_eq!(object.counters.get(&CounterType::Plus1Plus1), Some(&1));
    assert_eq!(
        (object.counters.get(&CounterType::Loyalty), object.loyalty),
        (None, None)
    );
    assert_entry_complete(&mut runner);
}

#[test]
fn spark_double_triskelion_and_exception_share_one_scales_application() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let donor = scenario
        .add_creature_from_oracle(P0, "Triskelion", 1, 1, TRISKELION)
        .id();
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", SCALES);
    let spell = copy_spell(&mut scenario, false);
    let mut runner = scenario.build();
    begin_copy(&mut runner, spell, donor);
    assert_eq!(
        runner.state().objects[&spell]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&5)
    );
    assert_entry_complete(&mut runner);
}

fn ordered_triskelion_case(scales_first: bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let donor = scenario
        .add_creature_from_oracle(P1, "Triskelion", 1, 1, TRISKELION)
        .id();
    scenario.add_creature_from_oracle(
        P0,
        "Soul Warden",
        1,
        1,
        "Whenever another creature enters, you gain 1 life.",
    );
    let scales = scenario
        .add_enchantment_from_oracle(P0, "Hardened Scales", SCALES)
        .id();
    let season = scenario
        .add_enchantment_from_oracle(P0, "Doubling Season", SEASON)
        .id();
    let spell = copy_spell(&mut scenario, true);
    let mut runner = scenario.build();
    let life = runner.life(P0);
    begin_copy(&mut runner, spell, donor);
    assert_eq!(runner.life(P0), life, "observer waits until entry settles");
    choose_counter_replacement(&mut runner, if scales_first { scales } else { season });
    assert_eq!(
        runner.state().objects[&spell]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&if scales_first { 8 } else { 7 })
    );
    assert_entry_complete(&mut runner);
    assert_eq!(
        runner.life(P0),
        life + 1,
        "entry observer fires exactly once"
    );
}

#[test]
fn spark_double_clever_triskelion_scales_before_season_completes_entry_once() {
    ordered_triskelion_case(true);
}

#[test]
fn spark_double_clever_triskelion_season_before_scales_completes_entry_once() {
    ordered_triskelion_case(false);
}

fn ordered_loyalty_case(laezel_first: bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let donor = jace(&mut scenario, P1);
    let laezel = scenario
        .add_creature_from_oracle(P0, "Lae'zel, Vlaakith's Champion", 3, 3, LAEZEL)
        .id();
    let season = scenario
        .add_enchantment_from_oracle(P0, "Doubling Season", SEASON)
        .id();
    let spell = copy_spell(&mut scenario, true);
    let mut runner = scenario.build();
    begin_copy(&mut runner, spell, donor);
    choose_counter_replacement(&mut runner, if laezel_first { laezel } else { season });
    let expected = if laezel_first { 8 } else { 7 };
    assert_eq!(
        runner.state().objects[&spell]
            .counters
            .get(&CounterType::Loyalty),
        Some(&expected)
    );
    assert_entry_complete(&mut runner);
    assert_eq!(runner.state().objects[&spell].loyalty, Some(expected));
}

#[test]
fn spark_double_clever_jace_laezel_before_season() {
    ordered_loyalty_case(true);
}

#[test]
fn spark_double_clever_jace_season_before_laezel() {
    ordered_loyalty_case(false);
}

fn paused_painter() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let donor = scenario
        .add_creature_from_oracle(P0, "Painter's Servant", 1, 3, PAINTERS_SERVANT)
        .id();
    scenario.add_creature_from_oracle(
        P0,
        "Soul Warden",
        1,
        1,
        "Whenever another creature enters, you gain 1 life.",
    );
    let scales = scenario
        .add_enchantment_from_oracle(P0, "Hardened Scales", SCALES)
        .id();
    let season = scenario
        .add_enchantment_from_oracle(P0, "Doubling Season", SEASON)
        .id();
    let spell = copy_spell(&mut scenario, false);
    let mut runner = scenario.build();
    begin_copy(&mut runner, spell, donor);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P0, .. }
    ));
    (runner, spell, scales, season)
}

fn choose_painter_blue(runner: &mut GameRunner) -> Vec<engine::types::events::GameEvent> {
    let WaitingFor::NamedChoice {
        player, options, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "copied color choice must follow counters: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    let choice = options
        .iter()
        .find(|option| option.eq_ignore_ascii_case("blue"))
        .unwrap()
        .clone();
    runner
        .act(GameAction::ChooseOption { choice })
        .expect("choose copied color exactly once")
        .events
}

#[test]
fn spark_double_painter_counter_pause_then_color_then_observer_once() {
    let (mut runner, spell, scales, _) = paused_painter();
    let life = runner.life(P0);
    choose_counter_replacement(&mut runner, scales);
    assert_eq!(runner.life(P0), life);
    choose_painter_blue(&mut runner);
    assert_eq!(
        runner.state().objects[&spell].chosen_color(),
        Some(ManaColor::Blue)
    );
    assert_eq!(
        runner.state().objects[&spell]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&4)
    );
    assert_entry_complete(&mut runner);
    assert_eq!(runner.life(P0), life + 1);
}

#[test]
fn spark_double_counter_pause_survives_persisted_restore() {
    use engine::types::game_state::{PersistedGameState, PersistedRestoreFinalization};
    let (runner, spell, _, season) = paused_painter();
    let life = runner.life(P0);
    let wire = serde_json::to_value(PersistedGameState::capture(runner.state().clone())).unwrap();
    let restored = serde_json::from_value::<PersistedGameState>(wire)
        .unwrap()
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .unwrap()
        .finalize_after_rehydration(|_| Ok(()))
        .unwrap();
    let mut runner = GameRunner::from_state(restored);
    choose_counter_replacement(&mut runner, season);
    choose_painter_blue(&mut runner);
    assert_eq!(
        runner.state().objects[&spell]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&3)
    );
    assert_entry_complete(&mut runner);
    assert_eq!(runner.life(P0), life + 1);
}

#[test]
fn spark_double_counter_completion_preserves_nonhead_order_and_nested_choice_remainder() {
    use engine::types::ability::EffectKind;
    use engine::types::events::GameEvent;
    use engine::types::game_state::{PendingCounterPostAction, PendingEffectResolutionEvent};
    let (mut runner, spell, scales, _) = paused_painter();
    let completion = runner
        .state_mut()
        .active_counter_additions_mut()
        .unwrap()
        .completion
        .as_mut()
        .unwrap();
    completion.post_actions.insert(
        0,
        PendingCounterPostAction::EmitEffectResolved {
            kind: EffectKind::Scry,
            source_id: spell,
        },
    );
    completion
        .post_actions
        .push(PendingCounterPostAction::EmitEffectResolved {
            kind: EffectKind::Surveil,
            source_id: spell,
        });
    completion.kind = EffectKind::Draw;
    completion.resolution_event = PendingEffectResolutionEvent::Emit;
    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        unreachable!()
    };
    let index = candidates
        .iter()
        .position(|candidate| candidate.source_id == scales)
        .unwrap();
    let mut events = runner
        .act(GameAction::ChooseReplacement { index })
        .unwrap()
        .events;
    let kinds = |events: &[GameEvent]| {
        events
            .iter()
            .filter_map(|event| {
                if let GameEvent::EffectResolved {
                    kind, source_id, ..
                } = event
                {
                    (*source_id == spell
                        && matches!(
                            kind,
                            EffectKind::Scry | EffectKind::Surveil | EffectKind::Draw
                        ))
                    .then_some(*kind)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(kinds(&events), vec![EffectKind::Scry]);
    events.extend(choose_painter_blue(&mut runner));
    assert_entry_complete(&mut runner);
    assert_eq!(
        kinds(&events),
        vec![EffectKind::Scry, EffectKind::Surveil, EffectKind::Draw]
    );
    assert_eq!(
        runner.state().objects[&spell].chosen_color(),
        Some(ManaColor::Blue)
    );
}

#[test]
fn spark_double_prevented_counter_placement_still_completes_entry() {
    use engine::types::ability::{QuantityModification, ReplacementDefinition};
    use engine::types::proposed_event::ProposedEvent;
    use engine::types::replacements::ReplacementEvent;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let donor = scenario
        .add_creature_from_oracle(P0, "Grizzly Bears", 2, 2, "")
        .id();
    scenario.add_creature_from_oracle(
        P0,
        "Soul Warden",
        1,
        1,
        "Whenever another creature enters, you gain 1 life.",
    );
    let scales = scenario
        .add_enchantment_from_oracle(P0, "Hardened Scales", SCALES)
        .id();
    scenario.add_enchantment_from_oracle(P0, "Doubling Season", SEASON);
    let spell = copy_spell(&mut scenario, false);
    let mut runner = scenario.build();
    let life = runner.life(P0);
    begin_copy(&mut runner, spell, donor);
    let pending = runner
        .state()
        .pending_replacement
        .as_ref()
        .expect("counter placement paused");
    assert!(matches!(
        &pending.proposed,
        ProposedEvent::AddCounter { .. }
    ));
    assert_eq!(pending.proposed.affected_object_id(), Some(spell));
    // A real Tatterkite + Scales cannot produce this pause: mandatory counter
    // prohibition short-circuits ordering in replacement::pipeline_loop.
    // Stage the native Prevent outcome on an already-offered candidate, as in
    // cost_removed_counter_amount::stage_prevented_pending_event. This tests
    // the public resume boundary, not a claim that the card's Oracle changed.
    let prevention = ReplacementDefinition::new(ReplacementEvent::AddCounter)
        .valid_card(engine::types::ability::TargetFilter::SpecificObject { id: spell })
        .quantity_modification(QuantityModification::Prevent);
    let source = runner.state_mut().objects.get_mut(&scales).unwrap();
    source.replacement_definitions = vec![prevention.clone()].into();
    source.base_replacement_definitions = std::sync::Arc::new(vec![prevention]);
    choose_counter_replacement(&mut runner, scales);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert!(
        runner.state().resolution_stack.is_empty(),
        "the answer must finish entry in the same action"
    );
    assert!(runner.state().resolving_stack_entry.is_none());
    assert_eq!(
        runner.life(P0),
        life,
        "the observer is queued, not yet resolved"
    );
    let copy = &runner.state().objects[&spell];
    assert_eq!(copy.name, "Grizzly Bears");
    assert_eq!(
        (copy.power, copy.toughness, copy.loyalty),
        (Some(2), Some(2), None)
    );
    assert!(runner.state().objects[&spell].counters.is_empty());
    assert_entry_complete(&mut runner);
    assert_eq!(runner.life(P0), life + 1);
}

#[test]
fn spark_double_stale_entry_pin_retires_without_color_or_observer() {
    let (mut runner, spell, scales, _) = paused_painter();
    let life = runner.life(P0);
    let object = runner.state_mut().objects.get_mut(&spell).unwrap();
    object.incarnation += 1;
    choose_counter_replacement(&mut runner, scales);
    // The pending counter placement belongs to the replacement machinery;
    // the stale completion must not choose a color or replay the old ETB.
    assert_eq!(runner.state().objects[&spell].chosen_color(), None);
    assert_entry_complete(&mut runner);
    assert_eq!(runner.life(P0), life);
    assert!(runner.state().deferred_entry_events.is_empty());
}

#[test]
fn spark_double_decline_and_no_legal_source_do_not_mint_copy_completion() {
    for with_source in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let donor = if with_source {
            Some(
                scenario
                    .add_creature_from_oracle(P0, "Grizzly Bears", 2, 2, "")
                    .id(),
            )
        } else {
            None
        };
        let spell = copy_spell(&mut scenario, false);
        let mut runner = scenario.build();
        let card_id = runner.state().objects[&spell].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        runner.advance_until_stack_empty();
        if with_source {
            let WaitingFor::ReplacementChoice {
                player, candidates, ..
            } = &runner.state().waiting_for
            else {
                panic!("decline must be offered");
            };
            assert_eq!(*player, P0);
            assert_eq!(candidates.len(), 2);
            runner
                .act(GameAction::ChooseReplacement { index: 1 })
                .unwrap();
        }
        assert_entry_complete(&mut runner);
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
        assert!(runner.state().objects[&spell].counters.is_empty());
        if let Some(donor) = donor {
            assert_eq!(runner.state().objects[&donor].zone, Zone::Battlefield);
        }
    }
}

#[test]
fn spark_double_copy_that_fails_to_apply_holds_no_counter_exception() {
    // The donor leaves between the prompt and the answer, so the copy effect
    // errors and Spark Double stays an uncopied 0/0 creature. The held
    // "+1/+1 counter if it's a creature" exception must not be evaluated
    // against that uncopied object, or it would survive as a 1/1.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let donor = scenario
        .add_creature_from_oracle(P0, "Grizzly Bears", 2, 2, "")
        .id();
    let spell = copy_spell(&mut scenario, false);
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast copier");
    runner.advance_until_stack_empty();
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept copy");
    // Reach guard: the copy-source prompt is up and offers the donor.
    let WaitingFor::CopyTargetChoice { valid_targets, .. } = &runner.state().waiting_for else {
        panic!("expected copy-source choice");
    };
    assert!(valid_targets.contains(&donor));
    runner.state_mut().battlefield.retain(|id| *id != donor);
    runner.state_mut().objects.remove(&donor);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(donor)),
        })
        .expect("answer with the vanished donor");
    assert_entry_complete(&mut runner);
    // Positive reach: Spark Double entered, and was not given the exception
    // counter, so state-based actions put the uncopied 0/0 into the graveyard.
    assert!(runner.state().objects[&spell].counters.is_empty());
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
}

#[test]
fn spark_double_printed_x_loyalty_noncast_boundary_adds_only_exception() {
    const NISSA: &str = "[+2]: Scry 2.\n[0]: Look at the top card of your library. If it's a land card or a creature card with mana value less than or equal to the number of loyalty counters on Nissa, you may put that card onto the battlefield.\n[−6]: Untap up to two target lands you control. They become 5/5 Elemental creatures with flying and haste until end of turn. They're still lands.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Spark Double copies a planeswalker directly.
    let donor = scenario
        .add_planeswalker_from_oracle(P0, "Nissa, Steward of Elements", "Nissa", 2, NISSA)
        .id();
    let spell = copy_spell(&mut scenario, false);
    let zombify = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Zombify",
            false,
            "Return target creature card from your graveyard to the battlefield.",
        )
        .id();
    let mut runner = scenario.build();
    {
        let object = runner.state_mut().objects.get_mut(&donor).unwrap();
        object.printed_loyalty = Some(PrintedLoyalty::X);
        object.base_printed_loyalty = Some(PrintedLoyalty::X);
    }
    engine::game::zones::move_to_zone(runner.state_mut(), spell, Zone::Graveyard, &mut Vec::new());
    runner.cast(zombify).target_object(spell).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P0, .. }
    ));
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .unwrap();
    let WaitingFor::CopyTargetChoice {
        player,
        valid_targets,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("Zombify entry must offer a copy");
    };
    assert_eq!(*player, P0);
    assert!(valid_targets.contains(&donor));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(donor)),
        })
        .unwrap();
    assert_eq!(
        runner.state().objects[&spell]
            .counters
            .get(&CounterType::Loyalty),
        Some(&1)
    );
    assert_entry_complete(&mut runner);
    assert_eq!(runner.state().objects[&zombify].zone, Zone::Graveyard);
}

#[test]
fn spark_double_copy_chain_rider_is_present_before_entry_observer_resolves() {
    use engine::types::keywords::Keyword;
    const FOURTEENTH: &str = "When you cast this spell, reveal the top fourteen cards of your library. Put all Doctor cards revealed this way into your graveyard and the rest on the bottom of your library in a random order.\nYou may have The Fourteenth Doctor enter as a copy of a Doctor card in your graveyard that was put there from your library this turn. If you do, it gains haste until end of turn.";
    const FIFTH: &str = "Peaceful Coexistence — At the beginning of your end step, put a +1/+1 counter on each creature you control that didn't attack or enter this turn. Untap those creatures.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let donor = scenario
        .add_creature_from_oracle(P0, "The Fifth Doctor", 2, 2, FIFTH)
        .with_subtypes(vec!["Time Lord", "Doctor"])
        .id();
    scenario.add_creature_from_oracle(
        P0,
        "Soul Warden",
        1,
        1,
        "Whenever another creature enters, you gain 1 life.",
    );
    let spell = scenario
        .add_creature_to_hand_from_oracle(P0, "The Fourteenth Doctor", 3, 3, FOURTEENTH)
        .with_subtypes(vec!["Time Lord", "Doctor"])
        .id();
    let mut runner = scenario.build();
    engine::game::zones::move_to_library_position(runner.state_mut(), donor, true, &mut Vec::new());
    let life = runner.life(P0);
    begin_copy(&mut runner, spell, donor);
    assert_eq!(
        runner.state().objects[&donor].zone,
        Zone::Graveyard,
        "the cast trigger produced the eligible graveyard source"
    );
    assert_eq!(runner.state().objects[&spell].name, "The Fifth Doctor");
    assert!(
        runner.state().objects[&spell].has_keyword(&Keyword::Haste),
        "the copy-chain rider ran before the entry observer's stack resolution"
    );
    assert_eq!(runner.life(P0), life);
    assert!(
        !runner.state().stack.is_empty(),
        "entry observer reached the stack"
    );
    assert_entry_complete(&mut runner);
    assert_eq!(runner.life(P0), life + 1);
}

#[test]
fn spark_double_copies_mirror_mage_tokens_modified_printed_loyalty() {
    const MIRROR_MAGE: &str = "Kicker {2}\nWhen Jace enters, if Jace was kicked, create a token that's a copy of Jace, except it's not legendary and its starting loyalty is 1.\n[+1]: Scry 2.\n[0]: Draw a card and reveal it. Remove a number of loyalty counters equal to that card's mana value from Jace.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mage = scenario
        .add_creature_to_hand_from_oracle(P0, "Jace, Mirror Mage", 0, 0, MIRROR_MAGE)
        .from_oracle_text_with_keywords(&["Kicker"], MIRROR_MAGE)
        .as_planeswalker_with_loyalty("Jace", 4)
        .as_legendary()
        .id();
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);
    let spell = copy_spell(&mut scenario, false);
    let mut runner = scenario.build();
    runner.cast(mage).accept_optional().resolve();
    assert_entry_complete(&mut runner);
    let tokens: Vec<_> = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            let object = &runner.state().objects[id];
            object.is_token && object.name == "Jace, Mirror Mage"
        })
        .collect();
    assert_eq!(
        tokens.len(),
        1,
        "kicked Jace created the real exception-bearing token"
    );
    let donor = tokens[0];
    assert_eq!(
        runner.state().objects[&donor]
            .counters
            .get(&CounterType::Loyalty),
        Some(&1)
    );
    begin_copy(&mut runner, spell, donor);
    assert_eq!(
        runner.state().objects[&spell]
            .counters
            .get(&CounterType::Loyalty),
        Some(&2)
    );
    assert_entry_complete(&mut runner);
    assert_eq!(runner.state().objects[&mage].zone, Zone::Battlefield);
}

#[test]
fn spark_double_counter_completion_replays_copied_targeted_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let donor = scenario
        .add_creature_from_oracle(
            P0,
            "Man-o'-War",
            2,
            2,
            "When this creature enters, return target creature to its owner's hand.",
        )
        .id();
    let scales = scenario
        .add_enchantment_from_oracle(P0, "Hardened Scales", SCALES)
        .id();
    scenario.add_enchantment_from_oracle(P0, "Doubling Season", SEASON);
    let spell = copy_spell(&mut scenario, false);
    let mut runner = scenario.build();
    begin_copy(&mut runner, spell, donor);
    choose_counter_replacement(&mut runner, scales);
    let WaitingFor::TriggerTargetSelection {
        player, source_id, ..
    } = &runner.state().waiting_for
    else {
        panic!("the copied entry trigger must reach target selection");
    };
    assert_eq!(*player, P0);
    assert_eq!(*source_id, Some(spell));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(donor)),
        })
        .expect("choose the donor for the copied trigger");
    assert_entry_complete(&mut runner);
    assert_eq!(runner.state().objects[&donor].zone, Zone::Hand);
    let copy = &runner.state().objects[&spell];
    assert_eq!(copy.zone, Zone::Battlefield);
    assert_eq!(copy.name, "Man-o'-War");
    assert_eq!(copy.counters.get(&CounterType::Plus1Plus1), Some(&4));
}
