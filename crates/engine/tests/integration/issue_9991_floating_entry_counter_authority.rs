//! Floating entry-counter riders retain their installing source (issue #9991).
//!
//! CR 109.5: "you" refers to the triggered ability's controller.
//! CR 113.7a: a triggered ability exists independently of its source.
//! CR 608.2h: source information must come from the specified object.
//! CR 614.12: replacements modify how a permanent enters the battlefield.
//! CR 616.1: the affected object's controller chooses among replacements.
//! These fixtures keep source information unchanged between trigger and entry;
//! resolution-time X snapshots and last-known information are separate work.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{GameState, ReplacementChoiceKind, WaitingFor};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;
use engine::types::{ObjectId, PlayerId};

const BREWING: &str = "When this enchantment enters, any number of target opponents each draw a card. Put an ingredient counter on this enchantment, then put an ingredient counter on it for each card drawn this way.\nWhenever you cast a creature spell, that creature enters with X additional +1/+1 counters on it, where X is the number of ingredient counters on this enchantment.";
const RUNADI: &str = "Whenever you cast a creature spell with mana value 5 or greater, that creature enters with X additional +1/+1 counters on it, where X is its mana value minus 4.\nCreatures you control with three or more +1/+1 counters on them have haste.\n{T}: Add {G}.";
const ARCHAIC: &str = "Reach, trample\nConverge — This creature enters with a +1/+1 counter on it for each color of mana spent to cast it.\nWhenever you cast a creature spell, that creature enters with X additional +1/+1 counters on it, where X is the number of colors of mana spent to cast it.";

fn add_brewing(scenario: &mut GameScenario, player: PlayerId, count: u32) -> ObjectId {
    let brewing = scenario
        .add_enchantment_from_oracle(player, "Communal Brewing", BREWING)
        .id();
    scenario.with_counter(
        brewing,
        CounterType::Generic("ingredient".to_string()),
        count,
    );
    brewing
}

fn add_spell(scenario: &mut GameScenario, player: PlayerId, generic: u32) -> ObjectId {
    let spell = scenario
        .add_creature_to_hand_from_oracle(player, "Test Entrant", 1, 1, "")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green, ManaCostShard::Blue],
            generic,
        })
        .id();
    scenario.with_mana_pool(
        player,
        [ManaType::Green, ManaType::Blue]
            .into_iter()
            .chain((0..generic).map(|_| ManaType::Colorless))
            .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
            .collect(),
    );
    spell
}

fn assert_cast_trigger(runner: &GameRunner, source: ObjectId) {
    assert!(
        runner.state().objects[&source]
            .trigger_definitions
            .as_slice()
            .iter()
            .any(|entry| entry.definition.mode == TriggerMode::SpellCast),
        "source {source:?} must register its SpellCast trigger before casting"
    );
}

fn make_p1_active(runner: &mut GameRunner) {
    // GameScenario exposes no public state_mut; mirror the existing P1-active
    // fixture in stack_session_batch_authority through the runner instead.
    let state = runner.state_mut();
    state.active_player = P1;
    state.priority_player = P1;
    state.waiting_for = WaitingFor::Priority { player: P1 };
}

fn assert_resolved(state: &GameState, expected: u32, sources: &[ObjectId]) {
    let entrant = state
        .objects
        .values()
        .find(|object| object.name == "Test Entrant" && object.zone == Zone::Battlefield)
        .expect("the cast creature must have entered the battlefield");
    // CR 614.12: the counters belong to the entering creature.
    assert_eq!(
        entrant
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        expected
    );
    assert_eq!(state.pending_damage_replacements.len(), sources.len());
    for rider in &state.pending_damage_replacements {
        assert!(rider.is_consumed, "every installed rider must have applied");
    }
    let mut actual: Vec<_> = state
        .pending_damage_replacements
        .iter()
        .map(|rider| {
            rider
                .source_object
                .expect("rider must retain its installing source")
        })
        .collect();
    let mut expected_sources = sources.to_vec();
    actual.sort();
    expected_sources.sort();
    assert_eq!(actual, expected_sources);
    assert_eq!(state.stack.len(), 0, "the entire stack must resolve");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "resolution must finish at Priority, got {:?}",
        state.waiting_for
    );
}

#[test]
fn r1_brewing_reads_its_own_ingredient_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let brewing = add_brewing(&mut scenario, P0, 3);
    let spell = add_spell(&mut scenario, P0, 0);
    let mut runner = scenario.build();
    assert_cast_trigger(&runner, brewing);

    let outcome = runner.cast(spell).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert_resolved(outcome.state(), 3, &[brewing]);
}

#[test]
fn r2_only_the_casters_brewing_installs_a_rider() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let p0_brewing = add_brewing(&mut scenario, P0, 5);
    let p1_brewing = add_brewing(&mut scenario, P1, 2);
    let spell = add_spell(&mut scenario, P1, 0);
    let mut runner = scenario.build();
    make_p1_active(&mut runner);
    assert_cast_trigger(&runner, p0_brewing);
    assert_cast_trigger(&runner, p1_brewing);

    // CR 109.5: only P1's "Whenever you cast" ability triggers here.
    let outcome = runner.cast(spell).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert_resolved(outcome.state(), 2, &[p1_brewing]);
}

#[test]
fn r3_two_brewings_each_apply_their_own_count_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let brewing_a = add_brewing(&mut scenario, P0, 3);
    let brewing_b = add_brewing(&mut scenario, P0, 2);
    let spell = add_spell(&mut scenario, P0, 0);
    let mut runner = scenario.build();
    assert_cast_trigger(&runner, brewing_a);
    assert_cast_trigger(&runner, brewing_b);

    let outcome = runner.cast(spell).resolve();
    // CR 616.1: two applicable replacement effects on one entering permanent
    // surface an ordering choice to its controller even when the order is
    // immaterial. Pin that boundary, prove its actor and the two distinct rider
    // identities (both Brewings share a name), then answer it.
    let WaitingFor::ReplacementChoice {
        player,
        candidate_count,
        candidates,
        kind,
        ..
    } = outcome.final_waiting_for()
    else {
        panic!(
            "two Brewing riders must reach a ReplacementChoice ordering prompt, got {:?}",
            outcome.final_waiting_for()
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*kind, ReplacementChoiceKind::Order);
    assert_eq!(*candidate_count, 2);
    assert_eq!(candidates.len(), 2);
    let mut sources: Vec<_> = candidates
        .iter()
        .map(|candidate| candidate.source_id)
        .collect();
    sources.sort();
    let mut expected = vec![brewing_a, brewing_b];
    expected.sort();
    assert_eq!(
        sources, expected,
        "same-named Brewings must expose distinct source identities"
    );
    let index = candidates
        .iter()
        .position(|candidate| candidate.source_id == brewing_b)
        .expect("Brewing B must be a selectable replacement");
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("replacement ordering choice must be accepted");
    runner.advance_until_stack_empty();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_resolved(runner.state(), 5, &[brewing_a, brewing_b]);
}

#[test]
fn r4_preservation_p1_runadi_uses_the_entrants_mana_value() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let runadi = scenario
        .add_creature_from_oracle(P1, "Runadi, Behemoth Caller", 1, 3, RUNADI)
        .id();
    let spell = add_spell(&mut scenario, P1, 6);
    let mut runner = scenario.build();
    make_p1_active(&mut runner);
    assert_cast_trigger(&runner, runadi);

    let outcome = runner.cast(spell).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert_resolved(outcome.state(), 4, &[runadi]);
}

#[test]
fn r5_preservation_archaic_uses_colors_spent_on_the_entrant() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let archaic = scenario
        .add_creature_from_oracle(P0, "Wildgrowth Archaic", 0, 0, ARCHAIC)
        .id();
    // Wildgrowth Archaic is a 0/0 that normally survives via its own converge
    // counters. As a pre-existing permanent it needs those counters in setup.
    scenario.with_counter(archaic, CounterType::Plus1Plus1, 3);
    let spell = add_spell(&mut scenario, P0, 0);
    let mut runner = scenario.build();
    assert_cast_trigger(&runner, archaic);

    let commit = runner.cast(spell).commit();
    let cast = commit
        .state()
        .objects
        .get(&spell)
        .expect("cast creature must exist on the stack");
    assert_eq!(cast.zone, Zone::Stack);
    assert_eq!(cast.colors_spent_to_cast.green, 1);
    assert_eq!(cast.colors_spent_to_cast.blue, 1);
    assert_eq!(cast.colors_spent_to_cast.distinct_colors(), 2);
    let outcome = commit.resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert_resolved(outcome.state(), 2, &[archaic]);
}

// Dark Intimations: full printed Oracle, parsed afresh rather than fixture data.
const DARK: &str = "Each opponent sacrifices a creature or planeswalker of their choice, then discards a card. You return a creature or planeswalker card from your graveyard to your hand, then draw a card.\nWhen you cast a Bolas planeswalker spell, exile this card from your graveyard. That planeswalker enters with an additional loyalty counter on it.";
const BOLAS: &str = "Nicol Bolas has all loyalty abilities of all other planeswalkers on the battlefield.\n[+1]: You draw a card. Each opponent exiles a card from their hand or a permanent they control.\n[−3]: Destroy target creature or planeswalker.\n[−8]: Each opponent who doesn't control a legendary creature or planeswalker loses the game.";
const CHANDRA: &str = "[+1]: Exile the top card of your library. You may cast that card. If you don't, Chandra deals 2 damage to each opponent.\n[+1]: Add {R}{R}.\n[−3]: Chandra deals 4 damage to target creature.\n[−7]: You get an emblem with \"Whenever you cast a spell, this emblem deals 5 damage to any target.\"";
const RECOLLECT: &str = "Return target card from your graveyard to your hand.";
const STIFLE: &str =
    "Counter target activated or triggered ability. (Mana abilities can't be targeted.)";
const LEYLINE: &str = "If this card is in your opening hand, you may begin the game with it on the battlefield.\nYou may cast spells as though they had flash.";
const DOUBLING: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.\nIf an effect would put one or more counters on a permanent you control, it puts twice that many of those counters on that permanent instead.";

fn add_dark(scenario: &mut GameScenario, owner: PlayerId, zone: Zone) -> ObjectId {
    let mut card = match zone {
        Zone::Graveyard => scenario.add_spell_to_graveyard(owner, "Dark Intimations", false),
        Zone::Hand => scenario.add_spell_to_hand(owner, "Dark Intimations", false),
        Zone::Exile => scenario.add_spell_to_exile(owner, "Dark Intimations", false),
        Zone::Library => scenario.add_spell_to_library_top(owner, "Dark Intimations", false),
        // Synthetic carrier: a legal permanent hosting the full parsed ability,
        // not a claim that the printed sorcery can exist on the battlefield.
        Zone::Battlefield => {
            scenario.add_enchantment_from_oracle(owner, "Test Graveyard Carrier", DARK)
        }
        _ => panic!("unsupported fixture zone {zone:?}"),
    };
    card.from_oracle_text(DARK)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![
                ManaCostShard::Blue,
                ManaCostShard::Black,
                ManaCostShard::Red,
            ],
            generic: 2,
        })
        .id()
}

fn add_walker(scenario: &mut GameScenario, owner: PlayerId, bolas: bool) -> ObjectId {
    let (name, subtype, oracle) = if bolas {
        ("Nicol Bolas, Dragon-God", "Bolas", BOLAS)
    } else {
        ("Chandra, Torch of Defiance", "Chandra", CHANDRA)
    };
    scenario
        .add_spell_to_hand(owner, name, false)
        .as_planeswalker_with_loyalty(subtype, 4)
        .as_legendary()
        .from_oracle_text(oracle)
        // Inline Oracle faces omit printed loyalty; restore that metadata after
        // parsing, as add_planeswalker_from_oracle does for battlefield fixtures.
        .as_planeswalker_with_loyalty(subtype, 4)
        .with_mana_cost(if bolas {
            ManaCost::Cost {
                shards: vec![
                    ManaCostShard::Blue,
                    ManaCostShard::Black,
                    ManaCostShard::Black,
                    ManaCostShard::Black,
                    ManaCostShard::Red,
                ],
                generic: 0,
            }
        } else {
            ManaCost::Cost {
                shards: vec![ManaCostShard::Red, ManaCostShard::Red],
                generic: 2,
            }
        })
        .id()
}

fn fund(scenario: &mut GameScenario, owner: PlayerId, count: u32) {
    scenario.with_mana_pool(
        owner,
        (0..count)
            .flat_map(|_| {
                [
                    ManaType::White,
                    ManaType::Blue,
                    ManaType::Blue,
                    ManaType::Black,
                    ManaType::Black,
                    ManaType::Black,
                    ManaType::Red,
                    ManaType::Red,
                    ManaType::Colorless,
                    ManaType::Colorless,
                    ManaType::Colorless,
                    ManaType::Colorless,
                    ManaType::Colorless,
                ]
            })
            .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
            .collect(),
    );
}

fn assert_loyalty(state: &GameState, walker: ObjectId, expected: u32) {
    let object = &state.objects[&walker];
    assert_eq!(object.zone, Zone::Battlefield);
    // CR 306.5b + CR 306.5c: printed loyalty and the additional entry counters
    // contribute to the same loyalty value, not creature +1/+1 counters.
    assert_eq!(
        object
            .counters
            .get(&CounterType::Loyalty)
            .copied()
            .unwrap_or(0),
        expected
    );
    assert_eq!(object.loyalty, Some(expected));
    assert_eq!(
        object
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        0
    );
}

fn assert_finished(state: &GameState) {
    assert!(state.stack.is_empty(), "{:?}", state.stack);
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        state.waiting_for
    );
}

fn queued_dark_trigger(runner: &GameRunner, dark: ObjectId, controller: PlayerId) -> ObjectId {
    let trigger = runner
        .state()
        .stack
        .iter()
        .find(|entry| {
            entry.source_id == dark
                && matches!(
                    entry.kind,
                    engine::types::game_state::StackEntryKind::TriggeredAbility { .. }
                )
        })
        .expect("graveyard cast trigger must be queued above the spell");
    assert_eq!(trigger.controller, controller);
    trigger.id
}

#[test]
fn dark_intimations_two_graveyard_copies_add_two_loyalty_once_each() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = add_dark(&mut scenario, P0, Zone::Graveyard);
    let b = add_dark(&mut scenario, P0, Zone::Graveyard);
    let walker = add_walker(&mut scenario, P0, true);
    fund(&mut scenario, P0, 1);
    let mut runner = scenario.build();
    let outcome = runner.cast(walker).resolve();
    let WaitingFor::ReplacementChoice {
        player,
        candidate_count,
        kind,
        ..
    } = outcome.final_waiting_for()
    else {
        panic!(
            "two riders require a replacement ordering choice: {:?}",
            outcome.final_waiting_for()
        );
    };
    // CR 616.1: choose the second distinct stored rider first, even though both
    // floating entries have the same sentinel source and display name.
    assert_eq!(*player, P0);
    assert_eq!(*kind, ReplacementChoiceKind::Order);
    assert_eq!(*candidate_count, 2);
    let candidates = &runner
        .state()
        .pending_replacement
        .as_ref()
        .unwrap()
        .candidates;
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].source, ObjectId(0));
    assert_eq!(candidates[1].source, ObjectId(0));
    assert_ne!(candidates[0].index, candidates[1].index);
    for source in [a, b] {
        assert_eq!(runner.state().objects[&source].zone, Zone::Exile);
    }
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("choose second rider first");
    runner.advance_until_stack_empty();
    assert_loyalty(runner.state(), walker, 6);
    assert_eq!(runner.state().pending_damage_replacements.len(), 2);
    assert!(runner
        .state()
        .pending_damage_replacements
        .iter()
        .all(|r| r.is_consumed));
    assert_finished(runner.state());
}

#[test]
fn dark_intimations_filters_bolas_subtype_not_all_planeswalkers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dark = add_dark(&mut scenario, P0, Zone::Graveyard);
    let chandra = add_walker(&mut scenario, P0, false);
    let bolas = add_walker(&mut scenario, P0, true);
    fund(&mut scenario, P0, 2);
    let mut runner = scenario.build();
    assert_cast_trigger(&runner, dark);
    runner.cast(chandra).resolve();
    assert_loyalty(runner.state(), chandra, 4);
    assert_eq!(runner.state().objects[&dark].zone, Zone::Graveyard);
    assert_finished(runner.state());
    runner.cast(bolas).resolve();
    assert_loyalty(runner.state(), bolas, 5);
    assert_eq!(runner.state().objects[&dark].zone, Zone::Exile);
    assert_finished(runner.state());
}

#[test]
fn dark_intimations_functions_only_in_the_graveyard() {
    for zone in [
        Zone::Hand,
        Zone::Exile,
        Zone::Library,
        Zone::Battlefield,
        Zone::Graveyard,
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let dark = add_dark(&mut scenario, P0, zone);
        let walker = add_walker(&mut scenario, P0, true);
        fund(&mut scenario, P0, 1);
        let mut runner = scenario.build();
        assert_cast_trigger(&runner, dark);
        let trigger = &runner.state().objects[&dark].trigger_definitions.as_slice()[0].definition;
        // CR 113.6m: the self-exile's origin is the sole functioning zone.
        assert_eq!(trigger.trigger_zones, vec![Zone::Graveyard]);
        runner.cast(walker).resolve();
        let expected = if zone == Zone::Graveyard { 5 } else { 4 };
        assert_loyalty(runner.state(), walker, expected);
        assert_eq!(
            runner.state().objects[&dark].zone,
            if zone == Zone::Graveyard {
                Zone::Exile
            } else {
                zone
            }
        );
        assert_finished(runner.state());
    }
}

#[test]
fn dark_intimations_on_the_stack_does_not_supply_a_rider() {
    // Pair the response-window negative with the same legal flash setup's
    // graveyard positive; do not grant flash by mutating the spell after cast.
    for source_zone in [Zone::Hand, Zone::Graveyard] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_enchantment_from_oracle(P0, "Leyline of Anticipation", LEYLINE);
        scenario.add_card_to_library_top(P0, "Draw for Dark Intimations");
        let dark = add_dark(&mut scenario, P0, source_zone);
        let walker = add_walker(&mut scenario, P0, true);
        fund(&mut scenario, P0, 2);
        let mut runner = scenario.build();
        if source_zone == Zone::Hand {
            runner.cast(dark).commit();
            assert_eq!(runner.state().objects[&dark].zone, Zone::Stack);
        }
        runner.cast(walker).commit();
        if source_zone == Zone::Graveyard {
            queued_dark_trigger(&runner, dark, P0);
        } else {
            assert_eq!(runner.state().stack.len(), 2, "only the two spells");
        }
        runner.advance_until_stack_empty();
        assert_loyalty(
            runner.state(),
            walker,
            if source_zone == Zone::Hand { 4 } else { 5 },
        );
        assert_eq!(
            runner.state().objects[&dark].zone,
            if source_zone == Zone::Hand {
                Zone::Graveyard
            } else {
                Zone::Exile
            }
        );
        assert_finished(runner.state());
    }
}

#[test]
fn dark_intimations_uses_the_graveyard_owners_cast_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let p0_dark = add_dark(&mut scenario, P0, Zone::Graveyard);
    let p1_dark = add_dark(&mut scenario, P1, Zone::Graveyard);
    let walker = add_walker(&mut scenario, P1, true);
    fund(&mut scenario, P1, 1);
    let mut runner = scenario.build();
    make_p1_active(&mut runner);
    runner.cast(walker).commit();
    // CR 113.8: a graveyard card has no controller; its owner controls its trigger.
    queued_dark_trigger(&runner, p1_dark, P1);
    assert_eq!(
        runner.state().stack.len(),
        2,
        "opponent's graveyard copy does not trigger"
    );
    runner.advance_until_stack_empty();
    assert_loyalty(runner.state(), walker, 5);
    assert_eq!(runner.state().objects[&p0_dark].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&p1_dark].zone, Zone::Exile);
    assert_finished(runner.state());
}

#[test]
fn dark_intimations_source_departure_does_not_gate_the_rider() {
    for return_source_in_response in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_enchantment_from_oracle(P0, "Leyline of Anticipation", LEYLINE);
        let dark = add_dark(&mut scenario, P0, Zone::Graveyard);
        let walker = add_walker(&mut scenario, P0, true);
        let recollect = scenario
            .add_spell_to_hand_from_oracle(P0, "Recollect", false, RECOLLECT)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 2,
            })
            .id();
        scenario.with_mana_pool(
            P0,
            [
                ManaType::Blue,
                ManaType::Black,
                ManaType::Black,
                ManaType::Black,
                ManaType::Red,
                ManaType::Green,
                ManaType::Colorless,
                ManaType::Colorless,
            ]
            .into_iter()
            .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
            .collect(),
        );
        let mut runner = scenario.build();
        runner.cast(walker).commit();
        let trigger = queued_dark_trigger(&runner, dark, P0);
        if return_source_in_response {
            runner.cast(recollect).target_object(dark).commit();
            runner.resolve_top();
            assert_eq!(runner.state().objects[&dark].zone, Zone::Hand);
            assert!(runner.state().stack.iter().any(|entry| entry.id == trigger));
        } else {
            assert_eq!(runner.state().objects[&dark].zone, Zone::Graveyard);
        }
        runner.resolve_top();
        // CR 400.7 + CR 609.3: the returned card is not the graveyard object
        // the trigger could exile. It stays in hand while the rider installs;
        // the no-response control still exiles the original graveyard source.
        let expected_source_zone = if return_source_in_response {
            Zone::Hand
        } else {
            Zone::Exile
        };
        assert_eq!(runner.state().objects[&dark].zone, expected_source_zone);
        assert_eq!(runner.state().objects[&walker].zone, Zone::Stack);
        assert_eq!(runner.state().pending_damage_replacements.len(), 1);
        runner.advance_until_stack_empty();
        assert_loyalty(runner.state(), walker, 5);
        assert_eq!(runner.state().objects[&dark].zone, expected_source_zone);
        assert_finished(runner.state());
    }
}

#[test]
fn dark_intimations_stifled_trigger_neither_exiles_nor_installs() {
    for stifle_trigger in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let dark = add_dark(&mut scenario, P0, Zone::Graveyard);
        let walker = add_walker(&mut scenario, P0, true);
        let stifle = scenario
            .add_spell_to_hand_from_oracle(P0, "Stifle", true, STIFLE)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 0,
            })
            .id();
        fund(&mut scenario, P0, 2);
        let mut runner = scenario.build();
        runner.cast(walker).commit();
        let trigger = queued_dark_trigger(&runner, dark, P0);
        if stifle_trigger {
            runner.cast(stifle).target_object(trigger).commit();
            runner.resolve_top();
            assert!(!runner.state().stack.iter().any(|entry| entry.id == trigger));
        }
        runner.advance_until_stack_empty();
        assert_loyalty(runner.state(), walker, if stifle_trigger { 4 } else { 5 });
        assert_eq!(
            runner.state().objects[&dark].zone,
            if stifle_trigger {
                Zone::Graveyard
            } else {
                Zone::Exile
            }
        );
        assert_finished(runner.state());
    }
}

#[test]
fn dark_intimations_additional_loyalty_is_doubled_as_part_of_entry() {
    for with_dark in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_enchantment_from_oracle(P0, "Doubling Season", DOUBLING);
        if with_dark {
            add_dark(&mut scenario, P0, Zone::Graveyard);
        }
        let walker = add_walker(&mut scenario, P0, true);
        fund(&mut scenario, P0, 1);
        let mut runner = scenario.build();
        let outcome = runner.cast(walker).resolve();
        // CR 306.5b + CR 122.6: both the four printed loyalty counters and
        // Dark Intimations' additional counter are entry counters, so Doubling
        // Season doubles both. CR 616.1e: the controller may order the doubler and
        // the rider (9 or 10); the engine applies the rider first, giving
        // (4 + 1) * 2 = 10, without an ordering prompt (known simplification).
        // The engine applies Doubling Season to AddCounter events, separately
        // from the rider's ChangeZone replacement. No CR 616.1 ordering prompt
        // or ordering candidates are surfaced for this single-doubler fixture.
        assert!(
            matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
            "with_dark={with_dark}: expected no ordering prompt in the per-placement model, got {:?}",
            outcome.final_waiting_for()
        );
        assert!(
            runner.state().pending_replacement.is_none(),
            "with_dark={with_dark}: expected zero queued ordering candidates"
        );
        assert_loyalty(runner.state(), walker, if with_dark { 10 } else { 8 });
        assert_finished(runner.state());
    }
}

#[test]
fn dark_intimations_uses_its_owner_after_its_spell_was_stolen() {
    const COMMANDEER: &str = "You may exile two blue cards from your hand rather than pay this spell's mana cost.\nGain control of target noncreature spell. You may choose new targets for it. (If that spell is an artifact, enchantment, or planeswalker, the permanent enters under your control.)";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dark = add_dark(&mut scenario, P0, Zone::Hand);
    let walker = add_walker(&mut scenario, P0, true);
    let discarded = scenario.add_card_to_hand(P0, "Discard instead of Bolas");
    let drawn = scenario.add_card_to_library_top(P1, "Stolen sorcery draw");
    let commandeer = scenario
        .add_spell_to_hand_from_oracle(P1, "Commandeer", true, COMMANDEER)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 5,
        })
        .id();
    fund(&mut scenario, P0, 2);
    fund(&mut scenario, P1, 1);
    let mut runner = scenario.build();
    runner.cast(dark).commit();
    runner
        .act(GameAction::PassPriority)
        .expect("owner passes to opponent");
    runner.cast(commandeer).target_object(dark).commit();
    runner.resolve_top();
    assert_eq!(runner.state().objects[&dark].zone, Zone::Stack);
    assert_eq!(
        runner.state().objects[&dark].controller,
        P1,
        "Commandeer took the sorcery"
    );
    assert!(matches!(runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { player: P1, source_id, .. } if source_id == commandeer));
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("decline Commandeer's new-target choice");
    runner.resolve_top();
    let WaitingFor::DiscardChoice { player, cards, .. } = &runner.state().waiting_for else {
        panic!(
            "stolen Dark Intimations makes P1's opponent discard: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert!(cards.contains(&discarded) && cards.contains(&walker));
    runner
        .act(GameAction::SelectCards {
            cards: vec![discarded],
        })
        .expect("preserve Bolas in hand");
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&drawn].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&dark].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&dark].owner, P0);
    assert_finished(runner.state());
    runner.cast(walker).commit();
    // CR 113.8: the graveyard source's owner, not its former spell controller,
    // controls the ability and supplies the meaning of "you cast".
    queued_dark_trigger(&runner, dark, P0);
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&dark].zone, Zone::Exile);
    assert_loyalty(runner.state(), walker, 5);
    assert_finished(runner.state());
}
