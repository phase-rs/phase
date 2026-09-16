//! Issue #782 — Thought Lash: "Exile the top card of your library: Prevent the
//! next 1 damage that would be dealt to you this turn."
//!
//! Reported as the activated ability resolving without its exile-the-top-card
//! cost being paid. CR 602.2b + CR 601.2h: activating an ability pays its total
//! cost, and an ability whose cost can't be paid can't be activated.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, ReplacementDefinition, ReplacementMode,
    TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::{EtbTapState, Zone};

// Verbatim Oracle text (Scryfall, 2026-09-15).
const THOUGHT_LASH: &str = "Cumulative upkeep—Exile the top card of your library. (At the beginning of your upkeep, put an age counter on this permanent, then sacrifice it unless you pay its upkeep cost for each age counter on it.)\nWhen a player doesn't pay this enchantment's cumulative upkeep, that player exiles all cards from their library.\nExile the top card of your library: Prevent the next 1 damage that would be dealt to you this turn.";

fn thought_lash_with_library(library_top_first: &[&str]) -> (GameRunner, ObjectId, usize) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    if !library_top_first.is_empty() {
        scenario.with_library_top(P0, library_top_first);
    }
    let lash = scenario
        .add_creature(P0, "Thought Lash", 0, 0)
        .as_enchantment()
        .from_oracle_text(THOUGHT_LASH)
        .id();
    let runner = scenario.build();
    // Reach-guard: the activated ability exists and its cost is the
    // top-of-library exile, so every later assertion is about paying it.
    let index = runner.state().objects[&lash]
        .abilities
        .iter()
        .position(|ability| {
            matches!(
                &ability.cost,
                Some(AbilityCost::Exile {
                    zone: Some(Zone::Library),
                    ..
                })
            )
        })
        .unwrap_or_else(|| {
            panic!(
                "Thought Lash must carry an exile-the-top-card activated ability; got {:?}",
                runner.state().objects[&lash]
                    .abilities
                    .iter()
                    .map(|a| (&a.cost, &a.effect))
                    .collect::<Vec<_>>()
            )
        });
    (runner, lash, index)
}

fn library_ids(runner: &GameRunner) -> Vec<ObjectId> {
    runner.state().players[0].library.iter().copied().collect()
}

#[test]
fn thought_lash_activation_exiles_the_top_card_of_your_library() {
    let (mut runner, lash, index) =
        thought_lash_with_library(&["Top Card", "Second Card", "Third Card"]);
    let before = library_ids(&runner);
    let top = *before.first().expect("reach-guard: library seeded");

    runner
        .act(GameAction::ActivateAbility {
            source_id: lash,
            ability_index: index,
        })
        .expect("activation with a nonempty library must be accepted");
    runner.advance_until_stack_empty();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the activation settles at priority; got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().objects[&top].zone,
        Zone::Exile,
        "paying the cost exiles the top card of the library"
    );
    assert_eq!(
        library_ids(&runner),
        before[1..].to_vec(),
        "exactly the top card leaves the library"
    );
}

#[test]
fn thought_lash_cannot_be_activated_with_an_empty_library() {
    let (mut runner, lash, index) = thought_lash_with_library(&[]);
    assert!(
        library_ids(&runner).is_empty(),
        "reach-guard: the library is empty"
    );
    let stack_before = runner.state().stack.len();

    let result = runner.act(GameAction::ActivateAbility {
        source_id: lash,
        ability_index: index,
    });

    assert!(
        result.is_err() || runner.state().stack.len() == stack_before,
        "an unpayable exile-the-top-card cost must not put the ability on the stack; \
         result = {result:?}, stack = {:?}",
        runner.state().stack
    );
}

// Verbatim Oracle text (Scryfall, 2026-09-16).
const PHYREXIAN_DEVOURER: &str = "When this creature's power is 7 or greater, sacrifice it.\nExile the top card of your library: Put X +1/+1 counters on this creature, where X is the exiled card's mana value.";

/// Find the exile-the-top-card activated ability on `source`.
fn exile_top_ability_index(runner: &GameRunner, source: ObjectId) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| {
            matches!(
                &ability.cost,
                Some(AbilityCost::Exile {
                    zone: Some(Zone::Library),
                    ..
                })
            )
        })
        .expect("reach-guard: the exile-the-top-card activated ability must be present")
}

/// CR 608.2k: "If an ability's effect refers to a specific untargeted object
/// that has been previously referred to by that ability's COST ... it still
/// affects that object." Phyrexian Devourer's "where X is the exiled card's
/// mana value" names the card its own cost exiled, so the cost-paid referent
/// must be bound BEFORE the payment moves that card to exile.
///
/// Revert-proof: without the pre-payment binding, X resolves against no
/// referent and the Devourer gains no counters (stays 1/1).
#[test]
fn phyrexian_devourer_counts_the_exiled_cards_mana_value() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Three Drop", "Filler One", "Filler Two"]);

    let devourer = {
        let mut b = scenario.add_creature(P0, "Phyrexian Devourer", 1, 1);
        b.from_oracle_text(PHYREXIAN_DEVOURER);
        b.id()
    };

    let mut runner = scenario.build();

    // A known mana value on top so X is unambiguous: {2}{R} = 3.
    let top = runner.state().players[0]
        .library
        .iter()
        .copied()
        .next()
        .expect("reach-guard: library seeded");
    runner.state_mut().objects.get_mut(&top).unwrap().mana_cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 2,
    };

    let index = exile_top_ability_index(&runner, devourer);
    runner
        .act(GameAction::ActivateAbility {
            source_id: devourer,
            ability_index: index,
        })
        .expect("activation must be accepted with a nonempty library");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&top].zone,
        Zone::Exile,
        "reach-guard: the cost exiled the top card"
    );
    assert_eq!(
        (
            runner.state().objects[&devourer].power,
            runner.state().objects[&devourer].toughness
        ),
        (Some(4), Some(4)),
        "CR 608.2k: X must equal the exiled card's mana value (3), leaving a 4/4"
    );
}

/// An OPTIONAL redirect of a move into `Zone::Exile`. Declining it leaves the
/// card settling in exile, which is the replacement-choice-to-exile path — the
/// shape `cost_zone_pipeline::exile_tracking_parked_resume_preserves_source_link`
/// uses for the effect-driven case.
fn optional_exile_redirect() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Exile)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                destination: Zone::Graveyard,
                origin: None,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        ))
        .mode(ReplacementMode::Optional { decline: None })
}

/// CR 406.6 + CR 616.1: a deterministic library-exile ACTIVATION cost that pauses
/// on a replacement choice and then settles in exile must still index the paid
/// card as "exiled with [source] this turn".
///
/// Revert-proof: the cast cost-move resume re-enters `finish_cost_object_moves`
/// at `paused_at_index + 1`, so without recording the settled paused object at
/// the delivery boundary this single-card cost records no link at all.
#[test]
fn a_paused_activation_cost_that_settles_in_exile_keeps_its_source_link() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Paid Card", "Filler One", "Filler Two"]);

    let lash = scenario
        .add_creature(P0, "Thought Lash", 0, 0)
        .as_enchantment()
        .from_oracle_text(THOUGHT_LASH)
        .id();
    scenario
        .add_creature(P0, "Optional Exile Redirect", 0, 0)
        .as_enchantment()
        .with_replacement_definition(optional_exile_redirect());

    let mut runner = scenario.build();
    let paid = runner.state().players[0]
        .library
        .iter()
        .copied()
        .next()
        .expect("reach-guard: library seeded");
    let index = exile_top_ability_index(&runner, lash);

    runner
        .act(GameAction::ActivateAbility {
            source_id: lash,
            ability_index: index,
        })
        .expect("activation must be accepted");

    // Reach-guard: the cost move really did pause on the replacement choice —
    // otherwise this test would pass through the unpaused path and prove nothing.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "reach-guard: the cost move must pause on the optional redirect; got {:?}",
        runner.state().waiting_for
    );

    // Decline the redirect, so the card settles in exile after the pause.
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("declining the optional redirect must be accepted");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&paid].zone,
        Zone::Exile,
        "reach-guard: declining the redirect leaves the paid card in exile"
    );
    assert!(
        runner
            .state()
            .cards_exiled_with_source_this_turn
            .get(&lash)
            .is_some_and(|cards| cards.contains(&paid)),
        "CR 406.6: the paid card must be indexed as exiled with its source even \
         when its cost move paused on a replacement choice; index = {:?}",
        runner.state().cards_exiled_with_source_this_turn.get(&lash)
    );
}
