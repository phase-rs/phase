//! Sarkhan, Dragon Ascendant — the "you may behold a Dragon" ETB effect
//! (`Effect::Behold`, CR 701.4a) driven through the real cast → ETB trigger →
//! `OptionalEffectChoice` → behold → Treasure pipeline.
//!
//! Oracle (verbatim, from client/public/card-data.json):
//!   "When Sarkhan enters, you may behold a Dragon. If you do, create a Treasure
//!    token. (To behold a Dragon, choose a Dragon you control or reveal a Dragon
//!    card from your hand.)
//!    Whenever a Dragon you control enters, put a +1/+1 counter on Sarkhan. Until
//!    end of turn, Sarkhan becomes a Dragon in addition to its other types and
//!    gains flying."
//!
//! CR 701.4a: "Behold a [quality]" means "Reveal a [quality] card from your hand
//! or choose a [quality] permanent you control on the battlefield." The candidate
//! set is battlefield-you-control ∪ your matching hand cards. With 0 candidates
//! the behold whiffs; with 1 it auto-collapses; with 2+ the controller chooses
//! (CR 608.2d), and a HAND pick is publicly revealed (`CardsRevealed`).
//!
//! #5051 (CR 400.7): Sarkhan's behold is FIXED-quality (Subtype Dragon) — it
//! writes no `ChosenAttribute`, so the stale-chosen-type recast bug cannot
//! manifest here. If `Effect::Behold` ever gains a `type_choice` axis it joins
//! the #5051 blast radius.
//!
//! Each runtime assertion is revert-to-red: reverting the parser leaf leaves the
//! ETB effect as `Effect::Unimplemented { name: "behold" }`, which resolves to a
//! no-op — the behold never happens, so the Treasure (gated on
//! `OptionalEffectPerformed`) is never created and no `CardsRevealed` fires.

use engine::game::filter_state_for_viewer;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCondition, Effect, EffectOutcomeSignal, TargetFilter, TypeFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const SARKHAN: &str = "When Sarkhan enters, you may behold a Dragon. If you do, create a Treasure \
token. (To behold a Dragon, choose a Dragon you control or reveal a Dragon card from your hand.)\n\
Whenever a Dragon you control enters, put a +1/+1 counter on Sarkhan. Until end of turn, Sarkhan \
becomes a Dragon in addition to its other types and gains flying.";

/// Number of Treasure tokens on the battlefield owned by `player`. The behold
/// rider ("If you do, create a Treasure token") is the single Treasure source in
/// this fixture, so this count is the behold-performed discriminator.
fn treasure_count(runner: &GameRunner, player: engine::types::player::PlayerId) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|&&id| {
            runner
                .state()
                .objects
                .get(&id)
                .is_some_and(|o| o.owner == player && o.name == "Treasure")
        })
        .count()
}

/// All card ids that appeared in any `CardsRevealed` event.
fn revealed_ids(events: &[GameEvent]) -> Vec<ObjectId> {
    events
        .iter()
        .filter_map(|e| match e {
            GameEvent::CardsRevealed { card_ids, .. } => Some(card_ids.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

/// Cast Sarkhan (a 0-cost creature here) and drive the ETB behold trigger to
/// completion through `apply()`. Accept/decline the "you may" per `accept`; when
/// a `WaitingFor::BeholdChoice` is raised (2+ candidates) submit `behold_pick`.
/// Returns (accumulated events, whether an OptionalEffectChoice was offered,
/// whether a BeholdChoice prompt was reached).
struct DriveResult {
    events: Vec<GameEvent>,
    optional_offered: bool,
    behold_prompted: bool,
}

fn cast_and_drive(
    runner: &mut GameRunner,
    sarkhan: ObjectId,
    accept: bool,
    behold_pick: Option<ObjectId>,
) -> DriveResult {
    let mut events = Vec::new();
    let mut optional_offered = false;
    let mut behold_prompted = false;

    let card_id = runner.state().objects[&sarkhan].card_id;
    let r = runner
        .act(GameAction::CastSpell {
            object_id: sarkhan,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Sarkhan must be accepted");
    events.extend(r.events);

    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                optional_offered = true;
                let r = runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("DecideOptionalEffect must be accepted");
                events.extend(r.events);
            }
            WaitingFor::BeholdChoice { choices, .. } => {
                behold_prompted = true;
                let pick = behold_pick.expect("a BeholdChoice prompt needs a declared pick");
                assert!(
                    choices.contains(&pick),
                    "declared behold pick {pick:?} must be an offered candidate: {choices:?}"
                );
                let r = runner
                    .act(GameAction::SelectCards { cards: vec![pick] })
                    .expect("submitting the behold pick must be accepted");
                events.extend(r.events);
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                match runner.act(GameAction::PassPriority) {
                    Ok(r) => events.extend(r.events),
                    Err(_) => break,
                }
            }
            other => panic!("unexpected prompt while driving Sarkhan's behold: {other:?}"),
        }
    }

    DriveResult {
        events,
        optional_offered,
        behold_prompted,
    }
}

/// Build a game with Sarkhan in P0's hand, ready to cast.
fn sarkhan_scenario() -> (GameScenario, ()) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    (scenario, ())
}

/// (a) One Dragon on the battlefield P0 controls, none in hand → accept →
/// behold auto-collapses to the single candidate (|elig| == 1). A battlefield
/// permanent is already public, so NO `CardsRevealed` fires; the Treasure is
/// created.
#[test]
fn behold_single_battlefield_dragon_no_reveal_makes_treasure() {
    let (mut scenario, ()) = sarkhan_scenario();
    // A Dragon already on the battlefield (placed, not entering — does not fire
    // the "Whenever a Dragon you control enters" trigger).
    scenario
        .add_creature(P0, "Shivan Dragon", 5, 5)
        .with_subtypes(vec!["Dragon"]);
    let sarkhan = scenario
        .add_creature_to_hand_from_oracle(P0, "Sarkhan, Dragon Ascendant", 2, 2, SARKHAN)
        .id();
    let mut runner = scenario.build();

    let result = cast_and_drive(&mut runner, sarkhan, true, None);

    assert!(
        result.optional_offered,
        "the optional 'you may behold' must be offered (reach-guard)"
    );
    assert!(
        !result.behold_prompted,
        "a single battlefield candidate must auto-collapse — no BeholdChoice prompt"
    );
    assert!(
        revealed_ids(&result.events).is_empty(),
        "beholding a controlled battlefield permanent reveals nothing (CR 701.4a)"
    );
    assert_eq!(
        treasure_count(&runner, P0),
        1,
        "behold performed → the Treasure rider fires (revert-to-red: Unimplemented behold makes no Treasure)"
    );
}

/// (b) One Dragon card in P0's hand, none on the battlefield → accept → behold
/// auto-collapses to the single hand candidate, emitting `CardsRevealed` for it
/// (CR 701.4a). The card STAYS in hand; the Treasure is created.
#[test]
fn behold_single_hand_dragon_reveals_and_makes_treasure() {
    let (mut scenario, ()) = sarkhan_scenario();
    let hand_dragon = scenario
        .add_creature_to_hand(P0, "Furnace Whelp", 2, 2)
        .with_subtypes(vec!["Dragon"])
        .id();
    let sarkhan = scenario
        .add_creature_to_hand_from_oracle(P0, "Sarkhan, Dragon Ascendant", 2, 2, SARKHAN)
        .id();
    let mut runner = scenario.build();

    let result = cast_and_drive(&mut runner, sarkhan, true, None);

    assert!(
        !result.behold_prompted,
        "a single hand candidate must auto-collapse — no BeholdChoice prompt"
    );
    assert!(
        revealed_ids(&result.events).contains(&hand_dragon),
        "beholding a hand card publicly reveals it (CR 701.4a)"
    );
    // CR 701.4a: revealing does NOT move the card — it stays in hand.
    assert!(
        runner
            .state()
            .players
            .iter()
            .find(|p| p.id == P0)
            .is_some_and(|p| p.hand.contains(&hand_dragon)),
        "the revealed Dragon must remain in hand (behold does not move it)"
    );
    assert_eq!(
        treasure_count(&runner, P0),
        1,
        "behold performed → Treasure created"
    );
}

/// (b2) Two DISTINCT Dragon cards in hand, none on the battlefield → accept →
/// the interactive `WaitingFor::BeholdChoice` is reached (CR 608.2d). Submitting
/// the pick reveals ONLY the chosen Dragon; the non-chosen Dragon stays hidden.
/// The Treasure is created.
#[test]
fn behold_two_hand_dragons_prompts_and_reveals_only_chosen() {
    let (mut scenario, ()) = sarkhan_scenario();
    let dragon_a = scenario
        .add_creature_to_hand(P0, "Furnace Whelp", 2, 2)
        .with_subtypes(vec!["Dragon"])
        .id();
    let dragon_b = scenario
        .add_creature_to_hand(P0, "Shivan Dragon", 5, 5)
        .with_subtypes(vec!["Dragon"])
        .id();
    let sarkhan = scenario
        .add_creature_to_hand_from_oracle(P0, "Sarkhan, Dragon Ascendant", 2, 2, SARKHAN)
        .id();
    let mut runner = scenario.build();

    let result = cast_and_drive(&mut runner, sarkhan, true, Some(dragon_a));

    assert!(
        result.behold_prompted,
        "two hand Dragons is a genuine choice — the BeholdChoice prompt must be reached"
    );
    let revealed = revealed_ids(&result.events);
    assert!(
        revealed.contains(&dragon_a),
        "the chosen Dragon must be revealed (CR 701.4a)"
    );
    assert!(
        !revealed.contains(&dragon_b),
        "the NON-chosen Dragon must stay hidden — only the beheld card is revealed"
    );
    assert!(
        runner
            .state()
            .players
            .iter()
            .find(|p| p.id == P0)
            .is_some_and(|p| p.hand.contains(&dragon_a) && p.hand.contains(&dragon_b)),
        "both Dragons remain in hand (behold does not move the revealed card)"
    );
    assert_eq!(
        treasure_count(&runner, P0),
        1,
        "behold performed → Treasure created"
    );
}

/// (b2-redaction / B2) While `WaitingFor::BeholdChoice` is parked with 2+ hidden
/// hand-Dragon candidates, the opponent's serialized view must NOT contain the
/// real candidate ids — they are redacted to `ObjectId(0)`. Revert-to-red:
/// removing the `BeholdChoice` arm in `visibility.rs::filter_state_for_viewer`
/// leaks the pre-choice hand-Dragon list to the opponent (this assertion fails).
#[test]
fn behold_choice_candidates_redacted_from_opponent_view() {
    let (mut scenario, ()) = sarkhan_scenario();
    let dragon_a = scenario
        .add_creature_to_hand(P0, "Furnace Whelp", 2, 2)
        .with_subtypes(vec!["Dragon"])
        .id();
    let dragon_b = scenario
        .add_creature_to_hand(P0, "Shivan Dragon", 5, 5)
        .with_subtypes(vec!["Dragon"])
        .id();
    let sarkhan = scenario
        .add_creature_to_hand_from_oracle(P0, "Sarkhan, Dragon Ascendant", 2, 2, SARKHAN)
        .id();
    let mut runner = scenario.build();

    // Drive up to (but not through) the BeholdChoice prompt.
    let card_id = runner.state().objects[&sarkhan].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: sarkhan,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Sarkhan must be accepted");
    let mut reached = false;
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the you-may");
            }
            WaitingFor::BeholdChoice { .. } => {
                reached = true;
                break;
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert!(reached, "must reach the parked BeholdChoice (reach-guard)");

    // Positive reach-guard: the acting player (P0) sees the real candidate ids.
    let own = filter_state_for_viewer(runner.state(), P0);
    let WaitingFor::BeholdChoice {
        choices: own_choices,
        ..
    } = &own.waiting_for
    else {
        panic!("P0 view must still be a BeholdChoice");
    };
    assert!(
        own_choices.contains(&dragon_a) && own_choices.contains(&dragon_b),
        "the choosing player must see their real candidates: {own_choices:?}"
    );

    // B2: the opponent's serialized view must redact the candidate ids — the
    // pre-choice hand-Dragon list is hidden information (CR 400.2 + CR 701.4a).
    let opp = filter_state_for_viewer(runner.state(), P1);
    let WaitingFor::BeholdChoice {
        choices: opp_choices,
        ..
    } = &opp.waiting_for
    else {
        panic!("opponent view must still be a BeholdChoice");
    };
    assert!(
        !opp_choices.contains(&dragon_a) && !opp_choices.contains(&dragon_b),
        "opponent must NOT see the real hand-Dragon candidate ids: {opp_choices:?}"
    );
    assert!(
        opp_choices.iter().all(|&id| id == ObjectId(0)),
        "opponent's candidate ids must be redacted to ObjectId(0): {opp_choices:?}"
    );
}

/// (c) NO Dragon anywhere → ACCEPT the "you may" (drive the accept path, not
/// decline) → the behold whiffs (0 candidates) → `cost_payment_failed_flag` is
/// set → the Treasure rider does NOT fire. Reach-guard: the OptionalEffectChoice
/// was offered and accepted, and no BeholdChoice prompt was raised.
#[test]
fn behold_accept_with_no_dragon_makes_no_treasure() {
    let (mut scenario, ()) = sarkhan_scenario();
    let sarkhan = scenario
        .add_creature_to_hand_from_oracle(P0, "Sarkhan, Dragon Ascendant", 2, 2, SARKHAN)
        .id();
    let mut runner = scenario.build();

    let result = cast_and_drive(&mut runner, sarkhan, true, None);

    assert!(
        result.optional_offered,
        "the you-may must be offered and accepted (reach-guard: not vacuously skipped)"
    );
    assert!(
        !result.behold_prompted,
        "no candidate → no BeholdChoice prompt"
    );
    assert!(
        revealed_ids(&result.events).is_empty(),
        "a whiffed behold reveals nothing"
    );
    assert_eq!(
        treasure_count(&runner, P0),
        0,
        "accepted but unable to behold → no Treasure (performed && !cost_payment_failed_flag = false)"
    );
}

/// (d) DECLINE the "you may" → the behold never runs → no reveal, no Treasure.
/// Reach-guard: the OptionalEffectChoice was offered (and declined).
#[test]
fn behold_decline_makes_no_treasure() {
    let (mut scenario, ()) = sarkhan_scenario();
    // A beholdable Dragon exists — proving the decline (not an inability) is what
    // suppresses the Treasure.
    scenario
        .add_creature(P0, "Shivan Dragon", 5, 5)
        .with_subtypes(vec!["Dragon"]);
    let sarkhan = scenario
        .add_creature_to_hand_from_oracle(P0, "Sarkhan, Dragon Ascendant", 2, 2, SARKHAN)
        .id();
    let mut runner = scenario.build();

    let result = cast_and_drive(&mut runner, sarkhan, false, None);

    assert!(
        result.optional_offered,
        "the you-may must be offered (reach-guard) so the decline is meaningful"
    );
    assert!(
        !result.behold_prompted,
        "declining never reaches the behold choice"
    );
    assert!(
        revealed_ids(&result.events).is_empty(),
        "declining reveals nothing"
    );
    assert_eq!(
        treasure_count(&runner, P0),
        0,
        "declining the you-may creates no Treasure (revert-to-red: decline path)"
    );
}

/// True if `filter` is a plain Dragon-subtype quality filter.
fn filter_is_dragon(filter: &TargetFilter) -> bool {
    let TargetFilter::Typed(tf) = filter else {
        return false;
    };
    tf.type_filters
        .iter()
        .any(|t| matches!(t, TypeFilter::Subtype(s) if s == "Dragon"))
}

/// Recursively scan an ability + its sub/else chain for any `Unimplemented`.
fn chain_has_unimplemented(ability: &engine::types::ability::AbilityDefinition) -> bool {
    if matches!(*ability.effect, Effect::Unimplemented { .. }) {
        return true;
    }
    ability
        .sub_ability
        .as_deref()
        .is_some_and(chain_has_unimplemented)
        || ability
            .else_ability
            .as_deref()
            .is_some_and(chain_has_unimplemented)
}

/// SHAPE: Sarkhan's ETB trigger lowers "you may behold a Dragon" to
/// `Effect::Behold { filter: Subtype(Dragon) }` with ZERO residual
/// `Unimplemented`; the "If you do, create a Treasure token" rider is preserved
/// as a `Token` sub_ability gated on `OptionalEffectPerformed`; and the separate
/// "Whenever a Dragon you control enters" trigger (PutCounter) is unchanged.
/// Revert-to-red: without the parser leaf the ETB effect is
/// `Effect::Unimplemented { name: "behold" }`.
#[test]
fn sarkhan_parses_behold_effect_preserving_treasure_and_dragon_trigger() {
    let parsed = parse_oracle_text(
        SARKHAN,
        "Sarkhan, Dragon Ascendant",
        &[],
        &["Legendary".to_string(), "Creature".to_string()],
        &["Human".to_string(), "Druid".to_string()],
    );

    // Two triggers: the ETB behold + the Dragon-enters counter trigger.
    assert_eq!(
        parsed.triggers.len(),
        2,
        "expected two triggers: {parsed:#?}"
    );

    let behold_trigger = parsed
        .triggers
        .iter()
        .filter_map(|t| t.execute.as_deref())
        .find(|e| matches!(*e.effect, Effect::Behold { .. }))
        .expect("one ETB trigger must carry Effect::Behold");

    // The behold quality is the Dragon subtype filter.
    let Effect::Behold { filter } = behold_trigger.effect.as_ref() else {
        unreachable!("filtered to Behold above");
    };
    assert!(
        filter_is_dragon(filter),
        "behold filter must be the Dragon subtype, got {filter:?}"
    );

    // The "If you do, create a Treasure token" rider is preserved as a Token
    // sub_ability gated on OptionalEffectPerformed (CR 608.2c).
    let treasure = behold_trigger
        .sub_ability
        .as_deref()
        .expect("the behold trigger must keep its Treasure sub_ability");
    assert!(
        matches!(*treasure.effect, Effect::Token { .. }),
        "the rider must be a Token effect, got {:?}",
        treasure.effect
    );
    assert!(
        matches!(
            treasure.condition,
            Some(AbilityCondition::EffectOutcome {
                signal: EffectOutcomeSignal::OptionalEffectPerformed
            })
        ),
        "the Treasure rider must stay gated on OptionalEffectPerformed, got {:?}",
        treasure.condition
    );

    // Zero residual Unimplemented anywhere in the trigger chains.
    for trigger in &parsed.triggers {
        if let Some(exec) = trigger.execute.as_deref() {
            assert!(
                !chain_has_unimplemented(exec),
                "no Unimplemented in any trigger chain: {trigger:#?}"
            );
        }
    }

    // The Dragon-enters trigger is unchanged: its effect is PutCounter (the
    // "+1/+1 counter on Sarkhan" head), not touched by the behold change.
    let dragon_trigger = parsed
        .triggers
        .iter()
        .filter_map(|t| t.execute.as_deref())
        .find(|e| matches!(*e.effect, Effect::PutCounter { .. }));
    assert!(
        dragon_trigger.is_some(),
        "the 'Whenever a Dragon you control enters' PutCounter trigger must be intact: {parsed:#?}"
    );
}
