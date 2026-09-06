//! Curse of Vengeance — CR 603.10f player-loss look-back for an Aura swept by
//! CR 704.5m in the same state-based-action pass as the loss it observes.
//!
//! SCOPE NOTE — the 2-player case is deliberately out of scope. When only two
//! players remain, `engine_priority.rs`'s `GameOver` path discards the event
//! batch and `elimination.rs` tears down the trigger scaffolding, so no
//! observer can be admitted. This is CR 104.1-defensible ("A game ends
//! immediately when a player wins"), so a payoff that would resolve after the
//! game has already ended has no observable effect. Every runtime test here is
//! therefore a 3-player game. This is a known, intentional blank — not an
//! oversight and not a regression.
//!
//! CR references:
//!   - CR 104.3b: A player at 0 or less life loses the game (a state-based action).
//!   - CR 303.4b: The player an Aura is attached to is called "enchanted".
//!   - CR 400.7: An object changing zones becomes a new object with no memory.
//!   - CR 603.10f: Abilities that trigger when a player loses the game look back in time.
//!   - CR 608.2h: Last-known information is the authority once the object has moved.
//!   - CR 704.3: State-based actions are checked in one pass before priority.
//!   - CR 704.5m: An Aura attached to an illegal player is put into its owner's graveyard.

use std::collections::HashMap;

use engine::game::effects::attach::attach_to_player;
use engine::game::layers::evaluate_layers;
use engine::game::sba::check_state_based_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::trigger_index::reindex_object_triggers;
use engine::game::triggers::{drain_order_triggers_with_identity, process_triggers};
use engine::parser::parse_oracle_text;
use engine::types::ability::{AbilityDefinition, Effect, TargetFilter};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

/// VERBATIM Oracle text (verified against Scryfall). A paraphrase can take a
/// different parser branch and let this suite go green while the real card
/// stays broken.
const CURSE_OF_VENGEANCE_ORACLE: &str = "Enchant player\n\
     Whenever enchanted player casts a spell, put a spite counter on this Aura.\n\
     When enchanted player loses the game, you gain X life and draw X cards, where X is the number of spite counters on this Aura.";

/// A2-class control card exercising the already-shipped "a player loses the
/// game" actor form — a DIFFERENT `TargetFilter` from `AttachedTo`.
const ANY_PLAYER_LOSS_ORACLE: &str = "Enchant player\n\
     When a player loses the game, you gain 1 life.";

/// Parse an Aura's Oracle text with the card types the real card carries, so
/// the parse takes the same branch production does.
fn parse_curse(oracle: &str, name: &str) -> engine::parser::oracle::ParsedAbilities {
    parse_oracle_text(
        oracle,
        name,
        &[],
        &["Enchantment".to_string()],
        &["Aura".to_string(), "Curse".to_string()],
    )
}

fn spite() -> CounterType {
    CounterType::Generic("spite".to_string())
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner.state().players[player.0 as usize].life
}

fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn live_attached_to(
    runner: &GameRunner,
    id: ObjectId,
) -> Option<engine::game::game_object::AttachTarget> {
    runner.state().objects[&id].attached_to
}

fn player_lost_emitted(events: &[GameEvent], player: PlayerId) -> bool {
    events
        .iter()
        .any(|e| matches!(e, GameEvent::PlayerLost { player_id } if *player_id == player))
}

/// Stage `count` spite counters on `id`.
fn stage_spite(runner: &mut GameRunner, id: ObjectId, count: u32) {
    runner
        .state_mut()
        .objects
        .get_mut(&id)
        .expect("curse object must exist")
        .counters
        .insert(spite(), count);
}

fn live_spite(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&spite())
        .copied()
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// A1 — parser row (U1)
// ---------------------------------------------------------------------------

/// A1 / NAMED FIX: "When enchanted player loses the game, …" must parse to
/// `TriggerMode::LosesGame` with `valid_target == Some(TargetFilter::AttachedTo)`.
///
/// REVERT-FAILING ASSERTIONS: `mode == LosesGame` and
/// `valid_target == Some(AttachedTo)`. Before U1 this line produced
/// `TriggerMode::Unknown("When enchanted player loses the game")` with
/// `valid_target == None`, so BOTH flip on revert.
///
/// REACH-GUARD: the whole-card parse must still yield exactly 2 triggers and
/// line 2 must still be the `SpellCast` + `AttachedTo` + put-counter trigger —
/// so an A1 pass can never be a whole-card parse failure in disguise.
#[test]
fn a1_enchanted_player_loses_the_game_parses_to_attached_to() {
    let parsed = parse_curse(CURSE_OF_VENGEANCE_ORACLE, "Curse of Vengeance");

    assert_eq!(
        parsed.triggers.len(),
        2,
        "REACH-GUARD: Curse of Vengeance must parse to exactly 2 triggered abilities"
    );

    // Reach-guard: line 2 (already working before this change) must not regress.
    let spell_cast = &parsed.triggers[0];
    assert_eq!(
        spell_cast.mode,
        TriggerMode::SpellCast,
        "REACH-GUARD: line 2 must remain a SpellCast trigger"
    );
    assert_eq!(
        spell_cast.valid_target,
        Some(TargetFilter::AttachedTo),
        "REACH-GUARD: line 2 must remain scoped to the enchanted player"
    );

    // The fix itself.
    let loses_game = &parsed.triggers[1];
    assert_eq!(
        loses_game.mode,
        TriggerMode::LosesGame,
        "CR 104.3b: 'When enchanted player loses the game' must parse as LosesGame, \
         not TriggerMode::Unknown"
    );
    assert_eq!(
        loses_game.valid_target,
        Some(TargetFilter::AttachedTo),
        "CR 303.4b: the actor 'enchanted player' must map to TargetFilter::AttachedTo"
    );
}

/// A1 SIBLINGS: the three pre-existing loses-game actors must not regress when
/// the fourth arm is added to the same `alt()`. Pins arm ordering.
#[test]
fn a1_sibling_loses_game_actors_still_parse() {
    let cases: Vec<(&str, TargetFilter)> = vec![
        (
            "When a player loses the game, you gain 1 life.",
            TargetFilter::Player,
        ),
        (
            "When you lose the game, you gain 1 life.",
            TargetFilter::Controller,
        ),
    ];

    for (text, expected) in cases {
        let parsed = parse_curse(text, "Sibling Actor Probe");
        assert_eq!(
            parsed.triggers.len(),
            1,
            "sibling actor line must still parse to one trigger: {text}"
        );
        assert_eq!(
            parsed.triggers[0].mode,
            TriggerMode::LosesGame,
            "sibling actor must still be a LosesGame trigger: {text}"
        );
        assert_eq!(
            parsed.triggers[0].valid_target,
            Some(expected),
            "sibling actor must keep its existing TargetFilter: {text}"
        );
    }

    // "an opponent" maps to a Typed filter with an Opponent controller ref; assert
    // structurally rather than reconstructing the whole TypedFilter.
    let opponent = parse_curse(
        "When an opponent loses the game, you gain 1 life.",
        "Opponent Actor Probe",
    );
    assert_eq!(opponent.triggers.len(), 1);
    assert_eq!(opponent.triggers[0].mode, TriggerMode::LosesGame);
    assert!(
        matches!(
            opponent.triggers[0].valid_target,
            Some(TargetFilter::Typed(_))
        ),
        "'an opponent loses the game' must keep its Typed(Opponent) filter"
    );
}

/// A1 ADJACENT-NEGATIVE: `all_consuming` must keep an unrelated "enchanted
/// creature" line out of the loses-game grammar.
///
/// REACH-GUARD (non-vacuous): this must parse as the existing attached-creature
/// `ChangesZone` trigger, so a `LosesGame` absence here is a real grammar
/// boundary, not a dead parser.
#[test]
fn a1_enchanted_creature_dies_is_not_a_loses_game_trigger() {
    let parsed = parse_curse(
        "When enchanted creature dies, you gain 1 life.",
        "Adjacent Negative Probe",
    );
    assert_eq!(
        parsed.triggers.len(),
        1,
        "REACH-GUARD: the adjacent negative must parse to one trigger"
    );
    assert_eq!(
        parsed.triggers[0].mode,
        TriggerMode::ChangesZone,
        "REACH-GUARD: 'enchanted creature dies' must retain its existing ChangesZone parse"
    );
    assert!(
        parsed
            .triggers
            .iter()
            .all(|t| t.mode != TriggerMode::LosesGame),
        "'enchanted creature dies' must NOT be admitted by the loses-game actor grammar"
    );
}

// ---------------------------------------------------------------------------
// Runtime harness (U2)
// ---------------------------------------------------------------------------

struct CurseSpec {
    controller: PlayerId,
    attach_to: PlayerId,
    oracle: &'static str,
    spite: u32,
    /// CR 111.1 + CR 704.5d: when true the Aura is a TOKEN, so the CR 704.5m
    /// sweep to the graveyard is followed — in the SAME state-based-action pass,
    /// before triggers are collected — by the token ceasing to exist, which
    /// REMOVES it from `state.objects` entirely. This is the object-EXISTENCE
    /// axis, distinct from the attachment-IDENTITY axis the other hostile
    /// fixtures probe.
    token: bool,
}

struct RuntimeFixture {
    runner: GameRunner,
    curses: Vec<ObjectId>,
    /// The unrelated co-departing Aura on a Bear (hostile fixture (i)).
    unrelated_aura: ObjectId,
    events: Vec<GameEvent>,
    attached_before: HashMap<ObjectId, Option<engine::game::game_object::AttachTarget>>,
}

/// Build a 3-player board, stage every curse, then drive the REAL SBA path by
/// dropping P2 to 0 life. Also stages an unrelated Aura attached to a 2/0 Bear
/// so a hostile co-departing permanent is present in the same pass.
fn run_player_loss_pass(specs: &[CurseSpec]) -> RuntimeFixture {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let mut curses = Vec::new();
    for (idx, spec) in specs.iter().enumerate() {
        let id = {
            let mut builder =
                scenario.add_creature(spec.controller, &format!("Curse of Vengeance {idx}"), 0, 0);
            builder.as_enchantment();
            builder.with_subtypes(vec!["Aura", "Curse"]);
            builder.from_oracle_text(spec.oracle);
            builder.id()
        };
        curses.push(id);
    }

    // Hostile fixture (i): an unrelated Aura attached to a 2/0 Bear. The Bear
    // dies to the CR 704.5f toughness SBA in the SAME pass, so the Aura
    // co-departs — but its `record.attached_to` is an Object, never
    // `Player(P2)`, which is exactly the discrimination the new arm relies on.
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 0).id();
    let unrelated_aura = {
        let mut builder = scenario.add_creature(P0, "Unrelated Aura", 0, 0);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura"]);
        builder.from_oracle_text(CURSE_OF_VENGEANCE_ORACLE);
        builder.id()
    };

    // Deep libraries so the mandatory draws never deck anyone out.
    for _ in 0..30 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
        scenario.add_card_to_library_top(P2, "Plains");
    }

    let mut runner = scenario.build();

    for (id, spec) in curses.iter().zip(specs.iter()) {
        attach_to_player(runner.state_mut(), *id, spec.attach_to);
    }
    // Attach the unrelated Aura to the Bear (an OBJECT host, not a player).
    {
        let aura = runner
            .state_mut()
            .objects
            .get_mut(&unrelated_aura)
            .expect("unrelated aura must exist");
        aura.attached_to = Some(engine::game::game_object::AttachTarget::Object(bear));
    }

    evaluate_layers(runner.state_mut());
    for id in &curses {
        reindex_object_triggers(runner.state_mut(), *id);
    }
    reindex_object_triggers(runner.state_mut(), unrelated_aura);

    for (id, spec) in curses.iter().zip(specs.iter()) {
        stage_spite(&mut runner, *id, spec.spite);
    }
    stage_spite(&mut runner, unrelated_aura, 7);

    // CR 111.1: mark the token Auras. `GameScenario` has no token-Aura
    // constructor, so this sets `GameObject::is_token` directly after
    // `build()` — the same pattern the rest of the integration suite uses to
    // make a staged permanent a token. It is set AFTER `stage_spite` and
    // BEFORE the SBA pass so the CR 704.5m sweep moves a fully staged,
    // attached, and counter-bearing token Aura; CR 704.5d then makes it cease
    // to exist.
    for (id, spec) in curses.iter().zip(specs.iter()) {
        if spec.token {
            runner
                .state_mut()
                .objects
                .get_mut(id)
                .expect("curse object must exist")
                .is_token = true;
        }
    }

    // REACH-GUARD: every curse really is on the battlefield with its counters
    // staged BEFORE the pass, so a later +0 can never be blamed on an empty
    // counter stack or a card that never hit the battlefield.
    for (id, spec) in curses.iter().zip(specs.iter()) {
        assert_eq!(
            zone_of(&runner, *id),
            Zone::Battlefield,
            "REACH-GUARD: curse must start on the battlefield"
        );
        assert_eq!(
            live_spite(&runner, *id),
            spec.spite,
            "REACH-GUARD: staged spite count must be live on the object pre-SBA"
        );
        assert!(
            live_attached_to(&runner, *id).is_some(),
            "REACH-GUARD: curse must start attached"
        );
        assert_eq!(
            runner.state().objects[id].is_token,
            spec.token,
            "REACH-GUARD: the requested CR 111.1 token-ness must be live on the object \
             pre-SBA, so a token row can never silently degrade into the non-token case"
        );
    }

    let attached_before: HashMap<_, _> = curses
        .iter()
        .chain(std::iter::once(&unrelated_aura))
        .map(|id| (*id, live_attached_to(&runner, *id)))
        .collect();

    // Drive the REAL CR 104.3b state-based loss.
    runner.state_mut().players[2].life = 0;

    let mut events = Vec::new();
    check_state_based_actions(runner.state_mut(), &mut events);
    process_triggers(runner.state_mut(), &events);
    drain_order_triggers_with_identity(runner.state_mut());
    runner.advance_until_stack_empty();

    RuntimeFixture {
        runner,
        curses,
        unrelated_aura,
        events,
        attached_before,
    }
}

// ---------------------------------------------------------------------------
// A3 — the discriminating runtime row (U2 + U1)
// ---------------------------------------------------------------------------

/// A3 / THE DISCRIMINATING ROW: a real, `from_oracle_text`-parsed Curse of
/// Vengeance attached to P2 with 3 spite counters, swept to the graveyard by
/// CR 704.5m in the SAME SBA pass in which P2 loses, must still fire and pay
/// out its LKI spite count.
///
/// REVERT-FAILING ASSERTIONS: `P0 life + 3` and `P0 hand + 3`. The pre-fix
/// baseline was probe-measured at `+0/+0`.
///
/// REACH-GUARDS (mandatory, so a `+0/+0` can never be blamed on a dead
/// fixture): the Curse's zone IS `Graveyard`, a `GameEvent::PlayerLost{P2}` WAS
/// emitted, and the staged spite count was 3 on the live object pre-SBA
/// (asserted inside `run_player_loss_pass`).
#[test]
fn a3_curse_of_vengeance_pays_out_lki_spite_when_enchanted_player_loses() {
    let fixture = run_player_loss_pass(&[CurseSpec {
        controller: P0,
        attach_to: P2,
        oracle: CURSE_OF_VENGEANCE_ORACLE,
        spite: 3,
        token: false,
    }]);

    let curse = fixture.curses[0];

    // REACH-GUARDS.
    assert!(
        player_lost_emitted(&fixture.events, P2),
        "REACH-GUARD: the real SBA pass must have emitted PlayerLost{{P2}}"
    );
    assert_eq!(
        zone_of(&fixture.runner, curse),
        Zone::Graveyard,
        "REACH-GUARD: CR 704.5m must have swept the Curse to the graveyard"
    );

    // THE FIX.
    assert_eq!(
        life(&fixture.runner, P0),
        20 + 3,
        "CR 603.10f: the controller must gain X = 3 (the LKI spite count)"
    );
    assert_eq!(
        hand_size(&fixture.runner, P0),
        3,
        "CR 603.10f: the controller must draw X = 3 cards"
    );
}

/// A3 HOSTILE (i): the unrelated co-departing Aura attached to a dying 2/0 Bear
/// must NOT fire for P2's loss. Its `record.attached_to` is
/// `AttachTarget::Object(..)`, never `Player(P2)`.
///
/// REACH-GUARD (non-vacuous): the same pass is asserted to have paid the REAL
/// Curse its +3/+3, proving the admission arm ran and fired at all — so the
/// unrelated Aura's 7 spite counters being absent from the totals is a real
/// discrimination, not a dead pass.
#[test]
fn a3_hostile_unrelated_co_departing_aura_does_not_fire_for_player_loss() {
    let fixture = run_player_loss_pass(&[CurseSpec {
        controller: P0,
        attach_to: P2,
        oracle: CURSE_OF_VENGEANCE_ORACLE,
        spite: 3,
        token: false,
    }]);

    // REACH-GUARD: the real Curse DID fire in this very pass.
    assert_eq!(
        life(&fixture.runner, P0),
        20 + 3,
        "REACH-GUARD: the genuinely-attached Curse must have paid out, proving the pass fired"
    );

    // The unrelated Aura co-departed (Bear died to the 0-toughness SBA) but is
    // attached to an OBJECT, so it must contribute nothing.
    assert_eq!(
        zone_of(&fixture.runner, fixture.unrelated_aura),
        Zone::Graveyard,
        "REACH-GUARD: the unrelated Aura really did co-depart in this pass"
    );
    assert_eq!(
        hand_size(&fixture.runner, P0),
        3,
        "the unrelated Aura's 7 spite counters must NOT be added: only the Curse attached \
         to the LOSING player is admitted (3, not 10)"
    );
}

/// A3 HOSTILE (ii) + (iii): a Curse attached to SURVIVING P1 must not fire for
/// P2's loss — and after the pass every Aura's live `attached_to` must equal its
/// pre-pass value, proving the helper's RESTORE ran and no LKI leaked.
///
/// REACH-GUARD (non-vacuous): the P2-attached Curse in the same pass IS asserted
/// to have paid out its own count, so P1's Curse contributing nothing is a real
/// per-player discrimination.
#[test]
fn a3_hostile_curse_on_surviving_player_does_not_fire_and_lki_is_restored() {
    let fixture = run_player_loss_pass(&[
        CurseSpec {
            controller: P0,
            attach_to: P2,
            oracle: CURSE_OF_VENGEANCE_ORACLE,
            spite: 3,
            token: false,
        },
        CurseSpec {
            controller: P1,
            attach_to: P1,
            oracle: CURSE_OF_VENGEANCE_ORACLE,
            spite: 5,
            token: false,
        },
    ]);

    // REACH-GUARD: the P2-attached Curse fired.
    assert_eq!(
        life(&fixture.runner, P0),
        20 + 3,
        "REACH-GUARD: the Curse attached to the LOSING player must have paid out"
    );

    // Hostile (ii)/(iii): the Curse attached to surviving P1 must be silent.
    assert_eq!(
        life(&fixture.runner, P1),
        20,
        "a Curse attached to a SURVIVING player must not fire for another player's loss"
    );
    assert_eq!(
        hand_size(&fixture.runner, P1),
        0,
        "a Curse attached to a SURVIVING player must not draw"
    );

    // Hostile (iii): the RESTORE ran — no LKI `attached_to` leaked into later passes.
    for (id, before) in &fixture.attached_before {
        let after = live_attached_to(&fixture.runner, *id);
        if zone_of(&fixture.runner, *id) == Zone::Battlefield {
            assert_eq!(
                after, *before,
                "an Aura still on the battlefield must keep its pre-pass attached_to \
                 (the LKI window must have been restored)"
            );
        } else {
            assert!(
                after.is_none(),
                "an Aura that left the battlefield must NOT retain a leaked LKI \
                 attached_to after the pass"
            );
        }
    }
}

/// A3 HOSTILE (iv) / MULTI-AUTHORITY: two Curses simultaneously attached to P2
/// with DIFFERENT spite counts under DIFFERENT controllers. Each controller must
/// gain and draw THEIR OWN count. A binding resolved through any shared or
/// global authority collapses these to equal values and fails here.
#[test]
fn a3_hostile_two_curses_on_the_losing_player_pay_their_own_controllers() {
    let fixture = run_player_loss_pass(&[
        CurseSpec {
            controller: P0,
            attach_to: P2,
            oracle: CURSE_OF_VENGEANCE_ORACLE,
            spite: 3,
            token: false,
        },
        CurseSpec {
            controller: P1,
            attach_to: P2,
            oracle: CURSE_OF_VENGEANCE_ORACLE,
            spite: 6,
            token: false,
        },
    ]);

    assert!(
        player_lost_emitted(&fixture.events, P2),
        "REACH-GUARD: the real SBA pass must have emitted PlayerLost{{P2}}"
    );

    assert_eq!(
        life(&fixture.runner, P0),
        20 + 3,
        "P0's Curse had 3 spite counters — P0 gains exactly 3"
    );
    assert_eq!(
        hand_size(&fixture.runner, P0),
        3,
        "P0 draws exactly its own Curse's count"
    );
    assert_eq!(
        life(&fixture.runner, P1),
        20 + 6,
        "P1's Curse had 6 spite counters — P1 gains exactly 6"
    );
    assert_eq!(
        hand_size(&fixture.runner, P1),
        6,
        "P1 draws exactly its own Curse's count"
    );
}

/// A3 HOSTILE (v) / OBJECT-EXISTENCE AXIS: a TOKEN Curse of Vengeance attached
/// to the losing player must still pay out its LKI spite count even though the
/// object no longer exists in `state.objects` when triggers are collected.
///
/// WHY THIS IS A DISTINCT AXIS: hostile fixtures (i)-(iv) all probe the
/// attachment-IDENTITY axis (Object host vs Player host vs surviving player vs
/// multiple controllers) using NON-token Auras, which survive the pass in the
/// graveyard. A token takes a strictly longer path inside the SAME CR 704.3
/// state-based-action pass:
///   1. CR 104.3b eliminates P2 at 0 life.
///   2. CR 704.5m sweeps the now-illegally-attached Aura to its owner's graveyard.
///   3. CR 704.5d then makes the token CEASE TO EXIST, removing it from
///      `state.objects` outright — all before `process_triggers` runs.
///
/// The admission arm's guard therefore sees an ABSENT object, not an
/// off-battlefield one. CR 603.10f still requires the trigger to fire, with the
/// departure record's CR 608.2h last-known information supplying both the
/// attachment identity and the spite count.
///
/// REVERT-FAILING ASSERTIONS: `P0 life == 20 + 4` and `P0 hand == 4`. With the
/// pre-fix guard (`!...is_some_and(|o| o.zone != Zone::Battlefield)`) an absent
/// object returns `false` from `is_some_and`, the `!` turns that into
/// `continue`, and the token Curse is SKIPPED — measured `+0/+0`.
///
/// REACH-GUARDS (so a `+0/+0` can never be blamed on a dead fixture):
///   - `PlayerLost{P2}` was emitted by the real SBA pass;
///   - the staged spite count was live on the object pre-SBA (asserted inside
///     `run_player_loss_pass`), as was its CR 111.1 token-ness;
///   - the token is ABSENT from `state.objects` after the pass — this is what
///     proves the test exercised the absent-object path rather than silently
///     degrading into the already-covered non-token graveyard case.
#[test]
fn a3_hostile_token_curse_ceasing_to_exist_still_pays_out_lki_spite() {
    let fixture = run_player_loss_pass(&[CurseSpec {
        controller: P0,
        attach_to: P2,
        oracle: CURSE_OF_VENGEANCE_ORACLE,
        spite: 4,
        token: true,
    }]);

    let curse = fixture.curses[0];

    // REACH-GUARD: the real CR 104.3b state-based loss happened.
    assert!(
        player_lost_emitted(&fixture.events, P2),
        "REACH-GUARD: the real SBA pass must have emitted PlayerLost{{P2}}"
    );

    // REACH-GUARD / THE AXIS ITSELF: CR 704.5d removed the token from the game
    // state entirely. If this ever became `true`, the test would have degraded
    // into the non-token case that hostile fixture (i)-(iv) already cover, and
    // the payout assertions below would prove nothing new.
    assert!(
        !fixture.runner.state().objects.contains_key(&curse),
        "REACH-GUARD: CR 704.5d must have made the token cease to exist, so the \
         admission arm sees an ABSENT object — this is the whole point of the row"
    );

    // THE FIX: absence must be treated as "not on the battlefield" and admitted.
    assert_eq!(
        life(&fixture.runner, P0),
        20 + 4,
        "CR 603.10f + CR 608.2h: a token Curse swept by CR 704.5m and then ceasing to \
         exist under CR 704.5d must STILL gain its controller X = 4 (the LKI spite count)"
    );
    assert_eq!(
        hand_size(&fixture.runner, P0),
        4,
        "CR 603.10f: the controller must still draw X = 4 cards for a token Curse"
    );
}

// ---------------------------------------------------------------------------
// A4 — class-fix proof (U2 alone, independent of U1)
// ---------------------------------------------------------------------------

/// A4 / CLASS-FIX PROOF: the same repair fixes the ALREADY-SHIPPED
/// "a player loses the game" actor form, which uses `TargetFilter::Player` —
/// a DIFFERENT filter from A3's `AttachedTo`. This exercises the admission arm
/// without the attachment matcher, and fails before / passes after
/// INDEPENDENTLY of U1.
///
/// REACH-GUARDS: `PlayerLost{P2}` emitted and the Aura's zone IS `Graveyard`.
#[test]
fn a4_any_player_loss_aura_swept_in_the_same_pass_still_fires() {
    let fixture = run_player_loss_pass(&[CurseSpec {
        controller: P0,
        attach_to: P2,
        oracle: ANY_PLAYER_LOSS_ORACLE,
        spite: 0,
        token: false,
    }]);

    let aura = fixture.curses[0];

    assert!(
        player_lost_emitted(&fixture.events, P2),
        "REACH-GUARD: the real SBA pass must have emitted PlayerLost{{P2}}"
    );
    assert_eq!(
        zone_of(&fixture.runner, aura),
        Zone::Graveyard,
        "REACH-GUARD: CR 704.5m must have swept the Aura to the graveyard"
    );

    assert_eq!(
        life(&fixture.runner, P0),
        20 + 1,
        "CR 603.10f: a pre-existing 'a player loses the game' Aura swept in the same \
         pass must still gain its controller 1 life (probe-measured 0 triggers pre-fix)"
    );
}

// ---------------------------------------------------------------------------
// A5 — coverage honesty
// ---------------------------------------------------------------------------

/// A5 / COVERAGE HONESTY: Curse of Vengeance moves unsupported → supported.
/// No `Effect::Unimplemented` may remain in either trigger's effect list, no
/// trigger may carry `TriggerMode::Unknown(_)`, and there must be no parse
/// warnings.
///
/// The `Unknown(_)` assertion is load-bearing: the coverage report gates
/// supported-ness on exactly that predicate, so this is the same authority the
/// report uses rather than a proxy.
///
/// REACH-GUARD: asserting `parse_warnings.is_empty()` AND `triggers.len() == 2`
/// together prevents "supported" being achieved by the card silently parsing to
/// fewer abilities.
#[test]
fn a5_curse_of_vengeance_is_fully_supported_with_no_unimplemented_markers() {
    let parsed = parse_curse(CURSE_OF_VENGEANCE_ORACLE, "Curse of Vengeance");

    assert_eq!(
        parsed.triggers.len(),
        2,
        "REACH-GUARD: 'supported' must not be achieved by parsing to fewer abilities"
    );
    assert!(
        parsed.parse_warnings.is_empty(),
        "Curse of Vengeance must parse with no warnings, got: {:?}",
        parsed.parse_warnings
    );

    for (idx, trigger) in parsed.triggers.iter().enumerate() {
        assert!(
            !matches!(trigger.mode, TriggerMode::Unknown(_)),
            "trigger {idx} must not be TriggerMode::Unknown — this is the exact predicate \
             the coverage report gates supported-ness on; got {:?}",
            trigger.mode
        );
        let execute = trigger
            .execute
            .as_deref()
            .unwrap_or_else(|| panic!("trigger {idx} must carry an effect payload"));
        assert!(
            !contains_unimplemented(execute),
            "trigger {idx} must not contain Effect::Unimplemented (including its \
             sub_ability chain)"
        );
    }
}

/// Walk an ability and its whole `sub_ability` chain looking for
/// `Effect::Unimplemented`. Curse of Vengeance's "draw X cards" rides in a
/// sub-ability, so a check that only inspected the head effect would miss it.
fn contains_unimplemented(ability: &AbilityDefinition) -> bool {
    let mut current = Some(ability);
    while let Some(node) = current {
        if matches!(*node.effect, Effect::Unimplemented { .. }) {
            return true;
        }
        current = node.sub_ability.as_deref();
    }
    false
}
