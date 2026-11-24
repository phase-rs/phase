//! Phytotitan's delayed return at its owner's next upkeep, bound to every
//! object its death put in the graveyard; and the same creation-referent
//! binding on the delayed `ParentTarget` arm for a non-source dying creature.
//!
//! Oracle text (MTGJSON 5.3.0+20261021, verbatim):
//!   * Phytotitan — "When this creature dies, return it to the battlefield tapped
//!     under its owner's control at the beginning of their next upkeep."
//!     Ruling (2014-07-18): "Phytotitan's ability will return it to the
//!     battlefield only if it's still in the graveyard when the delayed
//!     triggered ability resolves. If it's not, it won't return to the
//!     battlefield."
//!   * Avacyn, Angel of Horror — "Flying, deathtouch\nWhenever Avacyn or another
//!     nontoken creature you control dies, return that card to the battlefield
//!     under your control at the beginning of the next end step."
//!   * Gemrazer — "Mutate {1}{G}{G} … Reach, trample\nWhenever this creature
//!     mutates, destroy target artifact or enchantment an opponent controls."
//!     (fixture card)
//!   * Shock — "Shock deals 2 damage to any target."
//!   * Act of Treason — "Gain control of target creature until end of turn.
//!     Untap that creature. It gains haste until end of turn. (It can attack and
//!     {T} this turn.)"
//!   * Cremate — "Exile target card from a graveyard.\nDraw a card."
//!   * Noxious Revival — "({G/P} can be paid with either {G} or 2 life.)\nPut
//!     target card from a graveyard on top of its owner's library."
//!   * Thought Scour — "Target player mills two cards.\nDraw a card."
//!   * Archelos, Lagoon Mystic — "As long as Archelos is tapped, other permanents
//!     enter tapped.\nAs long as Archelos is untapped, other permanents enter
//!     untapped."
//!   * Imposing Sovereign — "Creatures your opponents control enter tapped."
//!
//! Rules:
//!   * CR 108.3: the owner of a card is the player who started the game with it.
//!   * CR 110.2a: an object put onto the battlefield enters under the control of
//!     the player putting it there unless the effect states otherwise.
//!   * CR 400.7 / CR 400.7e: an object that moves to another zone becomes a new
//!     object; a zone-change trigger can find the new object it became.
//!   * CR 603.3a: a triggered ability is controlled by its source's controller
//!     when it triggered.
//!   * CR 603.7a / CR 603.7b / CR 603.7e: a delayed triggered ability is created
//!     at resolution, triggers once, and is controlled by its creator.
//!   * CR 603.7c: a delayed ability does not affect an object that left the zone
//!     it was expected in, even if it returned (a new object, CR 400.7).
//!   * CR 609.3: an effect does as much as possible.
//!   * CR 702.140c / CR 702.140e: a mutating creature spell merges with its
//!     target; the merged permanent has every component's abilities.
//!   * CR 730.3 / CR 730.3c: a merged permanent that leaves becomes several
//!     objects; an effect that finds the new object finds all of them.

use engine::game::game_object::ObjectIncarnationRefExt;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{DelayedTriggerCondition, TargetRef};
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::events::GameEvent;
use engine::types::game_state::{CastingVariant, DelayedTrigger, WaitingFor};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const PHYTOTITAN: &str = "When this creature dies, return it to the battlefield tapped under its owner's control at the beginning of their next upkeep.";
const AVACYN_ANGEL_OF_HORROR: &str = "Flying, deathtouch\nWhenever Avacyn or another nontoken creature you control dies, return that card to the battlefield under your control at the beginning of the next end step.";
const SHOCK: &str = "Shock deals 2 damage to any target.";
const ACT_OF_TREASON: &str = "Gain control of target creature until end of turn. Untap that creature. It gains haste until end of turn. (It can attack and {T} this turn.)";
const CREMATE: &str = "Exile target card from a graveyard.\nDraw a card.";
const NOXIOUS_REVIVAL: &str = "({G/P} can be paid with either {G} or 2 life.)\nPut target card from a graveyard on top of its owner's library.";
const THOUGHT_SCOUR: &str = "Target player mills two cards.\nDraw a card.";
const ARCHELOS_LAGOON_MYSTIC: &str = "As long as Archelos is tapped, other permanents enter tapped.\nAs long as Archelos is untapped, other permanents enter untapped.";
const IMPOSING_SOVEREIGN: &str = "Creatures your opponents control enter tapped.";

// ------------------------------------------------------------------ fixture

/// Every instant/sorcery a row may cast, one copy per caster.
struct Spells {
    p0_shock: ObjectId,
    p0_revival: ObjectId,
    p0_scour: ObjectId,
    p1_treason: ObjectId,
    p1_shock: ObjectId,
    p1_cremate: ObjectId,
    p1_revival: ObjectId,
    p1_scour: ObjectId,
}

fn cost(shard: ManaCostShard, generic: u32) -> ManaCost {
    ManaCost::Cost {
        shards: vec![shard],
        generic,
    }
}

fn add_spell(
    scenario: &mut GameScenario,
    owner: PlayerId,
    name: &str,
    instant: bool,
    text: &str,
    mana_cost: ManaCost,
) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(owner, name, instant, text)
        .with_mana_cost(mana_cost)
        .id()
}

/// An instant with its verbatim Oracle text and no mana cost. Used for Noxious
/// Revival: its {G/P} shard parks the cast on `WaitingFor::PhyrexianPayment`
/// (CR 107.4f), which the `SpellCast` driver does not answer, and the cost is
/// immaterial to every row here.
fn add_free_spell(
    scenario: &mut GameScenario,
    owner: PlayerId,
    name: &str,
    text: &str,
) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(owner, name, true, text)
        .id()
}

/// A board at `phase` of P0's turn with P0's Phytotitan, basic lands for every
/// spell, padded libraries (no one decks out, CR 704.5b) and the spell set.
/// `setup` adds the row's extra permanents before the board is built.
fn board<T>(
    phase: Phase,
    setup: impl FnOnce(&mut GameScenario) -> T,
) -> (GameRunner, ObjectId, Spells, T) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(phase);
    let phytotitan = scenario
        .add_creature_from_oracle(P0, "Phytotitan", 7, 2, PHYTOTITAN)
        .with_subtypes(vec!["Plant", "Elemental"])
        .id();
    for color in [
        ManaColor::Red,
        ManaColor::Red,
        ManaColor::Green,
        ManaColor::Green,
        ManaColor::Blue,
        ManaColor::Blue,
        ManaColor::Black,
    ] {
        scenario.add_basic_land(P0, color);
    }
    for color in [
        ManaColor::Red,
        ManaColor::Red,
        ManaColor::Red,
        ManaColor::Red,
        ManaColor::Green,
        ManaColor::Blue,
        ManaColor::Black,
    ] {
        scenario.add_basic_land(P1, color);
    }
    for i in 0..30 {
        scenario.add_card_to_library_top(P0, &format!("P0 Filler {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Filler {i}"));
    }
    let spells = Spells {
        p0_shock: add_spell(
            &mut scenario,
            P0,
            "Shock",
            true,
            SHOCK,
            cost(ManaCostShard::Red, 0),
        ),
        p0_revival: add_free_spell(&mut scenario, P0, "Noxious Revival", NOXIOUS_REVIVAL),
        p0_scour: add_spell(
            &mut scenario,
            P0,
            "Thought Scour",
            true,
            THOUGHT_SCOUR,
            cost(ManaCostShard::Blue, 0),
        ),
        p1_treason: add_spell(
            &mut scenario,
            P1,
            "Act of Treason",
            false,
            ACT_OF_TREASON,
            cost(ManaCostShard::Red, 2),
        ),
        p1_shock: add_spell(
            &mut scenario,
            P1,
            "Shock",
            true,
            SHOCK,
            cost(ManaCostShard::Red, 0),
        ),
        p1_cremate: add_spell(
            &mut scenario,
            P1,
            "Cremate",
            true,
            CREMATE,
            cost(ManaCostShard::Black, 0),
        ),
        p1_revival: add_free_spell(&mut scenario, P1, "Noxious Revival", NOXIOUS_REVIVAL),
        p1_scour: add_spell(
            &mut scenario,
            P1,
            "Thought Scour",
            true,
            THOUGHT_SCOUR,
            cost(ManaCostShard::Blue, 0),
        ),
    };
    let extra = setup(&mut scenario);
    (scenario.build(), phytotitan, spells, extra)
}

/// The merged fixture: Gemrazer (fixture card) cast for mutate under P0's
/// Phytotitan, so Phytotitan is on top (CR 702.140c/e) and the merged
/// permanent is a 7/2 with both cards' abilities. `extra` adds the row's
/// other permanents. Returns `(runner, phytotitan, gemrazer, spells)`.
fn merged_board(extra: impl FnOnce(&mut GameScenario)) -> (GameRunner, ObjectId, ObjectId, Spells) {
    let db = crate::support::shared_card_db().expect("integration card fixture must load");
    let (mut runner, phytotitan, spells, gemrazer) = board(Phase::PreCombatMain, |scenario| {
        let gemrazer = scenario.add_real_card(P0, "Gemrazer", Zone::Hand, db);
        // Gemrazer's mutate cost is {1}{G}{G}.
        scenario.with_mana_pool(
            P0,
            (0..3)
                .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
                .collect(),
        );
        extra(scenario);
        gemrazer
    });
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    assert!(
        !runner.state().objects[&phytotitan]
            .trigger_definitions
            .is_empty(),
        "reach-guard: the Oracle-built Phytotitan keeps its dies trigger through the rehydrate"
    );

    runner
        .cast(gemrazer)
        .casting_variant(CastingVariant::Mutate)
        .alternative_cast(AlternativeCastDecision::Alternative)
        .target_object(phytotitan)
        .commit();
    runner.advance_until_stack_empty();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::MutateMergeChoice { player, .. } if player == P0
        ),
        "the merge placement is P0's choice; got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseMutateMergeSide {
            side: engine::game::merge::MergeSide::Bottom,
        })
        .expect("Gemrazer goes under Phytotitan");
    // Gemrazer's mutate trigger has no legal target (P1 controls no artifact or
    // enchantment), so nothing waits on the stack.
    runner.advance_until_stack_empty();
    let merged = runner.state().objects[&phytotitan]
        .merged_components
        .clone();
    assert_eq!(merged.len(), 2, "reach-guard: one merged permanent");
    assert!(merged.contains(&gemrazer), "Gemrazer is a component");
    assert_eq!(runner.state().objects[&phytotitan].power, Some(7));
    assert_eq!(runner.state().objects[&phytotitan].toughness, Some(2));
    (runner, phytotitan, gemrazer, spells)
}

// ------------------------------------------------------------------ drivers

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn incarnation(runner: &GameRunner, id: ObjectId) -> ObjectIncarnationRef {
    ObjectIncarnationRef::from_object(&runner.state().objects[&id])
}

fn only_delayed(runner: &GameRunner) -> &DelayedTrigger {
    let delayed = &runner.state().delayed_triggers;
    assert_eq!(delayed.len(), 1, "exactly one delayed trigger: {delayed:?}");
    &delayed[0]
}

/// Pass priority and answer the turn-structure prompts through public actions
/// until `done`, returning every event emitted on the way.
fn drive_until(
    runner: &mut GameRunner,
    what: &str,
    mut done: impl FnMut(&GameRunner) -> bool,
) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..400 {
        if done(runner) {
            return events;
        }
        let action = match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            WaitingFor::DiscardToHandSize { count, cards, .. } => GameAction::SelectCards {
                cards: cards.iter().take(*count).copied().collect(),
            },
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("{what}: unexpected prompt {other:?}"),
        };
        match runner.act(action) {
            Ok(result) => events.extend(result.events),
            Err(error) => panic!("{what}: {error:?} at {:?}", runner.state().waiting_for),
        }
    }
    panic!(
        "{what}: never reached (turn {}, phase {:?})",
        runner.state().turn_number,
        runner.state().phase
    );
}

/// Drive to `player`'s main phase of a later turn.
fn to_main_of(runner: &mut GameRunner, player: PlayerId) {
    let start = runner.state().turn_number;
    drive_until(runner, "main phase", |r| {
        r.state().active_player == player
            && r.state().turn_number > start
            && r.state().phase == Phase::PreCombatMain
            && r.state().stack.is_empty()
    });
}

/// Drive through `player`'s next upkeep (a later turn) and resolve whatever it
/// put on the stack. Returns every event emitted on the way.
fn through_next_upkeep_of(runner: &mut GameRunner, player: PlayerId) -> Vec<GameEvent> {
    let start = runner.state().turn_number;
    let events = drive_until(runner, "next upkeep", |r| {
        r.state().active_player == player
            && r.state().turn_number > start
            && !matches!(r.state().phase, Phase::Untap | Phase::Upkeep)
            && r.state().stack.is_empty()
    });
    runner.advance_until_stack_empty();
    events
}

/// Drive through this turn's end step and resolve what it put on the stack.
fn through_end_step(runner: &mut GameRunner) {
    let start = runner.state().turn_number;
    drive_until(runner, "end step", |r| {
        (r.state().phase == Phase::End && r.state().stack.is_empty())
            || r.state().turn_number > start
    });
    // The end step's delayed trigger is put on the stack as the step begins.
    runner.advance_until_stack_empty();
}

/// Cast `spell` through the public pipeline and resolve only that spell,
/// leaving anything beneath it (a dies trigger) on the stack.
fn cast_and_resolve_only(runner: &mut GameRunner, spell: ObjectId, target: TargetRef) {
    let cast = runner.cast(spell);
    let cast = match target {
        TargetRef::Object(id) => cast.target_object(id),
        TargetRef::Player(player) => cast.target_player(player),
    };
    let _ = cast.commit();
    for _ in 0..8 {
        if !runner.state().stack.iter().any(|entry| entry.id == spell) {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to resolve the spell");
    }
    panic!("{spell:?} never resolved");
}

/// Cast `spell` and resolve the whole stack, triggers included.
fn cast_and_resolve_all(runner: &mut GameRunner, spell: ObjectId, target: TargetRef) {
    cast_and_resolve_only(runner, spell, target);
    runner.advance_until_stack_empty();
}

/// Noxious Revival puts `card` on top of its owner's library, then Thought
/// Scour mills it back: the card is a new object in the graveyard (CR 400.7).
fn round_trip_through_library(
    runner: &mut GameRunner,
    card: ObjectId,
    revival: ObjectId,
    scour: ObjectId,
    leave_rest_on_stack: bool,
) {
    let before = incarnation(runner, card);
    let owner = runner.state().objects[&card].owner;
    cast_and_resolve_only(runner, revival, TargetRef::Object(card));
    assert_eq!(
        zone(runner, card),
        Zone::Library,
        "Noxious Revival resolved"
    );
    cast_and_resolve_only(runner, scour, TargetRef::Player(owner));
    if !leave_rest_on_stack {
        runner.advance_until_stack_empty();
    }
    assert_eq!(zone(runner, card), Zone::Graveyard, "milled back");
    assert!(
        incarnation(runner, card).incarnation > before.incarnation,
        "the milled card is a new object (CR 400.7)"
    );
}

fn assert_returned_tapped_under(runner: &GameRunner, id: ObjectId, controller: PlayerId) {
    let object = &runner.state().objects[&id];
    assert_eq!(object.zone, Zone::Battlefield, "{} returned", object.name);
    assert!(object.tapped, "{} returned tapped", object.name);
    assert_eq!(object.controller, controller, "{} controller", object.name);
}

// --------------------------------------------------------------- Phytotitan

/// V4 — positive control: dies on its owner's turn, returns at the owner's next
/// upkeep, tapped, under its owner's control.
#[test]
fn phytotitan_dies_on_owners_turn_returns_at_owners_next_upkeep() {
    let (mut runner, phytotitan, spells, ()) = board(Phase::PreCombatMain, |_| ());
    cast_and_resolve_all(&mut runner, spells.p0_shock, TargetRef::Object(phytotitan));

    assert_eq!(zone(&runner, phytotitan), Zone::Graveyard);
    let delayed = only_delayed(&runner);
    assert!(matches!(
        delayed.condition,
        DelayedTriggerCondition::AtNextPhaseForPlayer {
            phase: Phase::Upkeep,
            player,
            ..
        } if player == P0
    ));
    assert_eq!(delayed.controller, P0);
    assert_eq!(delayed.ability.targets, vec![TargetRef::Object(phytotitan)]);
    assert_eq!(
        delayed.ability.target_incarnations,
        vec![incarnation(&runner, phytotitan)],
        "pinned to the graveyard object the death created"
    );

    through_next_upkeep_of(&mut runner, P0);
    assert_returned_tapped_under(&runner, phytotitan, P0);
    assert!(runner.state().delayed_triggers.is_empty());
}

/// V5 + V13 — stolen: the thief controls the dies trigger and the delayed
/// trigger (CR 603.3a, CR 603.7e), but "their" is the owner (CR 108.3).
#[test]
fn phytotitan_stolen_dies_returns_at_owners_next_upkeep() {
    let (mut runner, phytotitan, spells, ()) = board(Phase::PreCombatMain, |_| ());
    to_main_of(&mut runner, P1);
    cast_and_resolve_all(
        &mut runner,
        spells.p1_treason,
        TargetRef::Object(phytotitan),
    );
    assert_eq!(runner.state().objects[&phytotitan].controller, P1);
    cast_and_resolve_only(&mut runner, spells.p1_shock, TargetRef::Object(phytotitan));
    let dies_trigger = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.source_id == phytotitan)
        .expect("the dies trigger waits on the stack");
    assert_eq!(
        dies_trigger.controller, P1,
        "CR 603.3a: the thief controls it"
    );
    runner.advance_until_stack_empty();

    let delayed = only_delayed(&runner);
    assert_eq!(delayed.controller, P1, "CR 603.7e");
    assert!(
        matches!(
            delayed.condition,
            DelayedTriggerCondition::AtNextPhaseForPlayer { player, .. } if player == P0
        ),
        "\"their\" is the owner: {:?}",
        delayed.condition
    );
    assert_eq!(
        delayed.ability.target_incarnations,
        vec![incarnation(&runner, phytotitan)]
    );

    through_next_upkeep_of(&mut runner, P0);
    assert_returned_tapped_under(&runner, phytotitan, P0);
    assert!(runner.state().delayed_triggers.is_empty());
}

/// V6 — exiled from the graveyard before the upkeep: nothing returns.
#[test]
fn phytotitan_exiled_from_graveyard_does_not_return() {
    let (mut runner, phytotitan, spells, ()) = board(Phase::PreCombatMain, |_| ());
    cast_and_resolve_all(&mut runner, spells.p0_shock, TargetRef::Object(phytotitan));
    assert_eq!(zone(&runner, phytotitan), Zone::Graveyard);
    only_delayed(&runner);

    to_main_of(&mut runner, P1);
    cast_and_resolve_all(
        &mut runner,
        spells.p1_cremate,
        TargetRef::Object(phytotitan),
    );
    assert_eq!(zone(&runner, phytotitan), Zone::Exile);

    through_next_upkeep_of(&mut runner, P0);
    assert!(runner.state().delayed_triggers.is_empty(), "it fired");
    assert_eq!(zone(&runner, phytotitan), Zone::Exile);
}

/// V7 — dying during the owner's upkeep waits for the following upkeep
/// (CR 603.7a: the delayed trigger cannot trigger on an earlier event).
#[test]
fn phytotitan_dying_in_owners_upkeep_waits_for_the_following_upkeep() {
    let (mut runner, phytotitan, spells, ()) = board(Phase::Upkeep, |_| ());
    cast_and_resolve_all(&mut runner, spells.p0_shock, TargetRef::Object(phytotitan));
    assert_eq!(zone(&runner, phytotitan), Zone::Graveyard);
    only_delayed(&runner);

    to_main_of(&mut runner, P1);
    assert_eq!(zone(&runner, phytotitan), Zone::Graveyard);
    only_delayed(&runner);

    through_next_upkeep_of(&mut runner, P0);
    assert_returned_tapped_under(&runner, phytotitan, P0);
    assert!(runner.state().delayed_triggers.is_empty());
}

/// V8 — an opponent's upkeep does not trigger it.
#[test]
fn phytotitan_return_ignores_opponents_upkeep() {
    let (mut runner, phytotitan, spells, ()) = board(Phase::PreCombatMain, |_| ());
    cast_and_resolve_all(&mut runner, spells.p0_shock, TargetRef::Object(phytotitan));

    through_next_upkeep_of(&mut runner, P1);
    assert_eq!(zone(&runner, phytotitan), Zone::Graveyard);
    only_delayed(&runner);

    through_next_upkeep_of(&mut runner, P0);
    assert_returned_tapped_under(&runner, phytotitan, P0);
}

/// V10 — put back into the graveyard after its trigger resolved: the delayed
/// trigger does not return the new object (CR 603.7c, CR 400.7).
#[test]
fn phytotitan_put_back_into_graveyard_after_its_trigger_is_not_returned() {
    let (mut runner, phytotitan, spells, ()) = board(Phase::PreCombatMain, |_| ());
    cast_and_resolve_all(&mut runner, spells.p0_shock, TargetRef::Object(phytotitan));
    let pinned = incarnation(&runner, phytotitan);
    assert_eq!(
        only_delayed(&runner).ability.target_incarnations,
        vec![pinned]
    );

    to_main_of(&mut runner, P1);
    round_trip_through_library(
        &mut runner,
        phytotitan,
        spells.p1_revival,
        spells.p1_scour,
        false,
    );

    through_next_upkeep_of(&mut runner, P0);
    assert!(runner.state().delayed_triggers.is_empty(), "it fired");
    assert_eq!(zone(&runner, phytotitan), Zone::Graveyard);
}

/// V11 — put back into the graveyard before its trigger resolved: "it" can no
/// longer find the object the death created, so nothing returns.
#[test]
fn phytotitan_put_back_into_graveyard_before_its_trigger_resolves_is_not_returned() {
    let (mut runner, phytotitan, spells, ()) = board(Phase::PreCombatMain, |_| ());
    cast_and_resolve_only(&mut runner, spells.p0_shock, TargetRef::Object(phytotitan));
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == phytotitan),
        "reach-guard: the dies trigger waits on the stack"
    );
    round_trip_through_library(
        &mut runner,
        phytotitan,
        spells.p0_revival,
        spells.p0_scour,
        true,
    );
    runner.advance_until_stack_empty();

    let delayed = only_delayed(&runner);
    assert!(
        delayed.ability.targets.is_empty(),
        "the dies trigger found no successor: {:?}",
        delayed.ability.targets
    );

    through_next_upkeep_of(&mut runner, P0);
    assert!(runner.state().delayed_triggers.is_empty(), "it fired");
    assert_eq!(zone(&runner, phytotitan), Zone::Graveyard);
}

// ------------------------------------------------------------ non-self subject

/// G2′ shape: a phase-delayed payload that reads only a player from the dead
/// creature. No printed card has it (the export's 90 phase-delayed payloads
/// under zone-change triggers include none), so the text is parsed like a card's.
const OWNER_DRAWS_LATER: &str = "Whenever another nontoken creature you control dies, its owner draws a card at the beginning of the next end step.";

/// P0 controls the observer and a Grizzly Bears that P1 owns; P0 Shocks the
/// Bears. With `put_back`, P0 puts the dead Bears back into P1's graveyard (a
/// new object, CR 400.7) before the observer's trigger resolves. Returns the
/// cards each player drew at the end step.
fn owner_draws_later(put_back: bool) -> (usize, usize) {
    let (mut runner, _, spells, bears) = board(Phase::PreCombatMain, |scenario| {
        scenario.add_creature_from_oracle(P0, "Observer", 1, 1, OWNER_DRAWS_LATER);
        scenario
            .add_creature(P1, "Grizzly Bears", 2, 2)
            .controlled_by(P0)
            .id()
    });
    cast_and_resolve_only(&mut runner, spells.p0_shock, TargetRef::Object(bears));
    if put_back {
        round_trip_through_library(&mut runner, bears, spells.p0_revival, spells.p0_scour, true);
    }
    runner.advance_until_stack_empty();
    let delayed = only_delayed(&runner);
    assert!(
        format!("{:?}", delayed.ability.effect).contains("ParentTargetOwner"),
        "reach-guard: the payload reads its owner: {:?}",
        delayed.ability.effect
    );
    let hands = |r: &GameRunner| {
        (
            r.state().players[0].hand.len(),
            r.state().players[1].hand.len(),
        )
    };
    let before = hands(&runner);
    through_end_step(&mut runner);
    assert!(runner.state().delayed_triggers.is_empty(), "it fired");
    let after = hands(&runner);
    (after.0 - before.0, after.1 - before.1)
}

/// E1: the owner of the dead Bears draws (CR 108.3).
#[test]
fn owner_draws_later_control() {
    assert_eq!(owner_draws_later(false), (0, 1));
}

/// E2: CR 608.2h: the Bears left the graveyard, so its owner is its last known
/// information; that player still draws.
#[test]
fn owner_draws_later_after_the_creature_is_put_back() {
    assert_eq!(owner_draws_later(true), (0, 1));
}

/// E3: stolen and put back before its trigger resolves: "their" is still the
/// owner, read from the departed object (CR 108.3, CR 608.2h); nothing returns.
#[test]
fn phytotitan_stolen_put_back_waits_for_owners_upkeep_and_returns_nothing() {
    let (mut runner, phytotitan, spells, ()) = board(Phase::PreCombatMain, |_| ());
    to_main_of(&mut runner, P1);
    cast_and_resolve_all(
        &mut runner,
        spells.p1_treason,
        TargetRef::Object(phytotitan),
    );
    cast_and_resolve_only(&mut runner, spells.p1_shock, TargetRef::Object(phytotitan));
    round_trip_through_library(
        &mut runner,
        phytotitan,
        spells.p1_revival,
        spells.p1_scour,
        true,
    );
    runner.advance_until_stack_empty();
    assert!(
        matches!(
            only_delayed(&runner).condition,
            DelayedTriggerCondition::AtNextPhaseForPlayer { player, .. } if player == P0
        ),
        "\"their\" is the owner"
    );
    through_next_upkeep_of(&mut runner, P0);
    assert!(runner.state().delayed_triggers.is_empty(), "it fired");
    assert_eq!(zone(&runner, phytotitan), Zone::Graveyard);
}

fn avacyn_board() -> (GameRunner, ObjectId, Spells) {
    let (runner, _phytotitan, spells, bears) = board(Phase::PreCombatMain, |scenario| {
        scenario.add_creature_from_oracle(
            P0,
            "Avacyn, Angel of Horror",
            8,
            8,
            AVACYN_ANGEL_OF_HORROR,
        );
        scenario.add_creature(P0, "Grizzly Bears", 2, 2).id()
    });
    (runner, bears, spells)
}

/// A1 — positive control: Avacyn returns another dead creature at the next end
/// step.
#[test]
fn avacyn_returns_another_dead_creature_at_the_next_end_step() {
    let (mut runner, bears, spells) = avacyn_board();
    cast_and_resolve_all(&mut runner, spells.p0_shock, TargetRef::Object(bears));
    assert_eq!(zone(&runner, bears), Zone::Graveyard);
    only_delayed(&runner);

    through_end_step(&mut runner);
    assert_eq!(zone(&runner, bears), Zone::Battlefield);
    assert_eq!(runner.state().objects[&bears].controller, P0);
    assert!(runner.state().delayed_triggers.is_empty());
}

/// A2 — the dying creature is put back into the graveyard before Avacyn's
/// trigger resolves: "that card" is no longer the object the death created
/// (CR 400.7e, CR 603.7c), so nothing returns.
#[test]
fn avacyn_does_not_return_a_creature_put_back_before_its_trigger_resolves() {
    let (mut runner, bears, spells) = avacyn_board();
    cast_and_resolve_only(&mut runner, spells.p0_shock, TargetRef::Object(bears));
    assert!(
        !runner.state().stack.is_empty(),
        "reach-guard: Avacyn's trigger waits on the stack"
    );
    round_trip_through_library(&mut runner, bears, spells.p0_revival, spells.p0_scour, true);
    runner.advance_until_stack_empty();

    let delayed = only_delayed(&runner);
    let pin = delayed
        .ability
        .target_incarnations
        .iter()
        .find(|pin| pin.object_id == bears)
        .copied()
        .expect("the creation referent is pinned");
    assert!(
        !pin.is_current(runner.state()),
        "the pin must be stale after the graveyard object leaves and returns"
    );

    through_end_step(&mut runner);
    assert_eq!(zone(&runner, bears), Zone::Graveyard);
    assert!(runner.state().delayed_triggers.is_empty());
}

// ------------------------------------------------------------------- merged

/// M1 — merged, nothing moved: both cards return (CR 730.3c).
#[test]
fn merged_phytotitan_returns_every_component_at_owners_next_upkeep() {
    let (mut runner, phytotitan, gemrazer, spells) = merged_board(|_| ());
    cast_and_resolve_all(&mut runner, spells.p0_shock, TargetRef::Object(phytotitan));
    assert_eq!(zone(&runner, phytotitan), Zone::Graveyard);
    assert_eq!(zone(&runner, gemrazer), Zone::Graveyard);
    let delayed = only_delayed(&runner);
    for id in [phytotitan, gemrazer] {
        assert!(
            delayed.ability.targets.contains(&TargetRef::Object(id)),
            "{id:?} is bound: {:?}",
            delayed.ability.targets
        );
        let pin = delayed
            .ability
            .target_incarnations
            .iter()
            .find(|pin| pin.object_id == id)
            .copied()
            .expect("each object is pinned");
        assert!(pin.is_current(runner.state()));
    }

    let events = through_next_upkeep_of(&mut runner, P0);
    for id in [phytotitan, gemrazer] {
        assert_returned_tapped_under(&runner, id, P0);
        assert!(runner.state().objects[&id].merged_components.is_empty());
    }
    let gemrazer_entries = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::ZoneChanged { object_id, to: Zone::Battlefield, .. }
                    if *object_id == gemrazer
            )
        })
        .count();
    assert_eq!(gemrazer_entries, 1, "Gemrazer returns once");
    assert!(runner.state().delayed_triggers.is_empty());
}

/// M2 — merged, the component left and re-entered the graveyard: only
/// Phytotitan returns.
#[test]
fn merged_phytotitan_component_put_back_is_not_returned() {
    let (mut runner, phytotitan, gemrazer, spells) = merged_board(|_| ());
    cast_and_resolve_all(&mut runner, spells.p0_shock, TargetRef::Object(phytotitan));
    only_delayed(&runner);

    to_main_of(&mut runner, P1);
    round_trip_through_library(
        &mut runner,
        gemrazer,
        spells.p1_revival,
        spells.p1_scour,
        false,
    );

    through_next_upkeep_of(&mut runner, P0);
    assert_returned_tapped_under(&runner, phytotitan, P0);
    assert_eq!(zone(&runner, gemrazer), Zone::Graveyard);
    assert!(runner.state().delayed_triggers.is_empty());
}

/// M3a — merged, Phytotitan left and re-entered the graveyard after its trigger
/// resolved: Gemrazer still returns (CR 730.3c + CR 609.3).
#[test]
fn merged_phytotitan_survivor_put_back_after_its_trigger_still_returns_its_component() {
    let (mut runner, phytotitan, gemrazer, spells) = merged_board(|_| ());
    cast_and_resolve_all(&mut runner, spells.p0_shock, TargetRef::Object(phytotitan));
    only_delayed(&runner);

    to_main_of(&mut runner, P1);
    round_trip_through_library(
        &mut runner,
        phytotitan,
        spells.p1_revival,
        spells.p1_scour,
        false,
    );

    through_next_upkeep_of(&mut runner, P0);
    assert_returned_tapped_under(&runner, gemrazer, P0);
    assert_eq!(zone(&runner, phytotitan), Zone::Graveyard);
    assert!(runner.state().delayed_triggers.is_empty());
}

/// M3b — merged, Phytotitan left and re-entered the graveyard before its
/// trigger resolved: Gemrazer, still the object the departure made, returns.
#[test]
fn merged_phytotitan_survivor_put_back_before_its_trigger_still_returns_its_component() {
    let (mut runner, phytotitan, gemrazer, spells) = merged_board(|_| ());
    cast_and_resolve_only(&mut runner, spells.p0_shock, TargetRef::Object(phytotitan));
    round_trip_through_library(
        &mut runner,
        phytotitan,
        spells.p0_revival,
        spells.p0_scour,
        true,
    );
    runner.advance_until_stack_empty();

    let delayed = only_delayed(&runner);
    assert_eq!(delayed.ability.targets, vec![TargetRef::Object(gemrazer)]);
    assert_eq!(
        delayed.ability.target_incarnations,
        vec![incarnation(&runner, gemrazer)]
    );

    through_next_upkeep_of(&mut runner, P0);
    assert_returned_tapped_under(&runner, gemrazer, P0);
    assert_eq!(zone(&runner, phytotitan), Zone::Graveyard);
    assert!(runner.state().delayed_triggers.is_empty());
}

/// CR 616.1e: answer a two-way entry tap-state ordering offered to P0.
fn answer_tap_state_ordering(runner: &mut GameRunner, answer: &str) {
    let index = match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice {
            player, candidates, ..
        } => {
            assert_eq!(*player, P0, "the entrant's controller orders (CR 616.1)");
            for source in ["Imposing Sovereign", "Archelos, Lagoon Mystic"] {
                assert!(
                    candidates.iter().any(|c| c.source_name == source),
                    "{source} must be offered: {candidates:?}"
                );
            }
            candidates
                .iter()
                .position(|c| c.source_name == answer)
                .unwrap_or_else(|| panic!("{answer} must be offered"))
        }
        other => panic!("expected the CR 616.1 tap-state ordering, got {other:?}"),
    };
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("answer the tap-state ordering");
}

/// M4 — a merged return stages both members' entries together; P0 holds both
/// CR 616.1 tap-state orderings at once, picks which to make first (CR 101.4c),
/// and the other member's ordering follows after the answer.
fn merged_return_resumes_through_replacement_ordering(answer: &str) {
    let (mut runner, phytotitan, gemrazer, spells) = merged_board(|scenario| {
        scenario.add_creature_from_oracle(
            P0,
            "Archelos, Lagoon Mystic",
            2,
            4,
            ARCHELOS_LAGOON_MYSTIC,
        );
        scenario.add_creature_from_oracle(P1, "Imposing Sovereign", 2, 1, IMPOSING_SOVEREIGN);
    });
    cast_and_resolve_all(&mut runner, spells.p0_shock, TargetRef::Object(phytotitan));
    only_delayed(&runner);

    let start = runner.state().turn_number;
    drive_until(&mut runner, "the return's decision-order prompt", |r| {
        (r.state().active_player == P0 && r.state().turn_number > start)
            && matches!(
                r.state().waiting_for,
                WaitingFor::ReplacementDecisionOrder { .. }
            )
    });
    let first = match &runner.state().waiting_for {
        WaitingFor::ReplacementDecisionOrder { player, objects } => {
            assert_eq!(*player, P0, "the entrants' controller orders (CR 616.1)");
            assert_eq!(objects.len(), 2, "both members' decisions: {objects:?}");
            assert!(
                objects.contains(&phytotitan) && objects.contains(&gemrazer),
                "{objects:?}"
            );
            objects[0]
        }
        other => panic!("expected the CR 101.4c decision order, got {other:?}"),
    };
    runner
        .act(GameAction::SelectCards { cards: vec![first] })
        .expect("P0 makes the first member's decision first");
    answer_tap_state_ordering(&mut runner, answer);
    // Reach-guard: the other member meets its own ordering.
    answer_tap_state_ordering(&mut runner, answer);
    runner.advance_until_stack_empty();

    for id in [phytotitan, gemrazer] {
        let object = &runner.state().objects[&id];
        assert_eq!(object.zone, Zone::Battlefield, "{} returned", object.name);
        assert_eq!(object.controller, P0);
        assert!(object.merged_components.is_empty());
    }
    assert!(runner.state().delayed_triggers.is_empty());
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        runner.state().waiting_for
    );
}

/// M4a — the ordering answered with Imposing Sovereign first.
#[test]
fn merged_phytotitan_return_resumes_through_replacement_ordering() {
    merged_return_resumes_through_replacement_ordering("Imposing Sovereign");
}

/// M4b — the ordering answered with Archelos first.
#[test]
fn merged_phytotitan_return_resumes_through_replacement_ordering_other_answer() {
    merged_return_resumes_through_replacement_ordering("Archelos, Lagoon Mystic");
}
