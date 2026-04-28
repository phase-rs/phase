//! Integration tests for the Tempting Offer cycle (Tempt with Discovery, Glory,
//! Immortality, Reflections, Vengeance — Commander 2013). The cycle shares a
//! "you do X, each opponent may do X, then for each opponent who took the offer
//! you do X again" shape; the "for each opponent who [verbed] this way" step
//! is the architectural crux.
//!
//! GitHub issue #132: "Playing tempt with discovery gives you the first tutor,
//! the opponents tutor, but you don't get to tutor for each opponent that did
//! so." This file pins the parser-level shape; the engine round-trip is
//! covered by `tempt_with_discovery_engine.rs`.

use engine::game::effects::resolve_ability_chain;
use engine::game::engine::apply;
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    Effect, PlayerFilter, QuantityExpr, QuantityRef, ResolvedAbility, SearchSelectionConstraint,
    TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// CR 207.2c + CR 608.2c + CR 109.5: Tempt with Discovery's full Oracle text
/// must produce an ability whose 4th sentence uses
/// `repeat_for: PlayerCount { OpponentZoneChangedThisWay }`.
///
/// "Tempting offer —" is an ability word (CR 207.2c) and is stripped by the
/// parser before the body parses. The remaining sentences chain via
/// `sub_ability`:
///
///   1. `SearchLibrary { filter: land, target_player: None (controller) }`
///   2. `SearchLibrary { ..., player_scope: Opponent, optional: true }` — each
///      opponent independently decides yes/no per CR 608.2d.
///   3. `SearchLibrary { ..., repeat_for: PlayerCount {
///         filter: OpponentZoneChangedThisWay } }` — the bonus tutor per
///      accepting opponent.
///
/// The runtime evaluates the `repeat_for` quantity once at sentence 3's start;
/// `players_zone_changed_this_way` (populated across all sentence-2
/// `player_scope` iterations) gives the count of opponents who actually
/// searched. See `crates/engine/src/types/game_state.rs` for the accumulator
/// rationale and `crates/engine/src/types/ability.rs` for the
/// `OpponentZoneChangedThisWay` filter.
#[test]
fn tempt_with_discovery_step_4_uses_opponent_zone_changed_this_way_repeat_for() {
    let oracle = "Tempting offer — Search your library for a land card and put it onto the battlefield. \
                  Then each opponent may search their library for a land card and put it onto the battlefield. \
                  For each opponent who searches their library this way, search your library for a land card \
                  and put it onto the battlefield. Then shuffle.";

    let result = parse_oracle_text(
        oracle,
        "Tempt with Discovery",
        &[],
        &["Sorcery".to_string()],
        &[],
    );

    // Tempt with Discovery is a sorcery — its body becomes a single
    // OnResolve ability (the "spell" ability) with chained sub_abilities for
    // each sentence after the first.
    assert!(
        !result.abilities.is_empty(),
        "Tempt with Discovery must produce at least one ability, got {:?}",
        result.abilities
    );

    // Walk the entire ability + sub_ability chain looking for a SearchLibrary
    // step whose `repeat_for` is `PlayerCount { OpponentZoneChangedThisWay }`.
    // We don't pin sentence ordering or sub_ability nesting depth — the
    // architectural assertion is "somewhere in the chain, the bonus-tutor
    // step parses with the right repeat_for filter."
    fn walk(def: &engine::types::ability::AbilityDefinition) -> bool {
        let here_matches = matches!(&*def.effect, Effect::SearchLibrary { .. })
            && matches!(
                &def.repeat_for,
                Some(QuantityExpr::Ref {
                    qty: QuantityRef::PlayerCount {
                        filter: PlayerFilter::OpponentZoneChangedThisWay,
                    },
                })
            );
        if here_matches {
            return true;
        }
        if let Some(sub) = &def.sub_ability {
            if walk(sub) {
                return true;
            }
        }
        if let Some(else_branch) = &def.else_ability {
            if walk(else_branch) {
                return true;
            }
        }
        false
    }

    let found = result.abilities.iter().any(walk);
    assert!(
        found,
        "Expected a SearchLibrary step with \
         repeat_for = PlayerCount {{ OpponentZoneChangedThisWay }} somewhere in \
         the ability chain. Parsed abilities: {:#?}",
        result.abilities
    );
}

/// Build a 3-player game state and seed P0's library with `count` basic
/// Forest cards. P0 is the controller.
fn make_3p_game_with_p0_lands(count: usize) -> (GameState, Vec<ObjectId>) {
    let mut state = GameState::new(FormatConfig::standard(), 3, 42);
    let mut lands = Vec::with_capacity(count);
    for i in 0..count {
        let land = create_object(
            &mut state,
            CardId(100 + i as u64),
            PlayerId(0),
            format!("Forest #{i}"),
            Zone::Library,
        );
        let obj = state.objects.get_mut(&land).unwrap();
        obj.card_types.core_types = vec![CoreType::Land];
        obj.card_types.supertypes.push(Supertype::Basic);
        lands.push(land);
    }
    (state, lands)
}

/// Build the step-4 ability for Tempt with Discovery in isolation: P0
/// (controller) searches their library for a land card and puts it onto the
/// battlefield, with `repeat_for = PlayerCount { OpponentZoneChangedThisWay }`.
/// Pre-populates `players_zone_changed_this_way` to simulate steps 1-3
/// having already run (so we can test step 4 in isolation without driving
/// the entire chain end-to-end through the cast pipeline).
fn make_step_4_ability() -> ResolvedAbility {
    let put = ResolvedAbility::new(
        Effect::ChangeZone {
            origin: Some(Zone::Library),
            destination: Zone::Battlefield,
            target: TargetFilter::Any,
            owner_library: false,
            enter_transformed: false,
            under_your_control: false,
            enter_tapped: false,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
        },
        vec![],
        ObjectId(9000),
        PlayerId(0),
    );
    let mut search = ResolvedAbility::new(
        Effect::SearchLibrary {
            filter: TargetFilter::Typed(TypedFilter::land()),
            count: QuantityExpr::Fixed { value: 1 },
            reveal: false,
            target_player: None, // searcher = controller (P0)
            selection_constraint: SearchSelectionConstraint::None,
        },
        vec![],
        ObjectId(9000),
        PlayerId(0),
    )
    .sub_ability(put);
    search.repeat_for = Some(QuantityExpr::Ref {
        qty: QuantityRef::PlayerCount {
            filter: PlayerFilter::OpponentZoneChangedThisWay,
        },
    });
    search
}

/// CR 608.2c + CR 109.5: Engine-level proof of issue #132. Pre-populates the
/// `players_zone_changed_this_way` accumulator with two opponents (simulating
/// "P1 and P2 both took the offer" outcomes from step 2), then runs only
/// step 4 (the bonus-tutor repeat). The loop must run exactly twice — once
/// per accepting opponent — and place 2 lands onto the battlefield from P0's
/// library after P0's search choices.
///
/// This is the exact bug from issue #132: prior to the fix, step 4 fired
/// either zero times (PlayerCount { Opponent } would over-count to 2 but
/// `players_zone_changed_this_way` did not exist, so the parser would
/// produce an Unimplemented step that did nothing) or only once (the LAST
/// player_scope iteration's `last_zone_changed_ids` would be visible). With
/// the fix, the accumulator persists across iterations and step 4 fires
/// once per accepting opponent.
#[test]
fn tempt_with_discovery_step_4_fires_once_per_accepting_opponent_two_accept() {
    let (mut state, lands) = make_3p_game_with_p0_lands(3);
    state.players_zone_changed_this_way.insert(PlayerId(1));
    state.players_zone_changed_this_way.insert(PlayerId(2));

    let ability = make_step_4_ability();
    let mut events: Vec<GameEvent> = Vec::new();
    // depth=1 to simulate being inside the larger chain (steps 1-3 already
    // ran at depth=0 above).
    resolve_ability_chain(&mut state, &ability, &mut events, 1).unwrap();

    // Iteration 0: P0 prompted, picks lands[0].
    let r0 = apply(
        &mut state,
        PlayerId(0),
        GameAction::SelectCards {
            cards: vec![lands[0]],
        },
    )
    .unwrap();
    events.extend(r0.events);

    // Iteration 1: P0 prompted again, picks lands[1].
    let r1 = apply(
        &mut state,
        PlayerId(0),
        GameAction::SelectCards {
            cards: vec![lands[1]],
        },
    )
    .unwrap();
    events.extend(r1.events);

    // Both lands moved from library to battlefield.
    assert_eq!(
        state.objects.get(&lands[0]).unwrap().zone,
        Zone::Battlefield,
        "iteration 0: P0's first chosen land must be on the battlefield"
    );
    assert_eq!(
        state.objects.get(&lands[1]).unwrap().zone,
        Zone::Battlefield,
        "iteration 1: P0's second chosen land must be on the battlefield — \
         failure means step 4 only ran once (issue #132's exact bug). With \
         two accepting opponents, the bonus tutor must fire twice."
    );
    // Third land remains in library (only 2 iterations consumed).
    assert_eq!(
        state.objects.get(&lands[2]).unwrap().zone,
        Zone::Library,
        "third land must remain in library — only 2 iterations should run \
         (one per accepting opponent), not 3"
    );

    // No pending iteration — the loop completed.
    assert!(
        state.pending_repeat_iteration.is_none(),
        "loop must clear pending_repeat_iteration after final iteration completes"
    );
}

/// CR 608.2c + CR 109.5: Boundary case — zero opponents accept. Step 4 must
/// not fire at all (repeat count = 0). P0's library should be untouched by
/// the bonus step, and no SearchChoice prompt should be raised.
#[test]
fn tempt_with_discovery_step_4_does_not_fire_when_no_opponents_accept() {
    let (mut state, lands) = make_3p_game_with_p0_lands(3);
    // Accumulator only contains P0 (the controller from step 1's own search).
    // No opponents took the offer.
    state.players_zone_changed_this_way.insert(PlayerId(0));

    let ability = make_step_4_ability();
    let mut events: Vec<GameEvent> = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 1).unwrap();

    // No SearchChoice raised — `repeat_for` evaluated to 0 and the loop
    // never entered.
    assert!(
        !matches!(
            state.waiting_for,
            engine::types::game_state::WaitingFor::SearchChoice { .. }
        ),
        "no SearchChoice expected when repeat count is 0; got {:?}",
        state.waiting_for
    );
    // All three lands remain in P0's library.
    for (i, land) in lands.iter().enumerate() {
        assert_eq!(
            state.objects.get(land).unwrap().zone,
            Zone::Library,
            "land {i} must remain in P0's library — step 4 should not fire \
             when zero opponents took the offer"
        );
    }
    assert!(state.pending_repeat_iteration.is_none());
}

/// CR 608.2c + CR 109.5: Boundary case — all opponents accept. With 3
/// players (P0 + P1, P2 opponents), step 4 must fire twice.
#[test]
fn tempt_with_discovery_step_4_fires_n_times_when_n_opponents_accept() {
    // Use a 4-player game (P0 + 3 opponents) to exercise N=3.
    let mut state = GameState::new(FormatConfig::standard(), 4, 42);
    let mut lands = Vec::with_capacity(4);
    for i in 0..4 {
        let land = create_object(
            &mut state,
            CardId(100 + i as u64),
            PlayerId(0),
            format!("Forest #{i}"),
            Zone::Library,
        );
        let obj = state.objects.get_mut(&land).unwrap();
        obj.card_types.core_types = vec![CoreType::Land];
        obj.card_types.supertypes.push(Supertype::Basic);
        lands.push(land);
    }

    // All three opponents took the offer.
    state.players_zone_changed_this_way.insert(PlayerId(1));
    state.players_zone_changed_this_way.insert(PlayerId(2));
    state.players_zone_changed_this_way.insert(PlayerId(3));

    let ability = make_step_4_ability();
    let mut events: Vec<GameEvent> = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 1).unwrap();

    // Three iterations: P0 picks one land per iteration.
    for (i, &land) in lands.iter().take(3).enumerate() {
        let result = apply(
            &mut state,
            PlayerId(0),
            GameAction::SelectCards { cards: vec![land] },
        )
        .unwrap_or_else(|e| panic!("iteration {i} apply failed: {e:?}"));
        events.extend(result.events);
    }

    // Three lands moved to battlefield, one remains in library.
    assert_eq!(
        state.objects.get(&lands[0]).unwrap().zone,
        Zone::Battlefield
    );
    assert_eq!(
        state.objects.get(&lands[1]).unwrap().zone,
        Zone::Battlefield
    );
    assert_eq!(
        state.objects.get(&lands[2]).unwrap().zone,
        Zone::Battlefield
    );
    assert_eq!(
        state.objects.get(&lands[3]).unwrap().zone,
        Zone::Library,
        "fourth land must remain in P0's library — only 3 iterations \
         should run (one per accepting opponent)"
    );
    assert!(state.pending_repeat_iteration.is_none());
}
