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
//!   * R8 — Declaration in Stone: a mass sweep scoped to the target's
//!     controller ("all other creatures its controller controls") names whose
//!     objects are swept, not who acts — the caster still performs it.
//!   * R9 — Ashiok, Nightmare Muse: the paired pick ("then that player exiles a
//!     card from their hand") with the same `ParentTargetController` scope is
//!     the subject form and records that player.
//!   * R10 — Mudhole: a public-action subject-form mass exile that parks on an
//!     optional `ReplacementChoice` records the subject for paused and resumed
//!     members. Three-player departure rows distinguish the subject from the
//!     resolving family's controller with an explicitly typed continuation
//!     witness; that witness is not part of Mudhole's printed Oracle.
//!     No supported printed card supplies this exile-destination replacement.
//!     The nearest printed replacements are Rest in Peace and Dauthi Voidwalker,
//!     which redirect graveyard-bound moves into exile. The fixture's optional
//!     exile-to-graveyard replacement is synthetic.
//!   * R11 — Rona, Tolarian Obliterator: another player's Lightning Bolt emits
//!     the real damage trigger, which randomly exiles one of two hand cards.
//!     The printed optional land tail is explicitly declined.
//!
//! Class premise for R8/R9: every parsed `ChangeZoneAll` scoped by
//! `ParentTargetController` is a subjectless sweep (Declaration in Stone,
//! Deputy of Detention, Eradicate, Counterbore, Legions to Ashes), and every
//! parsed `ChangeZone` scoped by it is a subject-form pick (Ashiok, Nightmare
//! Muse -3; Isildur's Fateful Strike; Azula, Cunning Usurper; Struggle for
//! Sanity's "That player exiles a card from it, then you exile a card from it").
//!
//! Revert sensitivity is stated per test. The gameplay readers of `exiled_by` are
//! `casting::exile_permission_pool` (guarded by
//! `uba_mask_draw_to_exile_play.rs`) and `PermissionGrantee::ObjectExiler` in
//! `grant_permission` (guarded by `vulcan_mind_meld_exiler_plays.rs`; Vulcan
//! Mind-Meld's exile is an `ExileTop`, which this change does not touch);
//! `object_content_eq` in `types/game_state.rs` also compares it for loop
//! detection. No printed card observes this change, and these rows claim no
//! reader-level discrimination.

use std::sync::Arc;

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, QuantityExpr, ReplacementDefinition, ReplacementMode,
    TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{
    PersistedGameState, PersistedRestoreFinalization, ReplacementChoiceKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
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

/// Declaration in Stone `{1}{W}` Sorcery — verbatim Oracle text.
const DECLARATION_IN_STONE: &str = "Exile target creature and all other creatures its controller controls with the same name as that creature. That player investigates for each nontoken creature exiled this way.";

/// Ashiok, Nightmare Muse `{3}{U}{B}` Legendary Planeswalker — Ashiok —
/// verbatim Oracle text (U+2212 minus signs).
const ASHIOK_NIGHTMARE_MUSE: &str = "[+1]: Create a 2/3 blue and black Nightmare creature token with \"Whenever this token attacks or blocks, each opponent exiles the top two cards of their library.\"\n[\u{2212}3]: Return target nonland permanent to its owner's hand, then that player exiles a card from their hand.\n[\u{2212}7]: You may cast up to three spells from among face-up cards your opponents own from exile without paying their mana costs.";

/// Mudhole `{2}{R}` Instant — verbatim Oracle text.
const MUDHOLE: &str = "Target player exiles all land cards from their graveyard.";

/// Rona, Tolarian Obliterator, the 5/5 back face — verbatim Oracle text.
const RONA_TOLARIAN_OBLITERATOR: &str = "Trample\nWhenever a source deals damage to Rona, that source's controller exiles a card from their hand at random. If it's a land card, you may put it onto the battlefield under your control. Otherwise, you may cast it without paying its mana cost.";

/// Lightning Bolt `{R}` Instant — verbatim Oracle text.
const LIGHTNING_BOLT: &str = "Lightning Bolt deals 3 damage to any target.";

const AJANIS_PRIDEMATE: &str = "Whenever you gain life, put a +1/+1 counter on this creature.";

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

fn assert_resolution_finished(runner: &GameRunner) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "expected ordinary priority, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert!(runner.state().resolving_stack_entry.is_none());
    assert!(runner.state().active_change_zone_frame().is_none());
    assert!(runner.state().active_ability_continuation().is_none());
    assert!(runner.state().pending_replacement.is_none());
    assert!(runner
        .state()
        .pending_zone_change_delivery_from_replacement()
        .is_none());
}

fn assert_pridemate_counters(runner: &GameRunner, expected: u32) {
    let observer = runner
        .state()
        .objects
        .values()
        .find(|object| object.name == "Ajani's Pridemate")
        .expect("the surviving printed life-gain observer exists");
    assert_eq!(observer.counters.values().copied().sum::<u32>(), expected);
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
/// single-eligible shortcut (no prompt), which is the site that passed the
/// controller instead of the computed chooser.
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
/// graveyard." The target names whose graveyard, not who acts; the
/// controller follows the instruction (CR 608.2c).
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

// ---------------------------------------------------------------------------
// R8 — a mass sweep scoped to the target's controller stays caster-performed.
// ---------------------------------------------------------------------------

/// Battlefield Clue tokens controlled by `player`.
fn clues_controlled(runner: &GameRunner, player: PlayerId) -> usize {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| o.controller == player && o.card_types.subtypes.iter().any(|s| s == "Clue"))
        .count()
}

/// Declaration in Stone: "Exile target creature and all other creatures its
/// controller controls with the same name as that creature." The second leg is
/// a subjectless sweep: "its controller" names whose creatures are swept, and
/// the spell's controller follows the instruction (CR 608.2c), so the caster
/// exiles both. The announced target is the caster's exile too (CR 601.2c).
///
/// Revert sensitivity: without the sweep rule the twin records `Some(P1)`.
#[test]
fn declaration_in_stone_sweep_stays_caster_performed() {
    assert_parses_clean(
        "Declaration in Stone",
        &[],
        &["Sorcery"],
        DECLARATION_IN_STONE,
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_creature(P1, "Grizzly Bear", 2, 2).id();
    let twin = scenario.add_creature(P1, "Grizzly Bear", 2, 2).id();
    // Hostile owner-vs-caster case: the caster's own same-name creature is not
    // part of "its controller controls", so it must stay.
    let decoy = scenario.add_creature(P0, "Grizzly Bear", 2, 2).id();
    let dis = scenario
        .add_spell_to_hand_from_oracle(P0, "Declaration in Stone", false, DECLARATION_IN_STONE)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![],
        })
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(dis).target_objects(&[victim]).resolve();

    // Reach-guards: the spell resolved fully, both of P1's bears were exiled,
    // and the controller scope bound to P1 (two Clues to P1, none to P0).
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "Declaration in Stone resolves cleanly; got {:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(zone(&runner, victim), Zone::Exile);
    assert_eq!(zone(&runner, twin), Zone::Exile);
    assert_eq!(zone(&runner, decoy), Zone::Battlefield);
    assert_eq!(clues_controlled(&runner, P1), 2, "one Clue per exiled bear");
    assert_eq!(clues_controlled(&runner, P0), 0);

    assert_eq!(
        exiled_by(&runner, victim),
        Some(P0),
        "the announced target is the caster's exile"
    );
    assert_eq!(
        exiled_by(&runner, twin),
        Some(P0),
        "a subjectless sweep scoped to the target's controller is the caster's exile"
    );
}

// ---------------------------------------------------------------------------
// R9 — the paired PICK with the same scope records the subject.
// ---------------------------------------------------------------------------

/// Ashiok, Nightmare Muse -3: "Return target nonland permanent to its owner's
/// hand, then that player exiles a card from their hand." The pick carries the
/// same `ParentTargetController` scope as R8's sweep but has a subject
/// ("that player"), so that player performs the exile.
///
/// Revert sensitivity: records `Some(P0)` against the base, and `Some(P0)`
/// against a sweep rule that does not distinguish a pick from a sweep.
#[test]
fn ashiok_minus_three_pick_records_that_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Opp Bear", 2, 2).id();
    let ashiok = scenario
        .add_planeswalker_from_oracle(
            P0,
            "Ashiok, Nightmare Muse",
            "Ashiok",
            5,
            ASHIOK_NIGHTMARE_MUSE,
        )
        .id();

    let mut runner = scenario.build();
    let index = runner.state().objects[&ashiok]
        .abilities
        .iter()
        .position(|ability| matches!(&*ability.effect, Effect::Bounce { .. }))
        .expect("Ashiok has the -3 ability (a Bounce chain)");
    // Reach-guard: the -3 chain parsed cleanly into a bounce followed by the
    // exile pick (the -7 line is a separate ability).
    let chain = &runner.state().objects[&ashiok].abilities[index];
    assert!(
        !chain_has_unimplemented(chain),
        "the -3 chain parses with no Unimplemented"
    );
    assert!(
        matches!(
            chain.sub_ability.as_deref().map(|sub| &*sub.effect),
            Some(Effect::ChangeZone { .. })
        ),
        "the -3 chain's second clause is the ChangeZone pick"
    );
    assert!(runner.state().players[P1.0 as usize].hand.is_empty());

    runner
        .activate(ashiok, index)
        .target_objects(&[bear])
        .resolve();
    drive_to_priority(&mut runner);

    // Reach-guard: single eligible card, no choice prompt, the bounced bear was
    // the one exiled.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "one eligible card takes the prompt-free shortcut; got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(zone(&runner, bear), Zone::Exile);
    assert_eq!(exile_count(&runner), 1);
    assert!(runner.state().players[P1.0 as usize].hand.is_empty());
    assert_eq!(
        exiled_by(&runner, bear),
        Some(P1),
        "\"that player exiles a card from their hand\" is that player's exile"
    );
}

// ---------------------------------------------------------------------------
// R10 — a paused subject-form mass exile records the subject across resume.
// ---------------------------------------------------------------------------

/// Mudhole's real mass-exile root, with one SYNTHETIC optional replacement.
/// The optional typed continuation is solely a family-lifetime witness, not
/// printed Mudhole text: P2 gains 3 life only if the surviving family completes.
fn mudhole_replacement_fixture(
    continuation_witness: bool,
) -> (GameRunner, [ObjectId; 2], ObjectId, ObjectId) {
    assert_parses_clean("Mudhole", &[], &["Instant"], MUDHOLE);

    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    if continuation_witness {
        assert_parses_clean("Ajani's Pridemate", &[], &["Creature"], AJANIS_PRIDEMATE);
        scenario
            .add_creature(P2, "Ajani's Pridemate", 2, 2)
            .from_oracle_text(AJANIS_PRIDEMATE);
    }
    let land_a = scenario
        .add_land_to_graveyard(P1, "Graveyard Forest A")
        .id();
    let land_b = scenario
        .add_land_to_graveyard(P1, "Graveyard Forest B")
        .id();
    scenario.with_graveyard(P1, &["Graveyard Relic"]);
    scenario
        .add_creature(P2, "Synthetic Exile Redirect", 1, 1)
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::Moved)
                .mode(ReplacementMode::Optional { decline: None })
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChangeZone {
                        origin: None,
                        destination: Zone::Graveyard,
                        target: TargetFilter::SelfRef,
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
                ))
                .destination_zone(Zone::Exile),
        );
    let mudhole = scenario
        .add_spell_to_hand_from_oracle(P0, "Mudhole", true, MUDHOLE)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![],
        })
        .id();

    let mut runner = scenario.build();
    if continuation_witness {
        let object = runner.state_mut().objects.get_mut(&mudhole).unwrap();
        for abilities in [&mut object.abilities, &mut object.base_abilities] {
            let mut tail = &mut Arc::make_mut(abilities)[0].sub_ability;
            while let Some(next) = tail {
                tail = &mut next.sub_ability;
            }
            *tail = Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 3 },
                    player: TargetFilter::SpecificPlayer { id: P2 },
                },
            )));
        }
    }
    let relic = runner.state().players[P1.0 as usize]
        .graveyard
        .iter()
        .copied()
        .find(|id| *id != land_a && *id != land_b)
        .expect("the nonland graveyard card exists");
    (runner, [land_a, land_b], relic, mudhole)
}

/// CR 608.2c: the target player performs every member, including after a pause.
/// The two actual optional replacement prompts are both declined publicly.
#[test]
fn mudhole_replacement_pause_resume_records_the_subject() {
    let (mut runner, [land_a, land_b], relic, mudhole) = mudhole_replacement_fixture(false);

    let outcome = runner.cast(mudhole).target_player(P1).resolve();

    // First prompt: the first land is parked on the replacement choice and the
    // second sits in the pending frame, carrying the subject as its performer.
    match outcome.final_waiting_for() {
        WaitingFor::ReplacementChoice { player, kind, .. } => {
            assert_eq!(
                *player, P1,
                "the engine routes the optional-branch prompt to the affected card's owner"
            );
            assert_eq!(*kind, ReplacementChoiceKind::OptionalBranch);
        }
        other => panic!("expected the first replacement prompt; got {other:?}"),
    }
    let frame = runner
        .state()
        .active_change_zone_frame()
        .and_then(|frame| frame.pending.as_ref())
        .expect("the paused mass move stashed a pending frame");
    assert_eq!(frame.remaining.len(), 1, "one land is still to be moved");

    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("decline the first replacement");

    // Second prompt: the resumed member parks again, with nothing left over.
    match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { player, kind, .. } => {
            assert_eq!(*player, P1);
            assert_eq!(*kind, ReplacementChoiceKind::OptionalBranch);
        }
        other => panic!("expected the second replacement prompt; got {other:?}"),
    }
    let frame = runner
        .state()
        .active_change_zone_frame()
        .and_then(|frame| frame.pending.as_ref())
        .expect("the resumed member re-parked with a pending frame");
    assert!(
        frame.remaining.is_empty(),
        "the last land is the one parked"
    );

    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("decline the second replacement");

    // Settled: nothing pending, both lands exiled by the subject.
    assert_resolution_finished(&runner);
    for land in [land_a, land_b] {
        assert_eq!(zone(&runner, land), Zone::Exile);
        assert_eq!(
            exiled_by(&runner, land),
            Some(P1),
            "\"target player exiles\" is the target player's exile, paused or resumed"
        );
    }
    assert_eq!(
        zone(&runner, relic),
        Zone::Graveyard,
        "the nonland card is not part of \"all land cards\""
    );
    assert_eq!(exile_count(&runner), 2);
}

/// Narrow typed continuation contract fixture, not an additional printed
/// Mudhole clause. This positive control proves the exact P2 benefit is reachable.
#[test]
fn mudhole_typed_continuation_completes_after_replacement_pauses() {
    let (mut runner, lands, relic, mudhole) = mudhole_replacement_fixture(true);
    runner.cast(mudhole).target_player(P1).resolve();
    for _ in lands {
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice {
                player: P1,
                kind: ReplacementChoiceKind::OptionalBranch,
                ..
            }
        ));
        runner
            .act(GameAction::ChooseReplacement { index: 1 })
            .expect("decline the optional replacement");
    }
    drive_to_priority(&mut runner);
    assert_resolution_finished(&runner);
    assert_eq!(
        runner.life(P2),
        23,
        "the typed continuation runs exactly once"
    );
    assert_pridemate_counters(&runner, 1);
    for land in lands {
        assert_eq!(zone(&runner, land), Zone::Exile);
        assert_eq!(exiled_by(&runner, land), Some(P1));
    }
    assert_eq!(zone(&runner, relic), Zone::Graveyard);
    assert_eq!(zone(&runner, mudhole), Zone::Graveyard);
}

#[test]
fn paused_subject_exile_restore_preserves_actor_before_public_resume() {
    let (mut runner, lands, _, mudhole) = mudhole_replacement_fixture(true);
    runner.cast(mudhole).target_player(P1).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { player: P1, .. }
    ));
    let snapshots = [
        PersistedGameState::Raw(Box::new(runner.state().clone())),
        PersistedGameState::capture(runner.state().clone()),
    ];
    for snapshot in snapshots {
        let bytes = serde_json::to_value(snapshot).expect("the public saved-game writer succeeds");
        let state = serde_json::from_value::<PersistedGameState>(bytes)
            .expect("the public saved-game reader accepts the current snapshot")
            .prepare_for_restore(PersistedRestoreFinalization::Immediate)
            .expect("the saved frame is valid for restoration")
            .finalize_immediately()
            .expect("the saved game restores");
        let mut restored = GameRunner::from_state(state);
        for _ in lands {
            assert!(matches!(
                restored.state().waiting_for,
                WaitingFor::ReplacementChoice { player: P1, .. }
            ));
            restored
                .act(GameAction::ChooseReplacement { index: 1 })
                .expect("the restored game accepts the actual replacement answer");
        }
        drive_to_priority(&mut restored);
        assert_resolution_finished(&restored);
        for land in lands {
            assert_eq!(zone(&restored, land), Zone::Exile);
            assert_eq!(exiled_by(&restored, land), Some(P1));
        }
        assert_eq!(restored.life(P2), 23);
        assert_pridemate_counters(&restored, 1);
        assert_eq!(zone(&restored, mudhole), Zone::Graveyard);
    }
}

/// CR 800.4a + CR 800.4g: losing the instruction's subject removes that
/// player's cards, not the surviving controller's resolving family.
/// The typed P2 benefit discriminates completion from silent abandonment;
/// Mudhole's own final part alone would not distinguish those outcomes.
#[test]
fn mudhole_subject_departure_preserves_the_surviving_family_continuation() {
    let (mut runner, lands, relic, mudhole) = mudhole_replacement_fixture(true);
    runner.cast(mudhole).target_player(P1).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice {
            player: P1,
            kind: ReplacementChoiceKind::OptionalBranch,
            ..
        }
    ));
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("decline the first replacement");

    // The first member really moved under P1's authority; the resumed member
    // must have reached its own actual replacement prompt before departure.
    let exiled: Vec<_> = lands
        .into_iter()
        .filter(|land| zone(&runner, *land) == Zone::Exile)
        .collect();
    assert_eq!(exiled.len(), 1);
    assert_eq!(exiled_by(&runner, exiled[0]), Some(P1));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice {
            player: P1,
            kind: ReplacementChoiceKind::OptionalBranch,
            ..
        }
    ));
    assert_eq!(runner.life(P2), 20, "the continuation has not run yet");
    runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("the subject concedes at the actual replacement prompt");

    assert!(runner.state().players[P1.0 as usize].is_eliminated);
    assert!(!runner.state().players[P0.0 as usize].is_eliminated);
    assert!(!runner.state().players[P2.0 as usize].is_eliminated);
    assert_eq!(
        runner.life(P2),
        23,
        "P1's departure must not discard P0's surviving continuation"
    );
    // CR 603.2: the resumed life-gain event must reach its real printed observer.
    drive_to_priority(&mut runner);
    assert_pridemate_counters(&runner, 1);
    assert_resolution_finished(&runner);
    assert_eq!(zone(&runner, mudhole), Zone::Graveyard);
    for card in lands.into_iter().chain([relic]) {
        assert_eq!(
            zone(&runner, card),
            Zone::Exile,
            "P1-owned cards leave the game with P1"
        );
    }
}

/// CR 800.4a: the resolving family's controller leaves, so its typed tail
/// must retire even though the subject, replacement source and P2 beneficiary
/// all survive. The positive row above proves the benefit can actually run.
#[test]
fn mudhole_controller_departure_retires_the_family_continuation() {
    let (mut runner, lands, relic, mudhole) = mudhole_replacement_fixture(true);
    runner.cast(mudhole).target_player(P1).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice {
            player: P1,
            kind: ReplacementChoiceKind::OptionalBranch,
            ..
        }
    ));
    assert_eq!(runner.life(P2), 20);
    runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("the controller concedes at the actual replacement prompt");

    assert!(runner.state().players[P0.0 as usize].is_eliminated);
    assert!(!runner.state().players[P1.0 as usize].is_eliminated);
    assert!(!runner.state().players[P2.0 as usize].is_eliminated);
    assert_eq!(runner.life(P2), 20, "the departed family's tail cannot run");
    assert_resolution_finished(&runner);
    assert_pridemate_counters(&runner, 0);
    assert_eq!(
        zone(&runner, mudhole),
        Zone::Exile,
        "P0's spell leaves the game"
    );
    for card in lands.into_iter().chain([relic]) {
        assert_eq!(
            zone(&runner, card),
            Zone::Graveyard,
            "P1's surviving cards must not be moved by the retired family"
        );
        assert_eq!(exiled_by(&runner, card), None);
    }
}

// ---------------------------------------------------------------------------
// R11 — Rona's real damage trigger binds the Random pick to the damage source.
// ---------------------------------------------------------------------------

/// CR 603.2: damage from P1's publicly cast Lightning Bolt triggers P0's Rona.
/// CR 608.2c: "that source's controller exiles" makes P1 the performer, while
/// the printed "you may" land tail still belongs to Rona's controller P0.
/// Both hand candidates are lands, so the actual optional land instruction
/// must be offered regardless of which random identity the engine chooses.
#[test]
fn rona_damage_trigger_random_pick_records_the_damage_source_controller() {
    assert_parses_clean(
        "Rona, Tolarian Obliterator",
        &["Trample"],
        &["Creature"],
        RONA_TOLARIAN_OBLITERATOR,
    );
    assert_parses_clean("Lightning Bolt", &[], &["Instant"], LIGHTNING_BOLT);
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let rona = scenario
        .add_creature(P0, "Rona, Tolarian Obliterator", 5, 5)
        .from_oracle_text_with_keywords(&["Trample"], RONA_TOLARIAN_OBLITERATOR)
        .id();
    let candidates = [
        scenario.add_land_to_hand(P1, "Hand Forest A").id(),
        scenario.add_land_to_hand(P1, "Hand Forest B").id(),
    ];
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P1, "Lightning Bolt", true, LIGHTNING_BOLT)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes so P1 can cast Lightning Bolt");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    let outcome = runner
        .cast(bolt)
        .target_objects(&[rona])
        .commit()
        .stop_at_optional()
        .resolve();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::OptionalEffectChoice { player: P0, .. }
        ),
        "Rona's actual printed optional land tail must be offered to P0; got {:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(runner.state().objects[&rona].damage_marked, 3);
    assert_eq!(zone(&runner, bolt), Zone::Graveyard);
    let exiled: Vec<_> = candidates
        .into_iter()
        .filter(|card| zone(&runner, *card) == Zone::Exile)
        .collect();
    let remaining: Vec<_> = candidates
        .into_iter()
        .filter(|card| zone(&runner, *card) == Zone::Hand)
        .collect();
    assert_eq!((exiled.len(), remaining.len()), (1, 1));
    assert!(candidates.contains(&exiled[0]));
    assert!(candidates.contains(&remaining[0]));
    assert_ne!(exiled[0], remaining[0]);
    assert_eq!(runner.state().players[P1.0 as usize].hand.len(), 1);
    assert_eq!(exile_count(&runner), 1);
    assert_eq!(
        exiled_by(&runner, exiled[0]),
        Some(P1),
        "Rona attributes the random exile to the damage source's controller"
    );
    assert_eq!(exiled_by(&runner, remaining[0]), None);
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("decline Rona's printed optional land instruction");
    drive_to_priority(&mut runner);
    assert_resolution_finished(&runner);
    assert_eq!(zone(&runner, rona), Zone::Battlefield);
    assert_eq!(zone(&runner, exiled[0]), Zone::Exile);
    assert_eq!(zone(&runner, remaining[0]), Zone::Hand);
    assert_eq!(exiled_by(&runner, exiled[0]), Some(P1));
    assert_eq!(exile_count(&runner), 1);
}
