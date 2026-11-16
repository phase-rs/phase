//! Issue #10414 — the exiling player is the instruction's acting player.
//!
//! A zone move's `performed_by` follows the instruction's subject (CR 608.2c).
//! Delivery writes that player to `GameObject::exiled_by` on each card that
//! reaches exile. Entry ownership under `EntryOwnership::ActorOwns` (Dandân)
//! reads a separate input from the resolving controller or the zone prompt's
//! chooser on the interactive route. When the printed instruction
//! names another player as its subject ("that player exiles a card from their
//! hand", "target opponent exiles a creature they control", "each opponent
//! exiles a card from their hand"), that subject performs the move — the
//! controller of the spell or ability performs only the unsubjected case
//! (CR 608.2c). The parser carries the subject as the zone effect's `actor`;
//! no row's attribution is read from the shape of its filter.
//!
//! Rows, each with a verbatim Oracle text and a paired positive reach-guard:
//!   * R1 — Strategic Betrayal: a subject-form MASS leg records the subject
//!     (`resolve_all` member loop).
//!   * R2 — Skullcap Snail: a subject-form PICK records the subject
//!     (single-eligible shortcut in `resolve`).
//!   * R7 — Rahilda, Wanted Cutthroat: a library-owned filter is NOT the
//!     subject's exile — "their library" names whose library it is, and the
//!     imperative clause has no subject, so the caster performs it.
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
//!     card from their hand") is the subject form and records that player: the
//!     returned permanent's OWNER (`ParentTargetOwner`), told apart from its
//!     controller in a second row.
//!   * R10 — Mudhole: a public-action subject-form mass exile that parks on an
//!     optional `ReplacementChoice` records the subject for paused and resumed
//!     members. An explicitly typed continuation witness, not part of
//!     Mudhole's printed Oracle, shows the family completes. A real
//!     Aethersnatch transfer also distinguishes both from the original caster.
//!     Current saves keep the subject performer separate. Deployed v11 saves
//!     preserve their recorded controller as both family and historical actor.
//!     No supported printed card supplies this exile-destination replacement.
//!     The nearest printed replacements are Rest in Peace and Dauthi Voidwalker,
//!     which redirect graveyard-bound moves into exile. The fixture's optional
//!     exile-to-graveyard replacement is synthetic.
//!   * R11 — Rona, Tolarian Obliterator: another player's Lightning Bolt emits
//!     the real damage trigger, which randomly exiles one of two hand cards.
//!     The printed optional land tail is explicitly declined.
//!   * Reference kinds: one printed card per player reference the parser
//!     stamps — Fiend of the Shadows (`TriggeringPlayer`), Bane of Bala Ged
//!     (`DefendingPlayer`), Arcane Artisan (`DeclaredPlayer`), Azula, Cunning
//!     Usurper (`DeclaredPlayer` reached by the chain's anaphor rewrite), Elkin Lair
//!     (`ScopedPlayer`), Wildfire Devils (`Typed { ChosenPlayer }`). Where the
//!     move can prompt, the single-candidate shortcut and the interactive
//!     selection must credit the same player. (`ParentTargetOwner` is
//!     exercised by Ashiok's R9 rows; `change_zone.rs` also has a synthetic
//!     resolver unit row for it.)
//!   * Dandân — Skullwinder: an opponent-performed return keeps the receiving
//!     hand and owner the entry-ownership input decides (the controller on the
//!     shortcut, the prompt's chooser on the selection route), not the
//!     performer. These rows pin that outcome unchanged; whether the printed
//!     "their hand" should send the card to the performer's hand under Dandân
//!     is a separate entry-ownership question this change does not decide.
//!
//! R8 and R9 both name a player through the object the ability acted on.
//! Declaration in Stone's sweep is imperative, so its actor stays `Controller`;
//! Ashiok's "then that player exiles a card from their hand" has a subject, so
//! its actor is that player, the owner whose hand the permanent returned to.
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
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    PersistedGameState, PersistedRestoreFinalization, ReplacementChoiceKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::resolution::ResolutionFrame;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

/// Unmodified Raw/Trusted output from deployed writer
/// 82d5dfc9b86c47cd6afa7359e6177785972ff81a (wire 11), captured after public,
/// paid Aethersnatch/Mudhole actions and the documented synthetic pause/witness.
/// Raw SHA-256: 290548f929910b2c0950f42cc95e9d70a90b35eb9a7a8bc16587a67ce394f3a2
/// Trusted SHA-256: 1565330bfae23056a67105f929d15037c8c00aed2598567018920157d8f8a57a
const DEPLOYED_V11_EXILE_SAVES: [(&str, &[u8]); 2] = [
    (
        "Raw",
        include_bytes!("../fixtures/exile_attribution_v11_82d5dfc9_raw.json"),
    ),
    (
        "Trusted",
        include_bytes!("../fixtures/exile_attribution_v11_82d5dfc9_trusted.json"),
    ),
];

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

/// Aethersnatch `{4}{U}{U}` Instant — verbatim MTGJSON Oracle text.
const AETHERSNATCH: &str = "Gain control of target spell. You may choose new targets for it. (If that spell becomes a permanent, it enters under your control.)";

/// Rona, Tolarian Obliterator, the 5/5 back face — verbatim Oracle text.
const RONA_TOLARIAN_OBLITERATOR: &str = "Trample\nWhenever a source deals damage to Rona, that source's controller exiles a card from their hand at random. If it's a land card, you may put it onto the battlefield under your control. Otherwise, you may cast it without paying its mana cost.";

/// Lightning Bolt `{R}` Instant — verbatim Oracle text.
const LIGHTNING_BOLT: &str = "Lightning Bolt deals 3 damage to any target.";

/// Fiend of the Shadows `{3}{B}{B}` Creature — Vampire Wizard 3/3 — verbatim
/// Oracle text.
const FIEND_OF_THE_SHADOWS: &str = "Flying\nWhenever this creature deals combat damage to a player, that player exiles a card from their hand. You may play that card for as long as it remains exiled.\nSacrifice a Human: Regenerate this creature.";

/// Elkin Lair `{3}{R}` World Enchantment — verbatim Oracle text.
const ELKIN_LAIR: &str = "At the beginning of each player's upkeep, that player exiles a card at random from their hand. The player may play that card this turn. At the beginning of the next end step, if the player hasn't played the card, they put it into their graveyard.";

/// Arcane Artisan `{2}{U}` Creature — Human Wizard 0/3 — verbatim Oracle text.
const ARCANE_ARTISAN: &str = "{2}{U}, {T}: Target player draws a card, then exiles a card from their hand. If a creature card is exiled this way, that player creates a token that's a copy of that card.\nWhen this creature leaves the battlefield, exile all tokens created with it at the beginning of the next end step.";

/// Bane of Bala Ged `{7}` Creature — Eldrazi 7/5 — verbatim Oracle text.
const BANE_OF_BALA_GED: &str =
    "Whenever this creature attacks, defending player exiles two permanents they control.";

/// Wildfire Devils `{3}{R}` Creature — Devil 4/2 — verbatim Oracle text.
const WILDFIRE_DEVILS: &str = "When this creature enters and at the beginning of your upkeep, choose a player at random. That player exiles an instant or sorcery card from their graveyard. Copy that card. You may cast the copy without paying its mana cost.";

const AZULA_CUNNING_USURPER: &str = "Firebending 2 (Whenever this creature attacks, add {R}{R}. This mana lasts until end of combat.)\nWhen Azula enters, target opponent exiles a nontoken creature they control, then they exile a nonland card from their graveyard.\nDuring your turn, you may cast cards exiled with Azula and you may cast them as though they had flash. Mana of any type can be spent to cast those spells.";

const SKULLWINDER: &str = "Deathtouch\nWhen this creature enters, return target card from your graveyard to your hand, then choose an opponent. That player returns a card from their graveyard to their hand.";

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
        if drive_step(runner) {
            return;
        }
    }
    panic!(
        "drive_to_priority did not settle: {:?}",
        runner.state().waiting_for
    );
}

/// One step of [`drive_to_priority`]; `true` once the stack is empty and a
/// priority window is open.
fn drive_step(runner: &mut GameRunner) -> bool {
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
                return true;
            }
            runner
                .act(GameAction::PassPriority)
                .expect("priority pass is accepted");
        }
        other => panic!("unexpected prompt while driving resolution: {other:?}"),
    }
    false
}

/// The zone-choice prompts a run answered, as `(chooser, performer)`.
type ZoneChoicePrompts = Vec<(PlayerId, Option<PlayerId>)>;

/// [`drive_to_priority`], answering each zone-choice prompt with its first
/// offered cards. Returns the prompts answered.
fn drive_answering_zone_choices(runner: &mut GameRunner) -> ZoneChoicePrompts {
    let mut answered = Vec::new();
    for _ in 0..60 {
        if let WaitingFor::EffectZoneChoice {
            player,
            cards,
            count,
            performed_by,
            ..
        } = &runner.state().waiting_for
        {
            answered.push((*player, *performed_by));
            let chosen = cards.iter().copied().take(*count).collect();
            runner
                .act(GameAction::SelectCards { cards: chosen })
                .expect("the zone choice accepts its first offered cards");
            continue;
        }
        // An optional follow-up pick ("you may cast the copy") is declined.
        if matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseFromZoneChoice { up_to: true, .. }
        ) {
            runner
                .act(GameAction::SelectCards { cards: Vec::new() })
                .expect("an optional pick accepts no cards");
            continue;
        }
        if drive_step(runner) {
            return answered;
        }
    }
    panic!(
        "drive_answering_zone_choices did not settle: {:?}",
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

/// CR 608.2c + CR 730.3: Strategic Betrayal when the creature the
/// opponent exiles is a merged permanent made by a printed mutate cast, all
/// through public actions and the runner's turn advance. In P0's main phase P0
/// casts Gemrazer (real card data) for its mutate cost onto P0's own printed
/// non-Human creature (Grizzly Bears) and accepts the merge placement. The
/// runner then advances to P1's turn, where P1 casts Strategic Betrayal at P0;
/// P0, with a second printed creature available, selects the merged one. Both
/// printed component cards reach exile attributed to P0, and the unchosen
/// creature stays on the battlefield. (CR 702.140c: the creature spell merges
/// with its target as it resolves.)
///
/// Revert sensitivity: with the component requests built without the
/// performer, both components reach exile with no exiler recorded.
#[test]
fn strategic_betrayal_merged_creature_records_the_subject_on_every_component() {
    use engine::game::scenario_db::GameScenarioDbExt;
    use engine::types::game_state::{CastingVariant, StackEntryKind};

    let db = crate::support::shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gemrazer = scenario.add_real_card(P0, "Gemrazer", Zone::Hand, db);
    let host = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let other = scenario.add_real_card(P0, "Runeclaw Bear", Zone::Battlefield, db);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Strategic Betrayal", false, STRATEGIC_BETRAYAL)
        .id();
    // Both players draw on the turns the runner advances through.
    scenario.with_library_top(P0, &["P0 Filler A", "P0 Filler B"]);
    scenario.with_library_top(P1, &["P1 Filler A", "P1 Filler B"]);
    // Gemrazer's mutate cost is {1}{G}{G}.
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // P0 casts Gemrazer for mutate onto P0's own creature.
    let offered = engine::ai_support::legal_actions(runner.state())
        .into_iter()
        .find(|action| {
            matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == gemrazer)
        })
        .expect("the mutate cast is offered");
    runner.act(offered).expect("the mutate cast is accepted");
    runner
        .act(GameAction::SelectTargets {
            targets: vec![engine::types::ability::TargetRef::Object(host)],
        })
        .expect("P0's own non-Human creature is the mutate target");
    assert!(
        runner.state().stack.iter().any(|entry| matches!(
            &entry.kind,
            StackEntryKind::Spell {
                casting_variant: CastingVariant::Mutate,
                ..
            } if entry.id == gemrazer
        )),
        "Gemrazer is on the stack cast for mutate"
    );
    runner.advance_until_stack_empty();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::MutateMergeChoice { player, .. } if player == P0
        ),
        "the merge placement is P0's choice; got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseMutateMergeSide {
            side: engine::game::merge::MergeSide::Top,
        })
        .expect("the merge placement is accepted");
    runner.advance_until_stack_empty();
    // Reach-guard: the host is a merged permanent of the two printed cards.
    let merged = runner.state().objects[&host].merged_components.clone();
    assert_eq!(
        merged.len(),
        2,
        "host and Gemrazer are one merged permanent"
    );
    assert!(
        merged.contains(&gemrazer),
        "Gemrazer is a component: {merged:?}"
    );

    // The runner advances through P0's turn to P1's precombat main phase.
    for _ in 0..8 {
        if runner.state().active_player == P1 && runner.state().phase == Phase::PreCombatMain {
            break;
        }
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ) {
            // P0 attacks with nothing.
            runner
                .declare_attackers(&[])
                .expect("an empty attack declaration is accepted");
        } else if runner.state().phase == Phase::PreCombatMain {
            runner.advance_to_end_step();
        } else {
            runner.advance_to_phase(Phase::PreCombatMain);
        }
    }
    assert_eq!(runner.state().active_player, P1, "reach-guard: P1's turn");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P1
    ));

    // P1 casts Strategic Betrayal at P0.
    let outcome = runner.cast(spell).target_player(P0).resolve();
    match outcome.final_waiting_for() {
        WaitingFor::EffectZoneChoice { player, cards, .. } => {
            assert_eq!(*player, P0, "the opponent chooses the exiled creature");
            assert!(cards.contains(&host) && cards.contains(&other), "{cards:?}");
        }
        other => panic!("expected the opponent's creature choice; got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards { cards: vec![host] })
        .expect("the opponent selects the merged creature");
    drive_to_priority(&mut runner);

    for component in [host, gemrazer] {
        assert_eq!(zone(&runner, component), Zone::Exile);
        assert_eq!(
            exiled_by(&runner, component),
            Some(P0),
            "each printed component of the merged creature records the opponent"
        );
    }
    assert_eq!(zone(&runner, other), Zone::Battlefield);
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
/// Revert sensitivity: crediting the player whose library the filter reads
/// records `Some(P1)` and fails; base behavior (`Some(P0)`) passes.
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
/// Revert sensitivity: this row preserves an already-correct behavior: the
/// fan-out rebinds the controller to each seat, and the default actor is that
/// controller.
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
/// Revert sensitivity: crediting the player whose creatures the filter sweeps
/// records `Some(P1)` for the twin.
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
/// hand, then that player exiles a card from their hand." "That player" is
/// the permanent's owner; here the owner also controls it (the
/// owner-and-controller-apart row follows). Unlike R8's subjectless sweep the
/// pick has a subject, so that player performs the exile.
///
/// Revert sensitivity: records `Some(P0)` against the base, where the
/// controller performed every move.
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

/// CR 608.2c + CR 400.3: Ashiok, Nightmare Muse -3 on a permanent P1 owns and
/// P2 controls. The permanent returns to P1's hand, "that player" is P1, and
/// the exiled card comes from P1's hand and is recorded as exiled by P1. P2,
/// who merely controlled the permanent, keeps the card in their hand.
///
/// Revert sensitivity: with "that player" read as the permanent's controller,
/// the pick is made from P2's hand and credited to P2.
#[test]
fn ashiok_minus_three_that_player_is_the_owner_not_the_controller() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario
        .add_creature(P1, "P1-owned Bear", 2, 2)
        .controlled_by(P2)
        .id();
    let owner_card = scenario.add_card_to_hand(P1, "P1 Hand Card");
    let controller_card = scenario.add_card_to_hand(P2, "P2 Hand Card");
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

    runner
        .activate(ashiok, index)
        .target_objects(&[bear])
        .resolve();
    let prompts = drive_answering_zone_choices(&mut runner);

    // Reach-guard: the permanent went back to its owner, whose hand then held
    // two cards, so the pick was a real selection performed by P1.
    assert_eq!(runner.state().objects[&bear].owner, P1);
    assert_eq!(prompts.len(), 1, "two cards in P1's hand prompt once");
    assert_eq!(prompts[0].1, Some(P1), "P1 performs the exile");
    let exiled = exiled_among(&runner, &[bear, owner_card, controller_card]);
    assert_eq!(exiled.len(), 1, "exactly one card is exiled: {exiled:?}");
    assert!(
        [bear, owner_card].contains(&exiled[0]),
        "the exiled card came from the owner's hand"
    );
    assert_eq!(exiled_by(&runner, exiled[0]), Some(P1));
    assert_eq!(zone(&runner, controller_card), Zone::Hand);
    assert!(runner.state().players[P2.0 as usize]
        .hand
        .contains(&controller_card));
    assert_eq!(runner.state().players[P1.0 as usize].hand.len(), 1);
}

/// Recoil `{1}{U}{B}` Instant — verbatim Oracle text.
const RECOIL: &str =
    "Return target permanent to its owner's hand. Then that player discards a card.";

/// CR 608.2c + CR 400.3: the same antecedent on a discard (Recoil; also
/// Compelling Deterrence and Dinrova Horror). The permanent P1 owns and P2
/// controls returns to P1's hand and P1 discards; P2's hand is untouched.
///
/// Revert sensitivity: with "that player" read as the permanent's controller,
/// P2 discards their only card and P1 keeps two.
#[test]
fn recoil_that_player_is_the_owner_not_the_controller() {
    assert_parses_clean("Recoil", &[], &["Instant"], RECOIL);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario
        .add_creature(P1, "P1-owned Bear", 2, 2)
        .controlled_by(P2)
        .id();
    scenario.add_card_to_hand(P1, "P1 Hand Card");
    let controller_card = scenario.add_card_to_hand(P2, "P2 Hand Card");
    let recoil = scenario
        .add_spell_to_hand_from_oracle(P0, "Recoil", true, RECOIL)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner.cast(recoil).target_objects(&[bear]).resolve();
    let mut discarders = Vec::new();
    for _ in 0..60 {
        if let WaitingFor::DiscardChoice {
            player,
            cards,
            count,
            ..
        } = runner.state().waiting_for.clone()
        {
            discarders.push(player);
            runner
                .act(GameAction::SelectCards {
                    cards: cards.into_iter().take(count).collect(),
                })
                .expect("the discard choice is accepted");
            continue;
        }
        if drive_step(&mut runner) {
            break;
        }
    }

    // Reach-guard: the permanent returned to its owner, who then chose a
    // discard from two cards.
    assert_eq!(discarders, vec![P1], "the owner discards");
    assert_eq!(runner.state().players[P1.0 as usize].hand.len(), 1);
    assert_eq!(runner.state().players[P1.0 as usize].graveyard.len(), 1);
    assert_eq!(zone(&runner, controller_card), Zone::Hand);
    assert!(runner.state().players[P2.0 as usize].graveyard.is_empty());
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
    let (runner, lands, relic, mudhole, _) =
        mudhole_replacement_fixture_with_response(continuation_witness, None);
    (runner, lands, relic, mudhole)
}

fn mudhole_replacement_fixture_with_response(
    continuation_witness: bool,
    response_controller: Option<PlayerId>,
) -> (
    GameRunner,
    [ObjectId; 2],
    ObjectId,
    ObjectId,
    Option<ObjectId>,
) {
    assert_parses_clean("Mudhole", &[], &["Instant"], MUDHOLE);

    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        [ManaType::Colorless, ManaType::Colorless, ManaType::Red]
            .into_iter()
            .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
            .collect(),
    );
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
                        actor: engine::types::ability::LibraryInstructionActor::Controller,
                        library_shuffle: Default::default(),
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
            generic: 2,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    let response = response_controller.map(|player| {
        assert_parses_clean("Aethersnatch", &[], &["Instant"], AETHERSNATCH);
        scenario.with_mana_pool(
            player,
            (0..4)
                .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
                .chain((0..2).map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![])))
                .collect(),
        );
        scenario
            .add_spell_to_hand_from_oracle(player, "Aethersnatch", true, AETHERSNATCH)
            .with_mana_cost(ManaCost::Cost {
                generic: 4,
                shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            })
            .id()
    });

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
    (runner, [land_a, land_b], relic, mudhole, response)
}

/// Both spells follow their real public casting/payment paths. Only the
/// replacement and the already-documented completion witness are synthetic.
fn aethersnatch_mudhole_replacement_pause() -> (GameRunner, [ObjectId; 2], ObjectId, ObjectId) {
    let (mut runner, lands, relic, mudhole, response) =
        mudhole_replacement_fixture_with_response(true, Some(P2));
    runner.cast(mudhole).target_player(P1).commit();
    assert!(runner.state().players[P0.0 as usize].mana_pool.is_empty());
    for _ in 0..3 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P2 }
        ) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to Aethersnatch's caster");
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P2 }
    ));
    let theft = runner
        .cast(response.expect("the fixture includes Aethersnatch"))
        .target_object(mudhole)
        .commit()
        .stop_at_optional()
        .try_resolve()
        .expect("resolve Aethersnatch up to its real new-target offer");
    assert!(matches!(
        theft.final_waiting_for(),
        WaitingFor::OptionalEffectChoice { player: P2, .. }
    ));
    assert!(runner.state().players[P2.0 as usize].mana_pool.is_empty());
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("decline choosing new targets for the stolen Mudhole");
    assert_eq!(runner.state().objects[&mudhole].controller, P2);
    assert_eq!(zone(&runner, mudhole), Zone::Stack);
    assert_eq!(runner.state().stack.len(), 1);
    for _ in 0..3 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("resolve the stolen Mudhole");
    }
    let pending = runner
        .state()
        .active_change_zone_frame()
        .and_then(|frame| frame.pending.as_ref())
        .expect("the actual stolen spell parks its mass-exile family");
    assert_eq!(pending.controller, P2);
    assert_eq!(pending.performed_by, P1);
    let continuation = runner
        .state()
        .resolution_stack
        .iter()
        .find_map(|frame| match frame {
            ResolutionFrame::AbilityContinuation(frame) => Some(&frame.pending),
            _ => None,
        })
        .expect("the actual resolution retains its completion witness");
    assert_eq!(continuation.chain.controller, P2);
    let retained = runner
        .state()
        .resolving_stack_entry
        .as_ref()
        .expect("the resolving stack carrier is retained");
    assert_eq!(retained.controller, P0, "announcement is not resolution");
    assert_eq!(
        retained
            .ability()
            .expect("Mudhole's baked ability")
            .controller,
        P0,
        "both retained fields predate the live-controller rebind"
    );
    (runner, lands, relic, mudhole)
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
        serde_json::to_value(PersistedGameState::Raw(Box::new(runner.state().clone()))),
        serde_json::to_value(PersistedGameState::capture(runner.state().clone())),
        serde_json::to_value(runner.state()),
    ];
    for snapshot in snapshots {
        let bytes = snapshot.expect("the public saved-game writer succeeds");
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

fn wire_state_mut(wire: &mut serde_json::Value) -> &mut serde_json::Value {
    if wire.get("state").is_some() {
        wire.get_mut("state").expect("the saved-state envelope")
    } else {
        wire
    }
}

fn restore_public_saved_runner(wire: serde_json::Value) -> GameRunner {
    let state = serde_json::from_value::<PersistedGameState>(wire)
        .expect("the public reader accepts the saved state")
        .prepare_for_restore(PersistedRestoreFinalization::Immediate)
        .expect("the saved frame is valid for restoration")
        .finalize_immediately()
        .expect("the public saved game restores");
    GameRunner::from_state(state)
}

fn assert_restored_subject_exile_roles(runner: &GameRunner, controller: PlayerId) {
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice {
            player: P1,
            kind: ReplacementChoiceKind::OptionalBranch,
            ..
        }
    ));
    let pending = runner
        .state()
        .active_change_zone_frame()
        .and_then(|frame| frame.pending.as_ref())
        .expect("the restored mass move retains its family");
    assert_eq!(pending.performed_by, P1, "preserve the recorded actor");
    assert_eq!(
        pending.controller, controller,
        "preserve the actual resolving family's controller"
    );
    assert_eq!(
        runner
            .state()
            .pending_replacement
            .as_ref()
            .expect("the actual replacement remains pending")
            .exile_controller,
        Some(controller),
        "the exact paused member retains its independent duration/link anchor"
    );
}

#[test]
fn aethersnatch_mudhole_current_restore_preserves_actual_controller_and_subject() {
    let (runner, lands, relic, mudhole) = aethersnatch_mudhole_replacement_pause();
    assert_restored_subject_exile_roles(&runner, P2);
    let pending = runner
        .state()
        .active_change_zone_frame()
        .and_then(|frame| frame.pending.as_ref())
        .unwrap();
    let group = pending.logical_zone_change_group.logical_group_id;
    let duration = pending.duration.clone();
    let snapshots = [
        serde_json::to_value(PersistedGameState::Raw(Box::new(runner.state().clone()))),
        serde_json::to_value(PersistedGameState::capture(runner.state().clone())),
        serde_json::to_value(runner.state()),
    ];
    for snapshot in snapshots {
        let mut restored =
            restore_public_saved_runner(snapshot.expect("serialize the actual stolen-spell pause"));
        assert_restored_subject_exile_roles(&restored, P2);
        let pending = restored
            .state()
            .active_change_zone_frame()
            .and_then(|frame| frame.pending.as_ref())
            .unwrap();
        assert_eq!(pending.logical_zone_change_group.logical_group_id, group);
        assert_eq!(pending.duration, duration);
        assert_eq!(
            restored
                .state()
                .pending_replacement
                .as_ref()
                .unwrap()
                .exile_duration,
            duration,
            "restoring the anchor never changes the captured duration"
        );
        for _ in lands {
            assert!(matches!(
                restored.state().waiting_for,
                WaitingFor::ReplacementChoice { player: P1, .. }
            ));
            restored
                .act(GameAction::ChooseReplacement { index: 1 })
                .expect("the original target declines each actual replacement");
        }
        drive_to_priority(&mut restored);
        assert_resolution_finished(&restored);
        for land in lands {
            assert_eq!(zone(&restored, land), Zone::Exile);
            assert_eq!(exiled_by(&restored, land), Some(P1));
        }
        assert_eq!(zone(&restored, relic), Zone::Graveyard);
        assert_eq!(zone(&restored, mudhole), Zone::Graveyard);
        assert_eq!(
            restored.life(P2),
            23,
            "the saved completion runs exactly once"
        );
        assert_pridemate_counters(&restored, 1);
    }
}

#[test]
fn deployed_v11_aethersnatch_mudhole_restore_preserves_recorded_roles_and_anchor() {
    for (format, bytes) in DEPLOYED_V11_EXILE_SAVES {
        let encoded: serde_json::Value =
            serde_json::from_slice(bytes).expect("the captured old writer emitted JSON");
        let saved = encoded.get("state").unwrap_or(&encoded);
        assert_eq!(saved["resolution_state_version"], 11);
        let recorded = &saved["resolution_frames"]["frames"]
            .as_array()
            .unwrap()
            .iter()
            .find(|frame| frame["type"] == "ChangeZone")
            .expect("the actual capture paused Mudhole's mass move")["data"]["pending"];
        assert_eq!(recorded["controller"], serde_json::json!(P2));
        assert!(recorded.get("performed_by").is_none());
        assert_eq!(
            saved["pending_replacement"]["exile_controller"],
            serde_json::json!(P2)
        );
        let recorded_duration = recorded
            .get("duration")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        // The primary compatibility proof decodes the original bytes directly:
        // no version stamp, controller, event or continuation is rewritten.
        let state = serde_json::from_slice::<PersistedGameState>(bytes)
            .unwrap_or_else(|error| panic!("deployed v11 {format}: {error}"))
            .prepare_for_restore(PersistedRestoreFinalization::Immediate)
            .expect("the deployed save prepares for restore")
            .finalize_immediately()
            .expect("the deployed save finalizes");
        let mut restored = GameRunner::from_state(state);
        assert!(matches!(
            restored.state().waiting_for,
            WaitingFor::ReplacementChoice {
                player: P1,
                kind: ReplacementChoiceKind::OptionalBranch,
                ..
            }
        ));
        let pending = restored
            .state()
            .active_change_zone_frame()
            .and_then(|frame| frame.pending.as_ref())
            .expect("restore retains the recorded family");
        assert_eq!(
            serde_json::to_value(pending.controller).unwrap(),
            recorded["controller"]
        );
        assert_eq!(
            serde_json::to_value(pending.performed_by).unwrap(),
            recorded["controller"],
            "legacy admission copies the old recorded performer, not today's subject"
        );
        assert_eq!(
            serde_json::to_value(&pending.duration).unwrap(),
            recorded_duration
        );
        assert_eq!(
            serde_json::to_value(pending.logical_zone_change_group.logical_group_id).unwrap(),
            recorded["logical_zone_change_group"]["logical_group_id"]
        );
        let mudhole = pending.source_id;
        let lands: Vec<_> = std::iter::once(
            pending
                .paused_current
                .as_ref()
                .expect("the first member is paused")
                .member
                .object_id,
        )
        .chain(pending.remaining.iter().map(|member| member.object_id))
        .collect();
        assert_eq!(lands.len(), 2);
        assert_eq!(restored.state().objects[&mudhole].name, "Mudhole");
        let replacement = restored.state().pending_replacement.as_ref().unwrap();
        assert_eq!(
            serde_json::to_value(replacement.exile_controller).unwrap(),
            saved["pending_replacement"]["exile_controller"]
        );
        assert_eq!(
            serde_json::to_value(&replacement.exile_duration).unwrap(),
            saved["pending_replacement"]["exile_duration"]
        );
        assert_eq!(
            restored
                .state()
                .resolving_stack_entry
                .as_ref()
                .unwrap()
                .controller,
            P0,
            "the stale announcement entry never replaces the recorded P2 family"
        );
        for _ in &lands {
            assert!(matches!(
                restored.state().waiting_for,
                WaitingFor::ReplacementChoice { player: P1, .. }
            ));
            restored
                .act(GameAction::ChooseReplacement { index: 1 })
                .expect("the actual surviving target declines the saved replacement");
        }
        drive_to_priority(&mut restored);
        assert_resolution_finished(&restored);
        for land in lands {
            assert_eq!(zone(&restored, land), Zone::Exile);
            assert_eq!(
                exiled_by(&restored, land),
                Some(P2),
                "deployed v11 attribution remains P2, unlike current v12's subject P1"
            );
        }
        assert_eq!(restored.life(P2), 23, "the old saved witness finishes once");
        assert_eq!(zone(&restored, mudhole), Zone::Graveyard);
    }
}

/// The continuation's journal cause is independently bound to its owning node.
/// This corrupts one recorded identity, not a missing migration-era marker or a
/// replacement effect's unrelated authorship.
#[test]
fn deployed_v11_subject_exile_restore_refuses_contradictory_recorded_continuation_cause() {
    for (_, bytes) in DEPLOYED_V11_EXILE_SAVES {
        let mut wire: serde_json::Value =
            serde_json::from_slice(bytes).expect("parse the untouched deployed bytes");
        let restored = restore_public_saved_runner(wire.clone());
        let pending = restored
            .state()
            .active_change_zone_frame()
            .and_then(|frame| frame.pending.as_ref())
            .expect("the unmodified fixture retains its family");
        assert_eq!(pending.controller, P2);
        assert_eq!(pending.performed_by, P2);
        let mudhole = pending.source_id;
        let entries = wire_state_mut(&mut wire)["resolved_rules_journal"]["entries"]
            .as_array_mut()
            .expect("the actual writer retains its rules journal");
        let index = entries
            .iter()
            .position(|entry| {
                let frame = &entry["command"]["FrameTransition"]["transition"]
                    ["InsertParentOfActive"]["frame"];
                frame["type"] == "AbilityContinuation"
                    && frame["data"]["pending"]["chain"]["source_id"] == serde_json::json!(mudhole)
            })
            .expect("the actual paused continuation has its journaled insertion");
        let node = entries[index]["node"].clone();
        assert_eq!(entries[index]["command"]["FrameTransition"]["cause"], node);
        let unrelated_node = entries
            .iter()
            .find(|entry| entry["node"] != node)
            .expect("the real cast and payment trace recorded other causal nodes")["node"]
            .clone();
        entries[index]["command"]["FrameTransition"]["cause"] = unrelated_node;
        let error = serde_json::from_value::<PersistedGameState>(wire)
            .expect_err("the recorded continuation cause cannot contradict its owning journal node")
            .to_string();
        assert!(
            error.contains("frame-transition command has an unrelated cause"),
            "{error}"
        );
    }
}

#[test]
fn subject_exile_restore_keeps_explicit_actor_and_current_missing_actor_is_strict() {
    let (mut runner, _, _, mudhole) = mudhole_replacement_fixture(true);
    runner.cast(mudhole).target_player(P1).resolve();
    let (transferred, _, _, _) = aethersnatch_mudhole_replacement_pause();
    for (runner, controller) in [(runner, P0), (transferred, P2)] {
        for version in [11, 12] {
            let mut wire =
                serde_json::to_value(PersistedGameState::Raw(Box::new(runner.state().clone())))
                    .expect("serialize the current explicit actor");
            let state = wire_state_mut(&mut wire);
            state["resolution_state_version"] = serde_json::json!(version);
            let restored = restore_public_saved_runner(wire.clone());
            assert_restored_subject_exile_roles(&restored, controller);
            if version == 12 {
                let state = wire_state_mut(&mut wire);
                state["resolution_frames"]["frames"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|frame| frame["type"] == "ChangeZone")
                    .unwrap()["data"]["pending"]
                    .as_object_mut()
                    .unwrap()
                    .remove("performed_by");
                let error = serde_json::from_value::<PersistedGameState>(wire)
                    .expect_err("current saves must never migrate a missing actor")
                    .to_string();
                assert!(error.contains("performed_by"), "{error}");
            }
        }
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

// ---------------------------------------------------------------------------
// Actor reference kinds. Each row runs a printed card whose subject the parser
// binds as a different player reference, and where a choice prompt exists it
// runs both the single-candidate shortcut and the interactive selection, which
// must credit the same performer (CR 608.2c). Revert sensitivity: crediting
// `ability.controller` instead of the parsed actor (the pre-fix performer)
// records P0 on every row.
// ---------------------------------------------------------------------------

/// The P1 cards in `candidates` that ended in exile.
fn exiled_among(runner: &GameRunner, candidates: &[ObjectId]) -> Vec<ObjectId> {
    candidates
        .iter()
        .copied()
        .filter(|card| zone(runner, *card) == Zone::Exile)
        .collect()
}

fn assert_routes_credit(
    label: &str,
    routes: Vec<(GameRunner, Vec<ObjectId>, ZoneChoicePrompts)>,
    performer: PlayerId,
) {
    let mut prompted = false;
    for (runner, candidates, prompts) in routes {
        let exiled = exiled_among(&runner, &candidates);
        assert!(
            !exiled.is_empty(),
            "{label}: a candidate is exiled; prompts {prompts:?}"
        );
        for card in exiled {
            assert_eq!(
                exiled_by(&runner, card),
                Some(performer),
                "{label}: the instruction's subject is the exiling player; prompts {prompts:?}"
            );
        }
        for (_, carried) in &prompts {
            assert_eq!(
                *carried,
                Some(performer),
                "{label}: the selection prompt carries the same performer"
            );
        }
        // Recorded for the separate CR 608.2d chooser question; not asserted here.
        println!("{label}: (chooser, performer) per prompt: {prompts:?}");
        prompted |= !prompts.is_empty();
    }
    assert!(prompted, "{label}: one route reached the selection prompt");
}

/// `TriggeringPlayer`: "Whenever this creature deals combat damage to a player,
/// that player exiles a card from their hand." One held card takes the
/// single-candidate shortcut; two reach the selection prompt.
#[test]
fn triggering_player_subject_credits_the_damaged_player_on_both_routes() {
    assert_parses_clean(
        "Fiend of the Shadows",
        &["Flying", "Regenerate"],
        &["Creature"],
        FIEND_OF_THE_SHADOWS,
    );
    let route = |held: usize| {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let cards: Vec<ObjectId> = (0..held)
            .map(|i| scenario.add_card_to_hand(P1, &format!("P1 Held Card {i}")))
            .collect();
        let fiend = scenario
            .add_creature_from_oracle(P0, "Fiend of the Shadows", 3, 3, FIEND_OF_THE_SHADOWS)
            .id();
        let mut runner = scenario.build();
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(fiend, AttackTarget::Player(P1))])
            .expect("Fiend of the Shadows attacks");
        runner.combat_damage();
        let prompts = drive_answering_zone_choices(&mut runner);
        (runner, cards, prompts)
    };
    assert_routes_credit("Fiend of the Shadows", vec![route(1), route(2)], P1);
}

/// `DefendingPlayer`: "Whenever this creature attacks, defending player exiles
/// two permanents they control." One permanent takes the shortcut; three reach
/// the selection prompt, whose chooser this change leaves as it was. The row
/// does not depend on how many are exiled: today's parse drops "two" (the
/// card's known count misparse), and the row holds either way.
#[test]
fn defending_player_subject_credits_the_defender_on_both_routes() {
    assert_parses_clean("Bane of Bala Ged", &[], &["Creature"], BANE_OF_BALA_GED);
    let route = |permanents: usize| {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let cards: Vec<ObjectId> = (0..permanents)
            .map(|i| {
                scenario
                    .add_creature(P1, &format!("P1 Permanent {i}"), 1, 1)
                    .id()
            })
            .collect();
        let bane = scenario
            .add_creature_from_oracle(P0, "Bane of Bala Ged", 7, 5, BANE_OF_BALA_GED)
            .id();
        let mut runner = scenario.build();
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(bane, AttackTarget::Player(P1))])
            .expect("Bane of Bala Ged attacks");
        let prompts = drive_answering_zone_choices(&mut runner);
        (runner, cards, prompts)
    };
    assert_routes_credit("Bane of Bala Ged", vec![route(1), route(3)], P1);
}

/// `DeclaredPlayer`: "Target player draws a card, then exiles a card from their
/// hand." The carried subject binds the declared target. With an empty hand
/// the drawn card is the only candidate; with one held card there are two.
#[test]
fn declared_player_subject_credits_the_target_on_both_routes() {
    assert_parses_clean("Arcane Artisan", &[], &["Creature"], ARCANE_ARTISAN);
    let route = |held: usize| {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let mut cards: Vec<ObjectId> = (0..held)
            .map(|i| scenario.add_card_to_hand(P1, &format!("P1 Held Card {i}")))
            .collect();
        scenario.with_library_top(P1, &["P1 Drawn Card"]);
        scenario.with_mana_pool(
            P0,
            (0..2)
                .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
                .chain(std::iter::once(ManaUnit::new(
                    ManaType::Blue,
                    ObjectId(0),
                    false,
                    vec![],
                )))
                .collect(),
        );
        let artisan = scenario
            .add_creature_from_oracle(P0, "Arcane Artisan", 0, 3, ARCANE_ARTISAN)
            .id();
        let mut runner = scenario.build();
        cards.push(runner.state().players[P1.0 as usize].library[0]);
        let index = activated_index(&runner, artisan);
        runner.activate(artisan, index).target_player(P1).resolve();
        let prompts = drive_answering_zone_choices(&mut runner);
        (runner, cards, prompts)
    };
    assert_routes_credit("Arcane Artisan", vec![route(0), route(1)], P1);
}

/// `DeclaredPlayer` reached through the chain's anaphor rewrite: "target
/// opponent exiles a nontoken creature they control, then they exile a nonland
/// card from their graveyard." The second exile's "they" is the announced
/// opponent. One graveyard card takes the shortcut; two reach the selection.
/// Positive witness: the prior `ParentTargetController` actor also resolves to
/// the opponent here; the revert-sensitive control is the parser row
/// `pronoun_subject_binds_its_continuation`.
#[test]
fn declared_player_continuation_credits_the_target_on_both_routes() {
    assert_parses_clean(
        "Azula, Cunning Usurper",
        &["Firebending"],
        &["Creature"],
        AZULA_CUNNING_USURPER,
    );
    let route = |grave: usize| {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let creature = scenario.add_creature(P1, "P1 Bear", 2, 2).id();
        let cards: Vec<ObjectId> = (0..grave)
            .map(|i| {
                scenario
                    .add_spell_to_graveyard(P1, &format!("P1 Grave Spell {i}"), false)
                    .id()
            })
            .collect();
        let azula = scenario
            .add_creature_to_hand_from_oracle(
                P0,
                "Azula, Cunning Usurper",
                4,
                4,
                AZULA_CUNNING_USURPER,
            )
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = scenario.build();
        runner.cast(azula).resolve();
        let prompts = drive_answering_zone_choices(&mut runner);
        // Reach-guard: the first exile ran and credits the opponent too.
        assert_eq!(zone(&runner, creature), Zone::Exile);
        assert_eq!(exiled_by(&runner, creature), Some(P1));
        (runner, cards, prompts)
    };
    assert_routes_credit("Azula, Cunning Usurper", vec![route(1), route(2)], P1);
}

/// `ScopedPlayer`: "At the beginning of each player's upkeep, that player exiles
/// a card at random from their hand." A random pick has no selection prompt.
#[test]
fn scoped_player_subject_credits_the_upkeep_player() {
    // Reach-guard: the exile clause parses with the stamped subject. Elkin
    // Lair's later play-permission and end-step sentences are unsupported
    // today, so the whole-card guard used elsewhere does not apply.
    let parsed = parse_oracle_text(
        ELKIN_LAIR,
        "Elkin Lair",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    let exile = parsed.triggers[0]
        .execute
        .as_deref()
        .expect("the upkeep trigger has a body");
    assert!(
        matches!(
            &*exile.effect,
            Effect::ChangeZone {
                actor: engine::types::ability::LibraryInstructionActor::Player {
                    player: TargetFilter::ScopedPlayer
                },
                destination: Zone::Exile,
                ..
            }
        ),
        "the upkeep trigger opens with the subject's exile; got {:?}",
        exile.effect
    );
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cards: Vec<ObjectId> = (0..2)
        .map(|i| scenario.add_card_to_hand(P1, &format!("P1 Held Card {i}")))
        .collect();
    scenario.add_enchantment_from_oracle(P0, "Elkin Lair", ELKIN_LAIR);
    let mut runner = scenario.build();
    runner.advance_to_upkeep();
    assert_eq!(
        runner.state().active_player,
        P1,
        "reach: P1's upkeep, P0's world enchantment"
    );
    drive_to_priority(&mut runner);
    let exiled = exiled_among(&runner, &cards);
    assert_eq!(exiled.len(), 1, "one of P1's two cards is exiled at random");
    assert_eq!(
        exiled_by(&runner, exiled[0]),
        Some(P1),
        "the upkeep player exiles their own card"
    );
}

/// `Typed { ChosenPlayer }`: "choose a player at random. That player exiles an
/// instant or sorcery card from their graveyard." Both players hold instants,
/// so the exile is the random choice's; the seed picks P1. One instant takes
/// the shortcut; two reach the selection prompt.
#[test]
fn chosen_player_subject_credits_the_chosen_player_on_both_routes() {
    assert_parses_clean("Wildfire Devils", &[], &["Creature"], WILDFIRE_DEVILS);
    let route = |instants: usize| {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let cards: Vec<ObjectId> = (0..instants)
            .map(|i| {
                scenario
                    .add_spell_to_graveyard(P1, &format!("P1 Grave Instant {i}"), true)
                    .id()
            })
            .collect();
        scenario.add_spell_to_graveyard(P0, "P0 Grave Instant", true);
        let devils = scenario
            .add_creature_to_hand_from_oracle(P0, "Wildfire Devils", 4, 2, WILDFIRE_DEVILS)
            .id();
        let mut runner = scenario.build();
        runner.cast(devils).resolve();
        let prompts = drive_answering_zone_choices(&mut runner);
        (runner, cards, prompts)
    };
    assert_routes_credit("Wildfire Devils", vec![route(1), route(2)], P1);
}

// ---------------------------------------------------------------------------
// Dandân — the performer does not decide the receiving hand or the owner.
// ---------------------------------------------------------------------------

/// Run Skullwinder for P0 under Dandân, whose graveyard is one pile both seats
/// read. The enters trigger targets `pile[first]`; the chosen opponent, P1, then
/// returns one of the cards left in the pile. Returns the staged pile and the
/// zone-choice prompts answered as `(chooser, performer)`; a prompt is answered
/// with `pile[pick]`.
fn skullwinder_dandan_run(
    pile: &[(PlayerId, &str)],
    first: usize,
    pick: usize,
) -> (GameRunner, Vec<ObjectId>, ZoneChoicePrompts) {
    assert_parses_clean("Skullwinder", &["Deathtouch"], &["Creature"], SKULLWINDER);
    let mut scenario = GameScenario::new_with_format(FormatConfig::dandan(), 2, 11);
    scenario.at_phase(Phase::PreCombatMain);
    let skullwinder = scenario
        .add_creature_to_hand_from_oracle(P0, "Skullwinder", 1, 3, SKULLWINDER)
        .with_mana_cost(ManaCost::zero())
        .id();
    let cards: Vec<ObjectId> = pile
        .iter()
        .map(|&(owner, name)| scenario.add_spell_to_graveyard(owner, name, true).id())
        .collect();
    let mut runner = scenario.build();
    runner
        .cast(skullwinder)
        .target_objects(&[cards[first]])
        .resolve();
    let mut prompts = Vec::new();
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::NamedChoice { options, .. } => {
                assert_eq!(options, vec![P1.0.to_string()], "P1 is the only opponent");
                runner
                    .act(GameAction::ChooseOption {
                        choice: options[0].clone(),
                    })
                    .expect("P1 is chosen");
            }
            WaitingFor::EffectZoneChoice {
                player,
                performed_by,
                cards: offered,
                ..
            } => {
                prompts.push((player, performed_by));
                assert!(offered.contains(&cards[pick]), "the pick is offered");
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![cards[pick]],
                    })
                    .expect("the zone choice accepts the pick");
            }
            _ => {
                if drive_step(&mut runner) {
                    return (runner, cards, prompts);
                }
            }
        }
    }
    panic!(
        "the Skullwinder run did not settle: {:?}",
        runner.state().waiting_for
    );
}

fn hand_holder(runner: &GameRunner, id: ObjectId) -> Option<PlayerId> {
    runner
        .state()
        .players
        .iter()
        .find(|player| player.hand.contains(&id))
        .map(|player| player.id)
}

/// One card is left for the opponent's return, so it takes the
/// single-candidate shortcut. P1 performs the move (CR 608.2c), but the
/// Dandân receiving hand and owner are read from the entry-ownership input,
/// which stays the resolving controller, as before this change: the P0-owned
/// card goes to P0's hand and stays P0's. Feeding the performer to that input
/// instead sends it to P1's hand, owned by P1.
#[test]
fn skullwinder_dandan_single_candidate_ownership_ignores_the_performer() {
    let (runner, cards, prompts) =
        skullwinder_dandan_run(&[(P1, "P1 Grave Spell"), (P0, "P0 Grave Spell")], 0, 1);
    assert!(prompts.is_empty(), "one candidate takes the shortcut");
    // Reach-guard: the enters trigger's own return resolved.
    assert_eq!(zone(&runner, cards[0]), Zone::Hand);
    assert_eq!(zone(&runner, cards[1]), Zone::Hand, "the opponent's return");
    assert_eq!(hand_holder(&runner, cards[1]), Some(P0));
    assert_eq!(runner.state().objects[&cards[1]].owner, P0);
}

/// Two cards are left, so the return reaches the zone prompt. Its chooser is
/// still the prompt's existing chooser, P0 (who should choose under CR 608.2d
/// is a separate question this change does not decide), while P1 performs the
/// move (CR 608.2c). The interactive
/// resume keeps reading the chooser for Dandân ownership, as before this
/// change, so the P0-owned pick goes to P0's hand and stays P0's. Feeding the
/// performer to that input instead sends it to P1's hand, owned by P1.
#[test]
fn skullwinder_dandan_selected_candidate_ownership_ignores_the_performer() {
    let (runner, cards, prompts) = skullwinder_dandan_run(
        &[
            (P1, "P1 Grave Spell"),
            (P0, "P0 Grave Spell A"),
            (P0, "P0 Grave Spell B"),
        ],
        0,
        1,
    );
    assert_eq!(prompts, vec![(P0, Some(P1))], "P0 chooses, P1 performs");
    assert_eq!(zone(&runner, cards[0]), Zone::Hand);
    assert_eq!(zone(&runner, cards[1]), Zone::Hand, "the opponent's pick");
    assert_eq!(zone(&runner, cards[2]), Zone::Graveyard);
    assert_eq!(hand_holder(&runner, cards[1]), Some(P0));
    assert_eq!(runner.state().objects[&cards[1]].owner, P0);
}

// ---------------------------------------------------------------------------
// The player an earlier clause announced stays the subject across the
// controller's own intervening choice.
// ---------------------------------------------------------------------------

/// Juggernaut Peddler `{W}{B}` Creature — Human Artificer 3/2 — verbatim Oracle
/// text.
const JUGGERNAUT_PEDDLER: &str = "Vigilance\nWhen Juggernaut Peddler enters, target player reveals all nonland cards in their hand. You may choose one of those cards. If you do, that player exiles it and conjures a card named Juggernaut into their hand.";

/// CR 608.2c: "target player reveals … You may choose one of those cards. If
/// you do, that player exiles it": P0 makes the choice, but the exile is the
/// revealing player's (here P1), as for Taster of Wares
/// (`count_slot_fail_closed_runtime`). Revert sensitivity: without the
/// antecedent walk reading past the controller's choice the stamp is absent
/// and P0 is recorded.
#[test]
fn juggernaut_peddler_exile_is_the_revealing_players() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let peddler = scenario
        .add_creature_to_hand_from_oracle(P0, "Juggernaut Peddler", 3, 2, JUGGERNAUT_PEDDLER)
        .with_mana_cost(ManaCost::zero())
        .id();
    let revealed: Vec<ObjectId> = (0..2)
        .map(|i| {
            scenario
                .add_spell_to_hand(P1, &format!("P1 Hand Spell {i}"), false)
                .id()
        })
        .collect();
    let mut runner = scenario.build();
    runner.cast(peddler).target_player(P1).resolve();

    let mut chosen = None;
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![engine::types::ability::TargetRef::Player(P1)],
                    })
                    .expect("P1 is a legal target player");
            }
            WaitingFor::RevealChoice { player, cards, .. } => {
                assert_eq!(
                    player, P0,
                    "the controller chooses among the revealed cards"
                );
                assert!(cards.iter().all(|card| revealed.contains(card)));
                chosen = Some(cards[0]);
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![cards[0]],
                    })
                    .expect("the choice is accepted");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("the controller accepts the optional choice");
            }
            WaitingFor::ChooseFromZoneChoice { cards, .. } => {
                chosen = Some(cards[0]);
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![cards[0]],
                    })
                    .expect("the choice is accepted");
            }
            _ => {
                if drive_step(&mut runner) {
                    break;
                }
            }
        }
    }
    let chosen = chosen.expect("the controller chose a revealed card");
    // Reach-guard: the exile ran on the card the controller chose.
    assert_eq!(zone(&runner, chosen), Zone::Exile);
    assert_eq!(
        exiled_by(&runner, chosen),
        Some(P1),
        "the revealing player exiles the chosen card, not Juggernaut Peddler's controller"
    );
}
