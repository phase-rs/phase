//! Regression: "Until end of turn, you may cast spells from your hand without
//! paying their mana costs" (Chandra, Flame's Catalyst's ultimate) is a
//! PLAYER-scoped permission over a set the game keeps re-reading — Omniscience
//! for a turn — not a resolution-time pick (CR 608.2g) and not a bundle of
//! per-object permissions either.
//!
//! THE DEFECT, as reported from play. The clause lowered to
//! `Effect::CastFromZone`, and `cast_from_zone::resolve`'s empty-target tail
//! asked only "does this filter name the hand?" — if so it opened
//! `WaitingFor::EffectZoneChoice { count: 1, up_to: true }`, and answering that
//! with one card runs `complete_hand_pick_cast_from_zone`, which casts it AS THE
//! ABILITY RESOLVES. The printed turn collapsed into a single immediate cast.
//!
//! WHY A PER-OBJECT PERMISSION IS ALSO WRONG, which is what makes this a
//! promotion rather than a reroute. `CastingPermission`s are stamped once per
//! card at resolution; `CastFromZoneDriver::for_batch_bounds`' own capability
//! table says that mechanism "writes an INDEPENDENT `CastingPermission` per
//! object". A card DRAWN
//! LATER in the same turn would therefore never be covered, and the printed
//! effect covers it. `a_turn_long_hand_grant_covers_a_card_drawn_after_it_resolved`
//! is that row, and it is the discriminating test of this change.
//!
//! THE MECHANISM ALREADY EXISTS AND THIS SENTENCE ALREADY LOWERS TO IT.
//! `StaticMode::CastFromHandFree` is what a permanent printing the same words as
//! a printed static gets (Omniscience, the Tamiyo emblem), built by
//! `oracle_static::restriction::try_parse_cast_free_permission`. Chandra's line
//! is that sentence with a lifetime in front, so `apply_duration_to_effect` — the
//! one seam holding the effect and the stated lifetime at the same time —
//! promotes the grant into it and carries it as a duration-bound player grant:
//! the shape `effects/effect.rs` installs for `MayLookAtFaceDown` (Lumbering
//! Laundry) and `casting.rs` reads for `CastWithKeyword` (Teferi, Time Raveler).
//!
//! THE SOURCE IS GONE BY THE TIME THE ABILITY RESOLVES. Paying `[-8]` from
//! loyalty 8 leaves zero loyalty and CR 704.5i puts Chandra in the graveyard
//! before her own ability resolves, so a grant bound to her battlefield presence
//! would never exist. The transient carrier survives that by construction, and
//! the sibling test asserts her zone before measuring the permission.
//!
//! WHY THE ZONE CANNOT DECIDE IT. The corpus prints hand-origin cast grants on
//! 49 cards, and the zone is identical across all of them: Electrodominance's
//! class ("you may cast a spell with mana value X or less from your hand without
//! paying its mana cost", `DuringResolution`), the hand-bound "from among those
//! cards" anaphor (Silent-Blade Oni, Mindclaw Shaman, Mindleech Mass — whose
//! `CastMechanism::ResolutionTimePrivateZonePick` deliberately shares the
//! `LingeringPermission` driver value), the full-cost "as though they were the
//! card ..." grants, The Face of Boe's borrowed suspend cost, and Chandra. Of
//! those, Chandra's is the one clause that names a lifetime AND a free cast,
//! which is the pair CR 611.2a turns into a later priority window (CR 117.1a).
//! (Karlach, Tiefling Spellrager prints that pair too, but her grant binds
//! `ParentTarget` rather than a zone, so she is not in this set and the
//! promotion's `TargetFilter::Typed` gate declines her — see below.) Measured
//! against the base, Chandra is the only card in the corpus whose parse this
//! change moves.
//!
//! NOT REPAIRED, measured and stated rather than implied:
//!
//!   * Sen Triplets keeps the per-object mechanism, and this change leaves it
//!     byte-identical to `main` (measured): "you may play lands and cast spells
//!     from that player's hand this turn" is a FULL-COST grant, and
//!     `CastFromHandFree` means exactly "without paying the mana cost", so the
//!     promotion declines it. Its own defect is separate and untouched — its
//!     filter carries no `ControllerRef`, so `compute_hand_pick_eligible` falls
//!     back to the ability's controller and reads the CONTROLLER's hand rather
//!     than the chosen opponent's (#7674).
//!   * Karlach, Tiefling Spellrager is untested rather than established as
//!     broken: her grant carries `target: ParentTarget`, which the promotion's
//!     `TargetFilter::Typed` gate declines, so she never reaches it. Whether the
//!     specialize chain binds her sought card was not measured either way.
//!   * The plural exile-origin grants (Dream Harvest, and Ugin, Eye of the
//!     Storms) and the cards whose grant clause the resolver never usefully
//!     reaches — Thranduil's Decree (#7132) and Kheru Spellsnatcher never reach
//!     it at all, Planeswalker's Mischief reaches it with an empty tracked set —
//!     are untouched here; they are named in the sibling module.

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::Duration;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::PlayerId;

/// Verbatim Oracle text (`client/public/card-data.json`, key
/// `chandra, flame's catalyst`).
const CHANDRA_FLAMES_CATALYST: &str = "[+1]: Chandra deals 3 damage to each opponent.\n\
     [\u{2212}2]: You may cast target red instant or sorcery card from your graveyard. If that \
     spell would be put into your graveyard, exile it instead.\n\
     [\u{2212}8]: Discard your hand, then draw seven cards. Until end of turn, you may cast \
     spells from your hand without paying their mana costs.";

fn can_cast(runner: &GameRunner, id: ObjectId) -> bool {
    legal_actions(runner.state())
        .iter()
        .any(|action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == id))
}

fn recorded_permission_durations(runner: &GameRunner, id: ObjectId) -> Vec<Option<Duration>> {
    runner.state().objects[&id]
        .casting_permissions
        .iter()
        .map(|permission| permission.lifetime().duration.cloned())
        .collect()
}

fn hand_of(runner: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player present")
        .hand
        .iter()
        .copied()
        .collect()
}

/// The end-to-end regression. CR 611.2a + CR 117.1a: the ultimate's stated
/// "Until end of turn" means the seven drawn cards are cast at LATER priority
/// windows, so resolution must offer no pick and every one of them must carry
/// the turn-long permission.
///
/// DISCRIMINATING, and measured both ways: with the resolver's zone-only
/// routing restored, resolution stops at
/// `WaitingFor::EffectZoneChoice { effect_kind: CastFromZone, zone: Hand }` and
/// the first two assertions fail. The per-card permission assertion is the one
/// that pins the WIDTH of the repair — a fix that granted only the chosen card
/// would pass "no prompt" and fail here.
///
/// The cleanup assertion is the counter-direction: an over-broad degrade that
/// outlived its printed turn fails there rather than passing vacuously.
#[test]
fn a_turn_long_hand_cast_grant_is_not_a_resolution_time_pick() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // CR 104.3c: the ultimate draws seven and the expiry assertion below is made
    // after a full turn cycle, so both libraries must outlast the run.
    for i in 0..40 {
        scenario.add_card_to_library_top(engine::game::scenario::P1, &format!("P1 Library {i}"));
    }
    // EVERY drawn card costs {6}, and the controller has no lands and no mana.
    // `GameScenario`'s own filler cards cost {0} (`ManaCost::default()` is
    // `zero()`), so a `can_cast` assertion over them is true whether or not any
    // permission exists — measured: an earlier draft of the sibling test below
    // stayed green with the promotion removed for exactly that reason. With a
    // real cost, `can_cast` here can only be true because the permission zeroed
    // it, and the same is true of the expiry assertion at the end.
    for i in 0..40 {
        scenario
            .add_spell_to_library_top(P0, &format!("P0 Costly {i}"), false)
            .with_mana_cost(ManaCost::Cost {
                generic: 6,
                shards: vec![],
            });
    }
    let chandra = scenario
        .add_planeswalker_from_oracle(
            P0,
            "Chandra, Flame's Catalyst",
            "Chandra",
            8,
            CHANDRA_FLAMES_CATALYST,
        )
        .id();
    let mut runner = scenario.build();

    // Loyalty ability index 2 is the [-8] ultimate.
    runner.activate(chandra, 2).resolve();
    runner.advance_until_stack_empty();

    // The `EffectZoneChoice` arm is the discriminating one: with the promotion
    // neutralized the run stops there, which is the offer the field report
    // described. The `OptionalEffectChoice` arm has never fired for this clause
    // on either side — measured, both parse dumps carry `"optional": false` —
    // and is kept only so a change that re-promotes the printed "may" is caught
    // here rather than in a playtest. Named rather than left to look
    // load-bearing.
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::EffectZoneChoice { .. } | WaitingFor::OptionalEffectChoice { .. }
        ),
        "CR 611.2a: a stated lifetime means a later priority window, so resolution must \
         offer no cast pick; got {}",
        runner.waiting_for_kind()
    );

    let hand = hand_of(&runner, P0);
    assert_eq!(
        hand.len(),
        7,
        "reach guard: the ultimate must have discarded and drawn seven, got {hand:?}"
    );
    for &card in &hand {
        assert_eq!(
            zone_of(&runner, card),
            Zone::Hand,
            "the permission is exercised from the hand; no card may be moved by the grant"
        );
        assert!(
            can_cast(&runner, card),
            "CR 601.2b: every card in the hand must be castable under the turn-long \
             permission"
        );
    }
    // CR 611.2a: the permission is player-scoped and lives on no card. Asserted
    // rather than assumed, because the per-object shape is exactly what this
    // change replaced: a `CastingPermission` stamped on the seven cards would
    // pass the castability check above and still be the wrong model — it cannot
    // cover a card drawn later, which the sibling test measures.
    for &card in &hand {
        assert!(
            recorded_permission_durations(&runner, card).is_empty(),
            "the grant must not stamp a per-object permission; got {:?}",
            recorded_permission_durations(&runner, card)
        );
    }

    // CR 514.2: the counter-direction — the permission must still expire.
    let turn_of_the_grant = runner.state().turn_number;
    //
    // THE PRIORITY GUARD BELOW IS THE POINT. `can_cast` reads `legal_actions`,
    // which is scoped to the `WaitingFor::Priority { player }` holder, and
    // `advance_to_phase` stops at the FIRST matching phase — the OPPONENT's
    // precombat main after this cleanup. Asserting `!can_cast` there is true
    // whatever the permission did, so the run continues to the controller's own
    // next main phase and refuses to assert until priority is actually theirs.
    // Walking by UPKEEP and MAIN alternately, and both halves are needed.
    // CR 514.3: NORMALLY no player receives priority during the cleanup step, so
    // `advance_to_phase(Cleanup)` does not observe that phase and burns whole
    // turns looking for it. The discard itself is the CR 514.1 turn-based action;
    // a priority window opens only under CR 514.3a, and either way the run stops
    // at the prompt, which is what `settle_cleanup_discards` answers. And
    // `advance_to_upkeep` is a no-op when the run is already standing in an
    // upkeep. Measured: each failure mode on its own skipped the
    // controller's next main phase every time.
    let reached_own_next_main = |runner: &GameRunner| {
        runner.state().turn_number > turn_of_the_grant
            && runner.state().active_player == P0
            && runner.state().phase == Phase::PreCombatMain
            && matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0)
    };
    for _ in 0..8 {
        settle_cleanup_discards(&mut runner);
        if reached_own_next_main(&runner) {
            break;
        }
        runner.advance_to_phase(Phase::PreCombatMain);
        settle_cleanup_discards(&mut runner);
        if reached_own_next_main(&runner) {
            break;
        }
        runner.advance_to_upkeep();
    }
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
        "reach guard: the expiry must be measured in a live game, not after a player decked out"
    );
    // The FULL loop condition, not just its priority half. The two differ, and the
    // difference is a silent green: the library cards are sorceries, so
    // `!can_cast` is trivially true in the OPPONENT's turn whether or not the
    // permission expired. Asserting `reached_own_next_main` makes the loop's own
    // exit condition the thing that has to hold.
    assert!(
        reached_own_next_main(&runner),
        "reach guard: the expiry assertion below only measures anything on the grant \
         holder's own later turn while they hold priority; got turn {} active {:?} \
         phase {:?} waiting {}",
        runner.state().turn_number,
        runner.state().active_player,
        runner.state().phase,
        runner.waiting_for_kind()
    );
    let survivors: Vec<ObjectId> = hand_of(&runner, P0)
        .into_iter()
        .filter(|card| hand.contains(card))
        .collect();
    assert!(
        !survivors.is_empty(),
        "reach guard: cards from the granted hand must still be in hand at the assertion"
    );
    for &card in &survivors {
        assert!(
            !can_cast(&runner, card),
            "CR 514.2: the turn-long permission must end at the cleanup step"
        );
    }
}

/// Verbatim Oracle text (`client/public/card-data.json`, key `divination`).
const DIVINATION: &str = "Draw two cards.";

/// THE POINT OF THE WHOLE CHANGE. CR 611.2a: the printed effect is "Until end of
/// turn, you may cast spells from your hand" — the hand, continuously, not the
/// seven cards that happened to be there when the ability resolved.
///
/// A per-object `CastingPermission` cannot express that: it is stamped once per
/// card at resolution (`CastFromZoneDriver::for_batch_bounds`' capability table:
/// "writes an INDEPENDENT `CastingPermission` per object"). So the ultimate is
/// promoted to
/// `StaticMode::CastFromHandFree` — Omniscience's mechanism — carried as a
/// player-scoped transient continuous effect whose affected filter is re-read at
/// every cast attempt.
///
/// The discriminating step is the SECOND free cast: Divination is drawn by the
/// first one, so it was not in hand when the ultimate resolved and no per-object
/// grant could have reached it. It must still be castable for zero.
///
/// THE {6} COST IS LOAD-BEARING, and this test was WRONG without it. The
/// scenario's own filler cards cost {0}, so an earlier draft asserted `can_cast`
/// on cards that were castable anyway and stayed green with the promotion
/// removed — measured. The drawn cards now cost {6} and the controller has no
/// lands and no mana, so `can_cast` on them can only be true because the
/// permission zeroed the cost.
#[test]
fn a_turn_long_hand_grant_covers_a_card_drawn_after_it_resolved() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..12 {
        scenario.add_card_to_library_top(engine::game::scenario::P1, &format!("P1 Library {i}"));
    }
    // Library order matters and the helpers PREPEND, so this block reads
    // bottom-first: the two cards Divination will draw go in before the six
    // fillers, and Divination itself goes in last so it lands on top.
    let costly: Vec<ObjectId> = (0..2)
        .map(|i| {
            scenario
                .add_spell_to_library_top(P0, &format!("Costly Draw {i}"), false)
                .with_mana_cost(ManaCost::Cost {
                    generic: 6,
                    shards: vec![],
                })
                .id()
        })
        .collect();
    for i in 0..6 {
        scenario.add_card_to_library_top(P0, &format!("P0 Filler {i}"));
    }
    let divination = scenario
        .add_spell_to_library_top(P0, "Divination", false)
        .from_oracle_text(DIVINATION)
        .id();
    let chandra = scenario
        .add_planeswalker_from_oracle(
            P0,
            "Chandra, Flame's Catalyst",
            "Chandra",
            8,
            CHANDRA_FLAMES_CATALYST,
        )
        .id();
    let mut runner = scenario.build();

    runner.activate(chandra, 2).resolve();
    runner.advance_until_stack_empty();

    // CR 704.5i: paying [-8] from loyalty 8 leaves zero loyalty, so Chandra is in
    // the graveyard by now. The permission must not have died with her — that is
    // why it is a player-scoped transient effect and not a static on her, and it
    // is asserted rather than assumed.
    assert_eq!(
        zone_of(&runner, chandra),
        Zone::Graveyard,
        "reach guard: the ultimate's own cost must have killed Chandra (CR 704.5i), \
         so the permission is being measured without its source"
    );

    let hand_before = hand_of(&runner, P0);
    assert!(
        hand_before.contains(&divination),
        "reach guard: Divination must be among the seven drawn, got {hand_before:?}"
    );

    // Cast Divination for free, which draws two cards that were NOT in hand when
    // the ultimate resolved.
    runner.cast(divination).free_cast().commit();
    runner.advance_until_stack_empty();

    let mut drawn: Vec<ObjectId> = hand_of(&runner, P0)
        .into_iter()
        .filter(|card| !hand_before.contains(card))
        .collect();
    drawn.sort();
    let mut expected = costly.clone();
    expected.sort();
    assert_eq!(
        drawn, expected,
        "reach guard: Divination must have drawn exactly the two costly cards that \
         were not in the hand the ultimate saw"
    );
    for &card in &drawn {
        assert!(
            can_cast(&runner, card),
            "CR 611.2a: the printed permission covers the hand for the whole turn, so \
             a {{6}} card drawn after it resolved must be castable with no mana available"
        );
    }
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner
        .state()
        .objects
        .get(&id)
        .expect("object present")
        .zone
}

/// CR 514.1: seven cards plus the draw for turn exceeds the maximum hand size, so
/// the cleanup step's turn-based action stops the run for a discard. Answering it is how the run reaches the
/// controller's own next main phase; it is not part of any claim, and it also
/// unblocks `advance_to_phase`, which stops as soon as `waiting_for` is not
/// `Priority`.
fn settle_cleanup_discards(runner: &mut GameRunner) {
    for _ in 0..8 {
        let WaitingFor::DiscardToHandSize {
            count, ref cards, ..
        } = runner.state().waiting_for.clone()
        else {
            return;
        };
        let chosen: Vec<ObjectId> = cards.iter().take(count).copied().collect();
        runner
            .act(GameAction::SelectCards { cards: chosen })
            .expect("the cleanup discard must be accepted");
    }
}
