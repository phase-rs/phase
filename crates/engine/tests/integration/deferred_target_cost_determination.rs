//! CR 601.2c + CR 601.2f: a spell whose target declaration is deferred until
//! after X (or a cost taken before targets) completes that declaration either
//! by prompting, automatically (one legal choice), at random, or with no slot
//! left. Every completion determines the total cost through the same
//! authority, and an Emerge or Offering sacrifice taken before the declaration
//! is made once, with its reduction locked into the total once.
//!
//! Each unprompted row is paired with the prompted control (two legal targets)
//! it must read like. Fixture names contain no keyword and no Oracle word.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::casting_costs::ReductionProvenance;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, SpellCostSource, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{EmergeCost, Keyword};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const TAX: &str =
    "Spells your opponents cast that target this creature cost an additional 3 life to cast.";
const OBSERVER: &str = "Whenever this creature becomes the target of a spell, this creature deals 2 damage to that spell's controller.";
const MOD: &str = "This spell costs {1} less to cast if it targets a red creature.";
const MORE: &str = "This spell costs {1} more to cast if it targets a red creature.";
const REQ_DISCARD: &str = "As an additional cost to cast this spell, discard a card.";
const REQ_LAND: &str = "As an additional cost to cast this spell, sacrifice a land.";
const CHOICE: &str =
    "As an additional cost to cast this spell, sacrifice a land or discard a card.";
const OPTIONAL: &str = "As an additional cost to cast this spell, you may sacrifice a land.";
const OPTIONAL_MANA: &str = "As an additional cost to cast this spell, you may pay {1}.";
const OPTIONAL_EVIDENCE: &str =
    "As an additional cost to cast this spell, you may collect evidence 2.";
const DESTROY_ONE: &str = "Destroy target creature an opponent controls.";
const DESTROY_X: &str = "Destroy X target creatures.";
const DESTROY_RANDOM: &str = "Destroy a random target creature.";
const MODAL_X: &str = "Choose one —\n• Destroy X target creatures.\n• You gain 1 life.";
const MODAL_ONE: &str =
    "Choose one —\n• Destroy target creature an opponent controls.\n• You gain 3 life.";
const SPLICE_GAIN: &str = "Splice onto Arcane {1}\nYou gain 2 life.";
const BLUE_SORCERY_LESS: &str = "Sorcery spells you cast cost {U} less to cast. This effect reduces only the amount of colored mana you pay.";

// Verbatim Oracle text (MTGJSON AtomicCards).
const CALL_FOR_BLOOD: &str = "As an additional cost to cast this spell, sacrifice a creature.\nTarget creature gets -X/-X until end of turn, where X is the sacrificed creature's power.";
const DEVOURING_RAGE: &str = "As an additional cost to cast this spell, you may sacrifice any number of Spirits.\nTarget creature gets +3/+0 until end of turn. For each Spirit sacrificed this way, that creature gets an additional +3/+0 until end of turn.";
const MARCH_OF_OTHERWORLDLY_LIGHT: &str = "As an additional cost to cast this spell, you may exile any number of white cards from your hand. This spell costs {2} less to cast for each card exiled this way.\nExile target artifact, creature, or enchantment with mana value X or less.";
const MARCH_OF_SWIRLING_MIST: &str = "As an additional cost to cast this spell, you may exile any number of blue cards from your hand. This spell costs {2} less to cast for each card exiled this way.\nUp to X target creatures phase out. (While they're phased out, they're treated as though they don't exist. Each one phases in before its controller untaps during their next untap step.)";
const SICKENING_SHOAL: &str = "You may exile a black card with mana value X from your hand rather than pay this spell's mana cost.\nTarget creature gets -X/-X until end of turn.";
const AETHER_TIDE: &str = "As an additional cost to cast this spell, discard X creature cards.\nReturn X target creatures to their owners' hands.";
const SCORCHED_EARTH: &str =
    "As an additional cost to cast this spell, discard X land cards.\nDestroy X target lands.";
const KODAMAS_MIGHT: &str = "Target creature gets +2/+2 until end of turn.\nSplice onto Arcane {G} (As you cast an Arcane spell, you may reveal this card from your hand and pay its splice cost. If you do, add this card's effects to that spell.)";
const MARDU_OUTRIDER: &str = "As an additional cost to cast this spell, discard a card.";
const CRESTING_MOSASAURUS: &str = "Emerge {6}{U} (You may cast this spell by sacrificing a creature and paying the emerge cost reduced by that creature's mana value.)\nWhen this creature enters, if you cast it, return each non-Dinosaur creature to its owner's hand.";
const DEFILER_OF_DREAMS: &str = "Flying\nAs an additional cost to cast blue permanent spells, you may pay 2 life. Those spells cost {U} less to cast if you paid life this way. This effect reduces only the amount of blue mana you pay.\nWhenever you cast a blue permanent spell, draw a card.";

fn units(mana_type: ManaType, n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(mana_type, ObjectId(0), false, vec![]))
        .collect()
}

fn blue_and_colorless(blue: usize, colorless: usize) -> Vec<ManaUnit> {
    let mut pool = units(ManaType::Blue, blue);
    pool.extend(units(ManaType::Colorless, colorless));
    pool
}

fn cost(shards: Vec<ManaCostShard>, generic: u32) -> ManaCost {
    ManaCost::Cost { shards, generic }
}

fn black(n: usize) -> Vec<ManaCostShard> {
    vec![ManaCostShard::Black; n]
}

fn blue(n: usize) -> Vec<ManaCostShard> {
    vec![ManaCostShard::Blue; n]
}

fn with_extra(extra: &str, body: &str) -> String {
    if extra.is_empty() {
        body.to_string()
    } else {
        format!("{extra}\n{body}")
    }
}

/// How each prompt of a cast is answered.
#[derive(Clone, Default)]
struct Answers {
    x: u32,
    target: Option<ObjectId>,
    pay_optional: bool,
    /// The caster's preferred object for a cost selection.
    pick: Option<ObjectId>,
    splice: Option<ObjectId>,
    alt: Option<AlternativeCastDecision>,
    defiler_pay: Option<bool>,
}

/// What a cast did up to the point the spell is on the stack (or refused).
struct Cast {
    prompts: Vec<&'static str>,
    optional_offers: usize,
    becomes_target: usize,
    refused: bool,
    elections: Vec<Vec<ReductionProvenance>>,
}

impl Cast {
    fn prompted(&self, kind: &str) -> bool {
        self.prompts.contains(&kind)
    }

    fn prompts_for_targets(&self) -> bool {
        self.prompted("TargetSelection")
    }
}

fn cast_action(runner: &GameRunner, spell: ObjectId) -> GameAction {
    GameAction::CastSpell {
        object_id: spell,
        card_id: runner.state().objects[&spell].card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    }
}

/// Cast `spell` and answer every prompt from `answers` until priority returns.
fn cast(runner: &mut GameRunner, spell: ObjectId, answers: &Answers) -> Cast {
    let first = cast_action(runner, spell);
    drive(runner, spell, first, answers)
}

fn drive(runner: &mut GameRunner, source: ObjectId, first: GameAction, answers: &Answers) -> Cast {
    let mut events: Vec<GameEvent> = Vec::new();
    let mut out = Cast {
        prompts: Vec::new(),
        optional_offers: 0,
        becomes_target: 0,
        refused: false,
        elections: Vec::new(),
    };
    match runner.act(first) {
        Ok(result) => events.extend(result.events),
        Err(_) => out.refused = true,
    }
    while !out.refused {
        let waiting_for = runner.state().waiting_for.clone();
        let action = match waiting_for {
            WaitingFor::ChooseXValue { .. } => GameAction::ChooseX { value: answers.x },
            WaitingFor::ModeChoice { .. } => GameAction::SelectModes { indices: vec![0] },
            WaitingFor::TargetSelection { selection, .. } => GameAction::ChooseTarget {
                target: answers
                    .target
                    .map(TargetRef::Object)
                    .or_else(|| selection.current_legal_targets.first().cloned()),
            },
            WaitingFor::OptionalCostChoice { .. } => {
                out.optional_offers += 1;
                GameAction::DecideOptionalCost {
                    pay: answers.pay_optional,
                }
            }
            WaitingFor::DefilerPayment { .. } => GameAction::DecideOptionalCost {
                pay: answers.defiler_pay.unwrap_or(answers.pay_optional),
            },
            WaitingFor::AlternativeCastChoice { .. } => GameAction::ChooseAlternativeCast {
                choice: answers.alt.unwrap_or(AlternativeCastDecision::Alternative),
            },
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
            WaitingFor::CollectEvidenceChoice { cards, .. } => GameAction::SelectCards { cards },
            WaitingFor::ManaPayment { .. } => GameAction::PassPriority,
            WaitingFor::SpliceOffer { .. } => GameAction::RespondToSpliceOffer {
                card: answers.splice,
            },
            WaitingFor::OrderCostReductions { ref reductions, .. } => {
                out.elections
                    .push(reductions.iter().map(|entry| entry.provenance).collect());
                GameAction::OrderCostReductions {
                    order: (0..reductions.len()).collect(),
                    hybrid_announcement: vec![],
                }
            }
            _ => break,
        };
        out.prompts.push(runner.waiting_for_kind());
        match runner.act(action) {
            Ok(result) => events.extend(result.events),
            Err(_) => out.refused = true,
        }
    }
    out.becomes_target = events
        .iter()
        .filter(|event| {
            matches!(event, GameEvent::BecomesTarget { source_id, .. } if *source_id == source)
        })
        .count();
    out
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn pool(runner: &GameRunner) -> usize {
    runner.state().players[0].mana_pool.total()
}

fn life(runner: &GameRunner) -> i32 {
    runner.state().players[0].life
}

fn on_battlefield(runner: &GameRunner, ids: &[ObjectId]) -> usize {
    ids.iter()
        .filter(|id| zone(runner, **id) == Zone::Battlefield)
        .count()
}

fn observer_triggers_on_stack(runner: &GameRunner, observers: &[ObjectId]) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|entry| {
            matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
                && observers.contains(&entry.source_id)
        })
        .count()
}

fn victims(scenario: &mut GameScenario, n: usize, oracle: &str, color: ManaColor) -> Vec<ObjectId> {
    (0..n)
        .map(|i| {
            let name = format!("Mark {i}");
            let mut builder = if oracle.is_empty() {
                scenario.add_creature(P1, &name, 2, 2)
            } else {
                scenario.add_creature_from_oracle(P1, &name, 2, 2, oracle)
            };
            builder.with_color(vec![color]);
            builder.id()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The deferred-X class: X announced, then targets declared.
// ---------------------------------------------------------------------------

/// How the deferred declaration completes.
#[derive(Clone, Copy)]
enum Completion {
    /// One legal target: the automatic declaration.
    Automatic,
    /// Two legal targets: the prompted control.
    Prompted,
    /// X = 0: no target slot is left.
    EmptySlot,
}

struct DeferredX {
    runner: GameRunner,
    spell: ObjectId,
    lands: Vec<ObjectId>,
    filler: ObjectId,
    victims: Vec<ObjectId>,
}

struct DeferredXBoard<'a> {
    body: &'a str,
    extra: &'a str,
    victim_text: &'a str,
    generic_x: bool,
    lands: usize,
    life: Option<i32>,
}

impl Default for DeferredXBoard<'_> {
    fn default() -> Self {
        DeferredXBoard {
            body: DESTROY_X,
            extra: "",
            victim_text: "",
            generic_x: false,
            lands: 3,
            life: None,
        }
    }
}

impl DeferredXBoard<'_> {
    fn build(&self, completion: Completion) -> DeferredX {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        if let Some(life) = self.life {
            scenario.with_life(P0, life);
        }
        let lands = (0..self.lands)
            .map(|_| scenario.add_basic_land(P0, ManaColor::Black))
            .collect();
        let filler = scenario.add_card_to_hand(P0, "Spare Page");
        let n = match completion {
            Completion::Prompted => 2,
            Completion::Automatic | Completion::EmptySlot => 1,
        };
        let victims = victims(&mut scenario, n, self.victim_text, ManaColor::Red);
        let spell = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Probe Hex",
                false,
                &with_extra(self.extra, self.body),
            )
            .as_sorcery()
            .with_mana_cost(if self.generic_x {
                cost(vec![ManaCostShard::X], 2)
            } else {
                cost(vec![ManaCostShard::X, ManaCostShard::Black], 0)
            })
            .id();
        scenario.with_mana_pool(P0, units(ManaType::Black, 4));
        DeferredX {
            runner: scenario.build(),
            spell,
            lands,
            filler,
            victims,
        }
    }

    fn cast(&self, completion: Completion, pay_optional: bool) -> (DeferredX, Cast) {
        let mut board = self.build(completion);
        let answers = Answers {
            x: match completion {
                Completion::EmptySlot => 0,
                Completion::Automatic | Completion::Prompted => 1,
            },
            target: board.victims.first().copied(),
            pay_optional,
            ..Answers::default()
        };
        let cast = cast(&mut board.runner, board.spell, &answers);
        (board, cast)
    }
}

/// The cost reading a deferred-X cast leaves behind once the spell is on the
/// stack: lands left, whether the filler was discarded, mana left, life.
fn cost_reading(board: &DeferredX) -> (usize, Zone, usize, i32) {
    (
        on_battlefield(&board.runner, &board.lands),
        zone(&board.runner, board.filler),
        pool(&board.runner),
        life(&board.runner),
    )
}

fn assert_unprompted_reads_prompted(board: &DeferredXBoard<'_>, pay_optional: bool) -> Cast {
    let (prompted, prompted_cast) = board.cast(Completion::Prompted, pay_optional);
    assert!(prompted_cast.prompts_for_targets(), "control must prompt");
    let (automatic, automatic_cast) = board.cast(Completion::Automatic, pay_optional);
    assert!(!automatic_cast.prompts_for_targets());
    assert!(
        !automatic_cast.refused,
        "the automatic cast must reach the stack"
    );
    assert_eq!(zone(&automatic.runner, automatic.spell), Zone::Stack);
    assert_eq!(
        cost_reading(&automatic),
        cost_reading(&prompted),
        "the automatic declaration determines the cost the prompted one does"
    );
    assert_eq!(
        automatic_cast.optional_offers,
        prompted_cast.optional_offers
    );
    automatic_cast
}

#[test]
fn automatic_declaration_charges_required_land_sacrifice_once() {
    let board = DeferredXBoard {
        extra: REQ_LAND,
        ..DeferredXBoard::default()
    };
    assert_unprompted_reads_prompted(&board, true);
    let (automatic, _) = board.cast(Completion::Automatic, true);
    assert_eq!(on_battlefield(&automatic.runner, &automatic.lands), 2);
}

#[test]
fn automatic_declaration_charges_required_discard_once() {
    for body in [DESTROY_X, MODAL_X] {
        let board = DeferredXBoard {
            body,
            extra: REQ_DISCARD,
            ..DeferredXBoard::default()
        };
        assert_unprompted_reads_prompted(&board, true);
        let (automatic, _) = board.cast(Completion::Automatic, true);
        assert_eq!(zone(&automatic.runner, automatic.filler), Zone::Graveyard);
    }
}

#[test]
fn automatic_declaration_offers_optional_cost_accepted() {
    let board = DeferredXBoard {
        extra: OPTIONAL,
        ..DeferredXBoard::default()
    };
    let automatic = assert_unprompted_reads_prompted(&board, true);
    assert_eq!(automatic.optional_offers, 1);
    let (automatic, _) = board.cast(Completion::Automatic, true);
    assert_eq!(on_battlefield(&automatic.runner, &automatic.lands), 2);
}

#[test]
fn automatic_declaration_offers_optional_cost_declined() {
    let board = DeferredXBoard {
        extra: OPTIONAL,
        ..DeferredXBoard::default()
    };
    let automatic = assert_unprompted_reads_prompted(&board, false);
    assert_eq!(automatic.optional_offers, 1);
    let (automatic, _) = board.cast(Completion::Automatic, false);
    assert_eq!(on_battlefield(&automatic.runner, &automatic.lands), 3);
}

#[test]
fn automatic_declaration_offers_choice_cost_accepted() {
    let board = DeferredXBoard {
        extra: CHOICE,
        ..DeferredXBoard::default()
    };
    let automatic = assert_unprompted_reads_prompted(&board, true);
    assert_eq!(automatic.optional_offers, 1);
    let (automatic, _) = board.cast(Completion::Automatic, true);
    assert_eq!(on_battlefield(&automatic.runner, &automatic.lands), 2);
    assert_eq!(zone(&automatic.runner, automatic.filler), Zone::Hand);
}

#[test]
fn automatic_declaration_offers_choice_cost_declined() {
    let board = DeferredXBoard {
        extra: CHOICE,
        ..DeferredXBoard::default()
    };
    let automatic = assert_unprompted_reads_prompted(&board, false);
    assert_eq!(automatic.optional_offers, 1);
    let (automatic, _) = board.cast(Completion::Automatic, false);
    assert_eq!(on_battlefield(&automatic.runner, &automatic.lands), 3);
    assert_eq!(zone(&automatic.runner, automatic.filler), Zone::Graveyard);
}

#[test]
fn automatic_declaration_applies_target_dependent_modification() {
    let board = DeferredXBoard {
        extra: MOD,
        generic_x: true,
        ..DeferredXBoard::default()
    };
    assert_unprompted_reads_prompted(&board, true);
    // {X}{2} at X = 1, one less for targeting a red creature: 2 of 4 spent.
    let (automatic, _) = board.cast(Completion::Automatic, true);
    assert_eq!(pool(&automatic.runner), 2);
}

#[test]
fn automatic_declaration_charges_imposed_life_tax() {
    let board = DeferredXBoard {
        victim_text: TAX,
        ..DeferredXBoard::default()
    };
    assert_unprompted_reads_prompted(&board, true);
    let (automatic, _) = board.cast(Completion::Automatic, true);
    assert_eq!(life(&automatic.runner), 17);
}

#[test]
fn empty_slot_declaration_determines_costs_as_prompted() {
    let board = DeferredXBoard {
        extra: REQ_LAND,
        ..DeferredXBoard::default()
    };
    let (empty, empty_cast) = board.cast(Completion::EmptySlot, true);
    assert!(!empty_cast.refused);
    assert!(!empty_cast.prompts_for_targets());
    assert_eq!(zone(&empty.runner, empty.spell), Zone::Stack);
    assert!(empty_cast.prompted("PayCost"));
    assert_eq!(on_battlefield(&empty.runner, &empty.lands), 2);
    assert_eq!(empty_cast.becomes_target, 0);
}

#[test]
fn automatic_declaration_refuses_unpayable_required_cost() {
    let board = DeferredXBoard {
        extra: REQ_LAND,
        lands: 0,
        ..DeferredXBoard::default()
    };
    for completion in [Completion::Automatic, Completion::EmptySlot] {
        let (refused, refused_cast) = board.cast(completion, true);
        assert!(refused_cast.refused, "no land can pay the sacrifice");
        assert_eq!(zone(&refused.runner, refused.spell), Zone::Hand);
        assert_eq!(pool(&refused.runner), 4);
    }
}

#[test]
fn automatic_declaration_refuses_unpayable_life_tax() {
    let board = DeferredXBoard {
        victim_text: TAX,
        life: Some(2),
        ..DeferredXBoard::default()
    };
    let (refused, refused_cast) = board.cast(Completion::Automatic, true);
    assert!(refused_cast.refused, "2 life cannot pay a 3-life tax");
    assert_eq!(zone(&refused.runner, refused.spell), Zone::Hand);
    assert_eq!(life(&refused.runner), 2);
}

// ---------------------------------------------------------------------------
// The random declaration: splice's offer completes a random-target host.
// ---------------------------------------------------------------------------

struct Arcane {
    runner: GameRunner,
    spell: ObjectId,
    splicer: ObjectId,
    lands: Vec<ObjectId>,
    filler: ObjectId,
    victims: Vec<ObjectId>,
}

fn arcane_host(body: &str, extra: &str, generic: bool, n: usize, victim_text: &str) -> Arcane {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lands = (0..3)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Black))
        .collect();
    let filler = scenario.add_card_to_hand(P0, "Spare Page");
    let victims = victims(&mut scenario, n, victim_text, ManaColor::Red);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Quiet Host", true, &with_extra(extra, body))
        .with_subtypes(vec!["Arcane"])
        .with_mana_cost(if generic {
            ManaCost::generic(2)
        } else {
            cost(black(2), 0)
        })
        .id();
    let splicer = scenario
        .add_spell_to_hand_from_oracle(P0, "Ember Note", true, "")
        .with_subtypes(vec!["Arcane"])
        .from_oracle_text_with_keywords(&["Splice"], SPLICE_GAIN)
        .with_mana_cost(cost(black(1), 0))
        .id();
    scenario.with_mana_pool(P0, units(ManaType::Black, 4));
    Arcane {
        runner: scenario.build(),
        spell,
        splicer,
        lands,
        filler,
        victims,
    }
}

/// Cast the host, splicing the Arcane card onto it (the splice offer defers
/// the host's target declaration).
fn cast_host(host: &mut Arcane, pay_optional: bool) -> Cast {
    let answers = Answers {
        pay_optional,
        pick: Some(host.filler),
        splice: Some(host.splicer),
        ..Answers::default()
    };
    let spell = host.spell;
    cast(&mut host.runner, spell, &answers)
}

#[test]
fn random_declaration_determines_costs_as_prompted() {
    // A random target is never prompted, with one legal creature or two.
    for n in [1, 2] {
        let mut required = arcane_host(DESTROY_RANDOM, REQ_DISCARD, false, n, "");
        let required_cast = cast_host(&mut required, true);
        assert!(!required_cast.prompts_for_targets());
        assert_eq!(zone(&required.runner, required.filler), Zone::Graveyard);
        assert_eq!(required_cast.becomes_target, 1);

        let mut optional = arcane_host(DESTROY_RANDOM, OPTIONAL, false, n, "");
        let optional_cast = cast_host(&mut optional, true);
        assert_eq!(optional_cast.optional_offers, 1);
        assert_eq!(on_battlefield(&optional.runner, &optional.lands), 2);
        // {B}{B} plus the {1} splice cost: 1 of 4.
        assert_eq!(pool(&optional.runner), 1);

        // {2} less {1} for the red target, plus the {1} splice cost: 2 of 4.
        let mut modified = arcane_host(DESTROY_RANDOM, MOD, true, n, "");
        cast_host(&mut modified, true);
        assert_eq!(pool(&modified.runner), 2);
    }
}

// ---------------------------------------------------------------------------
// Printed members of the deferred class, on one generic board.
// ---------------------------------------------------------------------------

struct Printed {
    runner: GameRunner,
    spell: ObjectId,
    fodder: ObjectId,
    black_card: ObjectId,
}

/// A spell with Oracle `text`, cost `mana_cost` and `subtypes` beside a
/// shroud Spirit (a sacrifice candidate no spell can target), a black card in
/// hand, a splice card, and `n` opponent creatures.
fn printed_board(
    name: &str,
    text: &str,
    mana_cost: ManaCost,
    instant: bool,
    subtypes: Vec<&str>,
    n: usize,
) -> Printed {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for color in [
        ManaColor::Black,
        ManaColor::Red,
        ManaColor::Blue,
        ManaColor::White,
        ManaColor::Green,
    ] {
        scenario.add_basic_land(P0, color);
    }
    let fodder = scenario
        .add_creature(P0, "Husk Alpha", 2, 2)
        .with_subtypes(vec!["Spirit"])
        .with_mana_cost(ManaCost::generic(1))
        .with_keyword(Keyword::Shroud)
        .id();
    scenario.add_card_to_hand(P0, "Spare Page");
    let black_card = scenario
        .add_spell_to_hand_from_oracle(P0, "Ember Note", true, "")
        .with_subtypes(vec!["Arcane"])
        .from_oracle_text_with_keywords(&["Splice"], SPLICE_GAIN)
        .with_mana_cost(cost(black(1), 0))
        .id();
    for _ in 0..5 {
        scenario.add_card_to_library_top(P0, "Deep Page");
        scenario.add_card_to_library_top(P1, "Deep Page");
    }
    for i in 0..n {
        scenario
            .add_creature(P1, &format!("Mark {i}"), 1, 1)
            .with_color(vec![ManaColor::Red, ManaColor::Blue])
            .with_mana_cost(ManaCost::generic(1));
    }
    let spell = {
        let mut builder = scenario.add_spell_to_hand_from_oracle(P0, name, instant, text);
        builder.with_mana_cost(mana_cost);
        if !instant {
            builder.as_sorcery();
        }
        if !subtypes.is_empty() {
            builder.with_subtypes(subtypes);
        }
        builder.id()
    };
    let mut mana = Vec::new();
    for mana_type in [
        ManaType::White,
        ManaType::Blue,
        ManaType::Black,
        ManaType::Red,
        ManaType::Green,
        ManaType::Colorless,
    ] {
        mana.extend(units(mana_type, 5));
    }
    scenario.with_mana_pool(P0, mana);
    Printed {
        runner: scenario.build(),
        spell,
        fodder,
        black_card,
    }
}

fn cast_printed(
    name: &str,
    text: &str,
    mana_cost: ManaCost,
    instant: bool,
    subtypes: Vec<&str>,
    n: usize,
    x: u32,
) -> (Printed, Cast) {
    let mut board = printed_board(name, text, mana_cost, instant, subtypes, n);
    let answers = Answers {
        x,
        pay_optional: true,
        pick: Some(board.fodder),
        alt: Some(AlternativeCastDecision::Normal),
        ..Answers::default()
    };
    let spell = board.spell;
    let cast = cast(&mut board.runner, spell, &answers);
    (board, cast)
}

/// Cast the printed card with one legal target (automatic) and with two
/// (prompted), and return both.
fn printed_pair(
    name: &str,
    text: &str,
    mana_cost: ManaCost,
    instant: bool,
    subtypes: Vec<&str>,
) -> ((Printed, Cast), (Printed, Cast)) {
    let automatic = cast_printed(
        name,
        text,
        mana_cost.clone(),
        instant,
        subtypes.clone(),
        1,
        1,
    );
    let prompted = cast_printed(name, text, mana_cost, instant, subtypes, 2, 1);
    assert!(!automatic.1.prompts_for_targets());
    assert!(prompted.1.prompts_for_targets());
    (automatic, prompted)
}

#[test]
fn call_for_blood_unprompted_completion_reads_prompted_cost() {
    let ((automatic, automatic_cast), (prompted, _)) = printed_pair(
        "Call for Blood",
        CALL_FOR_BLOOD,
        cost(black(1), 4),
        true,
        vec!["Arcane"],
    );
    assert!(automatic_cast.prompted("PayCost"));
    assert_eq!(zone(&automatic.runner, automatic.fodder), Zone::Graveyard);
    assert_eq!(zone(&prompted.runner, prompted.fodder), Zone::Graveyard);
    assert_eq!(automatic_cast.becomes_target, 1);
}

#[test]
fn devouring_rage_unprompted_completion_reads_prompted_cost() {
    let ((automatic, automatic_cast), (prompted, prompted_cast)) = printed_pair(
        "Devouring Rage",
        DEVOURING_RAGE,
        cost(vec![ManaCostShard::Red], 4),
        true,
        vec!["Arcane"],
    );
    assert_eq!(automatic_cast.optional_offers, 1);
    assert_eq!(prompted_cast.optional_offers, 1);
    assert_eq!(zone(&automatic.runner, automatic.fodder), Zone::Graveyard);
    assert_eq!(zone(&prompted.runner, prompted.fodder), Zone::Graveyard);
}

#[test]
fn march_of_otherworldly_light_unprompted_completion_reads_prompted_cost() {
    let ((automatic, automatic_cast), (_, prompted_cast)) = printed_pair(
        "March of Otherworldly Light",
        MARCH_OF_OTHERWORLDLY_LIGHT,
        cost(vec![ManaCostShard::X, ManaCostShard::White], 0),
        true,
        vec![],
    );
    assert_eq!(automatic_cast.optional_offers, 1);
    assert_eq!(prompted_cast.optional_offers, 1);
    assert_eq!(zone(&automatic.runner, automatic.spell), Zone::Stack);
}

#[test]
fn march_of_swirling_mist_x0_unprompted_completion_reads_prompted_cost() {
    let (empty, empty_cast) = cast_printed(
        "March of Swirling Mist",
        MARCH_OF_SWIRLING_MIST,
        cost(vec![ManaCostShard::X, ManaCostShard::Blue], 0),
        true,
        vec![],
        1,
        0,
    );
    let (_, prompted_cast) = cast_printed(
        "March of Swirling Mist",
        MARCH_OF_SWIRLING_MIST,
        cost(vec![ManaCostShard::X, ManaCostShard::Blue], 0),
        true,
        vec![],
        2,
        1,
    );
    assert_eq!(prompted_cast.optional_offers, 1);
    assert_eq!(empty_cast.optional_offers, 1);
    assert_eq!(zone(&empty.runner, empty.spell), Zone::Stack);
}

#[test]
fn sickening_shoal_unprompted_completion_reads_prompted_cost() {
    let ((automatic, automatic_cast), (prompted, _)) = printed_pair(
        "Sickening Shoal",
        SICKENING_SHOAL,
        cost(
            vec![ManaCostShard::X, ManaCostShard::Black, ManaCostShard::Black],
            0,
        ),
        true,
        vec!["Arcane"],
    );
    // CR 118.9: the alternative cost exiles the black card instead of mana.
    assert_eq!(automatic_cast.optional_offers, 1);
    assert_eq!(zone(&automatic.runner, automatic.black_card), Zone::Exile);
    assert_eq!(pool(&automatic.runner), 30);
    assert_eq!(pool(&automatic.runner), pool(&prompted.runner));
    assert_eq!(zone(&prompted.runner, prompted.black_card), Zone::Exile);
}

#[test]
fn kodamas_might_unprompted_declaration_emits_one_targeting_event() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P1, "Mark 0", 1, 1);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Kodama's Might", true, "")
        .with_subtypes(vec!["Arcane"])
        .from_oracle_text_with_keywords(&["Splice"], KODAMAS_MIGHT)
        .with_mana_cost(cost(vec![ManaCostShard::Green], 0))
        .id();
    scenario
        .add_spell_to_hand_from_oracle(P0, "Ember Note", true, "")
        .with_subtypes(vec!["Arcane"])
        .from_oracle_text_with_keywords(&["Splice"], SPLICE_GAIN)
        .with_mana_cost(cost(black(1), 0));
    let mut mana = units(ManaType::Green, 2);
    mana.extend(units(ManaType::Black, 2));
    scenario.with_mana_pool(P0, mana);
    let mut runner = scenario.build();
    let kodama = cast(&mut runner, spell, &Answers::default());
    assert!(kodama.prompted("SpliceOffer"));
    assert!(!kodama.prompts_for_targets());
    assert_eq!(kodama.becomes_target, 1);
    assert_eq!(zone(&runner, spell), Zone::Stack);
}

// ---------------------------------------------------------------------------
// A printed Required cost whose X comes from the mana cost.
// ---------------------------------------------------------------------------

/// Cast with `targets` opponent permanents and one matching card in hand, at
/// X = 1. Returns the runner, the spell and the matching card.
fn discard_x_board(
    text: &str,
    name: &str,
    color: ManaCostShard,
    lands: bool,
    targets: usize,
) -> (GameRunner, ObjectId, ObjectId, Cast) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..targets {
        if lands {
            scenario.add_basic_land(P1, ManaColor::Green);
        } else {
            scenario.add_creature(P1, &format!("Mark {i}"), 2, 2);
        }
    }
    let matching = if lands {
        scenario.add_land_to_hand(P0, "Spare Field").id()
    } else {
        scenario.add_creature_to_hand(P0, "Spare Beast", 1, 1).id()
    };
    scenario.add_card_to_hand(P0, "Spare Page");
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, name, false, text)
        .as_sorcery()
        .with_mana_cost(cost(vec![ManaCostShard::X, color], 0))
        .id();
    let mana_type = match color {
        ManaCostShard::Red => ManaType::Red,
        _ => ManaType::Blue,
    };
    scenario.with_mana_pool(P0, units(mana_type, 3));
    let mut runner = scenario.build();
    let answers = Answers {
        x: 1,
        pick: Some(matching),
        ..Answers::default()
    };
    let cast = cast(&mut runner, spell, &answers);
    (runner, spell, matching, cast)
}

/// CR 107.3a + CR 601.2f + CR 601.2h: the unprompted completion discards the
/// announced X matching cards, exactly as the prompted control does.
fn assert_discard_x_reads_prompted(text: &str, name: &str, color: ManaCostShard, lands: bool) {
    let (prompted, _, prompted_card, prompted_cast) = discard_x_board(text, name, color, lands, 2);
    assert!(prompted_cast.prompts_for_targets());
    assert_eq!(zone(&prompted, prompted_card), Zone::Graveyard);
    let (automatic, spell, automatic_card, automatic_cast) =
        discard_x_board(text, name, color, lands, 1);
    assert!(!automatic_cast.prompts_for_targets());
    assert!(!automatic_cast.refused);
    assert_eq!(zone(&automatic, spell), Zone::Stack);
    assert_eq!(zone(&automatic, automatic_card), Zone::Graveyard);
    assert_eq!(pool(&automatic), pool(&prompted));
}

#[test]
fn aether_tide_unprompted_completion_reads_prompted_cost() {
    assert_discard_x_reads_prompted(AETHER_TIDE, "Aether Tide", ManaCostShard::Blue, false);
}

#[test]
fn scorched_earth_unprompted_completion_reads_prompted_cost() {
    assert_discard_x_reads_prompted(SCORCHED_EARTH, "Scorched Earth", ManaCostShard::Red, true);
}

// ---------------------------------------------------------------------------
// The kept pre-declaration cost: Emerge and Offering taken before targets.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Route {
    /// A targeted sorcery with emerge {4}; fodders of mana value 2 (chosen)
    /// and 1.
    Emerge,
    /// A targeted sorcery with Spirit offering cast in the upkeep, so the
    /// Offering is taken before targets.
    OfferingInstant,
    /// The same, cast in the main phase: the Offering is an optional cost.
    OfferingMain,
    /// A modal instant-speed Offering spell.
    ModalOfferingInstant,
    /// Casualty 1 on a targeted spell.
    Casualty,
    /// Kicker on a spell whose kicked mode adds a target.
    Kicker,
    /// A Required "sacrifice X lands" spell.
    RequiredX,
}

struct Kept {
    runner: GameRunner,
    spell: ObjectId,
    filler: ObjectId,
    fodders: Vec<ObjectId>,
    victims: Vec<ObjectId>,
    evidence: Option<ObjectId>,
}

struct KeptBoard<'a> {
    route: Route,
    extra: &'a str,
    victim_text: &'a str,
    /// The Offering spells' mana cost.
    offering_cost: ManaCost,
    mana: usize,
    lands: usize,
    victim_color: ManaColor,
    /// The mana value of a creature card in the graveyard to collect as evidence.
    evidence: Option<u32>,
}

impl<'a> KeptBoard<'a> {
    fn new(route: Route) -> Self {
        KeptBoard {
            route,
            extra: "",
            victim_text: "",
            offering_cost: cost(black(3), 0),
            mana: 4,
            lands: 3,
            victim_color: ManaColor::Red,
            evidence: None,
        }
    }

    fn build(&self, n: usize) -> Kept {
        let mut scenario = GameScenario::new();
        scenario.at_phase(match self.route {
            Route::OfferingInstant | Route::ModalOfferingInstant => Phase::Upkeep,
            _ => Phase::PreCombatMain,
        });
        for _ in 0..self.lands {
            scenario.add_basic_land(P0, ManaColor::Black);
        }
        let filler = scenario.add_card_to_hand(P0, "Spare Page");
        let victims = victims(&mut scenario, n, self.victim_text, self.victim_color);
        let evidence = self.evidence.map(|mv| {
            scenario
                .add_creature_to_graveyard(P0, "Case File", 1, 1)
                .with_mana_cost(ManaCost::generic(mv))
                .id()
        });
        let body = match self.route {
            Route::RequiredX => {
                "As an additional cost to cast this spell, sacrifice X lands.\nDestroy X target creatures."
            }
            Route::Kicker => {
                "Kicker {1}\nDestroy target creature with mana value 2 or less. If this spell was kicked, instead destroy target creature."
            }
            Route::Casualty => "Casualty 1\nDestroy target creature an opponent controls.",
            Route::ModalOfferingInstant => MODAL_ONE,
            Route::Emerge | Route::OfferingInstant | Route::OfferingMain => DESTROY_ONE,
        };
        let oracle = with_extra(self.extra, body);
        let mut fodders = Vec::new();
        let spell = match self.route {
            Route::Emerge => {
                for (name, mv) in [("Husk Alpha", 2), ("Husk Beta", 1)] {
                    fodders.push(
                        scenario
                            .add_creature(P0, name, 2, 2)
                            .with_mana_cost(ManaCost::generic(mv))
                            .id(),
                    );
                }
                scenario
                    .add_spell_to_hand_from_oracle(P0, "Probe Hex", false, &oracle)
                    .with_mana_cost(ManaCost::generic(8))
                    .with_keyword(Keyword::Emerge(EmergeCost::creature(ManaCost::generic(4))))
                    .id()
            }
            Route::OfferingInstant | Route::OfferingMain | Route::ModalOfferingInstant => {
                for (name, mv) in [("Wisp Alpha", 2), ("Wisp Beta", 1)] {
                    fodders.push(
                        scenario
                            .add_creature(P0, name, 2, 2)
                            .with_subtypes(vec!["Spirit"])
                            .with_mana_cost(ManaCost::generic(mv))
                            .id(),
                    );
                }
                scenario
                    .add_spell_to_hand_from_oracle(P0, "Probe Hex", false, &oracle)
                    .with_mana_cost(self.offering_cost.clone())
                    .with_keyword(Keyword::Offering("Spirit".into()))
                    .id()
            }
            Route::Casualty => {
                fodders.push(scenario.add_creature(P0, "Husk Alpha", 2, 2).id());
                scenario
                    .add_spell_to_hand_from_oracle(P0, "Probe Hex", false, &oracle)
                    .with_mana_cost(cost(black(1), 0))
                    .id()
            }
            Route::Kicker | Route::RequiredX => scenario
                .add_spell_to_hand_from_oracle(P0, "Probe Hex", false, &oracle)
                .with_mana_cost(cost(black(1), 0))
                .id(),
        };
        scenario.with_mana_pool(P0, units(ManaType::Black, self.mana));
        Kept {
            runner: scenario.build(),
            spell,
            filler,
            fodders,
            victims,
            evidence,
        }
    }

    fn cast(&self, n: usize, pay_optional: bool) -> (Kept, Cast) {
        let mut board = self.build(n);
        let answers = Answers {
            x: 1,
            target: board.victims.first().copied(),
            pay_optional,
            pick: board.fodders.first().copied(),
            ..Answers::default()
        };
        let spell = board.spell;
        let cast = cast(&mut board.runner, spell, &answers);
        (board, cast)
    }
}

/// The chosen fodder is sacrificed and the unchosen one stays.
fn assert_fodder_sacrificed_once(board: &Kept) {
    assert_eq!(zone(&board.runner, board.fodders[0]), Zone::Graveyard);
    assert_eq!(zone(&board.runner, board.fodders[1]), Zone::Battlefield);
}

#[test]
fn prompted_emerge_sacrifices_fodder_once() {
    let (emerge, emerge_cast) = KeptBoard::new(Route::Emerge).cast(2, true);
    assert!(emerge_cast.prompts_for_targets());
    assert_eq!(zone(&emerge.runner, emerge.spell), Zone::Stack);
    assert_fodder_sacrificed_once(&emerge);
    // {4} reduced by the sacrificed creature's mana value 2.
    assert_eq!(pool(&emerge.runner), 2);
}

/// CR 702.119c + CR 601.2f: an Emerge sacrifice chosen before a deferred
/// target declaration survives an optional collect-evidence payment (CR
/// 701.59a): the fodder is sacrificed once and its reduction applies once.
#[test]
fn probe_hex_with_synthetic_emerge_and_optional_evidence_sacrifices_fodder_once() {
    for n in [1, 2] {
        let (kept, kept_cast) = KeptBoard {
            extra: OPTIONAL_EVIDENCE,
            evidence: Some(2),
            ..KeptBoard::new(Route::Emerge)
        }
        .cast(n, true);
        assert!(kept_cast.prompted("CollectEvidenceChoice"), "{n} targets");
        assert_eq!(zone(&kept.runner, kept.spell), Zone::Stack, "{n} targets");
        assert_eq!(
            kept.evidence.map(|card| zone(&kept.runner, card)),
            Some(Zone::Exile),
            "{n} targets"
        );
        assert_fodder_sacrificed_once(&kept);
        // {4} reduced once by the sacrificed creature's mana value 2.
        assert_eq!(pool(&kept.runner), 2, "{n} targets");
    }
}

#[test]
fn prompted_instant_offering_offered_once() {
    let (offering, offering_cast) = KeptBoard::new(Route::OfferingInstant).cast(2, true);
    assert!(offering_cast.prompts_for_targets());
    assert_eq!(
        offering_cast.optional_offers, 0,
        "the Offering is not offered again"
    );
    assert_fodder_sacrificed_once(&offering);
    // {B}{B}{B}: the generic 2 of the sacrificed cost reduces nothing coloured.
    assert_eq!(pool(&offering.runner), 1);
}

#[test]
fn prompted_instant_offering_target_trigger_after_cast() {
    let board = KeptBoard {
        victim_text: OBSERVER,
        ..KeptBoard::new(Route::OfferingInstant)
    };
    let (mut offering, offering_cast) = board.cast(2, true);
    assert_eq!(offering_cast.optional_offers, 0);
    assert_eq!(
        observer_triggers_on_stack(&offering.runner, &offering.victims),
        1
    );
    offering.runner.advance_until_stack_empty();
    assert_eq!(life(&offering.runner), 18);
}

fn exact_pool(route: Route, n: usize) -> (Kept, Cast) {
    KeptBoard {
        extra: REQ_DISCARD,
        offering_cost: ManaCost::generic(4),
        mana: 2,
        lands: 0,
        ..KeptBoard::new(route)
    }
    .cast(n, true)
}

fn assert_exact_pool_cast(route: Route, n: usize) {
    let (kept, kept_cast) = exact_pool(route, n);
    assert!(!kept_cast.refused, "the once-reduced total is payable");
    assert_eq!(zone(&kept.runner, kept.spell), Zone::Stack);
    assert_eq!(pool(&kept.runner), 0, "the reduction applies exactly once");
    assert_fodder_sacrificed_once(&kept);
    assert_eq!(zone(&kept.runner, kept.filler), Zone::Graveyard);
}

#[test]
fn exact_pool_emerge_cast_prompted() {
    assert_exact_pool_cast(Route::Emerge, 2);
}

#[test]
fn exact_pool_emerge_cast_unprompted() {
    assert_exact_pool_cast(Route::Emerge, 1);
}

#[test]
fn exact_pool_instant_offering_cast_prompted() {
    assert_exact_pool_cast(Route::OfferingInstant, 2);
}

#[test]
fn exact_pool_instant_offering_cast_unprompted() {
    assert_exact_pool_cast(Route::OfferingInstant, 1);
}

/// CR 702.48c + CR 601.2f: an Offering whose sacrifice already reduced the
/// spell to {0} is made at once; the reduction it earned is still locked into
/// the total exactly once, and the Offering is not offered again.
#[test]
fn instant_offering_that_zeroes_the_cost_is_charged_once() {
    for n in [1, 2] {
        let (kept, kept_cast) = KeptBoard {
            offering_cost: ManaCost::generic(2),
            ..KeptBoard::new(Route::OfferingInstant)
        }
        .cast(n, true);
        assert_eq!(kept_cast.optional_offers, 0, "{n} targets");
        assert_eq!(zone(&kept.runner, kept.spell), Zone::Stack);
        assert_eq!(pool(&kept.runner), 4, "{n} targets");
        assert_fodder_sacrificed_once(&kept);
    }
}

/// CR 601.2f: a target-dependent cost increase is added to the total the kept
/// Emerge or Offering reduction is taken from, once: {2} + {1} - {2} for the
/// Offering that alone would zero the cost, {4} + {1} - {2} otherwise.
#[test]
fn target_dependent_increase_applies_once_beside_kept_reduction() {
    for (route, offering_cost, left) in [
        (Route::OfferingInstant, ManaCost::generic(2), 3),
        (Route::OfferingInstant, ManaCost::generic(4), 1),
        (Route::OfferingMain, ManaCost::generic(4), 1),
        (Route::Emerge, ManaCost::generic(4), 1),
    ] {
        for n in [1, 2] {
            let (kept, _) = KeptBoard {
                extra: MORE,
                offering_cost: offering_cost.clone(),
                ..KeptBoard::new(route)
            }
            .cast(n, true);
            assert_eq!(zone(&kept.runner, kept.spell), Zone::Stack);
            assert_eq!(pool(&kept.runner), left, "{offering_cost:?}, {n} targets");
            assert_fodder_sacrificed_once(&kept);
        }
    }
}

#[test]
fn sorcery_offering_applies_target_reduction_once() {
    let board = KeptBoard {
        extra: MOD,
        offering_cost: ManaCost::generic(4),
        ..KeptBoard::new(Route::OfferingMain)
    };
    for n in [1, 2] {
        let (offering, offering_cast) = board.cast(n, true);
        assert_eq!(offering_cast.optional_offers, 1);
        assert_fodder_sacrificed_once(&offering);
        // {4} - 2 (the sacrificed mana cost) - 1 (red target) = 1 of 4.
        assert_eq!(pool(&offering.runner), 3);
    }
}

#[test]
fn sorcery_offering_applies_target_reduction_once_unmet() {
    let board = KeptBoard {
        extra: MOD,
        offering_cost: ManaCost::generic(4),
        victim_color: ManaColor::Green,
        ..KeptBoard::new(Route::OfferingMain)
    };
    for n in [1, 2] {
        let (offering, _) = board.cast(n, true);
        assert_fodder_sacrificed_once(&offering);
        assert_eq!(pool(&offering.runner), 2, "no reduction for a green target");
    }
}

#[test]
fn sorcery_offering_applies_target_reduction_once_declined() {
    let board = KeptBoard {
        extra: MOD,
        offering_cost: ManaCost::generic(4),
        ..KeptBoard::new(Route::OfferingMain)
    };
    for n in [1, 2] {
        let (offering, offering_cast) = board.cast(n, false);
        assert_eq!(offering_cast.optional_offers, 1);
        assert_eq!(on_battlefield(&offering.runner, &offering.fodders), 2);
    }
}

/// CR 601.2f: an optional mana cost the pool pays beside the once-reduced
/// total is offered once and charged once. The pool-2 half (not offered when
/// the pool holds only the reduced total) pins the engine's affordability gate
/// on optional-cost offers, not a CR 601.2f requirement.
fn assert_optional_mana_offered_once(route: Route, extra: &str) {
    for n in [1, 2] {
        let affordable = KeptBoard {
            extra,
            offering_cost: ManaCost::generic(4),
            mana: 3,
            lands: 0,
            ..KeptBoard::new(route)
        };
        let (kept, kept_cast) = affordable.cast(n, true);
        assert_eq!(
            kept_cast.optional_offers, 1,
            "{extra} at pool 3, {n} targets"
        );
        assert_eq!(
            pool(&kept.runner),
            0,
            "the accepted {extra} is charged once"
        );
        assert_fodder_sacrificed_once(&kept);

        let exact = KeptBoard {
            mana: 2,
            ..affordable
        };
        let (kept, kept_cast) = exact.cast(n, true);
        assert_eq!(
            kept_cast.optional_offers, 0,
            "{extra} at pool 2, {n} targets"
        );
        assert_eq!(pool(&kept.runner), 0);
    }
}

#[test]
fn prompted_emerge_offers_optional_mana_once() {
    for extra in [
        OPTIONAL_MANA,
        "Kicker {1}",
        "Buyback {1}",
        "Multikicker {1}",
    ] {
        assert_optional_mana_offered_once(Route::Emerge, extra);
    }
}

#[test]
fn instant_offering_offers_optional_mana_once() {
    for extra in [OPTIONAL_MANA, "Kicker {1}"] {
        assert_optional_mana_offered_once(Route::OfferingInstant, extra);
    }
}

#[test]
fn offering_reduction_joins_cost_reduction_election() {
    for (phase, n) in [
        (Phase::Upkeep, 1),
        (Phase::Upkeep, 2),
        (Phase::PreCombatMain, 1),
        (Phase::PreCombatMain, 2),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(phase);
        scenario.add_artifact_from_oracle(P0, "Tide Idol", BLUE_SORCERY_LESS);
        let chosen = scenario
            .add_creature(P0, "Wisp Alpha", 2, 2)
            .with_subtypes(vec!["Spirit"])
            .with_mana_cost(cost(blue(2), 0))
            .with_color(vec![ManaColor::Blue])
            .id();
        let unchosen = scenario
            .add_creature(P0, "Wisp Beta", 1, 1)
            .with_subtypes(vec!["Spirit"])
            .with_mana_cost(ManaCost::generic(1))
            .id();
        let marks = victims(&mut scenario, n, "", ManaColor::Red);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Probe Hex", false, DESTROY_ONE)
            .with_mana_cost(cost(blue(2), 2))
            .with_color(vec![ManaColor::Blue])
            .with_keyword(Keyword::Offering("Spirit".into()))
            .id();
        scenario.with_mana_pool(P0, blue_and_colorless(1, 3));
        let mut runner = scenario.build();
        let answers = Answers {
            target: marks.first().copied(),
            pay_optional: true,
            pick: Some(chosen),
            ..Answers::default()
        };
        let offering = cast(&mut runner, spell, &answers);
        assert_eq!(
            offering.elections.len(),
            1,
            "{phase:?} {n}: the two orders lock different totals"
        );
        let provenances = &offering.elections[0];
        assert!(
            provenances.contains(&ReductionProvenance::SacrificedForCost(
                SpellCostSource::Offering
            ))
        );
        assert!(provenances
            .iter()
            .any(|provenance| matches!(provenance, ReductionProvenance::Static { .. })));
        assert_eq!(zone(&runner, spell), Zone::Stack);
        assert_eq!(zone(&runner, chosen), Zone::Graveyard);
        assert_eq!(zone(&runner, unchosen), Zone::Battlefield);
    }
}

// ---------------------------------------------------------------------------
// Targeting events.
// ---------------------------------------------------------------------------

#[test]
fn unprompted_declaration_emits_one_targeting_event_per_target() {
    for body in [DESTROY_X, MODAL_X] {
        let board = DeferredXBoard {
            body,
            victim_text: OBSERVER,
            ..DeferredXBoard::default()
        };
        let (automatic, automatic_cast) = board.cast(Completion::Automatic, true);
        let (_, prompted_cast) = board.cast(Completion::Prompted, true);
        assert_eq!(automatic_cast.becomes_target, 1);
        assert_eq!(automatic_cast.becomes_target, prompted_cast.becomes_target);
        assert_eq!(
            observer_triggers_on_stack(&automatic.runner, &automatic.victims),
            1
        );
    }
    for route in [
        Route::Emerge,
        Route::OfferingInstant,
        Route::OfferingMain,
        Route::Casualty,
    ] {
        let board = KeptBoard {
            victim_text: OBSERVER,
            ..KeptBoard::new(route)
        };
        let (automatic, automatic_cast) = board.cast(1, true);
        assert_eq!(automatic_cast.becomes_target, 1);
        assert_eq!(
            observer_triggers_on_stack(&automatic.runner, &automatic.victims),
            1
        );
    }
    // CR 601.2c: the kicked "instead" target is the spell's only target, so it
    // becomes a target once, as when prompted.
    let kicker = KeptBoard {
        victim_text: OBSERVER,
        ..KeptBoard::new(Route::Kicker)
    };
    let (_, automatic_cast) = kicker.cast(1, true);
    let (_, prompted_cast) = kicker.cast(2, true);
    assert_eq!(automatic_cast.becomes_target, 1);
    assert_eq!(automatic_cast.becomes_target, prompted_cast.becomes_target);
    for body in [DESTROY_ONE, DESTROY_RANDOM] {
        let mut host = arcane_host(body, "", false, 1, OBSERVER);
        let host_cast = cast_host(&mut host, true);
        assert_eq!(host_cast.becomes_target, 1);
        assert_eq!(observer_triggers_on_stack(&host.runner, &host.victims), 1);
    }
}

#[test]
fn refused_unprompted_cast_reads_prompted_event_reading() {
    let board = DeferredXBoard {
        extra: REQ_LAND,
        victim_text: OBSERVER,
        lands: 0,
        ..DeferredXBoard::default()
    };
    for completion in [Completion::Automatic, Completion::Prompted] {
        let (refused, refused_cast) = board.cast(completion, true);
        assert!(refused_cast.refused);
        assert_eq!(zone(&refused.runner, refused.spell), Zone::Hand);
        assert_eq!(life(&refused.runner), 20);
        assert_eq!(refused_cast.becomes_target, 0);
        assert_eq!(
            observer_triggers_on_stack(&refused.runner, &refused.victims),
            0
        );
    }
}

#[test]
fn activation_and_prompted_declarations_keep_event_counts() {
    for (targets, x, events) in [(1, 1, 1), (2, 1, 1), (1, 0, 0)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        for _ in 0..3 {
            scenario.add_basic_land(P0, ManaColor::Black);
        }
        let marks = victims(&mut scenario, targets, OBSERVER, ManaColor::Red);
        let engine_card = scenario
            .add_artifact_from_oracle(P0, "Quiet Engine", "{X}, {T}: Destroy X target creatures.")
            .id();
        scenario.with_mana_pool(P0, units(ManaType::Black, 4));
        let mut runner = scenario.build();
        let answers = Answers {
            x,
            target: marks.first().copied(),
            ..Answers::default()
        };
        let activation = drive(
            &mut runner,
            engine_card,
            GameAction::ActivateAbility {
                source_id: engine_card,
                ability_index: 0,
            },
            &answers,
        );
        assert_eq!(
            activation.becomes_target, events,
            "{targets} targets, X = {x}"
        );
    }
}

// ---------------------------------------------------------------------------
// Casts the change does not move, and costs it keeps.
// ---------------------------------------------------------------------------

#[test]
fn prompted_and_non_deferred_casts_keep_base_costs() {
    // A modal instant-speed Offering spell never takes the Offering before
    // targets: offered once, paid or declined.
    for n in [1, 2] {
        let modal = KeptBoard::new(Route::ModalOfferingInstant);
        let (paid, paid_cast) = modal.cast(n, true);
        assert_eq!(paid_cast.optional_offers, 1);
        assert_fodder_sacrificed_once(&paid);
        let (declined, declined_cast) = modal.cast(n, false);
        assert_eq!(declined_cast.optional_offers, 1);
        assert_eq!(on_battlefield(&declined.runner, &declined.fodders), 2);
    }
    // A declined sorcery-speed Offering keeps both creatures.
    for n in [1, 2] {
        let (declined, declined_cast) = KeptBoard::new(Route::OfferingMain).cast(n, false);
        assert_eq!(declined_cast.optional_offers, 1);
        assert_eq!(on_battlefield(&declined.runner, &declined.fodders), 2);
        assert_eq!(pool(&declined.runner), 1);
    }
    // Prompted Casualty, Kicker and Required-X casts with a printed Required
    // discard or an imposed tax.
    for route in [Route::Casualty, Route::Kicker, Route::RequiredX] {
        let required = KeptBoard {
            extra: REQ_DISCARD,
            ..KeptBoard::new(route)
        };
        let (kept, kept_cast) = required.cast(2, true);
        assert!(kept_cast.prompts_for_targets());
        assert_eq!(zone(&kept.runner, kept.spell), Zone::Stack);
        let imposed = KeptBoard {
            victim_text: TAX,
            ..KeptBoard::new(route)
        };
        let (kept, _) = imposed.cast(2, true);
        assert_eq!(zone(&kept.runner, kept.spell), Zone::Stack);
    }
    // An underfunded Emerge cast is refused before anything is paid.
    for n in [1, 2] {
        let underfunded = KeptBoard {
            extra: REQ_DISCARD,
            mana: 1,
            lands: 0,
            ..KeptBoard::new(Route::Emerge)
        };
        let (refused, refused_cast) = underfunded.cast(n, true);
        assert!(refused_cast.refused);
        assert_eq!(zone(&refused.runner, refused.spell), Zone::Hand);
        assert_eq!(on_battlefield(&refused.runner, &refused.fodders), 2);
        assert_eq!(zone(&refused.runner, refused.filler), Zone::Hand);
    }
}

#[test]
fn admitted_casts_keep_imposed_and_required_costs() {
    for route in [Route::Emerge, Route::OfferingInstant] {
        let imposed = KeptBoard {
            victim_text: TAX,
            ..KeptBoard::new(route)
        };
        let (kept, kept_cast) = imposed.cast(2, true);
        assert_eq!(kept_cast.optional_offers, 0);
        assert_eq!(life(&kept.runner), 17);
        assert_fodder_sacrificed_once(&kept);

        let required = KeptBoard {
            extra: REQ_DISCARD,
            ..KeptBoard::new(route)
        };
        let (kept, _) = required.cast(2, true);
        assert_eq!(zone(&kept.runner, kept.filler), Zone::Graveyard);
        assert_fodder_sacrificed_once(&kept);
    }
}

#[test]
fn admitted_casts_apply_target_reduction_once() {
    for route in [Route::Emerge, Route::OfferingInstant] {
        let board = KeptBoard {
            extra: MOD,
            offering_cost: ManaCost::generic(4),
            ..KeptBoard::new(route)
        };
        let (kept, _) = board.cast(2, true);
        assert_fodder_sacrificed_once(&kept);
        // {4} - 2 (the sacrifice) - 1 (red target) = 1 of 4.
        assert_eq!(pool(&kept.runner), 3);
    }
}

// ---------------------------------------------------------------------------
// No-target Emerge creatures.
// ---------------------------------------------------------------------------

struct NoTarget {
    runner: GameRunner,
    spell: ObjectId,
    filler: ObjectId,
    fodders: [ObjectId; 2],
}

#[allow(clippy::too_many_arguments)]
fn no_target_emerge(
    name: &str,
    text: &str,
    card_cost: ManaCost,
    emerge: ManaCost,
    fodder_values: (u32, u32),
    pick_first: bool,
    mana: usize,
    cost_increase: bool,
) -> (NoTarget, Cast) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    if cost_increase {
        scenario.add_artifact_from_oracle(P0, "Heavy Orb", "Spells cost {1} more to cast.");
    }
    let filler = scenario.add_card_to_hand(P0, "Spare Page");
    let first = scenario
        .add_creature(P0, "Husk Alpha", 2, 2)
        .with_mana_cost(ManaCost::generic(fodder_values.0))
        .id();
    let second = scenario
        .add_creature(P0, "Husk Beta", 1, 1)
        .with_mana_cost(ManaCost::generic(fodder_values.1))
        .id();
    let spell = {
        let mut builder = if text.is_empty() {
            scenario.add_creature_to_hand(P0, name, 3, 3)
        } else {
            scenario.add_creature_to_hand_from_oracle(P0, name, 3, 3, text)
        };
        builder.with_mana_cost(card_cost);
        builder.with_keyword(Keyword::Emerge(EmergeCost::creature(emerge)));
        builder.id()
    };
    scenario.with_mana_pool(P0, units(ManaType::Black, mana));
    let mut runner = scenario.build();
    let answers = Answers {
        pay_optional: true,
        pick: Some(if pick_first { first } else { second }),
        alt: Some(AlternativeCastDecision::Alternative),
        ..Answers::default()
    };
    let cast = cast(&mut runner, spell, &answers);
    (
        NoTarget {
            runner,
            spell,
            filler,
            fodders: [first, second],
        },
        cast,
    )
}

#[test]
fn no_target_emerge_creature_pays_printed_required_once() {
    // Emerge {5}, the mana value 3 creature chosen: the pool holds exactly
    // the once-reduced {2}.
    for (text, discarded) in [(REQ_DISCARD, Zone::Graveyard), ("", Zone::Hand)] {
        let (emerged, emerged_cast) = no_target_emerge(
            "Hollow Brute",
            text,
            ManaCost::generic(8),
            ManaCost::generic(5),
            (3, 1),
            true,
            2,
            false,
        );
        assert!(!emerged_cast.refused);
        assert_eq!(zone(&emerged.runner, emerged.spell), Zone::Stack);
        assert_eq!(pool(&emerged.runner), 0);
        assert_eq!(zone(&emerged.runner, emerged.fodders[0]), Zone::Graveyard);
        assert_eq!(zone(&emerged.runner, emerged.fodders[1]), Zone::Battlefield);
        assert_eq!(zone(&emerged.runner, emerged.filler), discarded);
    }
}

#[test]
fn mardu_outrider_with_synthetic_emerge_pays_printed_discard_once() {
    // Only the Emerge route is affordable (pool 2), then both are (pool 3).
    for mana in [2, 3] {
        let (outrider, outrider_cast) = no_target_emerge(
            "Mardu Outrider",
            MARDU_OUTRIDER,
            cost(black(2), 1),
            cost(black(2), 1),
            (2, 1),
            false,
            mana,
            false,
        );
        assert!(!outrider_cast.refused);
        assert_eq!(zone(&outrider.runner, outrider.spell), Zone::Stack);
        assert_eq!(zone(&outrider.runner, outrider.filler), Zone::Graveyard);
        assert_eq!(zone(&outrider.runner, outrider.fodders[1]), Zone::Graveyard);
        assert_eq!(
            zone(&outrider.runner, outrider.fodders[0]),
            Zone::Battlefield
        );
        assert_eq!(pool(&outrider.runner), mana - 2);
    }
}

#[test]
fn mardu_outrider_with_synthetic_emerge_cost_increase_is_not_recomputed_from_base() {
    // CR 601.2f: {1}{B}{B} + {1} - 3 = {B}{B}. Folding the reduction into the
    // base before the increase would total {1}{B}{B}.
    for mana in [2, 4] {
        let (outrider, outrider_cast) = no_target_emerge(
            "Mardu Outrider",
            MARDU_OUTRIDER,
            cost(black(2), 1),
            cost(black(2), 1),
            (3, 1),
            true,
            mana,
            true,
        );
        assert!(!outrider_cast.refused);
        assert_eq!(zone(&outrider.runner, outrider.spell), Zone::Stack);
        assert_eq!(pool(&outrider.runner), mana - 2);
        assert_eq!(zone(&outrider.runner, outrider.filler), Zone::Graveyard);
    }
}

fn mosasaurus_with_defiler(pay: bool) -> (GameRunner, ObjectId, [ObjectId; 2], Cast) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..3 {
        scenario.add_card_to_library_top(P0, "Deep Page");
        scenario.add_card_to_library_top(P1, "Deep Page");
    }
    scenario
        .add_creature_from_oracle(P0, "Defiler of Dreams", 5, 5, DEFILER_OF_DREAMS)
        .with_subtypes(vec!["Phyrexian", "Sphinx"])
        .with_mana_cost(cost(blue(2), 3))
        .with_color(vec![ManaColor::Blue]);
    let first = scenario
        .add_creature(P0, "Husk Alpha", 2, 2)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let second = scenario
        .add_creature(P0, "Husk Beta", 1, 1)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let spell = scenario
        .add_creature_to_hand(P0, "Cresting Mosasaurus", 9, 9)
        .from_oracle_text_with_keywords(&["Emerge"], CRESTING_MOSASAURUS)
        .with_subtypes(vec!["Dinosaur"])
        .with_mana_cost(cost(blue(2), 6))
        .with_color(vec![ManaColor::Blue])
        .id();
    scenario.with_mana_pool(P0, blue_and_colorless(1, 2));
    let mut runner = scenario.build();
    let answers = Answers {
        pay_optional: pay,
        pick: Some(first),
        alt: Some(AlternativeCastDecision::Alternative),
        ..Answers::default()
    };
    let cast = cast(&mut runner, spell, &answers);
    (runner, spell, [first, second], cast)
}

#[test]
fn cresting_mosasaurus_with_defiler_of_dreams_pay() {
    // Emerge {6}{U} - 4 = {2}{U}; Defiler's paid life takes the {U}.
    let (runner, spell, fodders, mosasaurus) = mosasaurus_with_defiler(true);
    assert!(mosasaurus.prompted("DefilerPayment"));
    assert_eq!(zone(&runner, spell), Zone::Stack);
    assert_eq!(zone(&runner, fodders[0]), Zone::Graveyard);
    assert_eq!(zone(&runner, fodders[1]), Zone::Battlefield);
    assert_eq!(life(&runner), 18);
    assert_eq!(pool(&runner), 1);
}

#[test]
fn cresting_mosasaurus_with_defiler_of_dreams_decline() {
    let (runner, spell, fodders, mosasaurus) = mosasaurus_with_defiler(false);
    assert!(mosasaurus.prompted("DefilerPayment"));
    assert_eq!(zone(&runner, spell), Zone::Stack);
    assert_eq!(zone(&runner, fodders[0]), Zone::Graveyard);
    assert_eq!(zone(&runner, fodders[1]), Zone::Battlefield);
    assert_eq!(life(&runner), 20);
    assert_eq!(pool(&runner), 0);
}

#[test]
fn sacrificed_for_cost_provenance_round_trips() {
    for (source, data) in [
        (SpellCostSource::Emerge, "Emerge"),
        (SpellCostSource::Offering, "Offering"),
    ] {
        let provenance = ReductionProvenance::SacrificedForCost(source);
        let json = serde_json::to_value(provenance).expect("serializes");
        assert_eq!(
            json,
            serde_json::json!({ "type": "SacrificedForCost", "data": data })
        );
        let back: ReductionProvenance = serde_json::from_value(json).expect("deserializes");
        assert_eq!(back, provenance);
    }
}
