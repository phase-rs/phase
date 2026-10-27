//! Issue #7352: "counter … if it has the same <quality> as <object>" riders.
//!
//! https://github.com/phase-rs/phase/issues/7352
//!
//! Counterbalance: "Whenever an opponent casts a spell, you may reveal the top
//! card of your library. If you do, counter that spell if it has the same mana
//! value as the revealed card."
//!
//! Hisoka, Minamo Sensei: "{2}{U}, Discard a card: Counter target spell if it
//! has the same mana value as the discarded card."
//!
//! CR 118.12: Counterbalance counters only if the reveal was performed. CR
//! 608.2c + CR 202.3: the spell is countered only when its mana value equals the
//! revealed card's (Counterbalance) or the discarded card's (Hisoka). CR 608.2k +
//! CR 608.2h: "the discarded card" is the card discarded to pay the cost, read
//! from the information captured when it was discarded.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, Effect, EffectOutcomeSignal, SharedQuality,
    TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, PayCostKind, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const COUNTERBALANCE: &str = "Whenever an opponent casts a spell, you may reveal the top card of your library. If you do, counter that spell if it has the same mana value as the revealed card.";
const HISOKA: &str =
    "{2}{U}, Discard a card: Counter target spell if it has the same mana value as the discarded card.";
const GRAVEYARD_EXILE: &str = "Exile target card from a graveyard.";

/// Makes P1 the active player in their precombat main phase with priority.
fn p1_main_phase(scenario: GameScenario) -> GameRunner {
    let mut runner = scenario.build();
    let s = runner.state_mut();
    s.turn_number = 3;
    s.active_player = P1;
    s.phase = Phase::PreCombatMain;
    s.waiting_for = WaitingFor::Priority { player: P1 };
    s.priority_player = P1;
    runner
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn pass_until_stack_empty(runner: &mut GameRunner) {
    for _ in 0..16 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority while the stack resolves");
    }
    assert!(runner.state().stack.is_empty(), "completion: stack empty");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "completion: back at priority, got {:?}",
        runner.state().waiting_for
    );
}

fn chain_has_unimplemented(def: &AbilityDefinition) -> bool {
    matches!(*def.effect, Effect::Unimplemented { .. })
        || def
            .sub_ability
            .as_deref()
            .is_some_and(chain_has_unimplemented)
        || def
            .else_ability
            .as_deref()
            .is_some_and(chain_has_unimplemented)
}

fn condition_has_share_quality(condition: &AbilityCondition) -> bool {
    match condition {
        AbilityCondition::ObjectsShareQuality { .. } => true,
        AbilityCondition::And { conditions } | AbilityCondition::Or { conditions } => {
            conditions.iter().any(condition_has_share_quality)
        }
        AbilityCondition::Not { condition } => condition_has_share_quality(condition),
        _ => false,
    }
}

/// Whether any node of the chain is gated by an `ObjectsShareQuality`.
fn chain_has_share_quality(def: &AbilityDefinition) -> bool {
    def.condition
        .as_ref()
        .is_some_and(condition_has_share_quality)
        || def
            .sub_ability
            .as_deref()
            .is_some_and(chain_has_share_quality)
        || def
            .else_ability
            .as_deref()
            .is_some_and(chain_has_share_quality)
}

// ---------------------------------------------------------------------------
// Counterbalance
// ---------------------------------------------------------------------------

/// P0 controls Counterbalance with a library top of mana value `revealed_mv`;
/// P1 casts the creature spell `spell_cost` (announcing `x` when given). P1's
/// own library top is a mana-value-2 decoy.
struct CounterbalanceBoard {
    runner: GameRunner,
    source: ObjectId,
    spell: ObjectId,
    revealed: ObjectId,
}

fn counterbalance_board(
    revealed_mv: u32,
    spell_cost: ManaCost,
    x: Option<u32>,
) -> CounterbalanceBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Counterbalance", COUNTERBALANCE)
        .id();
    let spell = scenario
        .add_creature_to_hand(P1, "Bear", 2, 2)
        .with_mana_cost(spell_cost)
        .id();
    for _ in 0..3 {
        scenario.add_basic_land(P1, ManaColor::Green);
    }
    // Decoy: the caster's library top shares the spell's mana value. "your
    // library" is Counterbalance's controller's, so this card is never read.
    scenario
        .add_spell_to_library_top(P1, "Decoy", true)
        .with_mana_cost(ManaCost::generic(2));
    let revealed = scenario
        .add_spell_to_library_top(P0, "Revealed", true)
        .with_mana_cost(ManaCost::generic(revealed_mv))
        .id();
    let mut runner = p1_main_phase(scenario);
    match x {
        Some(value) => runner.cast(spell).x(value).commit(),
        None => runner.cast(spell).commit(),
    };
    CounterbalanceBoard {
        runner,
        source,
        spell,
        revealed,
    }
}

/// Passes priority until Counterbalance's trigger resolves to its "you may"
/// prompt, answers it, and drives the stack empty.
fn resolve_counterbalance(board: &mut CounterbalanceBoard, accept: bool) {
    let runner = &mut board.runner;
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ) {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
    let WaitingFor::OptionalEffectChoice { player, .. } = runner.state().waiting_for else {
        panic!(
            "Counterbalance must ask its controller whether to reveal, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        player, P0,
        "the reveal is Counterbalance's controller's choice"
    );
    assert!(
        runner
            .state()
            .stack
            .iter()
            .all(|e| e.source_id != board.source),
        "reach guard: the trigger is resolving"
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept })
        .expect("answer the reveal prompt");
    pass_until_stack_empty(runner);
}

/// CB-1: equal mana values — the revealed card's 2 matches the spell's 2.
#[test]
fn counterbalance_counters_a_spell_with_the_revealed_cards_mana_value() {
    let mut board = counterbalance_board(2, ManaCost::generic(2), None);
    resolve_counterbalance(&mut board, true);
    assert_eq!(zone(&board.runner, board.spell), Zone::Graveyard);
    assert_eq!(
        zone(&board.runner, board.revealed),
        Zone::Library,
        "the revealed card stays on top of P0's library"
    );
}

/// CB-2 (#7352): different mana values — the revealed card's 3 does not match
/// the spell's 2, so the spell resolves. P1's decoy library top (mana value 2)
/// must not be read, and the spell must not be compared with itself or with the
/// revealed card alone.
#[test]
fn counterbalance_does_not_counter_a_spell_with_a_different_mana_value() {
    let mut board = counterbalance_board(3, ManaCost::generic(2), None);
    resolve_counterbalance(&mut board, true);
    assert_eq!(
        zone(&board.runner, board.spell),
        Zone::Battlefield,
        "a mana-value-2 spell is not countered by a mana-value-3 reveal"
    );
}

/// CB-3 (CR 118.12): declining the reveal counters nothing, even when the top
/// card would have matched.
#[test]
fn counterbalance_declined_reveal_counters_nothing() {
    let mut board = counterbalance_board(2, ManaCost::generic(2), None);
    resolve_counterbalance(&mut board, false);
    assert_eq!(zone(&board.runner, board.spell), Zone::Battlefield);
    assert!(board.runner.state().last_revealed_ids.is_empty());
}

/// CB-5 (CR 202.3e): an {X} spell's mana value on the stack includes the
/// announced X. {X}{G} with X=1 has mana value 2 (countered by a 2 reveal);
/// with X=0 it has mana value 1 (not countered).
#[test]
fn counterbalance_reads_an_x_spells_mana_value_with_x_on_the_stack() {
    let x_cost = || ManaCost::Cost {
        shards: vec![ManaCostShard::X, ManaCostShard::Green],
        generic: 0,
    };
    let mut x_one = counterbalance_board(2, x_cost(), Some(1));
    resolve_counterbalance(&mut x_one, true);
    assert_eq!(zone(&x_one.runner, x_one.spell), Zone::Graveyard);

    let mut x_zero = counterbalance_board(2, x_cost(), Some(0));
    resolve_counterbalance(&mut x_zero, true);
    assert_eq!(zone(&x_zero.runner, x_zero.spell), Zone::Battlefield);
}

/// CB-AST: the reveal is optional, "If you do" and the same-mana-value rider
/// both gate the counter, and "that spell" is the spell whose casting
/// triggered the ability. Nothing is hoisted into an intervening-if.
#[test]
fn counterbalance_lowers_to_a_reflexive_and_same_quality_gate() {
    let parsed = parse_oracle_text(
        COUNTERBALANCE,
        "Counterbalance",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::SpellCast)
        .expect("Counterbalance has a spell-cast trigger");
    let execute = trigger.execute.as_deref().expect("trigger execute");
    assert!(
        matches!(*execute.effect, Effect::RevealTop { .. }),
        "reach guard: the trigger reveals first, got {:?}",
        execute.effect
    );
    assert!(execute.optional, "the reveal is optional");
    assert!(
        trigger.condition.is_none(),
        "no intervening-if is hoisted onto the trigger: {:?}",
        trigger.condition
    );
    assert!(!chain_has_unimplemented(execute), "{execute:#?}");
    let counter = execute.sub_ability.as_deref().expect("counter sub-ability");
    assert!(matches!(
        &*counter.effect,
        Effect::Counter {
            target: TargetFilter::TriggeringSource,
            ..
        }
    ));
    assert_eq!(
        counter.condition,
        Some(AbilityCondition::And {
            conditions: vec![
                AbilityCondition::EffectOutcome {
                    signal: EffectOutcomeSignal::OptionalEffectPerformed,
                },
                AbilityCondition::ObjectsShareQuality {
                    subject: TargetFilter::TriggeringSource,
                    reference: TargetFilter::LastRevealed,
                    quality: SharedQuality::ManaValue,
                },
            ],
        })
    );
}

/// The Counterbalance trigger's lowered reveal and counter nodes for `text`.
fn counterbalance_shaped_trigger_nodes(text: &str) -> (AbilityDefinition, AbilityDefinition) {
    let parsed = parse_oracle_text(
        text,
        "Counterbalance",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::SpellCast)
        .expect("spell-cast trigger");
    let execute = trigger.execute.as_deref().expect("trigger execute").clone();
    assert!(
        matches!(*execute.effect, Effect::RevealTop { .. }) && execute.optional,
        "reach guard: the trigger reveals optionally first, got {execute:#?}"
    );
    let counter = execute
        .sub_ability
        .as_deref()
        .expect("reach guard: a counter sub-ability follows the reveal")
        .clone();
    assert!(
        matches!(*counter.effect, Effect::Counter { .. }),
        "reach guard: the sub-ability counters, got {counter:#?}"
    );
    (execute, counter)
}

/// CR 118.12 + CR 603.12 + CR 608.2c: the rider is claimed under a reflexive
/// connector only when the final condition can keep both. "If you do" composes
/// (`And`, CB-AST). A negated reflexive ("If they don't") or a zone-change "if
/// … this way" head does not compose, so the rider is left to the base
/// lowering instead of displacing the connector, which still gates the
/// counter. The sentences differ from the verbatim text only in the connector.
#[test]
fn same_quality_rider_is_claimed_only_under_a_composing_reflexive() {
    let (_, composing) = counterbalance_shaped_trigger_nodes(COUNTERBALANCE);
    assert!(
        chain_has_share_quality(&composing),
        "paired positive: \"If you do\" claims the rider, got {composing:#?}"
    );

    for connector in ["If they don't,", "If a card is exiled this way,"] {
        let text = COUNTERBALANCE.replace("If you do,", connector);
        assert_ne!(text, COUNTERBALANCE, "fixture: connector replaced");
        let (reveal, counter) = counterbalance_shaped_trigger_nodes(&text);
        assert!(
            !chain_has_share_quality(&reveal),
            "{connector} cannot compose with the rider, so it is not claimed: {reveal:#?}"
        );
        let kept = match connector {
            "If they don't," => {
                counter.condition
                    == Some(AbilityCondition::Not {
                        condition: Box::new(AbilityCondition::EffectOutcome {
                            signal: EffectOutcomeSignal::OptionalEffectPerformed,
                        }),
                    })
            }
            _ => matches!(
                counter.condition,
                Some(AbilityCondition::ZoneChangedThisWay { .. })
            ),
        };
        assert!(
            kept,
            "{connector} still gates the counter, got {:?}",
            counter.condition
        );
    }
}

// ---------------------------------------------------------------------------
// Hisoka, Minamo Sensei
// ---------------------------------------------------------------------------

struct HisokaBoard {
    runner: GameRunner,
    hisoka: ObjectId,
    spell: ObjectId,
    discard: ObjectId,
    graveyard_decoy: ObjectId,
    purge: ObjectId,
}

/// P0 controls Hisoka with {U}{C}{C} floating and a hand card of mana value
/// `discard_mv`; a mana-value-2 decoy already sits in P0's graveyard. P1 casts
/// a mana-value-2 creature spell and passes priority to P0, keeping a Forest
/// and a graveyard-exile instant to respond with.
fn hisoka_board(discard_mv: u32) -> HisokaBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hisoka = scenario
        .add_creature_from_oracle(P0, "Hisoka, Minamo Sensei", 1, 3, HISOKA)
        .id();
    let discard = scenario
        .add_spell_to_hand(P0, "Discarded", true)
        .with_mana_cost(ManaCost::generic(discard_mv))
        .id();
    let graveyard_decoy = scenario
        .add_spell_to_graveyard(P0, "Graveyard Decoy", true)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let spell = scenario
        .add_creature_to_hand(P1, "Bear", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let purge = scenario
        .add_spell_to_hand_from_oracle(P1, "Purge", true, GRAVEYARD_EXILE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .id();
    for _ in 0..3 {
        scenario.add_basic_land(P1, ManaColor::Green);
    }
    let mut runner = p1_main_phase(scenario);
    runner.cast(spell).commit();
    runner
        .act(GameAction::PassPriority)
        .expect("P1 passes priority with the spell on the stack");
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "reach guard: P0 can respond to the spell"
    );
    HisokaBoard {
        runner,
        hisoka,
        spell,
        discard,
        graveyard_decoy,
        purge,
    }
}

fn activate_hisoka(board: &mut HisokaBoard) {
    let outcome = board
        .runner
        .activate(board.hisoka, 0)
        .target_object(board.spell)
        .pay_with(&[board.discard])
        .resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. })
            && outcome.state().stack.is_empty(),
        "completion: stack empty at priority, got {:?}",
        outcome.final_waiting_for()
    );
}

/// Activates Hisoka through public actions and stops with its ability on the
/// stack, the discard paid.
fn activate_hisoka_and_hold(board: &mut HisokaBoard) {
    let runner = &mut board.runner;
    runner
        .act(GameAction::ActivateAbility {
            source_id: board.hisoka,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Hisoka");
    let mut paid_discard = false;
    for _ in 0..8 {
        match &runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(board.spell)),
                    })
                    .expect("target the spell");
            }
            WaitingFor::PayCost { kind, .. } => {
                assert_eq!(*kind, PayCostKind::Discard, "the cost is a discard");
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![board.discard],
                    })
                    .expect("discard to pay the cost");
                paid_discard = true;
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("finalize the mana payment");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected activation prompt {other:?}"),
        }
    }
    assert!(paid_discard, "reach guard: the discard cost was paid");
    assert_eq!(zone(runner, board.discard), Zone::Graveyard);
    let top = runner
        .state()
        .stack
        .back()
        .expect("Hisoka's ability on the stack");
    assert!(
        top.source_id == board.hisoka
            && matches!(top.kind, StackEntryKind::ActivatedAbility { .. }),
        "reach guard: Hisoka's ability is on top"
    );
}

/// With Hisoka's ability on the stack, P1 exiles the discarded card from P0's
/// graveyard in response; then the stack resolves.
fn exile_discarded_card_in_response_then_resolve(board: &mut HisokaBoard) {
    let runner = &mut board.runner;
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority with Hisoka's ability on the stack");
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 },
        "reach guard: P1 can respond"
    );
    runner
        .cast(board.purge)
        .target_object(board.discard)
        .commit();
    runner.resolve_top();
    assert_eq!(
        zone(runner, board.discard),
        Zone::Exile,
        "reach guard: the discarded card left the graveyard before Hisoka's ability resolves"
    );
    assert!(
        runner
            .state()
            .stack
            .back()
            .is_some_and(|e| e.source_id == board.hisoka),
        "reach guard: Hisoka's ability is still on the stack"
    );
    pass_until_stack_empty(runner);
}

/// H-1: the discarded card's mana value 2 matches the spell's 2.
#[test]
fn hisoka_counters_a_spell_with_the_discarded_cards_mana_value() {
    let mut board = hisoka_board(2);
    activate_hisoka(&mut board);
    assert_eq!(zone(&board.runner, board.spell), Zone::Graveyard);
    assert_eq!(zone(&board.runner, board.discard), Zone::Graveyard);
}

/// H-2 (#7352): the discarded card's mana value 3 does not match the spell's 2.
/// The mana-value-2 card already in P0's graveyard is not "the discarded card",
/// and the spell must not be compared with itself.
#[test]
fn hisoka_does_not_counter_a_spell_with_a_different_mana_value() {
    let mut board = hisoka_board(3);
    activate_hisoka(&mut board);
    assert_eq!(
        zone(&board.runner, board.spell),
        Zone::Battlefield,
        "a mana-value-2 spell is not countered by a mana-value-3 discard"
    );
    assert_eq!(zone(&board.runner, board.discard), Zone::Graveyard);
    assert_eq!(zone(&board.runner, board.graveyard_decoy), Zone::Graveyard);
}

/// H-3 (CR 608.2k + CR 608.2h): the discarded card (mana value 2) leaves the
/// graveyard while Hisoka's ability is on the stack; the ability still reads
/// the mana value it had when it was discarded and counters the spell.
#[test]
fn hisoka_reads_the_discarded_card_after_it_leaves_the_graveyard() {
    let mut board = hisoka_board(2);
    activate_hisoka_and_hold(&mut board);
    exile_discarded_card_in_response_then_resolve(&mut board);
    assert_eq!(zone(&board.runner, board.spell), Zone::Graveyard);
}

/// H-4: as H-3 with a mana-value-3 discard — the departed card's information
/// still does not match the spell's 2, so the spell resolves.
#[test]
fn hisoka_departed_discard_with_a_different_mana_value_counters_nothing() {
    let mut board = hisoka_board(3);
    activate_hisoka_and_hold(&mut board);
    exile_discarded_card_in_response_then_resolve(&mut board);
    assert_eq!(zone(&board.runner, board.spell), Zone::Battlefield);
}

/// H-AST: "it" is the ability's target and "the discarded card" is the card
/// discarded to pay the cost; the cost is unchanged.
#[test]
fn hisoka_lowers_to_a_cost_paid_same_quality_gate() {
    let parsed = parse_oracle_text(
        HISOKA,
        "Hisoka, Minamo Sensei",
        &[],
        &["Legendary".to_string(), "Creature".to_string()],
        &["Human".to_string(), "Wizard".to_string()],
    );
    let ability = parsed
        .abilities
        .first()
        .expect("Hisoka's activated ability");
    assert!(!chain_has_unimplemented(ability), "{ability:#?}");
    assert!(matches!(
        &*ability.effect,
        Effect::Counter {
            target: TargetFilter::StackSpell,
            ..
        }
    ));
    assert_eq!(
        ability.condition,
        Some(AbilityCondition::ObjectsShareQuality {
            subject: TargetFilter::ParentTarget,
            reference: TargetFilter::CostPaidObject,
            quality: SharedQuality::ManaValue,
        })
    );
    let Some(AbilityCost::Composite { costs }) = &ability.cost else {
        panic!(
            "Hisoka's cost is mana plus a discard, got {:?}",
            ability.cost
        );
    };
    assert!(costs.iter().any(|c| matches!(c, AbilityCost::Mana { .. })));
    assert!(costs
        .iter()
        .any(|c| matches!(c, AbilityCost::Discard { .. })));
}

/// CR 608.2k: "the discarded card" names a cost-paid object only when the
/// ability's cost discards. Here the discard is the effect's own first
/// instruction and the cost is mana only, so the rider is not claimed as a
/// `CostPaidObject` comparison (which would read no cost-paid object at
/// resolution). Paired positive: Hisoka's verbatim text, whose cost discards,
/// claims it (H-AST). The declined text keeps its base lowering; this row
/// asserts only the non-claim.
#[test]
fn discarded_card_rider_without_a_discard_cost_is_not_a_cost_paid_reference() {
    let types = ["Legendary".to_string(), "Creature".to_string()];
    let subtypes = ["Human".to_string(), "Wizard".to_string()];
    let hisoka = parse_oracle_text(HISOKA, "Hisoka, Minamo Sensei", &[], &types, &subtypes);
    let hisoka_ability = hisoka
        .abilities
        .first()
        .expect("Hisoka's activated ability");
    assert!(
        chain_has_share_quality(hisoka_ability),
        "paired positive: a discard cost binds the rider, got {hisoka_ability:#?}"
    );

    let effect_discard = "{2}{U}: Discard a card, then counter target spell if it has the same mana value as the discarded card.";
    let parsed = parse_oracle_text(
        effect_discard,
        "Hisoka, Minamo Sensei",
        &[],
        &types,
        &subtypes,
    );
    let ability = parsed.abilities.first().expect("the activated ability");
    assert!(
        matches!(ability.cost, Some(AbilityCost::Mana { .. })),
        "reach guard: the cost is mana only, got {:?}",
        ability.cost
    );
    assert!(
        ability
            .sub_ability
            .as_deref()
            .is_some_and(|counter| matches!(*counter.effect, Effect::Counter { .. })),
        "reach guard: the rider's counter clause was parsed, got {ability:#?}"
    );
    assert!(
        !chain_has_share_quality(ability),
        "an effect discard does not make \"the discarded card\" a cost-paid object: {ability:#?}"
    );
}

/// CR 608.2k: a cost-paid participle is claimed only when the ability's cost
/// has exactly one slot-binding payment, of its kind. A cost that discards and also
/// sacrifices or reveals has two payments that can write the one cost-paid
/// slot, so the participle is ambiguous and the rider is not claimed as a
/// `CostPaidObject` comparison. Paired positive: Hisoka's verbatim text, whose
/// only slot-binding cost is the discard (beside mana), claims it. The
/// declined text keeps its base lowering; this row asserts only the non-claim.
#[test]
fn cost_participle_rider_under_an_ambiguous_slot_binding_cost_is_not_claimed() {
    let types = ["Legendary".to_string(), "Creature".to_string()];
    let subtypes = ["Human".to_string(), "Wizard".to_string()];
    let hisoka = parse_oracle_text(HISOKA, "Hisoka, Minamo Sensei", &[], &types, &subtypes);
    let hisoka_ability = hisoka
        .abilities
        .first()
        .expect("Hisoka's activated ability");
    assert!(
        chain_has_share_quality(hisoka_ability),
        "paired positive: a discard-only cost binds the rider, got {hisoka_ability:#?}"
    );

    type CostMatcher = fn(&AbilityCost) -> bool;
    let is_sacrifice: CostMatcher = |c| matches!(c, AbilityCost::Sacrifice(_));
    let is_reveal: CostMatcher = |c| matches!(c, AbilityCost::Reveal { .. });
    let rows = [
        ("Sacrifice a creature", "the discarded card", is_sacrifice),
        (
            "Sacrifice a creature",
            "the sacrificed creature",
            is_sacrifice,
        ),
        (
            "Reveal a Dragon card from your hand",
            "the discarded card",
            is_reveal,
        ),
    ];
    for (second_cost, reference_text, is_second_cost) in rows {
        let text = format!(
            "{{2}}{{U}}, Discard a card, {second_cost}: Counter target spell if it has the same mana value as {reference_text}."
        );
        let parsed = parse_oracle_text(&text, "Hisoka, Minamo Sensei", &[], &types, &subtypes);
        let ability = parsed.abilities.first().expect("the activated ability");
        let Some(AbilityCost::Composite { costs }) = &ability.cost else {
            panic!(
                "reach guard: the cost is a composite, got {:?}",
                ability.cost
            );
        };
        assert!(
            costs
                .iter()
                .any(|c| matches!(c, AbilityCost::Discard { .. }))
                && costs.iter().any(is_second_cost),
            "reach guard: the cost discards and pays `{second_cost}`, got {costs:?}"
        );
        assert!(
            matches!(*ability.effect, Effect::Counter { .. }),
            "reach guard: the counter instruction was parsed, got {ability:#?}"
        );
        assert!(
            !chain_has_share_quality(ability),
            "an ambiguous cost-paid participle is not claimed ({text}): {ability:#?}"
        );
    }
}

/// CR 608.2k: a cost-paid participle is claimed only when exactly one payment
/// of the ability's cost can write the cost-paid object. Two sacrifice
/// payments share one cost category but each overwrites the single cost-paid
/// slot, so "the sacrificed creature" has no unambiguous referent and the
/// rider is not claimed as a `CostPaidObject` comparison. Paired positive:
/// Hisoka's verbatim text, whose one slot-binding payment is the discard,
/// claims it. The declined text keeps its base lowering; this row asserts only
/// the non-claim.
#[test]
fn cost_participle_rider_under_two_same_kind_payments_is_not_claimed() {
    let types = ["Legendary".to_string(), "Creature".to_string()];
    let subtypes = ["Human".to_string(), "Wizard".to_string()];
    let hisoka = parse_oracle_text(HISOKA, "Hisoka, Minamo Sensei", &[], &types, &subtypes);
    let hisoka_ability = hisoka
        .abilities
        .first()
        .expect("Hisoka's activated ability");
    assert!(
        chain_has_share_quality(hisoka_ability),
        "paired positive: a single discard payment binds the rider, got {hisoka_ability:#?}"
    );

    let text = "Sacrifice a creature, Sacrifice an enchantment: Counter target spell if it has the same mana value as the sacrificed creature.";
    let parsed = parse_oracle_text(text, "Hisoka, Minamo Sensei", &[], &types, &subtypes);
    let ability = parsed.abilities.first().expect("the activated ability");
    let Some(AbilityCost::Composite { costs }) = &ability.cost else {
        panic!(
            "reach guard: the cost is a composite, got {:?}",
            ability.cost
        );
    };
    assert_eq!(
        costs
            .iter()
            .filter(|c| matches!(c, AbilityCost::Sacrifice(_)))
            .count(),
        2,
        "reach guard: the cost is two sacrifice payments, got {costs:?}"
    );
    assert_eq!(
        costs.len(),
        2,
        "reach guard: nothing else is paid: {costs:?}"
    );
    assert!(
        matches!(*ability.effect, Effect::Counter { .. }),
        "reach guard: the counter instruction was parsed, got {ability:#?}"
    );
    assert!(
        !chain_has_share_quality(ability),
        "two sacrifice payments make \"the sacrificed creature\" ambiguous: {ability:#?}"
    );
}

/// Whether `cost` is a `Composite` that pays exactly `n` sacrifices.
fn is_composite_with_sacrifices(cost: &AbilityCost, n: usize) -> bool {
    matches!(cost, AbilityCost::Composite { costs }
        if costs
            .iter()
            .filter(|c| matches!(c, AbilityCost::Sacrifice(_)))
            .count()
            == n)
}

/// CR 608.2k: an alternative ("or") cost may pay any one of its
/// branches, so every payment any branch could make counts toward the
/// cost-paid slot. A branch that sacrifices twice ("Sacrifice a creature,
/// Sacrifice an artifact or {R}, {T}") writes the one cost-paid slot twice, so
/// "the sacrificed creature" has no unambiguous referent and the rider is not
/// claimed as a `CostPaidObject` comparison. Paired positive: the same
/// alternative cost with a single sacrifice in its sacrifice branch claims it.
/// The declined text keeps its base lowering; this row asserts only the
/// non-claim.
#[test]
fn cost_participle_rider_under_an_alternative_with_two_sacrifices_is_not_claimed() {
    let types = ["Artifact".to_string()];
    let rider = "Counter target spell if it has the same mana value as the sacrificed creature.";
    let parse = |cost_text: &str| {
        let text = format!("{cost_text}: {rider}");
        let parsed = parse_oracle_text(&text, "Test Shard", &[], &types, &[]);
        let ability = parsed
            .abilities
            .first()
            .expect("the activated ability")
            .clone();
        (text, ability)
    };

    for (cost_text, sacrifices, claimed) in [
        ("Sacrifice a creature or {R}, {T}", 1, true),
        (
            "Sacrifice a creature, Sacrifice an artifact or {R}, {T}",
            2,
            false,
        ),
    ] {
        let (text, ability) = parse(cost_text);
        let Some(AbilityCost::OneOf { costs }) = &ability.cost else {
            panic!(
                "reach guard: the cost is an alternative, got {:?} ({text})",
                ability.cost
            );
        };
        let sacrifice_branch_matches = match sacrifices {
            1 => costs.iter().any(|c| matches!(c, AbilityCost::Sacrifice(_))),
            n => costs.iter().any(|c| is_composite_with_sacrifices(c, n)),
        };
        assert!(
            sacrifice_branch_matches,
            "reach guard: one branch pays {sacrifices} sacrifice(s), got {costs:?} ({text})"
        );
        assert!(
            costs
                .iter()
                .any(|c| matches!(c, AbilityCost::Composite { costs } if costs
                    .iter()
                    .any(|c| matches!(c, AbilityCost::Mana { .. })))),
            "reach guard: the other branch pays mana, got {costs:?} ({text})"
        );
        assert!(
            matches!(*ability.effect, Effect::Counter { .. }),
            "reach guard: the counter instruction was parsed, got {ability:#?}"
        );
        assert_eq!(
            chain_has_share_quality(&ability),
            claimed,
            "\"the sacrificed creature\" is claimed only when one sacrifice can be paid ({text}): {ability:#?}"
        );
    }
}
