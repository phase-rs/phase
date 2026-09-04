//! Coverage-gap add (§3.3): Soul Tether (FRA "Reality Fracture").
//!
//! Verbatim Scryfall Oracle text (Woodwork Prodigy // Soul Tether, back face,
//! sorcery): "Create a Heartwood token. (It's a red and green artifact with
//! "{T}: Add {R} or {G}.")"
//!
//! Before this change the Heartwood token had no registry entry
//! (`crates/engine/data/known-tokens.toml`), so the whole ability parsed to
//! `Effect::Unimplemented { name: "create", .. }` (`supported: false`,
//! `gap_count: 1` in the coverage report). This is a pure data-addition case:
//! the token registry / `predefined_token_abilities` mechanism used by every
//! other named mana-rock token (Treasure, Gold, Powerstone, Vibranium, …)
//! already models "artifact token with a printed mana ability" — Heartwood
//! only needed its own catalog entry plus a `predefined_token_abilities`
//! arm parameterizing the existing `ManaProduction::AnyOneColor` building
//! block (already used by Treasure/Gold for "any of five colors") down to the
//! token's own two printed colors.
//!
//! CR 111.10: "Some effects instruct a player to create a predefined token.
//! These effects use the definition below to determine the characteristics
//! the token is created with." Heartwood is red/green (a departure from every
//! *other* CR 111.10 entry, which are all colorless) but the rule's mechanism
//! — the creating effect's reminder text fully specifies a fixed body — is
//! the same.
//! CR 605.1a: an activated ability with no target that could add mana to a
//! player's mana pool, and whose cost/effect moves no card to/from a library,
//! is a mana ability.
//!
//! This test drives the real cast pipeline end to end (CR 601 casting +
//! CR 111.1 token creation + CR 602 ability activation), and is discriminating
//! against three independent regressions: (1) reverting the registry entry
//! collapses the whole ability back to `Effect::Unimplemented` and the cast
//! creates no token at all; (2) reverting the `predefined_token_abilities`
//! arm leaves an abilityless Heartwood token (0 activated abilities); (3)
//! narrowing `color_options` to a single color removes Green (or Red) from
//! the offered `ChooseManaColor` options, which the reach-guard below asserts
//! explicitly before submitting the non-default (second-listed) color.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{ManaChoice, ManaChoicePrompt, WaitingFor};
use engine::types::mana::{ManaColor, ManaCost, ManaType};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SOUL_TETHER_ORACLE: &str =
    "Create a Heartwood token. (It's a red and green artifact with \"{T}: Add {R} or {G}.\")";

#[test]
fn soul_tether_creates_a_heartwood_token_with_a_two_color_mana_ability() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Soul Tether", false, SOUL_TETHER_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();

    // ── reach guard #1: the resolution actually created a token ──
    assert_eq!(
        outcome.state().last_created_token_ids.len(),
        1,
        "Soul Tether creates exactly one Heartwood token, not {:?}",
        outcome.state().last_created_token_ids
    );
    let token_id = *outcome
        .state()
        .last_created_token_ids
        .first()
        .expect("CR 111.1: resolving Soul Tether must create the Heartwood token");
    let token = outcome
        .state()
        .objects
        .get(&token_id)
        .expect("the created token exists on the battlefield");

    assert!(token.is_token, "the created object is a token");
    assert_eq!(token.zone, Zone::Battlefield);
    assert_eq!(token.name, "Heartwood");
    assert_eq!(
        token.card_types.core_types,
        vec![CoreType::Artifact],
        "Heartwood is an artifact, not a creature — no other core type"
    );
    assert!(
        token
            .card_types
            .subtypes
            .iter()
            .any(|subtype| subtype == "Heartwood"),
        "CR 111.10: the token's own name is its special type, got {:?}",
        token.card_types.subtypes
    );
    assert_eq!(token.power, None, "a noncreature token carries no power");
    assert_eq!(
        token.toughness, None,
        "a noncreature token carries no toughness"
    );
    let mut colors = token.color.clone();
    colors.sort_by_key(|c| format!("{c:?}"));
    assert_eq!(
        colors,
        vec![ManaColor::Green, ManaColor::Red],
        "Heartwood is red AND green — not colorless like every other CR 111.10 rock"
    );

    // ── reach guard #2: the injected mana ability is really there ──
    assert_eq!(
        token.base_abilities.len(),
        1,
        "CR 111.10: the predefined_token_abilities injection must contribute exactly \
         one ability — a reverted `\"Heartwood\" => vec![heartwood_ability()]` arm \
         leaves this at 0"
    );

    // ── activate the mana ability directly (CR 605.3b: mana abilities never
    // use the stack, so no Priority round-trip is needed between activation
    // and the color prompt) ──
    let mut runner = GameRunner::from_state(outcome.state().clone());
    runner
        .act(GameAction::ActivateAbility {
            source_id: token_id,
            ability_index: 0,
        })
        .expect("activating Heartwood's tap ability must be accepted");

    // THE DISCRIMINATOR: both printed colors — not just one — must be legal.
    // A regression that narrows `color_options` to `[Red]` (or hardcodes
    // `Fixed`) fails this assertion before the color is ever chosen.
    let WaitingFor::ChooseManaColor {
        choice: ManaChoicePrompt::SingleColor { options },
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected a ChooseManaColor prompt after tapping Heartwood, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        options,
        &vec![ManaType::Red, ManaType::Green],
        "CR 111.10 + CR 605.1a: Heartwood must offer exactly {{R}} or {{G}}"
    );

    // Deliberately choose the SECOND listed color: a default-to-first-option
    // implementation bug (or a `Fixed { colors: [Red] }` regression) would
    // make this either impossible to submit meaningfully or silently produce
    // Red anyway — the mana-pool assertion below catches that.
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaColor::Green.into()),
            count: 1,
        })
        .expect("choosing Green at Heartwood's mana prompt must be accepted");

    let player = &runner.state().players[P0.0 as usize];
    assert_eq!(
        player.mana_pool.count_color(ManaType::Green),
        1,
        "activating Heartwood and choosing Green must add exactly {{G}} to the pool"
    );
    assert_eq!(
        player.mana_pool.count_color(ManaType::Red),
        0,
        "choosing Green must not also add Red"
    );
    assert_eq!(player.mana_pool.total(), 1);
    assert!(
        runner.state().objects.get(&token_id).unwrap().tapped,
        "CR 107.5: the {{T}} symbol in the activation cost taps the token"
    );
}
