//! CR 601.2b + CR 601.2f + CR 702.47a: the mana of an additional cost the
//! caster has already declared (a splice cost, a chosen Spree mode's cost) is
//! part of the spell's total cost for the rest of the cast. When the
//! additional-cost authority rebuilds the pending cast after targets are
//! declared, the total it locks in still includes that mana, so the spell pays
//! it once and a pool that cannot pay it refuses the cast.
//!
//! Fixture names contain no keyword and no Oracle word.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::casting_costs::ReductionProvenance;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const OPTIONAL_LAND: &str = "As an additional cost to cast this spell, you may sacrifice a land.";
const DESTROY_ONE: &str = "Destroy target creature an opponent controls.";
const SPLICE_GAIN: &str = "Splice onto Arcane {1}\nYou gain 2 life.";
const SPREE_BODY: &str =
    "Spree\n+ {1} — Destroy target creature an opponent controls.\n+ {2} — You gain 3 life.";
const COLOURED_ONLY_LESS: &str = "Instant spells you cast cost {B} less to cast. This effect reduces only the amount of colored mana you pay.";
const SPILLING_LESS: &str = "Instant spells you cast cost {B} less to cast.";
const ALTERNATIVE_ONE: &str = "You may pay {1} rather than pay this spell's mana cost.";

// Verbatim Oracle text (pinned MTGJSON AtomicCards).
const DEVOURING_RAGE: &str = "As an additional cost to cast this spell, you may sacrifice any number of Spirits.\nTarget creature gets +3/+0 until end of turn. For each Spirit sacrificed this way, that creature gets an additional +3/+0 until end of turn.";
const KODAMAS_MIGHT: &str = "Target creature gets +2/+2 until end of turn.\nSplice onto Arcane {G} (As you cast an Arcane spell, you may reveal this card from your hand and pay its splice cost. If you do, add this card's effects to that spell.)";

fn units(mana_type: ManaType, n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(mana_type, ObjectId(0), false, vec![]))
        .collect()
}

fn cost(shards: Vec<ManaCostShard>, generic: u32) -> ManaCost {
    ManaCost::Cost { shards, generic }
}

/// How each prompt of a cast is answered.
#[derive(Clone, Copy)]
struct Answers {
    target: ObjectId,
    splice: Option<ObjectId>,
    pay_optional: bool,
    /// The caster's preferred object for a cost selection.
    pick: Option<ObjectId>,
    /// The permanent whose cost reduction the caster elects to apply first.
    first_reducer: Option<ObjectId>,
}

/// What a cast did up to the point the spell is on the stack (or refused).
struct Cast {
    prompts: Vec<&'static str>,
    optional_offers: usize,
    refused: bool,
}

impl Cast {
    fn prompted(&self, kind: &str) -> bool {
        self.prompts.contains(&kind)
    }
}

/// Cast `spell` and answer every prompt from `answers` until priority returns.
fn cast(runner: &mut GameRunner, spell: ObjectId, answers: Answers) -> Cast {
    let mut out = Cast {
        prompts: Vec::new(),
        optional_offers: 0,
        refused: false,
    };
    let first = GameAction::CastSpell {
        object_id: spell,
        card_id: runner.state().objects[&spell].card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    };
    out.refused = runner.act(first).is_err();
    while !out.refused {
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::SpliceOffer { .. } => GameAction::RespondToSpliceOffer {
                card: answers.splice,
            },
            WaitingFor::ModeChoice { .. } => GameAction::SelectModes { indices: vec![0] },
            WaitingFor::ChooseXValue { .. } => GameAction::ChooseX { value: 1 },
            WaitingFor::TargetSelection { selection, .. } => {
                let target = TargetRef::Object(answers.target);
                GameAction::ChooseTarget {
                    target: Some(target.clone())
                        .filter(|target| selection.current_legal_targets.contains(target))
                        .or_else(|| selection.current_legal_targets.first().cloned()),
                }
            }
            WaitingFor::OptionalCostChoice { .. } => {
                out.optional_offers += 1;
                GameAction::DecideOptionalCost {
                    pay: answers.pay_optional,
                }
            }
            WaitingFor::PayCost { choices, count, .. } => {
                let mut cards: Vec<ObjectId> = answers
                    .pick
                    .filter(|pick| choices.contains(pick))
                    .into_iter()
                    .collect();
                for choice in &choices {
                    if cards.len() >= count {
                        break;
                    }
                    if !cards.contains(choice) {
                        cards.push(*choice);
                    }
                }
                GameAction::SelectCards { cards }
            }
            WaitingFor::OrderCostReductions { ref reductions, .. } => {
                let (mut first, rest): (Vec<usize>, Vec<usize>) =
                    (0..reductions.len()).partition(|&index| {
                        matches!(
                            reductions[index].provenance,
                            ReductionProvenance::Static { source, .. }
                                if Some(source) == answers.first_reducer
                        )
                    });
                first.extend(rest);
                GameAction::OrderCostReductions {
                    order: first,
                    hybrid_announcement: vec![],
                }
            }
            WaitingFor::ManaPayment { .. } => GameAction::PassPriority,
            _ => break,
        };
        out.prompts.push(runner.waiting_for_kind());
        out.refused = runner.act(action).is_err();
    }
    out
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn pool(runner: &GameRunner) -> usize {
    runner.state().players[0].mana_pool.total()
}

fn lands_on_battlefield(runner: &GameRunner, lands: &[ObjectId]) -> usize {
    lands
        .iter()
        .filter(|land| zone(runner, **land) == Zone::Battlefield)
        .count()
}

/// Two opponent creatures, so the host's target declaration is prompted.
fn two_marks(scenario: &mut GameScenario) -> ObjectId {
    let first = scenario.add_creature(P1, "Mark 0", 2, 2).id();
    scenario.add_creature(P1, "Mark 1", 2, 2);
    first
}

fn add_spare_splice(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Ember Note", true, "")
        .with_subtypes(vec!["Arcane"])
        .from_oracle_text_with_keywords(&["Splice"], SPLICE_GAIN)
        .with_mana_cost(cost(vec![ManaCostShard::Black], 0))
        .id()
}

struct Host {
    runner: GameRunner,
    spell: ObjectId,
    lands: Vec<ObjectId>,
    answers: Answers,
}

/// An Arcane instant costing {B}{B} with `text`, three lands to sacrifice, a
/// splice card costing {1}, two legal targets and {B}{B}{B}{B} in the pool.
fn arcane_host(text: &str, splice: bool, pay_optional: bool) -> Host {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lands = (0..3)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Black))
        .collect();
    let target = two_marks(&mut scenario);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Quiet Host", true, text)
        .with_subtypes(vec!["Arcane"])
        .with_mana_cost(cost(vec![ManaCostShard::Black; 2], 0))
        .id();
    let splicer = add_spare_splice(&mut scenario);
    scenario.with_mana_pool(P0, units(ManaType::Black, 4));
    Host {
        runner: scenario.build(),
        spell,
        lands,
        answers: Answers {
            target,
            splice: splice.then_some(splicer),
            pay_optional,
            pick: None,
            first_reducer: None,
        },
    }
}

fn cast_host(host: &mut Host) -> Cast {
    let spell = host.spell;
    cast(&mut host.runner, spell, host.answers)
}

#[test]
fn splice_mana_is_charged_beside_an_accepted_optional_cost() {
    let mut host = arcane_host(&format!("{OPTIONAL_LAND}\n{DESTROY_ONE}"), true, true);
    let host_cast = cast_host(&mut host);
    assert!(host_cast.prompted("TargetSelection"));
    assert_eq!(host_cast.optional_offers, 1);
    assert_eq!(zone(&host.runner, host.spell), Zone::Stack);
    assert_eq!(lands_on_battlefield(&host.runner, &host.lands), 2);
    // {B}{B} plus the splice cost {1}.
    assert_eq!(pool(&host.runner), 1);
}

/// Preservation: declining the optional cost never recomputes the total, so
/// the splice cost was already charged once before the fix and must stay so.
#[test]
fn splice_mana_is_charged_beside_a_declined_optional_cost() {
    let mut host = arcane_host(&format!("{OPTIONAL_LAND}\n{DESTROY_ONE}"), true, false);
    let host_cast = cast_host(&mut host);
    assert_eq!(host_cast.optional_offers, 1);
    assert_eq!(zone(&host.runner, host.spell), Zone::Stack);
    assert_eq!(lands_on_battlefield(&host.runner, &host.lands), 3);
    assert_eq!(pool(&host.runner), 1);
}

#[test]
fn splice_without_additional_cost_is_charged_once() {
    let mut host = arcane_host(DESTROY_ONE, true, true);
    let host_cast = cast_host(&mut host);
    assert!(host_cast.prompted("SpliceOffer"));
    assert_eq!(host_cast.optional_offers, 0);
    assert_eq!(zone(&host.runner, host.spell), Zone::Stack);
    assert_eq!(pool(&host.runner), 1);
}

#[test]
fn unspliced_host_pays_only_its_own_cost() {
    let mut host = arcane_host(&format!("{OPTIONAL_LAND}\n{DESTROY_ONE}"), false, true);
    cast_host(&mut host);
    assert_eq!(zone(&host.runner, host.spell), Zone::Stack);
    assert_eq!(pool(&host.runner), 2);
}

/// CR 118.9b + CR 601.2f: declining the alternative cost pays the mana cost,
/// {B}{B}, plus the splice cost {1}.
#[test]
fn splice_mana_is_charged_beside_a_declined_alternative_mana_cost() {
    let mut host = arcane_host(&format!("{ALTERNATIVE_ONE}\n{DESTROY_ONE}"), true, false);
    let host_cast = cast_host(&mut host);
    assert_eq!(host_cast.optional_offers, 1);
    assert_eq!(zone(&host.runner, host.spell), Zone::Stack);
    assert_eq!(pool(&host.runner), 1);
}

/// CR 118.9d: additional costs apply to an alternative cost, so paying the
/// alternative {1} still pays the splice cost {1}.
#[test]
fn splice_mana_is_charged_beside_a_paid_alternative_mana_cost() {
    let mut host = arcane_host(&format!("{ALTERNATIVE_ONE}\n{DESTROY_ONE}"), true, true);
    let host_cast = cast_host(&mut host);
    assert_eq!(host_cast.optional_offers, 1);
    assert_eq!(zone(&host.runner, host.spell), Zone::Stack);
    assert_eq!(pool(&host.runner), 2);
}

/// A {1}{B} Arcane host spliced with a {1} card, beside a coloured-only {B}
/// reducer and a spilling {B} reducer, with {B}{B}{B}{B} in the pool. The
/// caster applies the `coloured_only_first` reducer's reduction first.
fn elected_splice_cast(coloured_only_first: bool) -> (GameRunner, ObjectId, Cast) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let coloured_only = scenario
        .add_artifact_from_oracle(P0, "Tide Idol", COLOURED_ONLY_LESS)
        .id();
    let spilling = scenario
        .add_artifact_from_oracle(P0, "Tide Charm", SPILLING_LESS)
        .id();
    let target = two_marks(&mut scenario);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Quiet Host", true, DESTROY_ONE)
        .with_subtypes(vec!["Arcane"])
        .with_mana_cost(cost(vec![ManaCostShard::Black], 1))
        .id();
    let splicer = add_spare_splice(&mut scenario);
    scenario.with_mana_pool(P0, units(ManaType::Black, 4));
    let mut runner = scenario.build();
    let answers = Answers {
        target,
        splice: Some(splicer),
        pay_optional: true,
        pick: None,
        first_reducer: Some(if coloured_only_first {
            coloured_only
        } else {
            spilling
        }),
    };
    let host_cast = cast(&mut runner, spell, answers);
    (runner, spell, host_cast)
}

/// CR 601.2f: the total is {1}{B} plus the splice cost {1}, i.e. {2}{B}. The
/// coloured-only {B} applied first removes the {B}, leaving {2}; the spilling
/// {B} then finds no {B} and reduces the generic part, locking {1}.
#[test]
fn splice_mana_is_charged_when_the_coloured_only_reduction_is_elected_first() {
    let (runner, spell, host_cast) = elected_splice_cast(true);
    assert!(host_cast.prompted("OrderCostReductions"));
    assert_eq!(zone(&runner, spell), Zone::Stack);
    assert_eq!(pool(&runner), 3);
}

/// CR 601.2f: the spilling {B} applied first removes the {B} of {2}{B},
/// leaving {2}; the coloured-only {B} then finds no {B} to reduce, locking {2}.
#[test]
fn splice_mana_is_charged_when_the_spilling_reduction_is_elected_first() {
    let (runner, spell, host_cast) = elected_splice_cast(false);
    assert!(host_cast.prompted("OrderCostReductions"));
    assert_eq!(zone(&runner, spell), Zone::Stack);
    assert_eq!(pool(&runner), 2);
}

/// CR 702.172a: a chosen Spree mode's cost is an additional cost declared
/// before targets; it is charged beside an optional cost taken after them.
#[test]
fn spree_mode_cost_is_charged_beside_an_optional_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lands: Vec<ObjectId> = (0..3)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Black))
        .collect();
    let target = two_marks(&mut scenario);
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Quiet Host",
            true,
            &format!("{OPTIONAL_LAND}\n{SPREE_BODY}"),
        )
        .with_mana_cost(cost(vec![ManaCostShard::Black], 0))
        .id();
    scenario.with_mana_pool(P0, units(ManaType::Black, 4));
    let mut runner = scenario.build();
    let answers = Answers {
        target,
        splice: None,
        pay_optional: true,
        pick: None,
        first_reducer: None,
    };
    let spree = cast(&mut runner, spell, answers);
    assert!(spree.prompted("ModeChoice"));
    assert_eq!(spree.optional_offers, 1);
    assert_eq!(zone(&runner, spell), Zone::Stack);
    assert_eq!(lands_on_battlefield(&runner, &lands), 2);
    // {B} plus the chosen mode's {1}.
    assert_eq!(pool(&runner), 2);
}

struct Printed {
    runner: GameRunner,
    spell: ObjectId,
    spirit: ObjectId,
    answers: Answers,
}

/// Devouring Rage ({4}{R}) beside Kodama's Might (splice onto Arcane {G}), a
/// Spirit to sacrifice, two opponent creatures, and `mana` in the pool.
fn devouring_rage_spliced(mana: Vec<ManaUnit>) -> Printed {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spirit = scenario
        .add_creature(P0, "Husk Alpha", 2, 2)
        .with_subtypes(vec!["Spirit"])
        .with_keyword(Keyword::Shroud)
        .id();
    let target = two_marks(&mut scenario);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Devouring Rage", true, DEVOURING_RAGE)
        .with_subtypes(vec!["Arcane"])
        .with_mana_cost(cost(vec![ManaCostShard::Red], 4))
        .id();
    let splicer = scenario
        .add_spell_to_hand_from_oracle(P0, "Kodama's Might", true, "")
        .with_subtypes(vec!["Arcane"])
        .from_oracle_text_with_keywords(&["Splice"], KODAMAS_MIGHT)
        .with_mana_cost(cost(vec![ManaCostShard::Green], 0))
        .id();
    scenario.with_mana_pool(P0, mana);
    Printed {
        runner: scenario.build(),
        spell,
        spirit,
        answers: Answers {
            target,
            splice: Some(splicer),
            pay_optional: true,
            pick: Some(spirit),
            first_reducer: None,
        },
    }
}

fn red_green_colorless(colorless: usize) -> Vec<ManaUnit> {
    let mut mana = units(ManaType::Red, 1);
    mana.extend(units(ManaType::Green, 1));
    mana.extend(units(ManaType::Colorless, colorless));
    mana
}

#[test]
fn devouring_rage_spliced_with_kodamas_might_pays_the_splice_cost() {
    let mut printed = devouring_rage_spliced(red_green_colorless(4));
    let spell = printed.spell;
    let rage = cast(&mut printed.runner, spell, printed.answers);
    assert!(!rage.refused);
    assert_eq!(rage.optional_offers, 1);
    assert_eq!(zone(&printed.runner, spell), Zone::Stack);
    assert_eq!(zone(&printed.runner, printed.spirit), Zone::Graveyard);
    // {4}{R} plus the splice cost {G}: the pool held exactly that.
    assert_eq!(pool(&printed.runner), 0);
}

#[test]
fn devouring_rage_spliced_with_kodamas_might_is_refused_short_of_the_splice_cost() {
    let mut printed = devouring_rage_spliced(red_green_colorless(3));
    let spell = printed.spell;
    let rage = cast(&mut printed.runner, spell, printed.answers);
    assert!(rage.refused, "{{3}}{{R}}{{G}} cannot pay {{4}}{{R}}{{G}}");
    // The splice was declared and the refusal came when the total was
    // determined after targets, not at announcement.
    assert!(rage.prompted("SpliceOffer"));
    assert_eq!(rage.prompts.last(), Some(&"TargetSelection"));
    assert_eq!(rage.optional_offers, 0);
    assert_eq!(zone(&printed.runner, spell), Zone::Hand);
    assert_eq!(zone(&printed.runner, printed.spirit), Zone::Battlefield);
    assert_eq!(pool(&printed.runner), 5);
}
