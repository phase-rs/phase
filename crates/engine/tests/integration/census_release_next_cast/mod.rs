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
use engine::game::scenario::{GameRunner, GameScenario, OptionalPolicy, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{ChoiceType, Effect, ReplacementDefinition, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::Supertype;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, GameState, ManaChoice, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
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
    /// `Stop` is rejected — a listed prompt must be answered to finish the witness.
    Optional {
        actor: PlayerId,
        source: ObjectId,
        policy: OptionalPolicy,
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
                    policy,
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
                let accept = match policy {
                    OptionalPolicy::Accept => true,
                    OptionalPolicy::Decline => false,
                    OptionalPolicy::Stop => {
                        panic!("optional prompt for {source:?} must be answered by the script")
                    }
                };
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

/// The floating entry riders installed for `spell`: `ChangeZone` replacements
/// pinned to that spell occurrence (CR 400.7 + CR 614.1c). The three next-cast
/// counter cards install these instead of `pending_etb_counters` rows.
fn entry_riders(state: &GameState, spell: ObjectId) -> Vec<&ReplacementDefinition> {
    state
        .pending_damage_replacements
        .iter()
        .filter(|def| {
            def.event == ReplacementEvent::ChangeZone
                && def
                    .triggering_spell
                    .is_some_and(|pin| pin.object_id == spell)
                && matches!(&def.valid_card, Some(TargetFilter::SpecificObject { id }) if *id == spell)
        })
        .collect()
}

fn entry_rider_installed(state: &GameState, spell: ObjectId) -> bool {
    !entry_riders(state, spell).is_empty()
}

/// `spell`'s one-shot entry rider was installed once and consumed by its entry
/// (CR 614.1c + CR 614.5).
fn assert_entry_rider_consumed(r: &GameRunner, spell: ObjectId) {
    let riders = entry_riders(r.state(), spell);
    assert_eq!(riders.len(), 1, "one floating entry rider for {spell:?}");
    assert!(
        riders[0].is_consumed,
        "the entry consumed its one-shot rider"
    );
}

/// The triggered abilities waiting on the stack, in stack order (CR 603.7: the
/// delayed trigger and any Strionic Resonator copy of it).
fn triggered_on_stack(state: &GameState) -> Vec<ObjectId> {
    state
        .stack
        .iter()
        .filter(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. }))
        .map(|entry| entry.id)
        .collect()
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
fn spelldancer_attack(policy: OptionalPolicy) -> (GameRunner, ObjectId, Vec<ObjectId>) {
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
            policy,
        }],
    );
    (r, dancer, spells)
}

/// CR 603.7a: paying the two oil counters creates the delayed copy of the next instant or sorcery.
#[test]
fn mercurial_spelldancer_pays_two_oil_for_a_delayed_copy() {
    let (mut r, dancer, spells) = spelldancer_attack(OptionalPolicy::Accept);
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
    let (mut r, dancer, spells) = spelldancer_attack(OptionalPolicy::Decline);
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
///
/// The rider installs as a floating replacement pinned to the creature spell's cast occurrence
/// (CR 400.7 + CR 614.1c), so the Remand witnesses below cover the recast of a spell that left the
/// stack.
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
    assert_entry_rider_consumed(&r, first);
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
    assert_entry_rider_consumed(&r, first);
    cast(&mut r, second, vec![]);
    assert_eq!(counters(&r, second, CounterType::Plus1Plus1), 0);
    assert_idle(&r);
}

/// P0's next creature spell `bears` carries the installed entry rider (the delayed trigger has
/// resolved), and P1 Remands it. The rider must not follow the card to hand: P0 recasts `bears`
/// and it enters with no counters (CR 400.7: the recast card is a new object, CR 614.1c).
fn remanded_creature_recast_has_no_counters(mut r: GameRunner, bears: ObjectId) {
    let remand = r
        .state()
        .objects
        .iter()
        .find_map(|(id, o)| (o.name == "Remand" && o.owner == P1).then_some(*id))
        .expect("P1 holds Remand");
    fund(&mut r, P0, ManaType::Green, 4);
    fund(&mut r, P1, ManaType::Blue, 2);
    cast_action(&mut r, bears);
    drive(&mut r, |s| entry_rider_installed(s, bears), vec![]);
    assert_eq!(
        zone(&r, bears),
        Zone::Stack,
        "the rider was installed for the spell on the stack"
    );
    assert!(matches!(r.state().waiting_for, WaitingFor::Priority { player } if player == P0));
    r.act(GameAction::PassPriority).expect("P0 passes");
    assert!(matches!(r.state().waiting_for, WaitingFor::Priority { player } if player == P1));
    cast_action(&mut r, remand);
    // Remand's only legal target is the creature spell, so the engine announces it itself.
    play(&mut r, vec![]);
    assert_eq!(
        zone(&r, bears),
        Zone::Hand,
        "Remand returned the creature spell to its owner's hand"
    );
    // The install stays pinned to the departed occurrence (CR 400.7), so the
    // recast cannot inherit it; the outcome assertion below is the witness.
    cast(&mut r, bears, vec![]);
    assert_eq!(zone(&r, bears), Zone::Battlefield);
    assert_eq!(
        counters(&r, bears, CounterType::Plus1Plus1),
        0,
        "the recast card is a new object"
    );
    assert_idle(&r);
}

fn remand_for_p1(s: &mut GameScenario) {
    s.add_real_card(P1, "Remand", Zone::Hand, db());
    pad_libraries(s);
}

/// Kumano Faces Kakkazan II, Summon: Fenrir II: a Remanded creature spell recast gets no counter.
#[test]
fn kumano_counter_rider_does_not_follow_a_remanded_creature() {
    let (mut s, saga) = saga_board("Kumano Faces Kakkazan");
    let bears = card(&mut s, "Grizzly Bears", Zone::Hand);
    remand_for_p1(&mut s);
    let mut r = s.build();
    run_chapter(&mut r, saga, 2, vec![]);
    remanded_creature_recast_has_no_counters(r, bears);
}

#[test]
fn summon_fenrir_counter_rider_does_not_follow_a_remanded_creature() {
    let (mut s, saga) = saga_board("Summon: Fenrir");
    let bears = card(&mut s, "Grizzly Bears", Zone::Hand);
    remand_for_p1(&mut s);
    let mut r = s.build();
    run_chapter(&mut r, saga, 2, vec![]);
    remanded_creature_recast_has_no_counters(r, bears);
}

/// Yuna, Grand Summoner: the same for her two-counter rider.
#[test]
fn yuna_counter_rider_does_not_follow_a_remanded_creature() {
    let mut s = main_phase();
    let yuna = card(&mut s, "Yuna, Grand Summoner", Zone::Battlefield);
    let bears = card(&mut s, "Grizzly Bears", Zone::Hand);
    remand_for_p1(&mut s);
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
    remanded_creature_recast_has_no_counters(r, bears);
}

/// A creature spell that is countered (stack to graveyard) never enters, so its installed rider
/// is inert, and a later creature spell gets no counter (CR 400.7 + CR 614.1c).
#[test]
fn kumano_counter_rider_does_not_survive_a_countered_creature() {
    let (mut s, saga) = saga_board("Kumano Faces Kakkazan");
    let bears = card(&mut s, "Grizzly Bears", Zone::Hand);
    let later = card(&mut s, "Grizzly Bears", Zone::Hand);
    let counterspell = s.add_real_card(P1, "Counterspell", Zone::Hand, db());
    pad_libraries(&mut s);
    let mut r = s.build();
    run_chapter(&mut r, saga, 2, vec![]);
    fund(&mut r, P0, ManaType::Green, 4);
    fund(&mut r, P1, ManaType::Blue, 2);
    cast_action(&mut r, bears);
    drive(&mut r, |s| entry_rider_installed(s, bears), vec![]);
    assert_eq!(zone(&r, bears), Zone::Stack);
    r.act(GameAction::PassPriority).expect("P0 passes");
    cast_action(&mut r, counterspell);
    play(&mut r, vec![]);
    assert_eq!(
        zone(&r, bears),
        Zone::Graveyard,
        "the creature spell was countered"
    );
    cast(&mut r, later, vec![]);
    assert_eq!(zone(&r, later), Zone::Battlefield);
    assert_eq!(
        counters(&r, later, CounterType::Plus1Plus1),
        0,
        "the countered spell's rider belongs to the departed occurrence"
    );
    assert_idle(&r);
}

/// Remand answers the creature spell while the delayed trigger is still on the stack: the trigger
/// resolves with its spell already gone, registers nothing, and the recast gets no counter
/// (CR 603.7c, CR 400.7).
#[test]
fn kumano_counter_rider_is_not_registered_for_a_spell_remanded_in_response() {
    let (mut s, saga) = saga_board("Kumano Faces Kakkazan");
    let bears = card(&mut s, "Grizzly Bears", Zone::Hand);
    remand_for_p1(&mut s);
    let mut r = s.build();
    run_chapter(&mut r, saga, 2, vec![]);
    let remand = r
        .state()
        .objects
        .iter()
        .find_map(|(id, o)| (o.name == "Remand" && o.owner == P1).then_some(*id))
        .expect("P1 holds Remand");
    fund(&mut r, P0, ManaType::Green, 4);
    fund(&mut r, P1, ManaType::Blue, 2);
    cast_action(&mut r, bears);
    assert_eq!(
        r.state().stack.len(),
        2,
        "the Bears and the delayed trigger above it"
    );
    assert!(
        entry_riders(r.state(), bears).is_empty(),
        "the rider is not installed before the delayed trigger resolves"
    );
    r.act(GameAction::PassPriority).expect("P0 passes");
    assert!(matches!(r.state().waiting_for, WaitingFor::Priority { player } if player == P1));
    cast_action(&mut r, remand);
    assert_eq!(
        r.state().stack.len(),
        3,
        "the Bears, the trigger, and Remand"
    );
    play(&mut r, vec![]);
    assert_eq!(zone(&r, bears), Zone::Hand);
    assert!(
        entry_riders(r.state(), bears).is_empty(),
        "the trigger resolved with its spell gone and installed no rider"
    );
    cast(&mut r, bears, vec![]);
    assert_eq!(zone(&r, bears), Zone::Battlefield);
    assert_eq!(counters(&r, bears, CounterType::Plus1Plus1), 0);
    assert_idle(&r);
}

/// Two creature spells are on the stack at once and Kumano II's rider is installed for the first.
/// P1 Remands only the second: the survivor keeps its pinned rider and enters with its counter
/// (CR 400.7 + CR 614.1c). The Remanded spell is a new object when recast, so it gets none.
#[test]
fn remanding_one_spell_keeps_the_other_spells_counter_rider() {
    let (mut s, saga) = saga_board("Kumano Faces Kakkazan");
    let bears = card(&mut s, "Grizzly Bears", Zone::Hand);
    let viper = card(&mut s, "Ambush Viper", Zone::Hand);
    remand_for_p1(&mut s);
    let mut r = s.build();
    let remand = r
        .state()
        .objects
        .iter()
        .find_map(|(id, object)| (object.name == "Remand" && object.owner == P1).then_some(*id))
        .expect("P1 holds Remand");
    run_chapter(&mut r, saga, 2, vec![]);
    assert_eq!(delayed(&r), 1, "the chapter's next-cast trigger waits");
    fund(&mut r, P0, ManaType::Green, 6);
    fund(&mut r, P1, ManaType::Blue, 2);

    // The Bears resolve Kumano's delayed trigger, so their rider is installed while they wait on
    // the stack.
    cast_action(&mut r, bears);
    drive(&mut r, |s| entry_rider_installed(s, bears), vec![]);
    assert_eq!(zone(&r, bears), Zone::Stack);
    assert_eq!(delayed(&r), 0, "the next-cast trigger is spent");

    // Ambush Viper (flash) joins it. The next-cast trigger is already spent, so the flash spell
    // carries no rider of its own.
    cast_action(&mut r, viper);
    assert_eq!(
        r.state().stack.len(),
        2,
        "both creature spells are on the stack"
    );
    assert_eq!(zone(&r, bears), Zone::Stack);
    assert_eq!(zone(&r, viper), Zone::Stack);
    assert!(
        entry_rider_installed(r.state(), bears),
        "the first spell keeps its Kumano rider"
    );
    assert!(
        entry_riders(r.state(), viper).is_empty(),
        "the flash spell has no rider"
    );

    // Remand the flash spell; the Bears' installed rider must survive the other spell's exit.
    r.act(GameAction::PassPriority).expect("P0 passes");
    assert!(matches!(r.state().waiting_for, WaitingFor::Priority { player } if player == P1));
    cast_action(&mut r, remand);
    drive(
        &mut r,
        |s| s.stack.len() == 1 && s.stack[0].id == bears,
        vec![Ask::Target {
            actor: P1,
            pick: TargetRef::Object(viper),
        }],
    );
    assert_eq!(
        zone(&r, viper),
        Zone::Hand,
        "Remand returned the flash spell to its owner's hand"
    );
    assert_eq!(
        zone(&r, bears),
        Zone::Stack,
        "the other creature spell is still on the stack"
    );
    assert!(
        entry_rider_installed(r.state(), bears),
        "the survivor's rider was not cleared with the Remanded spell"
    );

    play(&mut r, vec![]);
    assert_eq!(zone(&r, bears), Zone::Battlefield);
    assert_eq!(
        counters(&r, bears, CounterType::Plus1Plus1),
        1,
        "the surviving spell entered with its rider's counter"
    );
    assert_entry_rider_consumed(&r, bears);

    // The Remanded spell is a new object when recast (CR 400.7), so it enters with no counter.
    assert_eq!(zone(&r, viper), Zone::Hand);
    cast(&mut r, viper, vec![]);
    assert_eq!(zone(&r, viper), Zone::Battlefield);
    assert_eq!(counters(&r, viper, CounterType::Plus1Plus1), 0);
    assert_idle(&r);
}

/// CR 400.7 + CR 603.7c + CR 707.10 + CR 614.1c: Kumano II's delayed trigger is spent when P0
/// casts Ambush Viper, and P1 Remands the Viper. P0 recasts the Viper and copies the still-waiting
/// delayed trigger with Strionic Resonator. The copy resolves while the recast Viper is on the
/// stack, but its trigger event names the FIRST Viper incarnation, so it installs no entry rider
/// for the recast. The original trigger waits BELOW the recast, so it resolves only after that
/// spell has entered, and installs nothing either.
#[test]
fn strionic_resonator_copy_of_kumano_rider_does_not_bind_a_remanded_spells_recast() {
    let (mut s, saga) = saga_board("Kumano Faces Kakkazan");
    let resonator = card(&mut s, "Strionic Resonator", Zone::Battlefield);
    let viper = card(&mut s, "Ambush Viper", Zone::Hand);
    remand_for_p1(&mut s);
    let mut r = s.build();
    let remand = r
        .state()
        .objects
        .iter()
        .find_map(|(id, object)| (object.name == "Remand" && object.owner == P1).then_some(*id))
        .expect("P1 holds Remand");
    run_chapter(&mut r, saga, 2, vec![]);
    assert_eq!(delayed(&r), 1, "the chapter's next-cast trigger waits");
    fund(&mut r, P0, ManaType::Green, 6);
    fund(&mut r, P1, ManaType::Blue, 2);

    cast_action(&mut r, viper);
    let t1 = {
        let triggers = triggered_on_stack(r.state());
        assert_eq!(
            triggers.len(),
            1,
            "the chapter's next-cast trigger is the only one"
        );
        triggers[0]
    };
    assert_eq!(zone(&r, viper), Zone::Stack);

    // P0 passes; P1 Remands the Viper while T1 still waits below Remand.
    r.act(GameAction::PassPriority).expect("P0 passes");
    assert!(matches!(r.state().waiting_for, WaitingFor::Priority { player } if player == P1));
    cast_action(&mut r, remand);
    // Remand's only legal target is the Viper spell (T1 is an ability), so the engine announces it
    // itself; stop once the Viper is back in hand with T1 still waiting.
    drive(&mut r, |s| s.objects[&viper].zone == Zone::Hand, vec![]);
    assert_eq!(zone(&r, viper), Zone::Hand);
    assert_eq!(
        triggered_on_stack(r.state()),
        vec![t1],
        "the delayed trigger still waits after its spell was Remanded"
    );
    assert_eq!(
        delayed(&r),
        0,
        "the chapter's next-cast trigger left the delayed list when it fired"
    );

    // Recast the Viper (flash) while T1 waits; the spent next-cast trigger does not fire again.
    cast_action(&mut r, viper);
    assert_eq!(zone(&r, viper), Zone::Stack);
    assert_eq!(r.state().stack.len(), 2, "T1 waits below the recast Viper");
    assert_eq!(triggered_on_stack(r.state()), vec![t1]);

    // P0 copies T1 with Strionic Resonator (CR 707.10).
    r.act(GameAction::ActivateAbility {
        source_id: resonator,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("Strionic Resonator's copy ability is activated");
    // The sole legal copy target (T1) is auto-announced, so the activation raises no
    // TargetSelection prompt; only the copy's creation needs driving.
    drive(&mut r, |s| triggered_on_stack(s).len() == 2, vec![]);
    let copy = triggered_on_stack(r.state())
        .into_iter()
        .find(|id| *id != t1)
        .expect("the Resonator copy is a new stack object");

    // Resolve the copy while the recast Viper is still on the stack.
    drive(&mut r, |s| !triggered_on_stack(s).contains(&copy), vec![]);
    assert_eq!(
        zone(&r, viper),
        Zone::Stack,
        "reach-guard: the recast Viper is on the stack when the copy resolves"
    );
    assert!(
        entry_riders(r.state(), viper).is_empty(),
        "CR 400.7: the copy names the first Viper incarnation, so it installs no rider for the recast"
    );

    // The original T1 waits below the recast Viper, so draining the stack resolves the Viper
    // first and T1 afterwards, with its spell already gone.
    play(&mut r, vec![]);
    assert_eq!(zone(&r, viper), Zone::Battlefield);
    assert_eq!(
        counters(&r, viper, CounterType::Plus1Plus1),
        0,
        "the recast is a new object with no rider"
    );
    assert!(
        entry_riders(r.state(), viper).is_empty(),
        "T1 resolved after its spell left, so it installed nothing"
    );
    assert_idle(&r);
}

/// Positive control for the witness above: the same Resonator copy resolving while the ORIGINAL
/// Viper is still on the stack installs its entry rider for that live spell (CR 400.7 + CR 707.10).
/// The witness stops at that install; the Viper's entry would order two identical riders.
#[test]
fn strionic_resonator_copy_of_kumano_rider_installs_for_a_spell_still_on_the_stack() {
    let (mut s, saga) = saga_board("Kumano Faces Kakkazan");
    let resonator = card(&mut s, "Strionic Resonator", Zone::Battlefield);
    let viper = card(&mut s, "Ambush Viper", Zone::Hand);
    let mut r = s.build();
    run_chapter(&mut r, saga, 2, vec![]);
    assert_eq!(delayed(&r), 1, "the chapter's next-cast trigger waits");
    fund(&mut r, P0, ManaType::Green, 4);

    cast_action(&mut r, viper);
    let t1 = {
        let triggers = triggered_on_stack(r.state());
        assert_eq!(triggers.len(), 1);
        triggers[0]
    };
    assert_eq!(zone(&r, viper), Zone::Stack);

    r.act(GameAction::ActivateAbility {
        source_id: resonator,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("Strionic Resonator's copy ability is activated");
    // The sole legal copy target (T1) is auto-announced, so the activation raises no
    // TargetSelection prompt; only the copy's creation needs driving.
    drive(&mut r, |s| triggered_on_stack(s).len() == 2, vec![]);
    let copy = triggered_on_stack(r.state())
        .into_iter()
        .find(|id| *id != t1)
        .expect("the Resonator copy is a new stack object");

    drive(&mut r, |s| !triggered_on_stack(s).contains(&copy), vec![]);
    assert_eq!(zone(&r, viper), Zone::Stack);
    assert_eq!(
        entry_riders(r.state(), viper).len(),
        1,
        "the copy installed its rider for the live spell"
    );

    // Stop here: the copy resolved while the original Viper is still live on the stack and
    // installed exactly one pinned rider for it. Resolving the original trigger and then the
    // Viper is the negative witness's job above.
}

/// A creature spell that merges (mutate) never enters the battlefield, so its pinned rider never
/// applies: after the merged permanent is bounced, the recast Gemrazer is a new object with no
/// counter (CR 730.2b, CR 400.7).
#[test]
fn kumano_counter_rider_is_spent_by_a_mutate_merge() {
    let (mut s, saga) = saga_board("Kumano Faces Kakkazan");
    let host = card(&mut s, "Grizzly Bears", Zone::Battlefield);
    let gemrazer = card(&mut s, "Gemrazer", Zone::Hand);
    let unsummon = card(&mut s, "Unsummon", Zone::Hand);
    pad_libraries(&mut s);
    let mut r = s.build();
    engine::game::rehydrate_game_from_card_db(r.state_mut(), db());
    run_chapter(&mut r, saga, 2, vec![]);
    fund(&mut r, P0, ManaType::Green, 3);
    let offered = engine::ai_support::legal_actions(r.state())
        .into_iter()
        .find(|a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == gemrazer))
        .expect("the mutate cast is offered");
    r.act(offered).expect("the mutate cast is accepted");
    // The host is the only legal mutate target, so the engine announces it itself.
    assert_eq!(
        r.state().stack.iter().filter(|e| e.id == gemrazer).count(),
        1
    );
    r.advance_until_stack_empty();
    assert!(
        matches!(r.state().waiting_for, WaitingFor::MutateMergeChoice { player, .. } if player == P0)
    );
    assert!(
        entry_rider_installed(r.state(), gemrazer),
        "the rider was installed for the mutating spell"
    );
    r.act(GameAction::ChooseMutateMergeSide {
        side: engine::game::merge::MergeSide::Top,
    })
    .expect("the merge placement is accepted");
    r.advance_until_stack_empty();
    assert_eq!(
        r.state().objects[&host].merged_components.len(),
        2,
        "host and Gemrazer are one merged permanent"
    );
    assert!(
        !entry_riders(r.state(), gemrazer)[0].is_consumed,
        "CR 730.2b: the merged spell never takes a Stack->Battlefield move, so its rider never applies"
    );
    fund(&mut r, P0, ManaType::Blue, 1);
    // The merged permanent is the only creature, so Unsummon's target is announced by the engine.
    cast(&mut r, unsummon, vec![]);
    assert_eq!(zone(&r, gemrazer), Zone::Hand);
    fund(&mut r, P0, ManaType::Green, 6);
    cast(&mut r, gemrazer, vec![]);
    assert_eq!(zone(&r, gemrazer), Zone::Battlefield);
    assert_eq!(counters(&r, gemrazer, CounterType::Plus1Plus1), 0);
    assert_idle(&r);
}

/// A merge spends ONLY the merging spell's own rider: another creature spell waiting below it on
/// the stack keeps its pinned rider and enters with its counter (CR 730.2b, CR 400.7, CR 614.1c).
///
/// The lower spell (Kumano II's one-counter rider) is cast first; Vedalken Orrery's "as though
/// they had flash" then lets Gemrazer's mutate cast be announced in response, where Yuna's
/// activated ability arms a two-counter rider for it. Gemrazer is above the lower spell, so the
/// merge resolves first: its rider never applies, and the lower spell's rider must stay. A witness
/// with one rider cannot tell retention from a plain clear, which is why both are installed here.
#[test]
fn mutate_merge_spends_only_the_merging_spells_counter_rider() {
    let (mut s, saga) = saga_board("Kumano Faces Kakkazan");
    let host = card(&mut s, "Grizzly Bears", Zone::Battlefield);
    let yuna = card(&mut s, "Yuna, Grand Summoner", Zone::Battlefield);
    let orrery = card(&mut s, "Vedalken Orrery", Zone::Battlefield);
    let bears = card(&mut s, "Grizzly Bears", Zone::Hand);
    let gemrazer = card(&mut s, "Gemrazer", Zone::Hand);
    let mut r = s.build();
    engine::game::rehydrate_game_from_card_db(r.state_mut(), db());
    run_chapter(&mut r, saga, 2, vec![]);
    assert_eq!(delayed(&r), 1, "the chapter's next-cast trigger waits");
    assert_eq!(
        zone(&r, orrery),
        Zone::Battlefield,
        "reach-guard: the flash permission source is on the battlefield"
    );

    // Plain green pays for the lower spell, so only the Kumano rider is installed for it, and it
    // waits on the stack (its spell, not the battlefield host, is the eventual entrant).
    fund(&mut r, P0, ManaType::Green, 1);
    fund(&mut r, P0, ManaType::Colorless, 1);
    cast_action(&mut r, bears);
    drive(&mut r, |state| entry_rider_installed(state, bears), vec![]);
    assert_eq!(zone(&r, bears), Zone::Stack);
    assert_eq!(delayed(&r), 0, "the next-cast trigger is spent");

    // Yuna's mana ability arms a second next-cast rider for the spell cast in response. It is a
    // mana ability, so it resolves without draining the lower spell off the stack.
    r.act(GameAction::ActivateAbility {
        source_id: yuna,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("Yuna's mana ability is activated");
    assert!(
        matches!(r.state().waiting_for, WaitingFor::ChooseManaColor { .. }),
        "any-color mana ability prompts, got {:?}",
        r.state().waiting_for
    );
    r.act(GameAction::ChooseManaColor {
        choice: ManaChoice::SingleColor(ManaType::Green),
        count: 1,
    })
    .expect("Yuna adds one green mana");
    assert_eq!(delayed(&r), 1, "Yuna's next-cast trigger waits");
    assert!(
        entry_rider_installed(r.state(), bears),
        "arming the second rider left the lower spell's rider alone"
    );

    // Orrery supplies flash, so the mutate cast is offered in response; the pool holds three mana,
    // so the printed {3}{G} is unaffordable and the unannounced cast is the mutate route
    // ({1}{G}{G}) — the same sole-affordable-route shape as the witness above.
    fund(&mut r, P0, ManaType::Green, 2);
    let offered = engine::ai_support::legal_actions(r.state())
        .into_iter()
        .find(|a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == gemrazer))
        .expect("the mutate cast is offered in response");
    r.act(offered).expect("the mutate cast is accepted");
    drive(
        &mut r,
        |state| state.stack.len() == 2 && entry_rider_installed(state, gemrazer),
        vec![],
    );
    assert_eq!(zone(&r, bears), Zone::Stack, "the lower spell still waits");
    assert_eq!(zone(&r, gemrazer), Zone::Stack);
    assert_eq!(
        r.state().stack.iter().filter(|e| e.id == gemrazer).count(),
        1
    );
    // Reach-guard: the one-counter Kumano rider belongs to the lower spell, the two-counter Yuna
    // rider to the merging spell; both are installed before anything resolves.
    assert!(entry_rider_installed(r.state(), bears));
    assert!(entry_rider_installed(r.state(), gemrazer));

    // Gemrazer is above the lower spell, so the merge resolves first (the host is the only legal
    // mutate target: Yuna is a Human, so the engine announces it itself).
    r.advance_until_stack_empty();
    assert!(
        matches!(r.state().waiting_for, WaitingFor::MutateMergeChoice { player, .. } if player == P0)
    );
    r.act(GameAction::ChooseMutateMergeSide {
        side: engine::game::merge::MergeSide::Top,
    })
    .expect("the merge placement is accepted");
    assert_eq!(
        r.state().objects[&host].merged_components.len(),
        2,
        "host and Gemrazer are one merged permanent"
    );
    assert!(
        !entry_riders(r.state(), gemrazer)[0].is_consumed,
        "CR 730.2b: the merging spell never enters, so nothing consumes its rider"
    );
    assert!(
        entry_rider_installed(r.state(), bears),
        "the lower spell's rider survives the merge"
    );

    // The lower creature spell then resolves and enters with exactly its own rider's counter.
    play(&mut r, vec![]);
    assert_eq!(zone(&r, bears), Zone::Battlefield);
    assert_eq!(counters(&r, bears, CounterType::Plus1Plus1), 1);
    assert_entry_rider_consumed(&r, bears);
    assert_idle(&r);
}

/// The rider is scoped to the next CREATURE spell: a noncreature spell cast while it waits
/// registers no rider and leaves the trigger armed for the next creature spell.
#[test]
fn noncreature_spell_does_not_consume_the_counter_rider() {
    let (mut s, saga) = saga_board("Kumano Faces Kakkazan");
    let lightning_bolt = card(&mut s, BOLT, Zone::Hand);
    let bears = card(&mut s, "Grizzly Bears", Zone::Hand);
    let mut r = s.build();
    run_chapter(&mut r, saga, 2, vec![]);
    assert_eq!(delayed(&r), 1, "the chapter's next-cast trigger waits");
    fund(&mut r, P0, ManaType::Red, 1);
    let life = r.life(P1);
    bolt(&mut r, lightning_bolt, 0);
    assert_eq!(r.life(P1), life - 3);
    assert!(
        entry_riders(r.state(), lightning_bolt).is_empty(),
        "a noncreature spell installs no rider"
    );
    assert_eq!(
        delayed(&r),
        1,
        "the trigger still waits for a creature spell"
    );
    fund(&mut r, P0, ManaType::Green, 2);
    cast(&mut r, bears, vec![]);
    assert_eq!(counters(&r, bears, CounterType::Plus1Plus1), 1);
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
            policy: OptionalPolicy::Accept,
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
            policy: OptionalPolicy::Accept,
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
