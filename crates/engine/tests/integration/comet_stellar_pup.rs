//! Comet, Stellar Pup — the whole card, from its verbatim Oracle text, through the
//! real activation pipeline.
//!
//! `[0]: Roll a six-sided die.` owns four result rows written as an em-dash list
//! ("1 or 2 — …", "3 — …", "4 or 5 — …", "6 — …"). Each row attaches to its roll
//! (CR 706.3b), and the `[+N]` / `[−N]` in the rows are the effect-side loyalty
//! symbol: they put or remove loyalty counters on Comet (CR 107.7). The rulings
//! these rows pin are not CR text: the symbols put and remove counters on him, none
//! of his abilities target, a face-3 roll with no eligible card still removes one
//! counter, a face-4/5 roll at loyalty 1 deals 1 damage and then loses the counter
//! (CR 704.5i), and a face-6 roll lets him activate his normal ability two more
//! times only.
//!
//! Die faces are forced with the repo's reseed mechanism (the Farideh test's):
//! `force_next_roll` derives a seed whose first outcome draw is the wanted face,
//! installs it after the activation and before priority passes, and every forced
//! row asserts the `DieRolled` event as its positive reach-guard.
//!
//! Component Pouch and Loathsome Troll are printed members of the class whose row
//! effects name their own source: the Pouch row is the red-at-base blink row of the
//! die-frame source-identity stamp, and the Troll rows are its preservation row.

use std::ops::ControlFlow;

use engine::game::engine::EngineError;
use engine::game::planeswalker::can_activate_loyalty_ability;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones;
use engine::game::{create_debug_cards, debug_card_entry_source, DebugCardCreateRequest};
use engine::parser::oracle::parse_oracle_text;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ContinuousModification, Duration, Effect,
    ObjectScope, QuantityExpr, QuantityRef, TargetChoiceTiming, TargetFilter, TargetRef,
};
use engine::types::ability_visit::visit_ability_def;
use engine::types::actions::{DebugCardCreationKind, GameAction};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{ActionResult, GameRng, RandomDraw, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use rand::Rng;

use crate::support::shared_card_db;

/// Verbatim Oracle text (MTGJSON `text`). The row separators are U+2014 and the
/// signs of `[−1]` and `[−2]` are U+2212.
const COMET: &str = "[0]: Roll a six-sided die.\n\
1 or 2 \u{2014} [+2], then create two 1/1 green Squirrel creature tokens. They gain haste until end of turn.\n\
3 \u{2014} [\u{2212}1], then return a card with mana value 2 or less from your graveyard to your hand.\n\
4 or 5 \u{2014} Comet deals damage equal to the number of loyalty counters on him to a creature or player, then [\u{2212}2].\n\
6 \u{2014} [+1], and you may activate Comet's loyalty ability two more times this turn.";

/// Verbatim Component Pouch: a `{T}` d20 whose rows put counters on the artifact.
const COMPONENT_POUCH: &str =
    "{T}, Remove a component counter from this artifact: Add two mana of different colors.\n\
{T}: Roll a d20.\n\
1\u{2014}9 | Put a component counter on this artifact.\n\
10\u{2014}20 | Put two component counters on this artifact.";

/// Verbatim Loathsome Troll: activated from the graveyard, and every row moves
/// the card that rolled.
const LOATHSOME_TROLL: &str =
    "{3}{G}: Roll a d20. Activate only if this card is in your graveyard.\n\
1\u{2014}9 | Put this card on top of your library.\n\
10\u{2014}19 | Return this card to your hand.\n\
20 | Return this card to the battlefield tapped.";

/// A bystander walker with no grant, to spend an activation.
const BYSTANDER: &str = "[+1]: You gain 1 life.";

/// The other printed cards whose result rows are written as an em-dash list.
const GOBLIN_TUTOR: &str = "Roll a six-sided die. If you roll a 1, Goblin Tutor has no effect. Otherwise, search your library for the indicated card, reveal it, put it into your hand, then shuffle.\n\
2 \u{2014} A card named Goblin Tutor\n\
3 \u{2014} An enchantment\n\
4 \u{2014} An artifact\n\
5 \u{2014} A creature\n\
6 \u{2014} An instant or sorcery";
const JACK_IN_THE_MOX: &str = "{T}: Roll a six-sided die. This ability has the indicated effect.\n\
1 \u{2014} Sacrifice this artifact and you lose 5 life.\n\
2 \u{2014} Add {W}.\n\
3 \u{2014} Add {U}.\n\
4 \u{2014} Add {B}.\n\
5 \u{2014} Add {R}.\n\
6 \u{2014} Add {G}.";
const STRATEGY_SCHMATEGY: &str =
    "Roll a six-sided die. Strategy, Schmategy has the indicated effect.\n\
1 \u{2014} Do nothing.\n\
2 \u{2014} Destroy all artifacts.\n\
3 \u{2014} Destroy all lands.\n\
4 \u{2014} Strategy, Schmategy deals 3 damage to each creature and each player.\n\
5 \u{2014} Each player discards their hand and draws seven cards.\n\
6 \u{2014} Repeat this process two more times.";
const URZAS_SCIENCE_FAIR_PROJECT: &str =
    "{2}: Roll a six-sided die. This creature gets the indicated result.\n\
1 \u{2014} It gets -2/-2 until end of turn.\n\
2 \u{2014} Prevent all combat damage it would deal this turn.\n\
3 \u{2014} It gains vigilance until end of turn.\n\
4 \u{2014} It gains first strike until end of turn.\n\
5 \u{2014} It gains flying until end of turn.\n\
6 \u{2014} It gets +2/+2 until end of turn.";

/// A planeswalker whose `[0]` rolls a die without a results table: the lines that
/// follow it are other abilities, not rows.
const MR_MONOPOLY: &str = "[0]: Roll a six-sided die. Put a number of loyalty counters on Mr. Monopoly equal to the result.\n\
[\u{2212}2]: Heist! \u{2014} Exile the top two cards of target opponent's library. Until end of turn, you may play those cards, and mana of any type can be spent to cast them.\n\
[\u{2212}4]: Shut Down! \u{2014} Destroy target artifact.\n\
[\u{2212}40]: Pass Go \u{2014} Create 200 Treasure tokens.";

fn scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

fn generic_cost(generic: u32) -> ManaCost {
    ManaCost::Cost {
        shards: vec![],
        generic,
    }
}

fn floating(mana: &[ManaType]) -> Vec<ManaUnit> {
    mana.iter()
        .map(|t| ManaUnit::new(*t, ObjectId(0), false, vec![]))
        .collect()
}

// ---------------------------------------------------------------------------
// Die-face forcing
// ---------------------------------------------------------------------------

/// A seed whose first outcome draw on a `sides`-sided die is `face`: the same
/// generator and draw `roll_natural` uses, so the face is derived, not assumed.
fn seed_for_face(sides: u8, face: u8) -> u64 {
    (0u64..10_000)
        .find(|&seed| {
            GameRng::seed_from_u64(seed)
                .draw(RandomDraw::Outcome)
                .random_range(1..=sides)
                == face
        })
        .expect("a seed exists for every face")
}

/// Make the next die roll land on `face`. Called after the activation has put the
/// ability on the stack and before priority passes.
fn force_next_roll(runner: &mut GameRunner, sides: u8, face: u8) {
    let seed = seed_for_face(sides, face);
    let state = runner.state_mut();
    state.rng_seed = seed;
    state.rng_word_pos = 0;
    state.rng = GameRng::seed_from_u64(seed);
}

/// The face the `sides`-sided die rolled, from the emitted event: the positive
/// reach-guard of every forced row.
fn die_face(events: &[GameEvent], sides: u8) -> Option<u8> {
    events.iter().find_map(|event| match event {
        GameEvent::DieRolled {
            sides: rolled,
            result: Some(result),
            ..
        } if *rolled == sides => Some(*result),
        _ => None,
    })
}

// ---------------------------------------------------------------------------
// State readers and drivers
// ---------------------------------------------------------------------------

/// The index of the first parsed ability of `id` satisfying `pred`, read from the
/// parsed abilities and never hard-coded.
fn ability_index(
    runner: &GameRunner,
    id: ObjectId,
    pred: impl Fn(&AbilityDefinition) -> bool,
) -> usize {
    runner.state().objects[&id]
        .abilities
        .iter()
        .position(pred)
        .expect("the fixture carries the expected ability")
}

fn is_loyalty(ability: &AbilityDefinition) -> bool {
    matches!(ability.cost, Some(AbilityCost::Loyalty { .. }))
}

fn is_roll(ability: &AbilityDefinition) -> bool {
    matches!(&*ability.effect, Effect::RollDie { .. })
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .life
}

fn hand(runner: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .iter()
        .copied()
        .collect()
}

/// The loyalty counters on `id` (CR 306.5c: a planeswalker's loyalty is the number
/// of loyalty counters on it).
fn loyalty(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Loyalty)
        .copied()
        .unwrap_or(0)
}

fn extra(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id].extra_loyalty_activations_this_turn
}

fn count(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id].loyalty_activations_this_turn
}

fn marked_damage(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id].damage_marked
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn gate_admits(runner: &GameRunner, id: ObjectId, index: usize) -> bool {
    let controller = runner.state().objects[&id].controller;
    can_activate_loyalty_ability(runner.state(), id, controller, index)
}

/// A refused activation is an `Err` from the real action path, not just a false gate.
fn activation_is_refused(runner: &mut GameRunner, id: ObjectId, index: usize) -> bool {
    runner
        .act(GameAction::ActivateAbility {
            source_id: id,
            ability_index: index,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .is_err()
}

fn activate(runner: &mut GameRunner, source: ObjectId, index: usize) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .unwrap_or_else(|error| panic!("the activation must be accepted: {error:?}"));
}

fn is_priority(runner: &GameRunner) -> bool {
    matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
}

/// Pass priority until the stack is empty or the engine asks for an answer,
/// collecting every event.
fn settle(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..20 {
        if runner.state().stack.is_empty() || !is_priority(runner) {
            break;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("priority passes while the stack resolves")
                .events,
        );
    }
    events
}

/// A readable head of a wait state: a full `Debug` dump of a pending cast is
/// thousands of characters.
fn short(waiting_for: &WaitingFor) -> String {
    format!("{waiting_for:?}").chars().take(90).collect()
}

/// The recipient prompt the engine is waiting on: its chooser and its eligible set.
fn recipient_prompt(runner: &GameRunner) -> (PlayerId, Vec<TargetRef>) {
    match &runner.state().waiting_for {
        WaitingFor::ChooseObjectsSelection {
            player,
            eligible,
            min: 1,
            max: Some(1),
            ..
        } => (*player, eligible.clone()),
        other => panic!("expected the recipient prompt, got {}", short(other)),
    }
}

fn pick_recipient(runner: &mut GameRunner, target: TargetRef) -> Vec<GameEvent> {
    runner
        .act(GameAction::SelectTargets {
            targets: vec![target],
        })
        .unwrap_or_else(|error| panic!("the pick must be accepted: {error:?}"))
        .events
}

/// Answer the single-pick recipient prompt the way the board does: one
/// `ChooseTarget` click (`None` = choose nothing).
fn click_recipient(
    runner: &mut GameRunner,
    target: Option<TargetRef>,
) -> Result<ActionResult, EngineError> {
    runner.act(GameAction::ChooseTarget { target })
}

/// The graveyard prompt the engine is waiting on: its eligible cards, sorted.
fn graveyard_prompt(runner: &GameRunner) -> Vec<ObjectId> {
    match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice {
            player,
            cards,
            zone: Zone::Graveyard,
            destination: Some(Zone::Hand),
            ..
        } => {
            assert_eq!(*player, P0, "the controller chooses");
            let mut cards = cards.clone();
            cards.sort();
            cards
        }
        other => panic!("expected the graveyard-return prompt, got {}", short(other)),
    }
}

fn pick_cards(runner: &mut GameRunner, cards: Vec<ObjectId>) {
    runner
        .act(GameAction::SelectCards { cards })
        .unwrap_or_else(|error| panic!("the pick must be accepted: {error:?}"));
}

/// The Squirrel tokens P0 controls on the battlefield.
fn squirrels(runner: &GameRunner) -> Vec<ObjectId> {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            let object = &state.objects[id];
            object.is_token && object.name == "Squirrel" && object.controller == P0
        })
        .collect()
}

/// Re-seed the loyalty a freshly re-entered planeswalker would otherwise lack
/// (its counters were on the previous object). Without it CR 704.5i removes the
/// zero-loyalty new object, which would make a "no change" reading vacuous: the
/// permanent would be gone for a reason that has nothing to do with the row.
fn reseed_loyalty(runner: &mut GameRunner, id: ObjectId, loyalty: u32) {
    let object = runner.state_mut().objects.get_mut(&id).unwrap();
    object.loyalty = Some(loyalty);
    object.counters.insert(CounterType::Loyalty, loyalty);
}

/// Blink `id`: exile it and return it, so the same storage id is a new object.
fn blink(runner: &mut GameRunner, id: ObjectId) {
    let mut events = Vec::new();
    zones::move_to_zone(runner.state_mut(), id, Zone::Exile, &mut events);
    zones::move_to_zone(runner.state_mut(), id, Zone::Battlefield, &mut events);
    reseed_loyalty(runner, id, 5);
}

/// The activated-ability stack entries sourced by `id`, as their captured stamps.
fn activation_stamps(runner: &GameRunner, id: ObjectId) -> Vec<Option<u64>> {
    runner
        .state()
        .stack
        .iter()
        .filter_map(|entry| match &entry.kind {
            StackEntryKind::ActivatedAbility { source_id, ability } if *source_id == id => {
                Some(ability.source_incarnation)
            }
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The board
// ---------------------------------------------------------------------------

struct Table {
    runner: GameRunner,
    comet: ObjectId,
}

/// Comet at loyalty 5 for P0, plus whatever `setup` stages around it.
fn table(setup: impl FnOnce(&mut GameScenario)) -> Table {
    let mut scenario = scenario();
    let comet = scenario
        .add_planeswalker_from_oracle(P0, "Comet, Stellar Pup", "Comet", 5, COMET)
        .id();
    setup(&mut scenario);
    Table {
        runner: scenario.build(),
        comet,
    }
}

/// Activate Comet's `[0]` ability and force its die to `face`, leaving the ability
/// on the stack.
fn activate_and_force(runner: &mut GameRunner, comet: ObjectId, face: u8) {
    let index = ability_index(runner, comet, is_loyalty);
    activate(runner, comet, index);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach-guard: the ability is on the stack"
    );
    force_next_roll(runner, 6, face);
}

/// Activate Comet, force the roll to `face` and let the stack settle, stopping at
/// the first prompt. The `DieRolled` reach-guard is part of the helper.
fn roll(runner: &mut GameRunner, comet: ObjectId, face: u8) -> Vec<GameEvent> {
    activate_and_force(runner, comet, face);
    let events = settle(runner);
    assert_eq!(
        die_face(&events, 6),
        Some(face),
        "reach-guard: the die rolled the forced face"
    );
    events
}

// ---------------------------------------------------------------------------
// Parse level: the table, the symbol, and the rows
// ---------------------------------------------------------------------------

fn parse_comet() -> engine::parser::oracle::ParsedAbilities {
    parse_oracle_text(
        COMET,
        "Comet, Stellar Pup",
        &[],
        &["Planeswalker".to_string()],
        &["Comet".to_string()],
    )
}

/// The count of `Effect::Unimplemented` nodes reachable from `def`, through
/// sub-abilities, branches and die-result rows alike.
fn unimplemented_count(def: &AbilityDefinition) -> usize {
    let mut gaps = 0;
    let _ = visit_ability_def(def, &mut |effect| {
        if matches!(effect, Effect::Unimplemented { .. }) {
            gaps += 1;
        }
        ControlFlow::Continue(())
    });
    gaps
}

/// The `(min, max)` ranges of every die-result row reachable from `def`, one list
/// per roll.
fn roll_tables(def: &AbilityDefinition) -> Vec<Vec<(u8, u8)>> {
    let mut tables = Vec::new();
    let _ = visit_ability_def(def, &mut |effect| {
        if let Effect::RollDie { results, .. } = effect {
            tables.push(results.iter().map(|row| (row.min, row.max)).collect());
        }
        ControlFlow::Continue(())
    });
    tables
}

/// The effects along `def`'s sub-ability spine.
fn spine(def: &AbilityDefinition) -> Vec<&Effect> {
    let mut effects = Vec::new();
    let mut next = Some(def);
    while let Some(current) = next {
        effects.push(&*current.effect);
        next = current.sub_ability.as_deref();
    }
    effects
}

/// Verbatim Comet: one loyalty ability, whose roll carries the four rows
/// with exactly the printed ranges, and nothing is left over as an orphan ability,
/// static or trigger. Row 6 is `(6, 6)`, never `6+`: a modified result of 7 (Scooch)
/// matches no row (CR 706.3a lists a single number, `N1–N2` and `N+`, and "if any"
/// leaves an unmatched result with no effect).
///
/// Red at base: the table never attaches, so the roll carries no rows and the four
/// rows are orphan abilities.
#[test]
fn comet_carries_its_four_rows_on_its_roll_and_nothing_is_left_over() {
    let parsed = parse_comet();
    assert_eq!(parsed.abilities.len(), 1, "{:#?}", parsed.abilities);
    assert!(parsed.triggers.is_empty());
    assert!(parsed.statics.is_empty());
    assert!(parsed.replacements.is_empty());
    assert!(
        parsed.parse_warnings.is_empty(),
        "{:?}",
        parsed.parse_warnings
    );

    let ability = &parsed.abilities[0];
    assert_eq!(ability.cost, Some(AbilityCost::Loyalty { amount: 0 }));
    let Effect::RollDie {
        count,
        sides,
        results,
        modifier,
    } = &*ability.effect
    else {
        panic!("expected the roll, got {ability:#?}");
    };
    assert_eq!(count, &QuantityExpr::Fixed { value: 1 });
    assert_eq!(*sides, 6);
    assert!(modifier.is_none());
    assert_eq!(
        results
            .iter()
            .map(|row| (row.min, row.max))
            .collect::<Vec<_>>(),
        [(1, 2), (3, 3), (4, 5), (6, 6)]
    );
    assert_eq!(unimplemented_count(ability), 0, "{ability:#?}");
}

/// Rows 1 and 2: the symbol is its own instruction, then the token row with
/// its haste rider (`LastCreated`, until end of turn).
#[test]
fn row_body_chain_faces_one_and_two_put_loyalty_then_make_hasty_tokens() {
    let def = parse_effect_chain(
        "[+2], then create two 1/1 green Squirrel creature tokens. They gain haste until end of turn.",
        AbilityKind::Spell,
    );
    assert_eq!(unimplemented_count(&def), 0, "{def:#?}");
    let effects = spine(&def);
    assert_eq!(effects.len(), 3, "{def:#?}");
    assert!(
        matches!(
            effects[0],
            Effect::PutCounter {
                counter_type: CounterType::Loyalty,
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::SelfRef,
            }
        ),
        "{def:#?}"
    );
    assert!(
        matches!(
            effects[1],
            Effect::Token {
                count: QuantityExpr::Fixed { value: 2 },
                ..
            }
        ),
        "{def:#?}"
    );
    let Effect::GenericEffect {
        static_abilities,
        duration,
        target,
        ..
    } = effects[2]
    else {
        panic!("expected the haste rider, got {def:#?}");
    };
    assert_eq!(target, &Some(TargetFilter::LastCreated));
    assert_eq!(duration, &Some(Duration::UntilEndOfTurn));
    assert!(static_abilities.iter().any(|s| s.modifications.iter().any(
        |m| matches!(m, ContinuousModification::AddKeyword { keyword } if *keyword == Keyword::Haste)
    )));
}

/// Row 3: the removal is `Fixed(1)` — `Fixed(-1)` would mean "remove all" —
/// and the return that follows is the existing graveyard-return shape.
#[test]
fn row_body_chain_face_three_removes_one_loyalty_then_returns_a_cheap_card() {
    let def = parse_effect_chain(
        "[\u{2212}1], then return a card with mana value 2 or less from your graveyard to your hand.",
        AbilityKind::Spell,
    );
    assert_eq!(unimplemented_count(&def), 0, "{def:#?}");
    let effects = spine(&def);
    assert_eq!(effects.len(), 2, "{def:#?}");
    assert!(
        matches!(
            effects[0],
            Effect::RemoveCounter {
                counter_type: Some(CounterType::Loyalty),
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            }
        ),
        "{def:#?}"
    );
    assert!(
        matches!(
            effects[1],
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Hand,
                ..
            }
        ),
        "{def:#?}"
    );
}

/// Rows 4 and 5: the damage reads Comet's loyalty when it is applied and the
/// `, then [−2]` that follows is its own instruction, so the amount is read before
/// the counters are removed (CR 608.2c + CR 608.2h).
#[test]
fn row_body_chain_faces_four_and_five_deal_damage_then_remove_two_loyalty() {
    let def = parse_effect_chain(
        "~ deals damage equal to the number of loyalty counters on him to a creature or player, then [\u{2212}2].",
        AbilityKind::Spell,
    );
    assert_eq!(unimplemented_count(&def), 0, "{def:#?}");
    let Effect::DealDamage { amount, .. } = &*def.effect else {
        panic!("expected the damage, got {def:#?}");
    };
    assert_eq!(
        amount,
        &QuantityExpr::Ref {
            qty: QuantityRef::CountersOn {
                scope: ObjectScope::Source,
                counter_type: Some(CounterType::Loyalty),
            }
        }
    );
    assert_eq!(def.target_choice_timing, TargetChoiceTiming::Resolution);
    let removal = def
        .sub_ability
        .as_deref()
        .expect("the removal is its own clause");
    assert!(
        matches!(
            &*removal.effect,
            Effect::RemoveCounter {
                counter_type: Some(CounterType::Loyalty),
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::SelfRef,
            }
        ),
        "{def:#?}"
    );
}

/// Row 6: the symbol, then the extra-activation grant, which is mandatory (the "may"
/// is the later activation permission, CR 608.2d) and scoped to the source.
#[test]
fn row_body_chain_face_six_puts_loyalty_then_grants_two_activations() {
    let def = parse_effect_chain(
        "[+1], and you may activate ~'s loyalty ability two more times this turn.",
        AbilityKind::Spell,
    );
    assert_eq!(unimplemented_count(&def), 0, "{def:#?}");
    let effects = spine(&def);
    assert_eq!(effects.len(), 2, "{def:#?}");
    assert!(
        matches!(
            effects[0],
            Effect::PutCounter {
                counter_type: CounterType::Loyalty,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            }
        ),
        "{def:#?}"
    );
    assert!(
        matches!(
            effects[1],
            Effect::GrantExtraLoyaltyActivations {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::SelfRef,
            }
        ),
        "{def:#?}"
    );
    assert!(!def.sub_ability.as_deref().unwrap().optional, "{def:#?}");
}

/// Hostile attach: the collector stops at the first line that is not a row, and the
/// next printed ability is intact.
#[test]
fn the_collector_stops_at_a_line_that_is_not_a_row() {
    let parsed = parse_oracle_text(
        "[+1]: Roll a six-sided die.\n1 \u{2014} Draw a card.\n[\u{2212}2]: Draw two cards.",
        "Probe Walker",
        &[],
        &["Planeswalker".to_string()],
        &["Probe".to_string()],
    );
    assert_eq!(parsed.abilities.len(), 2, "{:#?}", parsed.abilities);
    assert_eq!(roll_tables(&parsed.abilities[0]), [vec![(1, 1)]]);
    assert_eq!(
        parsed.abilities[1].cost,
        Some(AbilityCost::Loyalty { amount: -2 })
    );
    assert!(
        matches!(
            &*parsed.abilities[1].effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                ..
            }
        ),
        "{:#?}",
        parsed.abilities[1]
    );
}

/// Hostile attach: with no roll in the header there is no table to own the row, so
/// the row line is not consumed by the loyalty ability.
#[test]
fn a_row_after_a_loyalty_ability_without_a_roll_is_not_consumed() {
    let parsed = parse_oracle_text(
        "[+1]: Draw a card.\n1 \u{2014} Do nothing.",
        "Probe Walker",
        &[],
        &["Planeswalker".to_string()],
        &["Probe".to_string()],
    );
    let first = &parsed.abilities[0];
    assert!(
        matches!(&*first.effect, Effect::Draw { .. }),
        "reach-guard: the loyalty ability parsed as its own effect: {first:#?}"
    );
    assert!(
        parsed.abilities.iter().all(|a| roll_tables(a).is_empty()),
        "no roll exists to own a table: {:#?}",
        parsed.abilities
    );
    assert!(
        parsed.abilities.len()
            + parsed.statics.len()
            + parsed.triggers.len()
            + parsed.replacements.len()
            > 1,
        "the row line is still a printed line of its own: {parsed:#?}"
    );
}

/// Hostile attach, verbatim: Mr. Monopoly, On the Go rolls without a table, and the
/// `[−2]: Heist! — …` line that follows is another ability, not a row.
#[test]
fn a_roll_without_a_table_leaves_the_next_loyalty_ability_alone() {
    let parsed = parse_oracle_text(
        MR_MONOPOLY,
        "Mr. Monopoly, On the Go",
        &[],
        &["Planeswalker".to_string()],
        &["Monopoly".to_string()],
    );
    let amounts: Vec<_> = parsed
        .abilities
        .iter()
        .map(|a| match a.cost {
            Some(AbilityCost::Loyalty { amount }) => Some(amount),
            _ => None,
        })
        .collect();
    assert_eq!(amounts, [Some(0), Some(-2), Some(-4), Some(-40)]);
    assert_eq!(
        roll_tables(&parsed.abilities[0]),
        [Vec::<(u8, u8)>::new()],
        "reach-guard: the roll is there and carries no rows"
    );
}

/// Class census: the other printed cards written with em-dash rows attach
/// their rows to the roll, and each stays visibly unsupported through the header
/// connective it still cannot parse ("has the indicated effect", "gets the
/// indicated result", "search … for the indicated card"). These guards assert
/// today's red; when the connectives are implemented they flip and should be
/// replaced by runtime rows.
#[test]
fn the_other_em_dash_tables_attach_and_stay_visibly_unsupported() {
    /// One printed card of the class: its verbatim text, its types and subtypes,
    /// and how many rows its table carries.
    struct ClassCard {
        name: &'static str,
        oracle: &'static str,
        types: &'static [&'static str],
        subtypes: &'static [&'static str],
        rows: usize,
    }
    let cases = [
        ClassCard {
            name: "Goblin Tutor",
            oracle: GOBLIN_TUTOR,
            types: &["Instant"],
            subtypes: &[],
            rows: 5,
        },
        ClassCard {
            name: "Jack-in-the-Mox",
            oracle: JACK_IN_THE_MOX,
            types: &["Artifact"],
            subtypes: &[],
            rows: 6,
        },
        ClassCard {
            name: "Strategy, Schmategy",
            oracle: STRATEGY_SCHMATEGY,
            types: &["Sorcery"],
            subtypes: &[],
            rows: 6,
        },
        ClassCard {
            name: "Urza's Science Fair Project",
            oracle: URZAS_SCIENCE_FAIR_PROJECT,
            types: &["Artifact", "Creature"],
            subtypes: &["Construct"],
            rows: 6,
        },
    ];
    for ClassCard {
        name,
        oracle,
        types,
        subtypes,
        rows,
    } in cases
    {
        let types: Vec<String> = types.iter().map(|t| t.to_string()).collect();
        let subtypes: Vec<String> = subtypes.iter().map(|t| t.to_string()).collect();
        let parsed = parse_oracle_text(oracle, name, &[], &types, &subtypes);

        let tables: Vec<Vec<(u8, u8)>> = parsed.abilities.iter().flat_map(roll_tables).collect();
        assert_eq!(tables.len(), 1, "{name}: one roll: {:#?}", parsed.abilities);
        assert_eq!(tables[0].len(), rows, "{name}: the table attached");
        assert!(
            parsed
                .abilities
                .iter()
                .map(unimplemented_count)
                .sum::<usize>()
                > 0,
            "{name}: the header connective stays visible as Unimplemented"
        );
    }
}

// ---------------------------------------------------------------------------
// Runtime: every face
// ---------------------------------------------------------------------------

/// Faces 1 and 2: `[+2]` puts two loyalty counters on Comet, then two 1/1 green
/// Squirrel tokens enter with haste until end of turn. A creature that was already
/// there gains no haste, and a face-3 roll makes no tokens.
#[test]
fn faces_one_and_two_add_two_loyalty_and_make_two_hasty_squirrels() {
    for face in [1u8, 2] {
        let Table { mut runner, comet } = table(|scenario| {
            scenario.add_creature(P0, "Old Bear", 2, 2);
        });
        let old_bear = runner
            .state()
            .battlefield
            .iter()
            .copied()
            .find(|id| runner.state().objects[id].name == "Old Bear")
            .expect("the bear is on the battlefield");
        assert!(squirrels(&runner).is_empty());

        let events = roll(&mut runner, comet, face);
        assert!(is_priority(&runner), "no prompt: {events:?}");

        assert_eq!(loyalty(&runner, comet), 7, "face {face}");
        let tokens = squirrels(&runner);
        assert_eq!(tokens.len(), 2, "face {face}");
        for token in tokens {
            let object = &runner.state().objects[&token];
            assert_eq!((object.power, object.toughness), (Some(1), Some(1)));
            assert_eq!(object.color, vec![ManaColor::Green]);
            assert!(object.has_keyword(&Keyword::Haste), "{}", object.name);
        }
        assert!(
            !runner.state().objects[&old_bear].has_keyword(&Keyword::Haste),
            "\"They gain haste\" names the tokens this row created"
        );
    }

    let Table { mut runner, comet } = table(|_| {});
    roll(&mut runner, comet, 3);
    assert!(
        squirrels(&runner).is_empty(),
        "paired negative: a face-3 roll makes no tokens"
    );
}

/// The face-3 graveyard: an MV1 and an MV2 creature card and a land (MV0) are
/// eligible; an MV3 card and the opponent's MV1 card are not.
struct Graveyard {
    one: ObjectId,
    two: ObjectId,
    land: ObjectId,
    three: ObjectId,
    opposing: ObjectId,
}

fn stock_graveyards(scenario: &mut GameScenario) -> Graveyard {
    Graveyard {
        one: scenario
            .add_creature_to_graveyard(P0, "Cheap Bear", 1, 1)
            .with_mana_cost(generic_cost(1))
            .id(),
        two: scenario
            .add_creature_to_graveyard(P0, "Two Drop", 2, 2)
            .with_mana_cost(generic_cost(2))
            .id(),
        land: scenario.add_land_to_graveyard(P0, "Forest").id(),
        three: scenario
            .add_creature_to_graveyard(P0, "Big Bear", 3, 3)
            .with_mana_cost(generic_cost(3))
            .id(),
        opposing: scenario
            .add_creature_to_graveyard(P1, "Opposing Imp", 1, 1)
            .with_mana_cost(generic_cost(1))
            .id(),
    }
}

/// Face 3 with several eligible cards: `[−1]`, then the controller chooses among
/// exactly the cards of their own graveyard with mana value 2 or less (a land is
/// mana value 0), and the chosen card ends in hand.
#[test]
fn face_three_removes_one_loyalty_and_returns_a_chosen_cheap_card() {
    let mut graveyard = None;
    let Table { mut runner, comet } =
        table(|scenario| graveyard = Some(stock_graveyards(scenario)));
    let g = graveyard.unwrap();

    activate_and_force(&mut runner, comet, 3);
    let events = settle(&mut runner);
    assert_eq!(die_face(&events, 6), Some(3), "reach-guard: the die rolled");
    assert_eq!(loyalty(&runner, comet), 4, "the symbol removes one counter");

    let mut expected = vec![g.one, g.two, g.land];
    expected.sort();
    assert_eq!(graveyard_prompt(&runner), expected);

    pick_cards(&mut runner, vec![g.two]);
    assert!(is_priority(&runner));
    assert_eq!(hand(&runner, P0), vec![g.two]);
    for stays in [g.one, g.land, g.three, g.opposing] {
        assert_eq!(zone_of(&runner, stays), Zone::Graveyard);
    }
}

/// Face 3 with no eligible card still removes the counter (the card's ruling) and
/// raises no prompt; with exactly one the card is simply returned. In both the
/// MV3 card and the opponent's MV1 card stay where they are.
#[test]
fn face_three_with_no_or_one_eligible_card_asks_nothing() {
    // None eligible: only cards the row cannot return.
    let Table { mut runner, comet } = table(|scenario| {
        scenario
            .add_creature_to_graveyard(P0, "Big Bear", 3, 3)
            .with_mana_cost(generic_cost(3));
        scenario
            .add_creature_to_graveyard(P1, "Opposing Imp", 1, 1)
            .with_mana_cost(generic_cost(1));
    });
    roll(&mut runner, comet, 3);
    assert!(is_priority(&runner), "no prompt");
    assert_eq!(loyalty(&runner, comet), 4, "the counter is still removed");
    assert!(hand(&runner, P0).is_empty());

    // Exactly one eligible.
    let mut ids = None;
    let Table { mut runner, comet } = table(|scenario| {
        ids = Some((
            scenario
                .add_creature_to_graveyard(P0, "Cheap Bear", 1, 1)
                .with_mana_cost(generic_cost(1))
                .id(),
            scenario
                .add_creature_to_graveyard(P0, "Big Bear", 3, 3)
                .with_mana_cost(generic_cost(3))
                .id(),
            scenario
                .add_creature_to_graveyard(P1, "Opposing Imp", 1, 1)
                .with_mana_cost(generic_cost(1))
                .id(),
        ));
    });
    let (cheap, big, opposing) = ids.unwrap();
    activate_and_force(&mut runner, comet, 3);
    let events = settle(&mut runner);
    assert_eq!(die_face(&events, 6), Some(3), "reach-guard: the die rolled");
    assert_eq!(loyalty(&runner, comet), 4);
    if !is_priority(&runner) {
        // A one-option prompt would still hold the same outcome.
        assert_eq!(graveyard_prompt(&runner), vec![cheap]);
        pick_cards(&mut runner, vec![cheap]);
    }
    assert_eq!(hand(&runner, P0), vec![cheap]);
    assert_eq!(zone_of(&runner, big), Zone::Graveyard);
    assert_eq!(zone_of(&runner, opposing), Zone::Graveyard);
}

/// The board the face-4/5 rows are read on: Comet at loyalty 5, the controller's
/// creature, an artifact, the opponent's creature, a hexproof creature and an
/// opposing planeswalker.
struct Arena {
    runner: GameRunner,
    comet: ObjectId,
    own_creature: ObjectId,
    artifact: ObjectId,
    opposing_creature: ObjectId,
    hexproof_creature: ObjectId,
    opposing_walker: ObjectId,
}

fn arena() -> Arena {
    let mut ids = None;
    let Table { runner, comet } = table(|scenario| {
        ids = Some((
            scenario.add_creature(P0, "Own Bear", 2, 9).id(),
            scenario.add_artifact_from_oracle(P0, "Own Idol", "").id(),
            scenario.add_creature(P1, "Opposing Ogre", 3, 9).id(),
            scenario
                .add_creature(P1, "Hexproof Giant", 3, 9)
                .hexproof()
                .id(),
            scenario
                .add_planeswalker_from_oracle(P1, "Opposing Walker", "Probe", 4, BYSTANDER)
                .id(),
        ));
    });
    let (own_creature, artifact, opposing_creature, hexproof_creature, opposing_walker) =
        ids.unwrap();
    Arena {
        runner,
        comet,
        own_creature,
        artifact,
        opposing_creature,
        hexproof_creature,
        opposing_walker,
    }
}

/// Faces 4 and 5: nothing is announced (the ability has no target, CR 115.10a), the
/// controller chooses the recipient while the instruction is applied, the damage is
/// Comet's loyalty BEFORE the `[−2]`, and then the counters are removed. The
/// prompt offers the creatures and both players; a hexproof creature is offered and
/// is hit, because this is a choice and not a target.
#[test]
fn faces_four_and_five_deal_damage_equal_to_loyalty_then_remove_two() {
    for face in [4u8, 5] {
        let mut a = arena();
        activate_and_force(&mut a.runner, a.comet, face);
        let StackEntryKind::ActivatedAbility { ability, .. } = &a.runner.state().stack[0].kind
        else {
            panic!("expected the activated ability on the stack");
        };
        assert!(
            ability.targets.is_empty(),
            "nothing is announced for a described recipient"
        );
        assert!(is_priority(&a.runner), "no target prompt at activation");

        let events = settle(&mut a.runner);
        assert_eq!(
            die_face(&events, 6),
            Some(face),
            "reach-guard: the die rolled"
        );
        let (chooser, mut eligible) = recipient_prompt(&a.runner);
        assert_eq!(chooser, P0);
        eligible.sort_by_key(|target| format!("{target:?}"));
        let mut expected = vec![
            TargetRef::Object(a.own_creature),
            TargetRef::Object(a.opposing_creature),
            TargetRef::Object(a.hexproof_creature),
            TargetRef::Player(P0),
            TargetRef::Player(P1),
        ];
        expected.sort_by_key(|target| format!("{target:?}"));
        assert_eq!(eligible, expected, "creatures and both players");
        for excluded in [a.comet, a.artifact, a.opposing_walker] {
            assert!(!eligible.contains(&TargetRef::Object(excluded)));
        }

        pick_recipient(&mut a.runner, TargetRef::Player(P1));
        assert_eq!(
            life(&a.runner, P1),
            15,
            "damage equal to the loyalty before the removal"
        );
        assert_eq!(loyalty(&a.runner, a.comet), 3, "then [−2]");
        assert!(is_priority(&a.runner));
    }

    // Hexproof is not a restriction on a choice: the giant takes the 5.
    let mut a = arena();
    roll(&mut a.runner, a.comet, 4);
    let (_, eligible) = recipient_prompt(&a.runner);
    assert!(eligible.contains(&TargetRef::Object(a.hexproof_creature)));
    pick_recipient(&mut a.runner, TargetRef::Object(a.hexproof_creature));
    assert_eq!(marked_damage(&a.runner, a.hexproof_creature), 5);
    assert_eq!(
        life(&a.runner, P1),
        20,
        "paired negative: nobody else was hit"
    );
    assert_eq!(loyalty(&a.runner, a.comet), 3);
}

/// Faces 4 and 5 answered by one board click: the single-pick recipient prompt
/// (`min 1`, `max Some(1)`) takes a `ChooseTarget` as the same answer as a
/// one-item `SelectTargets` (CR 608.2d), so the damage is the loyalty before the
/// `[−2]` and the ability finishes. The controller may also pick himself.
#[test]
fn faces_four_and_five_take_a_single_board_click_for_the_recipient() {
    for face in [4u8, 5] {
        let mut a = arena();
        roll(&mut a.runner, a.comet, face);
        let (chooser, eligible) = recipient_prompt(&a.runner);
        assert_eq!(chooser, P0, "reach-guard: the controller chooses");
        assert!(eligible.contains(&TargetRef::Player(P1)));

        click_recipient(&mut a.runner, Some(TargetRef::Player(P1)))
            .unwrap_or_else(|error| panic!("the click must be accepted: {error:?}"));
        assert_eq!(
            life(&a.runner, P1),
            15,
            "damage equal to the loyalty before the removal"
        );
        assert_eq!(loyalty(&a.runner, a.comet), 3, "then [−2]");
        assert!(is_priority(&a.runner));
        assert!(a.runner.state().stack.is_empty(), "the ability finished");
    }

    // Sibling: the controller clicks his own seat.
    let mut a = arena();
    roll(&mut a.runner, a.comet, 4);
    let (_, eligible) = recipient_prompt(&a.runner);
    assert!(eligible.contains(&TargetRef::Player(P0)));
    click_recipient(&mut a.runner, Some(TargetRef::Player(P0)))
        .unwrap_or_else(|error| panic!("the click must be accepted: {error:?}"));
    assert_eq!(life(&a.runner, P0), 15);
    assert_eq!(life(&a.runner, P1), 20);
    assert_eq!(loyalty(&a.runner, a.comet), 3);
    assert!(is_priority(&a.runner));
    assert!(a.runner.state().stack.is_empty());
}

/// CR 115.10a: a board click on the recipient prompt is a choice, not a target, so
/// a hexproof creature is choosable and nothing becomes the target of anything.
#[test]
fn a_board_click_hits_a_hexproof_creature_without_targeting_it() {
    let mut a = arena();
    roll(&mut a.runner, a.comet, 4);
    let (_, eligible) = recipient_prompt(&a.runner);
    assert!(
        eligible.contains(&TargetRef::Object(a.hexproof_creature)),
        "reach-guard: the hexproof creature is offered"
    );

    let events = click_recipient(&mut a.runner, Some(TargetRef::Object(a.hexproof_creature)))
        .unwrap_or_else(|error| panic!("the click must be accepted: {error:?}"))
        .events;
    assert_eq!(
        marked_damage(&a.runner, a.hexproof_creature),
        5,
        "the clicked hexproof creature takes the damage"
    );
    assert_eq!(life(&a.runner, P1), 20, "nobody else was hit");
    assert_eq!(loyalty(&a.runner, a.comet), 3);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, GameEvent::BecomesTarget { .. })),
        "the click announces no target"
    );
    assert!(is_priority(&a.runner));
    assert!(a.runner.state().stack.is_empty());
}

/// Face 4 at loyalty 1 (the card's ruling): Comet deals 1 damage, then the `[−2]`
/// removes the one counter it can (CR 609.3), and with no loyalty left the state-based
/// action puts him into the graveyard (CR 704.5i).
#[test]
fn face_four_at_loyalty_one_deals_one_damage_then_dies() {
    let mut a = arena();
    reseed_loyalty(&mut a.runner, a.comet, 1);

    roll(&mut a.runner, a.comet, 4);
    pick_recipient(&mut a.runner, TargetRef::Player(P1));

    assert_eq!(life(&a.runner, P1), 19, "reach-guard: one damage landed");
    assert_eq!(loyalty(&a.runner, a.comet), 0);
    assert_eq!(zone_of(&a.runner, a.comet), Zone::Graveyard, "CR 704.5i");
}

/// Face 6: `[+1]`, and Comet's own ability may be activated two more times this
/// turn — no prompt, the grant is mandatory and scoped to him. Two more activations
/// succeed (each forced to face 3 with an empty graveyard, so no further grant) and
/// the fourth is refused; a second own planeswalker gets no extra activation, and an
/// opponent's is untouched.
#[test]
fn face_six_adds_loyalty_and_grants_two_more_activations_to_comet_only() {
    let mut walker = None;
    let Table { mut runner, comet } = table(|scenario| {
        walker = Some(
            scenario
                .add_planeswalker_from_oracle(P0, "Bystander Walker", "Probe", 5, BYSTANDER)
                .id(),
        );
    });
    let walker = walker.unwrap();

    roll(&mut runner, comet, 6);
    assert!(is_priority(&runner), "no prompt");
    assert_eq!(loyalty(&runner, comet), 6);
    assert_eq!(extra(&runner, comet), 2);
    assert_eq!(count(&runner, comet), 1);
    assert!(runner
        .state()
        .extra_loyalty_activations_this_turn
        .is_empty());

    let index = ability_index(&runner, comet, is_loyalty);
    for expected in [2, 3] {
        assert!(
            gate_admits(&runner, comet, index),
            "count {}",
            count(&runner, comet)
        );
        roll(&mut runner, comet, 3);
        assert_eq!(count(&runner, comet), expected);
    }
    assert!(!gate_admits(&runner, comet, index));
    assert!(
        activation_is_refused(&mut runner, comet, index),
        "the fourth"
    );

    // A second own walker: its first activation succeeds, its second is refused.
    let spend = ability_index(&runner, walker, is_loyalty);
    activate(&mut runner, walker, spend);
    settle(&mut runner);
    assert_eq!(count(&runner, walker), 1);
    assert_eq!(extra(&runner, walker), 0);
    assert!(activation_is_refused(&mut runner, walker, spend));

    // An opponent's walker is unaffected: flip the turn directly, so the live grant
    // is not swept, and the walker is activatable at count 0 and capped at 1.
    let mut opposing = None;
    let Table {
        runner: mut other, ..
    } = table(|scenario| {
        opposing = Some(
            scenario
                .add_planeswalker_from_oracle(P1, "Opposing Walker", "Probe", 5, BYSTANDER)
                .id(),
        );
    });
    let opposing = opposing.unwrap();
    let other_comet = other
        .state()
        .objects
        .values()
        .find(|o| o.name == "Comet, Stellar Pup")
        .map(|o| o.id)
        .unwrap();
    roll(&mut other, other_comet, 6);
    {
        let state = other.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    assert_eq!(
        extra(&other, other_comet),
        2,
        "no-sweep reach-guard: the grant is live"
    );
    let their = ability_index(&other, opposing, is_loyalty);
    assert!(
        gate_admits(&other, opposing, their),
        "paired control: count 0"
    );
    other
        .state_mut()
        .objects
        .get_mut(&opposing)
        .unwrap()
        .loyalty_activations_this_turn = 1;
    assert!(
        !gate_admits(&other, opposing, their),
        "the opponent's cap is 1"
    );
}

/// CR 601.2a + CR 608.3a: cast from hand, Comet resolves onto the battlefield with
/// his loyalty, no recipient prompt (the row is no longer an orphan ability merged
/// into the spell) and exactly one ability.
#[test]
fn casting_comet_puts_him_on_the_battlefield_with_one_ability_and_no_prompt() {
    let mut scenario = scenario();
    // The type is set before the Oracle text is parsed (the parse needs it), and the
    // loyalty is re-seeded afterwards, because a face built from Oracle text carries
    // none.
    let spell = scenario
        .add_spell_to_hand(P0, "Comet, Stellar Pup", false)
        .as_planeswalker_with_loyalty("Comet", 5)
        .from_oracle_text(COMET)
        .as_planeswalker_with_loyalty("Comet", 5)
        .with_mana_cost(generic_cost(0))
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "no recipient prompt at spell resolution: {}",
        short(outcome.final_waiting_for())
    );
    outcome.assert_zone(&[spell], Zone::Battlefield);
    assert_eq!(runner.state().objects[&spell].abilities.len(), 1);
    assert_eq!(loyalty(&runner, spell), 5);
}

// ---------------------------------------------------------------------------
// Sandbox entry: the debug Create Card tool
// ---------------------------------------------------------------------------

/// Comet for P0, put onto the battlefield by the sandbox Create Card tool from the
/// committed fixture's verbatim face, beside an opposing creature.
fn debug_table(run_etb: bool, creation_kind: DebugCardCreationKind) -> Table {
    let mut scenario = scenario();
    scenario.add_creature(P1, "Opposing Ogre", 3, 9);
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    let db = shared_card_db().expect("the committed integration fixture loads");
    let face = db
        .get_face_by_name("Comet, Stellar Pup")
        .expect("Comet is in the committed fixture");
    create_debug_cards(
        runner.state_mut(),
        DebugCardCreateRequest {
            actor: P0,
            source: debug_card_entry_source(db, face),
            owner: P0,
            zone: Zone::Battlefield,
            count: 1,
            attach_to: None,
            run_etb,
            nonlegendary: false,
            creation_kind,
        },
    )
    .unwrap_or_else(|error| panic!("the debug creation is accepted: {error:?}"));
    let comet = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].name == "Comet, Stellar Pup")
        .expect("Comet is on the battlefield");
    assert!(is_priority(&runner), "reach-guard: the creation settled");
    Table { runner, comet }
}

/// CR 306.5b + CR 306.5c: Comet put onto the battlefield by the sandbox Create Card
/// tool enters with his five loyalty counters, so his rows read and change them as
/// for a cast Comet: face 4 deals 5 to the clicked recipient, then `[−2]` leaves 3;
/// face 1's `[+2]` leaves 7, with two Squirrels. Rows: the real entry (`run_etb`),
/// the raw placement, and the token kind.
///
/// Red at base: the created Comet had a loyalty field of 5 but no loyalty counter,
/// so face 4 dealt 0 damage and left him at 5, and face 1 left him at 2.
#[test]
fn debug_created_comet_rolls_read_and_change_its_loyalty_counters() {
    for (run_etb, kind) in [
        (true, DebugCardCreationKind::Card),
        (false, DebugCardCreationKind::Card),
        (true, DebugCardCreationKind::Token),
    ] {
        let row = format!("run_etb={run_etb}, {kind:?}");

        // Face 4: one board click on P1 answers the recipient prompt.
        let Table { mut runner, comet } = debug_table(run_etb, kind);
        roll(&mut runner, comet, 4);
        let (chooser, eligible) = recipient_prompt(&runner);
        assert_eq!(chooser, P0, "{row}: the controller chooses");
        assert!(eligible.contains(&TargetRef::Player(P1)), "{row}");
        click_recipient(&mut runner, Some(TargetRef::Player(P1)))
            .unwrap_or_else(|error| panic!("{row}: the click must be accepted: {error:?}"));
        assert_eq!(
            life(&runner, P1),
            15,
            "{row}: damage equal to his five loyalty counters"
        );
        assert_eq!(loyalty(&runner, comet), 3, "{row}: then [−2]");
        assert_eq!(
            runner.state().objects[&comet].loyalty,
            Some(3),
            "{row}: CR 306.5c"
        );
        assert!(is_priority(&runner), "{row}");
        assert!(
            runner.state().stack.is_empty(),
            "{row}: the ability finished"
        );

        // Face 1: [+2], then two Squirrels.
        let Table { mut runner, comet } = debug_table(run_etb, kind);
        roll(&mut runner, comet, 1);
        assert!(is_priority(&runner), "{row}: no prompt");
        assert_eq!(loyalty(&runner, comet), 7, "{row}: [+2]");
        assert_eq!(squirrels(&runner).len(), 2, "{row}");
    }
}

// ---------------------------------------------------------------------------
// The die-frame source-identity stamp
// ---------------------------------------------------------------------------

/// CR 400.7 + CR 113.7a + CR 706.3b: a Component Pouch blinked while its roll is on
/// the stack is a new object, and its row ("Put a component counter on this
/// artifact") does not reach the new object. A second Pouch activated in the same
/// stack and left alone still receives its counters.
///
/// Red at base: the unstamped die-result frame binds whatever object now has the
/// source's id, so the new Pouch received one or two counters.
#[test]
fn a_blinked_component_pouch_gets_none_of_its_old_activations_counters() {
    let mut scenario = scenario();
    let pouch = scenario
        .add_artifact_from_oracle(P0, "Component Pouch", COMPONENT_POUCH)
        .id();
    let other = scenario
        .add_artifact_from_oracle(P0, "Component Pouch", COMPONENT_POUCH)
        .id();
    let mut runner = scenario.build();
    let component = CounterType::Generic("component".to_string());
    let counters = |runner: &GameRunner, id: ObjectId| -> u32 {
        runner.state().objects[&id]
            .counters
            .get(&component)
            .copied()
            .unwrap_or(0)
    };

    // `abilities[0]` is the mana ability; the roll is read by its effect.
    let roll_pouch = ability_index(&runner, pouch, is_roll);
    let roll_other = ability_index(&runner, other, is_roll);
    activate(&mut runner, pouch, roll_pouch);
    activate(&mut runner, other, roll_other);

    let inc0 = runner.state().objects[&pouch].incarnation;
    assert_eq!(
        activation_stamps(&runner, pouch),
        [Some(inc0)],
        "reach-guard: one activation of the Pouch, stamped with its incarnation"
    );
    blink(&mut runner, pouch);
    let object = &runner.state().objects[&pouch];
    assert_ne!(object.incarnation, inc0, "reach-guard: a new object");
    assert_eq!(object.zone, Zone::Battlefield);
    assert_eq!(counters(&runner, pouch), 0);

    let events = settle(&mut runner);
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, GameEvent::DieRolled { sides: 20, .. }))
            .count(),
        2,
        "reach-guard: both Pouches rolled"
    );
    assert_eq!(counters(&runner, pouch), 0, "the new object gets nothing");
    assert!(
        counters(&runner, other) >= 1,
        "paired positive: the untouched Pouch received its counters"
    );
}

/// Comet blinked in response to his own activation: the new object is not the
/// object that rolled, so the `[+2]` of faces 1 and 2, the `[−1]` of face 3 and the
/// `[+1]` and grant of face 6 do not reach it. Each of those faces has an unblinked
/// control. Faces 4 and 5 are not rolled here: their `[−2]` is the one row that
/// resumes from the parked recipient prompt, and no blink row exercises it. The
/// loyalty is re-seeded after the blink (a new object has none), so an unchanged 5
/// is the row not applying and not a removal by the zero-loyalty state-based action.
///
/// Red until the die-result frame carries the activation's stamp.
#[test]
fn a_blinked_comet_is_not_changed_by_the_rows_of_its_old_activation() {
    // (face, loyalty with no blink, extra activations with no blink)
    for (face, control_loyalty, control_extra) in [(1u8, 7u32, 0u32), (3, 4, 0), (6, 6, 2)] {
        let Table {
            runner: mut control,
            comet,
        } = table(|_| {});
        roll(&mut control, comet, face);
        assert_eq!(
            loyalty(&control, comet),
            control_loyalty,
            "control, face {face}"
        );
        assert_eq!(
            extra(&control, comet),
            control_extra,
            "control, face {face}"
        );

        let Table { mut runner, comet } = table(|_| {});
        activate_and_force(&mut runner, comet, face);
        let inc0 = runner.state().objects[&comet].incarnation;
        assert_eq!(activation_stamps(&runner, comet), [Some(inc0)]);
        blink(&mut runner, comet);
        let object = &runner.state().objects[&comet];
        assert_ne!(object.incarnation, inc0, "reach-guard: a new object");
        assert_eq!(object.zone, Zone::Battlefield);

        let events = settle(&mut runner);
        assert_eq!(
            die_face(&events, 6),
            Some(face),
            "reach-guard: the die rolled"
        );
        assert_eq!(
            loyalty(&runner, comet),
            5,
            "face {face}: the new object is unchanged"
        );
        assert_eq!(extra(&runner, comet), 0, "face {face}: nothing was granted");
        if face == 1 {
            assert_eq!(
                squirrels(&runner).len(),
                2,
                "the tokens do not depend on the source"
            );
        }
    }
}

/// PRESERVATION: Loathsome Troll's rows move the card that rolled, from the
/// graveyard, so they must keep binding it. Green at base and after the stamp:
/// the row selected by each forced face moves the Troll to its printed zone.
#[test]
fn loathsome_troll_rows_still_move_the_card_that_rolled() {
    for (face, expected) in [
        (5u8, Zone::Library),
        (15, Zone::Hand),
        (20, Zone::Battlefield),
    ] {
        let mut scenario = scenario();
        let troll = scenario
            .add_creature_to_graveyard(P0, "Loathsome Troll", 6, 2)
            .from_oracle_text(LOATHSOME_TROLL)
            .id();
        scenario.with_mana_pool(
            P0,
            floating(&[
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Green,
            ]),
        );
        let mut runner = scenario.build();
        assert_eq!(zone_of(&runner, troll), Zone::Graveyard, "reach-guard");

        let index = ability_index(&runner, troll, is_roll);
        activate(&mut runner, troll, index);
        for _ in 0..4 {
            if !matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
                break;
            }
            runner
                .act(GameAction::PassPriority)
                .expect("the payment finalizes from the pool");
        }
        assert_eq!(runner.state().stack.len(), 1, "reach-guard: on the stack");
        assert_eq!(
            runner.state().players[0].mana_pool.total(),
            0,
            "the cost was paid"
        );
        let inc0 = runner.state().objects[&troll].incarnation;
        assert_eq!(activation_stamps(&runner, troll), [Some(inc0)]);

        force_next_roll(&mut runner, 20, face);
        let events = settle(&mut runner);
        assert_eq!(
            die_face(&events, 20),
            Some(face),
            "reach-guard: the d20 rolled"
        );

        assert_eq!(zone_of(&runner, troll), expected, "face {face}");
        let state = runner.state();
        let on_top = state.players[0].library.front() == Some(&troll);
        assert_eq!(
            on_top,
            expected == Zone::Library,
            "face {face}: top of library"
        );
        assert_eq!(
            hand(&runner, P0).contains(&troll),
            expected == Zone::Hand,
            "face {face}: in hand"
        );
        assert_eq!(
            runner.state().objects[&troll].tapped,
            expected == Zone::Battlefield,
            "face {face}: returned tapped"
        );
    }
}
