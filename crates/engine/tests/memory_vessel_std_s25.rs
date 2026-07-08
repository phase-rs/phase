//! Memory Vessel (std S25) — runtime tests for the two engine building blocks
//! this card requires, driven through the real resolution / prune / apply
//! pipeline in a MULTIPLAYER (3-player) game.
//!
//! Card: "{T}, Exile this artifact: Each player exiles the top seven cards of
//! their library. Until your next turn, players may play cards they exiled this
//! way, and they can't play cards from their hand. Activate only as a sorcery."
//!
//! Two building blocks under test (both general, not Memory-Vessel-only):
//!   (a) `ProhibitedActivity::ProhibitPlayFromZone { zone }` — deny PLAY (cast
//!       OR land) from a zone. Enforced at the cast legal-actions surface, the
//!       actual cast gate, and the play-land gate.
//!   (b) The untap-step play-permission prune keys "until your next turn" on the
//!       granting effect's controller (`exiled_by_ability_controller`), so a
//!       per-owner grant (`PermissionGrantee::ObjectOwner`) expires at the
//!       ACTIVATOR's next turn, not each grantee's.
//!
//! The parser lowering of the compound sub-clause ("players may play cards they
//! exiled this way, and they can't play cards from their hand") is a separate,
//! currently-honest-red `Effect::unimplemented` item (stop-and-return); these
//! tests construct the effect chain the parser must eventually emit and prove
//! the engine primitives resolve it correctly.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::casting::spell_objects_available_to_cast;
use engine::game::effects::resolve_ability_chain;
use engine::game::layers::prune_until_next_turn_casting_permissions;
use engine::game::scenario::GameScenario;
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ActivationRestriction, CastingPermission, Duration, Effect,
    GameRestriction, PermissionGrantee, PlayerFilter, PlayerScope, ProhibitedActivity,
    QuantityExpr, RestrictionExpiry, RestrictionPlayerScope, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::identifiers::{CardId, ObjectId, TrackedSetId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::CastFrequency;
use engine::types::zones::{EtbTapState, Zone};

const P0: PlayerId = PlayerId(0); // activator
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);

/// A fresh `PlayFromExile` grant matching what `grant_permission::resolve`
/// produces for the "players may play cards they exiled this way" clause: a
/// per-owner grant whose "until your next turn" duration is keyed on the
/// activator via `exiled_by_ability_controller`.
fn play_from_exile_grant() -> CastingPermission {
    CastingPermission::PlayFromExile {
        duration: Duration::UntilNextTurnOf {
            player: PlayerScope::Controller,
        },
        granted_to: PlayerId(0), // placeholder; grant_permission::resolve rebinds to owner
        frequency: CastFrequency::Unlimited,
        source_id: None,
        invalidation: None,
        exiled_by_ability_controller: None,
        mana_spend_permission: None,
        card_filter: None,
        single_use_group: None,
        single_use: false,
        cast_cost_raise: None,
        land_enter_tapped: EtbTapState::Unspecified,
    }
}

/// ExileTop{ScopedPlayer, count} with `player_scope: All`, chained to a
/// per-owner `PlayFromExile` grant on the just-exiled tracked set — the shape
/// the parser produces for "Each player exiles the top N ... players may play
/// cards they exiled this way".
fn exile_and_grant_ability(count: i32) -> AbilityDefinition {
    let grant = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GrantCastingPermission {
            permission: play_from_exile_grant(),
            target: TargetFilter::TrackedSet {
                id: TrackedSetId(0),
            },
            grantee: PermissionGrantee::ObjectOwner,
        },
    );
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::ExileTop {
            player: TargetFilter::ScopedPlayer,
            count: QuantityExpr::Fixed { value: count },
            face_down: false,
        },
    )
    .player_scope(PlayerFilter::All)
    .sub_ability(grant)
}

fn play_grant_for(obj: &engine::game::game_object::GameObject, player: PlayerId) -> bool {
    obj.casting_permissions.iter().any(|p| {
        matches!(
            p,
            CastingPermission::PlayFromExile { granted_to, .. } if *granted_to == player
        )
    })
}

/// Resolve the exile-and-grant chain in a 3-player game; return the built runner
/// so callers can inspect / prune. Each player's library holds `per_lib` cards.
fn resolve_exile_and_grant(per_lib: i32, count: i32) -> engine::game::scenario::GameRunner {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    for &p in &[P0, P1, P2] {
        for i in 0..per_lib {
            scenario.add_card_to_library_top(p, &format!("Lib-{}-{}", p.0, i));
        }
    }
    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.active_player = P0;
    let src = create_object(
        state,
        CardId(9990),
        P0,
        "Memory Vessel".to_string(),
        Zone::Battlefield,
    );
    let def = exile_and_grant_ability(count);
    let resolved = build_resolved_from_def(&def, src, P0);
    let mut events = Vec::new();
    resolve_ability_chain(state, &resolved, &mut events, 0).unwrap();
    runner
}

/// Test 1 + 4: each player exiles their OWN top cards, owned by them, each
/// carrying a per-owner play grant whose provenance is the activator; and the
/// grant is scoped per-player (P1 may play P1's cards, P2 may not).
#[test]
fn each_player_exiles_own_top_cards_with_per_owner_grant_and_scoping() {
    let runner = resolve_exile_and_grant(3, 7);
    let state = runner.state();

    for &p in &[P0, P1, P2] {
        let exiled: Vec<_> = state
            .objects
            .values()
            .filter(|o| o.owner == p && o.zone == Zone::Exile)
            .collect();
        assert_eq!(
            exiled.len(),
            3,
            "player {p:?} should exile their own top 3 (library had 3, count 7 caps at library size)"
        );
        for o in &exiled {
            let (granted_to, provenance) = o
                .casting_permissions
                .iter()
                .find_map(|pm| match pm {
                    CastingPermission::PlayFromExile {
                        granted_to,
                        exiled_by_ability_controller,
                        ..
                    } => Some((*granted_to, *exiled_by_ability_controller)),
                    _ => None,
                })
                .expect("exiled card carries a PlayFromExile grant");
            assert_eq!(
                granted_to, p,
                "grant bound to the card's own owner (CR 108.3)"
            );
            assert_eq!(
                provenance,
                Some(P0),
                "grant provenance is the activator, not the grantee"
            );
        }
    }

    // Scoping (claim 4): P1's exiled cards are castable by P1 but NOT by P2.
    let p1_exiled: Vec<ObjectId> = state
        .objects
        .values()
        .filter(|o| o.owner == P1 && o.zone == Zone::Exile)
        .map(|o| o.id)
        .collect();
    let p1_castable = spell_objects_available_to_cast(state, P1);
    let p2_castable = spell_objects_available_to_cast(state, P2);
    assert!(
        p1_exiled.iter().all(|id| p1_castable.contains(id)),
        "P1 may play the cards P1 exiled this way"
    );
    assert!(
        p1_exiled.iter().all(|id| !p2_castable.contains(id)),
        "P2 may NOT play cards P1 exiled this way (per-player grant scoping)"
    );
}

/// Test 2 (load-bearing for building block b): the per-owner grant expires at
/// the ACTIVATOR's next untap step, not each grantee's.
///
/// Revert-failing: revert the `layers.rs` arm change (key on `granted_to`
/// instead of `exiled_by_ability_controller.unwrap_or(granted_to)`) and the
/// P1-untap prune below removes P1's own grant — the first assertion fails.
#[test]
fn grant_expires_at_activator_next_turn_not_grantee_turn() {
    let mut runner = resolve_exile_and_grant(3, 7);
    let state = runner.state_mut();

    // P1's own next untap step: Memory Vessel's grant must SURVIVE (it lasts
    // until the ACTIVATOR P0's next turn, not P1's).
    prune_until_next_turn_casting_permissions(state, P1);
    assert!(
        state
            .objects
            .values()
            .filter(|o| o.owner == P1 && o.zone == Zone::Exile)
            .all(|o| play_grant_for(o, P1)),
        "P1's play grant must survive P1's own untap (expires at activator P0's turn)"
    );
    assert!(
        state
            .objects
            .values()
            .filter(|o| o.owner == P2 && o.zone == Zone::Exile)
            .all(|o| play_grant_for(o, P2)),
        "P2's play grant likewise survives P1's untap"
    );

    // The activator P0's next untap step: every player's grant now expires.
    prune_until_next_turn_casting_permissions(state, P0);
    assert!(
        state
            .objects
            .values()
            .filter(|o| o.zone == Zone::Exile)
            .all(|o| o
                .casting_permissions
                .iter()
                .all(|p| !matches!(p, CastingPermission::PlayFromExile { .. }))),
        "all players' grants expire at the activator's next turn"
    );
}

/// Test 3 (building block a): during the duration, playing a card FROM HAND is
/// prohibited — for BOTH a cast (legal-actions surface excludes it) AND a land
/// play (real `GameAction::PlayLand` through `apply()` is rejected) — while
/// playing from the granted exile set stays legal (zone discriminator).
///
/// Revert-failing:
///   * revert the `handle_play_land` gate ⇒ the hand land play succeeds ⇒
///     `hand_land_play_rejected` assertion fails.
///   * revert the `spell_objects_available_to_cast` filter line ⇒ the hand
///     spell reappears in the castable set ⇒ `hand_spell_excluded` fails.
#[test]
fn cant_play_from_hand_blocks_cast_and_land_but_allows_exile_play() {
    let mut scenario = GameScenario::new_n_player(3, 11);
    scenario.at_phase(Phase::PreCombatMain);

    // P1's hand: a land and a creature spell (both would normally be playable).
    let hand_land = scenario.add_land_to_hand(P1, "Hand Forest").id();
    let hand_spell = scenario.add_creature_to_hand(P1, "Hand Bear", 2, 2).id();

    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.turn_number = 3;
    state.active_player = P1; // an intervening turn during the activator's cycle
    state.priority_player = P1;
    state.waiting_for = engine::types::game_state::WaitingFor::Priority { player: P1 };
    state.lands_played_this_turn = 0;

    // A land P1 exiled this way (in exile, with a per-owner play grant).
    let exile_land = create_object(
        state,
        CardId(7001),
        P1,
        "Exiled Island".to_string(),
        Zone::Exile,
    );
    {
        let o = state.objects.get_mut(&exile_land).unwrap();
        o.card_types
            .core_types
            .push(engine::types::card_type::CoreType::Land);
        o.base_card_types = o.card_types.clone();
        let mut grant = play_from_exile_grant();
        if let CastingPermission::PlayFromExile { granted_to, .. } = &mut grant {
            *granted_to = P1;
        }
        o.casting_permissions.push(grant);
    }
    // A spell P1 exiled this way (castable from exile).
    let exile_spell = create_object(
        state,
        CardId(7002),
        P1,
        "Exiled Bolt".to_string(),
        Zone::Exile,
    );
    {
        let o = state.objects.get_mut(&exile_spell).unwrap();
        o.card_types
            .core_types
            .push(engine::types::card_type::CoreType::Instant);
        o.base_card_types = o.card_types.clone();
        let mut grant = play_from_exile_grant();
        if let CastingPermission::PlayFromExile { granted_to, .. } = &mut grant {
            *granted_to = P1;
        }
        o.casting_permissions.push(grant);
    }

    // Install the "players can't play cards from their hand" restriction via the
    // real AddRestriction resolution path (fills expiry from the duration).
    let src = create_object(
        state,
        CardId(9991),
        P0,
        "Memory Vessel".to_string(),
        Zone::Battlefield,
    );
    let restriction_def = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::AddRestriction {
            restriction: GameRestriction::ProhibitActivity {
                source: ObjectId(0),
                affected_players: RestrictionPlayerScope::AllPlayers,
                expiry: RestrictionExpiry::EndOfTurn,
                activity: ProhibitedActivity::ProhibitPlayFromZone { zone: Zone::Hand },
            },
        },
    )
    .duration(Duration::UntilNextTurnOf {
        player: PlayerScope::Controller,
    });
    let resolved = build_resolved_from_def(&restriction_def, src, P0);
    let mut events = Vec::new();
    resolve_ability_chain(state, &resolved, &mut events, 0).unwrap();

    // The restriction expires at the activator's next turn (not end of turn).
    assert!(
        state.restrictions.iter().any(|r| matches!(
            r,
            GameRestriction::ProhibitActivity {
                expiry: RestrictionExpiry::UntilPlayerNextTurn { player },
                activity: ProhibitedActivity::ProhibitPlayFromZone { zone: Zone::Hand },
                ..
            } if *player == P0
        )),
        "restriction anchors on the activator's next turn"
    );

    // Cast half: the hand spell is excluded from the castable set; the exiled
    // spell is included (zone discriminator).
    let castable = spell_objects_available_to_cast(runner.state(), P1);
    assert!(
        !castable.contains(&hand_spell),
        "hand_spell_excluded: can't cast a spell from hand under the restriction"
    );
    assert!(
        castable.contains(&exile_spell),
        "the card P1 exiled this way stays castable (not from hand)"
    );

    // Land half through the real apply() pipeline.
    let hand_land_card = runner.state().objects[&hand_land].card_id;
    let hand_land_play_rejected = runner
        .act(GameAction::PlayLand {
            object_id: hand_land,
            card_id: hand_land_card,
        })
        .is_err();
    assert!(
        hand_land_play_rejected,
        "hand_land_play_rejected: can't play a land from hand under the restriction"
    );

    // Playing the land P1 exiled this way is still legal (not from hand).
    let exile_land_card = runner.state().objects[&exile_land].card_id;
    let exile_land_result = runner.act(GameAction::PlayLand {
        object_id: exile_land,
        card_id: exile_land_card,
    });
    assert!(
        exile_land_result.is_ok(),
        "the land P1 exiled this way is still playable (zone discriminator): {exile_land_result:?}"
    );
}

/// Card-level parser test: Memory Vessel's full oracle text must lower to a
/// complete effect chain with NO `Effect::Unimplemented`.
///
/// Discriminating: before the parser lowering (Steps 1-4) the second sentence
/// ("Until your next turn, players may play cards they exiled this way, and they
/// can't play cards from their hand") collapsed into a single
/// `Effect::Unimplemented` sub_ability, so the "no Unimplemented", the grant, and
/// the `AddRestriction{ProhibitPlayFromZone{Hand}}` assertions all revert-fail if
/// the compound recognizer or the grant-arm widening is reverted.
#[test]
fn memory_vessel_oracle_text_lowers_fully() {
    let parsed = parse_oracle_text(
        "{T}, Exile this artifact: Each player exiles the top seven cards of their library. Until your next turn, players may play cards they exiled this way, and they can't play cards from their hand. Activate only as a sorcery.",
        "Memory Vessel",
        &[],
        &["Artifact".to_string()],
        &[],
    );
    assert_eq!(parsed.abilities.len(), 1, "one activated ability");
    let head = &parsed.abilities[0];

    // Sorcery-speed activation restriction (CR 602.5d).
    assert!(
        head.activation_restrictions
            .contains(&ActivationRestriction::AsSorcery),
        "activate only as a sorcery, got {:?}",
        head.activation_restrictions
    );

    // Walk head + sub_ability chain into a flat Vec of (effect, owning-def duration).
    let mut chain: Vec<(&Effect, &Option<Duration>)> = Vec::new();
    let mut cursor: Option<&AbilityDefinition> = Some(head);
    while let Some(def) = cursor {
        chain.push((&def.effect, &def.duration));
        cursor = def.sub_ability.as_deref();
    }

    // No honest-red placeholder survives anywhere in the chain.
    assert!(
        !chain
            .iter()
            .any(|(e, _)| matches!(e, Effect::Unimplemented { .. })),
        "chain must not carry Effect::Unimplemented: {chain:#?}"
    );

    // ExileTop head.
    assert!(
        matches!(chain[0].0, Effect::ExileTop { .. }),
        "head is ExileTop, got {:?}",
        chain[0].0
    );

    // Per-owner PlayFromExile grant, until the activator's next turn, bound to
    // the just-exiled tracked set.
    assert!(
        chain.iter().any(|(e, _)| matches!(
            e,
            Effect::GrantCastingPermission {
                permission: CastingPermission::PlayFromExile {
                    duration: Duration::UntilNextTurnOf {
                        player: PlayerScope::Controller
                    },
                    ..
                },
                target: TargetFilter::TrackedSet {
                    id: TrackedSetId(0)
                },
                grantee: PermissionGrantee::ObjectOwner,
            }
        )),
        "chain must carry a per-owner PlayFromExile grant on TrackedSet(0): {chain:#?}"
    );

    // Play-from-hand prohibition for all players, whose owning def carries the
    // shared UntilNextTurnOf{Controller} duration (→ activator-keyed expiry at
    // AddRestriction resolution).
    assert!(
        chain.iter().any(|(e, d)| matches!(
            e,
            Effect::AddRestriction {
                restriction: GameRestriction::ProhibitActivity {
                    affected_players: RestrictionPlayerScope::AllPlayers,
                    activity: ProhibitedActivity::ProhibitPlayFromZone { zone: Zone::Hand },
                    ..
                },
            }
        ) && matches!(
            d,
            Some(Duration::UntilNextTurnOf {
                player: PlayerScope::Controller
            })
        )),
        "chain must carry ProhibitPlayFromZone{{Hand}} for AllPlayers with UntilNextTurnOf duration: {chain:#?}"
    );
}
