//! Entry-counter composition and copied as-enters choices for ordinary copiers
//! and Feldon's liminal copy tokens, including ordering and completion boundaries.

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

mod feldon_tokens {
    use super::*;
    use engine::game::scenario_db::GameScenarioDbExt;
    use engine::types::ability::ChosenAttribute;
    use engine::types::events::GameEvent;
    use engine::types::keywords::Keyword;
    use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
    use engine::types::player::PlayerId;

    const FELDON: &str = "{2}{R}, {T}: Create a token that's a copy of target creature card in your graveyard, except it's an artifact in addition to its other types. It gains haste. Sacrifice it at the beginning of the next end step.";
    const JACE: &str = "+2: Each player draws a card.\n−1: Target player draws a card.\n−10: Target player mills twenty cards.";

    fn setup() -> (GameScenario, ObjectId) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PostCombatMain);
        scenario.with_library_top(P0, &["Plains", "Island", "Swamp", "Mountain"]);
        scenario.with_library_top(P1, &["Forest", "Island", "Swamp", "Mountain"]);
        scenario.with_mana_pool(
            P0,
            (0..12)
                .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
                .collect(),
        );
        let feldon = scenario
            .add_creature_from_oracle(P0, "Feldon of the Third Path", 2, 3, FELDON)
            .as_legendary()
            .id();
        (scenario, feldon)
    }

    fn graveyard_copier(scenario: &mut GameScenario, clever: bool) -> ObjectId {
        let (name, oracle) = if clever {
            ("Clever Impersonator", CLEVER_IMPERSONATOR)
        } else {
            ("Spark Double", SPARK_DOUBLE)
        };
        scenario
            .add_creature_to_graveyard(P0, name, 0, 0)
            .from_oracle_text(oracle)
            .id()
    }

    fn donor_jace(scenario: &mut GameScenario, owner: PlayerId) -> ObjectId {
        scenario
            .add_planeswalker_from_oracle(owner, "Jace Beleren", "Jace", 3, JACE)
            .as_planeswalker_with_loyalty("Jace", 3)
            .as_legendary()
            .id()
    }

    fn stage_loyalty(runner: &mut GameRunner, donor: ObjectId, current: u32) {
        let object = runner.state_mut().objects.get_mut(&donor).unwrap();
        object.counters.insert(CounterType::Loyalty, current);
        object.loyalty = Some(current);
    }

    fn choose_copy(runner: &mut GameRunner, donor: ObjectId) -> (ObjectId, Vec<GameEvent>) {
        let WaitingFor::ReplacementChoice {
            player, candidates, ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "required token copy offer, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*player, P0);
        assert_eq!(candidates.len(), 2, "copy offer includes explicit decline");
        let entrant = runner
            .state()
            .pending_replacement
            .as_ref()
            .unwrap()
            .proposed
            .affected_object_id()
            .expect("copy replacement identifies the entering token");
        let index = candidates
            .iter()
            .position(|candidate| candidate.source_id == entrant)
            .expect("entrant's copy replacement is offered");
        let mut events = runner
            .act(GameAction::ChooseReplacement { index })
            .unwrap()
            .events;
        let WaitingFor::CopyTargetChoice {
            player,
            source_id,
            valid_targets,
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "required token donor choice, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*player, P0);
        assert_eq!(*source_id, entrant);
        assert!(valid_targets.contains(&donor), "selected donor is offered");
        events.extend(
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(donor)),
                })
                .unwrap()
                .events,
        );
        (entrant, events)
    }

    fn activate_copy(
        runner: &mut GameRunner,
        feldon: ObjectId,
        card: ObjectId,
        donor: ObjectId,
    ) -> ObjectId {
        assert!(!runner.state().objects[&feldon].tapped);
        runner.activate(feldon, 0).target_object(card).resolve();
        assert!(
            runner.state().objects[&feldon].tapped,
            "activation paid its tap cost"
        );
        choose_copy(runner, donor).0
    }

    fn assert_loyalty(runner: &GameRunner, token: ObjectId, name: &str, expected: u32) {
        let entered = runner
            .state()
            .objects
            .get(&token)
            .expect("copied token survives");
        assert_eq!(entered.zone, Zone::Battlefield);
        assert!(entered.is_token);
        assert_eq!(entered.name, name);
        // CR 306.5b + CR 707.2: printed loyalty, not the donor's current counters.
        // CR 306.5c: cached loyalty equals the resulting loyalty-counter count.
        assert_eq!(
            (
                entered.counters.get(&CounterType::Loyalty).copied(),
                entered.loyalty
            ),
            (Some(expected), Some(expected)),
            "printed intrinsic loyalty and copy exceptions share the entry placement"
        );
    }

    fn assert_token_entry_complete(runner: &mut GameRunner) {
        runner.advance_until_stack_empty();
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "token entry did not reach priority: {:?}",
            runner.state().waiting_for
        );
        assert!(runner.state().stack.is_empty());
        assert!(
            runner.state().resolution_stack.is_empty(),
            "token entry stranded a resolution frame"
        );
        // A completed activated-ability carrier may remain cached until the next
        // action's ingress retirement. Empty instruction frames plus the concrete
        // haste and delayed-sacrifice assertions below prove Feldon's completion.
    }

    fn finish_and_sacrifice(runner: &mut GameRunner, tokens: &[ObjectId], donors: &[ObjectId]) {
        assert_token_entry_complete(runner);
        for token in tokens {
            // CR 608.2c: Feldon's later instruction applies to every created token.
            assert!(runner.state().objects[token].has_keyword(&Keyword::Haste));
        }
        runner.advance_to_end_step();
        assert_eq!(runner.state().phase, Phase::End);
        assert_token_entry_complete(runner);
        for token in tokens {
            // CR 603.7c + CR 704.5d: delayed sacrifice follows the created permanent,
            // even when it is a planeswalker; the departed token ceases to exist.
            assert!(
                !runner.state().objects.contains_key(token),
                "delayed sacrifice removes {token:?}"
            );
        }
        for donor in donors {
            assert_eq!(runner.state().objects[donor].zone, Zone::Battlefield);
        }
    }

    #[test]
    fn t1_spark_token_uses_printed_loyalty_and_completes_feldon() {
        let (mut scenario, feldon) = setup();
        let donor = donor_jace(&mut scenario, P0);
        let card = graveyard_copier(&mut scenario, false);
        let mut runner = scenario.build();
        stage_loyalty(&mut runner, donor, 9);
        let token = activate_copy(&mut runner, feldon, card, donor);
        assert_loyalty(&runner, token, "Jace Beleren", 4);
        assert_eq!(
            runner.state().objects[&donor]
                .counters
                .get(&CounterType::Loyalty),
            Some(&9)
        );
        finish_and_sacrifice(&mut runner, &[token], &[donor]);
    }

    #[test]
    fn t2_clever_token_gets_intrinsic_loyalty_from_opponents_donor() {
        let (mut scenario, feldon) = setup();
        let donor = donor_jace(&mut scenario, P1);
        let card = graveyard_copier(&mut scenario, true);
        let mut runner = scenario.build();
        stage_loyalty(&mut runner, donor, 9);
        let token = activate_copy(&mut runner, feldon, card, donor);
        assert_loyalty(&runner, token, "Jace Beleren", 3);
        assert_eq!(
            runner.state().objects[&donor]
                .counters
                .get(&CounterType::Loyalty),
            Some(&9)
        );
        finish_and_sacrifice(&mut runner, &[token], &[donor]);
    }

    fn restore(runner: GameRunner) -> GameRunner {
        use engine::types::game_state::{PersistedGameState, PersistedRestoreFinalization};
        let wire =
            serde_json::to_value(PersistedGameState::capture(runner.state().clone())).unwrap();
        let state = serde_json::from_value::<PersistedGameState>(wire)
            .unwrap()
            .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
            .unwrap()
            .finalize_after_rehydration(|_| Ok(()))
            .unwrap();
        GameRunner::from_state(state)
    }

    fn choose_token_counter_order(
        runner: &mut GameRunner,
        token: ObjectId,
        source: ObjectId,
    ) -> Vec<GameEvent> {
        let WaitingFor::ReplacementChoice {
            player, candidates, ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "required token counter order, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*player, P0);
        assert_eq!(candidates.len(), 2);
        assert_eq!(
            runner
                .state()
                .pending_replacement
                .as_ref()
                .unwrap()
                .proposed
                .affected_object_id(),
            Some(token)
        );
        let index = candidates
            .iter()
            .position(|candidate| candidate.source_id == source)
            .expect("requested counter replacement is offered");
        runner
            .act(GameAction::ChooseReplacement { index })
            .unwrap()
            .events
    }

    fn assert_entry_publication(runner: &GameRunner, token: ObjectId, events: &[GameEvent]) {
        // CR 111.1 + CR 614.12a: each realized token enters once after its entry choices.
        // Check publication/bookkeeping, not downstream observer collection.
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event, GameEvent::ZoneChanged { object_id, record, to: Zone::Battlefield, .. }
                        if *object_id == token && record.object_id == token
                ))
                .count(),
            1,
            "exactly one battlefield-entry event for {token:?}"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event, GameEvent::TokenCreated { object_id, .. } if *object_id == token
                ))
                .count(),
            1,
            "exactly one token-created event for {token:?}"
        );
        assert_eq!(
            runner
                .state()
                .zone_changes_this_turn
                .iter()
                .filter(|record| record.object_id == token && record.to_zone == Zone::Battlefield)
                .count(),
            1,
            "exactly one battlefield-arrival zone ledger row for {token:?}"
        );
        assert_eq!(
            runner
                .state()
                .battlefield_entries_this_turn
                .iter()
                .filter(|record| record.object_id == token)
                .count(),
            1,
            "exactly one battlefield-entry ledger row for {token:?}"
        );
    }

    fn ordered_loyalty_batch(persist: bool) {
        let (mut scenario, feldon) = setup();
        let donor = donor_jace(&mut scenario, P0);
        let card = graveyard_copier(&mut scenario, false);
        let laezel = scenario
            .add_creature_from_oracle(P0, "Lae'zel, Vlaakith's Champion", 3, 3, LAEZEL)
            .as_legendary()
            .id();
        let season = scenario
            .add_enchantment_from_oracle(P0, "Doubling Season", SEASON)
            .id();
        let mut runner = scenario.build();
        stage_loyalty(&mut runner, donor, 9);
        let mut events = runner
            .activate(feldon, 0)
            .target_object(card)
            .resolve()
            .events()
            .to_vec();
        assert!(
            runner.state().objects[&feldon].tapped,
            "activation paid its tap cost"
        );
        let (first, copy_events) = choose_copy(&mut runner, donor);
        events.extend(copy_events);
        if persist {
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { player: P0, .. }
            ));
            runner = restore(runner);
        }
        // CR 616.1 + CR 616.1f: (printed 3 + Spark 1 + Lae'zel 1) × 2.
        events.extend(choose_token_counter_order(&mut runner, first, laezel));
        assert_loyalty(&runner, first, "Jace Beleren", 10);
        let (second, copy_events) = choose_copy(&mut runner, donor);
        events.extend(copy_events);
        assert_ne!(first, second, "Season creates two distinct entrants");
        if persist {
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { player: P0, .. }
            ));
            runner = restore(runner);
        }
        // CR 616.1 + CR 616.1f: (printed 3 + Spark 1) × 2 + Lae'zel 1.
        events.extend(choose_token_counter_order(&mut runner, second, season));
        assert_loyalty(&runner, second, "Jace Beleren", 9);
        assert_token_entry_complete(&mut runner);
        assert_entry_publication(&runner, first, &events);
        assert_entry_publication(&runner, second, &events);
        assert_eq!(
            runner.state().objects[&donor]
                .counters
                .get(&CounterType::Loyalty),
            Some(&9)
        );
        finish_and_sacrifice(&mut runner, &[first, second], &[donor, feldon]);
    }

    #[test]
    fn t4_opposite_counter_orders_publish_and_finish_both_tokens_once() {
        ordered_loyalty_batch(false);
    }

    #[test]
    fn p1_both_counter_pauses_survive_persisted_restore() {
        ordered_loyalty_batch(true);
    }

    #[test]
    fn altar_collects_each_token_entry_across_counter_ordering_pauses() {
        let db = crate::support::shared_card_db().expect("integration card database");
        let (mut scenario, feldon) = setup();
        let card = graveyard_copier(&mut scenario, false);
        let scales = scenario
            .add_enchantment_from_oracle(P0, "Hardened Scales", SCALES)
            .id();
        let season = scenario
            .add_enchantment_from_oracle(P0, "Doubling Season", SEASON)
            .id();
        scenario.add_real_card(P0, "Altar of the Brood", Zone::Battlefield, db);
        let mut runner = scenario.build();
        let before = runner.state().players[1].library.len();
        assert!(before >= 4);
        let mut events = runner
            .activate(feldon, 0)
            .target_object(card)
            .resolve()
            .events()
            .to_vec();
        let (first, copy_events) = choose_copy(&mut runner, feldon);
        events.extend(copy_events);
        events.extend(choose_token_counter_order(&mut runner, first, scales));
        let (second, copy_events) = choose_copy(&mut runner, feldon);
        events.extend(copy_events);
        assert_ne!(first, second, "Season creates two distinct entrants");
        events.extend(choose_token_counter_order(&mut runner, second, season));
        assert_entry_publication(&runner, first, &events);
        assert_entry_publication(&runner, second, &events);
        runner.advance_until_stack_empty();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(runner.state().stack.is_empty());
        // CR 603.2 + CR 603.3b: each entrant contributes one Altar trigger,
        // even when identical triggers share a batched stack entry.
        assert_eq!(
            runner.state().players[1].library.len(),
            before - 2,
            "Altar mills exactly once for each entrant"
        );
    }

    #[test]
    fn counter_ordering_answers_do_not_split_one_or_more_token_observers() {
        counter_ordering_observer_draws(false);
    }

    #[test]
    fn counter_ordering_answers_do_not_spend_parked_one_or_more_firing_limits() {
        counter_ordering_observer_draws(true);
    }

    fn counter_ordering_observer_draws(with_firing_limit: bool) {
        let db = crate::support::shared_card_db().expect("integration card database");
        let (mut scenario, feldon) = setup();
        let card = graveyard_copier(&mut scenario, false);
        let scales = scenario
            .add_enchantment_from_oracle(P0, "Hardened Scales", SCALES)
            .id();
        let season = scenario
            .add_enchantment_from_oracle(P0, "Doubling Season", SEASON)
            .id();
        scenario.add_real_card(P0, "Altar of the Brood", Zone::Battlefield, db);
        let observer = if with_firing_limit {
            scenario
                .add_enchantment_from_oracle(
                    P0,
                    "Tocasia's Welcome",
                    "Whenever one or more creatures you control with mana value 3 or less enter, draw a card. This ability triggers only once each turn.",
                )
                .id()
        } else {
            scenario
                .add_creature_from_oracle(
                    P0,
                    "Mister Fantastic, Reed Richards",
                    2,
                    4,
                    "Reach\nWhenever one or more tokens you control enter, you may draw a card.",
                )
                .as_legendary()
                .id()
        };
        let later_entry = with_firing_limit.then(|| {
            scenario
                .add_spell_to_hand_from_oracle(
                    P0,
                    "Raise the Alarm",
                    true,
                    "Create two 1/1 white Soldier creature tokens.",
                )
                .id()
        });
        let mut runner = scenario.build();
        let library_before = runner.state().players[1].library.len();
        let hand_before = runner.state().players[0].hand.len();
        runner.activate(feldon, 0).target_object(card).resolve();
        let (first, _) = choose_copy(&mut runner, feldon);
        choose_token_counter_order(&mut runner, first, scales);
        let (second, _) = choose_copy(&mut runner, feldon);
        assert_ne!(first, second, "Season creates two distinct entrants");
        choose_token_counter_order(&mut runner, second, season);
        for _ in 0..3 {
            runner.advance_until_stack_empty();
            let WaitingFor::OptionalEffectChoice {
                player, source_id, ..
            } = &runner.state().waiting_for
            else {
                break;
            };
            assert_eq!(*player, P0);
            assert_eq!(
                *source_id, observer,
                "only the token observer offers a draw"
            );
            runner
                .act(GameAction::DecideOptionalEffect { accept: true })
                .unwrap();
        }
        assert_token_entry_complete(&mut runner);
        assert_eq!(
            runner.state().players[1].library.len(),
            library_before - 2,
            "Altar still mills exactly once for each entrant"
        );
        let drawn = runner.state().players[0].hand.len() - hand_before;
        // CR 603.2c + CR 603.6a: the two entrants are one entry event, so a
        // "one or more" observer triggers once for it, after the last entrant.
        assert_ne!(drawn, 2, "one batch must not produce two observer draws");
        assert_eq!(
            drawn, 1,
            "positive reach guard: the observer actually triggers and its draw resolves"
        );
        if let Some(spell) = later_entry {
            let outcome = runner.cast(spell).resolve();
            outcome.assert_hand_drawn(P0, 0);
            assert!(matches!(
                outcome.final_waiting_for(),
                WaitingFor::Priority { .. }
            ));
            assert_token_entry_complete(&mut runner);
            assert_eq!(
                runner
                    .state()
                    .objects
                    .values()
                    .filter(|object| object.is_token && object.zone == Zone::Battlefield)
                    .count(),
                6,
                "four later qualifying entrants do not bypass Welcome's spent turn limit"
            );
        }
    }

    // Oracle text below is from the generated card data (MTGJSON), reminder text omitted.
    const BODY_DOUBLE: &str =
        "You may have this creature enter as a copy of any creature card in a graveyard.";
    const HUSHBRINGER: &str =
        "Flying, lifelink\nCreatures entering or dying don't cause abilities to trigger.";
    const SOUL_WARDEN: &str = "Whenever another creature enters, you gain 1 life.";
    const SERVANT_OF_THE_SCALE: &str = "This creature enters with a +1/+1 counter on it.\nWhen this creature dies, put X +1/+1 counters on target creature you control, where X is the number of +1/+1 counters on this creature.";
    const ASCENDANT_EVINCAR: &str =
        "Flying\nOther black creatures get +1/+1.\nNonblack creatures get -1/-1.";
    const ELEMENTAL_BOND: &str =
        "Whenever a creature you control with power 3 or greater enters, draw a card.";
    const MOON_GIRL: &str = "Whenever you draw your second card each turn, until end of turn, Moon Girl and Devil Dinosaur's base power and toughness become 6/6 and they gain trample.\nWhenever an artifact you control enters, draw a card. This ability triggers only once each turn.";
    const TOCASIAS_WELCOME: &str = "Whenever one or more creatures you control with mana value 3 or less enter, draw a card. This ability triggers only once each turn.";

    /// The board every later-co-entrant row shares. Feldon targets a Body Double
    /// card in the graveyard; Doubling Season makes two tokens of it, and with
    /// Hardened Scales a first copy that enters with +1/+1 counters stops on a
    /// CR 616.1f ordering choice. Its answer puts that token onto the battlefield
    /// and then stops again on the second token's copy offer, so the first
    /// entrant is on the battlefield while its co-entrant is still entering.
    struct BodyDoubleBoard {
        scenario: GameScenario,
        feldon: ObjectId,
        body_double: ObjectId,
        scales: ObjectId,
        season: ObjectId,
    }

    fn body_double_board() -> BodyDoubleBoard {
        let (mut scenario, feldon) = setup();
        let body_double = scenario
            .add_creature_to_graveyard(P0, "Body Double", 0, 0)
            .with_subtypes(vec!["Shapeshifter"])
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 4,
            })
            .from_oracle_text(BODY_DOUBLE)
            .id();
        let scales = scenario
            .add_enchantment_from_oracle(P0, "Hardened Scales", SCALES)
            .id();
        let season = scenario
            .add_enchantment_from_oracle(P0, "Doubling Season", SEASON)
            .id();
        BodyDoubleBoard {
            scenario,
            feldon,
            body_double,
            scales,
            season,
        }
    }

    fn graveyard_triskelion(scenario: &mut GameScenario) -> ObjectId {
        scenario
            .add_creature_to_graveyard(P0, "Triskelion", 1, 1)
            .as_artifact_creature()
            .with_subtypes(vec!["Construct"])
            .with_mana_cost(ManaCost::Cost {
                shards: vec![],
                generic: 6,
            })
            .from_oracle_text(TRISKELION)
            .id()
    }

    fn graveyard_hushbringer(scenario: &mut GameScenario) -> ObjectId {
        scenario
            .add_creature_to_graveyard(P0, "Hushbringer", 1, 2)
            .with_subtypes(vec!["Faerie"])
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 1,
            })
            .from_oracle_text_with_keywords(&["Flying", "Lifelink"], HUSHBRINGER)
            .id()
    }

    fn graveyard_servant_of_the_scale(scenario: &mut GameScenario) -> ObjectId {
        scenario
            .add_creature_to_graveyard(P0, "Servant of the Scale", 0, 0)
            .with_subtypes(vec!["Human", "Soldier"])
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 0,
            })
            .from_oracle_text(SERVANT_OF_THE_SCALE)
            .id()
    }

    /// Activate Feldon and bring the first Body Double token in as a copy of
    /// `donor`, which enters with +1/+1 counters. Returns that token, on the
    /// battlefield, with the second token's copy offer open.
    fn enter_first_copy_before_second_offer(
        runner: &mut GameRunner,
        feldon: ObjectId,
        body_double: ObjectId,
        donor: ObjectId,
        first_counter_replacement: ObjectId,
    ) -> ObjectId {
        runner
            .activate(feldon, 0)
            .target_object(body_double)
            .resolve();
        let (first, _) = choose_copy(runner, donor);
        choose_token_counter_order(runner, first, first_counter_replacement);
        assert_first_entered_with_second_offer_open(runner, first, donor);
        first
    }

    /// Reach guard: the first entrant has entered and the entry event is not
    /// finished. Every later-co-entrant row depends on this exact pause.
    fn assert_first_entered_with_second_offer_open(
        runner: &GameRunner,
        first: ObjectId,
        donor: ObjectId,
    ) {
        let entered = &runner.state().objects[&first];
        assert_eq!(entered.zone, Zone::Battlefield);
        assert!(entered.is_token);
        assert_eq!(entered.name, runner.state().objects[&donor].name);
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ),
            "second entrant's copy offer is open: {:?}",
            runner.state().waiting_for
        );
    }

    fn graveyard_hill_giant(scenario: &mut GameScenario) -> ObjectId {
        // Hill Giant is a vanilla 3/3 red Giant with no entry counters.
        scenario
            .add_creature_to_graveyard(P0, "Hill Giant", 3, 3)
            .with_subtypes(vec!["Giant"])
            .with_color(vec![ManaColor::Red])
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 3,
            })
            .id()
    }

    /// Soul Warden's life gain for one activation whose tokens copy `first`
    /// and then `second`. `persist` round-trips the game through the persisted
    /// wire at the pause between the two entrants.
    fn warden_life_for_copies(first: FirstCopy, second: SecondCopy, persist: bool) -> i32 {
        let BodyDoubleBoard {
            mut scenario,
            feldon,
            body_double,
            scales,
            season,
        } = body_double_board();
        let triskelion = graveyard_triskelion(&mut scenario);
        let hushbringer = graveyard_hushbringer(&mut scenario);
        let hill_giant = graveyard_hill_giant(&mut scenario);
        scenario.add_creature_from_oracle(P0, "Soul Warden", 1, 1, SOUL_WARDEN);
        let mut runner = scenario.build();
        let life = runner.life(P0);
        let first = match first {
            // The counter-ordering answer publishes this entrant.
            FirstCopy::Triskelion => enter_first_copy_before_second_offer(
                &mut runner,
                feldon,
                body_double,
                triskelion,
                scales,
            ),
            // No entry counters: the copy-choice answer publishes this entrant.
            FirstCopy::HillGiant => {
                runner
                    .activate(feldon, 0)
                    .target_object(body_double)
                    .resolve();
                let (token, _) = choose_copy(&mut runner, hill_giant);
                assert_first_entered_with_second_offer_open(&runner, token, hill_giant);
                token
            }
        };
        assert_eq!(runner.life(P0), life, "nothing has resolved at the pause");
        if persist {
            runner = restore(runner);
        }
        let second_token = match second {
            SecondCopy::Hushbringer => {
                let (token, _) = choose_copy(&mut runner, hushbringer);
                token
            }
            SecondCopy::Triskelion => {
                let (token, _) = choose_copy(&mut runner, triskelion);
                choose_token_counter_order(&mut runner, token, season);
                token
            }
        };
        assert_ne!(first, second_token, "Season creates two distinct entrants");
        assert_token_entry_complete(&mut runner);
        let entered = &runner.state().objects[&second_token];
        assert_eq!(entered.zone, Zone::Battlefield);
        assert_eq!(
            entered.name,
            match second {
                SecondCopy::Hushbringer => "Hushbringer",
                SecondCopy::Triskelion => "Triskelion",
            }
        );
        runner.life(P0) - life
    }

    #[derive(Clone, Copy)]
    enum FirstCopy {
        Triskelion,
        HillGiant,
    }

    #[derive(Clone, Copy)]
    enum SecondCopy {
        Hushbringer,
        Triskelion,
    }

    /// CR 603.6a: each time an event puts one or more permanents onto the
    /// battlefield, all permanents on the battlefield, including the newcomers,
    /// are checked for enters triggers that match the event. Both tokens are one
    /// event, so Hushbringer ("Creatures entering or dying don't cause abilities
    /// to trigger.") is on the battlefield for the check and Soul Warden
    /// ("Whenever another creature enters, you gain 1 life.") triggers for
    /// neither token, although the Triskelion copy entered an answer earlier.
    #[test]
    fn later_co_entrant_suppressor_governs_the_whole_token_entry_event() {
        assert_eq!(
            warden_life_for_copies(FirstCopy::Triskelion, SecondCopy::Triskelion, false),
            2,
            "positive control: without a suppressor Soul Warden sees both entrants"
        );
        assert_eq!(
            warden_life_for_copies(FirstCopy::Triskelion, SecondCopy::Hushbringer, false),
            0,
            "the completed entry event includes Hushbringer, so no entrant triggers Soul Warden"
        );
    }

    /// The same rows with the game saved and restored at the pause between the
    /// two entrants: the first entrant's unmatched entry survives the wire and
    /// is still judged against the completed event.
    #[test]
    fn later_co_entrant_suppressor_survives_persisted_restore_at_the_pause() {
        assert_eq!(
            warden_life_for_copies(FirstCopy::Triskelion, SecondCopy::Triskelion, true),
            2,
            "positive control: the first entrant's entry is not lost across restore"
        );
        assert_eq!(
            warden_life_for_copies(FirstCopy::Triskelion, SecondCopy::Hushbringer, true),
            0,
            "the restored game still checks the first entrant against the completed event"
        );
    }

    /// Cards Elemental Bond draws for one activation whose first token copies
    /// Servant of the Scale and whose second copies `second`.
    fn bond_draws_for_second_copy(
        second: impl FnOnce(&mut GameScenario) -> ObjectId,
    ) -> (usize, i32) {
        let BodyDoubleBoard {
            mut scenario,
            feldon,
            body_double,
            scales: _,
            season,
        } = body_double_board();
        let servant = graveyard_servant_of_the_scale(&mut scenario);
        let second = second(&mut scenario);
        scenario.add_enchantment_from_oracle(P0, "Elemental Bond", ELEMENTAL_BOND);
        let mut runner = scenario.build();
        let hand = runner.state().players[0].hand.len();
        // Season first, then Scales: one counter doubled to two, plus one.
        let first =
            enter_first_copy_before_second_offer(&mut runner, feldon, body_double, servant, season);
        assert_eq!(
            runner.state().objects[&first]
                .counters
                .get(&CounterType::Plus1Plus1),
            Some(&3)
        );
        assert_eq!(runner.state().objects[&first].power, Some(3));
        choose_copy(&mut runner, second);
        assert_token_entry_complete(&mut runner);
        (
            runner.state().players[0].hand.len() - hand,
            runner.state().objects[&first]
                .power
                .expect("first entrant is still a creature"),
        )
    }

    /// CR 603.6b + CR 611.3c: continuous effects that modify a permanent's
    /// characteristics apply as it enters, before determining whether it causes
    /// an ability to trigger. Ascendant Evincar ("Nonblack creatures get -1/-1.")
    /// enters in the same event as the 3/3 Servant of the Scale copy, so that
    /// copy enters as a 2/2 and Elemental Bond ("Whenever a creature you control
    /// with power 3 or greater enters, draw a card.") triggers only for the 3/3
    /// Evincar.
    #[test]
    fn later_co_entrant_characteristic_modifier_governs_the_earlier_entrant() {
        let (draws, first_power) = bond_draws_for_second_copy(graveyard_hill_giant);
        assert_eq!(first_power, 3);
        assert_eq!(
            draws, 2,
            "positive control: with no modifier both 3-power entrants draw"
        );
        let (draws, first_power) = bond_draws_for_second_copy(|scenario| {
            scenario
                .add_creature_to_graveyard(P0, "Ascendant Evincar", 3, 3)
                .as_legendary()
                .with_subtypes(vec!["Phyrexian", "Vampire", "Noble"])
                .with_color(vec![ManaColor::Black])
                .with_mana_cost(ManaCost::Cost {
                    shards: vec![ManaCostShard::Black, ManaCostShard::Black],
                    generic: 4,
                })
                .from_oracle_text(ASCENDANT_EVINCAR)
                .id()
        });
        assert_eq!(first_power, 2, "Evincar shrinks the nonblack first entrant");
        assert_eq!(
            draws, 1,
            "only Evincar enters with power 3 or greater in the completed event"
        );
    }

    /// CR 603.2c + CR 603.6a: a "triggers only once each turn" ability is spent
    /// by an event that triggers it, and the suppressed entry event triggers
    /// nothing. Moon Girl and Devil Dinosaur ("Whenever an artifact you control
    /// enters, draw a card. This ability triggers only once each turn.") must
    /// not draw for Feldon's artifact tokens when the second is Hushbringer,
    /// and must still be able to trigger for a later artifact that turn.
    #[test]
    fn suppressed_token_entry_event_does_not_spend_a_once_each_turn_limit() {
        let BodyDoubleBoard {
            mut scenario,
            feldon,
            body_double,
            scales,
            season: _,
        } = body_double_board();
        let triskelion = graveyard_triskelion(&mut scenario);
        let hushbringer = graveyard_hushbringer(&mut scenario);
        scenario
            .add_creature_from_oracle(P0, "Moon Girl and Devil Dinosaur", 2, 2, MOON_GIRL)
            .as_legendary();
        // Hushbringer stops creatures entering from causing triggers; a
        // noncreature artifact still triggers Moon Girl.
        let spellbook = scenario
            .add_artifact_to_hand_from_oracle(P0, "Spellbook", "You have no maximum hand size.")
            .id();
        let mut runner = scenario.build();
        let library = runner.state().players[0].library.len();
        let first = enter_first_copy_before_second_offer(
            &mut runner,
            feldon,
            body_double,
            triskelion,
            scales,
        );
        assert!(
            runner.state().objects[&first]
                .card_types
                .core_types
                .contains(&engine::types::card_type::CoreType::Artifact),
            "the first entrant is an artifact Moon Girl would see"
        );
        choose_copy(&mut runner, hushbringer);
        assert_token_entry_complete(&mut runner);
        assert_eq!(
            runner.state().players[0].library.len(),
            library,
            "the suppressed entry event draws nothing"
        );
        let outcome = runner.cast(spellbook).resolve();
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
        assert_token_entry_complete(&mut runner);
        assert_eq!(runner.state().objects[&spellbook].zone, Zone::Battlefield);
        assert_eq!(
            runner.state().players[0].library.len(),
            library - 1,
            "the once-each-turn trigger was still available for the later artifact"
        );
    }

    /// CR 603.2c: a "one or more" ability triggers once for an event with
    /// several occurrences. Tocasia's Welcome ("Whenever one or more creatures
    /// you control with mana value 3 or less enter, draw a card. This ability
    /// triggers only once each turn.") must see the mana value 1 Servant of the
    /// Scale copy although the entrant that completes the event, a Triskelion
    /// copy, has mana value 6, and that one trigger spends the turn's limit.
    #[test]
    fn one_or_more_observer_sees_a_qualifying_entrant_that_is_not_the_last() {
        let BodyDoubleBoard {
            mut scenario,
            feldon,
            body_double,
            scales,
            season,
        } = body_double_board();
        let servant = graveyard_servant_of_the_scale(&mut scenario);
        let triskelion = graveyard_triskelion(&mut scenario);
        scenario.add_enchantment_from_oracle(P0, "Tocasia's Welcome", TOCASIAS_WELCOME);
        let later_entry = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Raise the Alarm",
                true,
                "Create two 1/1 white Soldier creature tokens.",
            )
            .id();
        let mut runner = scenario.build();
        let library = runner.state().players[0].library.len();
        enter_first_copy_before_second_offer(&mut runner, feldon, body_double, servant, scales);
        let (second, _) = choose_copy(&mut runner, triskelion);
        choose_token_counter_order(&mut runner, second, season);
        assert_token_entry_complete(&mut runner);
        assert_eq!(
            runner.state().players[0].library.len(),
            library - 1,
            "Welcome draws once for the event's qualifying entrant"
        );
        let outcome = runner.cast(later_entry).resolve();
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
        assert_token_entry_complete(&mut runner);
        assert_eq!(
            runner
                .state()
                .objects
                .values()
                .filter(|object| object.is_token && object.zone == Zone::Battlefield)
                .count(),
            6,
            "positive reach guard: Raise the Alarm's Season-doubled qualifying tokens entered"
        );
        assert_eq!(
            runner.state().players[0].library.len(),
            library - 1,
            "the limit was spent by the completed entry event"
        );
    }

    /// A first copy with no entry counters is published by the copy-choice
    /// answer itself, which then stops on the second token's copy offer. That
    /// entry is still part of the one event (CR 603.6a): Soul Warden sees it
    /// when nothing suppresses the event, and does not when the second copy is
    /// Hushbringer.
    #[test]
    fn copy_choice_answer_keeps_its_entrant_for_the_completed_event() {
        assert_eq!(
            warden_life_for_copies(FirstCopy::HillGiant, SecondCopy::Triskelion, false),
            2,
            "the entrant published by the copy-choice answer is not lost"
        );
        assert_eq!(
            warden_life_for_copies(FirstCopy::HillGiant, SecondCopy::Hushbringer, false),
            0,
            "and it is checked against the completed event, which Hushbringer suppresses"
        );
    }

    const BESTIAL_MENACE: &str = "Create a 1/1 green Snake creature token, a 2/2 green Wolf creature token, and a 3/3 green Elephant creature token.";
    const METALLIC_MIMIC: &str = "As this creature enters, choose a creature type.\nThis creature is the chosen type in addition to its other types.\nEach other creature you control of the chosen type enters with an additional +1/+1 counter on it.";
    const BRANCHING_EVOLUTION: &str = "If one or more +1/+1 counters would be put on a creature you control, twice that many +1/+1 counters are put on that creature instead.";

    /// CR 603.2c + CR 603.6a: Bestial Menace's three tokens are one entry event,
    /// so Mister Fantastic ("Whenever one or more tokens you control enter, you
    /// may draw a card.") triggers once for it. Metallic Mimic naming Elephant
    /// gives only the Elephant an entry counter; Hardened Scales and Branching
    /// Evolution make that a CR 616.1f ordering choice, so the resolving spell
    /// itself puts the Snake and the Wolf onto the battlefield and then stops.
    #[test]
    fn resolving_spell_that_stops_on_its_last_token_matches_the_event_once() {
        let (mut scenario, _feldon) = setup();
        let scales = scenario
            .add_enchantment_from_oracle(P0, "Hardened Scales", SCALES)
            .id();
        scenario.add_enchantment_from_oracle(P0, "Branching Evolution", BRANCHING_EVOLUTION);
        let mimic = scenario
            .add_creature_from_oracle(P0, "Metallic Mimic", 2, 1, METALLIC_MIMIC)
            .as_artifact_creature()
            .with_subtypes(vec!["Shapeshifter"])
            .id();
        let fantastic = scenario
            .add_creature_from_oracle(
                P0,
                "Mister Fantastic, Reed Richards",
                2,
                4,
                "Reach\nWhenever one or more tokens you control enter, you may draw a card.",
            )
            .as_legendary()
            .id();
        let menace = scenario
            .add_spell_to_hand_from_oracle(P0, "Bestial Menace", false, BESTIAL_MENACE)
            .id();
        let mut runner = scenario.build();
        runner
            .state_mut()
            .objects
            .get_mut(&mimic)
            .unwrap()
            .chosen_attributes = vec![ChosenAttribute::CreatureType("Elephant".to_string())];
        let hand = runner.state().players[0].hand.len();
        let tokens_named = |runner: &GameRunner, name: &str| {
            runner
                .state()
                .objects
                .values()
                .filter(|object| {
                    object.is_token && object.zone == Zone::Battlefield && object.name == name
                })
                .count()
        };
        runner.cast(menace).resolve();
        // Reach guard: two entrants have entered and the third is still
        // entering, stopped on the order of its entry counter's replacements.
        assert_eq!(tokens_named(&runner, "Snake"), 1);
        assert_eq!(tokens_named(&runner, "Wolf"), 1);
        let elephant = runner
            .state()
            .objects
            .values()
            .find(|object| object.is_token && object.name == "Elephant")
            .map(|object| object.id)
            .expect("the Elephant token is being created");
        assert_eq!(
            runner
                .state()
                .pending_replacement
                .as_ref()
                .expect("a replacement is parked")
                .proposed
                .affected_object_id(),
            Some(elephant)
        );
        assert_eq!(
            runner.state().objects[&elephant]
                .counters
                .get(&CounterType::Plus1Plus1),
            None,
            "the Elephant's entry counter has not been placed"
        );
        let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
            panic!(
                "required the Elephant's counter order, got {:?}",
                runner.state().waiting_for
            );
        };
        let index = candidates
            .iter()
            .position(|candidate| candidate.source_id == scales)
            .expect("Hardened Scales is offered");
        runner.act(GameAction::ChooseReplacement { index }).unwrap();
        let mut offers = 0;
        for _ in 0..4 {
            runner.advance_until_stack_empty();
            let WaitingFor::OptionalEffectChoice { source_id, .. } = &runner.state().waiting_for
            else {
                break;
            };
            assert_eq!(*source_id, fantastic, "only Mister Fantastic offers a draw");
            offers += 1;
            runner
                .act(GameAction::DecideOptionalEffect { accept: true })
                .unwrap();
        }
        assert_token_entry_complete(&mut runner);
        // Scales first, then Branching Evolution: (1 + 1) doubled.
        assert_eq!(
            runner.state().objects[&elephant]
                .counters
                .get(&CounterType::Plus1Plus1),
            Some(&4)
        );
        assert_eq!(offers, 1, "one entry event offers one draw");
        assert_eq!(
            runner.state().players[0].hand.len(),
            hand,
            "the one accepted draw replaces the cast card"
        );
    }

    #[test]
    fn t8_triskelion_self_counters_and_spark_share_one_scales_application() {
        let (mut scenario, feldon) = setup();
        let donor = scenario
            .add_creature_from_oracle(P0, "Triskelion", 1, 1, TRISKELION)
            .as_artifact_creature()
            .with_subtypes(vec!["Construct"])
            .id();
        scenario.add_enchantment_from_oracle(P0, "Hardened Scales", SCALES);
        let card = graveyard_copier(&mut scenario, false);
        let mut runner = scenario.build();
        let token = activate_copy(&mut runner, feldon, card, donor);
        let entered = &runner.state().objects[&token];
        assert_eq!(entered.name, "Triskelion");
        assert!(entered.is_token);
        assert_eq!(entered.zone, Zone::Battlefield);
        // CR 614.12 + CR 614.1c + CR 122.6 + CR 614.5:
        // copied Triskelion 3 + Spark 1, then Scales once, is five counters.
        assert_eq!(entered.counters.get(&CounterType::Plus1Plus1), Some(&5));
        finish_and_sacrifice(&mut runner, &[token], &[donor]);
    }

    #[test]
    fn t3_laezel_modifies_intrinsic_and_exception_loyalty_once() {
        let (mut scenario, feldon) = setup();
        let donor = donor_jace(&mut scenario, P0);
        scenario
            .add_creature_from_oracle(P0, "Lae'zel, Vlaakith's Champion", 3, 3, LAEZEL)
            .as_legendary();
        let card = graveyard_copier(&mut scenario, false);
        let mut runner = scenario.build();
        stage_loyalty(&mut runner, donor, 9);
        let token = activate_copy(&mut runner, feldon, card, donor);
        // CR 614.1c + CR 122.6 + CR 614.5: one merged (3 + 1) + 1 placement.
        assert_loyalty(&runner, token, "Jace Beleren", 5);
        finish_and_sacrifice(&mut runner, &[token], &[donor]);
    }

    fn choose_color(runner: &mut GameRunner, token: ObjectId, color: &str) {
        let WaitingFor::NamedChoice {
            player,
            source,
            options,
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "entry color must precede the next token, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*player, P0);
        assert_eq!(
            source.as_ref().unwrap().prompt.identity.reference.object_id,
            token
        );
        let choice = options
            .iter()
            .find(|option| option.eq_ignore_ascii_case(color))
            .unwrap()
            .clone();
        runner.act(GameAction::ChooseOption { choice }).unwrap();
    }

    #[test]
    fn t7_counter_witness_finishes_each_color_before_the_next_token() {
        let (mut scenario, feldon) = setup();
        let donor = scenario
            .add_creature_from_oracle(P0, "Painter's Servant", 1, 3, PAINTERS_SERVANT)
            .as_artifact_creature()
            .with_subtypes(vec!["Scarecrow"])
            .id();
        let laezel = scenario
            .add_creature_from_oracle(P0, "Lae'zel, Vlaakith's Champion", 3, 3, LAEZEL)
            .as_legendary()
            .id();
        let season = scenario
            .add_enchantment_from_oracle(P0, "Doubling Season", SEASON)
            .id();
        let card = graveyard_copier(&mut scenario, false);
        let mut runner = scenario.build();
        let first = activate_copy(&mut runner, feldon, card, donor);
        choose_counter_replacement(&mut runner, laezel);
        // CR 614.12a + CR 616.1: settle this entrant's required color before publication/batch resume.
        choose_color(&mut runner, first, "blue");
        let (second, _) = choose_copy(&mut runner, donor);
        assert_ne!(first, second);
        choose_counter_replacement(&mut runner, season);
        choose_color(&mut runner, second, "red");
        runner.advance_until_stack_empty();
        // CR 608.2c: both created tokens receive Feldon's rider before priority.
        assert!(
            runner.state().objects[&second].has_keyword(&Keyword::Haste),
            "second token must receive Feldon's haste before entry completion"
        );
        assert!(
            runner.state().objects[&first].has_keyword(&Keyword::Haste),
            "first token must receive Feldon's haste before entry completion"
        );
        assert_token_entry_complete(&mut runner);
        for (token, color, counters) in [(first, ManaColor::Blue, 4), (second, ManaColor::Red, 3)] {
            let entered = &runner.state().objects[&token];
            assert_eq!(entered.name, "Painter's Servant");
            assert_eq!(entered.chosen_color(), Some(color));
            assert_eq!(
                entered.counters.get(&CounterType::Plus1Plus1),
                Some(&counters)
            );
        }
        finish_and_sacrifice(&mut runner, &[first, second], &[donor]);
    }

    #[test]
    fn t5a_decline_enters_and_dies_once_without_a_copy() {
        let (mut scenario, feldon) = setup();
        let donor = donor_jace(&mut scenario, P0);
        let card = graveyard_copier(&mut scenario, false);
        scenario.add_artifact_from_oracle(
            P0,
            "Altar of the Brood",
            "Whenever another permanent you control enters, each opponent mills a card.",
        );
        let mut runner = scenario.build();
        let library: Vec<_> = runner.state().players[1]
            .library
            .iter()
            .take(2)
            .copied()
            .collect();
        runner.activate(feldon, 0).target_object(card).resolve();
        let WaitingFor::ReplacementChoice {
            player, candidates, ..
        } = &runner.state().waiting_for
        else {
            panic!("decline requires the optional token-copy offer");
        };
        assert_eq!(*player, P0);
        assert_eq!(candidates.len(), 2);
        let token = runner
            .state()
            .pending_replacement
            .as_ref()
            .unwrap()
            .proposed
            .affected_object_id()
            .unwrap();
        assert!(candidates
            .iter()
            .any(|candidate| candidate.source_id == token));
        runner
            .act(GameAction::ChooseReplacement { index: 1 })
            .unwrap();
        assert_token_entry_complete(&mut runner);
        // CR 704.5f + CR 704.5d: the uncopied 0/0 enters, dies, and ceases to exist.
        assert!(!runner.state().objects.contains_key(&token));
        // CR 603.6a: its single entry is still observed.
        assert_eq!(runner.state().objects[&library[0]].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&library[1]].zone, Zone::Library);
        assert_eq!(runner.state().objects[&donor].zone, Zone::Battlefield);
    }

    fn announce_feldon(runner: &mut GameRunner, feldon: ObjectId, card: ObjectId) {
        runner
            .act(GameAction::ActivateAbility {
                source_id: feldon,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::TargetSelection { player: P0, .. }
            ),
            "Feldon offers the two graveyard targets: {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(card)),
            })
            .unwrap();
        // Pool-funded activation can finish payment here or expose its payment window.
        if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
            runner.act(GameAction::PassPriority).unwrap();
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ));
        assert_eq!(
            runner.state().stack.len(),
            1,
            "activation is committed before the response"
        );
        assert!(runner.state().objects[&feldon].tapped);
    }

    fn murder(scenario: &mut GameScenario) -> ObjectId {
        scenario
            .add_spell_to_hand_from_oracle(P0, "Murder", true, "Destroy target creature.")
            .id()
    }

    #[test]
    fn t5b_source_removed_leaves_no_legal_spark_copy_source() {
        let (mut scenario, feldon) = setup();
        let opposing_donor = donor_jace(&mut scenario, P1);
        let card = graveyard_copier(&mut scenario, false);
        graveyard_copier(&mut scenario, false);
        let response = murder(&mut scenario);
        scenario.add_artifact_from_oracle(
            P0,
            "Altar of the Brood",
            "Whenever another permanent you control enters, each opponent mills a card.",
        );
        let mut runner = scenario.build();
        let library: Vec<_> = runner.state().players[1]
            .library
            .iter()
            .take(2)
            .copied()
            .collect();
        announce_feldon(&mut runner, feldon, card);
        runner.cast(response).target_object(feldon).resolve();
        // CR 113.7a: the activation still creates its token after Feldon leaves.
        assert_eq!(runner.state().objects[&feldon].zone, Zone::Graveyard);
        assert_token_entry_complete(&mut runner);
        assert_eq!(runner.state().objects[&library[0]].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&library[1]].zone, Zone::Library);
        assert_eq!(
            runner.state().objects[&opposing_donor].zone,
            Zone::Battlefield
        );
        assert!(!runner
            .state()
            .objects
            .values()
            .any(|object| object.is_token));
    }

    #[test]
    fn r_source_left_still_copies_and_finishes_feldon() {
        let (mut scenario, feldon) = setup();
        let donor = donor_jace(&mut scenario, P0);
        let card = graveyard_copier(&mut scenario, false);
        graveyard_copier(&mut scenario, false);
        let response = murder(&mut scenario);
        let mut runner = scenario.build();
        announce_feldon(&mut runner, feldon, card);
        runner.cast(response).target_object(feldon).resolve();
        assert_eq!(runner.state().objects[&feldon].zone, Zone::Graveyard);
        // CR 113.7a: the independent activation may still create and copy a token.
        let (token, _) = choose_copy(&mut runner, donor);
        assert_loyalty(&runner, token, "Jace Beleren", 4);
        finish_and_sacrifice(&mut runner, &[token], &[donor]);
    }

    #[test]
    fn r_target_exiled_prevents_creation_and_cremate_draws() {
        let (mut scenario, feldon) = setup();
        let donor = donor_jace(&mut scenario, P0);
        let card = graveyard_copier(&mut scenario, false);
        graveyard_copier(&mut scenario, false);
        let response = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Cremate",
                true,
                "Exile target card from a graveyard.\nDraw a card.",
            )
            .id();
        let mut runner = scenario.build();
        let drawn = runner.state().players[0].library[0];
        announce_feldon(&mut runner, feldon, card);
        runner.cast(response).target_object(card).resolve();
        assert_eq!(runner.state().objects[&card].zone, Zone::Exile);
        assert_eq!(runner.state().objects[&drawn].zone, Zone::Hand);
        assert_token_entry_complete(&mut runner);
        // CR 608.2b: the sole target is illegal, so Feldon creates no tokens.
        assert!(!runner
            .state()
            .objects
            .values()
            .any(|object| object.is_token));
        assert_eq!(runner.state().objects[&donor].zone, Zone::Battlefield);
    }

    #[test]
    fn t6_fresh_batches_bind_their_own_donor_and_created_token() {
        const CHANDRA: &str = "+1: Exile the top card of your library. You may cast that card. If you don't, Chandra deals 2 damage to each opponent.\n+1: Add {R}{R}.\n−3: Chandra deals 4 damage to target creature.\n−7: You get an emblem with \"Whenever you cast a spell, this emblem deals 5 damage to any target.\"";
        let (mut scenario, feldon) = setup();
        let jace = donor_jace(&mut scenario, P0);
        let chandra = scenario
            .add_planeswalker_from_oracle(P0, "Chandra, Torch of Defiance", "Chandra", 4, CHANDRA)
            .as_planeswalker_with_loyalty("Chandra", 4)
            .as_legendary()
            .id();
        let first_card = graveyard_copier(&mut scenario, false);
        let second_card = graveyard_copier(&mut scenario, false);
        let twiddle = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Twiddle",
                true,
                "You may tap or untap target artifact, creature, or land.",
            )
            .id();
        let mut runner = scenario.build();
        stage_loyalty(&mut runner, jace, 9);
        stage_loyalty(&mut runner, chandra, 11);
        let first = activate_copy(&mut runner, feldon, first_card, jace);
        assert_loyalty(&runner, first, "Jace Beleren", 4);
        assert_token_entry_complete(&mut runner);
        runner
            .cast(twiddle)
            .target_object(feldon)
            .commit()
            .stop_at_optional()
            .resolve();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: P0, .. }
        ));
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .unwrap();
        let WaitingFor::ChooseOneOfBranch {
            player,
            source_id,
            branches,
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "Twiddle must offer tap or untap, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*player, P0);
        assert_eq!(*source_id, twiddle);
        let index = branches
            .iter()
            .position(|branch| branch.description.as_deref() == Some("untap"))
            .unwrap();
        // CR 608.2d: explicitly choose untap, making the second tap-cost activation legal.
        runner.act(GameAction::ChooseBranch { index }).unwrap();
        assert_token_entry_complete(&mut runner);
        assert!(!runner.state().objects[&feldon].tapped);
        let second = activate_copy(&mut runner, feldon, second_card, chandra);
        assert_ne!(first, second);
        assert_loyalty(&runner, second, "Chandra, Torch of Defiance", 5);
        assert_eq!(
            runner.state().objects[&jace]
                .counters
                .get(&CounterType::Loyalty),
            Some(&9)
        );
        assert_eq!(
            runner.state().objects[&chandra]
                .counters
                .get(&CounterType::Loyalty),
            Some(&11)
        );
        finish_and_sacrifice(&mut runner, &[first, second], &[jace, chandra, feldon]);
    }
}
