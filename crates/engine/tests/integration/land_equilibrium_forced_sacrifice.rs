//! Runtime pipeline regression for Land Equilibrium (misparse-backlog category
//! #4 — conjoined/chained second-effect clause dropped).
//!
//! Land Equilibrium (Legends): "If an opponent who controls at least as many
//! lands as you do would put a land onto the battlefield, that player instead
//! puts that land onto the battlefield then sacrifices a land of their choice."
//!
//! These tests drive the REAL replacement pipeline: they parse the card's exact
//! Oracle text through `engine::parser::parse_oracle_text` (so Part 1's new
//! `parse_opponent_who_controls_at_least_as_many` combinator and Part 2's
//! dispatch branch are exercised), attach the resulting `ReplacementDefinition`
//! to a Land Equilibrium enchantment on P0's battlefield, and then have an
//! opponent PLAY A LAND through `GameAction::PlayLand`. The forced sacrifice is
//! observed as a real `WaitingFor::EffectZoneChoice` for the ENTERING opponent,
//! gated by the scoped-player land-count comparison (Part 4 — `ScopedPlayer`
//! resolves against the entering land's controller).
//!
//! NOTE on Part 3: the sacrifice binds to the ENTERING opponent (not the caster)
//! because the land-drop epilogue and the general zone-change drain both rebind
//! the post-replacement continuation to the entering object at drain time. That
//! binding is PRE-EXISTING; Part 3's stash-source change is defensive alignment
//! and is NOT observable on the land-drop path (all of these tests pass with
//! Part 3 reverted). Part 4's `scoped_player` threading, by contrast, IS uniquely
//! discriminating here — see the per-test notes.
//!
//! Discriminating signals:
//!   * (a) positive / (c) boundary: the sacrifice `EffectZoneChoice.player` is the
//!     ENTERING OPPONENT and its eligible pool is that opponent's lands only.
//!   * (b) negative / (d) three-player ungated: NO `EffectZoneChoice` — reverting
//!     Part 4's `scoped_player` threading makes the condition compare the caster's
//!     land count against itself (`ScopedPlayer` falls back to the source
//!     controller ⇒ always equal ⇒ GE true), wrongly forcing a sacrifice here.

use engine::game::effects::change_zone;
use engine::game::engine::apply_as_current;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Effect, QuantityExpr, QuantityRef,
    ReplacementDefinition, ResolvedAbility, TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);

/// Verbatim Oracle text (matches `card-data.json`'s `"land equilibrium"` entry).
const LAND_EQUILIBRIUM_TEXT: &str = "If an opponent who controls at least as many lands as you do would put a land onto the battlefield, that player instead puts that land onto the battlefield then sacrifices a land of their choice.";

/// Parse Land Equilibrium's real Oracle text and return its single replacement.
/// Exercises the new parser combinator + dispatch branch end-to-end.
fn land_equilibrium_replacement() -> ReplacementDefinition {
    let parsed =
        engine::parser::parse_oracle_text(LAND_EQUILIBRIUM_TEXT, "Land Equilibrium", &[], &[], &[]);
    assert_eq!(
        parsed.replacements.len(),
        1,
        "Land Equilibrium must parse to exactly one replacement; got {:?}",
        parsed.replacements
    );
    assert!(
        !parsed
            .parse_warnings
            .iter()
            .any(|w| format!("{w:?}").contains("SwallowedClause")),
        "Land Equilibrium must no longer emit a SwallowedClause warning; got {:?}",
        parsed.parse_warnings
    );
    parsed.replacements.into_iter().next().unwrap()
}

/// Create a Land permanent named `name` on the battlefield controlled by `owner`.
fn add_battlefield_land(state: &mut GameState, owner: PlayerId, name: &str) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, owner, name.to_string(), Zone::Battlefield);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Land);
    obj.base_card_types = obj.card_types.clone();
    obj.entered_battlefield_turn = Some(state.turn_number.saturating_sub(1));
    obj.summoning_sick = false;
    id
}

/// Create the Land Equilibrium enchantment on P0's battlefield carrying the
/// real parsed replacement.
fn add_land_equilibrium(state: &mut GameState) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(
        state,
        card_id,
        P0,
        "Land Equilibrium".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Enchantment);
    obj.base_card_types = obj.card_types.clone();
    obj.entered_battlefield_turn = Some(state.turn_number.saturating_sub(1));
    obj.summoning_sick = false;
    obj.replacement_definitions = vec![land_equilibrium_replacement()].into();
    id
}

/// Put a Land card named `name` into `player`'s hand. Returns its object id.
fn add_land_to_hand(state: &mut GameState, player: PlayerId, name: &str) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, player, name.to_string(), Zone::Hand);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Land);
    obj.base_card_types = obj.card_types.clone();
    id
}

/// Make `player` the active player with priority in their pre-combat main phase.
fn give_turn(runner: &mut GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.phase = Phase::PreCombatMain;
    state.active_player = player;
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

/// Play the land `land_id` from `actor`'s hand.
fn play_land(runner: &mut GameRunner, land_id: ObjectId) {
    let card_id = runner.state().objects[&land_id].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land_id,
            card_id,
        })
        .expect("actor should be able to play a land");
}

fn lands_in_graveyard(state: &GameState, player: PlayerId) -> Vec<ObjectId> {
    state.players[player.0 as usize]
        .graveyard
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|o| o.card_types.core_types.contains(&CoreType::Land))
        })
        .collect()
}

/// (a) Positive: an opponent controlling MORE lands than the caster plays a land
/// — the land enters AND that opponent is forced to sacrifice a land of THEIR
/// choice (not the caster's).
#[test]
fn opponent_with_more_lands_is_forced_to_sacrifice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();

    let eq_id = add_land_equilibrium(runner.state_mut());
    // Caster (P0) controls 1 land; opponent (P1) controls 2 → GE holds.
    let p0_land = add_battlefield_land(runner.state_mut(), P0, "Plains");
    let p1_land_a = add_battlefield_land(runner.state_mut(), P1, "Forest");
    let p1_land_b = add_battlefield_land(runner.state_mut(), P1, "Island");
    let hand_land = add_land_to_hand(runner.state_mut(), P1, "Mountain");

    give_turn(&mut runner, P1);
    play_land(&mut runner, hand_land);

    // The land entered the battlefield.
    assert_eq!(
        runner.state().objects[&hand_land].zone,
        Zone::Battlefield,
        "the played land must still enter the battlefield (it is put on, THEN a sacrifice follows)"
    );

    // The forced sacrifice must prompt the ENTERING OPPONENT (P1), not the caster.
    let WaitingFor::EffectZoneChoice {
        player,
        cards,
        count,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected the opponent's forced-sacrifice EffectZoneChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        *player, P1,
        "Land Equilibrium forces the ENTERING opponent (P1) to sacrifice, NOT the caster (P0)"
    );
    assert_eq!(*count, 1, "exactly one land must be sacrificed");
    // Eligible pool is P1's lands only (including the just-entered one); never P0's.
    for c in cards {
        assert_eq!(
            runner.state().objects[c].controller,
            P1,
            "the sacrifice pool must contain only the entering opponent's lands; {c:?} is not P1's"
        );
    }
    assert!(
        !cards.contains(&p0_land),
        "the caster's land ({p0_land:?}) must NEVER be a sacrifice candidate; pool={cards:?}"
    );
    assert!(
        cards.contains(&p1_land_a) && cards.contains(&p1_land_b) && cards.contains(&hand_land),
        "P1's three lands must all be eligible; pool={cards:?}"
    );

    // P1 sacrifices Forest.
    runner
        .act(GameAction::SelectCards {
            cards: vec![p1_land_a],
        })
        .expect("opponent sacrifices a land");

    assert_eq!(
        runner.state().objects[&p1_land_a].zone,
        Zone::Graveyard,
        "the opponent's chosen land is sacrificed to the graveyard"
    );
    assert_eq!(
        runner.state().objects[&p0_land].zone,
        Zone::Battlefield,
        "the CASTER's land is untouched — only the opponent sacrifices"
    );
    assert!(
        lands_in_graveyard(runner.state(), P0).is_empty(),
        "no caster land may end up in the caster's graveyard"
    );
    // Sanity reach-guard: the replacement really fired (the enchantment exists and
    // the entering land is present), so the negative assertions above are not vacuous.
    assert!(runner.state().objects.contains_key(&eq_id));
}

/// (b) Negative: an opponent controlling FEWER lands than the caster plays a land
/// — it enters normally with NO forced sacrifice.
#[test]
fn opponent_with_fewer_lands_plays_land_normally() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();

    add_land_equilibrium(runner.state_mut());
    // Caster (P0) controls 3 lands; opponent (P1) controls 1 → GE fails.
    add_battlefield_land(runner.state_mut(), P0, "Plains");
    add_battlefield_land(runner.state_mut(), P0, "Island");
    add_battlefield_land(runner.state_mut(), P0, "Swamp");
    add_battlefield_land(runner.state_mut(), P1, "Forest");
    let hand_land = add_land_to_hand(runner.state_mut(), P1, "Mountain");

    give_turn(&mut runner, P1);
    play_land(&mut runner, hand_land);

    // Reach-guard: the land actually entered (input got past the replacement gate).
    assert_eq!(
        runner.state().objects[&hand_land].zone,
        Zone::Battlefield,
        "the land must enter normally"
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::EffectZoneChoice { .. }
        ),
        "no forced sacrifice may occur when the opponent controls fewer lands; got {:?}",
        runner.state().waiting_for
    );
    assert!(
        lands_in_graveyard(runner.state(), P1).is_empty(),
        "the opponent must not have sacrificed any land"
    );
}

/// (c) Boundary: the opponent's land count EXACTLY equals the caster's (GE, not
/// GT) — the sacrifice still triggers.
#[test]
fn equal_land_counts_still_force_sacrifice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();

    add_land_equilibrium(runner.state_mut());
    // Both control 2 lands at the moment the land would enter → 2 >= 2 holds.
    add_battlefield_land(runner.state_mut(), P0, "Plains");
    add_battlefield_land(runner.state_mut(), P0, "Island");
    let p1_land_a = add_battlefield_land(runner.state_mut(), P1, "Forest");
    let p1_land_b = add_battlefield_land(runner.state_mut(), P1, "Mountain");
    let hand_land = add_land_to_hand(runner.state_mut(), P1, "Swamp");

    give_turn(&mut runner, P1);
    play_land(&mut runner, hand_land);

    let WaitingFor::EffectZoneChoice { player, cards, .. } = &runner.state().waiting_for else {
        panic!(
            "equal land counts (GE boundary) must still force a sacrifice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        *player, P1,
        "the entering opponent sacrifices at the GE boundary"
    );
    assert!(
        cards.contains(&p1_land_a) && cards.contains(&p1_land_b) && cards.contains(&hand_land),
        "all of the opponent's lands are eligible; pool={cards:?}"
    );

    runner
        .act(GameAction::SelectCards {
            cards: vec![p1_land_b],
        })
        .expect("opponent sacrifices at the boundary");
    assert_eq!(runner.state().objects[&p1_land_b].zone, Zone::Graveyard);
}

/// (d) Three-player hostile fixture: the gate binds to the SPECIFIC entering
/// player, not an aggregate across all opponents. P1 (>= caster) is gated; P2
/// (< caster) is NOT — even though P1, another opponent, controls many lands.
#[test]
fn three_player_gate_binds_to_specific_entering_player() {
    // --- Sub-case 1: P1 (many lands) plays → forced sacrifice. ---
    {
        let mut scenario = GameScenario::new_n_player(3, 7);
        scenario.at_phase(Phase::PreCombatMain);
        let mut runner = scenario.build();

        add_land_equilibrium(runner.state_mut());
        // Caster P0: 2 lands. P1: 3 lands (gated). P2: 1 land (ungated).
        add_battlefield_land(runner.state_mut(), P0, "Plains");
        add_battlefield_land(runner.state_mut(), P0, "Island");
        add_battlefield_land(runner.state_mut(), P1, "Forest");
        add_battlefield_land(runner.state_mut(), P1, "Mountain");
        add_battlefield_land(runner.state_mut(), P1, "Swamp");
        add_battlefield_land(runner.state_mut(), P2, "Plains");
        let p1_hand = add_land_to_hand(runner.state_mut(), P1, "Wastes");

        give_turn(&mut runner, P1);
        play_land(&mut runner, p1_hand);

        let WaitingFor::EffectZoneChoice { player, .. } = &runner.state().waiting_for else {
            panic!(
                "P1 (>= caster lands) must be forced to sacrifice, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*player, P1, "the gated opponent P1 sacrifices");
    }

    // --- Sub-case 2: P2 (few lands) plays → NO sacrifice, even though P1 has
    //     many lands. An existential/aggregate check would wrongly fire here. ---
    {
        let mut scenario = GameScenario::new_n_player(3, 7);
        scenario.at_phase(Phase::PreCombatMain);
        let mut runner = scenario.build();

        add_land_equilibrium(runner.state_mut());
        add_battlefield_land(runner.state_mut(), P0, "Plains");
        add_battlefield_land(runner.state_mut(), P0, "Island");
        add_battlefield_land(runner.state_mut(), P1, "Forest");
        add_battlefield_land(runner.state_mut(), P1, "Mountain");
        add_battlefield_land(runner.state_mut(), P1, "Swamp");
        let p2_land = add_battlefield_land(runner.state_mut(), P2, "Plains");
        let p2_hand = add_land_to_hand(runner.state_mut(), P2, "Wastes");

        give_turn(&mut runner, P2);
        play_land(&mut runner, p2_hand);

        // Reach-guard: P2's land entered (past the gate).
        assert_eq!(
            runner.state().objects[&p2_hand].zone,
            Zone::Battlefield,
            "P2's land enters"
        );
        assert!(
            !matches!(
                runner.state().waiting_for,
                WaitingFor::EffectZoneChoice { .. }
            ),
            "P2 controls FEWER lands than the caster, so it must NOT be forced to \
             sacrifice — the gate binds to the entering player, not an aggregate over \
             opponents; got {:?}",
            runner.state().waiting_for
        );
        assert!(
            lands_in_graveyard(runner.state(), P2).is_empty(),
            "ungated P2 must not sacrifice"
        );
        // Reach-guard: P2's pre-existing land is untouched.
        assert_eq!(runner.state().objects[&p2_land].zone, Zone::Battlefield);
    }
}

/// (e) Regression: the Part 3 continuation-source change is a no-op for the
/// Devour family (all `valid_card: SelfRef`, so `rid.source ==
/// affected_object_id`). A Devour-1 creature entering the battlefield must still
/// stash and resolve its post-replacement continuation — sacrifice a creature,
/// then receive one +1/+1 counter (EventContextAmount = 1 creature devoured) —
/// unaffected by the entering-object source rebinding.
#[test]
fn devour_post_replacement_continuation_still_resolves() {
    let mut state = GameState::new_two_player(42);
    state.active_player = P0;
    state.priority_player = P0;

    // The devourer's only legal victim (a pre-existing creature on the battlefield).
    let victim = {
        let id = create_object(
            &mut state,
            CardId(10),
            P0,
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        id
    };

    // A Devour-1 creature waiting in Exile, mirroring `synthesize_devour`:
    // Sacrifice(any number of your creatures) → sub_ability PutCounter(EventContextAmount).
    let devourer = {
        let id = create_object(
            &mut state,
            CardId(20),
            P0,
            "Devourer".to_string(),
            Zone::Exile,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let put_counters = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                target: TargetFilter::SelfRef,
            },
        );
        let sacrifice = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Sacrifice {
                target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
                count: QuantityExpr::up_to(QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: TargetFilter::Typed(
                            TypedFilter::creature().controller(ControllerRef::You),
                        ),
                    },
                }),
                min_count: 0,
            },
        )
        .sub_ability(put_counters);
        let repl = ReplacementDefinition {
            event: ReplacementEvent::Moved,
            execute: Some(Box::new(sacrifice)),
            valid_card: Some(TargetFilter::SelfRef),
            ..ReplacementDefinition::new(ReplacementEvent::Moved)
        };
        state.objects.get_mut(&id).unwrap().replacement_definitions = vec![repl].into();
        id
    };

    let change_zone = ResolvedAbility::new(
        Effect::ChangeZone {
            origin: Some(Zone::Exile),
            destination: Zone::Battlefield,
            target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: engine::types::zones::EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        },
        vec![],
        ObjectId(100),
        P0,
    );

    let mut events = Vec::new();
    change_zone::resolve(&mut state, &change_zone, &mut events).unwrap();

    // The as-enters sacrifice continuation surfaced its prompt (still stashed +
    // drained after the Part 3 change). Its pool excludes the entering devourer.
    let WaitingFor::EffectZoneChoice { player, cards, .. } = &state.waiting_for else {
        panic!(
            "Devour's post-replacement sacrifice continuation must still surface, got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(
        *player, P0,
        "the devourer's controller makes the devour choice"
    );
    assert!(
        cards.contains(&victim) && !cards.contains(&devourer),
        "pool is the controller's creatures excluding the entering devourer; pool={cards:?}"
    );

    // Devour the pre-existing creature.
    apply_as_current(
        &mut state,
        GameAction::SelectCards {
            cards: vec![victim],
        },
    )
    .expect("devour the victim");

    assert_eq!(
        state.objects[&victim].zone,
        Zone::Graveyard,
        "the devoured creature is sacrificed"
    );
    // The chained PutCounter continuation ran: devour-1 of one creature = one +1/+1.
    assert_eq!(
        state.objects[&devourer]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied(),
        Some(1),
        "the devourer must receive exactly one +1/+1 counter from its post-replacement \
         continuation — proving Part 3 did not break the SelfRef stash/resolve path"
    );
}
