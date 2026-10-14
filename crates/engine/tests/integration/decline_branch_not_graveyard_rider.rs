//! An "If you don't [cast it], …" decline branch is a following instruction,
//! not a graveyard rider on the spell cast this way.
//!
//! CR 608.2c: "The controller of the spell or ability follows its instructions
//! in the order written." The "If you don't …" sentence after a "you may cast /
//! play" instruction is followed only when that cast was not performed.
//! CR 614.1a: effects that use the word "instead" are replacement effects. The
//! decline sentence has no "instead" and none of the other replacement forms
//! CR 614.1b-e list, so it replaces nothing. A spell that WAS cast this way
//! therefore follows the default rules: it goes to its owner's graveyard as the
//! final part of its resolution (CR 608.2n) or when it is countered (CR 701.6a).
//!
//! The class is every `CastFromZone` whose sub link is a decline branch gated on
//! the preceding optional, across three destinations: Hand (Discover the
//! Impossible, Rashmi, Eternities Crafter, Solstice Revelations, Ziatora's
//! Envoy), Exile (Djinn of Wishes) and library bottom (Descendants' Path,
//! Enigma Sphinx Avatar). Torrential Gearhulk's "If that spell would be put
//! into your graveyard, exile it instead" is a real CR 614.1a rider and stays
//! one.
//!
//! Rows (verbatim Oracle text throughout):
//! * R1 / R2 / R3: the accepted cast's spell takes its normal route to the
//!   graveyard (Hand / Exile / library-bottom class respectively).
//! * G1 / G2 / G3 / G4: declined or never-offered paths still follow the
//!   decline instruction.
//! * G5: the unconditioned "instead" rider still exiles the cast spell.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::EffectKind;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const DIVINATION: &str = "Draw two cards.";
const SOLSTICE_REVELATIONS: &str = "Exile cards from the top of your library until you exile a nonland card. You may cast that card without paying its mana cost if the spell's mana value is less than the number of Mountains you control. If you don't cast that card this way, put it into your hand.\nFlashback {6}{R} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";
const DJINN_OF_WISHES: &str = "Flying\nThis creature enters with three wish counters on it.\n{2}{U}{U}, Remove a wish counter from this creature: Reveal the top card of your library. You may play that card without paying its mana cost. If you don't, exile it.";
const DESCENDANTS_PATH: &str = "At the beginning of your upkeep, reveal the top card of your library. If it's a creature card that shares a creature type with a creature you control, you may cast it without paying its mana cost. If you don't cast it, put it on the bottom of your library.";
const TORRENTIAL_GEARHULK: &str = "Flash\nWhen this creature enters, you may cast target instant card from your graveyard without paying its mana cost. If that spell would be put into your graveyard, exile it instead.";
const OPT: &str = "Scry 1. (Look at the top card of your library. You may put that card on the bottom.)\nDraw a card.";
const CANCEL: &str = "Counter target spell.";

fn resolved(events: &[GameEvent], kind: EffectKind, source: ObjectId) -> bool {
    events.iter().any(|e| {
        matches!(
            e,
            GameEvent::EffectResolved { kind: k, source_id, .. } if *k == kind && *source_id == source
        )
    })
}

fn resolved_kind(events: &[GameEvent], kind: EffectKind) -> bool {
    events
        .iter()
        .any(|e| matches!(e, GameEvent::EffectResolved { kind: k, .. } if *k == kind))
}

// ---------------------------------------------------------------------------
// Solstice Revelations (Hand class, during-resolution cast)
// ---------------------------------------------------------------------------

struct SolsticeRun {
    offered: bool,
    solstice: ObjectId,
    divination: ObjectId,
    /// Every event from the cast through the end of resolution.
    events: Vec<GameEvent>,
    zone: Zone,
    hand_drawn: i64,
}

/// Cast Solstice Revelations with `mountains` Mountains in play over a library
/// of Mountain, then Divination (mana value 3), then fillers. Passes priority
/// until the "you may cast" prompt appears (recording whether it did), answers
/// it with `accept`, and drives to an empty stack.
fn cast_solstice(mountains: usize, accept: bool) -> SolsticeRun {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..mountains {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    for i in 0..4 {
        scenario.add_spell_to_library_top(P0, &format!("Filler {i}"), false);
    }
    let divination = scenario
        .add_spell_to_library_top(P0, "Divination", false)
        .from_oracle_text(DIVINATION)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    scenario.add_land_to_library_top(P0, "Mountain");
    let solstice = scenario
        .add_spell_to_hand_from_oracle(P0, "Solstice Revelations", false, SOLSTICE_REVELATIONS)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    let mut commit = runner.cast(solstice).commit();
    let mut events = Vec::new();
    while matches!(commit.state().waiting_for, WaitingFor::Priority { .. })
        && !commit.state().stack.is_empty()
    {
        events.extend(commit.act(GameAction::PassPriority).expect("pass").events);
    }
    let offered = matches!(
        commit.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    );
    let commit = if accept {
        commit.accept_optional()
    } else {
        commit.decline_optional()
    };
    let outcome = commit.resolve();
    events.extend_from_slice(outcome.events());
    SolsticeRun {
        offered,
        solstice,
        divination,
        events,
        zone: outcome.zone_of(divination),
        hand_drawn: outcome.hand_drawn(P0),
    }
}

/// R1 — CR 608.2c + CR 608.2n: Divination, cast this way, resolves and goes to
/// its owner's graveyard. "If you don't cast that card this way, put it into
/// your hand" is not followed, because the card was cast.
#[test]
fn solstice_revelations_accepted_cast_spell_goes_to_graveyard() {
    let run = cast_solstice(4, true);
    assert!(run.offered, "reach: 3 < 4 Mountains offers the free cast");
    assert!(
        resolved(&run.events, EffectKind::Draw, run.divination),
        "reach: Divination was cast and resolved; events={:?}",
        run.events
    );
    assert_eq!(
        run.zone,
        Zone::Graveyard,
        "the resolved spell goes to its owner's graveyard (CR 608.2n)"
    );
    assert_eq!(
        run.hand_drawn, 2,
        "Divination draws two and does not return to the hand"
    );
}

/// G1 — declined: the decline instruction puts the card into the hand.
#[test]
fn solstice_revelations_declined_card_goes_to_hand() {
    let run = cast_solstice(4, false);
    assert!(run.offered, "reach: the free cast was offered");
    assert!(
        !resolved_kind(&run.events, EffectKind::CastFromZone),
        "the declined cast resolves no CastFromZone"
    );
    assert!(
        resolved(&run.events, EffectKind::ChangeZone, run.solstice),
        "the decline instruction runs; events={:?}",
        run.events
    );
    assert_eq!(run.zone, Zone::Hand);
}

/// G2 — head gate false (mana value 3 is not less than 2 Mountains): the cast
/// is never offered, so "If you don't cast that card this way" holds.
#[test]
fn solstice_revelations_gate_false_card_goes_to_hand() {
    let run = cast_solstice(2, true);
    assert!(
        resolved(&run.events, EffectKind::ExileFromTopUntil, run.solstice),
        "reach: Solstice exiled from the top; events={:?}",
        run.events
    );
    assert!(!run.offered, "3 < 2 is false, so the cast is not offered");
    assert_eq!(run.zone, Zone::Hand);
}

// ---------------------------------------------------------------------------
// Djinn of Wishes (Exile class, "play that card")
// ---------------------------------------------------------------------------

fn floating(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

/// A Djinn of Wishes with three wish counters, {2}{U}{U} floating, and
/// Divination on top of the library over fillers. Returns the runner, the Djinn
/// and Divination.
fn djinn_board() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        floating(2, ManaType::Colorless)
            .into_iter()
            .chain(floating(2, ManaType::Blue))
            .collect(),
    );
    for i in 0..4 {
        scenario.add_spell_to_library_top(P0, &format!("Filler {i}"), false);
    }
    let divination = scenario
        .add_spell_to_library_top(P0, "Divination", false)
        .from_oracle_text(DIVINATION)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let djinn = scenario
        .add_creature_from_oracle(P0, "Djinn of Wishes", 4, 4, DJINN_OF_WISHES)
        .id();
    scenario.with_counter(djinn, CounterType::Generic("wish".to_string()), 3);
    (scenario.build(), djinn, divination)
}

/// R2 — CR 608.2c + CR 608.2n: Divination, played this way, resolves and goes
/// to its owner's graveyard; "If you don't, exile it" is not a rider on it.
#[test]
fn djinn_of_wishes_accepted_cast_spell_goes_to_graveyard() {
    let (mut runner, djinn, divination) = djinn_board();
    runner.activate(djinn, 0).accept_optional().resolve();
    let outcome = runner.cast(divination).resolve();
    assert!(
        resolved(outcome.events(), EffectKind::Draw, divination),
        "reach: Divination was cast and resolved; events={:?}",
        outcome.events()
    );
    assert_eq!(outcome.hand_drawn(P0), 2);
    assert_eq!(
        outcome.zone_of(divination),
        Zone::Graveyard,
        "the resolved spell goes to its owner's graveyard (CR 608.2n)"
    );
}

/// G3 — declined: the decline instruction runs. The card is already in exile
/// from the reveal, so the discriminator is the decline link's own
/// `EffectResolved { ChangeZone }`, which the accepted run never emits.
#[test]
fn djinn_of_wishes_declined_runs_the_exile_instruction() {
    let (mut runner, djinn, divination) = djinn_board();
    let outcome = runner.activate(djinn, 0).decline_optional().resolve();
    assert!(
        resolved(outcome.events(), EffectKind::ChangeZone, djinn),
        "the decline instruction runs; events={:?}",
        outcome.events()
    );
    assert_eq!(outcome.zone_of(divination), Zone::Exile);
}

// ---------------------------------------------------------------------------
// Descendants' Path (library-bottom class, upkeep trigger)
// ---------------------------------------------------------------------------

/// Seed a Goblin creature card on top of `player`'s library.
fn seed_library_top_goblin(state: &mut GameState, player: PlayerId) -> ObjectId {
    let id = create_object(
        state,
        CardId(state.next_object_id),
        player,
        "Revealed Goblin".to_string(),
        Zone::Library,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![CoreType::Creature];
    obj.base_card_types.core_types = vec![CoreType::Creature];
    obj.card_types.subtypes = vec!["Goblin".to_string()];
    obj.base_card_types.subtypes = vec!["Goblin".to_string()];
    obj.mana_cost = ManaCost::generic(2);
    let ps = state.players.iter_mut().find(|p| p.id == player).unwrap();
    ps.library.retain(|&o| o != id);
    ps.library.insert(0, id);
    id
}

/// P1 controls Descendants' Path and a Goblin, holds a {0} "Counter target
/// spell.", and has a Goblin creature card on top of the library. Advances to
/// P1's upkeep with the trigger on the stack. Returns the runner, the revealed
/// card and the counterspell.
fn path_board() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P1, "Descendants' Path", DESCENDANTS_PATH);
    scenario
        .add_creature(P1, "Goblin Piker", 2, 1)
        .with_subtypes(vec!["Goblin"]);
    scenario.with_library_top(P1, &["Filler One", "Filler Two", "Filler Three"]);
    let cancel = scenario
        .add_spell_to_hand_from_oracle(P1, "Cancel", true, CANCEL)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Goblin".to_string(), "Wall".to_string()];
    let top = seed_library_top_goblin(runner.state_mut(), P1);
    runner.advance_to_phase(Phase::Upkeep);
    assert_eq!(runner.state().active_player, P1, "reach: P1's upkeep");
    assert!(
        !runner.state().stack.is_empty(),
        "reach: the upkeep trigger is on the stack"
    );
    (runner, top, cancel)
}

/// Pass priority and answer the "you may cast" prompt with `accept` until
/// `stop` holds or the stack is empty. Stops inside the upkeep step, before the
/// CR 504.1 draw. Returns whether the prompt was offered.
fn drive_upkeep(runner: &mut GameRunner, accept: bool, stop: impl Fn(&GameState) -> bool) -> bool {
    let mut offered = false;
    for _ in 0..40 {
        if stop(runner.state()) {
            break;
        }
        match runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                offered = true;
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("optional");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            ref other => panic!("unexpected prompt: {other:?}"),
        }
    }
    offered
}

/// R3 — CR 608.2c + CR 701.6a: the creature spell cast this way and then
/// countered goes to its owner's graveyard, not to the bottom of the library.
#[test]
fn descendants_path_accepted_then_countered_goes_to_graveyard() {
    let (mut runner, top, cancel) = path_board();
    let offered = drive_upkeep(&mut runner, true, |s| s.objects[&top].zone == Zone::Stack);
    assert!(offered, "reach: the free cast was offered");
    assert_eq!(
        runner.state().objects[&top].zone,
        Zone::Stack,
        "reach: the revealed creature was cast"
    );
    let outcome = runner.cast(cancel).target_object(top).resolve();
    assert!(
        outcome.events().iter().any(|e| matches!(
            e,
            GameEvent::SpellCountered { object_id, .. } if *object_id == top
        )),
        "reach: the creature spell was countered; events={:?}",
        outcome.events()
    );
    assert_eq!(
        outcome.zone_of(top),
        Zone::Graveyard,
        "a countered spell goes to its owner's graveyard (CR 701.6a)"
    );
    assert!(
        !outcome.state().players[P1.0 as usize]
            .library
            .contains(&top),
        "the countered spell is not put on the bottom of the library"
    );
}

/// G4 — declined after being offered: the decline instruction puts the card on
/// the bottom of the library.
#[test]
fn descendants_path_declined_goes_to_library_bottom() {
    let (mut runner, top, _) = path_board();
    let offered = drive_upkeep(&mut runner, false, |_| false);
    assert!(offered, "reach: the free cast was offered");
    assert_eq!(
        runner.state().players[P1.0 as usize].library.last(),
        Some(&top),
        "the declined card is on the bottom of the library"
    );
}

// ---------------------------------------------------------------------------
// Torrential Gearhulk (an unconditioned CR 614.1a rider)
// ---------------------------------------------------------------------------

/// G5 — CR 614.1a: "If that spell would be put into your graveyard, exile it
/// instead" still exiles the spell cast this way.
#[test]
fn torrential_gearhulk_instead_rider_still_exiles() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..4 {
        scenario.add_spell_to_library_top(P0, &format!("Filler {i}"), false);
    }
    let opt = scenario
        .add_spell_to_graveyard(P0, "Opt", true)
        .from_oracle_text(OPT)
        .with_mana_cost(ManaCost::zero())
        .id();
    let gearhulk = scenario
        .add_creature_to_hand_from_oracle(P0, "Torrential Gearhulk", 5, 6, TORRENTIAL_GEARHULK)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let outcome = runner
        .cast(gearhulk)
        .target_object(opt)
        .accept_optional()
        .resolve();
    assert!(
        resolved(outcome.events(), EffectKind::Draw, opt),
        "reach: Opt was cast and resolved; events={:?}",
        outcome.events()
    );
    assert_eq!(
        outcome.zone_of(opt),
        Zone::Exile,
        "the 'instead' rider exiles the spell (CR 614.1a)"
    );
}
