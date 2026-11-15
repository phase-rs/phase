//! Issue #6301 — Force of Will cast for its alternative cost in response to an
//! opponent's spell, on the reporter's board shape.
//!
//! > You may pay 1 life and exile a blue card from your hand rather than pay
//! > this spell's mana cost.
//! > Counter target spell.
//!
//! The report (v0.31.0) hung on "Casting…" responding to Fatal Push. A later
//! engine run on v0.78.0 did not reproduce it, but that run had a single spell
//! on the stack, so the target was auto-chosen and no target prompt was ever
//! raised. The reporter's stack held TWO spells — their own Brainstorm beneath
//! the opponent's Fatal Push — with too little mana for the printed cost and a
//! single blue card eligible to pitch. This test drives exactly that shape.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::card_type::CoreType;
use engine::types::game_state::{
    CastPaymentMode, CastingVariant, StackEntry, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

// Verbatim Oracle text (Scryfall, 2026-09-15).
const FORCE_OF_WILL: &str = "You may pay 1 life and exile a blue card from your hand rather than pay this spell's mana cost.\n\
Counter target spell.";

/// Push a bare spell onto the stack. `core_type` lets the caller pick a
/// permanent spell whose resolution is observable on the battlefield.
fn push_spell(
    runner: &mut engine::game::scenario::GameRunner,
    controller: PlayerId,
    card: u64,
    name: &str,
    core_type: CoreType,
) -> ObjectId {
    let spell = engine::game::zones::create_object(
        runner.state_mut(),
        CardId(card),
        controller,
        name.to_string(),
        Zone::Stack,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&spell)
        .expect("stack spell exists")
        .card_types
        .core_types = vec![core_type];
    runner.state_mut().stack.push_back(StackEntry {
        id: spell,
        source_id: spell,
        controller,
        kind: StackEntryKind::Spell {
            card_id: CardId(card),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    spell
}

/// The reporter's board. Returns the runner and the ids a test needs.
struct Board {
    runner: engine::game::scenario::GameRunner,
    force: ObjectId,
    pitch: ObjectId,
    off_color: ObjectId,
    land: ObjectId,
    own_spell: ObjectId,
    opponent_spell: ObjectId,
}

fn reporters_board() -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let force = scenario
        .add_spell_to_hand_from_oracle(P0, "Force of Will", true, FORCE_OF_WILL)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 3,
        })
        .id();
    // The only card eligible to pitch: blue, and not Force of Will itself.
    let pitch = scenario
        .add_creature_to_hand(P0, "Blue Pitch Card", 5, 5)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 5,
        })
        .id();
    // A non-blue card the pitch cost must not accept.
    let off_color = scenario
        .add_creature_to_hand(P0, "Black Card", 1, 1)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();
    // One untapped blue source: the printed {3}{U}{U} is unaffordable, so the
    // alternative cost is the only way to cast (CR 118.9).
    let land = scenario.add_basic_land(P0, ManaColor::Blue);

    let mut runner = scenario.build();
    // Stack, bottom to top: P0's own spell, then P1's spell on top. An artifact
    // spell resolves onto the battlefield, so countering the WRONG spell is
    // observable as that object ending up in the graveyard instead.
    let own_spell = push_spell(
        &mut runner,
        P0,
        702,
        "Own Artifact Spell",
        CoreType::Artifact,
    );
    let opponent_spell = push_spell(&mut runner, P1, 701, "Opponent Instant", CoreType::Instant);

    Board {
        runner,
        force,
        pitch,
        off_color,
        land,
        own_spell,
        opponent_spell,
    }
}

#[test]
fn force_of_will_pitch_cast_picks_one_of_two_stack_spells_and_resolves() {
    let Board {
        mut runner,
        force,
        pitch,
        off_color,
        land,
        own_spell,
        opponent_spell,
    } = reporters_board();

    // CR 118.9a + CR 601.2c + CR 601.2h: announce the alternative cost, choose
    // the opponent's spell from the two legal stack targets, then pay 1 life
    // and exile the one eligible blue card.
    let outcome = runner
        .cast(force)
        .alternative_cast(AlternativeCastDecision::Alternative)
        .accept_optional()
        .target_objects(&[opponent_spell])
        .pay_cost_with(&[pitch])
        .resolve();

    // CR 701.6a: the targeted spell is countered into its owner's graveyard.
    assert_eq!(
        outcome.zone_of(opponent_spell),
        Zone::Graveyard,
        "Force of Will must counter the targeted opponent spell"
    );
    // The spell beneath was not the chosen target, so it still resolves. That the
    // prompt offered it at all is pinned by the next test.
    assert_eq!(
        outcome.zone_of(own_spell),
        Zone::Battlefield,
        "the untargeted spell beneath must still resolve"
    );
    assert_eq!(outcome.zone_of(force), Zone::Graveyard);
    // CR 118.9: the alternative cost was paid instead of the mana cost.
    assert_eq!(
        outcome.zone_of(pitch),
        Zone::Exile,
        "the blue card is pitched"
    );
    assert_eq!(
        outcome.zone_of(off_color),
        Zone::Hand,
        "the black card is not"
    );
    assert_eq!(outcome.life_delta(P0), -1, "1 life is paid");
    assert!(
        !outcome.state().objects[&land].tapped,
        "no mana is spent when the alternative cost is paid"
    );
    assert!(
        outcome.state().stack.is_empty(),
        "no stalled stack object remains"
    );
}

/// CR 601.2c: the multi-spell stack is the shape the v0.78.0 run never reached —
/// with one spell on the stack the target is chosen without a prompt. Here the
/// target prompt must actually be raised, and it must offer BOTH stack spells.
/// Driven action by action so the prompt itself is observed rather than hidden
/// inside the scenario driver.
#[test]
fn force_of_will_target_prompt_offers_both_stack_spells() {
    let Board {
        mut runner,
        force,
        pitch,
        own_spell,
        opponent_spell,
        ..
    } = reporters_board();
    let card_id = runner.state().objects[&force].card_id;

    let mut waiting = runner
        .act(GameAction::CastSpell {
            object_id: force,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Force of Will for its alternative cost must be offered")
        .waiting_for;

    let mut offered_targets = None;
    let mut seen = Vec::new();
    // Bounded: the full cast is a handful of prompts. A stall shows up as
    // running out of steps, with every prompt seen listed in the failure.
    for _ in 0..12 {
        seen.push(format!("{waiting:?}").chars().take(60).collect::<String>());
        let action = match &waiting {
            WaitingFor::Priority { .. } => break,
            WaitingFor::AlternativeCastChoice { .. } => GameAction::ChooseAlternativeCast {
                choice: AlternativeCastDecision::Alternative,
            },
            WaitingFor::OptionalCostChoice { .. } => GameAction::DecideOptionalCost { pay: true },
            WaitingFor::TargetSelection { selection, .. } => {
                offered_targets = Some(selection.current_legal_targets.clone());
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(opponent_spell)),
                }
            }
            WaitingFor::PayCost { choices, .. } => {
                assert_eq!(choices, &vec![pitch], "only the blue card may be pitched");
                GameAction::SelectCards { cards: vec![pitch] }
            }
            other => panic!("unexpected prompt while casting: {other:?}; prompts so far: {seen:?}"),
        };
        waiting = runner
            .act(action)
            .unwrap_or_else(|e| panic!("action rejected: {e:?}; prompts so far: {seen:?}"))
            .waiting_for;
    }

    assert!(
        matches!(waiting, WaitingFor::Priority { .. }),
        "the cast must reach Priority, not stall; prompts seen: {seen:?}"
    );
    let offered = offered_targets
        .unwrap_or_else(|| panic!("no target prompt was raised; prompts seen: {seen:?}"));
    for spell in [own_spell, opponent_spell] {
        assert!(
            offered.contains(&TargetRef::Object(spell)),
            "the target prompt must offer every spell on the stack; offered {offered:?}"
        );
    }
    assert_eq!(
        runner.state().stack.back().map(|entry| entry.id),
        Some(force),
        "Force of Will is on top of the stack, above both spells"
    );
}
