//! Integration coverage for The Notary Hobbits (HOB).
//!
//! Oracle:
//!   "When The Notary Hobbits enter, if they're not a token, create two
//!    tokens that are copies of them, except the tokens aren't legendary.
//!    {T}: Add {C} for each Halfling you control."
//!
//! Two mechanics under test:
//!   1. The ETB self-copy, gated by a "they're not a token" intervening-if
//!      (CR 603.4) so the two created token copies do NOT re-trigger their
//!      own copy of the same ability when THEY enter (CR 707.2: a token
//!      copies the source's rules text, including its triggered abilities).
//!      Without the guard this recurses without bound. The discriminating
//!      observation is that the stack is empty and the total permanent count
//!      is exactly 3 (1 original + 2 tokens) after the cast fully resolves —
//!      a naive implementation either hangs/panics on the recursion or (if
//!      some other limiter caps it) still produces the wrong count.
//!   2. The "except the tokens aren't legendary" copy modification (CR 707.9b)
//!      strips the Legendary supertype from the two tokens while the
//!      original stays legendary — CR 205.4a/d, the legend rule.
//!   3. The scaled mana ability, "{T}: Add {C} for each Halfling you
//!      control" — a standard tap-for-colorless ability, tested
//!      independently of summoning sickness via a battlefield placement.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 603.4: the intervening-if clause.
//!   - CR 707.2: a token copy acquires the copiable rules text (including
//!     triggered abilities) of the object it copies.
//!   - CR 707.9b: a copy effect may modify a characteristic (here, removing
//!     the Legendary supertype) as part of the copying process.
//!   - CR 111.1: tokens represent permanents not represented by a card.
//!   - CR 205.4a/d: the legendary supertype and the legend rule.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::card_type::Supertype;
use engine::types::game_state::GameState;
use engine::types::mana::ManaType;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const NOTARY_HOBBITS_ORACLE: &str = "When The Notary Hobbits enter, if they're not a token, \
    create two tokens that are copies of them, except the tokens aren't legendary.\n\
    {T}: Add {C} for each Halfling you control.";

fn notary_hobbits_permanents(
    state: &GameState,
    controller: PlayerId,
) -> Vec<&engine::game::game_object::GameObject> {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| o.name == "The Notary Hobbits" && o.controller == controller)
        .collect()
}

/// CR 603.4 + CR 707.2 + CR 707.9b: casting the original creates exactly two
/// token copies, both non-legendary, and the recursion guard prevents the
/// tokens' own copies of the same ETB from firing again.
#[test]
fn the_notary_hobbits_etb_creates_two_nonlegendary_tokens_without_recursion() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let notary_id = scenario
        .add_creature_to_hand(P0, "The Notary Hobbits", 1, 1)
        .as_legendary()
        .with_subtypes(vec!["Halfling", "Advisor"])
        .from_oracle_text(NOTARY_HOBBITS_ORACLE)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(notary_id).resolve();
    let state = outcome.state();

    // REVERT-GUARD (the critical anti-recursion assertion): without the
    // "if they're not a token" guard, each created token's own copy of the
    // ETB ability would ALSO try to create two more tokens, which either
    // never terminates (test hang/stack overflow) or leaves triggers
    // stranded on the stack. A clean, empty stack after `resolve()` is only
    // possible if the guard stopped the tokens from re-triggering.
    assert!(
        state.stack.is_empty(),
        "The Notary Hobbits ETB must resolve without token-copy ETB recursion; stack: {:?}",
        state.stack
    );

    let permanents = notary_hobbits_permanents(state, P0);
    assert_eq!(
        permanents.len(),
        3,
        "expected exactly 3 permanents named The Notary Hobbits (1 original + 2 tokens), got {}: {:?}",
        permanents.len(),
        permanents.iter().map(|o| (o.id, o.is_token)).collect::<Vec<_>>()
    );

    let originals: Vec<_> = permanents.iter().filter(|o| !o.is_token).collect();
    let tokens: Vec<_> = permanents.iter().filter(|o| o.is_token).collect();
    assert_eq!(originals.len(), 1, "exactly one non-token original");
    assert_eq!(tokens.len(), 2, "exactly two token copies");

    // CR 205.4d + CR 704.5j: the original keeps Legendary (it is not part of
    // the copy-modification's target set), so the legend rule still applies
    // to it going forward.
    assert!(
        originals[0]
            .card_types
            .supertypes
            .contains(&Supertype::Legendary),
        "the original permanent must remain legendary"
    );

    // CR 707.9b: "except the tokens aren't legendary" — both created tokens
    // must have the Legendary supertype stripped.
    for token in &tokens {
        assert!(
            !token.card_types.supertypes.contains(&Supertype::Legendary),
            "token copy {:?} must NOT be legendary (\"except the tokens aren't legendary\")",
            token.id
        );
        // CR 707.2: the copy still carries the copied subtypes (Halfling,
        // Advisor) — only the supertype is stripped by the except clause.
        assert!(
            token.card_types.subtypes.iter().any(|s| s == "Halfling"),
            "token copy must keep the Halfling subtype from the copied source"
        );
        // CR 707.2: a copied token retains the source's copiable rules text,
        // including the original ETB trigger. Its intervening-if condition is
        // what prevents that retained trigger from creating more tokens.
        assert!(
            token.trigger_definitions.iter_unchecked().any(|trigger| {
                trigger.definition.mode == TriggerMode::ChangesZone
                    && trigger.definition.destination == Some(Zone::Battlefield)
            }),
            "token copy must retain The Notary Hobbits's enters-the-battlefield trigger"
        );
    }
}

/// CR 605.1a + CR 205.3: "{T}: Add {C} for each Halfling you control" scales
/// with the number of Halflings the controller has, including itself.
/// Placed directly on the battlefield (no summoning sickness) so the tap
/// ability under test is isolated from the ETB/casting mechanic above.
#[test]
fn the_notary_hobbits_mana_ability_scales_with_halflings_controlled() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let notary_id = scenario
        .add_creature_from_oracle(P0, "The Notary Hobbits", 1, 1, NOTARY_HOBBITS_ORACLE)
        .as_legendary()
        .with_subtypes(vec!["Halfling", "Advisor"])
        .id();
    // Two more Halflings under the same controller: total of 3 Halflings.
    scenario
        .add_creature(P0, "Other Halfling A", 1, 1)
        .with_subtypes(vec!["Halfling"]);
    scenario
        .add_creature(P0, "Other Halfling B", 1, 1)
        .with_subtypes(vec!["Halfling"]);
    // A non-Halfling creature must NOT count toward the total.
    scenario.add_creature(P0, "Unrelated Bear", 2, 2);

    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: notary_id,
            ability_index: 0,
        })
        .expect("activating the mana ability must succeed");

    let pool = &runner.state().players[P0.0 as usize].mana_pool;
    assert_eq!(
        pool.count_color(ManaType::Colorless),
        3,
        "expected {{C}} equal to the number of Halflings controlled (3); pool = {:?}",
        pool.mana,
    );
    assert_eq!(
        pool.total(),
        3,
        "exactly the Halfling-scaled colorless total"
    );
}
