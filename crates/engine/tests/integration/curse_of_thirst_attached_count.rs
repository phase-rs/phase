//! CR 303.4b + CR 301.5: Curse of Thirst's "the number of Curses attached to
//! them" quantity clause.
//!
//! Oracle text (verified against MTGJSON, printings DKA/PRM/PW12):
//!   "Enchant player
//!    At the beginning of enchanted player's upkeep, this Aura deals damage
//!    to that player equal to the number of Curses attached to them."
//!
//! Building block under test: `FilterProp::AttachedToPlayer { player:
//! ControllerRef::EnchantedPlayer }` — a player-referent counterpart of the
//! existing object-referent `AttachedToSource`/`AttachedToRecipient` props,
//! composed into `QuantityRef::ObjectCount` over a `Curse`-subtype
//! `TargetFilter`. `curse_upkeep_triggers.rs` already proves the upkeep
//! TRIGGER fires (CR 503.1); these tests prove the damage AMOUNT the
//! resolved effect deals scales with the number of Curses attached to the
//! enchanted player, and only that player.
//!
//! Curse of Thirst is itself a Curse attached to the enchanted player, so the
//! minimum reachable count through its own trigger is 1 (itself) — there is
//! no in-game state where Curse of Thirst is on the battlefield, enchanting a
//! player, and the count is 0. The pure "zero Curses" case is covered instead
//! by a building-block-level unit test on `FilterProp::AttachedToPlayer`
//! directly in `crates/engine/src/game/filter.rs`.
//!
//! `curse_of_surveillance_target_exclusion_fails_closed` below pins a sibling
//! parser-honesty regression on the same "Curses attached to a player" family:
//! Curse of Surveillance's "any number of target players other than that
//! player" (CR 115.1d) has no player-scoped counterpart to
//! `FilterProp::Another` yet (issue #8581), and the parser must fail closed to
//! `Effect::Unimplemented` rather than silently reporting a bare
//! `target=player` filter — dropping the "other than that player" exclusion
//! — as fully supported.

use engine::game::effects::attach::attach_to_player;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::trigger_index::reindex_object_triggers;
use engine::parser::parse_oracle_text;
use engine::types::ability::Effect;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const CURSE_OF_THIRST: &str = "Enchant player\nAt the beginning of enchanted player's upkeep, this Aura deals damage to that player equal to the number of Curses attached to them.";

/// Add a bare, ability-less Curse Aura (e.g. Curse of Vitality's namesake
/// subtype without any of its rules text) to the battlefield under `player`'s
/// control, attached to `attached_to`. Used to pad the Curse count without
/// pulling in another Curse's own trigger body.
fn add_bare_curse(scenario: &mut GameScenario, controller: PlayerId, name: &str) -> ObjectId {
    let mut builder = scenario.add_creature(controller, name, 0, 0);
    builder.as_enchantment();
    builder.with_subtypes(vec!["Aura", "Curse"]);
    builder.id()
}

/// Build Curse of Thirst on the battlefield under P0's control, enchanting
/// P1, plus `extra_same` additional bare Curses also attached to P1 and
/// `extra_other` additional bare Curses attached to P0 instead (to prove the
/// count is scoped to the enchanted player, not a global Curse tally).
/// Starts at `Phase::Untap` so `advance_to_upkeep` drives into P1's upkeep.
fn setup(extra_same: u32, extra_other: u32) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);

    let curse_id = {
        let mut builder =
            scenario.add_creature_from_oracle(P0, "Curse of Thirst", 0, 0, CURSE_OF_THIRST);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.id()
    };

    let mut same_player_curses = Vec::new();
    for i in 0..extra_same {
        same_player_curses.push(add_bare_curse(
            &mut scenario,
            P0,
            &format!("Extra Curse Same {i}"),
        ));
    }
    let mut other_player_curses = Vec::new();
    for i in 0..extra_other {
        other_player_curses.push(add_bare_curse(
            &mut scenario,
            P0,
            &format!("Extra Curse Other {i}"),
        ));
    }

    // Library padding so advance_until_stack_empty doesn't deck anyone.
    for _ in 0..20 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let mut runner = scenario.build();

    // Set P1 as active player (it's their turn / their upkeep). Also force
    // the stale build-time `waiting_for` (still pointed at P0, the scenario's
    // default starting player) to agree with the new active/priority player —
    // CR 117.3a grants priority to the active player, and the engine only
    // re-derives `waiting_for` at a FRESH priority window, not retroactively
    // when a test hand-overrides `active_player`. Without this, priority
    // resolution silently stalls with P0 "holding" a priority window that no
    // longer matches the turn structure, and the upkeep trigger never
    // resolves (mirrors the established fixture pattern in
    // `game/derived_views.rs`'s cross-player priority handoff tests).
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    attach_to_player(runner.state_mut(), curse_id, P1);
    reindex_object_triggers(runner.state_mut(), curse_id);

    for id in same_player_curses {
        attach_to_player(runner.state_mut(), id, P1);
    }
    for id in other_player_curses {
        attach_to_player(runner.state_mut(), id, P0);
    }
    evaluate_layers(runner.state_mut());

    (runner, curse_id)
}

/// Curse of Thirst alone (no other Curses) counts ITSELF — it is a Curse
/// attached to the enchanted player — so exactly 1 damage is dealt.
#[test]
fn curse_of_thirst_counts_itself_when_alone() {
    let (mut runner, _curse_id) = setup(0, 0);

    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        19,
        "with only Curse of Thirst attached to P1, the count of Curses \
         attached to them is 1 (itself), so exactly 1 damage must be dealt"
    );
}

/// A second Curse attached to the SAME enchanted player raises the count to 2.
#[test]
fn curse_of_thirst_counts_a_second_curse_on_the_same_player() {
    let (mut runner, _curse_id) = setup(1, 0);

    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        18,
        "Curse of Thirst plus one more Curse attached to P1 must deal 2 \
         damage (itself + the extra Curse)"
    );
}

/// A third Curse (three total) raises the count to 3 — proves the count
/// scales rather than saturating at 2.
#[test]
fn curse_of_thirst_counts_three_curses_on_the_same_player() {
    let (mut runner, _curse_id) = setup(2, 0);

    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        17,
        "Curse of Thirst plus two more Curses attached to P1 must deal 3 \
         damage total"
    );
}

/// CR 303.4b: a Curse attached to a DIFFERENT player must NOT be counted.
/// Curse of Thirst enchants P1 with one extra Curse also on P1 (count 2), and
/// a THIRD Curse is attached to P0 instead — the P0-attached Curse must be
/// excluded from P1's count.
///
/// Mutation guard: if the "attached to <player>" scoping were dropped (e.g.
/// counting every Curse on the battlefield regardless of who it enchants),
/// this would deal 3 damage instead of 2.
#[test]
fn curse_of_thirst_excludes_curses_attached_to_a_different_player() {
    let (mut runner, _curse_id) = setup(1, 1);

    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        18,
        "the Curse attached to P0 must not count toward P1's total — only \
         Curse of Thirst itself and the one other Curse attached to P1 (2 \
         total) should be counted"
    );
    assert_eq!(
        runner.life(P0),
        20,
        "P0 is not the enchanted player and Curse of Thirst does not target \
         them, so P0 must take no damage from this resolution"
    );
}

/// Real Oracle text (verified against MTGJSON, printing PCY):
///   "Enchant player
///    At the beginning of enchanted player's upkeep, any number of target
///    players other than that player each draw cards equal to the number of
///    Curses attached to that player."
const CURSE_OF_SURVEILLANCE: &str = "Enchant player
At the beginning of enchanted player's upkeep, any number of target players other than that player each draw cards equal to the number of Curses attached to that player.";

/// CR 115.1d (issue #8581): the "other than that player" exclusion has no
/// player-scoped counterpart to `FilterProp::Another` yet. Before the
/// fail-closed guard in `parser::oracle_effect::subject`, the coordinated
/// player/opponent target arm in `parser::oracle_target` matched the bare
/// "player" tag against the plural "players", left "other than that player"
/// as an unconsumed remainder, and the "any number of target X" subject
/// application silently discarded that remainder — producing a bare
/// `target=player` filter and reporting the card as fully supported despite
/// dropping the exclusion entirely. The card must now fail closed to
/// `Effect::Unimplemented` instead.
#[test]
fn curse_of_surveillance_target_exclusion_fails_closed() {
    let parsed = parse_oracle_text(
        CURSE_OF_SURVEILLANCE,
        "Curse of Surveillance",
        &[],
        &["Enchantment".to_string()],
        &["Aura".to_string(), "Curse".to_string()],
    );

    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.execute.is_some())
        .expect("Curse of Surveillance's upkeep trigger must parse");
    let execute = trigger
        .execute
        .as_ref()
        .expect("the upkeep trigger must have an execute ability");

    assert!(
        matches!(&*execute.effect, Effect::Unimplemented { .. }),
        "the 'other than that player' exclusion must fail closed to          Effect::Unimplemented, not silently succeed as a bare target=player          filter; got {:?}",
        execute.effect
    );
}
