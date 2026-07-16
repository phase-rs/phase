//! Watched-red witness for phase.rs task #150 (DrawSequenceFrame origin/kind +
//! completion routing).
//!
//! CR 701.50a + CR 121.6b + CR 616.1g: connive's draw currently bypasses the
//! draw-sequence frame machinery — `resolve_connive_effect` calls
//! `effects::draw::draw_through_replacement` directly instead of
//! `start_draw_sequence`, so a per-unit draw replacement (Dredge) that pauses
//! the draw resumes through `engine_replacement::handle_replacement_choice`'s
//! generic `Draw` arm, which settles only the draw itself. Connive's own tail
//! (discard the drawn card(s), place a +1/+1 counter per nonland discard, emit
//! `EffectResolved { kind: Connive }`) never runs, because
//! `resolve_connive_effect`'s `ReplacementResult::NeedsChoice` arm returns
//! `Ok(())` before Step 2. This test drives that exact pause/resume path
//! against a real Connive activated ability (Hypnotic Grifter, verified via
//! Scryfall: "{3}: This creature connives.") and a real Dredge card
//! (Stinkweed Imp, Dredge 5) and MUST FAIL before the DrawSequenceFrame
//! origin/kind routing lands.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{Effect, EffectKind, ResolvedAbility};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const STINKWEED_IMP_ORACLE: &str = "Flying\n\
Whenever this creature deals combat damage to a creature, destroy that creature.\n\
Dredge 5 (If you would draw a card, you may mill five cards instead. If you do, return this card from your graveyard to your hand.)";

// Hypnotic Grifter — {U} Creature — Human Rogue, 1/2. Oracle text verified via
// the Scryfall API (2026-07-14): "{3}: This creature connives." A real,
// minimal, self-targeting Connive activated ability (Connive 1, no discard
// choice needed with an empty hand) — chosen instead of the fabricated
// "Doom Whisperer" example in the task brief, which is actually a Surveil
// creature with no Connive ability at all.
const HYPNOTIC_GRIFTER_ABILITY: &str = "{3}: This creature connives. \
(Draw a card, then discard a card. If you discarded a nonland card, put a +1/+1 counter on this creature.)";

fn connive_ability_index(runner: &GameRunner, id: ObjectId) -> usize {
    runner.state().objects[&id]
        .abilities
        .iter()
        .position(|a| matches!(a.effect.as_ref(), Effect::Connive { .. }))
        .expect("Hypnotic Grifter must carry a Connive activated ability")
}

/// CR 701.50a + CR 702.52a: Hypnotic Grifter connives; Stinkweed Imp's Dredge
/// pauses the underlying draw; declining Dredge must still let the connive's
/// tail complete — discard, +1/+1 counter, and `EffectResolved { Connive }`.
#[test]
fn connive_draw_pause_tail_completes_on_dredge_decline() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Library >= Stinkweed Imp's Dredge 5 so Dredge stays offered; the top
    // card is nonland so the auto-discard (hand size 1 <= connive count 1,
    // no ConniveDiscard choice) yields exactly one +1/+1 counter.
    scenario.with_library_top(P0, &["Nonland Top", "Lib 2", "Lib 3", "Lib 4", "Lib 5"]);
    scenario
        .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
        .from_oracle_text(STINKWEED_IMP_ORACLE);
    let grifter = scenario
        .add_creature_from_oracle(P0, "Hypnotic Grifter", 1, 2, HYPNOTIC_GRIFTER_ABILITY)
        .id();

    let mut runner = scenario.build();
    let idx = connive_ability_index(&runner, grifter);
    let def = runner.state().objects[&grifter].abilities[idx].clone();
    let ability: ResolvedAbility = build_resolved_from_def(&def, grifter, P0);

    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("connive must propose its draw");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "Stinkweed Imp's Dredge must pause the connive's underlying draw for a \
         replacement choice, got {:?}",
        runner.state().waiting_for
    );

    // Decline Dredge (index 1, matching the established convention in
    // replacement.rs's `multi_draw_decline_dredge_unit_one_still_draws_unit_two_normally`)
    // — draw the top card of the library normally.
    let outcome = runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("decline Dredge");

    assert!(
        runner.state().players[0].hand.is_empty(),
        "connive must discard the drawn card once its draw settles — hand: {:?}",
        runner.state().players[0].hand
    );
    assert_eq!(
        runner.state().objects[&grifter]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "CR 701.50a: discarding a nonland card must put a +1/+1 counter on the \
         conniving creature — dropped when the paused draw's frame completion \
         doesn't route back into connive's tail"
    );
    assert!(
        outcome.events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Connive,
                source_id,
                ..
            } if *source_id == grifter
        )),
        "CR 701.50f: EffectResolved {{ kind: Connive }} must fire once the paused \
         draw's frame completes — got {:?}",
        outcome.events
    );
}
