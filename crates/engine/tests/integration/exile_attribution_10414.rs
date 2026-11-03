//! Issue #10414 — the exiling player is the instruction's acting player.
//!
//! A zone move's `performed_by` feeds `GameObject::exiled_by` (CR 406.6 +
//! CR 607.2a/b, the per-card record the exile-cast permissions read) and entry
//! ownership under `EntryOwnership::ActorOwns`. When the printed instruction
//! names another player as its subject ("that player exiles a card from their
//! hand", "target opponent exiles a creature they control", "each opponent
//! exiles a card from their hand"), that subject performs the move — the
//! controller of the spell or ability performs only the unsubjected case
//! (CR 608.2c).
//!
//! Rows, each with a verbatim Oracle text and a paired positive reach-guard:
//!   * R1 — Strategic Betrayal: a subject-form MASS leg records the subject
//!     (`resolve_all` member loop).
//!   * R2 — Skullcap Snail: a subject-form PICK records the subject
//!     (single-eligible shortcut in `resolve`).
//!   * R7 — Rahilda, Wanted Cutthroat: a library-owned subject filter is NOT
//!     the subject's exile — "their library" names whose library it is, so the
//!     caster still performs it (the `move_performer` library gate).
//!   * R3 — Froghemoth: an announced-target move stays caster-performed even
//!     though the filter names another player's graveyard.
//!   * R5 — Tormod's Crypt: a player-valued filter ("target player's
//!     graveyard") stays caster-performed.
//!   * R6 — Yarok's Fenlurker: the `player_scope` fan-out keeps per-seat
//!     attribution.
//!
//! Revert sensitivity is stated per test. The one reader of `exiled_by`
//! (`casting::exile_permission_pool`) is guarded by
//! `uba_mask_draw_to_exile_play.rs`; no printed card observes this change, and
//! these rows claim no reader-level discrimination.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityDefinition, Effect};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

/// Strategic Betrayal `{1}{B}` Sorcery — verbatim Oracle text.
const STRATEGIC_BETRAYAL: &str =
    "Target opponent exiles a creature they control and their graveyard.";

/// Skullcap Snail `{1}{B}` Creature — Fungus Snail — verbatim Oracle text.
const SKULLCAP_SNAIL: &str =
    "When this creature enters, target opponent exiles a card from their hand.";

/// Rahilda, Wanted Cutthroat `{1}{R}` Legendary Creature — Human Werewolf 2/2 —
/// verbatim Oracle text.
const RAHILDA_WANTED_CUTTHROAT: &str = "First strike\nWhen Rahilda, Wanted Cutthroat deals combat damage to a player, exile a nonland card from their library at random. During any turn you attacked with a Wolf or Werewolf, you may cast that card and you may spend mana as though it were mana of any color to cast that spell.\nDaybound (If a player casts no spells during their own turn, it becomes night next turn.)";

/// Froghemoth `{3}{G}{G}` Creature — Frog Horror 4/4 — verbatim Oracle text.
const FROGHEMOTH: &str = "Trample, haste\nWhenever this creature deals combat damage to a player, exile up to that many target cards from their graveyard. Put a +1/+1 counter on this creature for each creature card exiled this way. You gain 1 life for each noncreature card exiled this way.";

/// Tormod's Crypt `{0}` Artifact — verbatim Oracle text.
const TORMODS_CRYPT: &str = "{T}, Sacrifice this artifact: Exile target player's graveyard.";

/// Yarok's Fenlurker `{B}{B}` Creature — Horror 1/1 — verbatim Oracle text.
const YAROKS_FENLURKER: &str = "When this creature enters, each opponent exiles a card from their hand.\n{2}{B}: This creature gets +1/+1 until end of turn.";

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn effect_unimplemented(effect: &Effect) -> bool {
    matches!(effect, Effect::Unimplemented { .. })
}

fn chain_has_unimplemented(def: &AbilityDefinition) -> bool {
    effect_unimplemented(&def.effect)
        || def
            .sub_ability
            .as_deref()
            .is_some_and(chain_has_unimplemented)
        || def
            .else_ability
            .as_deref()
            .is_some_and(chain_has_unimplemented)
}

/// Reach-guard: the card's own text must parse to real effects — no strict
/// failure hidden behind a green runtime assertion. `keywords` are the card's
/// printed keyword names, which the real loader supplies so the keyword-only
/// lines are recognized rather than read as unknown instructions.
fn assert_parses_clean(name: &str, keywords: &[&str], types: &[&str], oracle: &str) {
    let keywords: Vec<String> = keywords.iter().map(|k| (*k).to_string()).collect();
    let types: Vec<String> = types.iter().map(|t| (*t).to_string()).collect();
    let parsed = parse_oracle_text(oracle, name, &keywords, &types, &[]);
    let clean = parsed.abilities.iter().all(|a| !chain_has_unimplemented(a))
        && parsed.triggers.iter().all(|trigger| {
            !trigger
                .execute
                .as_deref()
                .is_some_and(chain_has_unimplemented)
        });
    assert!(
        clean,
        "{name} must parse with no strict-failure Unimplemented; got {parsed:#?}"
    );
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn exiled_by(runner: &GameRunner, id: ObjectId) -> Option<PlayerId> {
    runner.state().objects[&id].exiled_by
}

fn exile_count(runner: &GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|object| object.zone == Zone::Exile)
        .count()
}

/// Pass priority, order triggers and decline optional instructions until the
/// stack is empty and a priority window is open. Any target prompt still open
/// at this point is one these rows answer up front, so it is declined.
fn drive_to_priority(runner: &mut GameRunner) {
    for _ in 0..60 {
        match &runner.state().waiting_for {
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("trigger ordering is accepted");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("declining an optional instruction is accepted");
            }
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: Vec::new(),
                    })
                    .expect("declining the trigger's targets is accepted");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass is accepted");
            }
            other => panic!("unexpected prompt while driving resolution: {other:?}"),
        }
    }
    panic!(
        "drive_to_priority did not settle: {:?}",
        runner.state().waiting_for
    );
}

fn activated_index(runner: &GameRunner, source: ObjectId) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| ability.cost.is_some())
        .expect("an activated ability")
}

// ---------------------------------------------------------------------------
// R1 — a subject-form MASS leg records the subject.
// ---------------------------------------------------------------------------

/// Strategic Betrayal: "Target opponent exiles a creature they control and
/// their graveyard." The creature leg is a resolution-time pick (the opponent
/// chooses), the graveyard leg is a mass move. Both are the opponent's exile.
///
/// Revert sensitivity: the mass-leg assertions fail if `performed_by` reverts
/// to `ability.controller` (`Some(P0)`); the prompted creature leg is already
/// right and stays as the positive control.
#[test]
fn strategic_betrayal_mass_leg_records_the_subject() {
    assert_parses_clean("Strategic Betrayal", &[], &["Sorcery"], STRATEGIC_BETRAYAL);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let plain = scenario.add_creature(P1, "Opp Plain Beast", 2, 2).id();
    let other = scenario.add_creature(P1, "Opp Other Beast", 1, 1).id();
    scenario.with_graveyard(P1, &["Grave Relic A", "Grave Relic B"]);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Strategic Betrayal", false, STRATEGIC_BETRAYAL)
        .id();

    let mut runner = scenario.build();
    let graveyard: Vec<ObjectId> = runner.state().players[P1.0 as usize]
        .graveyard
        .iter()
        .copied()
        .collect();
    assert_eq!(
        graveyard.len(),
        2,
        "opponent graveyard seeded with two cards"
    );

    let outcome = runner.cast(spell).target_player(P1).resolve();
    match outcome.final_waiting_for() {
        WaitingFor::EffectZoneChoice { player, cards, .. } => {
            assert_eq!(*player, P1, "the opponent chooses the exiled creature");
            assert!(
                cards.contains(&plain) && cards.contains(&other),
                "both of the opponent's creatures must be offered; got {cards:?}"
            );
        }
        other => panic!("expected the opponent's creature choice; got {other:?}"),
    }

    runner
        .act(GameAction::SelectCards { cards: vec![plain] })
        .expect("the opponent selects a creature to exile");

    // Positive control: the prompted single-object leg already records the
    // subject (the interactive pick resume binds the chooser).
    assert_eq!(zone(&runner, plain), Zone::Exile);
    assert_eq!(
        exiled_by(&runner, plain),
        Some(P1),
        "the prompted creature leg records the opponent as the exiling player"
    );

    // The claim: the mass leg records the subject too.
    assert_eq!(zone(&runner, other), Zone::Battlefield);
    for card in &graveyard {
        assert_eq!(
            zone(&runner, *card),
            Zone::Exile,
            "every card in the opponent's graveyard is exiled"
        );
        assert_eq!(
            exiled_by(&runner, *card),
            Some(P1),
            "the mass leg's exiling player is the instruction's subject"
        );
    }
}

// ---------------------------------------------------------------------------
// R2 — a subject-form PICK records the subject.
// ---------------------------------------------------------------------------

/// Skullcap Snail: "When this creature enters, target opponent exiles a card
/// from their hand." With exactly one eligible card the resolver takes its
/// single-eligible shortcut (no prompt), which is the site that discarded the
/// computed performer.
///
/// Revert sensitivity: reverted, this records `Some(P0)`.
#[test]
fn skullcap_snail_pick_records_the_subject() {
    assert_parses_clean("Skullcap Snail", &[], &["Creature"], SKULLCAP_SNAIL);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let held = scenario.add_card_to_hand(P1, "Held Card");
    let snail = scenario
        .add_creature_to_hand_from_oracle(P0, "Skullcap Snail", 1, 1, SKULLCAP_SNAIL)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(snail).target_player(P1).resolve();

    // Reach-guard: the single-eligible shortcut ran (no choice prompt), and the
    // card actually moved.
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "one eligible card takes the prompt-free shortcut; got {:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(zone(&runner, snail), Zone::Battlefield);
    assert_eq!(zone(&runner, held), Zone::Exile, "the card was exiled");
    assert_eq!(
        exiled_by(&runner, held),
        Some(P1),
        "the instruction's subject performs the pick"
    );
}

// ---------------------------------------------------------------------------
// R7 — a library-owned subject filter keeps the caster as performer.
// ---------------------------------------------------------------------------

/// Rahilda, Wanted Cutthroat: "…exile a nonland card from their library at
/// random." "Their" names whose library it is, not who acts: the caster exiles.
/// The pick is random, so it takes the random-single shortcut.
///
/// Revert sensitivity: applying the blanket `filter_controller` rule to this
/// shape records `Some(P1)` and fails; base behavior (`Some(P0)`) passes.
#[test]
fn rahilda_library_owner_filter_stays_caster_performed() {
    assert_parses_clean(
        "Rahilda, Wanted Cutthroat",
        &["First strike", "Daybound"],
        &["Creature"],
        RAHILDA_WANTED_CUTTHROAT,
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Generic named cards carry no card types, so both are nonland.
    scenario.with_library_top(P1, &["Library Card A", "Library Card B"]);
    let rahilda = scenario
        .add_creature_from_oracle(
            P0,
            "Rahilda, Wanted Cutthroat",
            2,
            2,
            RAHILDA_WANTED_CUTTHROAT,
        )
        .id();

    let mut runner = scenario.build();
    let library: Vec<ObjectId> = runner.state().players[P1.0 as usize]
        .library
        .iter()
        .copied()
        .collect();
    assert_eq!(
        library.len(),
        2,
        "the damaged player's library holds two cards"
    );

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(rahilda, AttackTarget::Player(P1))])
        .expect("Rahilda attacks the opponent");
    runner.combat_damage();
    drive_to_priority(&mut runner);

    // Reach-guard: exactly one card moved out of the library.
    let exiled: Vec<ObjectId> = library
        .iter()
        .copied()
        .filter(|card| zone(&runner, *card) == Zone::Exile)
        .collect();
    let remaining: Vec<ObjectId> = library
        .iter()
        .copied()
        .filter(|card| zone(&runner, *card) == Zone::Library)
        .collect();
    assert_eq!(
        (exiled.len(), remaining.len()),
        (1, 1),
        "the random single-card pick exiles exactly one of the two cards"
    );
    assert_eq!(exile_count(&runner), 1, "nothing else was exiled");
    assert_eq!(
        exiled_by(&runner, exiled[0]),
        Some(P0),
        "a library-owned subject filter is the caster's exile, not the owner's"
    );
}

// ---------------------------------------------------------------------------
// R3 — an announced-target move stays caster-performed.
// ---------------------------------------------------------------------------

/// Froghemoth: "Whenever this creature deals combat damage to a player, exile
/// up to that many target cards from their graveyard." The caster announces
/// those targets (CR 601.2c), so the caster performs the move even though the
/// filter names the damaged player's graveyard.
///
/// Revert sensitivity: applying the subject rule on the announced-target path
/// would record the damaged player and fail this row.
#[test]
fn froghemoth_announced_targets_stay_caster_performed() {
    assert_parses_clean(
        "Froghemoth",
        &["Trample", "Haste"],
        &["Creature"],
        FROGHEMOTH,
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_graveyard(P1, &["Graveyard Card"]);
    let froghemoth = scenario
        .add_creature_from_oracle(P0, "Froghemoth", 4, 4, FROGHEMOTH)
        .id();

    let mut runner = scenario.build();
    let graveyard: Vec<ObjectId> = runner.state().players[P1.0 as usize]
        .graveyard
        .iter()
        .copied()
        .collect();
    assert_eq!(
        graveyard.len(),
        1,
        "the damaged player has one graveyard card"
    );
    let target = graveyard[0];

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(froghemoth, AttackTarget::Player(P1))])
        .expect("Froghemoth attacks the opponent");
    runner.combat_damage();

    // Reach-guard: the damaged-trigger fired and offers the graveyard card.
    match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection { target_slots, .. } => {
            let offered = target_slots
                .iter()
                .flat_map(|slot| slot.legal_targets.iter())
                .any(|legal| {
                    matches!(legal, engine::types::ability::TargetRef::Object(id) if *id == target)
                });
            assert!(
                offered,
                "the trigger offers the damaged player's graveyard card; got {target_slots:?}"
            );
        }
        other => panic!("expected the damage trigger's target prompt; got {other:?}"),
    }

    runner
        .act(GameAction::SelectTargets {
            targets: vec![engine::types::ability::TargetRef::Object(target)],
        })
        .expect("the caster announces the trigger's target");
    drive_to_priority(&mut runner);

    assert_eq!(zone(&runner, target), Zone::Exile, "the card was exiled");
    assert_eq!(
        exiled_by(&runner, target),
        Some(P0),
        "announced-target moves are the caster's exile"
    );
}

// ---------------------------------------------------------------------------
// R5 — a player-valued filter stays caster-performed.
// ---------------------------------------------------------------------------

/// Tormod's Crypt: "{T}, Sacrifice this artifact: Exile target player's
/// graveyard." The target names whose graveyard, not who acts (CR 109.5).
///
/// Revert sensitivity: treating a player-valued filter as the subject records
/// the graveyard's owner and fails; base behavior passes.
#[test]
fn tormods_crypt_player_filter_stays_caster_performed() {
    assert_parses_clean("Tormod's Crypt", &[], &["Artifact"], TORMODS_CRYPT);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_graveyard(P1, &["Grave Card A", "Grave Card B"]);
    let crypt = scenario
        .add_artifact_from_oracle(P0, "Tormod's Crypt", TORMODS_CRYPT)
        .id();

    let mut runner = scenario.build();
    let graveyard: Vec<ObjectId> = runner.state().players[P1.0 as usize]
        .graveyard
        .iter()
        .copied()
        .collect();
    assert_eq!(
        graveyard.len(),
        2,
        "the opponent's graveyard holds two cards"
    );

    let index = activated_index(&runner, crypt);
    runner.activate(crypt, index).target_player(P1).resolve();
    drive_to_priority(&mut runner);

    for card in &graveyard {
        assert_eq!(
            zone(&runner, *card),
            Zone::Exile,
            "the targeted player's whole graveyard is exiled"
        );
        assert_eq!(
            exiled_by(&runner, *card),
            Some(P0),
            "a player-valued filter is the caster's exile"
        );
    }
}

// ---------------------------------------------------------------------------
// R6 — the `player_scope` fan-out keeps per-seat attribution.
// ---------------------------------------------------------------------------

/// Yarok's Fenlurker: "When this creature enters, each opponent exiles a card
/// from their hand." The fan-out rebinds the controller per seat, so each
/// seat's own exile is its own.
///
/// Revert sensitivity: this row preserves an already-correct behavior through
/// the same gate (the fan-out's seat controller is the performer, and the
/// library gate does not fire on a hand filter).
#[test]
fn yaroks_fenlurker_fan_out_keeps_per_seat_attribution() {
    assert_parses_clean("Yarok's Fenlurker", &[], &["Creature"], YAROKS_FENLURKER);

    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let p1_card = scenario.add_card_to_hand(P1, "P1 Held Card");
    let p2_card = scenario.add_card_to_hand(P2, "P2 Held Card");
    let fenlurker = scenario
        .add_creature_to_hand_from_oracle(P0, "Yarok's Fenlurker", 1, 1, YAROKS_FENLURKER)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(fenlurker).resolve();
    drive_to_priority(&mut runner);

    // Reach-guard: the enters trigger resolved and both opponents lost a card.
    assert_eq!(zone(&runner, p1_card), Zone::Exile);
    assert_eq!(zone(&runner, p2_card), Zone::Exile);
    assert_eq!(
        exiled_by(&runner, p1_card),
        Some(P1),
        "each opponent's own exile is attributed to that opponent"
    );
    assert_eq!(
        exiled_by(&runner, p2_card),
        Some(P2),
        "each opponent's own exile is attributed to that opponent"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. })
            || matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the fan-out completes without a leftover prompt"
    );
}
