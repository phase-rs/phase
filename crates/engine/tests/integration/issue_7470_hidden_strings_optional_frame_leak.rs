//! Issue #7470 — Hidden Strings leaves its `OptionalEffect` frame on the
//! resolution stack, so the next prompt panics.
//!
//! Oracle (Hidden Strings):
//!   You may tap or untap target permanent, then you may tap or untap another
//!   target permanent.
//!
//! Reported as an engine panic in `prepend_to_pending_continuation`:
//!
//! ```text
//! paused child operation must retain its continuation as an immediate parent:
//! PromptMismatch { frame: OptionalEffect, waiting_for: "ScryChoice" }
//! ```
//!
//! `ResolutionStack::validate` rejects a direct-choice owner at the stack top
//! whose gate does not match the live prompt. Once this spell's `OptionalEffect`
//! frame survives its own resolution, ANY later prompt trips that check — the
//! report reached it by activating Thrasios, Triton Hero (Scry 1), but the scry
//! is incidental. The stale frame is the defect.
//!
//! Evidence from the attached save (turn 2, `waiting_for: Priority`): the
//! resolution stack still held one `OptionalEffect` frame carrying this spell's
//! own ability, tagged `cast_from_zone: "Hand"`, `cast_phase: "PreCombatMain"` —
//! i.e. left over from the ORIGINAL cast, not from the Cipher recast that later
//! ran into it. That recast trigger was stuck mid-resolution and never asked its
//! "you may cast a copy" question, which is why the copy was never offered.
//!
//! This file drives only the original cast: at priority, the resolution stack
//! must be empty.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{ExileLinkKind, WaitingFor};
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const HIDDEN_STRINGS_ORACLE: &str = "You may tap or untap target permanent, then \
     you may tap or untap another target permanent.";

/// Answer whatever the resolution asks until the engine hands back priority.
///
/// Every branch takes the FIRST offered option; which branch is taken does not
/// matter to this test, only that resolution runs to completion.
fn settle(runner: &mut GameRunner, host: engine::types::identifiers::ObjectId) {
    for _ in 0..60 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => return,
            WaitingFor::ChooseOneOfBranch { .. } => {
                if runner.act(GameAction::ChooseBranch { index: 0 }).is_err() {
                    return;
                }
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                if runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .is_err()
                {
                    return;
                }
            }
            WaitingFor::CipherEncodeChoice { .. } => {
                if runner
                    .act(GameAction::CipherEncode {
                        creature: Some(host),
                    })
                    .is_err()
                {
                    return;
                }
            }
            _ => return,
        }
    }
}

#[test]
fn hidden_strings_leaves_no_optional_effect_frame_behind() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for pid in [P0, P1] {
        scenario.with_library_top(pid, &["Lib A", "Lib B", "Lib C", "Lib D"]);
    }

    let first = scenario.add_creature(P0, "First Permanent", 2, 2).id();
    let second = scenario.add_creature(P1, "Second Permanent", 2, 2).id();
    // CR 702.99a: Cipher is load-bearing here. It pauses the spell's own
    // resolution for the encode offer, which is the window the frame survives.
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Hidden Strings", true, HIDDEN_STRINGS_ORACLE)
        .with_keyword(Keyword::Cipher)
        .id();

    let mut runner = scenario.build();
    runner
        .cast(spell)
        .target_objects(&[first, second])
        .resolve();
    settle(&mut runner, first);

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must finish and hand back priority, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().resolution_stack.is_empty(),
        "a finished resolution must leave no frame behind — found {:?}",
        runner
            .state()
            .resolution_stack
            .iter()
            .map(|frame| frame.kind())
            .collect::<Vec<_>>()
    );
}

/// Ground truth for the diagnosis: which prompts does the engine actually ask?
///
/// `SpellCast::resolve()` auto-answers optional prompts (default `Decline`), so
/// a sequence measured through it says nothing about what a PLAYER would see.
/// This drives the cast by hand instead and records every prompt, once with
/// Cipher and once without. The two lists are the measurement.
fn prompt_sequence(with_cipher: bool) -> Vec<String> {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for pid in [P0, P1] {
        scenario.with_library_top(pid, &["Lib A", "Lib B", "Lib C", "Lib D"]);
    }
    let first = scenario.add_creature(P0, "First Permanent", 2, 2).id();
    let second = scenario.add_creature(P1, "Second Permanent", 2, 2).id();
    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Hidden Strings", true, HIDDEN_STRINGS_ORACLE);
    if with_cipher {
        builder.with_keyword(Keyword::Cipher);
    }
    let spell = builder.id();

    let mut runner = scenario.build();
    runner.cast(spell).target_objects(&[first, second]).commit();

    let mut seen = Vec::new();
    for _ in 0..40 {
        let (label, action) = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => (None, GameAction::PassPriority),
            WaitingFor::OptionalEffectChoice { .. } => (
                Some("OptionalEffectChoice"),
                GameAction::DecideOptionalEffect { accept: true },
            ),
            WaitingFor::ChooseOneOfBranch { .. } => (
                Some("ChooseOneOfBranch"),
                GameAction::ChooseBranch { index: 0 },
            ),
            WaitingFor::CipherEncodeChoice { .. } => (
                Some("CipherEncodeChoice"),
                GameAction::CipherEncode {
                    creature: Some(first),
                },
            ),
            other => {
                // Anything the loop is not taught to answer ends the sequence;
                // the variant name alone keeps the row stable across unrelated
                // changes to the prompt payloads.
                seen.push(format!("UNBEKANNT: {}", other.variant_name()));
                break;
            }
        };
        if let Some(label) = label {
            seen.push(label.to_string());
        }
        if runner.act(action).is_err() {
            break;
        }
        if runner.state().stack.is_empty() && seen.iter().any(|s| s == "CipherEncodeChoice") {
            break;
        }
    }
    seen
}

/// Without Cipher the same spell asks all four of its questions, in order.
///
/// This is the control row: it proves the pause machinery is sound, so the
/// failure above cannot be blamed on the optional effect itself. Pinning the
/// exact sequence also makes the Cipher row's silence measurable — with Cipher
/// the player is asked NONE of these.
#[test]
fn without_cipher_the_spell_asks_both_optional_questions() {
    assert_eq!(
        prompt_sequence(false),
        vec![
            "OptionalEffectChoice",
            "ChooseOneOfBranch",
            "OptionalEffectChoice",
            "ChooseOneOfBranch",
            "UNBEKANNT: DeclareAttackers",
        ],
        "the un-ciphered spell must ask both \"you may\" questions and both tap/untap branches"
    );
}

/// The behavioural fix: with Cipher the player is asked the SAME questions, in
/// the same order, and the encode offer comes last.
///
/// CR 702.99a — "then you may exile this card encoded on a creature you
/// control" is the spell's final instruction, so it must follow the tap/untap
/// choices rather than replace them. Before the fix this row saw only
/// `CipherEncodeChoice`: the offer overwrote the live prompt and stranded its
/// frame, which is what made every later prompt panic.
#[test]
fn with_cipher_the_encode_offer_comes_after_the_spells_own_questions() {
    assert_eq!(
        prompt_sequence(true),
        vec![
            "OptionalEffectChoice",
            "ChooseOneOfBranch",
            "OptionalEffectChoice",
            "ChooseOneOfBranch",
            "CipherEncodeChoice",
        ],
        "the encode offer must come last, after both \"you may\" questions"
    );
}

/// Second card shape in the class: the pause need not be a "you may".
///
/// Mental Vapors ("Target player discards a card.") pauses on a DiscardChoice,
/// which owns the prompt exactly like the OptionalEffect frame above. Measuring
/// a second, structurally different pause is what turns "Hidden Strings works
/// now" into a statement about the class — the fix is keyed to prompt
/// ownership, not to any card's text.
fn discard_shaped_prompt_sequence(with_cipher: bool) -> Vec<String> {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for pid in [P0, P1] {
        scenario.with_library_top(pid, &["Lib A", "Lib B", "Lib C", "Lib D"]);
    }
    let host = scenario.add_creature(P0, "Host Creature", 2, 2).id();
    scenario.add_card_to_hand(P1, "Victim Card A");
    scenario.add_card_to_hand(P1, "Victim Card B");
    let mut builder = scenario.add_spell_to_hand_from_oracle(
        P0,
        "Mental Vapors",
        true,
        "Target player discards a card.",
    );
    if with_cipher {
        builder.with_keyword(Keyword::Cipher);
    }
    let spell = builder.id();

    let mut runner = scenario.build();
    runner.cast(spell).target_player(P1).commit();

    let mut seen = Vec::new();
    for _ in 0..40 {
        let (label, action) = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => (None, GameAction::PassPriority),
            WaitingFor::DiscardChoice { cards, .. } => (
                Some("DiscardChoice"),
                GameAction::SelectCards {
                    cards: vec![cards[0]],
                },
            ),
            WaitingFor::CipherEncodeChoice { .. } => (
                Some("CipherEncodeChoice"),
                GameAction::CipherEncode {
                    creature: Some(host),
                },
            ),
            other => {
                seen.push(format!("UNBEKANNT: {}", other.variant_name()));
                break;
            }
        };
        if let Some(label) = label {
            seen.push(label.to_string());
        }
        if runner.act(action).is_err() {
            break;
        }
        if seen.iter().any(|s| s == "CipherEncodeChoice") {
            break;
        }
    }
    seen
}

#[test]
fn a_discard_pause_also_keeps_the_encode_offer_last() {
    let without = discard_shaped_prompt_sequence(false);
    assert!(
        without.contains(&"DiscardChoice".to_string()),
        "control: without Cipher the discard is asked — {without:?}"
    );
    let with = discard_shaped_prompt_sequence(true);
    assert_eq!(
        with.iter().take(2).collect::<Vec<_>>(),
        vec!["DiscardChoice", "CipherEncodeChoice"],
        "the discard must be asked before the encode offer — {with:?}"
    );
}

/// Declining must consume the offer's frame exactly as accepting does.
///
/// `CipherEncode { creature: None }` ends the offer without encoding anything
/// (CR 608.2n: the card goes to its owner's graveyard). It reaches the same
/// handler as an acceptance, so it must leave the same empty stack behind — an
/// unconsumed owner is precisely the #7470 corruption, and a decline that
/// skipped the consumption would rebuild it from the other side. The offer is
/// declined here AFTER the spell's own prompts, i.e. from the parked-then-armed
/// path rather than the immediately-armed one.
#[test]
fn declining_the_encode_offer_consumes_its_frame_too() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for pid in [P0, P1] {
        scenario.with_library_top(pid, &["Lib A", "Lib B", "Lib C", "Lib D"]);
    }
    let first = scenario.add_creature(P0, "First Permanent", 2, 2).id();
    let second = scenario.add_creature(P1, "Second Permanent", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Hidden Strings", true, HIDDEN_STRINGS_ORACLE)
        .with_keyword(Keyword::Cipher)
        .id();

    let mut runner = scenario.build();
    runner.cast(spell).target_objects(&[first, second]).commit();

    let mut declined = false;
    for _ in 0..40 {
        match &runner.state().waiting_for {
            // The spell still has to be let through before it resolves; once
            // the offer has been declined, priority is the resting state this
            // test is measuring.
            WaitingFor::Priority { .. } => {
                if declined || runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("the spell's own optional must be answerable");
            }
            WaitingFor::ChooseOneOfBranch { .. } => {
                runner
                    .act(GameAction::ChooseBranch { index: 0 })
                    .expect("the tap/untap branch must be answerable");
            }
            WaitingFor::CipherEncodeChoice { .. } => {
                runner
                    .act(GameAction::CipherEncode { creature: None })
                    .expect("declining the encode offer must be a legal answer");
                declined = true;
            }
            other => panic!("unexpected prompt {:?}", other.variant_name()),
        }
    }

    assert!(declined, "the encode offer must have been reached at all");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "a declined offer must hand back priority, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().resolution_stack.is_empty(),
        "a declined offer must consume its own frame — found {:?}",
        runner
            .state()
            .resolution_stack
            .iter()
            .map(|frame| frame.kind())
            .collect::<Vec<_>>()
    );
    // CR 608.2n: the declined card is in its owner's graveyard, not stranded
    // off the stack — the outcome the dropped-offer path could not produce.
    assert!(
        runner.state().players[0]
            .graveyard
            .iter()
            .filter_map(|id| runner.state().objects.get(id))
            .any(|object| object.name == "Hidden Strings"),
        "the declined cipher card belongs in its owner's graveyard"
    );
}

/// The stack shape the #7496 review named: a paused post-replacement/draw pair.
///
/// Zur's Weirding replaces Last Thoughts' own draw, so the spell's resolution
/// rests on that replacement's "may pay 2 life" offer with the
/// `PostReplacement` → `MultiDraw` pair parked beneath it. `validate` requires
/// that pair to stay immediately adjacent (CR 614.11a + CR 121.6b), so parking
/// the encode offer "below the top" would land INSIDE it and be rejected.
///
/// This is the production pipeline, not a hand-built stack: a real cipher spell
/// (Last Thoughts, "Draw a card.") whose real draw is really replaced. What it
/// pins is that the offer survives such a shape at all — the earlier revision
/// dropped it there, leaving the card with neither an encode prompt nor its
/// ordinary graveyard route.
#[test]
fn the_encode_offer_survives_a_paused_post_replacement_draw_pair() {
    const ZURS_WEIRDING_ORACLE: &str = "If a player would draw a card, they reveal it instead. \
         Then any other player may pay 2 life. If a player does, put that card into its owner's \
         graveyard. Otherwise, that player draws a card.";

    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for pid in [P0, P1] {
        scenario.with_library_top(pid, &["Lib A", "Lib B", "Lib C", "Lib D"]);
    }
    // P1 controls the replacement, so P1 is the "any other player" who is asked
    // to pay while P0's own draw is what pauses.
    scenario
        .add_creature_from_oracle(P1, "Zur's Weirding", 0, 1, ZURS_WEIRDING_ORACLE)
        .as_enchantment();
    let host = scenario.add_creature(P0, "Host Creature", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Last Thoughts", false, "Draw a card.")
        .with_keyword(Keyword::Cipher)
        .id();

    let mut runner = scenario.build();
    runner.cast(spell).commit();

    let mut seen = Vec::new();
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            // Let the spell through to resolution; after the offer is answered
            // priority is the resting state.
            WaitingFor::Priority { .. } => {
                if seen.iter().any(|prompt| prompt == "CipherEncodeChoice")
                    || runner.act(GameAction::PassPriority).is_err()
                {
                    break;
                }
            }
            WaitingFor::OpponentMayChoice { .. } => {
                seen.push("OpponentMayChoice".to_string());
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("the draw replacement offer must be answerable");
            }
            WaitingFor::CipherEncodeChoice { .. } => {
                seen.push("CipherEncodeChoice".to_string());
                runner
                    .act(GameAction::CipherEncode {
                        creature: Some(host),
                    })
                    .expect("accepting the encode offer must be a legal answer");
                break;
            }
            other => panic!(
                "unexpected prompt {:?}; prompts={seen:?}",
                other.variant_name()
            ),
        }
    }

    assert!(
        seen.contains(&"OpponentMayChoice".to_string()),
        "the draw replacement must actually pause this resolution — otherwise this \
         test does not stand on the stack shape it claims; prompts={seen:?}"
    );
    assert!(
        seen.contains(&"CipherEncodeChoice".to_string()),
        "the encode offer must survive the paused draw pair and still be asked; \
         prompts={seen:?}"
    );
    assert!(
        runner.state().resolution_stack.is_empty(),
        "the answered offer must leave no frame behind — found {:?}",
        runner
            .state()
            .resolution_stack
            .iter()
            .map(|frame| frame.kind())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Exile,
        "accepting the offer must exile the cipher card"
    );
    assert!(
        runner.state().exile_links.iter().any(|link| {
            link.exiled_id == spell && link.source_id == host && link.kind == ExileLinkKind::Cipher
        }),
        "accepting the offer must encode Last Thoughts on the selected host"
    );
}
