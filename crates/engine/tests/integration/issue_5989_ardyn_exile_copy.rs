//! Regression for GitHub issue #5989 — Ardyn, the Usurper's combat-trigger
//! ability that exiles a graveyard creature card and creates a copy of it as
//! a 5/5 black Demon.
//!
//! Oracle (Final Fantasy, verified via Scryfall — exact current wording):
//!   "Demons you control have menace, lifelink, and haste.
//!    Starscourge — At the beginning of combat on your turn, exile up to one
//!    target creature card from a graveyard. If you exiled a card this way,
//!    create a token that's a copy of that card, except it's a 5/5 black
//!    Demon."
//!
//! Root cause: `strip_if_you_do_conditional` (the reflexive "you <verb> this
//! way" gate recognizer) had no active-voice exile arm at all, and the card's
//! actual reflexive clause is the active-PAST "If you exiled a card this
//! way," — an "if "-prefixed form the passive article-first combinator can't
//! consume either. The whole "If you exiled a card this way, create a
//! token…" clause fell through to `Effect::Unimplemented`, so the ability
//! exiled the target and then silently did nothing — the reported "I have to
//! make the copy myself" symptom. Fixed by `parse_you_exile_this_way_clause`
//! (`parser/oracle_nom/condition.rs`), which accepts both tenses
//! ("exile"/"exiled"), wired into `strip_if_you_do_conditional` under BOTH
//! the "if " and "when " prefixes (like the other hoisted active arms).
//!
//! This test drives the PRINTED path end to end: the real begin-combat
//! trigger (not a hand-written activated approximation) parsed from Ardyn's
//! full, exact Oracle text — turn advances into BeginCombat, the trigger
//! fires, its "up to one target" slot pauses for selection between two legal
//! candidates, the exile resolves mandatorily (no "you may" in the printed
//! text), and the reflexive copy must follow automatically.
//!
//! Discriminating scenario: TWO legal graveyard targets so target selection
//! genuinely pauses instead of auto-resolving a lone candidate (the #4948
//! single-legal-target auto-resolve lesson), one in EACH player's graveyard
//! — per the issue's own "only works for my own graveyard" claim, which this
//! test disproves: the parsed target filter carries no controller
//! restriction.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Ardyn's full, exact current Oracle text (Scryfall), including the Demon
/// keyword-grant static and the `Starscourge —` ability word — the test must
/// prove the printed card, not a paraphrase.
const ARDYN_ORACLE: &str = "Demons you control have menace, lifelink, and haste.\n\
    Starscourge — At the beginning of combat on your turn, exile up to one target \
    creature card from a graveyard. If you exiled a card this way, create a token \
    that's a copy of that card, except it's a 5/5 black Demon.";

fn find_new_token(runner: &GameRunner, known: &[ObjectId]) -> Option<ObjectId> {
    runner.state().battlefield.iter().copied().find(|id| {
        !known.contains(id)
            && runner
                .state()
                .objects
                .get(id)
                .is_some_and(|o| o.is_token && o.controller == P0)
    })
}

#[test]
fn ardyn_combat_trigger_exiles_opponents_graveyard_creature_and_copies_it_as_demon() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature_from_oracle(P0, "Ardyn, the Usurper", 4, 4, ARDYN_ORACLE);

    // Two legal targets — one in EACH player's graveyard — so target
    // selection genuinely pauses (a lone candidate auto-resolves inline on
    // this engine, defeating the point of choosing one) and the "any
    // graveyard, not just yours" claim is actually exercised.
    let own_grave_creature = scenario
        .add_creature_to_graveyard(P0, "Own Graveyard Golem", 3, 3)
        .id();
    let opp_grave_creature = scenario
        .add_creature_to_graveyard(P1, "Opponent's Graveyard Bear", 2, 2)
        .id();

    let mut runner = scenario.build();

    // Advance out of the pre-combat main phase; the begin-combat trigger
    // fires as the game enters BeginCombat on P0's own turn.
    runner.pass_both_players();
    assert_eq!(
        runner.state().phase,
        Phase::BeginCombat,
        "the game must be at the beginning of combat for Starscourge to fire"
    );

    // "up to one target creature card from a graveyard": a genuine optional
    // target slot on the TRIGGER, chosen when the trigger goes on the stack.
    let WaitingFor::TriggerTargetSelection { target_slots, .. } =
        runner.state().waiting_for.clone()
    else {
        panic!(
            "expected a TriggerTargetSelection prompt for Starscourge's optional \
             exile target, got {:?}",
            runner.state().waiting_for
        );
    };
    let legal = &target_slots[0].legal_targets;
    assert!(
        legal.contains(&TargetRef::Object(own_grave_creature)),
        "the controller's own graveyard creature must be a legal target; legal={legal:?}"
    );
    assert!(
        legal.contains(&TargetRef::Object(opp_grave_creature)),
        "issue #5989: the OPPONENT's graveyard creature must also be a legal target — \
         the filter carries no controller restriction; legal={legal:?}"
    );

    // Choose the opponent's creature — the half of the reported symptom
    // ("only works if I do it to my graveyard") this test is pinning.
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(opp_grave_creature)],
        })
        .expect("choosing the opponent's graveyard creature must succeed");

    let known: Vec<ObjectId> = runner.state().battlefield.iter().copied().collect();

    // Resolve the trigger off the stack. The printed instruction has no
    // "you may": once a target was chosen, the exile is mandatory, so no
    // optional-effect prompt may interpose.
    runner.advance_until_stack_empty();
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "the printed exile is mandatory (no \"you may\") — got an unexpected \
         optional-effect prompt"
    );

    assert_eq!(
        runner
            .state()
            .objects
            .get(&opp_grave_creature)
            .map(|o| o.zone),
        Some(Zone::Exile),
        "the chosen creature card must actually be exiled"
    );

    // The reflexive "If you exiled a card this way, create a token that's a
    // copy of that card, except it's a 5/5 black Demon" must have fired
    // automatically — no further player action needed.
    let demon = find_new_token(&runner, &known).unwrap_or_else(|| {
        panic!(
            "issue #5989: no token was created — the reflexive copy clause \
             never fired; battlefield={:?}",
            runner.state().battlefield
        )
    });

    let demon_obj = &runner.state().objects[&demon];
    assert_eq!(
        demon_obj.name, "Opponent's Graveyard Bear",
        "the token must be a COPY OF THAT CARD (the exiled Bear), got name={:?}",
        demon_obj.name
    );
    assert_eq!(
        (demon_obj.power, demon_obj.toughness),
        (Some(5), Some(5)),
        "the copy must be a 5/5 (the \"except\" clause), got {:?}/{:?}",
        demon_obj.power,
        demon_obj.toughness
    );
    assert!(
        demon_obj
            .card_types
            .subtypes
            .iter()
            .any(|s| s.eq_ignore_ascii_case("Demon")),
        "the copy must be a Demon, got subtypes={:?}",
        demon_obj.card_types.subtypes
    );
    assert_ne!(
        demon, opp_grave_creature,
        "the token must be a NEW, distinct object — not the original exiled card itself"
    );

    // Full-card cross-check: the first printed line ("Demons you control have
    // menace, lifelink, and haste.") must reach the new Demon token too.
    assert!(
        demon_obj.keywords.contains(&Keyword::Haste),
        "Ardyn's own Demon anthem must grant the new Demon token haste, got {:?}",
        demon_obj.keywords
    );
}
