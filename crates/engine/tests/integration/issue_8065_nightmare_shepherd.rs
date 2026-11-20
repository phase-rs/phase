//! Issue #8065 — Nightmare Shepherd's copy token must be a 1/1 Nightmare.
//!
//! Oracle (verified via Scryfall): "Flying\nWhenever another nontoken creature
//! you control dies, you may exile it. If you do, create a token that's a copy
//! of that creature, except it's 1/1 and it's a Nightmare in addition to its
//! other types."
//!
//! CR 707.9b: the `except it's 1/1` clause sets the copy's power and toughness;
//! CR 707.9d + CR 205.1b: `in addition to its other types` keeps the copied
//! types and adds the Nightmare subtype. Both parse as
//! `CopyTokenOf.additional_modifications` and must land on the created token
//! (CR 707.2 — a copy's exceptions are part of its copiable values).
//!
//! Discriminator: the donor is a 3/3, so a dropped P/T exception yields a 3/3
//! token. `copy_token_is_1_1_nightmare` fails on the pre-#9260 parser (the bare
//! `it's 1/1` body fell through the except-clause priority chain into the
//! fail-soft skip, leaving `[AddSubtype(Nightmare)]` as the only exception).

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{parse_mtgjson_mana_cost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const NIGHTMARE_SHEPHERD: &str = "Flying\nWhenever another nontoken creature you control dies, you may exile it. If you do, create a token that's a copy of that creature, except it's 1/1 and it's a Nightmare in addition to its other types.";

/// Drive the trigger the victim's death raises: answer every optional
/// ("you may exile it") prompt with `accept`, then let the chain (exile + copy
/// token creation) resolve. Returns how many optional prompts were answered, so
/// a caller can prove the trigger actually reached its decision.
fn answer_optionals_and_drain(runner: &mut GameRunner, accept: bool) -> u32 {
    let mut prompts = 0;
    for _ in 0..32 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ) {
            prompts += 1;
            runner
                .act(GameAction::DecideOptionalEffect { accept })
                .expect("answering the optional exile must succeed");
            continue;
        }
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            assert!(runner.state().resolution_stack.is_empty());
            assert!(runner.state().resolving_stack_entry.is_none());
            return prompts;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority must advance the removal and dies trigger");
    }
    panic!(
        "resolution did not settle; waiting_for = {:?}",
        runner.state().waiting_for
    );
}

/// CR 700.4 + CR 603.6: Murder's public cast produces the death and the dies trigger.
/// Returns the number of optional prompts its resolution presented.
fn kill(runner: &mut GameRunner, victim: ObjectId, murder: ObjectId, accept_exile: bool) -> u32 {
    runner
        .act(GameAction::CastSpell {
            object_id: murder,
            card_id: runner.state().objects[&murder].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Murder with its printed mana cost");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ));
    runner
        .act(GameAction::SelectTargets {
            targets: vec![engine::types::ability::TargetRef::Object(victim)],
        })
        .expect("Murder targets the donor");
    assert!(runner
        .state()
        .stack
        .iter()
        .any(|entry| entry.source_id == murder));
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    answer_optionals_and_drain(runner, accept_exile)
}

/// Battlefield tokens named `name`.
fn copy_tokens(state: &GameState, name: &str) -> Vec<ObjectId> {
    state
        .objects
        .iter()
        .filter(|(_, object)| object.zone == Zone::Battlefield)
        .filter(|(_, object)| object.is_token && object.name == name)
        .map(|(id, _)| *id)
        .collect()
}

fn shepherd_board() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Nightmare Shepherd", 4, 4, NIGHTMARE_SHEPHERD);
    // A 3/3 donor, so a token that keeps the donor's P/T is visibly wrong.
    let victim = scenario
        .add_creature(P0, "Centaur Courser", 3, 3)
        .with_subtypes(vec!["Centaur", "Warrior"])
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, "Destroy target creature.")
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{B}{B}"))
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]); 3],
    );
    (scenario.build(), victim, murder)
}

/// CR 707.9b + CR 707.9d: the copy is a 1/1 that is a Nightmare in addition to
/// the donor's own types.
#[test]
fn copy_token_is_1_1_nightmare() {
    let (mut runner, victim, murder) = shepherd_board();
    assert_eq!(
        kill(&mut runner, victim, murder, true),
        1,
        "CR 603.5: the dies trigger must present exactly one optional exile decision"
    );

    // Reach guard: the accepted optional exiled the dying creature, so the copy
    // source really is the exiled card and the token is not an unrelated object.
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Exile,
        "accepting \"you may exile it\" must exile the dead creature"
    );

    let tokens = copy_tokens(runner.state(), "Centaur Courser");
    assert_eq!(
        tokens.len(),
        1,
        "Nightmare Shepherd must create one copy token; waiting_for = {:?}",
        runner.state().waiting_for
    );
    let token = &runner.state().objects[&tokens[0]];
    // CR 707.9b: "except it's 1/1".
    assert_eq!(token.power, Some(1), "the copy must be a 1/1");
    assert_eq!(token.toughness, Some(1), "the copy must be a 1/1");
    assert_eq!(
        token.base_power,
        Some(1),
        "CR 707.9b: the exception is part of the copy's copiable values"
    );
    assert_eq!(token.base_toughness, Some(1));
    // CR 707.9d + CR 205.1b: "a Nightmare in addition to its other types".
    assert!(
        token.card_types.subtypes.iter().any(|s| s == "Nightmare"),
        "the copy must be a Nightmare, got {:?}",
        token.card_types.subtypes
    );
    assert!(
        token.card_types.core_types.contains(&CoreType::Creature),
        "the copy must still be a creature, got {:?}",
        token.card_types.core_types
    );
    assert!(token.card_types.subtypes.iter().any(|s| s == "Centaur"));
    assert!(token.card_types.subtypes.iter().any(|s| s == "Warrior"));
}

/// Paired control: declining "you may exile it" creates no token and leaves the
/// dead creature in the graveyard, so the row above is a real decision rather
/// than an unconditional copy.
#[test]
fn declining_the_exile_creates_no_token() {
    let (mut runner, victim, murder) = shepherd_board();
    // Reach guard: the trigger fired and presented the optional decision, so a
    // missing token below is the decline's effect rather than a trigger that
    // never resolved.
    assert_eq!(
        kill(&mut runner, victim, murder, false),
        1,
        "CR 603.5: the dies trigger must present the optional exile decision"
    );

    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Graveyard,
        "declining the optional must not exile the dead creature"
    );
    assert!(
        copy_tokens(runner.state(), "Centaur Courser").is_empty(),
        "no copy token may be created when the exile is declined"
    );
}
