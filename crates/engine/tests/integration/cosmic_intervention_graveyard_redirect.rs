//! Issue #8781 — "If \<subject\> would be put into a graveyard \[from \<zone\>\]
//! \[this turn\], exile it instead" (CR 614.1a).
//!
//! Two defects in one clause family:
//!
//! 1. **The antecedent's subject was dropped.** `parse_graveyard_exile_replacement`
//!    read only the graveyard's owner and the card/token axis (CR 108.2b +
//!    CR 111.1), so Dryad Militant ("an instant or sorcery card") and Samurai of
//!    the Pale Curtain ("a permanent") both installed an unfiltered, board-wide
//!    Rest in Peace.
//! 2. **A stated window made the whole line unparseable, or inert.** The
//!    grammar accepted only "from anywhere", so "from the battlefield" failed
//!    outright (Ugin's Nexus), and a windowed clause that did parse was hosted
//!    on the card as a printed static — which never applies from a spell,
//!    because `find_applicable_replacements` scans the battlefield and command
//!    zone only.
//!
//! Cosmic Intervention hits both at once, and it is the reported symptom:
//! `cosmic_intervention_exiles_your_dying_permanent_instead_of_graveyard`
//! reproduces the bug report (a permanent still reached the graveyard) and
//! `commander_exiled_this_way_is_offered_the_command_zone_from_exile`
//! reproduces its visible tell (the CR 903.9a prompt naming the graveyard).
//!
//! CR 611.2a is the discriminator the fix turns on: a printed static states no
//! window, so a definition that states one was CREATED by a resolving spell or
//! ability and must be installed into the floating store instead.

use engine::game::scenario::{CastOutcome, GameRunner, GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{Effect, FilterProp, TargetFilter, TypeFilter};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Cosmic Intervention {3}{W}, Instant (Kaldheim Commander), verbatim first
/// printed line (verified against the Scryfall API). Its second line is the
/// keyword ability `Foretell {1}{W}`, which MTGJSON supplies as a keyword rather
/// than as body text and which is irrelevant to this clause.
const COSMIC_INTERVENTION: &str = "If a permanent you control would be put into a graveyard from the battlefield this turn, exile it instead. Return it to the battlefield under its owner's control at the beginning of the next end step.";

/// The full printed Oracle text, reminder text included, for the parse-shape
/// test — proof that the real card, not a trimmed stand-in, reaches the fix.
const COSMIC_INTERVENTION_FULL: &str = "If a permanent you control would be put into a graveyard from the battlefield this turn, exile it instead. Return it to the battlefield under its owner's control at the beginning of the next end step.\nForetell {1}{W} (During your turn, you may pay {2} and exile this card from your hand face down. Cast it on a later turn for its foretell cost.)";

const DESTROY_TARGET_CREATURE: &str = "Destroy target creature.";
const DESTROY_TARGET_ARTIFACT: &str = "Destroy target artifact.";

/// Ugin's Nexus {5}, Legendary Artifact, BOTH printed lines verbatim (verified
/// against the card export). Line 2 is this file's clause; line 1 rides along so
/// the runtime test exercises the real card rather than a trimmed stand-in.
const UGINS_NEXUS_FULL: &str = "If a player would begin an extra turn, that player skips that turn instead.\nIf Ugin's Nexus would be put into a graveyard from the battlefield, instead exile it and take an extra turn after this one.";

/// Enough white mana for {3}{W} several times over; pool-funded casts auto-pay.
fn fund(scenario: &mut GameScenario, player: PlayerId) {
    scenario.with_mana_pool(
        player,
        (0..8)
            .map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]))
            .collect(),
    );
}

/// P0's turn, precombat main, both players funded, with Cosmic Intervention and
/// a removal spell in P0's hand and one 2/2 on each side.
struct Board {
    runner: GameRunner,
    intervention: ObjectId,
    removal: ObjectId,
    /// A second removal spell, so one test can kill BOTH creatures under the
    /// same installed shield and read the two outcomes against each other.
    removal_b: ObjectId,
    mine: ObjectId,
    theirs: ObjectId,
}

fn board() -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    fund(&mut scenario, P0);
    fund(&mut scenario, P1);

    let mine = scenario.add_creature(P0, "Home Bear", 2, 2).id();
    let theirs = scenario.add_creature(P1, "Away Bear", 2, 2).id();
    let intervention = scenario
        .add_spell_to_hand_from_oracle(P0, "Cosmic Intervention", true, COSMIC_INTERVENTION)
        .id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_TARGET_CREATURE)
        .id();
    let removal_b = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder B", true, DESTROY_TARGET_CREATURE)
        .id();

    Board {
        runner: scenario.build(),
        intervention,
        removal,
        removal_b,
        mine,
        theirs,
    }
}

/// REPORTED SYMPTOM. Before the fix the whole first sentence lowered to a
/// one-shot `ChangeZone { target: ParentTarget }` on a spell that targets
/// nothing, so nothing was exiled and the permanent reached the graveyard
/// exactly as the bug report describes. CR 614.1a + CR 614.6: the move to the
/// graveyard is replaced, so it never happens.
#[test]
fn cosmic_intervention_exiles_your_dying_permanent_instead_of_graveyard() {
    let mut b = board();
    b.runner.cast(b.intervention).resolve();

    let outcome = b.runner.cast(b.removal).target_object(b.mine).resolve();

    outcome.assert_zone(&[b.mine], Zone::Exile);
}

/// CR 603.7 + CR 614.6: the consequent sentence rides on the redirect and runs
/// once per redirected object, creating a delayed trigger that returns that
/// object at the beginning of the next end step.
#[test]
fn cosmic_intervention_returns_the_exiled_permanent_at_the_next_end_step() {
    let mut b = board();
    b.runner.cast(b.intervention).resolve();
    let outcome = b.runner.cast(b.removal).target_object(b.mine).resolve();
    // Positive reach-guard: the redirect fired, so the return below is a real
    // observation about the delayed trigger rather than a creature that never
    // left the battlefield.
    outcome.assert_zone(&[b.mine], Zone::Exile);

    b.runner.advance_to_end_step();
    b.runner.advance_until_stack_empty();

    assert_eq!(
        b.runner.state().objects[&b.mine].zone,
        Zone::Battlefield,
        "CR 603.7: the delayed trigger returns the exiled permanent at the \
         beginning of the next end step"
    );
}

/// CR 614.1: a replacement effect "watch[es] for a particular event", and the
/// antecedent describes it — so "a permanent YOU CONTROL" is a filter on the
/// affected object, not decoration. The
/// pre-fix parse dropped the subject entirely; had the windowed clause been
/// hosted at all, it would have shielded every permanent on the board.
#[test]
fn cosmic_intervention_does_not_shield_an_opponents_permanent() {
    let mut b = board();
    b.runner.cast(b.intervention).resolve();

    // THE NEGATIVE. On its own this is vacuous: a creature reaches the graveyard
    // just as readily when no shield installed at all, which is precisely the
    // pre-fix behavior this file exists to catch.
    let theirs = b.runner.cast(b.removal).target_object(b.theirs).resolve();
    assert_eq!(
        theirs.zone_of(b.theirs),
        Zone::Graveyard,
        "CR 614.1: the shield names permanents its caster controls, so an \
         opponent's creature still dies normally"
    );

    // PAIRED POSITIVE, same board and same installed shield, asserted AFTER the
    // negative so it proves the shield was live at a moment no earlier than the
    // observation above. Kill P0's own 2/2 and it must be exiled instead. If the
    // definition never installed, THIS assertion fails and the vacuous reading
    // of the negative is impossible.
    let mine = b.runner.cast(b.removal_b).target_object(b.mine).resolve();
    assert_eq!(
        mine.zone_of(b.mine),
        Zone::Exile,
        "reach-guard: the same shield redirects the controller's own permanent, \
         so the graveyard result above is a filter decision and not a missing \
         replacement"
    );
}

/// CR 514.2 + CR 611.2a: "this turn" ends at the cleanup step. A window captured
/// but not enforced would turn a one-turn shield into a permanent one.
#[test]
fn cosmic_intervention_shield_ends_at_cleanup() {
    let mut scenario = GameScenario::new();
    // P0's END STEP, so the walk to the next turn's main phase crosses cleanup
    // without passing through a combat step, where `advance_to_phase` would
    // stop on the declare-attackers turn-based action.
    scenario.at_phase(Phase::End);
    fund(&mut scenario, P0);
    fund(&mut scenario, P1);

    let mine = scenario.add_creature(P0, "Home Bear", 2, 2).id();
    let intervention = scenario
        .add_spell_to_hand_from_oracle(P0, "Cosmic Intervention", true, COSMIC_INTERVENTION)
        .id();
    // The follow-up removal is cast by P1 on P1's own turn, which is the only
    // way this harness can reach a later turn's priority window.
    let removal = scenario
        .add_spell_to_hand_from_oracle(P1, "Murder", true, DESTROY_TARGET_CREATURE)
        .id();
    // CR 704.5b: both libraries need cards, or the draw step across the turn
    // boundary ends the game before the shield can be observed to have expired.
    scenario.with_library_top(P0, &["Filler A", "Filler B", "Filler C"]);
    scenario.with_library_top(P1, &["Filler D", "Filler E", "Filler F"]);
    let mut runner = scenario.build();

    runner.cast(intervention).resolve();
    // Positive reach-guard: the shield really is installed on the turn it was
    // cast, so the graveyard result below is expiry and not a failed install.
    assert_eq!(
        runner
            .state()
            .pending_damage_replacements
            .iter()
            .filter(|def| def.destination_zone == Some(Zone::Graveyard))
            .count(),
        1,
        "CR 611.2a: the resolving spell installs exactly one floating shield"
    );

    runner.advance_to_phase(Phase::PreCombatMain);
    assert_ne!(
        runner.state().active_player,
        P0,
        "reach-guard: the test must actually have crossed into the next turn"
    );
    assert!(
        runner.state().pending_damage_replacements.is_empty(),
        "CR 514.2: cleanup prunes the floating shield when the turn it was \
         created on ends"
    );

    let outcome = runner.cast(removal).target_object(mine).resolve();

    assert_eq!(
        outcome.zone_of(mine),
        Zone::Graveyard,
        "CR 514.2: the shield lasted only 'this turn', so a permanent dying on \
         a later turn reaches the graveyard"
    );
}

/// CR 903.9a: a commander in a graveyard OR IN EXILE may be put into the command
/// zone. The rule is a state-based action, not a replacement, so it does not
/// compete under CR 616.1 — it applies to whichever zone the card actually
/// reached. The bug report's visible tell was this prompt naming the graveyard.
#[test]
fn commander_exiled_this_way_is_offered_the_command_zone_from_exile() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    fund(&mut scenario, P0);

    // CR 903.3: the commander designation is an attribute of the card, so the
    // permanent stays on the battlefield — `with_commander` would move it to the
    // command zone, where nothing can target or destroy it.
    let commander = {
        let mut b = scenario.add_creature(P0, "Commanding Bear", 2, 2);
        b.as_legendary().commander();
        b.id()
    };
    let intervention = scenario
        .add_spell_to_hand_from_oracle(P0, "Cosmic Intervention", true, COSMIC_INTERVENTION)
        .id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_TARGET_CREATURE)
        .id();
    let mut runner = scenario.build();
    // CR 903.9a is a Commander-format state-based action; the default scenario
    // format has no command zone and skips it entirely (`game/sba.rs`).
    runner.state_mut().format_config.command_zone = true;

    runner.cast(intervention).resolve();
    let outcome = runner.cast(removal).target_object(commander).resolve();

    match outcome.final_waiting_for() {
        WaitingFor::CommanderZoneChoice {
            commander_id,
            current_zone,
            ..
        } => {
            assert_eq!(*commander_id, commander);
            assert_eq!(
                *current_zone,
                Zone::Exile,
                "CR 903.9a covers a commander in a graveyard or in exile; the \
                 replacement sent it to exile, so the prompt must name exile"
            );
        }
        other => panic!("expected the CR 903.9a commander-zone choice, got {other:?}"),
    }

    // CR 903.9a is optional: declining leaves the commander in exile, where the
    // Cosmic Intervention return is still waiting for it.
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining the command-zone return is legal");
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&commander].zone,
        Zone::Battlefield,
        "declining the CR 903.9a return keeps the CR 603.7 delayed return alive"
    );
}

// ---------------------------------------------------------------------------
// SHAPE tests for the clause family — the building block, not one card.
// ---------------------------------------------------------------------------

fn parse(name: &str, text: &str, types: &[&str]) -> engine::parser::oracle::ParsedAbilities {
    let types: Vec<String> = types.iter().map(|t| t.to_string()).collect();
    parse_oracle_text(text, name, &[], &types, &[])
}

/// The whole printed card, reminder text and keyword line included, still
/// reaches the resolution-install route.
#[test]
fn cosmic_intervention_parses_as_a_windowed_resolution_install() {
    let parsed = parse(
        "Cosmic Intervention",
        COSMIC_INTERVENTION_FULL,
        &["Instant"],
    );
    assert!(
        parsed.replacements.is_empty(),
        "CR 611.2a: a windowed replacement must NOT be hosted on the card, \
         where it could never apply from a spell"
    );

    let install = parsed
        .abilities
        .iter()
        .find_map(|ability| match &*ability.effect {
            Effect::AddTargetReplacement {
                replacement,
                target: TargetFilter::None,
            } => Some(replacement),
            _ => None,
        })
        .expect("the clause lowers to a floating AddTargetReplacement install");

    assert_eq!(install.destination_zone, Some(Zone::Graveyard));
    assert!(
        install.expiry.is_some(),
        "CR 514.2: 'this turn' must reach `expiry`, the single lifetime authority"
    );
    let TargetFilter::Typed(filter) = install
        .valid_card
        .as_ref()
        .expect("the subject is a filter on the affected object")
    else {
        panic!(
            "expected a typed subject filter, got {:?}",
            install.valid_card
        );
    };
    assert_eq!(filter.type_filters, vec![TypeFilter::Permanent]);
    assert!(
        filter.properties.contains(&FilterProp::InZone {
            zone: Zone::Battlefield
        }),
        "CR 700.4: 'from the battlefield' is the dying test, not any move to a \
         graveyard"
    );
}

/// CR 614.1: the subject narrows the redirect for the whole family, not just
/// for the card in the bug report. Dryad Militant redirects instants and
/// sorceries; before the fix it redirected every nontoken card in the game.
#[test]
fn subject_type_phrase_scopes_a_board_wide_graveyard_redirect() {
    let parsed = parse(
        "Dryad Militant",
        "If an instant or sorcery card would be put into a graveyard from anywhere, exile it instead.",
        &["Creature"],
    );
    let def = parsed
        .replacements
        .first()
        .expect("a windowless printed static stays hosted on the card");
    let filter = def
        .valid_card
        .as_ref()
        .expect("the instant-or-sorcery subject must reach `valid_card`");
    assert!(
        filter_mentions(filter, &TypeFilter::Instant)
            && filter_mentions(filter, &TypeFilter::Sorcery),
        "expected an instant/sorcery subject filter, got {filter:?}"
    );
}

/// CR 110.1: a permanent is a card or token ON THE BATTLEFIELD. The engine's
/// `TypeFilter::Permanent` is a card-type test, so the zone half has to come
/// from the noun, or Samurai of the Pale Curtain would also claim a milled card.
#[test]
fn permanent_subject_is_battlefield_scoped() {
    let parsed = parse(
        "Samurai of the Pale Curtain",
        "If a permanent would be put into a graveyard, exile it instead.",
        &["Creature"],
    );
    let def = parsed.replacements.first().expect("printed static");
    let TargetFilter::Typed(filter) = def.valid_card.as_ref().expect("subject filter") else {
        panic!("expected a typed subject filter, got {:?}", def.valid_card);
    };
    assert_eq!(filter.type_filters, vec![TypeFilter::Permanent]);
    assert!(filter.properties.contains(&FilterProp::InZone {
        zone: Zone::Battlefield
    }));
}

/// CR 108.2b + CR 111.1: the bare card/token nouns ARE the token axis and
/// constrain nothing else. Rest in Peace must keep its unfiltered board-wide
/// redirect — the subject reader has to decline them, not re-express them.
#[test]
fn bare_card_or_token_subject_stays_unfiltered() {
    let parsed = parse(
        "Rest in Peace",
        "If a card or token would be put into a graveyard from anywhere, exile it instead.",
        &["Enchantment"],
    );
    let def = parsed.replacements.first().expect("printed static");
    assert_eq!(
        def.valid_card, None,
        "Rest in Peace redirects every object that can reach a graveyard"
    );
}

/// CR 400.1 + CR 608.2c: "from the battlefield" plus a trailing consequent — the
/// two halves that made this line unparseable before the fix (it fell through to
/// `Effect::Unimplemented`). Ugin's Nexus is the windowless member of the class,
/// so it stays a card-hosted static.
#[test]
fn stated_origin_and_consequent_parse_on_a_printed_static() {
    let parsed = parse(
        "Ugin's Nexus",
        "If Ugin's Nexus would be put into a graveyard from the battlefield, instead exile it and take an extra turn after this one.",
        &["Artifact"],
    );
    assert!(
        !parsed
            .abilities
            .iter()
            .any(|a| matches!(&*a.effect, Effect::Unimplemented { .. })),
        "reach-guard: the line must actually parse, not fail closed"
    );
    let def = parsed.replacements.first().expect("printed static");
    assert_eq!(def.destination_zone, Some(Zone::Graveyard));
    // The consequent's IDENTITY, not merely its presence: `sub_ability.is_some()`
    // is satisfied by any effect at all, so an unrelated or inert consequent
    // would keep this green while the extra turn silently went missing.
    let execute = def.execute.as_ref().expect("the redirect's execute chain");
    let consequent = execute
        .sub_ability
        .as_deref()
        .expect("CR 608.2c: the consequent must ride on the redirect");
    assert!(
        matches!(
            &*consequent.effect,
            Effect::ExtraTurn {
                target: TargetFilter::Controller,
                ..
            }
        ),
        "CR 608.2c: 'and take an extra turn after this one' must lower to an \
         ExtraTurn for the redirect's controller, got {:?}",
        consequent.effect
    );
}

/// CR 608.2c + CR 500.7: the consequent RUNS. The shape assertion above proves
/// the effect is wired to the redirect; this proves the wiring fires, queueing
/// the extra turn for the Nexus's controller when the redirect claims it.
///
/// Built from the card's FULL printed text, both lines. Line 1 ("If a player
/// would begin an extra turn, that player skips that turn instead") is a
/// card-hosted static that leaves the battlefield with the Nexus, so it does not
/// reach the queued turn — and whether that skip later applies is a separate
/// CR 614.6 question this test deliberately does not assert.
#[test]
fn ugins_nexus_redirect_queues_the_extra_turn_for_its_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    fund(&mut scenario, P0);

    let nexus = scenario
        .add_artifact_from_oracle(P0, "Ugin's Nexus", UGINS_NEXUS_FULL)
        .id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Shatter", true, DESTROY_TARGET_ARTIFACT)
        .id();
    let mut runner = scenario.build();

    let queued_before = runner.state().extra_turns.len();
    let outcome = runner.cast(removal).target_object(nexus).resolve();

    // Positive reach-guard: the redirect actually claimed the Nexus. Without
    // this, an unqueued extra turn could be blamed on the artifact never dying.
    outcome.assert_zone(&[nexus], Zone::Exile);

    let queued = &outcome.state().extra_turns[queued_before..];
    assert_eq!(
        queued.len(),
        1,
        "CR 500.7: the consequent queues exactly one extra turn"
    );
    assert_eq!(
        queued[0].player, P0,
        "CR 608.2c: the extra turn goes to the redirect's controller"
    );
}

/// CR 108.2b + CR 111.1: "tokens aren't considered cards", so a token-only
/// antecedent must claim tokens and leave cards alone.
///
/// A RUNTIME card-versus-token regression, not a parse-shape test: the token
/// axis is only meaningful once an object actually moves, and
/// `game/filter.rs` resolves `FilterProp::Token` off `record.is_token` on the
/// zone-change snapshot. Before the fix the subject was discarded entirely
/// (`valid_card: None`) and the nontoken creature was exiled too, so the second
/// assertion is the one that bites.
///
/// No printed card carries a token-only antecedent today — the five real cards
/// whose subject mentions tokens all read "a card or token" (Rest in Peace,
/// Necrodominance, Festival of Embers, Hades) or "a nontoken creature"
/// (Anafenza). This guards a shape the GRAMMAR accepts, which is where the
/// unfiltered redirect would come from.
#[test]
fn token_only_antecedent_claims_tokens_and_spares_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    fund(&mut scenario, P0);

    // Name shares no word with the clause, so self-reference normalization
    // cannot rewrite the subject to `~` and silently change what is tested.
    scenario.add_enchantment_from_oracle(
        P0,
        "Chronal Sieve",
        "If a token would be put into a graveyard from anywhere, exile it instead.",
    );

    let token = scenario.add_creature(P0, "Spirit", 1, 1).id();
    let card_creature = scenario.add_creature(P0, "Bear Cub", 2, 2).id();
    let kill_token = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY_TARGET_CREATURE)
        .id();
    let kill_card = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder B", true, DESTROY_TARGET_CREATURE)
        .id();
    let mut runner = scenario.build();
    // CR 111.1: the only difference between the two creatures is token identity,
    // which is what the antecedent selects on.
    runner
        .state_mut()
        .objects
        .get_mut(&token)
        .expect("the token creature exists")
        .is_token = true;

    // POSITIVE REACH-GUARD: the shield is live and does claim a token.
    // CR 111.7 removes a token from `objects` once it has left the battlefield,
    // so its destination is read off the `ZoneChanged` event rather than
    // `zone_of`, which would panic on the vanished id.
    let token_outcome = runner.cast(kill_token).target_object(token).resolve();
    assert_eq!(
        token_destination(&token_outcome, token),
        Some(Zone::Exile),
        "CR 111.1: the token-only antecedent must claim the token"
    );

    // THE BITE. Before the fix the subject was discarded and `valid_card` was
    // None, so this creature CARD was exiled along with the token.
    let card_outcome = runner
        .cast(kill_card)
        .target_object(card_creature)
        .resolve();
    assert_eq!(
        card_outcome.zone_of(card_creature),
        Zone::Graveyard,
        "CR 108.2b: tokens aren't cards, so a token-only antecedent must NOT \
         redirect a nontoken creature card"
    );
    // The resolving instant is a card too (CR 608.2n). An unfiltered redirect
    // swallowed it into exile, which is the same defect seen from the other end.
    assert_eq!(
        card_outcome.zone_of(kill_card),
        Zone::Graveyard,
        "CR 608.2n + CR 108.2b: the resolving spell card must reach its owner's \
         graveyard, not be claimed by a token-only shield"
    );
}

/// Where a token ended up, read from the zone-change event stream.
///
/// CR 111.7: a token that has left the battlefield ceases to exist and is
/// dropped from `objects`, so `Outcome::zone_of` cannot answer for it. The
/// `ZoneChanged` event is emitted before that cleanup and survives it.
fn token_destination(outcome: &CastOutcome, token: ObjectId) -> Option<Zone> {
    outcome.events().iter().find_map(|event| match event {
        GameEvent::ZoneChanged { object_id, to, .. } if *object_id == token => Some(*to),
        _ => None,
    })
}

fn filter_mentions(filter: &TargetFilter, wanted: &TypeFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed.type_filters.contains(wanted),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(|f| filter_mentions(f, wanted))
        }
        _ => false,
    }
}
