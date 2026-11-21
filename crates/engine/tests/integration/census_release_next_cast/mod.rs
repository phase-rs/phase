//! Runtime witnesses for the "When you next cast ..." delayed-trigger class.
//!
//! Every card is a printed card loaded with `add_real_card` from the integration card fixture
//! (printed cost, type line, P/T and parsed abilities); only the textless "Filler" library padding is
//! synthetic. Every witness plays through public `GameAction`s (three refresh derived power and
//! granted abilities with `evaluate_layers` after their direct setup). A scripted driver (`drive`) answers
//! each prompt the engine raises from an explicit list of `Ask`s: the prompt's variant and its actor
//! (and the options named in each arm) are asserted before the answer is submitted, an unlisted
//! prompt fails the test, and a listed prompt that never appears fails the test.
//! (`GameRunner::cast(..).resolve()` drains `OrderTriggers` in identity order and has no
//! `CopyRetarget` policy, so it cannot script or assert those prompts; these witnesses use their own
//! driver.) Setup values (lore, oil, charge and time counters, life, pooled mana, the card-name
//! pool, library order) are set directly. Some are read back (the lore, oil and time counters,
//! life totals, Codie's library order and mana pool); the charge counters and the card-name pool
//! are not. Each witness ends with an empty stack and the active player holding `Priority`
//! (`assert_idle`).

use engine::database::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{ChoiceType, Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::Supertype;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, GameState, ManaChoice, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const BOLT: &str = "Lightning Bolt";

fn db() -> &'static CardDatabase {
    shared_card_db().expect("integration card fixture must load")
}

// ---------------------------------------------------------------------------------------------
// Scripted prompt driver
// ---------------------------------------------------------------------------------------------

/// One prompt the engine is expected to raise, with the public answer submitted for it.
#[derive(Debug)]
enum Ask {
    /// Announcing a spell's or ability's target (CR 601.2c): `pick` must be among the legal targets.
    Target { actor: PlayerId, pick: TargetRef },
    /// Choosing the value of X (CR 107.3a).
    X { actor: PlayerId, value: u32 },
    /// "You may choose new targets for the copy": keep every target (CR 707.10c).
    KeepCopyTargets { actor: PlayerId },
    /// "You may choose new targets for the copy": move the target to `to` (CR 707.10c).
    Retarget { actor: PlayerId, to: TargetRef },
    /// An optional instruction ("you may remove two oil counters"): accept or decline.
    Optional {
        actor: PlayerId,
        source: ObjectId,
        accept: bool,
    },
    /// "Choose a card name" (CR 201.4).
    Name { actor: PlayerId, pick: &'static str },
    /// A library search: exactly `offered` is legal, `pick` is taken (CR 701.23).
    Search {
        actor: PlayerId,
        offered: Vec<ObjectId>,
        pick: ObjectId,
    },
    /// Two simultaneous triggers: order them last-to-first (CR 603.3b).
    ReverseOrder { actor: PlayerId, count: usize },
    /// Choosing the colour for "one mana of any color".
    Color { actor: PlayerId, color: ManaType },
    /// Declare no attackers (CR 508.1).
    NoAttack { actor: PlayerId },
}

impl Ask {
    // The `..` in the `WaitingFor` patterns skips prompt fields the witnesses do not assert; the
    // fields each `Ask` does check (actor and options) are named.
    fn answer(self, waiting: &WaitingFor) -> GameAction {
        match (self, waiting) {
            (
                Ask::Target { actor, pick },
                WaitingFor::TargetSelection {
                    player,
                    target_slots,
                    ..
                },
            ) => {
                assert_eq!(*player, actor, "target announcement by the wrong player");
                assert_eq!(target_slots.len(), 1, "one target slot");
                assert!(
                    target_slots[0].legal_targets.contains(&pick),
                    "{pick:?} must be a legal target, options {:?}",
                    target_slots[0].legal_targets
                );
                GameAction::SelectTargets {
                    targets: vec![pick],
                }
            }
            (
                Ask::X { actor, value },
                WaitingFor::ChooseXValue {
                    player, min, max, ..
                },
            ) => {
                assert_eq!(*player, actor);
                assert!(
                    (*min..=*max).contains(&value),
                    "X={value} outside {min}..={max}"
                );
                GameAction::ChooseX { value }
            }
            (
                Ask::KeepCopyTargets { actor },
                WaitingFor::CopyRetarget {
                    player,
                    target_slots,
                    ..
                },
            ) => {
                assert_eq!(*player, actor, "the copy's controller chooses its targets");
                assert!(
                    !target_slots.is_empty() && target_slots.iter().all(|slot| slot.can_keep),
                    "every copy target can be kept: {target_slots:?}"
                );
                GameAction::KeepAllCopyTargets
            }
            (
                Ask::Retarget { actor, to },
                WaitingFor::CopyRetarget {
                    player,
                    target_slots,
                    ..
                },
            ) => {
                assert_eq!(*player, actor);
                assert_eq!(target_slots.len(), 1, "one copy target slot");
                assert!(
                    target_slots[0].legal_alternatives.contains(&to),
                    "{to:?} must be a legal new target, options {:?}",
                    target_slots[0].legal_alternatives
                );
                GameAction::ChooseTarget { target: Some(to) }
            }
            (
                Ask::Optional {
                    actor,
                    source,
                    accept,
                },
                WaitingFor::OptionalEffectChoice {
                    player, source_id, ..
                },
            ) => {
                assert_eq!(*player, actor);
                assert_eq!(
                    *source_id, source,
                    "the optional instruction belongs to {source:?}"
                );
                GameAction::DecideOptionalEffect { accept }
            }
            (
                Ask::Name { actor, pick },
                WaitingFor::NamedChoice {
                    player,
                    choice_type,
                    ..
                },
            ) => {
                assert_eq!(*player, actor);
                assert!(
                    matches!(choice_type, ChoiceType::CardName { .. }),
                    "a card-name choice: {choice_type:?}"
                );
                GameAction::ChooseOption {
                    choice: pick.to_string(),
                }
            }
            (
                Ask::Search {
                    actor,
                    offered,
                    pick,
                },
                WaitingFor::SearchChoice {
                    player,
                    cards,
                    count,
                    ..
                },
            ) => {
                assert_eq!(*player, actor);
                assert_eq!(*count, 1, "search for one card");
                let (mut got, mut want) = (cards.clone(), offered);
                got.sort();
                want.sort();
                assert_eq!(got, want, "exactly the legal cards are offered");
                GameAction::SelectCards { cards: vec![pick] }
            }
            (
                Ask::ReverseOrder { actor, count },
                WaitingFor::OrderTriggers { player, triggers },
            ) => {
                assert_eq!(*player, actor);
                assert_eq!(triggers.len(), count, "{triggers:?}");
                GameAction::OrderTriggers {
                    order: (0..count).rev().collect(),
                }
            }
            (Ask::Color { actor, color }, WaitingFor::ChooseManaColor { player, .. }) => {
                assert_eq!(*player, actor);
                GameAction::ChooseManaColor {
                    choice: ManaChoice::SingleColor(color),
                    count: 1,
                }
            }
            (
                Ask::NoAttack { actor },
                WaitingFor::DeclareAttackers {
                    player,
                    valid_attacker_ids,
                    ..
                },
            ) => {
                assert_eq!(*player, actor);
                assert!(
                    !valid_attacker_ids.is_empty(),
                    "declining an attack that was possible"
                );
                GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                }
            }
            (ask, other) => panic!("scripted {ask:?} but the engine asked {other:?}"),
        }
    }
}

/// Passes priority and answers scripted prompts until `done` holds. Every `Ask` must be consumed.
fn drive(
    r: &mut GameRunner,
    done: impl Fn(&GameState) -> bool,
    script: Vec<Ask>,
) -> Vec<GameEvent> {
    let mut script = script.into_iter();
    let mut events = Vec::new();
    for _ in 0..200 {
        if done(r.state()) {
            let unasked: Vec<_> = script.collect();
            assert!(unasked.is_empty(), "prompts never raised: {unasked:?}");
            return events;
        }
        let waiting = r.state().waiting_for.clone();
        let action = match waiting {
            WaitingFor::Priority { player: _ } => GameAction::PassPriority,
            other => script
                .next()
                .unwrap_or_else(|| panic!("unscripted prompt {other:?}"))
                .answer(&other),
        };
        events.extend(r.act(action).expect("public action accepted").events);
    }
    panic!(
        "drive: never reached the goal; waiting for {:?}",
        r.state().waiting_for
    );
}

/// Resolve everything on the stack, answering `script`'s prompts, until the stack is empty again.
fn play(r: &mut GameRunner, script: Vec<Ask>) -> Vec<GameEvent> {
    drive(
        r,
        |s| s.stack.is_empty() && matches!(s.waiting_for, WaitingFor::Priority { player: _ }),
        script,
    )
}

/// The witness boundary: nothing on the stack and the active player (P0) holds priority.
fn assert_idle(r: &GameRunner) {
    assert!(r.state().stack.is_empty(), "stack: {:?}", r.state().stack);
    assert!(
        matches!(r.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "waiting for {:?}",
        r.state().waiting_for
    );
}

fn cast_action(r: &mut GameRunner, spell: ObjectId) -> Vec<GameEvent> {
    let card_id = r.state().objects[&spell].card_id;
    r.act(GameAction::CastSpell {
        object_id: spell,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("cast announced")
    .events
}

/// Cast `spell` and resolve the stack through `script`.
fn cast(r: &mut GameRunner, spell: ObjectId, script: Vec<Ask>) -> Vec<GameEvent> {
    let mut events = cast_action(r, spell);
    events.extend(play(r, script));
    events
}

fn activate(
    r: &mut GameRunner,
    source: ObjectId,
    index: usize,
    script: Vec<Ask>,
) -> Vec<GameEvent> {
    let mut events = r
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("ability activated")
        .events;
    events.extend(play(r, script));
    events
}

// ---------------------------------------------------------------------------------------------
// Scenario helpers
// ---------------------------------------------------------------------------------------------

fn main_phase() -> GameScenario {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s
}

fn card(s: &mut GameScenario, name: &str, zone: Zone) -> ObjectId {
    s.add_real_card(P0, name, zone, db())
}

fn mana(kind: ManaType, n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(kind, ObjectId(0), false, vec![]))
        .collect()
}

fn fund(r: &mut GameRunner, player: PlayerId, kind: ManaType, n: usize) {
    for unit in mana(kind, n) {
        r.state_mut().players[player.0 as usize].mana_pool.add(unit);
    }
}

/// Textless library padding so the turn passes in the expiry witnesses can draw.
fn pad_libraries(s: &mut GameScenario) {
    s.with_library_top(P0, &["Filler"; 12]);
    s.with_library_top(P1, &["Filler"; 12]);
}

fn delayed(r: &GameRunner) -> usize {
    r.state().delayed_triggers.len()
}

fn counters(r: &GameRunner, id: ObjectId, kind: CounterType) -> u32 {
    r.state().objects[&id]
        .counters
        .get(&kind)
        .copied()
        .unwrap_or(0)
}

fn zone(r: &GameRunner, id: ObjectId) -> Zone {
    r.state().objects[&id].zone
}

/// The (copy, original) stack ids of every `SpellCopied` event; each copy is controlled by P0.
fn copies(events: &[GameEvent]) -> Vec<(ObjectId, ObjectId)> {
    events
        .iter()
        .filter_map(|event| {
            let GameEvent::SpellCopied {
                card_id: _,
                controller,
                object_id,
                original_id,
            } = event
            else {
                return None;
            };
            assert_eq!(
                *controller, P0,
                "the trigger's controller controls the copy"
            );
            Some((*object_id, *original_id))
        })
        .collect()
}

/// Target P1 with a Bolt-like announcement.
fn at_p1() -> Ask {
    Ask::Target {
        actor: P0,
        pick: TargetRef::Player(P1),
    }
}

fn keep() -> Ask {
    Ask::KeepCopyTargets { actor: P0 }
}

/// Cast a Lightning Bolt at P1 and keep the targets of `kept` copies; returns the events.
fn bolt(r: &mut GameRunner, spell: ObjectId, kept: usize) -> Vec<GameEvent> {
    bolt_with(r, spell, vec![], kept)
}

/// `bolt`, with `between` prompts answered after the target and before the copies' targets (for a
/// cast that puts two triggers on the stack at once, the ordering prompt, CR 603.3b).
fn bolt_with(
    r: &mut GameRunner,
    spell: ObjectId,
    between: Vec<Ask>,
    kept: usize,
) -> Vec<GameEvent> {
    let mut script = vec![at_p1()];
    script.extend(between);
    script.extend((0..kept).map(|_| keep()));
    let events = cast(r, spell, script);
    assert_eq!(copies(&events).len(), kept, "copies of {spell:?}");
    events
}

/// The ordering prompt for two simultaneous triggers: last to first.
fn reorder() -> Vec<Ask> {
    vec![Ask::ReverseOrder {
        actor: P0,
        count: 2,
    }]
}

fn bolts(s: &mut GameScenario, n: usize) -> Vec<ObjectId> {
    (0..n).map(|_| card(s, BOLT, Zone::Hand)).collect()
}

/// Put `ids` (top first) on top of P0's library in that order.
fn seat_library_top(r: &mut GameRunner, ids: &[ObjectId]) {
    let library = &mut r.state_mut().players[0].library;
    library.retain(|id| !ids.contains(id));
    for id in ids.iter().rev() {
        library.push_front(*id);
    }
}

/// Set the Saga's lore counters directly: entry through `add_real_card` is not a lore-counter event.
fn set_lore(r: &mut GameRunner, saga: ObjectId, lore: u32) {
    let counters = &mut r
        .state_mut()
        .objects
        .get_mut(&saga)
        .expect("Saga on the battlefield")
        .counters;
    if lore == 0 {
        counters.remove(&CounterType::Lore);
    } else {
        counters.insert(CounterType::Lore, lore);
    }
}

/// A printed Saga at P0's draw step; two passes reach the main phase, where the turn-based action
/// adds the next lore counter (CR 714.3c) and the chapter ability triggers (CR 714.2b).
fn saga_board(name: &str) -> (GameScenario, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::Draw);
    let saga = card(&mut s, name, Zone::Battlefield);
    (s, saga)
}

/// Reach P0's main phase with the Saga at `chapter - 1` lore counters and resolve chapter `chapter`.
fn run_chapter(r: &mut GameRunner, saga: ObjectId, chapter: u32, script: Vec<Ask>) {
    set_lore(r, saga, chapter - 1);
    drive(r, |s| s.phase == Phase::PreCombatMain, vec![]);
    assert_eq!(
        counters(r, saga, CounterType::Lore),
        chapter,
        "the turn-based action added lore counter {chapter}"
    );
    play(r, script);
}

/// Pass until P0's next turn reaches its precombat main phase (P1's turn comes first), answering
/// `script`'s prompts on the way.
fn pass_to_own_turn(r: &mut GameRunner, script: Vec<Ask>) {
    let start = r.state().turn_number;
    drive(
        r,
        |s| s.turn_number > start && s.phase == Phase::PreCombatMain && s.active_player == P0,
        script,
    );
}

/// Pigment Wrangler // Striking Palette: the creature enters prepared (CR 722.3a); its prepared copy
/// is cast with `CastPreparedCopy` (CR 722.3c), and the resolving Palette creates the delayed copy.
fn cast_prepared_palette(r: &mut GameRunner, wrangler: ObjectId) {
    assert!(
        r.state().objects[&wrangler].prepared.is_some(),
        "Pigment Wrangler entered prepared"
    );
    r.act(GameAction::CastPreparedCopy { source: wrangler })
        .expect("prepared Striking Palette cast");
    play(r, vec![]);
    assert_idle(r);
}

// ---------------------------------------------------------------------------------------------
// "copy that spell": a delayed copy of the next instant or sorcery
// ---------------------------------------------------------------------------------------------

/// Striking Palette {R} Sorcery (the prepared half of Pigment Wrangler). CR 603.7 + CR 707.10: one
/// delayed trigger copies the next instant or sorcery once, and the copy resolves before the original.
#[test]
fn striking_palette_copies_the_next_instant_or_sorcery_once() {
    let mut s = main_phase();
    let wrangler = card(&mut s, "Pigment Wrangler", Zone::Battlefield);
    let spells = bolts(&mut s, 2);
    s.with_mana_pool(P0, mana(ManaType::Red, 3));
    let mut r = s.build();
    cast_prepared_palette(&mut r, wrangler);
    assert_eq!(delayed(&r), 1);
    let life = r.life(P1);
    let events = bolt(&mut r, spells[0], 1);
    let (copy, original) = copies(&events)[0];
    let resolved = |id: ObjectId| {
        events
            .iter()
            .position(|e| matches!(e, GameEvent::StackResolved { object_id } if *object_id == id))
            .expect("resolved")
    };
    assert!(
        resolved(copy) < resolved(original),
        "the copy resolves first"
    );
    assert_eq!(r.life(P1), life - 6, "Bolt and its copy");
    assert_eq!(delayed(&r), 0, "one-shot");
    assert_idle(&r);
    bolt(&mut r, spells[1], 0);
    assert_eq!(r.life(P1), life - 9, "the next spell is not copied");
    assert_idle(&r);
}

/// CR 514.2: "this turn" — an unfired delayed copy is gone at the next turn, so the Bolt cast then
/// is not copied.
#[test]
fn striking_palette_delayed_copy_expires_with_the_turn() {
    let mut s = main_phase();
    let wrangler = card(&mut s, "Pigment Wrangler", Zone::Battlefield);
    let spell = card(&mut s, BOLT, Zone::Hand);
    pad_libraries(&mut s);
    s.with_mana_pool(P0, mana(ManaType::Red, 1));
    let mut r = s.build();
    cast_prepared_palette(&mut r, wrangler);
    assert_eq!(delayed(&r), 1);
    pass_to_own_turn(&mut r, vec![Ask::NoAttack { actor: P0 }]);
    assert_eq!(r.state().active_player, P0);
    assert_eq!(delayed(&r), 0, "the delayed trigger expired");
    play(&mut r, vec![]);
    fund(&mut r, P0, ManaType::Red, 1);
    let life = r.life(P1);
    bolt(&mut r, spell, 0);
    assert_eq!(r.life(P1), life - 3);
    assert_idle(&r);
}

/// Ruling: the copy is made whether or not the cast spell has targets (Inferno has none).
#[test]
fn striking_palette_copies_a_spell_without_targets() {
    let mut s = main_phase();
    let wrangler = card(&mut s, "Pigment Wrangler", Zone::Battlefield);
    let inferno = card(&mut s, "Inferno", Zone::Hand);
    s.with_mana_pool(P0, mana(ManaType::Red, 8));
    let mut r = s.build();
    r.state_mut().players[0].life = 40;
    cast_prepared_palette(&mut r, wrangler);
    let life = r.life(P1);
    let events = cast(&mut r, inferno, vec![]);
    assert_eq!(copies(&events).len(), 1);
    assert_eq!(r.life(P1), life - 12, "Inferno and its copy each deal 6");
    assert_eq!(r.life(P0), 40 - 12);
    assert_idle(&r);
}

/// Ruling: the copy is created even if the original is countered before the trigger resolves. P1
/// counters the Bolt with the delayed trigger still on the stack.
#[test]
fn striking_palette_copy_survives_the_original_being_countered() {
    let mut s = main_phase();
    let wrangler = card(&mut s, "Pigment Wrangler", Zone::Battlefield);
    let spell = card(&mut s, BOLT, Zone::Hand);
    let counterspell = s.add_real_card(P1, "Counterspell", Zone::Hand, db());
    s.with_mana_pool(P0, mana(ManaType::Red, 2));
    let mut r = s.build();
    fund(&mut r, P1, ManaType::Blue, 2);
    cast_prepared_palette(&mut r, wrangler);
    let life = r.life(P1);
    cast_action(&mut r, spell);
    drive(&mut r, |s| s.stack.len() == 2, vec![at_p1()]);
    assert_eq!(
        delayed(&r),
        0,
        "the delayed trigger fired and is on the stack above the Bolt"
    );
    r.act(GameAction::PassPriority).expect("P0 passes");
    assert!(matches!(r.state().waiting_for, WaitingFor::Priority { player } if player == P1));
    cast_action(&mut r, counterspell);
    let events = play(&mut r, vec![keep()]);
    assert_eq!(zone(&r, spell), Zone::Graveyard, "the Bolt was countered");
    assert_eq!(copies(&events).len(), 1);
    assert_eq!(r.life(P1), life - 3, "only the copy resolved");
    assert_idle(&r);
}

/// Dual Strike {R}{R} Instant: "mana value 4 or less". Inferno (mana value 7) is not the trigger
/// event and does not use the trigger up; the next Bolt is copied.
#[test]
fn dual_strike_copies_only_mana_value_four_or_less() {
    let mut s = main_phase();
    let dual = card(&mut s, "Dual Strike", Zone::Hand);
    let inferno = card(&mut s, "Inferno", Zone::Hand);
    let spells = bolts(&mut s, 2);
    s.with_mana_pool(P0, mana(ManaType::Red, 11));
    let mut r = s.build();
    r.state_mut().players[0].life = 40;
    cast(&mut r, dual, vec![]);
    assert_eq!(delayed(&r), 1);
    let events = cast(&mut r, inferno, vec![]);
    assert_eq!(copies(&events).len(), 0, "mana value 7 is above the cap");
    assert_eq!(delayed(&r), 1, "a non-matching cast leaves the trigger");
    let life = r.life(P1);
    bolt(&mut r, spells[0], 1);
    assert_eq!(r.life(P1), life - 6);
    bolt(&mut r, spells[1], 0);
    assert_eq!(r.life(P1), life - 9, "one-shot");
    assert_idle(&r);
}

/// Dual Strike {R}{R} Instant: the cap is inclusive. Deal Damage ({2}{R}{R}, mana value
/// exactly 4) is copied; Lava Axe ({4}{R}, mana value exactly 5) is above the cap and leaves
/// the trigger armed for the next cast.
#[test]
fn dual_strike_copies_mana_value_four_but_not_five() {
    let mut s = main_phase();
    let dual = card(&mut s, "Dual Strike", Zone::Hand);
    let axe = card(&mut s, "Lava Axe", Zone::Hand);
    let deal = card(&mut s, "Deal Damage", Zone::Hand);
    let spell = card(&mut s, BOLT, Zone::Hand);
    s.with_mana_pool(P0, mana(ManaType::Red, 12));
    let mut r = s.build();
    cast(&mut r, dual, vec![]);
    assert_eq!(delayed(&r), 1);
    let events = cast(&mut r, axe, vec![at_p1()]);
    assert_eq!(copies(&events).len(), 0, "mana value 5 is above the cap");
    assert_eq!(delayed(&r), 1, "a non-matching cast leaves the trigger");
    let life = r.life(P1);
    let events = cast(&mut r, deal, vec![at_p1(), keep()]);
    assert_eq!(copies(&events).len(), 1, "mana value 4 is the cap");
    assert_eq!(delayed(&r), 0, "the matching cast uses the trigger up");
    assert_eq!(r.life(P1), life - 8, "Deal Damage and its copy");
    bolt(&mut r, spell, 0);
    assert_eq!(r.life(P1), life - 11, "one-shot");
    assert_idle(&r);
}

/// Complete the Circuit {5}{U} Instant: "copy that spell twice" — two copies of one cast.
#[test]
fn complete_the_circuit_copies_the_next_spell_twice() {
    let mut s = main_phase();
    let circuit = card(&mut s, "Complete the Circuit", Zone::Hand);
    let spells = bolts(&mut s, 2);
    s.with_mana_pool(P0, mana(ManaType::Blue, 6));
    s.with_mana_pool(P0, mana(ManaType::Red, 2));
    let mut r = s.build();
    cast(&mut r, circuit, vec![]);
    assert_eq!(delayed(&r), 1);
    let life = r.life(P1);
    bolt(&mut r, spells[0], 2);
    assert_eq!(r.life(P1), life - 9, "Bolt and two copies");
    bolt(&mut r, spells[1], 0);
    assert_eq!(r.life(P1), life - 12);
    assert_idle(&r);
}

/// Storm King's Thunder {X}{R}{R}{R}: the next spell is copied X times (X = 2). CR 107.3.
#[test]
fn storm_kings_thunder_copies_the_next_spell_x_times() {
    let mut s = main_phase();
    let thunder = card(&mut s, "Storm King's Thunder", Zone::Hand);
    let spells = bolts(&mut s, 2);
    s.with_mana_pool(P0, mana(ManaType::Red, 7));
    let mut r = s.build();
    cast(
        &mut r,
        thunder,
        vec![Ask::X {
            actor: P0,
            value: 2,
        }],
    );
    assert_eq!(delayed(&r), 1);
    let life = r.life(P1);
    bolt(&mut r, spells[0], 2);
    assert_eq!(r.life(P1), life - 9, "Bolt and X = 2 copies");
    bolt(&mut r, spells[1], 0);
    assert_idle(&r);
}

/// Howl of the Horde {2}{R} Sorcery without Raid: one delayed copy.
#[test]
fn howl_of_the_horde_without_raid_copies_once() {
    let mut s = main_phase();
    let howl = card(&mut s, "Howl of the Horde", Zone::Hand);
    let spell = card(&mut s, BOLT, Zone::Hand);
    s.with_mana_pool(P0, mana(ManaType::Red, 4));
    let mut r = s.build();
    cast(&mut r, howl, vec![]);
    assert_eq!(delayed(&r), 1, "only the base trigger; Raid is off");
    let life = r.life(P1);
    bolt(&mut r, spell, 1);
    assert_eq!(r.life(P1), life - 6);
    assert_idle(&r);
}

/// Howl of the Horde after attacking: Raid adds "an additional time", so the next cast is copied twice.
#[test]
fn howl_of_the_horde_with_raid_copies_twice() {
    let mut s = main_phase();
    let attacker = card(&mut s, "Grizzly Bears", Zone::Battlefield);
    let howl = card(&mut s, "Howl of the Horde", Zone::Hand);
    let spell = card(&mut s, BOLT, Zone::Hand);
    let mut r = s.build();
    r.advance_to_phase(Phase::DeclareAttackers);
    r.act(GameAction::DeclareAttackers {
        attacks: vec![(attacker, AttackTarget::Player(P1))],
        bands: vec![],
    })
    .expect("Grizzly Bears attacks");
    drive(&mut r, |s| s.phase == Phase::PostCombatMain, vec![]);
    fund(&mut r, P0, ManaType::Red, 4);
    play(&mut r, vec![]);
    cast(&mut r, howl, vec![]);
    assert_eq!(delayed(&r), 2, "the base trigger and the Raid trigger");
    let life = r.life(P1);
    let events = bolt_with(&mut r, spell, reorder(), 2);
    // Both copies are copies of the cast Bolt. The Raid trigger's delayed `CopySpell` target is
    // `ParentTarget` with no declared target, so it copies through the untargeted fallback of
    // `copy_source_entry` (the stack top is the Bolt itself here, so `original_id` alone does not
    // tell that fallback from a global-recency read; the copy count is what fails without it).
    for (object_id, original_id) in copies(&events) {
        assert_eq!(
            original_id, spell,
            "copy {object_id:?} copies the cast Bolt"
        );
        assert_ne!(object_id, spell, "a copy is a new object");
    }
    assert_eq!(
        r.life(P1),
        life - 9,
        "Bolt, one copy and an additional copy"
    );
    assert_idle(&r);
}

/// Mercurial Spelldancer (combat damage, "you may remove two oil counters. If you do, ..."): attack,
/// answer the optional prompt, and return to an idle second main phase.
fn spelldancer_attack(accept: bool) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut s = main_phase();
    let dancer = card(&mut s, "Mercurial Spelldancer", Zone::Battlefield);
    s.with_counter(dancer, CounterType::Generic("oil".to_string()), 2);
    let spells = bolts(&mut s, 2);
    let mut r = s.build();
    r.advance_to_phase(Phase::DeclareAttackers);
    r.act(GameAction::DeclareAttackers {
        attacks: vec![(dancer, AttackTarget::Player(P1))],
        bands: vec![],
    })
    .expect("Mercurial Spelldancer attacks");
    drive(
        &mut r,
        |s| s.phase == Phase::PostCombatMain && s.stack.is_empty(),
        vec![Ask::Optional {
            actor: P0,
            source: dancer,
            accept,
        }],
    );
    (r, dancer, spells)
}

/// CR 603.7a: paying the two oil counters creates the delayed copy of the next instant or sorcery.
#[test]
fn mercurial_spelldancer_pays_two_oil_for_a_delayed_copy() {
    let (mut r, dancer, spells) = spelldancer_attack(true);
    assert_eq!(
        counters(&r, dancer, CounterType::Generic("oil".to_string())),
        0
    );
    assert_eq!(delayed(&r), 1);
    fund(&mut r, P0, ManaType::Red, 2);
    play(&mut r, vec![]);
    let life = r.life(P1);
    bolt_with(&mut r, spells[0], reorder(), 1);
    assert_eq!(r.life(P1), life - 6);
    bolt(&mut r, spells[1], 0);
    assert_eq!(r.life(P1), life - 9, "one-shot");
    assert_idle(&r);
}

/// "If you do": declining the payment creates no delayed trigger.
#[test]
fn mercurial_spelldancer_declined_makes_no_delayed_copy() {
    let (mut r, dancer, spells) = spelldancer_attack(false);
    assert_eq!(
        counters(&r, dancer, CounterType::Generic("oil".to_string())),
        2
    );
    assert_eq!(delayed(&r), 0);
    fund(&mut r, P0, ManaType::Red, 1);
    play(&mut r, vec![]);
    let life = r.life(P1);
    bolt(&mut r, spells[0], 0);
    assert_eq!(r.life(P1), life - 3);
    assert_idle(&r);
}

/// Cursed Recording {2}{R}{R}: "{T}: When you next cast ..." creates the delayed copy, and its own
/// "whenever you cast" trigger fires on the same cast (two simultaneous triggers: ordered).
#[test]
fn cursed_recording_tap_copies_the_next_instant_or_sorcery() {
    let mut s = main_phase();
    let recording = card(&mut s, "Cursed Recording", Zone::Battlefield);
    let spells = bolts(&mut s, 2);
    s.with_mana_pool(P0, mana(ManaType::Red, 2));
    let mut r = s.build();
    activate(&mut r, recording, 0, vec![]);
    assert_eq!(delayed(&r), 1);
    let life = r.life(P1);
    let events = cast(
        &mut r,
        spells[0],
        vec![
            at_p1(),
            Ask::ReverseOrder {
                actor: P0,
                count: 2,
            },
            keep(),
        ],
    );
    assert_eq!(copies(&events).len(), 1);
    assert_eq!(r.life(P1), life - 6);
    assert_eq!(counters(&r, recording, CounterType::Time), 1);
    assert_eq!(
        r.life(P0),
        20,
        "fewer than seven time counters: no damage to you"
    );
    assert_eq!(delayed(&r), 0);
    assert_idle(&r);
}

/// Cursed Recording's seventh time counter: "remove those counters and it deals 20 damage to you".
#[test]
fn cursed_recording_seventh_time_counter_deals_twenty_to_you() {
    let mut s = main_phase();
    let recording = card(&mut s, "Cursed Recording", Zone::Battlefield);
    s.with_counter(recording, CounterType::Time, 6);
    let spell = card(&mut s, BOLT, Zone::Hand);
    s.with_mana_pool(P0, mana(ManaType::Red, 1));
    let mut r = s.build();
    r.state_mut().players[0].life = 40;
    cast(&mut r, spell, vec![at_p1()]);
    assert_eq!(counters(&r, recording, CounterType::Time), 0);
    assert_eq!(r.life(P0), 20);
    assert_idle(&r);
}

/// Gadwick's First Duel III: "mana value 3 or less". Inferno does not match; the next Bolt does.
#[test]
fn gadwicks_first_duel_chapter_three_copies_mana_value_three_or_less() {
    let (mut s, saga) = saga_board("Gadwick's First Duel");
    let spell = card(&mut s, BOLT, Zone::Hand);
    let inferno = card(&mut s, "Inferno", Zone::Hand);
    let mut r = s.build();
    run_chapter(&mut r, saga, 3, vec![]);
    assert_eq!(delayed(&r), 1);
    r.state_mut().players[0].life = 40;
    fund(&mut r, P0, ManaType::Red, 8);
    let events = cast(&mut r, inferno, vec![]);
    assert_eq!(
        copies(&events).len(),
        0,
        "mana value 7 is above the cap of 3"
    );
    assert_eq!(delayed(&r), 1);
    let life = r.life(P1);
    bolt(&mut r, spell, 1);
    assert_eq!(r.life(P1), life - 6);
    assert_eq!(delayed(&r), 0);
    assert_idle(&r);
}

/// Gadwick's First Duel III: the cap is inclusive. Brimstone Volley ({2}{R}, mana value exactly
/// 3) is copied; Deal Damage ({2}{R}{R}, mana value exactly 4) is above the cap and
/// leaves the trigger armed for the next cast.
#[test]
fn gadwicks_first_duel_chapter_three_copies_mana_value_three_but_not_four() {
    let (mut s, saga) = saga_board("Gadwick's First Duel");
    let deal = card(&mut s, "Deal Damage", Zone::Hand);
    let volley = card(&mut s, "Brimstone Volley", Zone::Hand);
    let spell = card(&mut s, BOLT, Zone::Hand);
    let mut r = s.build();
    run_chapter(&mut r, saga, 3, vec![]);
    assert_eq!(delayed(&r), 1);
    fund(&mut r, P0, ManaType::Red, 8);
    let events = cast(&mut r, deal, vec![at_p1()]);
    assert_eq!(
        copies(&events).len(),
        0,
        "mana value 4 is above the cap of 3"
    );
    assert_eq!(delayed(&r), 1, "a non-matching cast leaves the trigger");
    let life = r.life(P1);
    let events = cast(&mut r, volley, vec![at_p1(), keep()]);
    assert_eq!(copies(&events).len(), 1, "mana value 3 is the cap");
    assert_eq!(delayed(&r), 0, "the matching cast uses the trigger up");
    assert_eq!(r.life(P1), life - 6, "Brimstone Volley and its copy");
    bolt(&mut r, spell, 0);
    assert_eq!(r.life(P1), life - 9, "one-shot");
    assert_idle(&r);
}

/// Way of the Cryomancer: planeswalkers you control have "[-3]: When you next cast ... copy that
/// spell". Empower Jace creates the printed Jace token, whose granted loyalty ability is activated.
#[test]
fn way_of_the_cryomancer_grants_planeswalkers_the_delayed_copy() {
    let mut s = main_phase();
    let way = card(&mut s, "Way of the Cryomancer", Zone::Hand);
    let spells = bolts(&mut s, 2);
    s.with_mana_pool(P0, mana(ManaType::Blue, 3));
    s.with_mana_pool(P0, mana(ManaType::Red, 2));
    let mut r = s.build();
    cast(&mut r, way, vec![]);
    let jace = r
        .state()
        .objects
        .values()
        .find_map(|o| (o.name == "Jace" && o.zone == Zone::Battlefield).then_some(o.id))
        .expect("empower Jace created the Jace token");
    assert_eq!(counters(&r, jace, CounterType::Loyalty), 5);
    engine::game::layers::evaluate_layers(r.state_mut());
    let granted = r.state().objects[&jace]
        .abilities
        .iter()
        .position(|a| {
            matches!(
                &*a.effect,
                Effect::CreateDelayedTrigger {
                    condition: _,
                    effect: _,
                    uses_tracked_set: _
                }
            )
        })
        .expect("the granted loyalty ability is on the planeswalker");
    activate(&mut r, jace, granted, vec![]);
    assert_eq!(counters(&r, jace, CounterType::Loyalty), 2, "-3 paid");
    assert_eq!(delayed(&r), 1);
    let life = r.life(P1);
    bolt(&mut r, spells[0], 1);
    assert_eq!(r.life(P1), life - 6);
    bolt(&mut r, spells[1], 0);
    assert_idle(&r);
}

// ---------------------------------------------------------------------------------------------
// "copy it": the bare pronoun names the cast spell
// ---------------------------------------------------------------------------------------------

/// Summon: G.F. Cerberus II ("Double — ... copy it"): one copy of the next instant or sorcery.
#[test]
fn cerberus_chapter_two_copies_once() {
    let (mut s, saga) = saga_board("Summon: G.F. Cerberus");
    let spells = bolts(&mut s, 2);
    let mut r = s.build();
    run_chapter(&mut r, saga, 2, vec![]);
    assert_eq!(delayed(&r), 1);
    fund(&mut r, P0, ManaType::Red, 2);
    let life = r.life(P1);
    bolt(&mut r, spells[0], 1);
    assert_eq!(r.life(P1), life - 6);
    bolt(&mut r, spells[1], 0);
    assert_eq!(r.life(P1), life - 9);
    assert_idle(&r);
}

/// Summon: G.F. Cerberus III ("Triple — ... copy it twice"): the outcome is what is witnessed —
/// Bolt plus two copies from one cast, and no next-cast trigger survives that cast. The chapter parses
/// as two delayed triggers (`repeat_for: 2`), not one with a count, so the first assertion is `>= 1`.
#[test]
fn cerberus_chapter_three_copies_twice() {
    let (mut s, saga) = saga_board("Summon: G.F. Cerberus");
    let spells = bolts(&mut s, 2);
    let mut r = s.build();
    run_chapter(&mut r, saga, 3, vec![]);
    assert!(delayed(&r) >= 1, "the chapter leaves a next-cast trigger");
    fund(&mut r, P0, ManaType::Red, 2);
    let life = r.life(P1);
    bolt(&mut r, spells[0], 2);
    assert_eq!(r.life(P1), life - 9, "Bolt and two copies");
    assert_eq!(
        delayed(&r),
        0,
        "the chapter's triggers are all used up by one cast"
    );
    bolt(&mut r, spells[1], 0);
    assert_eq!(r.life(P1), life - 12);
    assert_idle(&r);
}

/// Rimefire Torque (the same body as a gain, no census row): the same "copy it" pronoun in an activated ability.
#[test]
fn rimefire_torque_copies_it() {
    let mut s = main_phase();
    let torque = card(&mut s, "Rimefire Torque", Zone::Battlefield);
    s.with_counter(torque, CounterType::Generic("charge".to_string()), 3);
    let spells = bolts(&mut s, 2);
    s.with_mana_pool(P0, mana(ManaType::Red, 2));
    let mut r = s.build();
    activate(&mut r, torque, 0, vec![]);
    assert_eq!(delayed(&r), 1);
    let life = r.life(P1);
    bolt(&mut r, spells[0], 1);
    assert_eq!(r.life(P1), life - 6);
    bolt(&mut r, spells[1], 0);
    assert_idle(&r);
}

/// The Clone Saga II: "copy it, except the copy isn't legendary", for a creature spell. A Bolt does
/// not match; the copy of Isamaru enters as a nonlegendary token (CR 707.10, CR 111.13).
#[test]
fn the_clone_saga_chapter_two_copies_a_creature_spell_nonlegendary() {
    let (mut s, saga) = saga_board("The Clone Saga");
    let isamaru = card(&mut s, "Isamaru, Hound of Konda", Zone::Hand);
    let spell = card(&mut s, BOLT, Zone::Hand);
    let mut r = s.build();
    run_chapter(&mut r, saga, 2, vec![]);
    assert_eq!(delayed(&r), 1);
    fund(&mut r, P0, ManaType::Red, 1);
    fund(&mut r, P0, ManaType::White, 1);
    let life = r.life(P1);
    bolt(&mut r, spell, 0);
    assert_eq!(r.life(P1), life - 3);
    assert_eq!(delayed(&r), 1, "a noncreature spell leaves the trigger");
    let events = cast(&mut r, isamaru, vec![]);
    assert_eq!(copies(&events).len(), 1, "the creature spell was copied");
    let isamarus: Vec<_> = r
        .state()
        .objects
        .values()
        .filter(|o| o.name == "Isamaru, Hound of Konda" && o.zone == Zone::Battlefield)
        .collect();
    assert_eq!(isamarus.len(), 2, "the original and its token copy");
    let legendary = |o: &engine::game::game_object::GameObject| {
        o.card_types.supertypes.contains(&Supertype::Legendary)
    };
    let original = isamarus.iter().find(|o| o.id == isamaru).expect("original");
    let copy = isamarus.iter().find(|o| o.id != isamaru).expect("copy");
    assert!(legendary(original), "the original stays legendary");
    assert!(
        copy.is_token && !legendary(copy),
        "the copy is a nonlegendary token"
    );
    assert_idle(&r);
}

// ---------------------------------------------------------------------------------------------
// Creature-spell riders and the search body
// ---------------------------------------------------------------------------------------------

/// A Saga chapter whose rider adds counters to the next creature spell: only the first of two
/// Grizzly Bears gets them.
fn counter_rider_saga(name: &str, expected: u32) {
    let (mut s, saga) = saga_board(name);
    let first = card(&mut s, "Grizzly Bears", Zone::Hand);
    let second = card(&mut s, "Grizzly Bears", Zone::Hand);
    let mut r = s.build();
    run_chapter(&mut r, saga, 2, vec![]);
    assert_eq!(delayed(&r), 1);
    fund(&mut r, P0, ManaType::Green, 4);
    cast(&mut r, first, vec![]);
    assert_eq!(counters(&r, first, CounterType::Plus1Plus1), expected);
    cast(&mut r, second, vec![]);
    assert_eq!(
        counters(&r, second, CounterType::Plus1Plus1),
        0,
        "only the next creature spell"
    );
    assert_idle(&r);
}

/// Kumano Faces Kakkazan II: "that creature enters with an additional +1/+1 counter".
#[test]
fn kumano_chapter_two_adds_a_counter_to_the_next_creature() {
    counter_rider_saga("Kumano Faces Kakkazan", 1);
}

/// Summon: Fenrir II: the same rider.
#[test]
fn summon_fenrir_chapter_two_adds_a_counter_to_the_next_creature() {
    counter_rider_saga("Summon: Fenrir", 1);
}

/// Yuna, Grand Summoner: "{T}: Add one mana of any color. When you next cast a creature spell this
/// turn, that creature enters with two additional +1/+1 counters."
#[test]
fn yuna_tap_for_mana_gives_the_next_creature_two_counters() {
    let mut s = main_phase();
    let yuna = card(&mut s, "Yuna, Grand Summoner", Zone::Battlefield);
    let first = card(&mut s, "Grizzly Bears", Zone::Hand);
    let second = card(&mut s, "Grizzly Bears", Zone::Hand);
    s.with_mana_pool(P0, mana(ManaType::Green, 4));
    let mut r = s.build();
    activate(
        &mut r,
        yuna,
        0,
        vec![Ask::Color {
            actor: P0,
            color: ManaType::Green,
        }],
    );
    assert_eq!(delayed(&r), 1);
    cast(&mut r, first, vec![]);
    assert_eq!(counters(&r, first, CounterType::Plus1Plus1), 2);
    cast(&mut r, second, vec![]);
    assert_eq!(counters(&r, second, CounterType::Plus1Plus1), 0);
    assert_idle(&r);
}

/// Summon: Brynhildr II: "it gains haste until end of turn" — the cast creature, not the Saga.
#[test]
fn summon_brynhildr_chapter_two_gives_the_next_creature_haste() {
    let (mut s, saga) = saga_board("Summon: Brynhildr");
    let first = card(&mut s, "Grizzly Bears", Zone::Hand);
    let second = card(&mut s, "Grizzly Bears", Zone::Hand);
    pad_libraries(&mut s);
    let mut r = s.build();
    run_chapter(&mut r, saga, 2, vec![]);
    assert_eq!(delayed(&r), 1);
    fund(&mut r, P0, ManaType::Green, 4);
    cast(&mut r, first, vec![]);
    cast(&mut r, second, vec![]);
    let haste =
        |r: &GameRunner, id: ObjectId| r.state().objects[&id].keywords.contains(&Keyword::Haste);
    assert!(haste(&r, first), "the creature spell cast gains haste");
    assert!(!haste(&r, saga), "the Saga does not");
    assert!(!haste(&r, second), "only the next creature spell");
    assert_idle(&r);
    pass_to_own_turn(&mut r, vec![Ask::NoAttack { actor: P0 }]);
    assert!(!haste(&r, first), "haste ended with the turn");
    play(&mut r, vec![]);
    assert_idle(&r);
}

/// Vivien, Monsters' Advocate -2: the delayed search is for a creature with lesser mana value than
/// the cast spell (Gray Ogre, 3), and "put it onto the battlefield" names the found card.
#[test]
fn vivien_minus_two_searches_for_lesser_mana_value() {
    let mut s = main_phase();
    let vivien = card(&mut s, "Vivien, Monsters' Advocate", Zone::Battlefield);
    let ogre = card(&mut s, "Gray Ogre", Zone::Hand);
    let lions = card(&mut s, "Savannah Lions", Zone::Library);
    let bears = card(&mut s, "Grizzly Bears", Zone::Library);
    let second_ogre = card(&mut s, "Gray Ogre", Zone::Library);
    let giant = card(&mut s, "Hill Giant", Zone::Library);
    s.with_mana_pool(P0, mana(ManaType::Red, 3));
    let mut r = s.build();
    let loyalty = counters(&r, vivien, CounterType::Loyalty);
    activate(&mut r, vivien, 1, vec![]);
    assert_eq!(counters(&r, vivien, CounterType::Loyalty), loyalty - 2);
    assert_eq!(delayed(&r), 1);
    cast(
        &mut r,
        ogre,
        vec![Ask::Search {
            actor: P0,
            offered: vec![lions, bears],
            pick: lions,
        }],
    );
    assert_eq!(
        zone(&r, lions),
        Zone::Battlefield,
        "the found creature is put onto the battlefield"
    );
    assert_eq!(
        zone(&r, ogre),
        Zone::Battlefield,
        "the cast creature spell resolved"
    );
    assert_eq!(zone(&r, bears), Zone::Library);
    assert_eq!(zone(&r, second_ogre), Zone::Library);
    assert_eq!(zone(&r, giant), Zone::Library);
    assert_eq!(delayed(&r), 0);
    assert_idle(&r);
}

/// Codie, Vociferous Codex "{4}, {T}: Add {W}{U}{B}{R}{G}. When you next cast a spell this turn,
/// exile cards from the top of your library until you exile an instant or sorcery card with lesser
/// mana value. Until end of turn, you may cast that card without paying its mana cost. Put each
/// other card exiled this way on the bottom of your library". Helix (2) makes Terminate (2) a miss.
#[test]
fn codie_finds_a_lesser_instant_and_bottoms_the_rest() {
    let mut s = main_phase();
    let codie = card(&mut s, "Codie, Vociferous Codex", Zone::Battlefield);
    let helix = card(&mut s, "Lightning Helix", Zone::Hand);
    let unreached = card(&mut s, "Shock", Zone::Library);
    let hit = card(&mut s, BOLT, Zone::Library);
    let equal = card(&mut s, "Terminate", Zone::Library);
    let land = card(&mut s, "Mountain", Zone::Library);
    s.with_mana_pool(P0, mana(ManaType::Colorless, 4));
    s.with_mana_pool(P0, mana(ManaType::Red, 1));
    s.with_mana_pool(P0, mana(ManaType::White, 1));
    let mut r = s.build();
    seat_library_top(&mut r, &[land, equal, hit, unreached]);
    activate(&mut r, codie, 0, vec![]);
    assert_eq!(delayed(&r), 1);
    cast(&mut r, helix, vec![at_p1()]);
    assert_eq!(zone(&r, hit), Zone::Exile, "the lesser instant was exiled");
    assert!(!r.state().objects[&hit].casting_permissions.is_empty());
    let library: Vec<ObjectId> = r.state().players[0].library.iter().copied().collect();
    assert_eq!(
        library.first(),
        Some(&unreached),
        "cards below the hit are untouched"
    );
    for miss in [land, equal] {
        assert!(library[1..].contains(&miss), "a miss goes to the bottom");
    }
    assert_idle(&r);
    let life = r.life(P1);
    let pool = r.state().players[0].mana_pool.total();
    assert!(
        pool > 0,
        "mana is available, so a charged cast would be payable"
    );
    cast(&mut r, hit, vec![at_p1()]);
    assert_eq!(
        r.state().players[0].mana_pool.total(),
        pool,
        "cast without paying its mana cost"
    );
    assert_eq!(r.life(P1), life - 3, "the free Bolt resolved");
    assert_idle(&r);
}

/// Magus Lucea Kane "Psychic Stimulus — {T}: Add {C}{C}. When you next cast a spell with {X} in its
/// mana cost or activate an ability with {X} in its activation cost this turn, copy that spell or
/// ability": a Bolt (no X) is not copied, Blaze with X = 2 is.
#[test]
fn magus_lucea_kane_copies_the_next_x_spell_not_a_plain_one() {
    let mut s = main_phase();
    let magus = card(&mut s, "Magus Lucea Kane", Zone::Battlefield);
    let spell = card(&mut s, BOLT, Zone::Hand);
    let blaze = card(&mut s, "Blaze", Zone::Hand);
    s.with_mana_pool(P0, mana(ManaType::Red, 4));
    let mut r = s.build();
    activate(&mut r, magus, 0, vec![]);
    assert_eq!(delayed(&r), 1);
    let life = r.life(P1);
    bolt(&mut r, spell, 0);
    assert_eq!(delayed(&r), 1, "a spell without X leaves the trigger");
    assert_eq!(r.life(P1), life - 3);
    let events = cast(
        &mut r,
        blaze,
        vec![
            at_p1(),
            Ask::X {
                actor: P0,
                value: 2,
            },
            keep(),
        ],
    );
    assert_eq!(copies(&events).len(), 1, "the X spell is copied");
    assert_eq!(r.life(P1), life - 3 - 4, "Blaze with X = 2 and its copy");
    assert_eq!(delayed(&r), 0);
    assert_idle(&r);
}

/// Adaptive Training Post (a silent misparse the pin corrects, no census row): "copy it" in an activated ability.
#[test]
fn adaptive_training_post_copies_it() {
    let mut s = main_phase();
    let post = card(&mut s, "Adaptive Training Post", Zone::Battlefield);
    s.with_counter(post, CounterType::Generic("charge".to_string()), 3);
    let spells = bolts(&mut s, 2);
    s.with_mana_pool(P0, mana(ManaType::Red, 2));
    let mut r = s.build();
    activate(&mut r, post, 0, vec![]);
    assert_eq!(delayed(&r), 1);
    let life = r.life(P1);
    bolt_with(&mut r, spells[0], reorder(), 1);
    assert_eq!(r.life(P1), life - 6);
    bolt(&mut r, spells[1], 0);
    assert_idle(&r);
}

/// Najal, the Storm Runner (a silent misparse the pin corrects, no census row): "Whenever Najal attacks, you may pay {2}. If you do,
/// when you next cast an instant or sorcery spell this turn, copy it."
#[test]
fn najal_attack_trigger_creates_a_delayed_copy_of_the_next_spell() {
    let mut s = main_phase();
    let najal = card(&mut s, "Najal, the Storm Runner", Zone::Battlefield);
    let spells = bolts(&mut s, 2);
    pad_libraries(&mut s);
    let mut r = s.build();
    r.advance_to_phase(Phase::DeclareAttackers);
    r.act(GameAction::DeclareAttackers {
        attacks: vec![(najal, AttackTarget::Player(P1))],
        bands: vec![],
    })
    .expect("Najal attacks");
    fund(&mut r, P0, ManaType::Colorless, 2);
    drive(
        &mut r,
        |s| s.stack.is_empty() && matches!(s.waiting_for, WaitingFor::Priority { player: _ }),
        vec![Ask::Optional {
            actor: P0,
            source: najal,
            accept: true,
        }],
    );
    assert_eq!(delayed(&r), 1, "paying {{2}} created the delayed copy");
    fund(&mut r, P0, ManaType::Red, 2);
    let life = r.life(P1);
    bolt(&mut r, spells[0], 1);
    assert_eq!(r.life(P1), life - 6);
    bolt(&mut r, spells[1], 0);
    assert_eq!(r.life(P1), life - 9);
    assert_idle(&r);
}

/// Jeong Jeong, the Deserter (same body, no census row): the exhaust ability copies the next Lesson.
#[test]
fn jeong_jeong_exhaust_copies_the_next_lesson() {
    let mut s = main_phase();
    let jeong = card(&mut s, "Jeong Jeong, the Deserter", Zone::Battlefield);
    let cruelty = card(&mut s, "Ozai's Cruelty", Zone::Hand);
    let spell = card(&mut s, BOLT, Zone::Hand);
    s.with_mana_pool(P0, mana(ManaType::Colorless, 3));
    s.with_mana_pool(P0, mana(ManaType::Black, 3));
    s.with_mana_pool(P0, mana(ManaType::Red, 1));
    let mut r = s.build();
    activate(&mut r, jeong, 0, vec![]);
    assert_eq!(delayed(&r), 1);
    let life = r.life(P1);
    bolt(&mut r, spell, 0);
    assert_eq!(delayed(&r), 1, "a non-Lesson spell leaves the trigger");
    let life = life - 3;
    let events = cast(&mut r, cruelty, vec![at_p1(), keep()]);
    assert_eq!(copies(&events).len(), 1);
    assert_eq!(
        r.life(P1),
        life - 4,
        "Ozai's Cruelty and its copy each deal 2"
    );
    assert_idle(&r);
}

// ---------------------------------------------------------------------------------------------
// The other clauses the released census rows cite
// ---------------------------------------------------------------------------------------------

/// The Clone Saga III: "Choose a card name. Whenever a creature with the chosen name deals combat
/// damage to a player this turn, draw a card." Only the named creature draws.
#[test]
fn the_clone_saga_chapter_three_draws_for_the_chosen_name_only() {
    let (mut s, saga) = saga_board("The Clone Saga");
    let bears = card(&mut s, "Grizzly Bears", Zone::Battlefield);
    let lions = card(&mut s, "Savannah Lions", Zone::Battlefield);
    pad_libraries(&mut s);
    let mut r = s.build();
    r.state_mut().all_card_names =
        std::sync::Arc::from(["Grizzly Bears".to_string(), "Savannah Lions".to_string()]);
    run_chapter(
        &mut r,
        saga,
        3,
        vec![Ask::Name {
            actor: P0,
            pick: "Grizzly Bears",
        }],
    );
    assert_eq!(delayed(&r), 1, "the 'whenever' draw trigger exists");
    let hand = r.state().players[0].hand.len();
    r.advance_to_phase(Phase::DeclareAttackers);
    r.act(GameAction::DeclareAttackers {
        attacks: vec![
            (bears, AttackTarget::Player(P1)),
            (lions, AttackTarget::Player(P1)),
        ],
        bands: vec![],
    })
    .expect("both attack");
    drive(&mut r, |s| s.phase == Phase::PostCombatMain, vec![]);
    assert_eq!(
        r.state().players[0].hand.len(),
        hand + 1,
        "only the Bears has the chosen name"
    );
    play(&mut r, vec![]);
    assert_idle(&r);
    pass_to_own_turn(&mut r, vec![]);
    assert_eq!(delayed(&r), 0, "the 'this turn' draw trigger expired");
    play(&mut r, vec![]);
    assert_idle(&r);
}

/// Summon: Brynhildr I exiles the top card and offers it to be played; the exiled land is playable the
/// turn the chapter resolves. (The "any turn a lore counter is put on this Saga" window of later turns
/// is not witnessed here: that clause is why the census row stays.)
#[test]
fn summon_brynhildr_chapter_one_exiles_a_card_playable_the_same_turn() {
    let (mut s, saga) = saga_board("Summon: Brynhildr");
    let land = card(&mut s, "Mountain", Zone::Library);
    let mut r = s.build();
    seat_library_top(&mut r, &[land]);
    run_chapter(
        &mut r,
        saga,
        1,
        vec![Ask::Optional {
            actor: P0,
            source: saga,
            accept: true,
        }],
    );
    assert_eq!(zone(&r, land), Zone::Exile, "chapter I exiled the top card");
    let card_id = r.state().objects[&land].card_id;
    r.act(GameAction::PlayLand {
        object_id: land,
        card_id,
    })
    .expect("the exiled land may be played this turn");
    assert_eq!(zone(&r, land), Zone::Battlefield);
    assert_idle(&r);
}

/// Gadwick's First Duel I: a Cursed Role (enchanted creature is 1/1) attached to the Giant.
#[test]
fn gadwicks_first_duel_chapter_one_attaches_a_cursed_role() {
    let (mut s, saga) = saga_board("Gadwick's First Duel");
    let giant = card(&mut s, "Hill Giant", Zone::Battlefield);
    let mut r = s.build();
    run_chapter(&mut r, saga, 1, vec![]);
    engine::game::layers::evaluate_layers(r.state_mut());
    assert!(r
        .state()
        .objects
        .values()
        .any(|o| o.name == "Cursed Role" && o.zone == Zone::Battlefield));
    assert_eq!(r.state().objects[&giant].power, Some(1));
    assert_eq!(r.state().objects[&giant].toughness, Some(1));
    assert_idle(&r);
}

/// Magus Lucea Kane's activation leg: Kessig Wolf Run's {X} ability is copied (X = 2, same target).
#[test]
fn magus_lucea_kane_copies_the_next_x_activation() {
    let mut s = main_phase();
    let magus = card(&mut s, "Magus Lucea Kane", Zone::Battlefield);
    let wolf_run = card(&mut s, "Kessig Wolf Run", Zone::Battlefield);
    s.with_mana_pool(P0, mana(ManaType::Red, 3));
    s.with_mana_pool(P0, mana(ManaType::Green, 1));
    let mut r = s.build();
    activate(&mut r, magus, 0, vec![]);
    assert_eq!(delayed(&r), 1);
    let power = r.state().objects[&magus].power.expect("power");
    activate(
        &mut r,
        wolf_run,
        1,
        vec![
            Ask::X {
                actor: P0,
                value: 2,
            },
            keep(),
        ],
    );
    engine::game::layers::evaluate_layers(r.state_mut());
    assert_eq!(
        r.state().objects[&magus].power,
        Some(power + 4),
        "the activation and its copy"
    );
    assert_eq!(delayed(&r), 0);
    assert_idle(&r);
}

/// CR 514.2: Kumano's rider is "this turn"; unfired, it is gone next turn and a later creature
/// spell gets no counter.
#[test]
fn kumano_chapter_two_rider_expires_at_end_of_turn() {
    let (mut s, saga) = saga_board("Kumano Faces Kakkazan");
    let creature = card(&mut s, "Grizzly Bears", Zone::Hand);
    pad_libraries(&mut s);
    let mut r = s.build();
    run_chapter(&mut r, saga, 2, vec![]);
    assert_eq!(delayed(&r), 1);
    pass_to_own_turn(&mut r, vec![]);
    assert_eq!(r.state().active_player, P0);
    assert_eq!(delayed(&r), 0, "expired with the turn");
    play(&mut r, vec![]);
    fund(&mut r, P0, ManaType::Green, 2);
    cast(&mut r, creature, vec![]);
    assert_eq!(counters(&r, creature, CounterType::Plus1Plus1), 0);
    assert_idle(&r);
}

/// CR 514.2: the delayed triggers made by Cursed Recording's ability and Yuna's mana ability expire.
#[test]
fn cursed_recording_and_yuna_delayed_triggers_expire_at_end_of_turn() {
    let mut s = main_phase();
    let recording = card(&mut s, "Cursed Recording", Zone::Battlefield);
    let yuna = card(&mut s, "Yuna, Grand Summoner", Zone::Battlefield);
    pad_libraries(&mut s);
    let mut r = s.build();
    activate(&mut r, recording, 0, vec![]);
    activate(
        &mut r,
        yuna,
        0,
        vec![Ask::Color {
            actor: P0,
            color: ManaType::Green,
        }],
    );
    assert_eq!(
        delayed(&r),
        2,
        "one from the activated ability, one from the mana ability"
    );
    pass_to_own_turn(&mut r, vec![]);
    assert_eq!(delayed(&r), 0, "both expired with the turn");
    play(&mut r, vec![]);
    assert_idle(&r);
}

/// CR 514.2: Codie's free-cast window lasts "until end of turn": the next turn the exiled Bolt cannot be cast.
#[test]
fn codie_free_cast_permission_lapses_at_end_of_turn() {
    let mut s = main_phase();
    let codie = card(&mut s, "Codie, Vociferous Codex", Zone::Battlefield);
    let helix = card(&mut s, "Lightning Helix", Zone::Hand);
    let other = card(&mut s, BOLT, Zone::Hand);
    let hit = card(&mut s, BOLT, Zone::Library);
    pad_libraries(&mut s);
    s.with_mana_pool(P0, mana(ManaType::Colorless, 4));
    s.with_mana_pool(P0, mana(ManaType::Red, 2));
    s.with_mana_pool(P0, mana(ManaType::White, 1));
    let mut r = s.build();
    seat_library_top(&mut r, &[hit]);
    activate(&mut r, codie, 0, vec![]);
    cast(&mut r, helix, vec![at_p1()]);
    assert_eq!(zone(&r, hit), Zone::Exile, "the lesser instant was exiled");
    assert!(!r.state().objects[&hit].casting_permissions.is_empty());
    let library = r.state().players[0].library.len();
    cast(&mut r, other, vec![at_p1()]);
    assert_eq!(
        r.state().players[0].library.len(),
        library,
        "the trigger was used up"
    );
    pass_to_own_turn(&mut r, vec![]);
    play(&mut r, vec![]);
    assert_idle(&r);
    assert_eq!(zone(&r, hit), Zone::Exile);
    assert!(
        r.state().objects[&hit].casting_permissions.is_empty(),
        "the free-cast permission ended with the turn"
    );
    fund(&mut r, P0, ManaType::Red, 1);
    let card_id = r.state().objects[&hit].card_id;
    assert!(
        r.act(GameAction::CastSpell {
            object_id: hit,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .is_err(),
        "the exiled card cannot be cast after the turn it was exiled"
    );
    assert_idle(&r);
}

/// "You may choose new targets for the copy" is a real choice (CR 707.10c): the copy of the Bolt is
/// moved to its controller, the original still hits the opponent.
#[test]
fn striking_palette_copy_may_choose_a_new_target() {
    let mut s = main_phase();
    let wrangler = card(&mut s, "Pigment Wrangler", Zone::Battlefield);
    let spell = card(&mut s, BOLT, Zone::Hand);
    s.with_mana_pool(P0, mana(ManaType::Red, 2));
    let mut r = s.build();
    r.state_mut().players[0].life = 40;
    cast_prepared_palette(&mut r, wrangler);
    let (own, theirs) = (r.life(P0), r.life(P1));
    let events = cast(
        &mut r,
        spell,
        vec![
            at_p1(),
            Ask::Retarget {
                actor: P0,
                to: TargetRef::Player(P0),
            },
        ],
    );
    assert_eq!(copies(&events).len(), 1);
    assert_eq!(
        r.life(P0),
        own - 3,
        "the copy was retargeted at its controller"
    );
    assert_eq!(r.life(P1), theirs - 3, "the original hit the opponent");
    assert_idle(&r);
}
