//! CR 111.1 + CR 601.2c + CR 115.9 + CR 111.8 + CR 608.2h + CR 730.2c + CR 730.2d:
//! an object's TOKEN bit is captured with the fact it belongs to — at the
//! announcement of a target and at a departure from the battlefield — and the
//! capture, not the live object and not the turn-scoped zone-change journal, is
//! what answers a historical question later.
//!
//! Both boundaries are exercised here as whole production pipelines rather than
//! as assembled state:
//!
//! * the ANNOUNCEMENT is an activation through `GameRunner::activate`, whose
//!   committed target records the bit it saw;
//! * the DEPARTURE is a real `DestroyAll` resolution that moves a real token and a
//!   real card off the battlefield, after which the resolving spell's own
//!   "for each nontoken creature you controlled that was destroyed this way"
//!   follow-up counts the departed set (Ceaseless Conflict's text).
//!
//! The merge shape is Cackling Counterpart's: a token copy of a creature is a
//! token that carries the copied card's characteristics, its name included, and a
//! real card of that same name then mutates on top of it. CR 730.2c keeps the
//! merged permanent the same object; CR 730.2d hands its token-ness to the
//! topmost component, so the bit flips to nontoken IN PLACE. CR 730.2b says the
//! merge is not a zone change, so no journal row records it, and the live object
//! only ever states the present. A selection made before the merge is a token
//! selection for the rest of the game, and the save has to carry that.
//!
//! Harness lineage: `resolve_oracle_effect` mirrors the private
//! `resolve_token_source` in `esix_fractal_bloom.rs` (parse the printed line,
//! build the resolved ability from its definition, run it through
//! `resolve_ability_chain`). That helper is private and `support.rs` exports no
//! equivalent, so the copy here is deliberate rather than an oversight —
//! promoting it would edit a file outside this change's scope.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::merge::{merge_object_onto, MergeSide};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    ActivationTargetFact, CapturedTokenBit, PersistedGameState, PersistedRestoreFinalization,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// `{2}: Tap target creature.` — an activated ability whose announced target is
/// the object whose epoch the first test is about.
const TAPPER: &str = "{2}: Tap target creature.";

/// Ceaseless Conflict's printed text, verbatim: a mass destroy whose follow-up
/// counts the departed set. The count is a `FilteredTrackedSetSize` over the set
/// `DestroyAll` published, so every member is read through its own departure
/// snapshot and — since this change — its own departure token capture. The token
/// it creates is 3/2 red and white, not a 1/1, which is why the assertions below
/// count Spirits rather than trusting a size.
const CEASELESS_CONFLICT: &str = "Destroy all creatures. Then create a 3/2 red and white \
                                 Spirit creature token for each nontoken creature you \
                                 controlled that was destroyed this way.";

fn mana(s: &mut GameScenario, player: PlayerId, n: usize) {
    s.with_mana_pool(
        player,
        (0..n)
            .map(|_| ManaUnit::new(ManaColor::Blue.into(), ObjectId(0), false, Vec::new()))
            .collect(),
    );
}

/// The announced token bit this activation's record states for `target`.
fn announced_bit(runner: &GameRunner, target: ObjectId) -> CapturedTokenBit {
    let records = runner
        .state()
        .abilities_activated_this_turn_by_player
        .get(&P0)
        .expect("P0 activated an ability this turn");
    let record = records.last().expect("one activation record");
    record
        .targets
        .iter()
        .find_map(|fact| match fact {
            ActivationTargetFact::Object { id, token, .. } if *id == target => Some(*token),
            ActivationTargetFact::Object { .. } | ActivationTargetFact::Player(_) => None,
        })
        .expect("the record names the announced target")
}

/// Resolve one line of Oracle text through the engine's own parser and effect
/// pipeline, from a throwaway sorcery on the stack. This is how a real spell's
/// effects reach the board — nothing here is asserted on a hand-built ability.
fn resolve_oracle_effect(
    runner: &mut GameRunner,
    controller: PlayerId,
    oracle: &str,
    source_name: &str,
) {
    let parsed = parse_oracle_text(oracle, source_name, &[], &["Sorcery".to_string()], &[]);
    let def = parsed
        .abilities
        .first()
        .expect("the oracle line parses to an ability");
    let source = create_object(
        runner.state_mut(),
        CardId(7002),
        controller,
        source_name.to_string(),
        Zone::Stack,
    );
    let ability = build_resolved_from_def(def, source, controller);
    let mut events = Vec::<GameEvent>::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("the effect resolves");
    // The source was never cast; drop it so it cannot answer a later question.
    runner.state_mut().objects.remove(&source);
}

/// How many objects named `name` are on the battlefield right now.
fn battlefield_count(runner: &GameRunner, name: &str) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|object| object.name == name)
        .count()
}

#[test]
fn announced_token_capture_survives_a_same_named_merge_and_a_save_round_trip() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let tapper = s.add_creature_from_oracle(P0, "Tapper", 2, 2, TAPPER).id();
    mana(&mut s, P0, 4);
    let mut runner = s.build();

    // CR 111.1 + CR 111.2: the token is put onto the battlefield by a real token
    // effect, so it is a token the way a game makes one — an object on the
    // battlefield that no card represents.
    resolve_oracle_effect(
        &mut runner,
        P0,
        "Create a 1/1 white Soldier creature token.",
        "Token Source",
    );
    let host = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| {
            runner
                .state()
                .objects
                .get(id)
                .is_some_and(|object| object.name == "Soldier" && object.is_token)
        })
        .expect("the token effect put a token onto the battlefield");

    // CR 601.2c: the announcement is where the bit is captured. The activation
    // targets the token on purpose.
    runner.activate(tapper, 0).target_object(host).resolve();
    assert_eq!(
        announced_bit(&runner, host),
        CapturedTokenBit::Known(true),
        "the announcement must capture the token bit it saw"
    );

    // CR 730.2a + CR 730.2c + CR 730.2d: a real CARD of the same name mutates ON
    // TOP — same ObjectId, same NAME (730.2a), nontoken from now on (730.2d).
    let card = create_object(
        runner.state_mut(),
        CardId(7001),
        P0,
        "Soldier".to_string(),
        Zone::Battlefield,
    );
    runner.state_mut().battlefield.retain(|&id| id != card);
    let mut events = Vec::new();
    merge_object_onto(runner.state_mut(), card, host, MergeSide::Top, &mut events);
    let live = &runner.state().objects[&host];
    assert!(
        !live.is_token,
        "fixture precondition: a card on top of a token host is nontoken (CR 730.2d)"
    );
    assert_eq!(
        live.name, "Soldier",
        "fixture precondition: the merge leaves the name unchanged, so no name can \
         place the selection on either side (CR 730.2a)"
    );

    // CR 514.2: the turn-scoped journal is cleared at cleanup, and CR 730.2b means
    // the merge never wrote a row anyway. Nothing a reconstruction could read is
    // left in place.
    runner.state_mut().zone_changes_this_turn.clear();

    // CR 115.9: the saved game carries the selection's own epoch. The capture is
    // not a fact about the merged permanent's present state.
    let json = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("state serializes");
    let restored = serde_json::from_str::<PersistedGameState>(&json)
        .expect("persisted state decodes")
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .expect("the save is admissible")
        .finalize_after_rehydration(|_| Ok(()))
        .expect("the restored state is publishable");
    let runner = GameRunner::from_state(restored);

    assert!(
        !runner.state().objects[&host].is_token,
        "reach: the restored live object is the post-merge nontoken permanent"
    );
    assert_eq!(
        announced_bit(&runner, host),
        CapturedTokenBit::Known(true),
        "CR 115.9: the restored record still states the SELECTION's token bit, \
         which the live object can no longer supply"
    );
}

#[test]
fn ceaseless_conflict_counts_a_departed_token_from_its_departure_capture() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    // P0: one real card and one real token; P1: one real card. Only P0's nontoken
    // creature is a "nontoken creature you controlled that was destroyed this way".
    let card = s.add_creature(P0, "Bear", 2, 2).id();
    s.add_creature(P1, "Opponent Bear", 2, 2);
    let mut runner = s.build();

    // CR 111.1 + CR 111.2: a real token put onto the battlefield by a real token
    // effect, not a card marked as one.
    resolve_oracle_effect(
        &mut runner,
        P0,
        "Create a 1/1 white Soldier creature token.",
        "Token Source",
    );
    let token = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| {
            runner
                .state()
                .objects
                .get(id)
                .is_some_and(|object| object.name == "Soldier" && object.is_token)
        })
        .expect("the token effect put a token onto the battlefield");

    // CR 701.8a + CR 608.2c: the mass destroy and its "for each … destroyed this
    // way" follow-up, resolved as the one spell its Oracle text describes.
    //
    // Why the count is asked in the same resolution instead of after a journal
    // clear: the tracked set it reads is RESOLUTION-scoped. Measured, not assumed
    // — `tracked_object_sets` is empty the moment this resolution returns, so a
    // later instruction (or a cleared journal) has no set left to count and would
    // read 0 for reasons that have nothing to do with the token bit. The
    // journal-vs-capture discrimination lives where a legal state can separate
    // them: `quantity::tests::tracked_member_token_bit_comes_from_its_departure_capture`
    // asks the same member filter with the journal empty. What THIS test
    // discriminates is the capture itself: with the departure bit uncaptured
    // (fix reverted) the member's token leaf refuses and the count is 0 instead
    // of 1.
    resolve_oracle_effect(&mut runner, P0, CEASELESS_CONFLICT, "Ceaseless Conflict");

    // CR 701.8a: the destroy, not a state-based action, moved both of them.
    assert!(
        runner.state().objects[&card].zone == Zone::Graveyard
            && runner.state().objects[&token].zone == Zone::Graveyard,
        "reach: the destroy moved both P0 creatures to the graveyard"
    );

    // CR 608.2h: the count reads last known information — the destroyed set's own
    // departure epochs. Exactly one Spirit: P1's creature was not the caster's,
    // and the departed TOKEN is not a nontoken creature. A defaulted "not a token"
    // for an uncaptured member would make this two.
    assert_eq!(
        battlefield_count(&runner, "Spirit"),
        1,
        "only the controller's NONTOKEN destroyed creature is counted"
    );

    // The capture the count read is the departure's own, keyed by the exact
    // incarnation the destroy stamped on the tracked-set member.
    let set_id = runner
        .state()
        .tracked_object_sets
        .iter()
        .max_by_key(|(id, _)| id.0)
        .map(|(id, _)| *id)
        .expect("the destroy published its tracked set");
    let origin = runner
        .state()
        .tracked_set_member_origins
        .get(&set_id)
        .expect("the published set stamps its members' departure epochs")[&token];
    assert_eq!(origin.departed_from, Zone::Battlefield);
    assert_eq!(
        runner
            .state()
            .departure_token_by_incarnation
            .get(&token)
            .and_then(|history| history.get(&origin.departure_incarnation)),
        Some(&true),
        "CR 111.8: the token's departure is the only epoch that will ever state \
         its bit, and it is captured beside that epoch's last known information"
    );
    assert_eq!(
        runner
            .state()
            .departure_token_by_incarnation
            .get(&card)
            .and_then(|history| history.get(
                &runner.state().tracked_set_member_origins[&set_id][&card].departure_incarnation
            )),
        Some(&false),
        "and the card's departure states the opposite bit"
    );
}

#[test]
fn departure_token_capture_survives_a_save_round_trip() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_creature(P0, "Bear", 2, 2);
    let mut runner = s.build();
    resolve_oracle_effect(
        &mut runner,
        P0,
        "Create a 1/1 white Soldier creature token.",
        "Token Source",
    );
    let token = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| {
            runner
                .state()
                .objects
                .get(id)
                .is_some_and(|object| object.name == "Soldier" && object.is_token)
        })
        .expect("the token effect put a token onto the battlefield");

    // CR 111.7 + CR 111.8: the token leaves the battlefield — the one epoch that
    // will ever state its bit, since it can never come back to answer for itself.
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), token, Zone::Graveyard, &mut events);
    let capture = runner
        .state()
        .departure_token_by_incarnation
        .get(&token)
        .cloned()
        .expect("the departure writes the epoch's token capture");
    assert_eq!(
        capture.len(),
        1,
        "one departure, one captured incarnation: {capture:?}"
    );
    assert!(
        capture.values().all(|is_token| *is_token),
        "the token departed as a token: {capture:?}"
    );

    // CR 514.2: the journal row that also recorded the departure does not outlive
    // the turn, so the capture is what a later read has.
    runner.state_mut().zone_changes_this_turn.clear();

    let json = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("state serializes");
    let restored = serde_json::from_str::<PersistedGameState>(&json)
        .expect("persisted state decodes")
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .expect("the save is admissible")
        .finalize_after_rehydration(|_| Ok(()))
        .expect("the restored state is publishable");

    assert_eq!(
        restored.departure_token_by_incarnation.get(&token),
        Some(&capture),
        "CR 608.2h: the departure capture is part of the saved state"
    );
}
