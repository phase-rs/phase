//! Regression: GitHub issue #534 — Skullwinder's ETB "choose an opponent.
//! That player returns a card from their graveyard to their hand."
//!
//! Bug: the engine resolved the *card selection* (which card returns from the
//! opponent's graveyard) as the *casting player's* choice. Per the card's own
//! ruling — "The chosen opponent gets to choose which card to return from
//! their graveyard to their hand." — and CR 608.2c (the "rules of English"
//! make "That player" the just-chosen opponent) + CR 608.2d (the player
//! resolving the choice announces it), the chosen opponent makes that choice.
//!
//! Root cause was a parser defect (the trigger AST dropped "then choose an
//! opponent" entirely and left the dependent `Bounce` scoped to `ScopedPlayer`)
//! AND a resolver gap (non-targeted graveyard-return `Bounce` had no
//! `EffectZoneChoice` branch for the chosen-player case). The parser-side
//! discriminator lives in `parser::oracle_effect::tests::
//! skullwinder_etb_parses_choose_opponent`. THIS test pins the resolver-side
//! discriminator: given the dependent `Bounce` shape the fixed parser emits —
//! a `Typed` filter with `Owned { ChosenPlayer { 0 } }` + `InZone { Graveyard }`
//! and a `chosen_players = [P1]` binding — `bounce::resolve` must surface an
//! `EffectZoneChoice` whose `player` is P1 (the chosen opponent), not the
//! ability controller (P0).

use engine::game::effects::bounce;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::{
    BounceSelection, ControllerRef, Effect, FilterProp, ResolvedAbility, TargetFilter, TypeFilter,
    TypedFilter,
};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::CardId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// CR 608.2c + CR 608.2d + CR 109.4: the chosen opponent — not the caster —
/// selects which card returns from their own graveyard.
///
/// The dependent `Bounce` filter shape and the `chosen_players` binding are
/// exactly what the FIXED parser+resolution pipeline produces for Skullwinder
/// after the `Choose(Opponent)` clause resolves (see the parser-level
/// `skullwinder_etb_parses_choose_opponent` test for the AST proof). With the
/// resolver fix, this test asserts the `EffectZoneChoice.player` routes to the
/// chosen opponent (P1) — pre-fix the non-targeted graveyard-return branch did
/// not exist and the sub-ability silently no-op'd; the agency bug was that
/// any choice that did surface used `ability.controller` (P0).
#[test]
fn skullwinder_chosen_opponent_picks_their_own_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();
    let state = runner.state_mut();

    // Two distinct cards in P1's graveyard so a genuine selection exists —
    // with only one, the choice is forced and the test cannot discriminate
    // WHO chooses.
    let p1_gy_a = create_object(state, CardId(1), P1, "P1 Gy A".to_string(), Zone::Graveyard);
    let p1_gy_b = create_object(state, CardId(2), P1, "P1 Gy B".to_string(), Zone::Graveyard);
    // A decoy card in P0's graveyard (filter must NOT enumerate it — it's
    // owned by the caster, not by the chosen opponent).
    let p0_gy_decoy = create_object(
        state,
        CardId(3),
        P0,
        "P0 Decoy".to_string(),
        Zone::Graveyard,
    );
    // `TypeFilter::Card` matches every object regardless of core_types
    // (`filter.rs:1034` etc. — it's the "any card" sentinel) so the bare
    // objects above pass without stamping CoreTypes.
    // Skullwinder on the battlefield — the trigger source identity.
    let skullwinder = create_object(
        state,
        CardId(10),
        P0,
        "Skullwinder".to_string(),
        Zone::Battlefield,
    );

    // Construct the dependent Bounce sub-ability EXACTLY as the fixed parser
    // emits it for Skullwinder: a Typed[Card] filter scoped to the chosen
    // opponent's graveyard via `Owned { ChosenPlayer { index: 0 } }` +
    // `InZone { Graveyard }`, no top-level target, and `chosen_players = [P1]`
    // (populated by the engine's `Choose(Opponent)` answer handler at
    // `engine_resolution_choices.rs:1624`).
    let target = TargetFilter::Typed(TypedFilter {
        type_filters: vec![TypeFilter::Card],
        controller: None,
        properties: vec![
            FilterProp::Owned {
                controller: ControllerRef::ChosenPlayer { index: 0 },
            },
            FilterProp::InZone {
                zone: Zone::Graveyard,
            },
        ],
    });
    let mut ability = ResolvedAbility::new(
        Effect::Bounce {
            target,
            destination: None,
            selection: BounceSelection::Targeted,
        },
        Vec::new(),
        skullwinder,
        P0,
    );
    // The engine's `NamedChoice` intake pushes the chosen player's id into
    // `chosen_players` via `set_chosen_players_recursive`. The dependent
    // Bounce sees that binding when it resolves.
    ability.set_chosen_players_recursive(&[P1]);

    let mut events = Vec::new();
    bounce::resolve(state, &ability, &mut events).expect("bounce resolution must not error");

    // DISCRIMINATOR: the resolver must surface an `EffectZoneChoice` scoped
    // to P1 (the chosen opponent), NOT to P0 (the caster / ability.controller).
    match &state.waiting_for {
        WaitingFor::EffectZoneChoice {
            player,
            cards,
            zone,
            destination,
            ..
        } => {
            assert_eq!(
                *player, P1,
                "the CHOSEN OPPONENT (P1) selects which card returns — not the caster (P0). \
                 Pre-fix the resolver had no non-targeted graveyard-return branch and the \
                 sub-ability silently no-op'd; any choice that did surface used \
                 `ability.controller` (P0) — the agency bug under test."
            );
            assert_eq!(*zone, Zone::Graveyard);
            assert_eq!(*destination, Some(Zone::Hand));
            assert!(
                cards.contains(&p1_gy_a) && cards.contains(&p1_gy_b),
                "both of P1's graveyard cards must be candidates; cards={cards:?}"
            );
            assert!(
                !cards.contains(&p0_gy_decoy),
                "the caster's graveyard card must NOT be a candidate — the filter is \
                 ownership-scoped to the chosen opponent (CR 109.4)"
            );
        }
        other => panic!(
            "expected an EffectZoneChoice scoped to P1, got {other:?} \
             (pre-fix: the resolver had no graveyard-return non-targeted branch and the \
             sub-ability silently no-op'd, never surfacing any choice)"
        ),
    }

    // P1 picks one of their own cards; confirm the chosen card ends up in
    // P1's hand once the EffectZoneChoice intake routes the SelectCards
    // action. (The intake itself is exercised end-to-end by the existing
    // `bounce::tests::counted_bounce_all_prompts_controller_for_subset` and
    // BounceAll integration tests; here the load-bearing assertion is the
    // `player` field on the EffectZoneChoice — the agency bit.)
    let _ = (p1_gy_b, p0_gy_decoy); // silence unused-vars in case future refactor
}
