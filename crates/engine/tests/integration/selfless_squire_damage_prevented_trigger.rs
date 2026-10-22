//! CR 615.13 — "Whenever damage that would be dealt to you is prevented" must
//! actually fire, and must read the amount of THAT application.
//!
//! Selfless Squire ({3}{W} Creature — Human Soldier 1/1) is the corpus's one
//! standalone prevention-trigger card. At base its second line parsed to
//! `TriggerMode::Unknown` and its matcher was `match_unimplemented` (a hard
//! `return false`), so the trigger never fired even though its execute payload
//! (`PutCounter{P1P1, Ref(EventContextAmount), SelfRef}`) was already correct.
//!
//! CR 615.13: "Some triggered abilities trigger when damage that would be dealt
//! is prevented. Such an ability triggers each time a prevention effect is
//! applied to one or more simultaneous damage events and prevents some or all of
//! that damage."
//!
//! The "once" in `DamagePreventedOnce` is a property of the EVENT STREAM, not of
//! the matcher: the combat path and simultaneous non-combat batches each emit
//! ONE aggregate `DamagePrevented` per shield per simultaneous batch (a paused
//! batch republishes prefix + resumed entry + tail as one application via its
//! pause-owned record). The matcher is therefore a pure per-event predicate
//! and adds no dedup or once-per-turn latch —
//! `two_separate_preventions_each_read_their_own_amount` pins that two
//! separate applications produce two triggers, each reading its own amount.
//!
//! THE COMBAT HALF, and how CR 615.13 and CR 120.3 are BOTH honored. The combat
//! path aggregates a whole CR 510.2 simultaneous batch before emitting, and it
//! used to re-derive the prevented target from the shield's own filter — mapping
//! only the `Specific(PlayerId)` scope to a player, so a controller-scoped
//! ("dealt to you") shield emitted an event naming the shield OBJECT and the
//! matcher correctly refused it under CR 120.3. The recipient is now THREADED
//! from the replacement applier that prevented the damage
//! (`game/replacement.rs`'s tally writes → `PreventedDamageEntry` →
//! `game/combat_damage.rs::fire_batch_prevention_riders`), never re-derived,
//! because a shield's `damage_target_filter` is a PREDICATE over recipients and
//! not the recipient of any one event. So:
//!
//!   * CR 615.13 fixes the TRIGGER/rider unit — one prevention APPLICATION per
//!     simultaneous batch, so the CR 615.5 rider fires once against the batch
//!     total;
//!   * CR 120.3 fixes the RECIPIENT — one `DamagePrevented` per (shield,
//!     recipient), each carrying that recipient's own amount.
//!
//! Safe Passage is the fixture that makes the split visible: one application,
//! two recipients (the player for 2, a creature for 3), and a player-scoped
//! trigger must read 2 — not 5, and not 3.
//!
//! KNOWN RESIDUALS, stated rather than hidden:
//!   * the aggregate's `source_id` is the SHIELD's host object (sentinel
//!     `ObjectId(0)` when floating), not the damage source — a pinned convention,
//!     because CR 615.13's application spans simultaneous events that under
//!     CR 510.2 routinely have DIFFERENT sources. No test in this file may assert
//!     `source_id` as if it were the damage source;
//!   * per-source-reflecting shields (Comeuppance) accumulate their PUBLIC
//!     prevented amount into the batch tally — one `DamagePrevented` per
//!     (shield, recipient) — while each reflection still binds its own damage
//!     source per CR 615.5 (which a batch aggregate cannot carry), draining
//!     inline per event. `reflecting_shields_aggregate_public_event_keep_per_source_reflection`
//!     pins both halves: one public event AND both attackers dealt back their
//!     own damage;
//!   * `PreventionAmount::Next(N)` / `AllBut(N)` likewise enter the tally —
//!     `depletion_shields_aggregate_public_event` pins the `Next(N)` member's
//!     single aggregate event alongside its unchanged depletion arithmetic.
//!     CR 615.7's simultaneous-damage choice is about WHICH damage the shield
//!     prevents, not about emitting per event, so it does not defend the old
//!     fragmented shape.
//!
//! The positive tests are discriminating: reverting the matcher
//! (`match_damage_prevented` → `match_unimplemented`) or the parser arm (mode →
//! `Unknown`) drops every counter assertion to 0. Each negative carries a paired
//! positive reach-guard in the SAME test, because "0 counters" is also what "the
//! trigger was never registered at all" looks like.
//!
//! Oracle text is verbatim from Scryfall.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::combat::AttackTarget;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCondition, AbilityDefinition, AbilityKind, ChosenAttribute, ControllerRef,
    DamageTargetFilter, DamageTargetPlayerScope, EachDamageRecipient, Effect, ObjectScope,
    PreventionAmount, QuantityExpr, QuantityRef, ShieldKind, StaticDefinition, TargetFilter,
    TargetRef, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{PersistedGameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::{MatchedProtectionQuality, ProtectionTarget};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

/// Verbatim Selfless Squire (Scryfall). The second line is the trigger under test.
const SELFLESS_SQUIRE_TEXT: &str = "Flash\nWhen this creature enters, prevent all damage that would be dealt to you this turn.\nWhenever damage that would be dealt to you is prevented, put that many +1/+1 counters on this creature.";

/// Verbatim Solitary Confinement — a printed static shield with no turn window,
/// used as an INDEPENDENT prevention source. Deliberately not the Squire's own
/// ETB shield: the trigger must observe prevention from any source, not only from
/// a shield it created.
const SOLITARY_CONFINEMENT_TEXT: &str = "Skip your draw step.\nAt the beginning of your upkeep, sacrifice Solitary Confinement unless you discard a card.\nPrevent all damage that would be dealt to you.";

/// Verbatim Energy Field (`data/card-data.json`). A PRINTED `Prevention{All}` /
/// `Player{Controller}` shield — the 14-card class the combat aggregate used to
/// mis-attribute to the shield object. (The raw census over the 35,804-card
/// corpus finds 22 such nodes; the other 8 carry a non-null `redirect_target` and
/// are CR 614.9 REDIRECTIONS, not CR 615 preventions — redirection deals the same
/// damage to another recipient, so nothing is prevented and they never reach the
/// tally at all.)
///
/// Chosen over this file's `SOLITARY_CONFINEMENT_TEXT` constant, which is NOT
/// verbatim: it reorders the printed lines, names the card instead of "this
/// enchantment", and drops "You have shroud."
const ENERGY_FIELD_TEXT: &str = "Prevent all damage that would be dealt to you by sources you don't control.\nWhen a card is put into your graveyard from anywhere, sacrifice this enchantment.";

/// Verbatim Safe Passage. THE multi-recipient shield: one CR 615.13 application,
/// a player leg and a creature leg in the same CR 510.2 simultaneous batch.
const SAFE_PASSAGE_TEXT: &str =
    "Prevent all damage that would be dealt to you and creatures you control this turn.";

/// Verbatim Riot Control. A resolution-created player shield: `TargetFilter::
/// Controller` is a context ref, so `untargeted_damage_filter` lowers it to
/// `Player { Specific(P0) }` — the one scope the deleted re-derivation used to
/// map, which is why this row must keep passing.
const RIOT_CONTROL_TEXT: &str = "You gain 1 life for each creature your opponents control. Prevent all damage that would be dealt to you this turn.";

/// Verbatim Fog. `TargetFilter::Any` lowers to NO `damage_target_filter` at all,
/// so there is no scope for a filter-derived recipient to come from — the old
/// path fabricated `Object(ObjectId(0))`, an object that does not exist.
const FOG_TEXT: &str = "Prevent all combat damage that would be dealt this turn.";

/// Verbatim New Way Forward. The chosen-source shield: aimed at the Bear, it
/// makes the Bear event (and only it) a two-shield CR 616.1 ordering pause.
const NEW_WAY_FORWARD_TEXT: &str = "The next time a source of your choice would deal damage to you this turn, prevent that damage. When damage is prevented this way, New Way Forward deals that much damage to that source's controller and you draw that many cards.";

/// Verbatim Abundance. Its optional draw replacement is the rider-created
/// child choice for the C2 drain-hook controls.
const ABUNDANCE_TEXT: &str = "If you would draw a card, you may instead choose land or nonland and reveal cards from the top of your library until you reveal a card of the chosen kind. Put that card into your hand and put all other cards revealed this way on the bottom of your library in any order.";

/// Verbatim Comeuppance. Per-source-reflecting, so it bypasses the batch tally
/// entirely and keeps the pre-existing per-damage-event emission.
const COMEUPPANCE_TEXT: &str = "Prevent all damage that would be dealt to you and planeswalkers you control this turn by sources you don't control. If damage from a creature source is prevented this way, Comeuppance deals that much damage to that creature. If damage from a noncreature source is prevented this way, Comeuppance deals that much damage to the source's controller.";

/// SYNTHETIC minimal depletion shield. No printed card carries exactly this
/// line; the SHAPE is grammar-real — `PreventionAmount::Next` with a
/// controller-relative "dealt to you" target — composed from two parser-handled
/// halves: Test of Faith's "Prevent the next 3 damage" amount grammar and Riot
/// Control's "dealt to you" target grammar. `Next(7)` overcovers the 2-and-3
/// batch with 2 to spare, so no CR 615.7 apportionment choice can be blamed for
/// anything the characterization row observes.
const DEPLETION_BULWARK_TEXT: &str =
    "Prevent the next 7 damage that would be dealt to you this turn.";

fn free_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![],
        generic: 0,
    }
}

/// Stock both libraries. Without this a player decks out, the game ends in
/// `WaitingFor::GameOver`, combat never runs, and EVERY counter assertion passes
/// vacuously at 0. Borrowed from `printed_damage_prevention_survives_turn.rs`.
fn stock_libraries(scenario: &mut GameScenario) {
    scenario.with_library_top(
        P0,
        &["F0a", "F0b", "F0c", "F0d", "F0e", "F0f", "F0g", "F0h"],
    );
    scenario.with_library_top(
        P1,
        &["F1a", "F1b", "F1c", "F1d", "F1e", "F1f", "F1g", "F1h"],
    );
}

/// Put Selfless Squire on the battlefield with its verbatim Oracle text.
fn add_squire(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_creature_from_oracle(player, "Selfless Squire", 1, 1, SELFLESS_SQUIRE_TEXT)
        .id()
}

/// Add a free enchantment carrying a printed static prevention line. A
/// permanent-type seed MUST precede the Oracle text — `parse_oracle_text` given
/// `types: ["Sorcery"]` returns zero replacements for a static prevention line.
fn add_shield_enchantment(
    scenario: &mut GameScenario,
    player: PlayerId,
    name: &str,
    text: &str,
) -> ObjectId {
    scenario
        .add_spell_to_hand(player, name, false)
        .as_enchantment()
        .from_oracle_text(text)
        .with_mana_cost(free_cost())
        .id()
}

/// Add a free INSTANT carrying a prevention line, for the rows that need the
/// shield to exist only after attackers are declared (CR 615.4).
fn add_instant_from_oracle(
    scenario: &mut GameScenario,
    player: PlayerId,
    name: &str,
    text: &str,
) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(player, name, true, text)
        .with_mana_cost(free_cost())
        .id()
}

/// Advance until `target` is the active player, returning whether that happened.
///
/// `GameRunner::advance_to_phase` cannot be used here: it breaks out of its loop
/// on ANY non-`Priority` waiting state, and every test in this file puts a
/// creature (the Squire) on P0's battlefield, so the engine surfaces the CR 508.1
/// declare-attackers turn-based action on P0's own turn and the advance silently
/// stalls at `DeclareAttackers`. This helper answers those turn-based prompts
/// (declaring no attacks and no blocks) so the turn can actually roll over.
///
/// The bool is the stall guard — callers MUST assert it. Every counter assertion
/// downstream reads "N counters", which is also what "the turn never advanced and
/// combat never happened" looks like.
#[must_use = "the turn crossing must be asserted to have happened — see doc comment"]
fn pass_turn_to(runner: &mut GameRunner, target: PlayerId) -> bool {
    for _ in 0..600 {
        if runner.state().active_player == target && runner.state().phase == Phase::PreCombatMain {
            return true;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return false;
                }
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                if runner.act(GameAction::OrderTriggers { order }).is_err() {
                    return false;
                }
            }
            WaitingFor::DeclareAttackers { .. } => {
                if runner.declare_attackers(&[]).is_err() {
                    return false;
                }
            }
            WaitingFor::DeclareBlockers { .. } => {
                if runner.declare_blockers(&[]).is_err() {
                    return false;
                }
            }
            _ => return false,
        }
    }
    false
}

/// Drive combat, declaring `attackers` against `defend_player`, and COLLECT every
/// event the drive produced.
///
/// The single combat driver for this file. It replaced a no-blocks,
/// no-event-log variant (`run_combat_unblocked`) whose only caller was the
/// characterization test that
/// `combat_prevention_from_a_printed_player_shield_puts_that_many_counters`
/// converted; keeping a delegating wrapper with no callers is dead code, so the
/// three capabilities were folded in here instead:
///
///   * every `ActionResult`'s events are appended to a log, so a test can assert
///     the exact `DamagePrevented` list the CR 510.2 batch emitted (length
///     included — a spurious `amount: 0` entry must surface as an extra element);
///   * `WaitingFor::DeclareBlockers` submits `blocks` (`(blocker, attacker)` per
///     CR 509.1a) instead of declaring none, so a batch can carry BOTH a player
///     recipient and a creature recipient;
///   * `flash`, if supplied, is cast at the first priority window seen AFTER
///     attackers were declared — the CR 615.4 window in which an instant-speed
///     prevention shield must exist before the damage event occurs.
///
/// The bool is the same combat reach-guard and MUST be asserted; it additionally
/// covers `flash` having actually been cast.
#[must_use = "combat must be asserted to have actually run — see doc comment"]
fn run_combat_collecting(
    runner: &mut GameRunner,
    attacker_player: PlayerId,
    attackers: &[ObjectId],
    defend_player: PlayerId,
    blocks: &[(ObjectId, ObjectId)],
    flash: Option<ObjectId>,
) -> (bool, Vec<GameEvent>) {
    let mut attacked = false;
    let mut reached_end_of_combat = false;
    let mut blocked = false;
    let mut flashed = flash.is_none();
    let mut log: Vec<GameEvent> = Vec::new();
    // The flash card's controller. `GameRunner::cast` routes through
    // `apply_as_current`, which acts as WHOEVER currently holds priority — and the
    // active player (the attacker) gets priority first in the declare-attackers
    // step. Casting a card out of the defender's hand while the attacker holds
    // priority is refused by `castable_from_current_zone` with
    // `InvalidAction("Card is not in a castable zone")`, so the cast must wait for
    // this player's own window.
    let flash_controller = flash.map(|spell| runner.state().objects[&spell].controller);

    for _ in 0..400 {
        if matches!(
            runner.state().phase,
            Phase::EndCombat | Phase::PostCombatMain
        ) {
            reached_end_of_combat = true;
            break;
        }
        match runner.state().waiting_for.clone() {
            // CR 615.4: the shield has to exist BEFORE the damage event, so it is
            // cast in the first priority window its controller gets after
            // attackers are declared.
            WaitingFor::Priority { player, .. }
                if attacked && !flashed && Some(player) == flash_controller =>
            {
                flashed = true;
                let spell = flash.expect("`flashed` starts true when there is nothing to flash");
                let outcome = runner.cast(spell).resolve();
                log.extend(outcome.events().iter().cloned());
            }
            WaitingFor::Priority { .. } => match runner.act(GameAction::PassPriority) {
                Ok(result) => log.extend(result.events),
                Err(_) => break,
            },
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                match runner.act(GameAction::OrderTriggers { order }) {
                    Ok(result) => log.extend(result.events),
                    Err(_) => break,
                }
            }
            WaitingFor::DeclareAttackers { player, .. }
                if player == attacker_player && !attacked =>
            {
                attacked = true;
                let decl: Vec<_> = attackers
                    .iter()
                    .map(|a| (*a, AttackTarget::Player(defend_player)))
                    .collect();
                let result = runner
                    .declare_attackers(&decl)
                    .expect("declaring the intended attackers must succeed");
                log.extend(result.events);
            }
            WaitingFor::DeclareAttackers { .. } => match runner.declare_attackers(&[]) {
                Ok(result) => log.extend(result.events),
                Err(_) => break,
            },
            WaitingFor::DeclareBlockers { .. } => {
                let assignments: &[(ObjectId, ObjectId)] = if blocked { &[] } else { blocks };
                blocked = true;
                match runner.declare_blockers(assignments) {
                    Ok(result) => log.extend(result.events),
                    Err(_) => break,
                }
            }
            _ => break,
        }
    }

    // `flashed` is part of the reach-guard: a flash that never found its
    // controller's priority window leaves NO shield, and "0 counters" is exactly
    // what that looks like.
    (attacked && reached_end_of_combat && flashed, log)
}

/// Every `DamagePrevented` in `events`, reduced to the two fields this file is
/// allowed to assert.
///
/// `source_id` is deliberately DROPPED: on the combat aggregate it is the SHIELD's
/// host object (the sentinel `ObjectId(0)` for a floating shield), a pinned
/// convention rather than the damage source — see the module header. Asserting it
/// would read as a claim about the damage source that this change does not make.
fn prevented_pairs(events: &[GameEvent]) -> Vec<(TargetRef, u32)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::DamagePrevented { target, amount, .. } => Some((target.clone(), *amount)),
            _ => None,
        })
        .collect()
}

/// `prevented_pairs`, sorted, for rows whose recipients have no required order.
fn sorted_prevented_pairs(events: &[GameEvent]) -> Vec<(TargetRef, u32)> {
    let mut pairs = prevented_pairs(events);
    pairs.sort();
    pairs
}

fn counters_on(runner: &GameRunner, obj: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&obj)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// T1 — non-combat prevention, concrete amount (charter rows 2 + 5)
// ---------------------------------------------------------------------------

/// CR 615.13 + CR 120.3: a burn spell aimed at the Squire's controller is fully
/// prevented by an INDEPENDENT shield; the Squire's trigger fires once and reads
/// the prevented amount.
///
/// Revert-failing: with `match_unimplemented` restored (or the parser arm
/// removed) the Squire gains 0 counters instead of 4. The amount is asserted
/// CONCRETELY (4, not "nonzero"), so a trigger that fires but misreads the amount
/// also fails.
#[test]
fn non_combat_prevention_puts_that_many_counters_on_the_squire() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let squire = add_squire(&mut scenario, P0);
    let shield = add_shield_enchantment(
        &mut scenario,
        P0,
        "Solitary Confinement",
        SOLITARY_CONFINEMENT_TEXT,
    );
    let bolt = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Prevention Probe",
            true,
            "Prevention Probe deals 4 damage to target player.",
        )
        .id();
    let mut runner = scenario.build();

    // The shield is an independent source — the Squire did not create it.
    runner.cast(shield).resolve();
    assert_eq!(
        counters_on(&runner, squire),
        0,
        "reach-guard: no prevention has happened yet, so the Squire has no counters"
    );

    let life_before = runner.state().players[P0.0 as usize].life;
    let outcome = runner.cast(bolt).target_player(P0).resolve();

    // Reach-guard: the damage really was PREVENTED, not merely never dealt.
    outcome.assert_life_delta(P0, 0);
    assert_eq!(
        outcome.state().players[P0.0 as usize].life,
        life_before,
        "the shield must have absorbed all 4 damage"
    );

    // CR 615.13: "that many" is the amount of THIS prevention application.
    outcome.assert_counters(squire, CounterType::Plus1Plus1, 4);
}

// ---------------------------------------------------------------------------
// V1 — combat prevention from a PRINTED player shield (charter rows 2 + 5)
//
// Converted from `combat_prevention_currently_blocked_by_object_targeted_aggregate_event`,
// which characterized the defect this row now proves fixed.
// ---------------------------------------------------------------------------

/// CR 615.13 + CR 120.3: a printed, controller-scoped ("dealt to you") shield
/// prevents a whole simultaneous combat-damage batch, and the aggregate event
/// names the PLAYER whose damage was prevented — so the Squire's trigger fires
/// and reads the batch amount.
///
/// This is the 14-card printed `Prevention{All}` / `Player{Controller}` class.
/// Every one of them parses to `Player { player: Controller }`, NOT `Specific`,
/// and the combat aggregate used to re-derive its target from that filter and map
/// only `Specific(PlayerId)` to a player — so it emitted `TargetRef::Object(the
/// enchantment)` and `match_damage_prevented` correctly refused it under CR 120.3
/// (a player-scoped trigger must not fire on object damage). The matcher was
/// right; the event's target was wrong. The recipient is now threaded from the
/// replacement applier, so the filter is never consulted for it.
///
/// BASE BEHAVIOR this inverts (measured): one event `target=Object(field)`,
/// amount 5, and **0** counters.
///
/// Revert-catches: restoring the `damage_target_filter` re-derivation in
/// `fire_combat_prevention_riders`; any fix that resolves `Controller` to the
/// wrong player.
#[test]
fn combat_prevention_from_a_printed_player_shield_puts_that_many_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let squire = add_squire(&mut scenario, P0);
    let shield = add_shield_enchantment(&mut scenario, P0, "Energy Field", ENERGY_FIELD_TEXT);
    let attacker_a = scenario.add_creature(P1, "Bear A", 2, 2).id();
    let attacker_b = scenario.add_creature(P1, "Bear B", 3, 3).id();
    let mut runner = scenario.build();

    runner.cast(shield).resolve();
    assert_eq!(counters_on(&runner, squire), 0, "reach-guard: pre-combat");

    // Hand the turn to P1 so it can attack.
    let crossed = pass_turn_to(&mut runner, P1);
    assert!(
        crossed,
        "stall guard: the turn must actually have passed to P1"
    );

    // CLASS reach-guard: this row must exercise the previously-broken scope, not
    // accidentally land on the one scope that always worked. A printed
    // "dealt to you" shield is Controller-scoped; `Specific` is never printed.
    assert_eq!(
        runner.state().objects[&shield].replacement_definitions[0].damage_target_filter,
        Some(DamageTargetFilter::Player {
            player: DamageTargetPlayerScope::Controller
        }),
        "the printed 'dealt to you' shield is Controller-scoped, not Specific — \
         this is the scope the combat aggregate used to drop"
    );

    let life_before = runner.state().players[P0.0 as usize].life;
    let (ran, events) =
        run_combat_collecting(&mut runner, P1, &[attacker_a, attacker_b], P0, &[], None);
    assert!(ran, "combat reach-guard: the attack must actually have run");

    // Reach-guard: 5 combat damage really WAS prevented. This is what makes the
    // counter assertion below a statement about the TRIGGER rather than about
    // combat never having happened.
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "all 5 combat damage must have been prevented by the shield"
    );

    // CR 615.13: ONE application over the CR 510.2 batch, so exactly ONE event —
    // and CR 120.3: it names the PLAYER. Exact list, length included, so a
    // per-source split or a spurious zero-amount entry reddens this row.
    assert_eq!(
        prevented_pairs(&events),
        vec![(TargetRef::Player(P0), 5)],
        "one prevention application over the batch, naming the damaged player"
    );

    assert_eq!(
        counters_on(&runner, squire),
        5,
        "CR 615.13: 'that many' is the amount of THIS application — 2 + 3"
    );
}

// ---------------------------------------------------------------------------
// V2 — one shield, TWO recipients: the discriminating row (charter rows 2 + 5)
// ---------------------------------------------------------------------------

/// CR 615.13 + CR 120.3: one Safe Passage application covers a player leg and a
/// creature leg in the SAME CR 510.2 simultaneous batch. The batch emits one
/// `DamagePrevented` per recipient, each carrying that recipient's own amount,
/// and the player-scoped Squire reads ONLY its own leg.
///
/// This is the row that rejects the tempting "just resolve the shield's
/// controller" fix: that fix emits one `(Player(P0), 5)` and hands the Squire 5
/// counters for damage 3 of which was never headed at a player. The rules-correct
/// answer is 2.
///
/// BASE BEHAVIOR this inverts (measured): ONE event, `target=Object(ObjectId(0))`
/// — a sentinel object that does not exist — amount 5, and **0** counters.
///
/// Revert-catches: the controller-resolving alternative (5 counters); any
/// re-collapse of recipients into one summed event; a lost creature leg; a
/// spurious zero-amount third event.
#[test]
fn one_shield_two_recipients_emits_one_event_each_and_the_squire_reads_only_its_own() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let squire = add_squire(&mut scenario, P0);
    let wall = scenario.add_creature(P0, "Wall", 0, 6).id();
    let passage = add_instant_from_oracle(&mut scenario, P0, "Safe Passage", SAFE_PASSAGE_TEXT);
    let attacker_a = scenario.add_creature(P1, "Bear A", 2, 2).id();
    let attacker_b = scenario.add_creature(P1, "Bear B", 3, 3).id();
    let mut runner = scenario.build();

    let crossed = pass_turn_to(&mut runner, P1);
    assert!(
        crossed,
        "stall guard: the turn must actually have passed to P1"
    );

    let life_before = runner.state().players[P0.0 as usize].life;
    let (ran, events) = run_combat_collecting(
        &mut runner,
        P1,
        &[attacker_a, attacker_b],
        P0,
        &[(wall, attacker_b)],
        Some(passage),
    );
    assert!(ran, "combat reach-guard: the attack must actually have run");

    // DUAL reach-guard: BOTH legs really were prevented. Without both halves,
    // "2 counters" is also what a lost creature leg looks like.
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "the player leg (2 from the unblocked Bear A) must have been prevented"
    );
    assert_eq!(
        runner.state().objects[&wall].damage_marked,
        0,
        "the creature leg (3 from the blocked Bear B) must have been prevented"
    );

    // CR 120.3: exactly two events, one per recipient, each with ITS OWN amount.
    let mut expected = vec![(TargetRef::Player(P0), 2), (TargetRef::Object(wall), 3)];
    expected.sort();
    assert_eq!(
        sorted_prevented_pairs(&events),
        expected,
        "one application, two recipients — the player's 2 and the Wall's 3, not one summed 5"
    );

    // CR 615.13 + CR 120.3: the trigger fires ONCE (one application) and reads
    // the amount prevented FOR ITS OWN RECIPIENT.
    assert_eq!(
        counters_on(&runner, squire),
        2,
        "'damage that would be dealt to YOU' is 2 — explicitly NOT 5, which is \
         what a shield-controller-derived single event would hand it"
    );
}

// ---------------------------------------------------------------------------
// V3 — negative: a shield that prevented nothing FOR THE PLAYER (charter row 2)
// ---------------------------------------------------------------------------

/// CR 120.3: the same Safe Passage, but only a creature is a recipient. There is
/// no player leg, so no player-targeted event and the Squire gains nothing.
///
/// Paired positives live in this same file:
/// `combat_prevention_from_a_printed_player_shield_puts_that_many_counters` and
/// `one_shield_two_recipients_emits_one_event_each_and_the_squire_reads_only_its_own`.
/// The in-test reach-guard is the Wall's `damage_marked == 0`: prevention really
/// did happen, so "0 counters" is a statement about the RECIPIENT and not about
/// nothing having been prevented.
///
/// BASE BEHAVIOR this inverts (measured): one event `target=Object(ObjectId(0))`,
/// amount 3, 0 counters — right answer, wrong reason (a fabricated object).
///
/// Revert-catches: the controller-resolving alternative FABRICATES
/// `(Player(P0), 3)` here and hands the Squire 3 counters for damage never headed
/// at a player.
#[test]
fn a_shield_that_prevented_nothing_for_the_player_emits_no_player_event() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let squire = add_squire(&mut scenario, P0);
    let wall = scenario.add_creature(P0, "Wall", 0, 6).id();
    let passage = add_instant_from_oracle(&mut scenario, P0, "Safe Passage", SAFE_PASSAGE_TEXT);
    let attacker = scenario.add_creature(P1, "Bear B", 3, 3).id();
    let mut runner = scenario.build();

    let crossed = pass_turn_to(&mut runner, P1);
    assert!(
        crossed,
        "stall guard: the turn must actually have passed to P1"
    );

    let (ran, events) = run_combat_collecting(
        &mut runner,
        P1,
        &[attacker],
        P0,
        &[(wall, attacker)],
        Some(passage),
    );
    assert!(ran, "combat reach-guard: the attack must actually have run");

    // Reach-guard: prevention DID happen — the 0/6 Wall took none of the 3.
    assert_eq!(
        runner.state().objects[&wall].damage_marked,
        0,
        "the creature leg must have been prevented, so 0 counters below is about \
         the RECIPIENT, not about nothing having happened"
    );

    assert_eq!(
        prevented_pairs(&events),
        vec![(TargetRef::Object(wall), 3)],
        "exactly one event, naming the creature — there is no player leg to name"
    );
    assert_eq!(
        counters_on(&runner, squire),
        0,
        "no damage was headed at the player, so 'damage that would be dealt to \
         you is prevented' never happened"
    );
}

// ---------------------------------------------------------------------------
// V4 — the deleted `Specific` arm's population still fires (charter row 2)
// ---------------------------------------------------------------------------

/// CR 120.3: a RESOLUTION-created player shield. `TargetFilter::Controller` is a
/// context ref, so `untargeted_damage_filter` lowers "prevent all damage that
/// would be dealt to you" to `Player { Specific(P0) }` — the one scope the old
/// re-derivation mapped, and therefore the one population that worked at base.
///
/// This row exists because the fix DELETES that arm. It is the runtime evidence
/// that the ~78-card resolution-created `Specific` / `ParentTarget` population
/// (63 `TargetFilter::Controller` + 15 `ParentTarget` `PreventDamage` effects)
/// still fires in combat with a concrete amount once the recipient is threaded
/// instead of derived.
///
/// BASE BEHAVIOR: identical — this row is green at base by design.
#[test]
fn resolution_created_specific_player_shield_still_fires_in_combat() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let squire = add_squire(&mut scenario, P0);
    let control = add_instant_from_oracle(&mut scenario, P0, "Riot Control", RIOT_CONTROL_TEXT);
    let attacker_a = scenario.add_creature(P1, "Bear A", 2, 2).id();
    let attacker_b = scenario.add_creature(P1, "Bear B", 3, 3).id();
    let mut runner = scenario.build();

    let crossed = pass_turn_to(&mut runner, P1);
    assert!(
        crossed,
        "stall guard: the turn must actually have passed to P1"
    );

    let (ran, events) = run_combat_collecting(
        &mut runner,
        P1,
        &[attacker_a, attacker_b],
        P0,
        &[],
        Some(control),
    );
    assert!(ran, "combat reach-guard: the attack must actually have run");

    // Reach-guard: the SPELL resolved (it gains 1 life per opponent creature, and
    // P1 controls two) AND no combat damage got through. 22, not 20 and not 17.
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        22,
        "Riot Control resolved (+2 life for P1's two creatures) and none of the 5 \
         combat damage was dealt"
    );

    assert_eq!(
        prevented_pairs(&events),
        vec![(TargetRef::Player(P0), 5)],
        "one application over the batch, naming the shielded player"
    );
    assert_eq!(counters_on(&runner, squire), 5, "2 + 3 prevented for P0");
}

// ---------------------------------------------------------------------------
// V5 — the unfiltered shield: no filter to derive a recipient FROM at all
//      (event-shape row for the building block; deliberately NO Squire)
// ---------------------------------------------------------------------------

/// CR 120.3: Fog has no `damage_target_filter` whatsoever — `TargetFilter::Any`
/// lowers to `None` — so there is no scope an arm could ever be added for. The old
/// path fell through to `TargetRef::Object(rid.source)`, and a floating
/// resolution-created shield's `rid.source` is the sentinel `ObjectId(0)`: the
/// event named an object that DOES NOT EXIST. This is the largest broken class
/// (218 resolution `PreventDamage{Any}` shields plus every typed-object shield).
///
/// BASE BEHAVIOR this inverts (measured): ONE event,
/// `target=Object(ObjectId(0))`, amount 9.
///
/// No Squire here on purpose: this row is about the EVENT SHAPE of the shared
/// building block, across three recipients of both kinds, not about the one card
/// that consumes it.
#[test]
fn an_unfiltered_shield_names_every_recipient_not_a_sentinel_object() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let fog = add_instant_from_oracle(&mut scenario, P0, "Fog", FOG_TEXT);
    let attacker_a = scenario.add_creature(P0, "Bear A", 2, 2).id();
    let attacker_b = scenario.add_creature(P0, "Bear B", 3, 3).id();
    let blocker = scenario.add_creature(P1, "Grizzly", 4, 4).id();
    let mut runner = scenario.build();

    // Cast pre-combat on P0's own turn: Fog is "this turn", so it is live for the
    // combat-damage step (CR 615.4 — the shield exists before the damage event).
    runner.cast(fog).resolve();

    let life_before = runner.state().players[P1.0 as usize].life;
    let (ran, events) = run_combat_collecting(
        &mut runner,
        P0,
        &[attacker_a, attacker_b],
        P1,
        &[(blocker, attacker_b)],
        None,
    );
    assert!(ran, "combat reach-guard: the attack must actually have run");

    // Reach-guard: all 9 really was prevented, across all three recipients.
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        life_before,
        "the unblocked Bear A's 2 must have been prevented"
    );
    for (creature, label) in [(attacker_b, "Bear B"), (blocker, "Grizzly")] {
        assert_eq!(
            runner.state().objects[&creature].damage_marked,
            0,
            "{label}'s combat damage must have been prevented"
        );
    }

    let mut expected = vec![
        (TargetRef::Player(P1), 2),
        (TargetRef::Object(blocker), 3),
        (TargetRef::Object(attacker_b), 4),
    ];
    expected.sort();
    assert_eq!(
        sorted_prevented_pairs(&events),
        expected,
        "one shield, three recipients — each named, each with its own amount"
    );
    assert!(
        !prevented_pairs(&events)
            .iter()
            .any(|(target, _)| *target == TargetRef::Object(ObjectId(0))),
        "no event may name the ObjectId(0) sentinel — that object does not exist"
    );
}

// ---------------------------------------------------------------------------
// V5(b) — prevention in BOTH combat-damage steps (the hostile case for a latch)
// ---------------------------------------------------------------------------

/// CR 510.4 + CR 702.4b + CR 615.13: first-strike and regular damage are two
/// separate combat-damage steps, each its own CR 510.2 simultaneous batch and
/// therefore its own prevention APPLICATION — so the Squire's trigger fires once
/// per step, each firing reading its own step's amount.
///
/// A 2/2 double-striker and a vanilla 3/3 both go unblocked at the shielded
/// player: the first-strike step prevents 2, the regular step prevents 2 + 3 = 5,
/// and the Squire ends on 2 + 5 = 7.
///
/// This is the Squire-level count the non-combat
/// `two_separate_preventions_each_read_their_own_amount` row cannot supply: a
/// naive once-per-turn latch in the matcher (or a dedup keyed on the turn rather
/// than on the prevention application) fires only for the first step and strands
/// the Squire on 2. The differing per-step amounts (2 then 5, mirroring the
/// 3-then-8 non-combat row) additionally reject an amount bleed: resolving both
/// firings against the first step's amount gives 2 + 2 = 4, against the last
/// gives 5 + 5 = 10.
///
/// The EVENT count per step is pinned at the building-block level by
/// `combat_damage.rs`'s `test_inkshield_double_strike_fires_rider_per_combat_step`;
/// this row pins the SQUIRE-level trigger count and per-step amount provenance.
///
/// Revert-catches: a once-per-turn latch in `match_damage_prevented` (2 counters,
/// caught by the final assertion); any cross-step dedup of the trigger; an
/// amount bleed in either direction (4 or 10 counters).
#[test]
fn double_strike_prevention_fires_once_per_combat_damage_step() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let squire = add_squire(&mut scenario, P0);
    let shield = add_shield_enchantment(&mut scenario, P0, "Energy Field", ENERGY_FIELD_TEXT);
    let striker = {
        let mut b = scenario.add_creature(P1, "Double Striker", 2, 2);
        b.double_strike();
        b.id()
    };
    let bear = scenario.add_creature(P1, "Bear", 3, 3).id();
    let mut runner = scenario.build();

    runner.cast(shield).resolve();
    assert_eq!(counters_on(&runner, squire), 0, "reach-guard: pre-combat");

    // Hand the turn to P1 so it can attack.
    let crossed = pass_turn_to(&mut runner, P1);
    assert!(
        crossed,
        "stall guard: the turn must actually have passed to P1"
    );

    // CLASS reach-guard: the same printed Controller-scoped population as V1, so
    // a regression to the filter-derived recipient cannot hide behind the one
    // scope that always worked.
    assert_eq!(
        runner.state().objects[&shield].replacement_definitions[0].damage_target_filter,
        Some(DamageTargetFilter::Player {
            player: DamageTargetPlayerScope::Controller
        }),
        "the printed 'dealt to you' shield is Controller-scoped, not Specific"
    );

    let life_before = runner.state().players[P0.0 as usize].life;
    let (ran, events) = run_combat_collecting(&mut runner, P1, &[striker, bear], P0, &[], None);
    assert!(ran, "combat reach-guard: the attack must actually have run");

    // Reach-guard: all 7 really was prevented across the two steps (2 in the
    // first-strike step, 2 + 3 in the regular step). Without this, "7 counters"
    // is also what combat-never-happened looks like.
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "all 7 combat damage must have been prevented by the shield"
    );

    // CR 510.4 + CR 615.13: two steps, two batches, two applications — one event
    // per step IN STEP ORDER, each carrying its own step's amount. Exact list,
    // length included, so a collapsed single event or a spurious third entry
    // reddens this row.
    assert_eq!(
        prevented_pairs(&events),
        vec![(TargetRef::Player(P0), 2), (TargetRef::Player(P0), 5)],
        "one prevention application per combat-damage step: 2 (first-strike) then 5 (regular)"
    );

    // CR 615.13: one firing per application, each reading its own step's amount.
    // A once-per-turn latch strands this at 2; a first-amount bleed gives 4; a
    // last-amount bleed gives 10.
    assert_eq!(
        counters_on(&runner, squire),
        7,
        "2 (first-strike step) + 5 (regular step) — explicitly NOT 2, which is \
         what a naive once-per-turn latch in the matcher would leave"
    );
}

// ---------------------------------------------------------------------------
// V6 — the Squire's own canonical play pattern (charter row 2)
// ---------------------------------------------------------------------------

/// CR 615.4 + CR 615.13: Selfless Squire is DESIGNED to be flashed in during
/// combat — its own ETB shield then prevents the batch and its own trigger reads
/// the amount. Single combat, single turn crossing.
///
/// BASE BEHAVIOR: identical — this row is green at base, because the ETB shield is
/// resolution-created from `TargetFilter::Controller` and therefore lowered to
/// `Specific(P0)`, the one scope the old re-derivation mapped. It is here so the
/// card's canonical line cannot regress under a change aimed at the other classes.
#[test]
fn the_squires_own_flashed_shield_fires_its_trigger_in_combat() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let squire = scenario
        .add_creature_to_hand_from_oracle(P0, "Selfless Squire", 1, 1, SELFLESS_SQUIRE_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let attacker_a = scenario.add_creature(P1, "Bear A", 2, 2).id();
    let attacker_b = scenario.add_creature(P1, "Bear B", 3, 3).id();
    let mut runner = scenario.build();

    let crossed = pass_turn_to(&mut runner, P1);
    assert!(
        crossed,
        "stall guard: the turn must actually have passed to P1"
    );

    let life_before = runner.state().players[P0.0 as usize].life;
    let (ran, events) = run_combat_collecting(
        &mut runner,
        P1,
        &[attacker_a, attacker_b],
        P0,
        &[],
        Some(squire),
    );
    assert!(ran, "combat reach-guard: the attack must actually have run");

    // Reach-guard: the Squire actually entered AND its ETB shield absorbed the
    // batch. Without the first half, "5 counters" could not even be read.
    assert!(
        runner.state().objects.contains_key(&squire),
        "the flashed Squire must be on the battlefield"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "the Squire's own ETB shield must have absorbed all 5"
    );

    assert_eq!(
        prevented_pairs(&events),
        vec![(TargetRef::Player(P0), 5)],
        "one application over the batch, naming the shielded player"
    );
    assert_eq!(counters_on(&runner, squire), 5, "2 + 3 prevented for P0");
}

// ---------------------------------------------------------------------------
// V12 — the Squire is ITSELF a recipient in the same batch (charter row 2)
// ---------------------------------------------------------------------------

/// CR 120.3: the per-recipient split newly makes ONE batch carry both an OBJECT
/// recipient and a PLAYER recipient. Only `match_damage_prevented`'s early guard
/// —
///
/// ```ignore
/// if trigger.valid_card.is_none() && trigger.valid_target.is_some() { return false }
/// ```
///
/// — stops the player-scoped Squire from ALSO reading the object leg. This row
/// makes the Squire itself the damaged object, which is the card's natural play
/// (a 1/1 chump-blocking behind Safe Passage), so deleting that guard turns 2
/// counters into 5.
///
/// `a_shield_that_prevented_nothing_for_the_player_emits_no_player_event` does
/// NOT catch that deletion: its creature recipient is a separate 0/6 Wall, so the
/// matcher's fallthrough arm compares `*target_id == source_id` and is false
/// anyway. Only making the trigger's OWN source the damaged object discriminates.
///
/// BASE BEHAVIOR this inverts: one event `target=Object(ObjectId(0))`, amount 5,
/// 0 counters.
#[test]
fn the_squire_itself_blocking_reads_only_the_player_leg() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let squire = add_squire(&mut scenario, P0);
    let passage = add_instant_from_oracle(&mut scenario, P0, "Safe Passage", SAFE_PASSAGE_TEXT);
    let attacker_a = scenario.add_creature(P1, "Bear A", 2, 2).id();
    let attacker_b = scenario.add_creature(P1, "Bear B", 3, 3).id();
    let mut runner = scenario.build();

    let crossed = pass_turn_to(&mut runner, P1);
    assert!(
        crossed,
        "stall guard: the turn must actually have passed to P1"
    );

    let life_before = runner.state().players[P0.0 as usize].life;
    let (ran, events) = run_combat_collecting(
        &mut runner,
        P1,
        &[attacker_a, attacker_b],
        P0,
        &[(squire, attacker_b)],
        Some(passage),
    );
    assert!(ran, "combat reach-guard: the attack must actually have run");

    // Reach-guards, all three load-bearing.
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "the player leg (2 from the unblocked Bear A) must have been prevented"
    );
    assert!(
        runner.state().objects.contains_key(&squire),
        "a 1/1 that survived blocking a 3/3 proves the CREATURE leg really was \
         prevented — if it were not, CR 704.5g would have put it in the graveyard"
    );
    assert_eq!(
        runner.state().objects[&squire].damage_marked,
        0,
        "the Squire took none of the blocked Bear B's 3"
    );

    let mut expected = vec![(TargetRef::Player(P0), 2), (TargetRef::Object(squire), 3)];
    expected.sort();
    assert_eq!(
        sorted_prevented_pairs(&events),
        expected,
        "one application, two recipients — and one of them IS the trigger's source"
    );

    assert_eq!(
        counters_on(&runner, squire),
        2,
        "'damage that would be dealt to YOU' is the player leg only — 2, NOT 5. \
         Deleting the matcher's player-scope guard makes this 5"
    );
}

// ---------------------------------------------------------------------------
// V11 — reflecting shields aggregate the public event, reflect per source
// ---------------------------------------------------------------------------

/// CR 615.13 + CR 615.5: one prevention application over simultaneous damage
/// events is ONE trigger, while each reflection still binds its own damage
/// source ("that much damage to THAT creature"). A shield whose rider reflects
/// per damage source (Comeuppance) therefore accumulates its PUBLIC prevented
/// amount into `combat_prevention_tally` — one `DamagePrevented` per
/// (shield, recipient) — and keeps its per-event rider stash + inline drain,
/// so each attacker is dealt back exactly what it dealt.
///
/// Revert-catches: re-fragmenting the public event (two events again) or
/// collapsing the reflections (one attacker surviving, or the wrong amounts
/// reflected).
#[test]
fn reflecting_shields_aggregate_public_event_keep_per_source_reflection() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let squire = add_squire(&mut scenario, P0);
    let comeuppance = add_instant_from_oracle(&mut scenario, P0, "Comeuppance", COMEUPPANCE_TEXT);
    let attacker_a = scenario.add_creature(P1, "Bear A", 2, 2).id();
    let attacker_b = scenario.add_creature(P1, "Bear B", 3, 3).id();
    let mut runner = scenario.build();

    let crossed = pass_turn_to(&mut runner, P1);
    assert!(
        crossed,
        "stall guard: the turn must actually have passed to P1"
    );

    let life_before = runner.state().players[P0.0 as usize].life;
    let (ran, events) = run_combat_collecting(
        &mut runner,
        P1,
        &[attacker_a, attacker_b],
        P0,
        &[],
        Some(comeuppance),
    );
    assert!(ran, "combat reach-guard: the attack must actually have run");

    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "reach-guard: all 5 really was prevented"
    );

    // CR 615.13: ONE application, ONE public event carrying the whole batch.
    assert_eq!(
        sorted_prevented_pairs(&events),
        vec![(TargetRef::Player(P0), 5)],
        "a per-source-reflecting shield aggregates its public prevented amount \
         like every other prevention application — one event, not one per \
         damage event"
    );
    assert_eq!(
        counters_on(&runner, squire),
        5,
        "the Squire fires ONCE for the aggregated 5"
    );
    // CR 615.5: each reflection still bound its own source — Bear A (2/2)
    // took back exactly 2 and Bear B (3/3) exactly 3, so both are dead. A
    // collapsed single-source reflection would leave one attacker alive, and
    // swapped amounts would spare the 3/3.
    assert_eq!(
        runner.state().objects[&attacker_a].zone,
        Zone::Graveyard,
        "Bear A must have been dealt back its own 2"
    );
    assert_eq!(
        runner.state().objects[&attacker_b].zone,
        Zone::Graveyard,
        "Bear B must have been dealt back its own 3"
    );
}

// ---------------------------------------------------------------------------
// V11-companion — depletion shields aggregate the public event
// ---------------------------------------------------------------------------

/// CR 615.13 + CR 615.7: one depletion application over simultaneous damage
/// events is ONE trigger. A `Next(7)` player shield facing a simultaneous
/// 2-and-3 batch accumulates its public prevented amount into
/// `combat_prevention_tally` — one `DamagePrevented` per (shield, recipient).
/// CR 615.7's choice ("the shielded player chooses which damage the shield
/// prevents") is about WHICH damage, expressly "the number of events or
/// sources dealing it doesn't matter", so aggregating the public event does
/// not disturb it; the depletion arithmetic (7 − 2 − 3 → `Next(2)`) is
/// unchanged. Sibling to
/// `reflecting_shields_aggregate_public_event_keep_per_source_reflection`.
///
/// Revert-catches: re-fragmenting the public event (two events again).
#[test]
fn depletion_shields_aggregate_public_event() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let squire = add_squire(&mut scenario, P0);
    let bulwark = add_instant_from_oracle(
        &mut scenario,
        P0,
        "Depletion Bulwark",
        DEPLETION_BULWARK_TEXT,
    );
    let attacker_a = scenario.add_creature(P1, "Bear A", 2, 2).id();
    let attacker_b = scenario.add_creature(P1, "Bear B", 3, 3).id();
    let mut runner = scenario.build();

    assert_eq!(counters_on(&runner, squire), 0, "reach-guard: pre-combat");

    let crossed = pass_turn_to(&mut runner, P1);
    assert!(
        crossed,
        "stall guard: the turn must actually have passed to P1"
    );

    let life_before = runner.state().players[P0.0 as usize].life;
    let (ran, events) = run_combat_collecting(
        &mut runner,
        P1,
        &[attacker_a, attacker_b],
        P0,
        &[],
        Some(bulwark),
    );
    assert!(ran, "combat reach-guard: the attack must actually have run");

    // CLASS reach-guard: this row must exercise the depletion population, not
    // accidentally land on an `All` shield. A resolution-created instant shield
    // floats in `pending_damage_replacements`, and CR 615.7 depletion arithmetic
    // (7 − 2 − 3) must have left exactly `Next(2)` behind — still installed,
    // since the shield was not used up.
    let depletion = runner
        .state()
        .pending_damage_replacements
        .iter()
        .find(|r| r.shield_kind.is_shield())
        .expect("reach guard: the depletion shield must be installed");
    assert_eq!(
        depletion.shield_kind,
        ShieldKind::Prevention {
            amount: PreventionAmount::Next(2)
        },
        "this row must exercise PreventionAmount::Next — 7 minus the 2-and-3 \
         batch leaves Next(2)"
    );

    // Reach-guard: all 5 really was prevented. Without this, "5 counters" is
    // also what combat-never-happened looks like.
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "reach-guard: all 5 really was prevented"
    );

    // CR 615.13: ONE application, ONE public event carrying the whole batch.
    assert_eq!(
        sorted_prevented_pairs(&events),
        vec![(TargetRef::Player(P0), 5)],
        "a depletion shield aggregates its public prevented amount like every \
         other prevention application — one event, not one per damage event"
    );
    assert_eq!(
        counters_on(&runner, squire),
        5,
        "the Squire fires ONCE for the aggregated 5"
    );
}

// ---------------------------------------------------------------------------
// T3 — negative: unprevented damage does NOT trigger (charter row 2)
// ---------------------------------------------------------------------------

/// CR 120.3: damage that is actually DEALT is not damage that was prevented.
///
/// The paired positive reach-guard is mandatory and lives in this same test: "0
/// counters" is equally satisfiable by the trigger never being registered at all.
/// Step 2 raises a shield and repeats the same burn, and the Squire must gain
/// counters — proving step 1's zero was a real discrimination.
#[test]
fn unprevented_damage_does_not_trigger_but_prevented_damage_does() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let squire = add_squire(&mut scenario, P0);
    let shield = add_shield_enchantment(
        &mut scenario,
        P0,
        "Solitary Confinement",
        SOLITARY_CONFINEMENT_TEXT,
    );
    let bolt_a = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Probe A",
            true,
            "Probe A deals 4 damage to target player.",
        )
        .id();
    let bolt_b = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Probe B",
            true,
            "Probe B deals 4 damage to target player.",
        )
        .id();
    let mut runner = scenario.build();

    // Step 1 — NEGATIVE. No shield yet: the damage lands.
    let outcome = runner.cast(bolt_a).target_player(P0).resolve();
    outcome.assert_life_delta(P0, -4);
    outcome.assert_counters(squire, CounterType::Plus1Plus1, 0);

    // Step 2 — PAIRED POSITIVE REACH-GUARD. Same burn, now prevented.
    runner.cast(shield).resolve();
    let outcome = runner.cast(bolt_b).target_player(P0).resolve();
    outcome.assert_life_delta(P0, 0);
    outcome.assert_counters(squire, CounterType::Plus1Plus1, 4);
}

// ---------------------------------------------------------------------------
// T4 / T5 — "once" semantics are an EVENT-STREAM property (charter row 4)
// ---------------------------------------------------------------------------

/// CR 603.2c + CR 615.13: two SEPARATE prevention applications in one turn are
/// two trigger events, each reading its OWN amount.
///
/// This is the amount-provenance hostile fixture. Two competing amount
/// authorities live in `quantity.rs`'s cascade (`enclosing_trigger_event_amount`
/// vs `state.last_effect_count`). Preventing 3 and then 5 must yield 3 + 5 = 8
/// total, arriving as 3 first and 8 second. A `last_effect_count` bleed would
/// give 5+5=10 or 3+3=6; a once-per-turn latch would give 3.
#[test]
fn two_separate_preventions_each_read_their_own_amount() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let squire = add_squire(&mut scenario, P0);
    let shield = add_shield_enchantment(
        &mut scenario,
        P0,
        "Solitary Confinement",
        SOLITARY_CONFINEMENT_TEXT,
    );
    let bolt_a = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Probe Three",
            true,
            "Probe Three deals 3 damage to target player.",
        )
        .id();
    let bolt_b = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Probe Five",
            true,
            "Probe Five deals 5 damage to target player.",
        )
        .id();
    let mut runner = scenario.build();

    runner.cast(shield).resolve();

    // First application: 3 prevented → exactly 3 counters.
    let outcome = runner.cast(bolt_a).target_player(P0).resolve();
    outcome.assert_life_delta(P0, 0);
    outcome.assert_counters(squire, CounterType::Plus1Plus1, 3);

    // Second application: 5 prevented → 3 + 5 = 8. Not 3 (latched), not 10
    // (last-amount bleed), not 6 (first-amount bleed).
    let outcome = runner.cast(bolt_b).target_player(P0).resolve();
    outcome.assert_life_delta(P0, 0);
    outcome.assert_counters(squire, CounterType::Plus1Plus1, 8);
}

// ---------------------------------------------------------------------------
// T6 — recipient scoping end-to-end (charter row 2, CR 120.3)
// ---------------------------------------------------------------------------

/// CR 120.3: "damage that would be dealt to YOU" is the ability's controller.
/// Prevention for a DIFFERENT player must not trigger it.
///
/// Paired positive reach-guard in the same test: the controller's own prevention
/// then fires. Without that pairing, "0 counters" would be satisfiable by a
/// trigger that never registered. Revert-failing on the recipient half: dropping
/// `valid_target = Controller` in the parser (or the `valid_player_matches` call
/// in the matcher) makes step 1 fire and the assertion flips.
#[test]
fn prevention_for_another_player_does_not_trigger_the_squire() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let squire = add_squire(&mut scenario, P0);
    // P1's own shield, placed directly on P1's battlefield — the runner casts as
    // the active player (P0), so P1's permanent cannot be routed through `cast`.
    // "Prevent all damage that would be dealt to you" is controller-relative, so
    // on P1's permanent it shields P1.
    let their_shield = scenario
        .add_enchantment_from_oracle(P1, "Solitary Confinement", SOLITARY_CONFINEMENT_TEXT)
        .id();
    let my_shield = add_shield_enchantment(
        &mut scenario,
        P0,
        "Solitary Confinement B",
        SOLITARY_CONFINEMENT_TEXT,
    );
    let bolt_them = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Probe Them",
            true,
            "Probe Them deals 4 damage to target player.",
        )
        .id();
    let bolt_me = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Probe Me",
            true,
            "Probe Me deals 4 damage to target player.",
        )
        .id();
    let mut runner = scenario.build();

    // Reach-guard: P1's shield really is installed, so step 1's prevention will
    // actually occur. Without this, "0 counters" could mean "nothing was ever
    // prevented" rather than "prevention happened for the wrong player".
    assert!(
        !runner.state().objects[&their_shield]
            .replacement_definitions
            .is_empty(),
        "P1's printed shield must be installed on the battlefield"
    );

    // Step 1 — NEGATIVE: damage to P1 is prevented by P1's shield. The Squire's
    // controller is P0, so its trigger must NOT fire.
    let outcome = runner.cast(bolt_them).target_player(P1).resolve();
    outcome.assert_life_delta(P1, 0);
    outcome.assert_counters(squire, CounterType::Plus1Plus1, 0);

    // Step 2 — PAIRED POSITIVE REACH-GUARD: the same shape aimed at P0 fires.
    runner.cast(my_shield).resolve();
    let outcome = runner.cast(bolt_me).target_player(P0).resolve();
    outcome.assert_life_delta(P0, 0);
    outcome.assert_counters(squire, CounterType::Plus1Plus1, 4);
}

// ---------------------------------------------------------------------------
// PR #9867 review (HIGH: noncombat simultaneous-batch aggregation)
// ---------------------------------------------------------------------------

/// Verbatim Rakdos Charm (Scryfall), mode 3: each creature deals 1 damage to
/// its controller — the `EachSourceDealsDamage` simultaneous batch.
const RAKDOS_CHARM_TEXT: &str = "Choose one \u{2014}\n\u{2022} Exile target player's graveyard.\n\u{2022} Destroy target artifact.\n\u{2022} Each creature deals 1 damage to its controller.";

/// Verbatim Hardened Scales (Scryfall): the nonlinear counter amplifier that
/// discriminates one 3-counter placement (4 total) from three 1-counter
/// placements (6 total).
const HARDENED_SCALES_TEXT: &str = "If one or more +1/+1 counters would be put on a creature you control, put that many plus one +1/+1 counters on it instead.";

/// Verbatim The One Ring (Scryfall): ETB burden counters, tap-for-draw, and
/// the protection-from-everything turn window this file expires and narrows.
const ONE_RING_TEXT: &str = "Indestructible\nWhen The One Ring enters, if you cast it, you gain protection from everything until your next turn.\nAt the beginning of your upkeep, you lose 1 life for each burden counter on The One Ring.\n{T}: Put a burden counter on The One Ring, then draw a card for each burden counter on it.";

/// CR 615.13 + CR 120.3: three simultaneous 1-damage events (two bears plus
/// the Squire itself) to one shielded player are ONE prevention application —
/// one `DamagePrevented` event, one Squire trigger placing 3 at once, +1 from
/// Hardened Scales = 4. Per-event fragmentation would fire 3 triggers for
/// (1+1) × 3 = 6.
///
/// Revert-catches: deactivating the batch tally in
/// `resolve_each_source_deals_damage` restores 3 events / 6 counters.
#[test]
fn rakdos_charm_simultaneous_batch_fires_one_squire_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let squire = scenario
        .add_creature_to_hand_from_oracle(P0, "Selfless Squire", 1, 1, SELFLESS_SQUIRE_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let scales = scenario
        .add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES_TEXT)
        .id();
    let _bear_a = scenario.add_creature(P0, "Bear A", 2, 2).id();
    let _bear_b = scenario.add_creature(P0, "Bear B", 2, 2).id();
    let charm = scenario
        .add_spell_to_hand_from_oracle(P0, "Rakdos Charm", true, RAKDOS_CHARM_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let mut runner = scenario.build();

    // The Squire must be cast for real so its ETB installs the shield.
    runner.cast(squire).resolve();
    assert!(
        runner.state().objects[&squire].zone == Zone::Battlefield,
        "reach guard: the Squire must be on the battlefield with its ETB shield"
    );
    assert!(
        !runner.state().objects[&scales]
            .replacement_definitions
            .is_empty(),
        "reach guard: Hardened Scales must be installed on the battlefield"
    );

    let life_before = runner.state().players[P0.0 as usize].life;
    let outcome = runner.cast(charm).modes(&[2]).resolve();

    // Reach-guard: all 3 damage really was PREVENTED, not merely never dealt.
    outcome.assert_life_delta(P0, 0);
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "the Squire's shield must have absorbed all 3 damage"
    );

    // CR 615.13: ONE application over the simultaneous batch.
    assert_eq!(
        prevented_pairs(outcome.events()),
        vec![(TargetRef::Player(P0), 3)],
        "the batch must emit one aggregate event naming the shielded player"
    );
    assert_eq!(
        counters_on(&runner, squire),
        4,
        "one trigger placing 3 at once, +1 from Hardened Scales (3 triggers would make 6)"
    );
}

/// PR #9867 review (HIGH: pause-path control) — one source (the Squire
/// itself) with TWO co-applicable shields (its ETB shield plus Safe Passage),
/// so Phase B pauses for a CR 616.1 ordering choice. The tally is
/// `#[serde(skip)]` transient state and cannot span the player-visible pause:
/// the (empty) prefix parks in the pause-owned batch record, the resumed
/// entry re-accumulates onto it through the preserved continuation machinery,
/// and the completion cascade publishes the single aggregate. Driven manually
/// so the test PROVES the pause prompt appeared (two candidates) rather than
/// merely tolerating it.
///
/// NOTE (pre-existing, out of scope): a multi-entry batch that pauses loses
/// its stashed tail on the base commit too (verified by stashing this
/// change) — a continuation-drain defect independent of batch aggregation,
/// flagged for follow-up. This control uses one source to pin the
/// pause/resume mechanics without depending on tail re-entry.
#[test]
fn rakdos_charm_batch_with_ordering_pause_completes_single_aggregate_after_flush() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let squire = scenario
        .add_creature_to_hand_from_oracle(P0, "Selfless Squire", 1, 1, SELFLESS_SQUIRE_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let passage = add_instant_from_oracle(&mut scenario, P0, "Safe Passage", SAFE_PASSAGE_TEXT);
    let charm = scenario
        .add_spell_to_hand_from_oracle(P0, "Rakdos Charm", true, RAKDOS_CHARM_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let mut runner = scenario.build();

    runner.cast(squire).resolve();
    runner.cast(passage).resolve();
    assert!(
        runner.state().objects[&squire].zone == Zone::Battlefield,
        "reach guard: the Squire must be on the battlefield with its ETB shield"
    );

    let life_before = runner.state().players[P0.0 as usize].life;
    // No auto-answer: the driver must HALT at the ordering prompt.
    let outcome = runner.cast(charm).modes(&[2]).resolve();
    let WaitingFor::ReplacementChoice { candidates, .. } = outcome.final_waiting_for() else {
        panic!(
            "the batch must pause for a CR 616.1 ordering choice, got {:?}",
            outcome.final_waiting_for()
        );
    };
    assert_eq!(
        candidates.len(),
        2,
        "both shields must be offered as ordering candidates"
    );

    // Answer and drive to a clean priority window.
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the ordering choice must be answerable");
    runner.advance_until_stack_empty();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must complete to a clean priority window, got {:?}",
        runner.state().waiting_for
    );

    // The resumed entry prevented its 1 damage and the Squire's trigger
    // (which observes prevention from ANY source) fired exactly once.
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "the resumed entry must prevent its damage"
    );
    assert_eq!(
        counters_on(&runner, squire),
        1,
        "one resumed entry fires the trigger once"
    );
}

/// CR 615.13 + CR 616.1e: a replacement-ordering pause at the FINAL entry of a
/// two-source batch, with a NONEMPTY parked prefix, still yields ONE
/// prevention application — the prefix is republished with the resumed entry,
/// not flushed separately at pause time. Squire first (one shield: its ETB,
/// no pause, parks 1), Bear last (two shields: Squire's ETB plus New Way
/// Forward aimed at the Bear, pauses for ordering); answering with the Squire
/// shield prevents both under one application: one aggregate event naming 2,
/// one trigger placing 2, +1 from Hardened Scales = 3. The old prefix-flush
/// published the prefix separately (two events, two triggers, 4 counters).
///
/// Revert-catches: restoring the pause-time prefix flush yields two
/// `DamagePrevented` events and 4 counters.
#[test]
fn rakdos_charm_nonempty_prefix_pause_keeps_one_prevention_application() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let squire = scenario
        .add_creature_to_hand_from_oracle(P0, "Selfless Squire", 1, 1, SELFLESS_SQUIRE_TEXT)
        .with_mana_cost(free_cost())
        .id();
    // The Bear must ENTER after the Squire: the batch iterates the
    // battlefield in entry order, and the pause must land on the final entry
    // with the Squire's event parked as a nonempty prefix.
    let bear = scenario
        .add_creature_to_hand(P0, "Bear", 2, 2)
        .with_mana_cost(free_cost())
        .id();
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES_TEXT);
    let way = add_instant_from_oracle(&mut scenario, P0, "New Way Forward", NEW_WAY_FORWARD_TEXT);
    let charm = scenario
        .add_spell_to_hand_from_oracle(P0, "Rakdos Charm", true, RAKDOS_CHARM_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let mut runner = scenario.build();

    runner.cast(squire).resolve();
    runner.cast(bear).resolve();
    runner.cast(way).resolve();
    // New Way Forward aims at the Bear: only the Bear event gets a second
    // shield, so only it can pause for ordering.
    let WaitingFor::DamageSourceChoice { .. } = runner.state().waiting_for.clone() else {
        panic!(
            "New Way Forward must ask for its damage source, got {:?}",
            runner.state().waiting_for
        );
    };
    runner
        .act(GameAction::ChooseDamageSource { source: bear })
        .expect("naming the Bear must be answerable");
    assert!(
        runner.state().objects[&squire].zone == Zone::Battlefield
            && runner.state().objects[&bear].zone == Zone::Battlefield,
        "reach guard: both creatures must be on the battlefield"
    );
    let battlefield_creatures: Vec<ObjectId> = runner
        .state()
        .battlefield
        .iter()
        .filter(|id| runner.state().objects[id].power.is_some())
        .copied()
        .collect();
    assert_eq!(
        battlefield_creatures,
        vec![squire, bear],
        "reach guard: the batch must iterate Squire first, Bear last"
    );

    let life_before = runner.state().players[P0.0 as usize].life;
    let hand_before = runner
        .state()
        .objects
        .values()
        .filter(|o| o.controller == P0 && o.zone == Zone::Hand)
        .count();
    let mut log: Vec<GameEvent> = Vec::new();
    let outcome = runner.cast(charm).modes(&[2]).resolve();
    log.extend(outcome.events().iter().cloned());
    let WaitingFor::ReplacementChoice { candidates, .. } = outcome.final_waiting_for() else {
        panic!(
            "the Bear event must pause for a CR 616.1 ordering choice, got {:?}",
            outcome.final_waiting_for()
        );
    };
    assert_eq!(
        candidates.len(),
        2,
        "the Bear event must offer both shields as ordering candidates"
    );
    let squire_index = candidates
        .iter()
        .position(|c| c.source_name.contains("Selfless Squire"))
        .expect("one candidate must be the Squire's ETB shield");
    assert!(
        candidates.iter().any(|c| c.source_name.is_empty()),
        "the other candidate must be New Way Forward's floating shield, got {:?}",
        candidates
            .iter()
            .map(|c| &c.source_name)
            .collect::<Vec<_>>()
    );
    // The pause parked a NONEMPTY prefix: exactly one source's prevention (the
    // Squire's event, first in battlefield order) with the Bear's event
    // paused — pause at the final entry, not index 0.
    assert_eq!(
        runner.state().pending_each_source_batches.len(),
        1,
        "the pause must park exactly one batch record"
    );
    assert_eq!(
        runner.state().pending_each_source_batches[0]
            .tally
            .iter()
            .map(|entry| entry.amount)
            .sum::<i32>(),
        1,
        "the parked prefix must hold the Squire's 1 prevented"
    );

    // Answer with the Squire shield: both preventions land under ONE shield,
    // hence ONE application. (Choosing New Way Forward instead would prevent
    // under two shields — two applications, four counters — for the right
    // reason, and is NOT this test.)
    let result = runner
        .act(GameAction::ChooseReplacement {
            index: squire_index,
        })
        .expect("the ordering choice must be answerable");
    log.extend(result.events);
    runner.advance_until_stack_empty();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must complete to a clean priority window, got {:?}",
        runner.state().waiting_for
    );

    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "both damage must be prevented (none dealt through)"
    );
    assert_eq!(
        prevented_pairs(&log),
        vec![(TargetRef::Player(P0), 2)],
        "prefix + resumed entry must publish as ONE aggregate application"
    );
    assert_eq!(
        counters_on(&runner, squire),
        3,
        "one trigger placing 2 at once, +1 from Hardened Scales (two triggers would make 4)"
    );
    assert_eq!(
        runner
            .state()
            .objects
            .values()
            .filter(|o| o.controller == P0 && o.zone == Zone::Hand)
            .count(),
        hand_before - 1,
        "New Way Forward's rider must stay silent (its shield prevented nothing): \
         only the Charm left the hand, nothing drawn"
    );
    assert!(
        runner.state().pending_each_source_batches.is_empty(),
        "the pause record must be consumed by the completion cascade"
    );
}

/// CR 615.13 + CR 615.5 + CR 616.1e (Matt C1): the completed batch's
/// accumulation scope must be deactivated BEFORE its riders dispatch. Same
/// nonempty-prefix pause as above, but the final Bear event is answered with
/// New Way Forward: the batch publishes TWO aggregate applications
/// (Squire-shield 1, NWF 1), then NWF's rider reflects 1 to P0 and draws 1 —
/// and the Squire ETB shield still prevents that reflected damage as a THIRD,
/// separate application (its own event + Squire trigger). Pre-fix the
/// reflection was absorbed into the already-popped batch and discarded on
/// restore, losing the separate event/trigger.
#[test]
fn rakdos_charm_rider_reflection_prevents_as_separate_application() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let squire = scenario
        .add_creature_to_hand_from_oracle(P0, "Selfless Squire", 1, 1, SELFLESS_SQUIRE_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let bear = scenario
        .add_creature_to_hand(P0, "Bear", 2, 2)
        .with_mana_cost(free_cost())
        .id();
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES_TEXT);
    let way = add_instant_from_oracle(&mut scenario, P0, "New Way Forward", NEW_WAY_FORWARD_TEXT);
    let charm = scenario
        .add_spell_to_hand_from_oracle(P0, "Rakdos Charm", true, RAKDOS_CHARM_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let mut runner = scenario.build();

    runner.cast(squire).resolve();
    runner.cast(bear).resolve();
    runner.cast(way).resolve();
    let WaitingFor::DamageSourceChoice { .. } = runner.state().waiting_for.clone() else {
        panic!(
            "New Way Forward must ask for its damage source, got {:?}",
            runner.state().waiting_for
        );
    };
    runner
        .act(GameAction::ChooseDamageSource { source: bear })
        .expect("naming the Bear must be answerable");
    let battlefield_creatures: Vec<ObjectId> = runner
        .state()
        .battlefield
        .iter()
        .filter(|id| runner.state().objects[id].power.is_some())
        .copied()
        .collect();
    assert_eq!(
        battlefield_creatures,
        vec![squire, bear],
        "reach guard: the batch must iterate Squire first, Bear last"
    );

    let life_before = runner.state().players[P0.0 as usize].life;
    let hand_before = runner
        .state()
        .objects
        .values()
        .filter(|o| o.controller == P0 && o.zone == Zone::Hand)
        .count();
    let mut log: Vec<GameEvent> = Vec::new();
    let outcome = runner.cast(charm).modes(&[2]).resolve();
    log.extend(outcome.events().iter().cloned());
    let WaitingFor::ReplacementChoice { candidates, .. } = outcome.final_waiting_for() else {
        panic!(
            "the Bear event must pause for a CR 616.1 ordering choice, got {:?}",
            outcome.final_waiting_for()
        );
    };
    assert_eq!(
        candidates.len(),
        2,
        "the Bear event must offer both shields as ordering candidates"
    );
    let way_index = candidates
        .iter()
        .position(|c| c.source_name.is_empty())
        .expect("one candidate must be New Way Forward's floating shield");
    assert_eq!(
        runner.state().pending_each_source_batches.len(),
        1,
        "the pause must park exactly one batch record"
    );

    // Answer with New Way Forward: the batch prevents under TWO shields (two
    // applications), and NWF's rider reflects 1 to P0 — still shielded — plus
    // draws 1.
    let result = runner
        .act(GameAction::ChooseReplacement { index: way_index })
        .expect("the ordering choice must be answerable");
    log.extend(result.events);
    runner.advance_until_stack_empty();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must complete to a clean priority window, got {:?}",
        runner.state().waiting_for
    );

    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "all damage (batch + reflected) must be prevented"
    );
    assert_eq!(
        prevented_pairs(&log),
        vec![
            (TargetRef::Player(P0), 1),
            (TargetRef::Player(P0), 1),
            (TargetRef::Player(P0), 1),
        ],
        "two batch applications plus the separate reflected prevention"
    );
    assert_eq!(
        counters_on(&runner, squire),
        6,
        "three triggers of 1, each +1 from Hardened Scales (a lost reflection would make 4)"
    );
    assert_eq!(
        runner
            .state()
            .objects
            .values()
            .filter(|o| o.controller == P0 && o.zone == Zone::Hand)
            .count(),
        hand_before,
        "the Charm left the hand and New Way Forward's rider drew exactly 1"
    );
    assert!(
        runner.state().pending_each_source_batches.is_empty(),
        "the pause record must be consumed by the completion cascade"
    );
}

/// Pass priority until `player` holds it (bounded; the loop returns the
/// moment the target holds priority, so the phase can never advance past an
/// empty stack).
fn pass_priority_to(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..4 {
        if runner.state().priority_player == player {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority must succeed");
    }
    panic!("priority never reached {player:?}");
}

/// CR 615.13 + CR 616.1e + CR 117.2e (Matt C2): aggregate drainage must
/// return completion versus a FRESH rider-created choice, and the drain hooks
/// must propagate it instead of the cached Priority.
///
/// P1 Bear first, P0 Squire last; P1 holds NWF naming Bear plus Abundance;
/// P0 holds the Squire ETB shield plus Safe Passage. The Bear prefix prevents
/// under NWF alone (no pause); the final Squire event pauses between the two
/// P0 shields. Answering Squire completes the batch with no raw tail, so the
/// answer hook drains the prefix — and NWF's rider draws P1 a card, which
/// Abundance replaces with a live child choice. Pre-fix the hook overwrote
/// that child with the cached Priority, stranding the draw.
#[test]
fn rakdos_charm_rider_draw_choice_propagates_through_drain_hooks() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    // Bear enters first (battlefield order = batch order: Bear, then Squire).
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    scenario.add_enchantment_from_oracle(P1, "Abundance", ABUNDANCE_TEXT);
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES_TEXT);
    let way = add_instant_from_oracle(&mut scenario, P1, "New Way Forward", NEW_WAY_FORWARD_TEXT);
    let squire = scenario
        .add_creature_to_hand_from_oracle(P0, "Selfless Squire", 1, 1, SELFLESS_SQUIRE_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let passage = add_instant_from_oracle(&mut scenario, P0, "Safe Passage", SAFE_PASSAGE_TEXT);
    let charm = scenario
        .add_spell_to_hand_from_oracle(P0, "Rakdos Charm", true, RAKDOS_CHARM_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let mut runner = scenario.build();

    // NWF resolves BEFORE the Squire's ETB shield exists, so the NWF
    // aggregate group drains first and its rider's draw pauses ahead of the
    // Squire group — leaving a parked remainder behind.
    pass_priority_to(&mut runner, P1);
    runner.cast(way).resolve();
    let WaitingFor::DamageSourceChoice { .. } = runner.state().waiting_for.clone() else {
        panic!(
            "New Way Forward must ask for its damage source, got {:?}",
            runner.state().waiting_for
        );
    };
    runner
        .act(GameAction::ChooseDamageSource { source: bear })
        .expect("naming the Bear must be answerable");
    pass_priority_to(&mut runner, P0);
    runner.cast(squire).resolve();
    runner.cast(passage).resolve();
    let battlefield_creatures: Vec<ObjectId> = runner
        .state()
        .battlefield
        .iter()
        .filter(|id| runner.state().objects[id].power.is_some())
        .copied()
        .collect();
    assert_eq!(
        battlefield_creatures,
        vec![bear, squire],
        "reach guard: the batch must iterate Bear first, Squire last"
    );

    let p0_life_before = runner.state().players[P0.0 as usize].life;
    let p1_life_before = runner.state().players[P1.0 as usize].life;
    let p1_hand_before = runner.state().players[P1.0 as usize].hand.len();
    let p1_library_before = runner.state().players[P1.0 as usize].library.len();
    let mut log: Vec<GameEvent> = Vec::new();
    let outcome = runner.cast(charm).modes(&[2]).resolve();
    log.extend(outcome.events().iter().cloned());
    let WaitingFor::ReplacementChoice { candidates, .. } = outcome.final_waiting_for() else {
        panic!(
            "the Squire event must pause for a CR 616.1 ordering choice, got {:?}",
            outcome.final_waiting_for()
        );
    };
    assert_eq!(
        candidates.len(),
        2,
        "the Squire event must offer both P0 shields as ordering candidates"
    );
    let squire_index = candidates
        .iter()
        .position(|c| c.source_name.contains("Selfless Squire"))
        .expect("one candidate must be the Squire's ETB shield");

    // Answer Squire: with no raw tail the answer hook drains the prefix, and
    // NWF's rider draw must surface Abundance's live child choice — NOT the
    // cached Priority.
    let result = runner
        .act(GameAction::ChooseReplacement {
            index: squire_index,
        })
        .expect("the ordering choice must be answerable");
    log.extend(result.events);
    let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the rider's Abundance choice must propagate (not cached Priority), got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        candidates
            .iter()
            .map(|c| c.description.as_str())
            .collect::<Vec<_>>(),
        vec!["Accept", "Decline"],
        "the live child must be Abundance's optional replacement"
    );
    // The Squire group was still unpublished when the NWF rider paused: the
    // remainder stays parked BELOW (parent frames stop unwinding).
    assert_eq!(
        runner.state().pending_each_source_batches.len(),
        1,
        "the unpublished Squire group must stay parked as a remainder"
    );
    assert_eq!(
        runner.state().pending_each_source_batches[0]
            .tally
            .iter()
            .map(|entry| entry.amount)
            .sum::<i32>(),
        1,
        "the remainder must hold the Squire group's 1 prevented"
    );

    // Decline through production actions: the natural draw resolves, the
    // rider chain completes, and the cascade publishes the remainder.
    let result = runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("declining Abundance must be answerable");
    log.extend(result.events);
    runner.advance_until_stack_empty();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must complete to a clean priority window, got {:?}",
        runner.state().waiting_for
    );

    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        p0_life_before,
        "P0's damage was prevented"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        p1_life_before - 1,
        "NWF reflected 1 to P1 with no shield to stop it"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].hand.len(),
        p1_hand_before + 1,
        "the rider's draw must resolve (declined Abundance draws naturally)"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].library.len(),
        p1_library_before - 1,
        "exactly one card drawn from P1's library"
    );
    assert_eq!(
        prevented_pairs(&log),
        vec![(TargetRef::Player(P1), 1), (TargetRef::Player(P0), 1)],
        "NWF application first, Squire remainder after the child answer"
    );
    assert_eq!(
        counters_on(&runner, squire),
        2,
        "one Squire trigger of 1, +1 from Hardened Scales"
    );
    assert!(
        runner.state().pending_each_source_batches.is_empty(),
        "the remainder must be consumed by the completion cascade"
    );
}

/// CR 615.13 + CR 616.1e persistence (Matt C2): the rider-draw control above,
/// save/loaded THROUGH Abundance's child prompt with the remainder parked —
/// the remainder, the paused rider dispatch, and the child prompt all survive
/// the authoritative round trip, and declining after restore completes
/// identically.
#[test]
fn rakdos_charm_rider_draw_choice_survives_save_load() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    scenario.add_enchantment_from_oracle(P1, "Abundance", ABUNDANCE_TEXT);
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES_TEXT);
    let way = add_instant_from_oracle(&mut scenario, P1, "New Way Forward", NEW_WAY_FORWARD_TEXT);
    let squire = scenario
        .add_creature_to_hand_from_oracle(P0, "Selfless Squire", 1, 1, SELFLESS_SQUIRE_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let passage = add_instant_from_oracle(&mut scenario, P0, "Safe Passage", SAFE_PASSAGE_TEXT);
    let charm = scenario
        .add_spell_to_hand_from_oracle(P0, "Rakdos Charm", true, RAKDOS_CHARM_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let mut runner = scenario.build();

    pass_priority_to(&mut runner, P1);
    runner.cast(way).resolve();
    runner
        .act(GameAction::ChooseDamageSource { source: bear })
        .expect("naming the Bear must be answerable");
    pass_priority_to(&mut runner, P0);
    runner.cast(squire).resolve();
    runner.cast(passage).resolve();

    let p1_life_before = runner.state().players[P1.0 as usize].life;
    let p1_hand_before = runner.state().players[P1.0 as usize].hand.len();
    let p1_library_before = runner.state().players[P1.0 as usize].library.len();
    let mut log: Vec<GameEvent> = Vec::new();
    let outcome = runner.cast(charm).modes(&[2]).resolve();
    log.extend(outcome.events().iter().cloned());
    let WaitingFor::ReplacementChoice { candidates, .. } = outcome.final_waiting_for() else {
        panic!(
            "the Squire event must pause for a CR 616.1 ordering choice, got {:?}",
            outcome.final_waiting_for()
        );
    };
    let squire_index = candidates
        .iter()
        .position(|c| c.source_name.contains("Selfless Squire"))
        .expect("one candidate must be the Squire's ETB shield");
    let result = runner
        .act(GameAction::ChooseReplacement {
            index: squire_index,
        })
        .expect("the ordering choice must be answerable");
    log.extend(result.events);

    // Save/load THROUGH the Abundance child prompt with the remainder parked.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "reach guard: the Abundance child prompt must be live, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().pending_each_source_batches.len(),
        1,
        "reach guard: the Squire remainder must be parked"
    );
    let saved = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("the authoritative paused state serializes");
    let restored: PersistedGameState =
        serde_json::from_str(&saved).expect("the authoritative paused state deserializes");
    let mut runner = GameRunner::from_state(
        restored
            .into_game_state()
            .expect("persisted test snapshot satisfies the checked restore contract"),
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the Abundance child prompt must survive restore, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().pending_each_source_batches.len(),
        1,
        "the parked remainder must survive authoritative restore"
    );
    assert_eq!(
        runner.state().pending_each_source_batches[0]
            .tally
            .iter()
            .map(|entry| entry.amount)
            .sum::<i32>(),
        1,
        "the remainder (Squire group's 1 prevented) must survive restore"
    );

    let result = runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("declining Abundance must be answerable after restore");
    log.extend(result.events);
    runner.advance_until_stack_empty();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must complete to a clean priority window, got {:?}",
        runner.state().waiting_for
    );

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        p1_life_before - 1,
        "NWF reflected 1 to P1 with no shield to stop it"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].hand.len(),
        p1_hand_before + 1,
        "the rider's draw must resolve after restore"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].library.len(),
        p1_library_before - 1,
        "exactly one card drawn from P1's library"
    );
    assert_eq!(
        prevented_pairs(&log),
        vec![(TargetRef::Player(P1), 1), (TargetRef::Player(P0), 1)],
        "NWF application first, Squire remainder after the child answer"
    );
    assert_eq!(
        counters_on(&runner, squire),
        2,
        "one Squire trigger of 1, +1 from Hardened Scales"
    );
    assert!(
        runner.state().pending_each_source_batches.is_empty(),
        "the remainder must be consumed by the completion cascade"
    );
}

/// CR 615.13 + CR 616.1e persistence: the nonempty-prefix pause above,
/// save/loaded THROUGH the ordering prompt (authoritative capture form) and
/// resumed — the parked prefix survives the round trip, so the resumed batch
/// still publishes ONE aggregate application (one event naming 2, 3
/// counters), not a prefix-less resumed fragment plus a lost prefix.
#[test]
fn rakdos_charm_paused_prefix_survives_save_load() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let squire = scenario
        .add_creature_to_hand_from_oracle(P0, "Selfless Squire", 1, 1, SELFLESS_SQUIRE_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let bear = scenario
        .add_creature_to_hand(P0, "Bear", 2, 2)
        .with_mana_cost(free_cost())
        .id();
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES_TEXT);
    let way = add_instant_from_oracle(&mut scenario, P0, "New Way Forward", NEW_WAY_FORWARD_TEXT);
    let charm = scenario
        .add_spell_to_hand_from_oracle(P0, "Rakdos Charm", true, RAKDOS_CHARM_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let mut runner = scenario.build();

    runner.cast(squire).resolve();
    runner.cast(bear).resolve();
    runner.cast(way).resolve();
    runner
        .act(GameAction::ChooseDamageSource { source: bear })
        .expect("naming the Bear must be answerable");

    let life_before = runner.state().players[P0.0 as usize].life;
    let mut log: Vec<GameEvent> = Vec::new();
    let outcome = runner.cast(charm).modes(&[2]).resolve();
    log.extend(outcome.events().iter().cloned());
    let WaitingFor::ReplacementChoice { candidates, .. } = outcome.final_waiting_for() else {
        panic!(
            "the Bear event must pause for a CR 616.1 ordering choice, got {:?}",
            outcome.final_waiting_for()
        );
    };
    let squire_index = candidates
        .iter()
        .position(|c| c.source_name.contains("Selfless Squire"))
        .expect("one candidate must be the Squire's ETB shield");

    // Save/load THROUGH the pause: the parked prefix (1 prevented under the
    // Squire shield) must survive the authoritative round trip.
    assert_eq!(
        runner.state().pending_each_source_batches.len(),
        1,
        "reach guard: the pause must park exactly one batch record"
    );
    let saved = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("the authoritative paused state serializes");
    let restored: PersistedGameState =
        serde_json::from_str(&saved).expect("the authoritative paused state deserializes");
    let mut runner = GameRunner::from_state(
        restored
            .into_game_state()
            .expect("persisted test snapshot satisfies the checked restore contract"),
    );
    assert_eq!(
        runner.state().pending_each_source_batches.len(),
        1,
        "the parked batch record must survive authoritative restore"
    );
    assert_eq!(
        runner.state().pending_each_source_batches[0]
            .tally
            .iter()
            .map(|entry| entry.amount)
            .sum::<i32>(),
        1,
        "the parked prefix (Squire's 1 prevented) must survive restore"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the ordering prompt must survive restore, got {:?}",
        runner.state().waiting_for
    );

    let result = runner
        .act(GameAction::ChooseReplacement {
            index: squire_index,
        })
        .expect("the ordering choice must be answerable after restore");
    log.extend(result.events);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "both damage must be prevented (none dealt through)"
    );
    assert_eq!(
        prevented_pairs(&log),
        vec![(TargetRef::Player(P0), 2)],
        "restored prefix + resumed entry must publish as ONE aggregate application"
    );
    assert_eq!(
        counters_on(&runner, squire),
        3,
        "one trigger placing 2 at once, +1 from Hardened Scales"
    );
}

/// CR 615.13 + CR 615.5 + CR 616.1e (Matt M1): parked prevention batches bind
/// to their owning continuation, and the nested child's application publishes
/// at the child's batch boundary — BEFORE the outer tail resumes.
///
/// Typed (not printed-card) nested composition: no printed card opens an
/// `EachSourceDealsDamage` batch from an inline prevention rider, so the outer
/// batch and the rider are built as typed AST and resolved through the
/// production pipeline:
///   * outer batch O: each Bear you control deals damage equal to its power to
///     its controller (Bear1 1/1, Bear2 2/2, Bear3 3/3; P0 takes it all);
///   * Bear2's shield is a production New Way Forward install whose rider slot
///     carries a typed inline `EachSourceDealsDamage` rider (each Bat you
///     control deals damage equal to its power to its controller), gated on
///     the prevented event's source being Bear2 (the Comeuppance inline shape);
///   * Bat2's shield is a second production NWF install (plain — its rider
///     never fires because Squire is ordered first and it prevents 0).
///
/// O pauses at Bear2 (Squire + NWF-Bear2). Answering NWF-Bear2 first applies
/// Bear2's prevention in the answer path, whose inline rider opens the nested
/// batch C while O's continuation is still queued; C pauses at Bat2 (Squire +
/// NWF-Bat2). Answering Squire first completes C, then O's downstream tail
/// (Bear3) resumes.
///
/// Pre-fix the outer tail hydrated the nested child's still-parked frame
/// (stack top), mixing O's downstream 3 into C's application: (P0,6) +
/// (P0,1) + (P0,2). Post-fix each application publishes under its own owner:
/// C's (P0,3) at C's boundary, then O's (P0,4) + (P0,2). The mid-nesting
/// save/load pins the parked stack + queued continuations (owners included)
/// across the authoritative round trip.
#[test]
fn typed_nested_batch_publishes_child_before_outer_tail() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let squire = scenario
        .add_creature_to_hand_from_oracle(P0, "Selfless Squire", 1, 1, SELFLESS_SQUIRE_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let bear1 = scenario
        .add_creature(P0, "Bear1", 1, 1)
        .with_subtypes(vec!["Bear"])
        .id();
    let bear2 = scenario
        .add_creature(P0, "Bear2", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    let bear3 = scenario
        .add_creature(P0, "Bear3", 3, 3)
        .with_subtypes(vec!["Bear"])
        .id();
    let bat1 = scenario
        .add_creature(P0, "Bat1", 1, 1)
        .with_subtypes(vec!["Bat"])
        .id();
    let bat2 = scenario
        .add_creature(P0, "Bat2", 2, 2)
        .with_subtypes(vec!["Bat"])
        .id();
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES_TEXT);
    let way_bear =
        add_instant_from_oracle(&mut scenario, P0, "New Way Forward", NEW_WAY_FORWARD_TEXT);
    let way_bat =
        add_instant_from_oracle(&mut scenario, P0, "New Way Forward", NEW_WAY_FORWARD_TEXT);
    let mut runner = scenario.build();

    runner.cast(squire).resolve();
    runner.cast(way_bear).resolve();
    runner
        .act(GameAction::ChooseDamageSource { source: bear2 })
        .expect("naming Bear2 must be answerable");
    runner.cast(way_bat).resolve();
    runner
        .act(GameAction::ChooseDamageSource { source: bat2 })
        .expect("naming Bat2 must be answerable");
    assert_eq!(
        runner.state().pending_damage_replacements.len(),
        3,
        "reach guard: the Squire ETB shield plus the two floating NWF shields"
    );
    assert!(
        runner.state().pending_damage_replacements[0]
            .damage_source_filter
            .is_none(),
        "reach guard: shield 0 is the sourceless Squire ETB shield"
    );
    let battlefield_creatures: Vec<ObjectId> = runner
        .state()
        .battlefield
        .iter()
        .filter(|id| runner.state().objects[id].power.is_some())
        .copied()
        .collect();
    assert_eq!(
        battlefield_creatures,
        vec![bear1, bear2, bear3, bat1, bat2, squire],
        "reach guard: batch iteration order is battlefield order"
    );

    // Swap NWF-Bear2's aggregate rider for the typed INLINE EachSource rider.
    // The shield (chosen-source Bear2, prevent-all, end-of-turn) stays
    // production-installed; only the rider slot carries typed AST, gated on
    // the prevented event's source (Comeuppance's inline marker).
    let bear2_filter = runner
        .state()
        .pending_damage_replacements
        .iter()
        .find(|r| {
            r.damage_source_filter
                .as_ref()
                .is_some_and(|f| matches!(f, TargetFilter::SpecificObject { id } if *id == bear2))
        })
        .expect("NWF-Bear2's installed shield must exist")
        .damage_source_filter
        .clone()
        .expect("a chosen-source shield binds its source");
    let mut rider_def = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::EachSourceDealsDamage {
            sources: TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Subtype("Bat".to_string()))
                    .controller(ControllerRef::You),
            ),
            amount: QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::BatchSource,
                },
            },
            recipient: EachDamageRecipient::EachController,
        },
    );
    rider_def.condition = Some(AbilityCondition::PostReplacementDamageSourceMatchesFilter {
        filter: bear2_filter,
    });
    let rider = build_resolved_from_def(&rider_def, way_bear, P0);
    runner
        .state_mut()
        .pending_damage_replacements
        .iter_mut()
        .find(|r| {
            r.damage_source_filter
                .as_ref()
                .is_some_and(|f| matches!(f, TargetFilter::SpecificObject { id } if *id == bear2))
        })
        .expect("NWF-Bear2's installed shield must exist")
        .runtime_execute = Some(Box::new(rider));

    // The typed outer batch: each Bear you control deals damage equal to its
    // power to its controller. Resolved directly (no printed card has this
    // shape); pauses still answer through the production action engine.
    let outer_def = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::EachSourceDealsDamage {
            sources: TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Subtype("Bear".to_string()))
                    .controller(ControllerRef::You),
            ),
            amount: QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::BatchSource,
                },
            },
            recipient: EachDamageRecipient::EachController,
        },
    );
    let card_id = CardId(runner.state().next_object_id);
    let source = create_object(
        runner.state_mut(),
        card_id,
        P0,
        "Typed Outer Batch".to_string(),
        Zone::Stack,
    );
    let outer = build_resolved_from_def(&outer_def, source, P0);

    let life_before = runner.state().players[P0.0 as usize].life;
    let hand_before = runner
        .state()
        .objects
        .values()
        .filter(|o| o.controller == P0 && o.zone == Zone::Hand)
        .count();
    let mut log: Vec<GameEvent> = Vec::new();
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &outer, &mut events, 0)
        .expect("the typed outer batch resolves to its pause");
    log.extend(events);
    let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the Bear2 event must pause for a CR 616.1 ordering choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        candidates.len(),
        2,
        "the Bear2 event must offer both shields as ordering candidates"
    );
    assert!(
        candidates
            .iter()
            .any(|c| c.source_name.contains("Selfless Squire")),
        "one candidate must be the Squire's ETB shield"
    );
    let way_index = candidates
        .iter()
        .position(|c| c.source_name.is_empty())
        .expect("the other candidate must be NWF-Bear2's floating shield");
    assert_eq!(
        runner.state().pending_each_source_batches.len(),
        1,
        "the pause must park exactly one batch record"
    );
    assert_eq!(
        runner.state().pending_each_source_batches[0]
            .tally
            .iter()
            .map(|entry| entry.amount)
            .sum::<i32>(),
        1,
        "the parked prefix must hold the Squire's 1 prevented (Bear1)"
    );

    // Answer NWF-Bear2 first: Bear2's prevention applies in the answer path
    // and the inline rider opens the nested batch C, which pauses at Bat2.
    let result = runner
        .act(GameAction::ChooseReplacement { index: way_index })
        .expect("the ordering choice must be answerable");
    log.extend(result.events);
    let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the nested Bat2 event must pause for its own ordering choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        candidates.len(),
        2,
        "the Bat2 event must offer both shields as ordering candidates"
    );
    let squire_index_before = candidates
        .iter()
        .position(|c| c.source_name.contains("Selfless Squire"))
        .expect("one candidate must be the Squire's ETB shield");
    assert!(
        candidates.iter().any(|c| c.source_name.is_empty()),
        "the other candidate must be NWF-Bat2's floating shield"
    );
    assert_eq!(
        runner.state().pending_each_source_batches.len(),
        3,
        "the nesting must park outer prefix + child scope + child prefix"
    );
    let parked_sums: Vec<i32> = runner
        .state()
        .pending_each_source_batches
        .iter()
        .map(|frame| frame.tally.iter().map(|entry| entry.amount).sum())
        .collect();
    assert_eq!(
        parked_sums,
        vec![3, 0, 1],
        "outer prefix holds Squire 1 + NWF-Bear2 2, child scope is empty, \
         child prefix holds Squire 1 (Bat1)"
    );
    let parked_owners: Vec<u64> = runner
        .state()
        .pending_each_source_batches
        .iter()
        .map(|frame| frame.owner)
        .collect();
    assert_ne!(
        parked_owners[0], 0,
        "the outer prefix must carry a minted owner, not the legacy sentinel"
    );
    assert_eq!(
        parked_owners[1], parked_owners[2],
        "the child scope and child prefix must share the child's owner"
    );
    assert_ne!(
        parked_owners[0], parked_owners[1],
        "the nested child must not share attribution with its outer"
    );
    assert_eq!(
        runner
            .state()
            .pending_replacement
            .as_ref()
            .and_then(|pending| pending.each_source_batch_owner),
        Some(parked_owners[2]),
        "the nested prompt must carry the child's boundary stamp (its tail is empty)"
    );

    // Save/load THROUGH the nested pause: the parked stack and the queued
    // continuations (owners included) must survive the authoritative round
    // trip and resume identically.
    let saved = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("the authoritative paused state serializes");
    let restored: PersistedGameState =
        serde_json::from_str(&saved).expect("the authoritative paused state deserializes");
    let mut runner = GameRunner::from_state(
        restored
            .into_game_state()
            .expect("persisted test snapshot satisfies the checked restore contract"),
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the nested ordering prompt must survive restore, got {:?}",
        runner.state().waiting_for
    );
    let restored_sums: Vec<i32> = runner
        .state()
        .pending_each_source_batches
        .iter()
        .map(|frame| frame.tally.iter().map(|entry| entry.amount).sum())
        .collect();
    assert_eq!(
        restored_sums,
        vec![3, 0, 1],
        "the parked nesting must survive authoritative restore"
    );
    assert_eq!(
        runner
            .state()
            .pending_each_source_batches
            .iter()
            .map(|frame| frame.owner)
            .collect::<Vec<_>>(),
        parked_owners,
        "the frame owners must survive authoritative restore verbatim"
    );
    assert_eq!(
        runner
            .state()
            .pending_replacement
            .as_ref()
            .and_then(|pending| pending.each_source_batch_owner),
        Some(parked_owners[2]),
        "the boundary stamp must survive authoritative restore verbatim"
    );

    // Answer Squire first: NWF-Bat2 prevents 0 (its rider stays silent),
    // C completes and publishes at C's boundary, then O's downstream tail
    // (Bear3) resumes and O publishes.
    let squire_index = match runner.state().waiting_for.clone() {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates
            .iter()
            .position(|c| c.source_name.contains("Selfless Squire"))
            .expect("one candidate must be the Squire's ETB shield"),
        other => panic!("expected the nested ordering prompt, got {other:?}"),
    };
    assert_eq!(
        squire_index, squire_index_before,
        "candidate order must survive restore"
    );
    let result = runner
        .act(GameAction::ChooseReplacement {
            index: squire_index,
        })
        .expect("the nested ordering choice must be answerable after restore");
    log.extend(result.events);
    runner.advance_until_stack_empty();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must complete to a clean priority window, got {:?}",
        runner.state().waiting_for
    );

    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "all damage must be prevented (none dealt through)"
    );
    assert_eq!(
        prevented_pairs(&log),
        vec![
            (TargetRef::Player(P0), 3),
            (TargetRef::Player(P0), 4),
            (TargetRef::Player(P0), 2),
        ],
        "child (P0,3) publishes at the child's boundary before the outer tail \
         resumes; outer publishes (P0,4) + (P0,2)"
    );
    assert_eq!(
        counters_on(&runner, squire),
        12,
        "three triggers of 3/4/2, each +1 from Hardened Scales"
    );
    assert_eq!(
        runner
            .state()
            .objects
            .values()
            .filter(|o| o.controller == P0 && o.zone == Zone::Hand)
            .count(),
        hand_before,
        "NWF-Bat2's rider must stay silent (its shield prevented nothing): \
         nothing drawn"
    );
    assert!(
        runner.state().pending_each_source_batches.is_empty(),
        "every parked record must be consumed by the completion cascade"
    );
}

/// CR 615.13 + CR 616.1e (Matt F3): a nested batch that opens synchronously
/// inside an already-owned OUTER TAIL must accumulate into its OWN frame —
/// never into the outer persisted record selected by the ambient drain
/// owner. The existing nested control starts its child from the
/// replacement-answer hook (ambient owner absent) and misses this branch.
///
/// P0 controls Bear1 (1/1), Bear2 (2/2), Bat1 (1/1), Bat2 (2/2), the Squire
/// and Scales; P1 controls Bear3 (3/3). P0's NWF names Bear1 (rider nulled
/// to isolate the shield from its printed reflect/draw halves); P1's NWF
/// names Bear3 and carries the typed INLINE EachSource rider over P0's
/// Bats. The typed outer batch (all Bears, no controller filter — the
/// Rakdos Charm shape — power each to its controller) pauses at Bear1
/// (Squire + NWF-Bear1). Answering NWF-Bear1 resumes the owned tail: Bear2
/// prevents under the sole Squire shield, Bear3 prevents under P1's sole
/// NWF shield, and that prevention fires the inline rider — opening the
/// child while the outer tail's owner is ambient. The child sees only the
/// Squire shield (the NWFs name Bears) and completes synchronously.
///
/// Pre-fix the child's write-through lands in the outer record (ambient
/// owner A): the outer flush republishes the child's (P0,3) and loses the
/// outer application — `[(P0,3),(P0,3)]`, eight counters. Post-fix the
/// child publishes (P0,3) at its own boundary and the outer publishes its
/// three applications — nine counters ((3+1)+(2+1)+(1+1); the P1
/// application triggers nothing for P0's Squire).
#[test]
fn typed_nested_batch_owned_tail_binds_child_owner() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let squire = scenario
        .add_creature_to_hand_from_oracle(P0, "Selfless Squire", 1, 1, SELFLESS_SQUIRE_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let bear1 = scenario
        .add_creature(P0, "Bear1", 1, 1)
        .with_subtypes(vec!["Bear"])
        .id();
    let bear2 = scenario
        .add_creature(P0, "Bear2", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    let bear3 = scenario
        .add_creature(P1, "Bear3", 3, 3)
        .with_subtypes(vec!["Bear"])
        .id();
    let bat1 = scenario
        .add_creature(P0, "Bat1", 1, 1)
        .with_subtypes(vec!["Bat"])
        .id();
    let bat2 = scenario
        .add_creature(P0, "Bat2", 2, 2)
        .with_subtypes(vec!["Bat"])
        .id();
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES_TEXT);
    let way_bear1 =
        add_instant_from_oracle(&mut scenario, P0, "New Way Forward", NEW_WAY_FORWARD_TEXT);
    let way_bear3 =
        add_instant_from_oracle(&mut scenario, P1, "New Way Forward", NEW_WAY_FORWARD_TEXT);
    let mut runner = scenario.build();

    runner.cast(squire).resolve();
    runner.cast(way_bear1).resolve();
    runner
        .act(GameAction::ChooseDamageSource { source: bear1 })
        .expect("naming Bear1 must be answerable");
    pass_priority_to(&mut runner, P1);
    runner.cast(way_bear3).resolve();
    runner
        .act(GameAction::ChooseDamageSource { source: bear3 })
        .expect("naming Bear3 must be answerable");
    pass_priority_to(&mut runner, P0);
    assert_eq!(
        runner.state().pending_damage_replacements.len(),
        3,
        "reach guard: the Squire ETB shield plus the two NWF shields"
    );
    assert!(
        runner.state().pending_damage_replacements[0]
            .damage_source_filter
            .is_none(),
        "reach guard: shield 0 is the sourceless Squire ETB shield"
    );
    let battlefield_creatures: Vec<ObjectId> = runner
        .state()
        .battlefield
        .iter()
        .filter(|id| runner.state().objects[id].power.is_some())
        .copied()
        .collect();
    assert_eq!(
        battlefield_creatures,
        vec![bear1, bear2, bear3, bat1, bat2, squire],
        "reach guard: batch iteration order is battlefield order"
    );

    // Isolate NWF-Bear1's shield from its printed reflect/draw rider: the
    // rider halves are orthogonal to the owner-binding discriminator and
    // would add reflection-prevention noise to the published sequence.
    runner
        .state_mut()
        .pending_damage_replacements
        .iter_mut()
        .find(|r| {
            r.damage_source_filter
                .as_ref()
                .is_some_and(|f| matches!(f, TargetFilter::SpecificObject { id } if *id == bear1))
        })
        .expect("NWF-Bear1's installed shield must exist")
        .runtime_execute = None;

    // Swap NWF-Bear3's aggregate rider for the typed INLINE EachSource rider
    // over P0's Bats. The shield (chosen-source Bear3, P1's "damage to you")
    // stays production-installed; only the rider slot carries typed AST,
    // gated on the prevented event's source (Comeuppance's inline marker).
    // The rider's controller is P0 so `You` binds P0's Bats.
    let bear3_filter = runner
        .state()
        .pending_damage_replacements
        .iter()
        .find(|r| {
            r.damage_source_filter
                .as_ref()
                .is_some_and(|f| matches!(f, TargetFilter::SpecificObject { id } if *id == bear3))
        })
        .expect("NWF-Bear3's installed shield must exist")
        .damage_source_filter
        .clone()
        .expect("a chosen-source shield binds its source");
    let mut rider_def = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::EachSourceDealsDamage {
            sources: TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Subtype("Bat".to_string()))
                    .controller(ControllerRef::You),
            ),
            amount: QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::BatchSource,
                },
            },
            recipient: EachDamageRecipient::EachController,
        },
    );
    rider_def.condition = Some(AbilityCondition::PostReplacementDamageSourceMatchesFilter {
        filter: bear3_filter,
    });
    let rider = build_resolved_from_def(&rider_def, way_bear3, P0);
    runner
        .state_mut()
        .pending_damage_replacements
        .iter_mut()
        .find(|r| {
            r.damage_source_filter
                .as_ref()
                .is_some_and(|f| matches!(f, TargetFilter::SpecificObject { id } if *id == bear3))
        })
        .expect("NWF-Bear3's installed shield must exist")
        .runtime_execute = Some(Box::new(rider));

    // The typed outer batch: each Bear (both players' — no controller
    // filter, the printed Rakdos Charm shape) deals damage equal to its
    // power to its controller. Resolved directly (no printed card has this
    // shape); pauses still answer through the production action engine.
    let outer_def = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::EachSourceDealsDamage {
            sources: TargetFilter::Typed(TypedFilter::new(TypeFilter::Subtype("Bear".to_string()))),
            amount: QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::BatchSource,
                },
            },
            recipient: EachDamageRecipient::EachController,
        },
    );
    let card_id = CardId(runner.state().next_object_id);
    let source = create_object(
        runner.state_mut(),
        card_id,
        P0,
        "Typed Outer Batch".to_string(),
        Zone::Stack,
    );
    let outer = build_resolved_from_def(&outer_def, source, P0);

    let p0_life_before = runner.state().players[P0.0 as usize].life;
    let p1_life_before = runner.state().players[P1.0 as usize].life;
    let p0_hand_before = runner.state().players[P0.0 as usize].hand.len();
    let p1_hand_before = runner.state().players[P1.0 as usize].hand.len();
    let mut log: Vec<GameEvent> = Vec::new();
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &outer, &mut events, 0)
        .expect("the typed outer batch resolves to its pause");
    log.extend(events);
    let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the Bear1 event must pause for a CR 616.1 ordering choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        candidates.len(),
        2,
        "the Bear1 event must offer the Squire and NWF-Bear1 shields"
    );
    assert!(
        candidates
            .iter()
            .any(|c| c.source_name.contains("Selfless Squire")),
        "one candidate must be the Squire's ETB shield"
    );
    let way_index = candidates
        .iter()
        .position(|c| c.source_name.is_empty())
        .expect("the other candidate must be NWF-Bear1's floating shield");
    assert_eq!(
        runner.state().pending_each_source_batches.len(),
        1,
        "the pause must park exactly one batch record"
    );
    assert_eq!(
        runner.state().pending_each_source_batches[0]
            .tally
            .iter()
            .map(|entry| entry.amount)
            .sum::<i32>(),
        0,
        "Bear1 is first: the parked prefix is empty"
    );
    let outer_owner = runner.state().pending_each_source_batches[0].owner;
    assert_ne!(
        outer_owner, 0,
        "the outer prefix must carry a minted owner, not the legacy sentinel"
    );
    assert_eq!(
        runner
            .state()
            .pending_replacement
            .as_ref()
            .and_then(|pending| pending.each_source_batch_owner),
        None,
        "the non-empty tail resumes via its stamped continuation, not a prompt stamp"
    );

    // Save/load THROUGH the outer pause: the parked record (owner included)
    // must survive the authoritative round trip and resume identically.
    let saved = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("the authoritative paused state serializes");
    let restored: PersistedGameState =
        serde_json::from_str(&saved).expect("the authoritative paused state deserializes");
    let mut runner = GameRunner::from_state(
        restored
            .into_game_state()
            .expect("persisted test snapshot satisfies the checked restore contract"),
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the ordering prompt must survive restore, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner
            .state()
            .pending_each_source_batches
            .iter()
            .map(|frame| frame.tally.iter().map(|entry| entry.amount).sum::<i32>())
            .collect::<Vec<_>>(),
        vec![0],
        "the parked empty prefix must survive authoritative restore"
    );
    assert_eq!(
        runner
            .state()
            .pending_each_source_batches
            .iter()
            .map(|frame| frame.owner)
            .collect::<Vec<_>>(),
        vec![outer_owner],
        "the frame owner must survive authoritative restore verbatim"
    );

    // Answer NWF-Bear1 first: Bear1's prevention applies in the answer path
    // and the owned tail resumes — Bear2 under the sole Squire shield, then
    // Bear3 under P1's sole NWF shield, whose inline rider opens the nested
    // Bat batch synchronously inside the owned tail.
    let result = runner
        .act(GameAction::ChooseReplacement { index: way_index })
        .expect("the ordering choice must be answerable after restore");
    log.extend(result.events);
    runner.advance_until_stack_empty();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must complete to a clean priority window, got {:?}",
        runner.state().waiting_for
    );

    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        p0_life_before,
        "Bear1 (NWF) and Bear2 (Squire) must be prevented; the nulled NWF-Bear1 rider reflects nothing"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        p1_life_before,
        "Bear3 (NWF-Bear3) must be prevented; the swapped rider reflects nothing"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        p0_hand_before,
        "the nulled NWF-Bear1 rider must draw nothing"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].hand.len(),
        p1_hand_before,
        "the swapped NWF-Bear3 rider must draw nothing"
    );
    assert_eq!(
        prevented_pairs(&log),
        vec![
            (TargetRef::Player(P0), 3),
            (TargetRef::Player(P0), 2),
            (TargetRef::Player(P0), 1),
            (TargetRef::Player(P1), 3),
        ],
        "child (P0,3) publishes at the child's boundary before the outer tail \
         resumes; outer publishes (P0,2) + (P0,1) + (P1,3) in sorted order"
    );
    assert_eq!(
        counters_on(&runner, squire),
        9,
        "three P0 triggers of 3/2/1, each +1 from Hardened Scales; the P1 application triggers nothing"
    );
    assert!(
        runner.state().pending_each_source_batches.is_empty(),
        "every parked record must be consumed by the completion cascade"
    );
}

/// CR 615.13 + CR 608.2c (Matt R2-F1): a genuine downstream instruction must
/// NOT join the simultaneous batch's prevention application. Typed root:
/// EachSource batch (two Bears, 1 each to P0) with a real `sub_ability`
/// dealing 3 to P0. The batch pauses once (Squire shield + NWF-Bear1 shield
/// both apply to Bear1); after save/load, answering Squire prevents Bear1,
/// Bear2 follows silently, the batch publishes (P0,2) at its own boundary,
/// then the downstream 3 prevents separately as (P0,3). Hardened Scales
/// discriminates: 3+4=7 counters fixed, 6 (one 5-application) pre-fix.
#[test]
fn typed_batch_downstream_instruction_publishes_separate_application() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let squire = scenario
        .add_creature_to_hand_from_oracle(P0, "Selfless Squire", 1, 1, SELFLESS_SQUIRE_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let bear1 = scenario
        .add_creature(P0, "Bear1", 1, 1)
        .with_subtypes(vec!["Bear"])
        .id();
    let _bear2 = scenario
        .add_creature(P0, "Bear2", 1, 1)
        .with_subtypes(vec!["Bear"])
        .id();
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES_TEXT);
    let way = add_instant_from_oracle(&mut scenario, P0, "New Way Forward", NEW_WAY_FORWARD_TEXT);
    let mut runner = scenario.build();

    runner.cast(squire).resolve();
    runner.cast(way).resolve();
    runner
        .act(GameAction::ChooseDamageSource { source: bear1 })
        .expect("naming Bear1 must be answerable");
    assert_eq!(
        runner.state().pending_damage_replacements.len(),
        2,
        "reach guard: the Squire ETB shield plus the NWF-Bear1 shield"
    );

    // Typed root batch: each Bear deals power to its controller (1+1 to P0).
    let batch_def = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::EachSourceDealsDamage {
            sources: TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Subtype("Bear".to_string()))
                    .controller(ControllerRef::You),
            ),
            amount: QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::BatchSource,
                },
            },
            recipient: EachDamageRecipient::EachController,
        },
    );
    let card_id = CardId(runner.state().next_object_id);
    let source = create_object(
        runner.state_mut(),
        card_id,
        P0,
        "Typed Batch With Downstream".to_string(),
        Zone::Stack,
    );
    // Genuine downstream instruction, production-parsed: 3 to target player.
    let downstream_def = parse_oracle_text(
        "Downstream Burn deals 3 damage to target player.",
        "Downstream Burn",
        &[],
        &["Instant".to_string()],
        &[],
    )
    .abilities
    .into_iter()
    .next()
    .expect("the burn line must parse to one ability");
    let mut downstream = build_resolved_from_def(&downstream_def, source, P0);
    downstream.targets = vec![TargetRef::Player(P0)];
    let mut root = build_resolved_from_def(&batch_def, source, P0);
    root.sub_ability = Some(Box::new(downstream));

    let life_before = runner.state().players[P0.0 as usize].life;
    let hand_before = runner
        .state()
        .objects
        .values()
        .filter(|o| o.controller == P0 && o.zone == Zone::Hand)
        .count();
    let mut log: Vec<GameEvent> = Vec::new();
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &root, &mut events, 0)
        .expect("the typed batch resolves to its pause");
    log.extend(events);
    let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the Bear1 event must pause for a CR 616.1 ordering choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        candidates.len(),
        2,
        "the Bear1 event must offer both shields as ordering candidates"
    );
    assert_eq!(
        runner
            .state()
            .pending_each_source_batches
            .iter()
            .map(|frame| frame.tally.iter().map(|entry| entry.amount).sum::<i32>())
            .collect::<Vec<_>>(),
        vec![0],
        "the pause at the first source parks one prefix with nothing yet prevented"
    );
    let batch_owner = runner.state().pending_each_source_batches[0].owner;
    assert_ne!(
        batch_owner, 0,
        "the parked prefix must carry a minted owner, not the legacy sentinel"
    );
    // R2-F1 split-tails pin: the stamped remainder frame holds ONLY the
    // batch remainder (Bear2's node) — downstream parks as its own
    // ownerless frame below, never spliced into this chain.
    let top = runner
        .state()
        .active_ability_continuation()
        .expect("the pause must park the batch remainder as the active frame");
    assert_eq!(
        top.each_source_batch_owner,
        Some(batch_owner),
        "the active frame must be the stamped batch remainder"
    );
    assert!(
        matches!(&top.chain.effect, Effect::DealDamage { .. }),
        "the remainder frame must head with Bear2's DealDamage node, got {:?}",
        top.chain.effect
    );
    assert!(
        top.chain.sub_ability.is_none(),
        "downstream must not be spliced into the remainder chain (split tails)"
    );

    // Save/load THROUGH the pause: the parked frame, the queued split tails
    // (owned batch chain above ownerless downstream), and the unstamped
    // prompt must survive the authoritative round trip.
    let saved = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("the authoritative paused state serializes");
    let restored: PersistedGameState =
        serde_json::from_str(&saved).expect("the authoritative paused state deserializes");
    let mut runner = GameRunner::from_state(
        restored
            .into_game_state()
            .expect("persisted test snapshot satisfies the checked restore contract"),
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the ordering prompt must survive restore, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().pending_each_source_batches[0].owner,
        batch_owner,
        "the frame owner must survive authoritative restore verbatim"
    );
    assert_eq!(
        runner
            .state()
            .active_ability_continuation()
            .expect("the remainder frame must survive restore")
            .each_source_batch_owner,
        Some(batch_owner),
        "the remainder stamp must survive authoritative restore verbatim"
    );

    // Answer Squire: Bear1's 1 is prevented; NWF-Bear1 has nothing left to
    // prevent (no second prompt), Bear2 follows under the Squire silently,
    // the batch publishes (P0,2), then downstream prevents (P0,3).
    let squire_index = match runner.state().waiting_for.clone() {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates
            .iter()
            .position(|c| c.source_name.contains("Selfless Squire"))
            .expect("one candidate must be the Squire's ETB shield"),
        other => panic!("expected the ordering prompt, got {other:?}"),
    };
    let result = runner
        .act(GameAction::ChooseReplacement {
            index: squire_index,
        })
        .expect("the ordering choice must be answerable after restore");
    log.extend(result.events);
    runner.advance_until_stack_empty();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must complete to a clean priority window, got {:?}",
        runner.state().waiting_for
    );

    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "all damage must be prevented (none dealt through)"
    );
    assert_eq!(
        prevented_pairs(&log),
        vec![(TargetRef::Player(P0), 2), (TargetRef::Player(P0), 3)],
        "the batch publishes (P0,2) at its own boundary; the downstream \
         instruction prevents separately as (P0,3) — pre-fix they merge to (P0,5)"
    );
    assert_eq!(
        counters_on(&runner, squire),
        7,
        "two triggers of 2/3, each +1 from Hardened Scales (pre-fix: one 5-trigger -> 6)"
    );
    assert_eq!(
        runner
            .state()
            .objects
            .values()
            .filter(|o| o.controller == P0 && o.zone == Zone::Hand)
            .count(),
        hand_before,
        "NWF's rider must stay silent (its shield prevented nothing): nothing drawn"
    );
    assert!(
        runner.state().pending_each_source_batches.is_empty(),
        "every parked record must be consumed by the completion cascade"
    );
}
/// CR 616.1f + CR 615.13 + CR 616.1e (Matt R2-F2): an empty-tail batch owner
/// stamp must survive an ordinary second ordering choice for the same event.
/// The nested fixture plus a Bat2-only mandatory Double: the nested Bat2
/// prompt offers [Double, Squire, NWF-Bat2]; answering Double re-parks a
/// second same-event prompt. Pre-fix the re-park drops the child stamp, so
/// the Squire answer skips child-boundary publication and outer work resumes
/// with the child frame still parked. Post-fix the second prompt retains the
/// stamp (pinned across save/load) and the child publishes (P0,5) -- Bat1 1
/// plus the doubled Bat2 4 -- before the outer tail resumes; the outer
/// applications are unchanged from the nested control.
#[test]
fn typed_nested_batch_second_ordering_retains_child_stamp() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    let squire = scenario
        .add_creature_to_hand_from_oracle(P0, "Selfless Squire", 1, 1, SELFLESS_SQUIRE_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let bear1 = scenario
        .add_creature(P0, "Bear1", 1, 1)
        .with_subtypes(vec!["Bear"])
        .id();
    let bear2 = scenario
        .add_creature(P0, "Bear2", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    let bear3 = scenario
        .add_creature(P0, "Bear3", 3, 3)
        .with_subtypes(vec!["Bear"])
        .id();
    let bat1 = scenario
        .add_creature(P0, "Bat1", 1, 1)
        .with_subtypes(vec!["Bat"])
        .id();
    let bat2 = scenario
        .add_creature(P0, "Bat2", 2, 2)
        .with_subtypes(vec!["Bat"])
        .id();
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES_TEXT);
    let way_bear =
        add_instant_from_oracle(&mut scenario, P0, "New Way Forward", NEW_WAY_FORWARD_TEXT);
    let way_bat =
        add_instant_from_oracle(&mut scenario, P0, "New Way Forward", NEW_WAY_FORWARD_TEXT);
    let double_bat = add_instant_from_oracle(
        &mut scenario,
        P0,
        "Chosen Doubling",
        "The next time a source of your choice would deal damage to you this turn, it deals double that damage instead.",
    );
    let mut runner = scenario.build();
    runner.cast(squire).resolve();
    runner.cast(way_bear).resolve();
    runner
        .act(GameAction::ChooseDamageSource { source: bear2 })
        .expect("naming Bear2 must be answerable");
    runner.cast(way_bat).resolve();
    runner
        .act(GameAction::ChooseDamageSource { source: bat2 })
        .expect("naming Bat2 must be answerable");
    runner.cast(double_bat).resolve();
    runner
        .act(GameAction::ChooseDamageSource { source: bat2 })
        .expect("naming Bat2 for the doubler must be answerable");
    assert_eq!(
        runner.state().pending_damage_replacements.len(),
        4,
        "reach guard: Squire plus two NWF shields plus the Bat2-only Double"
    );
    let double_pos = runner
        .state()
        .pending_damage_replacements
        .iter()
        .position(|r| {
            matches!(
                r.damage_modification,
                Some(engine::types::ability::DamageModification::Double)
            ) && r
                .damage_source_filter
                .as_ref()
                .is_some_and(|f| matches!(f, TargetFilter::SpecificObject { id } if *id == bat2))
        })
        .expect("the Bat2-only Double must install with a Bat2 source filter");
    let battlefield_creatures: Vec<ObjectId> = runner
        .state()
        .battlefield
        .iter()
        .filter(|id| runner.state().objects[id].power.is_some())
        .copied()
        .collect();
    assert_eq!(
        battlefield_creatures,
        vec![bear1, bear2, bear3, bat1, bat2, squire],
        "reach guard: batch iteration order is battlefield order"
    );
    let bear2_filter = runner
        .state()
        .pending_damage_replacements
        .iter()
        .find(|r| {
            r.damage_source_filter
                .as_ref()
                .is_some_and(|f| matches!(f, TargetFilter::SpecificObject { id } if *id == bear2))
        })
        .expect("NWF-Bear2 installed shield must exist")
        .damage_source_filter
        .clone()
        .expect("a chosen-source shield binds its source");
    let mut rider_def = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::EachSourceDealsDamage {
            sources: TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Subtype("Bat".to_string()))
                    .controller(ControllerRef::You),
            ),
            amount: QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::BatchSource,
                },
            },
            recipient: EachDamageRecipient::EachController,
        },
    );
    rider_def.condition = Some(AbilityCondition::PostReplacementDamageSourceMatchesFilter {
        filter: bear2_filter,
    });
    let rider = build_resolved_from_def(&rider_def, way_bear, P0);
    runner
        .state_mut()
        .pending_damage_replacements
        .iter_mut()
        .find(|r| {
            r.damage_source_filter
                .as_ref()
                .is_some_and(|f| matches!(f, TargetFilter::SpecificObject { id } if *id == bear2))
        })
        .expect("NWF-Bear2 installed shield must exist")
        .runtime_execute = Some(Box::new(rider));
    let outer_def = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::EachSourceDealsDamage {
            sources: TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Subtype("Bear".to_string()))
                    .controller(ControllerRef::You),
            ),
            amount: QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::BatchSource,
                },
            },
            recipient: EachDamageRecipient::EachController,
        },
    );
    let card_id = CardId(runner.state().next_object_id);
    let source = create_object(
        runner.state_mut(),
        card_id,
        P0,
        "Typed Outer Batch".to_string(),
        Zone::Stack,
    );
    let outer = build_resolved_from_def(&outer_def, source, P0);
    let life_before = runner.state().players[P0.0 as usize].life;
    let hand_before = runner
        .state()
        .objects
        .values()
        .filter(|o| o.controller == P0 && o.zone == Zone::Hand)
        .count();
    let mut log: Vec<GameEvent> = Vec::new();
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &outer, &mut events, 0)
        .expect("the typed outer batch resolves to its pause");
    log.extend(events);
    let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the Bear2 event must pause for a CR 616.1 ordering choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        candidates.len(),
        2,
        "the Bear2 event must offer only its two shields (the Double is Bat2-only)"
    );
    let way_index = candidates
        .iter()
        .position(|c| c.source_name.is_empty())
        .expect("the other Bear2 candidate must be the floating NWF-Bear2 shield");
    let result = runner
        .act(GameAction::ChooseReplacement { index: way_index })
        .expect("the ordering choice must be answerable");
    log.extend(result.events);
    let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the nested Bat2 event must pause for its own ordering choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        candidates.len(),
        3,
        "the Bat2 event must offer the Double plus both shields"
    );
    assert!(
        candidates
            .iter()
            .any(|c| c.source_name.contains("Selfless Squire")),
        "one Bat2 candidate must be the Squire ETB shield"
    );
    let pending = runner
        .state()
        .pending_replacement
        .as_ref()
        .expect("the nested pause must park a pending replacement");
    let double_index = pending
        .candidates
        .iter()
        .position(|rid| rid.index == double_pos as u64)
        .expect("the Double must be offered on the Bat2 event");
    let child_owner = pending
        .each_source_batch_owner
        .expect("the first nested prompt must carry the child stamp (empty tail)");
    assert_eq!(
        runner
            .state()
            .pending_each_source_batches
            .iter()
            .map(|frame| frame.tally.iter().map(|entry| entry.amount).sum::<i32>())
            .collect::<Vec<_>>(),
        vec![3, 0, 1],
        "outer prefix holds Squire 1 plus NWF-Bear2 2, child scope is empty, \
         child prefix holds Squire 1 (Bat1); the Double prevents nothing yet"
    );
    let result = runner
        .act(GameAction::ChooseReplacement {
            index: double_index,
        })
        .expect("the Double choice must be answerable");
    log.extend(result.events);
    let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the doubled Bat2 event must re-park a second ordering choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        candidates.len(),
        2,
        "the second prompt must offer only the two shields (Double applied)"
    );
    let pending = runner
        .state()
        .pending_replacement
        .as_ref()
        .expect("the second pause must park a pending replacement");
    assert!(
        matches!(
            &pending.proposed,
            engine::types::proposed_event::ProposedEvent::Damage { amount: 4, .. }
        ),
        "the re-parked event must be the doubled Bat2 event, got {:?}",
        pending.proposed
    );
    assert_eq!(
        pending.each_source_batch_owner,
        Some(child_owner),
        "the second same-event prompt must retain the child stamp (R2-F2)"
    );
    let saved = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("the authoritative paused state serializes");
    let restored: PersistedGameState =
        serde_json::from_str(&saved).expect("the authoritative paused state deserializes");
    let mut runner = GameRunner::from_state(
        restored
            .into_game_state()
            .expect("persisted test snapshot satisfies the checked restore contract"),
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the second ordering prompt must survive restore, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner
            .state()
            .pending_replacement
            .as_ref()
            .and_then(|pending| pending.each_source_batch_owner),
        Some(child_owner),
        "the re-parked stamp must survive authoritative restore verbatim"
    );
    let squire_index = match runner.state().waiting_for.clone() {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates
            .iter()
            .position(|c| c.source_name.contains("Selfless Squire"))
            .expect("one candidate must be the Squire ETB shield"),
        other => panic!("expected the second ordering prompt, got {other:?}"),
    };
    let result = runner
        .act(GameAction::ChooseReplacement {
            index: squire_index,
        })
        .expect("the second ordering choice must be answerable after restore");
    log.extend(result.events);
    runner.advance_until_stack_empty();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must complete to a clean priority window, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "all damage must be prevented (none dealt through)"
    );
    assert_eq!(
        prevented_pairs(&log),
        vec![
            (TargetRef::Player(P0), 5),
            (TargetRef::Player(P0), 4),
            (TargetRef::Player(P0), 2),
        ],
        "child (P0,5) publishes at the child boundary before the outer tail \
         resumes; outer publishes (P0,4) plus (P0,2) unchanged"
    );
    assert_eq!(
        counters_on(&runner, squire),
        14,
        "three triggers of 5/4/2, each plus 1 from Hardened Scales"
    );
    assert_eq!(
        runner
            .state()
            .objects
            .values()
            .filter(|o| o.controller == P0 && o.zone == Zone::Hand)
            .count(),
        hand_before,
        "NWF-Bat2 rider must stay silent (its shield prevented nothing)"
    );
    assert!(
        runner.state().pending_each_source_batches.is_empty(),
        "every parked record must be consumed by the completion cascade"
    );
}

/// CR 615.13 + CR 608.2c (Matt F1): the empty-remainder branch of the
/// batch/downstream boundary. One Bear (1 to P0) with the same genuine
/// downstream 3: the pause parks NO damage remainder, so the downstream
/// must park ownerlessly while the prompt carries the batch stamp. The
/// answered member publishes (P0,1) at its own boundary, then downstream
/// prevents (P0,3). Hardened Scales discriminates: 2+4=6 counters fixed,
/// 5 (one 4-application) pre-fix.
#[test]
fn typed_single_batch_downstream_instruction_publishes_separate_application() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let squire = scenario
        .add_creature_to_hand_from_oracle(P0, "Selfless Squire", 1, 1, SELFLESS_SQUIRE_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let bear1 = scenario
        .add_creature(P0, "Bear1", 1, 1)
        .with_subtypes(vec!["Bear"])
        .id();
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES_TEXT);
    let way = add_instant_from_oracle(&mut scenario, P0, "New Way Forward", NEW_WAY_FORWARD_TEXT);
    let mut runner = scenario.build();

    runner.cast(squire).resolve();
    runner.cast(way).resolve();
    runner
        .act(GameAction::ChooseDamageSource { source: bear1 })
        .expect("naming Bear1 must be answerable");
    assert_eq!(
        runner.state().pending_damage_replacements.len(),
        2,
        "reach guard: the Squire ETB shield plus the NWF-Bear1 shield"
    );

    // Typed root batch: the single Bear deals power to its controller.
    let batch_def = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::EachSourceDealsDamage {
            sources: TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Subtype("Bear".to_string()))
                    .controller(ControllerRef::You),
            ),
            amount: QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::BatchSource,
                },
            },
            recipient: EachDamageRecipient::EachController,
        },
    );
    let card_id = CardId(runner.state().next_object_id);
    let source = create_object(
        runner.state_mut(),
        card_id,
        P0,
        "Typed Single Batch With Downstream".to_string(),
        Zone::Stack,
    );
    // Genuine downstream instruction, production-parsed: 3 to target player.
    let downstream_def = parse_oracle_text(
        "Downstream Burn deals 3 damage to target player.",
        "Downstream Burn",
        &[],
        &["Instant".to_string()],
        &[],
    )
    .abilities
    .into_iter()
    .next()
    .expect("the burn line must parse to one ability");
    let mut downstream = build_resolved_from_def(&downstream_def, source, P0);
    downstream.targets = vec![TargetRef::Player(P0)];
    let mut root = build_resolved_from_def(&batch_def, source, P0);
    root.sub_ability = Some(Box::new(downstream));

    let life_before = runner.state().players[P0.0 as usize].life;
    let hand_before = runner
        .state()
        .objects
        .values()
        .filter(|o| o.controller == P0 && o.zone == Zone::Hand)
        .count();
    let mut log: Vec<GameEvent> = Vec::new();
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &root, &mut events, 0)
        .expect("the typed batch resolves to its pause");
    log.extend(events);
    let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the Bear1 event must pause for a CR 616.1 ordering choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        candidates.len(),
        2,
        "the Bear1 event must offer both shields as ordering candidates"
    );
    assert_eq!(
        runner
            .state()
            .pending_each_source_batches
            .iter()
            .map(|frame| frame.tally.iter().map(|entry| entry.amount).sum::<i32>())
            .collect::<Vec<_>>(),
        vec![0],
        "the single-source pause parks one prefix with nothing yet prevented"
    );
    let batch_owner = runner.state().pending_each_source_batches[0].owner;
    assert_ne!(
        batch_owner, 0,
        "the parked prefix must carry a minted owner, not the legacy sentinel"
    );
    // F1 empty-remainder pin: no damage remainder exists, so the prompt
    // carries the batch stamp (the answered member is the batch boundary)
    // while the ONLY parked frame is the ownerless downstream chain.
    assert_eq!(
        runner
            .state()
            .pending_replacement
            .as_ref()
            .and_then(|pending| pending.each_source_batch_owner),
        Some(batch_owner),
        "the prompt must carry the batch stamp (empty damage remainder)"
    );
    let top = runner
        .state()
        .active_ability_continuation()
        .expect("the pause must park the downstream chain as the active frame");
    assert_eq!(
        top.each_source_batch_owner, None,
        "the downstream frame must be ownerless (never stamped with the batch)"
    );
    assert!(
        matches!(&top.chain.effect, Effect::DealDamage { .. }),
        "the downstream frame must head with the burn node, got {:?}",
        top.chain.effect
    );
    assert!(
        top.chain.sub_ability.is_none(),
        "the downstream frame must hold only the downstream chain"
    );

    // Save/load THROUGH the pause: the stamped prompt, the parked prefix,
    // and the ownerless downstream frame must survive the round trip.
    let saved = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("the authoritative paused state serializes");
    let restored: PersistedGameState =
        serde_json::from_str(&saved).expect("the authoritative paused state deserializes");
    let mut runner = GameRunner::from_state(
        restored
            .into_game_state()
            .expect("persisted test snapshot satisfies the checked restore contract"),
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the ordering prompt must survive restore, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner
            .state()
            .pending_replacement
            .as_ref()
            .and_then(|pending| pending.each_source_batch_owner),
        Some(batch_owner),
        "the prompt stamp must survive authoritative restore verbatim"
    );
    assert_eq!(
        runner.state().pending_each_source_batches[0].owner,
        batch_owner,
        "the frame owner must survive authoritative restore verbatim"
    );
    assert_eq!(
        runner
            .state()
            .active_ability_continuation()
            .expect("the downstream frame must survive restore")
            .each_source_batch_owner,
        None,
        "the downstream frame must stay ownerless across restore"
    );

    // Answer Squire: Bear1's 1 publishes (P0,1) at the answered member's
    // boundary, then downstream prevents (P0,3) separately.
    let squire_index = match runner.state().waiting_for.clone() {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates
            .iter()
            .position(|c| c.source_name.contains("Selfless Squire"))
            .expect("one candidate must be the Squire's ETB shield"),
        other => panic!("expected the ordering prompt, got {other:?}"),
    };
    let result = runner
        .act(GameAction::ChooseReplacement {
            index: squire_index,
        })
        .expect("the ordering choice must be answerable after restore");
    log.extend(result.events);
    runner.advance_until_stack_empty();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must complete to a clean priority window, got {:?}",
        runner.state().waiting_for
    );

    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "all damage must be prevented (none dealt through)"
    );
    assert_eq!(
        prevented_pairs(&log),
        vec![(TargetRef::Player(P0), 1), (TargetRef::Player(P0), 3)],
        "the batch publishes (P0,1) at its own boundary; the downstream \
         instruction prevents separately as (P0,3) — pre-fix they merge to (P0,4)"
    );
    assert_eq!(
        counters_on(&runner, squire),
        6,
        "two triggers of 1/3, each plus 1 from Hardened Scales"
    );
    assert_eq!(
        runner
            .state()
            .objects
            .values()
            .filter(|o| o.controller == P0 && o.zone == Zone::Hand)
            .count(),
        hand_before,
        "NWF-Bear1's rider must stay silent (its shield prevented nothing): \
         nothing drawn"
    );
    assert!(
        runner.state().pending_each_source_batches.is_empty(),
        "every parked record must be consumed by the completion cascade"
    );
}

/// CR 615.5 + CR 616.1e (Matt F2): a fresh rider-created choice on the
/// UNPAUSED aggregate path must stop the drainage — later groups park
/// behind it instead of overwriting it. P0/P1 each control a Bear and
/// Abundance; each resolves NWF naming their own Bear (P0's shield first);
/// Rakdos Charm mode 3 deals 1 to each controller. Each event has one
/// shield, so Phase B needs no ordering choice. P0's NWF rider draw parks
/// P0's Abundance choice; pre-fix the drain continues into P1's rider and
/// P1's pending draw overwrites P0's parked choice. Post-fix P0's choice
/// surfaces first with the P1 group parked as remainder, then P1's choice
/// surfaces after P0 declines — both remain answerable.
#[test]
fn aggregate_rider_choice_parks_later_groups_behind_it() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let bear_p0 = scenario.add_creature(P0, "Bear P0", 2, 2).id();
    let bear_p1 = scenario.add_creature(P1, "Bear P1", 2, 2).id();
    scenario.add_enchantment_from_oracle(P0, "Abundance", ABUNDANCE_TEXT);
    scenario.add_enchantment_from_oracle(P1, "Abundance", ABUNDANCE_TEXT);
    let way_p0 =
        add_instant_from_oracle(&mut scenario, P0, "New Way Forward", NEW_WAY_FORWARD_TEXT);
    let way_p1 =
        add_instant_from_oracle(&mut scenario, P1, "New Way Forward", NEW_WAY_FORWARD_TEXT);
    let charm = scenario
        .add_spell_to_hand_from_oracle(P0, "Rakdos Charm", true, RAKDOS_CHARM_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let mut runner = scenario.build();

    // P0's shield installs FIRST so its group drains (and pauses) first.
    runner.cast(way_p0).resolve();
    runner
        .act(GameAction::ChooseDamageSource { source: bear_p0 })
        .expect("naming Bear P0 must be answerable");
    pass_priority_to(&mut runner, P1);
    runner.cast(way_p1).resolve();
    runner
        .act(GameAction::ChooseDamageSource { source: bear_p1 })
        .expect("naming Bear P1 must be answerable");
    pass_priority_to(&mut runner, P0);
    assert_eq!(
        runner.state().pending_damage_replacements.len(),
        2,
        "reach guard: both NWF shields installed, P0's first"
    );

    let p0_life_before = runner.state().players[P0.0 as usize].life;
    let p1_life_before = runner.state().players[P1.0 as usize].life;
    let p0_hand_before = runner.state().players[P0.0 as usize].hand.len();
    let p1_hand_before = runner.state().players[P1.0 as usize].hand.len();
    let p0_library_before = runner.state().players[P0.0 as usize].library.len();
    let p1_library_before = runner.state().players[P1.0 as usize].library.len();
    let mut log: Vec<GameEvent> = Vec::new();
    let outcome = runner.cast(charm).modes(&[2]).resolve();
    log.extend(outcome.events().iter().cloned());

    // First prompt: P0's Abundance Accept/Decline, with the P1 group still
    // unpublished behind it.
    let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = outcome.final_waiting_for().clone()
    else {
        panic!(
            "P0's Abundance choice must surface first (no ordering pause), got {:?}",
            outcome.final_waiting_for()
        );
    };
    assert_eq!(
        player, P0,
        "the first rider choice must be P0's Abundance (P0's shield installed first)"
    );
    assert_eq!(
        candidates
            .iter()
            .map(|c| c.description.as_str())
            .collect::<Vec<_>>(),
        vec!["Accept", "Decline"],
        "the live child must be Abundance's optional replacement"
    );
    assert_eq!(
        runner.state().pending_each_source_batches.len(),
        1,
        "the unpublished P1 group must stay parked as a remainder"
    );
    assert_eq!(
        runner.state().pending_each_source_batches[0]
            .tally
            .iter()
            .map(|entry| entry.amount)
            .sum::<i32>(),
        1,
        "the remainder must hold the P1 group's 1 prevented"
    );

    // Decline P0's: the natural draw resolves, then P1's Abundance choice
    // must surface — pre-fix it overwrote (or was overwritten by) P0's.
    let result = runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("declining P0's Abundance must be answerable");
    log.extend(result.events);
    runner.advance_until_stack_empty();
    let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "P1's Abundance choice must surface after P0 declines, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P1, "the second rider choice must be P1's Abundance");
    assert_eq!(
        candidates
            .iter()
            .map(|c| c.description.as_str())
            .collect::<Vec<_>>(),
        vec!["Accept", "Decline"],
        "the second live child must be Abundance's optional replacement"
    );
    let result = runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("declining P1's Abundance must be answerable");
    log.extend(result.events);
    runner.advance_until_stack_empty();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must complete to a clean priority window, got {:?}",
        runner.state().waiting_for
    );

    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        p0_life_before - 1,
        "P0's NWF reflected 1 with no shield to stop it"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        p1_life_before - 1,
        "P1's NWF reflected 1 with no shield to stop it"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        p0_hand_before,
        "the Charm left the hand and P0's rider drew exactly 1"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].hand.len(),
        p1_hand_before + 1,
        "P1's rider draw must resolve (declined Abundance draws naturally)"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].library.len(),
        p0_library_before - 1,
        "exactly one card drawn from P0's library"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].library.len(),
        p1_library_before - 1,
        "exactly one card drawn from P1's library"
    );
    assert_eq!(
        prevented_pairs(&log),
        vec![(TargetRef::Player(P0), 1), (TargetRef::Player(P1), 1)],
        "P0's application first, P1's remainder after P0's child answers"
    );
    assert!(
        runner.state().pending_each_source_batches.is_empty(),
        "the remainder must be consumed by the completion cascade"
    );
}

/// CR 702.16e + CR 615.13 (Matt F4): player-protection prevention joins
/// the simultaneous batch's aggregate instead of emitting one event per
/// damage event. P0 controls Squire, two 1/1 Bears, and Scales; casts The
/// One Ring (protection from everything until next turn); then Rakdos Charm
/// mode 3. All three 1s exit through protection and publish ONE (P0,3)
/// application, so Squire triggers once for 3 plus 1 from Scales = 4
/// counters. Pre-fix the gate fragments into three (P0,1) events and Squire
/// triggers three times for 6 counters.
#[test]
fn protection_prevention_aggregates_at_batch_boundary() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let squire = scenario
        .add_creature_to_hand_from_oracle(P0, "Selfless Squire", 1, 1, SELFLESS_SQUIRE_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let bear1 = scenario
        .add_creature(P0, "Bear1", 1, 1)
        .with_subtypes(vec!["Bear"])
        .id();
    let _bear2 = scenario
        .add_creature(P0, "Bear2", 1, 1)
        .with_subtypes(vec!["Bear"])
        .id();
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES_TEXT);
    let ring = scenario
        .add_artifact_to_hand_from_oracle(P0, "The One Ring", ONE_RING_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let charm = scenario
        .add_spell_to_hand_from_oracle(P0, "Rakdos Charm", true, RAKDOS_CHARM_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let mut runner = scenario.build();

    runner.cast(squire).resolve();
    runner.cast(ring).resolve();
    runner.advance_until_stack_empty();
    assert!(
        engine::game::static_abilities::player_protection_from(runner.state(), P0, Some(bear1)),
        "reach guard: the cast Ring must grant P0 protection from everything"
    );

    let life_before = runner.state().players[P0.0 as usize].life;
    let mut log: Vec<GameEvent> = Vec::new();
    let outcome = runner.cast(charm).modes(&[2]).resolve();
    log.extend(outcome.events().iter().cloned());
    runner.advance_until_stack_empty();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must complete to a clean priority window, got {:?}",
        runner.state().waiting_for
    );

    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "all three 1s must be prevented (none dealt through)"
    );
    assert_eq!(
        prevented_pairs(&log),
        vec![(TargetRef::Player(P0), 3)],
        "protection prevention publishes ONE (P0,3) application at the \
         batch boundary — pre-fix it fragments into three (P0,1) events"
    );
    assert_eq!(
        counters_on(&runner, squire),
        4,
        "one trigger for 3 damage, plus 1 from Hardened Scales"
    );
    assert!(
        runner.state().pending_each_source_batches.is_empty(),
        "no batch records must leak from the unpaused path"
    );
}

/// CR 702.16e + CR 510.2 + CR 615.13 (Matt R7-F1): combat protection
/// prevention joins the simultaneous damage step's aggregate instead of
/// emitting one event per source. P1 attacks P0 with two 1/1s; P0 controls a
/// seeded Squire (no ETB shield — the seed never triggers), Scales, and a cast
/// Ring. Both 1s exit through protection and publish ONE (P0,2) application,
/// so Squire triggers once for 2 plus 1 from Scales = 3 counters. Pre-fix
/// Phase A emits per-source and Squire triggers twice for 4 counters.
#[test]
fn combat_protection_aggregates_simultaneous_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let squire = add_squire(&mut scenario, P0);
    let bear1 = scenario.add_creature(P1, "Bear1", 1, 1).id();
    let bear2 = scenario.add_creature(P1, "Bear2", 1, 1).id();
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES_TEXT);
    let ring = scenario
        .add_artifact_to_hand_from_oracle(P0, "The One Ring", ONE_RING_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let mut runner = scenario.build();

    runner.cast(ring).resolve();
    runner.advance_until_stack_empty();
    assert!(
        engine::game::static_abilities::player_protection_from(runner.state(), P0, Some(bear1)),
        "reach guard: the cast Ring must grant P0 protection from everything"
    );

    let crossed = pass_turn_to(&mut runner, P1);
    assert!(
        crossed,
        "stall guard: the turn must actually have passed to P1"
    );
    assert!(
        engine::game::static_abilities::player_protection_from(runner.state(), P0, Some(bear1)),
        "the Ring's until-your-next-turn window must survive onto P1's turn"
    );

    let life_before = runner.state().players[P0.0 as usize].life;
    let (ran, log) = run_combat_collecting(&mut runner, P1, &[bear1, bear2], P0, &[], None);
    assert!(ran, "stall guard: combat must actually have run");

    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "both 1s must be prevented (none dealt through)"
    );
    assert_eq!(
        prevented_pairs(&log),
        vec![(TargetRef::Player(P0), 2)],
        "protection prevention publishes ONE (P0,2) application for the \
         simultaneous combat damage step — pre-fix Phase A emits per-source"
    );
    assert_eq!(
        counters_on(&runner, squire),
        3,
        "one trigger for 2 damage, plus 1 from Hardened Scales"
    );
}

/// CR 702.16g + CR 615.13 (Matt R8-F1): two DISJOINT protection abilities
/// publish two prevention applications, not one. P0 controls a seeded Squire,
/// Scales, and two Emissary-pattern grantors — one choosing artifact, one
/// enchantment ("You ... have protection from the chosen card type" as a
/// `PlayerProtection(ChosenCardType)` static). P1 attacks with a 1/1 Artifact
/// Creature (Gingerbrute pattern) and a 1/1 Enchantment Creature (Nyxborn
/// Rollicker pattern). Each 1 is claimed by a different ability, so the
/// boundary publishes TWO (P0,1) applications and Squire triggers twice for 1
/// each: 2 + 2 = 4 counters with Scales. Pre-fix both preventions collapse
/// under the quality-less key into ONE (P0,2) for 3 counters.
#[test]
fn disjoint_protections_publish_separate_applications() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let squire = add_squire(&mut scenario, P0);
    let emissary_artifact = scenario.add_creature(P0, "Emissary Artifact", 2, 2).id();
    let emissary_enchantment = scenario.add_creature(P0, "Emissary Enchantment", 2, 2).id();
    let gingerbrute = scenario.add_creature(P1, "Gingerbrute", 1, 1).id();
    let rollicker = scenario.add_creature(P1, "Rollicker", 1, 1).id();
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES_TEXT);
    let mut runner = scenario.build();

    // Grantors + attacker types, following the established post-build pattern
    // (cf. alternative_cost_defiler_offers): stats, base copies, then a flush.
    {
        let state = runner.state_mut();
        for (grantor, choice) in [
            (emissary_artifact, CoreType::Artifact),
            (emissary_enchantment, CoreType::Enchantment),
        ] {
            let def = StaticDefinition::new(StaticMode::PlayerProtection(
                ProtectionTarget::ChosenCardType,
            ))
            .affected(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            ));
            let obj = state.objects.get_mut(&grantor).unwrap();
            obj.static_definitions.push(def.clone());
            std::sync::Arc::make_mut(&mut obj.base_static_definitions).push(def);
            obj.chosen_attributes
                .push(ChosenAttribute::CardType(choice));
        }
        for (attacker, core) in [
            (gingerbrute, CoreType::Artifact),
            (rollicker, CoreType::Enchantment),
        ] {
            let obj = state.objects.get_mut(&attacker).unwrap();
            obj.card_types.core_types.push(core);
            obj.base_card_types = obj.card_types.clone();
        }
    }
    engine::game::layers::flush_layers(runner.state_mut());

    assert!(
        engine::game::static_abilities::player_protection_from(
            runner.state(),
            P0,
            Some(gingerbrute)
        ),
        "reach guard: P0 must have protection from the artifact attacker"
    );
    assert!(
        engine::game::static_abilities::player_protection_from(runner.state(), P0, Some(rollicker)),
        "reach guard: P0 must have protection from the enchantment attacker"
    );
    assert_eq!(
        engine::game::static_abilities::preventing_player_protection_quality(
            runner.state(),
            P0,
            Some(gingerbrute)
        ),
        Some(MatchedProtectionQuality::CardType(CoreType::Artifact)),
        "reach guard: the artifact attacker must resolve to the artifact quality"
    );
    assert_eq!(
        engine::game::static_abilities::preventing_player_protection_quality(
            runner.state(),
            P0,
            Some(rollicker)
        ),
        Some(MatchedProtectionQuality::CardType(CoreType::Enchantment)),
        "reach guard: the enchantment attacker must resolve to the enchantment quality"
    );

    let crossed = pass_turn_to(&mut runner, P1);
    assert!(
        crossed,
        "stall guard: the turn must actually have passed to P1"
    );

    let life_before = runner.state().players[P0.0 as usize].life;
    let (ran, log) =
        run_combat_collecting(&mut runner, P1, &[gingerbrute, rollicker], P0, &[], None);
    assert!(ran, "stall guard: combat must actually have run");

    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "both 1s must be prevented (none dealt through)"
    );
    assert_eq!(
        prevented_pairs(&log),
        vec![(TargetRef::Player(P0), 1), (TargetRef::Player(P0), 1)],
        "two disjoint abilities publish TWO (P0,1) applications — pre-fix \
         they collapse into ONE (P0,2)"
    );
    assert_eq!(
        counters_on(&runner, squire),
        4,
        "two triggers for 1 each, plus 1 per trigger from Hardened Scales"
    );
    assert!(
        runner.state().pending_each_source_batches.is_empty(),
        "no batch records must leak from the unpaused path"
    );
}

/// Shared setup for the R10-F1 overlap controls: a seeded Squire + Scales,
/// two non-creature Emissary-pattern grantors (artifact / creature choices),
/// a 1/1 Artifact Creature ("Brute"), and a free Rakdos Charm in hand. The
/// batch iterates Squire first, Brute last; only the Brute's event overlaps
/// two protections, so only it can pause for the CR 616.1 choice.
fn overlap_setup() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let squire = add_squire(&mut scenario, P0);
    let brute = scenario.add_creature(P0, "Brute", 1, 1).id();
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES_TEXT);
    let charm = scenario
        .add_spell_to_hand_from_oracle(P0, "Rakdos Charm", true, RAKDOS_CHARM_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let mut runner = scenario.build();

    // Non-creature grantors (excluded from the charm's creature iteration),
    // following the established post-build pattern: stats, base copies, flush.
    {
        let grantor_a = {
            let card_id = CardId(runner.state().next_object_id);
            engine::game::zones::create_object(
                runner.state_mut(),
                card_id,
                P0,
                "Emissary Artifact".to_string(),
                Zone::Battlefield,
            )
        };
        let grantor_b = {
            let card_id = CardId(runner.state().next_object_id);
            engine::game::zones::create_object(
                runner.state_mut(),
                card_id,
                P0,
                "Emissary Creature".to_string(),
                Zone::Battlefield,
            )
        };
        let state = runner.state_mut();
        for (grantor, choice) in [
            (grantor_a, CoreType::Artifact),
            (grantor_b, CoreType::Creature),
        ] {
            let def = StaticDefinition::new(StaticMode::PlayerProtection(
                ProtectionTarget::ChosenCardType,
            ))
            .affected(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            ));
            let obj = state.objects.get_mut(&grantor).unwrap();
            obj.static_definitions.push(def.clone());
            std::sync::Arc::make_mut(&mut obj.base_static_definitions).push(def);
            obj.chosen_attributes
                .push(ChosenAttribute::CardType(choice));
        }
        let obj = state.objects.get_mut(&brute).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.base_card_types = obj.card_types.clone();
    }
    engine::game::layers::flush_layers(runner.state_mut());

    assert!(
        engine::game::static_abilities::player_protection_from(runner.state(), P0, Some(brute)),
        "reach guard: P0 must have protection from the artifact source"
    );
    assert!(
        engine::game::static_abilities::player_protection_from(runner.state(), P0, Some(squire)),
        "reach guard: P0 must have protection from the plain-creature source"
    );
    (runner, squire, brute, charm)
}

/// CR 616.1 + CR 615.13 (Matt R10-F1): overlapping protections prompt the
/// affected player, and the chosen application survives the batch
/// continuation. P0 has artifact + creature Emissary-pattern protection;
/// Rakdos Charm mode 2 makes the Squire (creature only) and the Brute
/// (artifact AND creature) each deal 1 to P0. The Brute's event must pause
/// for a 616.1 choice offering both qualities; answering creature-protection
/// merges both preventions into ONE (P0,2) application: one Squire trigger
/// for 2, +1 from Scales = 3 counters. Pre-fix no prompt exists (first-match
/// claims artifact) and the pause assertion fails.
#[test]
fn overlapping_protections_prompt_and_merge_on_creature_answer() {
    let (mut runner, squire, brute, charm) = overlap_setup();
    let battlefield_creatures: Vec<ObjectId> = runner
        .state()
        .battlefield
        .iter()
        .filter(|id| runner.state().objects[id].power.is_some())
        .copied()
        .collect();
    assert_eq!(
        battlefield_creatures,
        vec![squire, brute],
        "reach guard: the batch must iterate Squire first, Brute last"
    );

    let life_before = runner.state().players[P0.0 as usize].life;
    let mut log: Vec<GameEvent> = Vec::new();
    let outcome = runner.cast(charm).modes(&[2]).resolve();
    log.extend(outcome.events().iter().cloned());
    let WaitingFor::ReplacementChoice { candidates, .. } = outcome.final_waiting_for() else {
        panic!(
            "the Brute event must pause for a CR 616.1 protection choice, got {:?}",
            outcome.final_waiting_for()
        );
    };
    assert_eq!(
        candidates.len(),
        2,
        "the Brute event must offer both protections, got {:?}",
        candidates
            .iter()
            .map(|c| &c.description)
            .collect::<Vec<_>>()
    );
    let creature_index = candidates
        .iter()
        .position(|c| c.description.contains("from creature"))
        .expect("one candidate must be creature-protection");
    assert!(
        candidates
            .iter()
            .any(|c| c.description.contains("from artifact")),
        "the other candidate must be artifact-protection, got {:?}",
        candidates
            .iter()
            .map(|c| &c.description)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        runner.state().pending_each_source_batches.len(),
        1,
        "the pause must park exactly one batch record"
    );
    assert_eq!(
        runner.state().pending_each_source_batches[0]
            .tally
            .iter()
            .map(|entry| entry.amount)
            .sum::<i32>(),
        1,
        "the parked prefix must hold the Squire's 1 prevented"
    );

    let result = runner
        .act(GameAction::ChooseReplacement {
            index: creature_index,
        })
        .expect("the protection choice must be answerable");
    log.extend(result.events);
    runner.advance_until_stack_empty();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must complete to a clean priority window, got {:?}",
        runner.state().waiting_for
    );

    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "both 1s must be prevented (none dealt through)"
    );
    assert_eq!(
        prevented_pairs(&log),
        vec![(TargetRef::Player(P0), 2)],
        "the chosen creature-protection must publish ONE (P0,2) application"
    );
    assert_eq!(
        counters_on(&runner, squire),
        3,
        "one trigger for 2 damage, plus 1 from Hardened Scales"
    );
    assert!(
        runner.state().pending_each_source_batches.is_empty(),
        "the pause record must be consumed by the completion cascade"
    );
}

/// CR 616.1 + CR 615.13 (Matt R10-F1, second leg): same overlap, answering
/// artifact-protection instead. The Brute's 1 lands under the artifact key
/// while the Squire's 1 stays under creature: TWO (P0,1) applications, two
/// Squire triggers, (1+1)*2 = 4 counters. Together with the merge leg this
/// proves the answer is honored rather than the choice being decorative.
#[test]
fn overlapping_protections_split_on_artifact_answer() {
    let (mut runner, squire, _brute, charm) = overlap_setup();

    let life_before = runner.state().players[P0.0 as usize].life;
    let mut log: Vec<GameEvent> = Vec::new();
    let outcome = runner.cast(charm).modes(&[2]).resolve();
    log.extend(outcome.events().iter().cloned());
    let WaitingFor::ReplacementChoice { candidates, .. } = outcome.final_waiting_for() else {
        panic!(
            "the Brute event must pause for a CR 616.1 protection choice, got {:?}",
            outcome.final_waiting_for()
        );
    };
    let artifact_index = candidates
        .iter()
        .position(|c| c.description.contains("from artifact"))
        .expect("one candidate must be artifact-protection");

    let result = runner
        .act(GameAction::ChooseReplacement {
            index: artifact_index,
        })
        .expect("the protection choice must be answerable");
    log.extend(result.events);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "both 1s must be prevented (none dealt through)"
    );
    assert_eq!(
        prevented_pairs(&log),
        vec![(TargetRef::Player(P0), 1), (TargetRef::Player(P0), 1)],
        "the chosen artifact-protection must split off its own (P0,1)"
    );
    assert_eq!(
        counters_on(&runner, squire),
        4,
        "two triggers for 1 each, plus 1 per trigger from Hardened Scales"
    );
    assert!(
        runner.state().pending_each_source_batches.is_empty(),
        "the pause record must be consumed by the completion cascade"
    );
}

/// CR 702.7 + CR 510.4 (Matt R7-F1 pin): the aggregate fix must not merge the
/// two combat damage steps. A first-striker and a regular 1/1 attack into the
/// same protection; each step is its own simultaneity, so prevention publishes
/// TWO (P0,1) applications and Squire triggers twice for 2+2 = 4 counters,
/// before AND after the fix.
#[test]
fn combat_protection_keeps_first_strike_steps_separate() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let squire = add_squire(&mut scenario, P0);
    let striker = {
        let mut b = scenario.add_creature(P1, "Striker", 1, 1);
        b.first_strike();
        b.id()
    };
    let bear = scenario.add_creature(P1, "Bear", 1, 1).id();
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES_TEXT);
    let ring = scenario
        .add_artifact_to_hand_from_oracle(P0, "The One Ring", ONE_RING_TEXT)
        .with_mana_cost(free_cost())
        .id();
    let mut runner = scenario.build();

    runner.cast(ring).resolve();
    runner.advance_until_stack_empty();

    let crossed = pass_turn_to(&mut runner, P1);
    assert!(
        crossed,
        "stall guard: the turn must actually have passed to P1"
    );
    assert!(
        engine::game::static_abilities::player_protection_from(runner.state(), P0, Some(bear)),
        "the Ring's until-your-next-turn window must survive onto P1's turn"
    );

    let life_before = runner.state().players[P0.0 as usize].life;
    let (ran, log) = run_combat_collecting(&mut runner, P1, &[striker, bear], P0, &[], None);
    assert!(ran, "stall guard: combat must actually have run");

    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "both 1s must be prevented (none dealt through)"
    );
    assert_eq!(
        prevented_pairs(&log),
        vec![(TargetRef::Player(P0), 1), (TargetRef::Player(P0), 1)],
        "first-strike and regular are separate damage steps: two (P0,1) \
         applications, never one merged (P0,2)"
    );
    assert_eq!(
        counters_on(&runner, squire),
        4,
        "two triggers for 1 damage each, plus 1 per trigger from Hardened Scales"
    );
}

/// CR 120.3e (Matt R7-F1 pin): sequential events stay sequential under
/// protection. Two separate 1-damage pings to P0, each fully resolved before
/// the next, publish TWO (P0,1) applications and Squire triggers twice for
/// 2+2 = 4 counters, before AND after the fix. A tally that leaked across
/// sequential resolutions would merge these into one (P0,2).
#[test]
fn sequential_noncombat_pings_stay_separate_under_protection() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);

    let squire = add_squire(&mut scenario, P0);
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES_TEXT);
    let ring = scenario
        .add_artifact_to_hand_from_oracle(P0, "The One Ring", ONE_RING_TEXT)
        .with_mana_cost(free_cost())
        .id();
    // Two separately castable pings: each cast is its own damage event, fully
    // resolved (triggers included) before the next is cast. The pings MUST be
    // untargeted ("each player"): protection from everything also stops P0's
    // OWN targeted pings (CR 702.16b has no controller exception), which
    // would fizzle with no damage event at all.
    let ping_one = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Ping One",
            true,
            "Ping One deals 1 damage to each player.",
        )
        .with_mana_cost(free_cost())
        .id();
    let ping_two = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Ping Two",
            true,
            "Ping Two deals 1 damage to each player.",
        )
        .with_mana_cost(free_cost())
        .id();
    let mut runner = scenario.build();

    runner.cast(ring).resolve();
    runner.advance_until_stack_empty();
    assert!(
        engine::game::static_abilities::player_protection_from(runner.state(), P0, None),
        "reach guard: the cast Ring must grant P0 protection from everything"
    );

    let p0_before = runner.state().players[P0.0 as usize].life;
    let p1_before = runner.state().players[P1.0 as usize].life;
    let mut log: Vec<GameEvent> = Vec::new();
    for ping in [ping_one, ping_two] {
        let outcome = runner.cast(ping).resolve();
        log.extend(outcome.events().iter().cloned());
        runner.advance_until_stack_empty();
    }

    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        p0_before,
        "both pings must be prevented on P0 (none dealt through)"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        p1_before - 2,
        "reach guard: P1 has no protection, so both pings must have dealt — \
         proving the casts resolved rather than fizzling"
    );
    assert_eq!(
        prevented_pairs(&log),
        vec![(TargetRef::Player(P0), 1), (TargetRef::Player(P0), 1)],
        "sequential pings are separate events: two (P0,1) applications, \
         never one merged (P0,2)"
    );
    assert_eq!(
        counters_on(&runner, squire),
        4,
        "two triggers for 1 damage each, plus 1 per trigger from Hardened Scales"
    );
}
