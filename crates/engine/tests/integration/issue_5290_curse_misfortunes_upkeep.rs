//! Issue #5290: Curse of Misfortunes upkeep search+attach on enchanted player.
//!
//! Discord report paired Lynde, Cheerful Tormentor with Curse of Misfortunes on
//! the same player's upkeep and questioned stack ordering. CR 603.3b lets the
//! controller choose trigger order when both are on the stack; this file locks
//! in the Misfortunes resolution path the reporter was trying to drive first:
//! accept the optional search, put a different-named Curse onto the battlefield
//! attached to the enchanted player without an Aura host prompt.

use engine::game::effects::attach::attach_to_player;
use engine::game::game_object::AttachTarget;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::trigger_index::reindex_object_triggers;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::WaitingFor;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::ObjectId;

const CURSE_OF_MISFORTUNES: &str =
    "Enchant player\nAt the beginning of your upkeep, you may search your library for a Curse card that doesn't have the same name as a Curse attached to enchanted player, put it onto the battlefield attached to that player, then shuffle.";

const LYNDE_ORACLE: &str =
    "Deathtouch\nWhenever a Curse is put into your graveyard from the battlefield, return it to the battlefield attached to you at the beginning of the next end step.\nAt the beginning of your upkeep, you may attach a Curse attached to you to one of your opponents. If you do, draw two cards.";

fn advance_until_optional_or_settled(runner: &mut GameRunner) {
    for _ in 0..64 {
        match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => return,
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            _ => {
                runner.act(GameAction::PassPriority).ok();
            }
        }
    }
}

fn advance_until_search_or_settled(runner: &mut GameRunner) {
    for _ in 0..64 {
        match &runner.state().waiting_for {
            WaitingFor::SearchChoice { .. } => return,
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            _ => {
                runner.act(GameAction::PassPriority).ok();
            }
        }
    }
}

fn search_attach_host_from_trigger(
    parsed: &engine::parser::oracle::ParsedAbilities,
) -> &TargetFilter {
    let trigger = parsed.triggers.first().expect("upkeep trigger");
    let execute = trigger.execute.as_ref().expect("execute");
    let change_zone = execute.sub_ability.as_ref().expect("search put-step sub");
    let attach = change_zone
        .sub_ability
        .as_ref()
        .expect("attach sub")
        .effect
        .as_ref();
    match attach {
        Effect::Attach { target, .. } => target,
        other => panic!("expected Attach sub, got {other:?}"),
    }
}

fn effect_is_unimplemented(effect: &Effect) -> bool {
    matches!(effect, Effect::Unimplemented { .. })
}

fn trigger_execute_is_supported(trigger: &engine::types::ability::TriggerDefinition) -> bool {
    trigger
        .execute
        .as_ref()
        .is_none_or(|exec| !effect_is_unimplemented(&exec.effect))
}

#[test]
fn curse_of_misfortunes_search_attach_host_parses_as_attached_to() {
    let parsed = parse_oracle_text(
        CURSE_OF_MISFORTUNES,
        "Curse of Misfortunes",
        &[],
        &["Enchantment".to_string()],
        &["Aura".to_string(), "Curse".to_string()],
    );
    assert_eq!(
        search_attach_host_from_trigger(&parsed),
        &TargetFilter::ParentTarget,
        "Misfortunes 'that player' attach host parses as ParentTarget"
    );
    assert!(
        parsed.triggers.iter().all(trigger_execute_is_supported),
        "Misfortunes upkeep trigger must parse to a supported effect"
    );
}

#[test]
fn lynde_oracle_parses_supported_triggers() {
    let parsed = parse_oracle_text(
        LYNDE_ORACLE,
        "Lynde, Cheerful Tormentor",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Warlock".to_string()],
    );
    assert_eq!(
        parsed.triggers.len(),
        2,
        "Lynde has two triggered abilities"
    );
    assert!(
        parsed.triggers.iter().all(trigger_execute_is_supported),
        "Lynde triggers must parse to supported effects: {:?}",
        parsed.triggers
    );
}

/// P0 controls Misfortunes attached to P0 (self-curse). At P0's upkeep, accept
/// the optional search and verify the found Curse enters attached to P0.
#[test]
fn curse_of_misfortunes_upkeep_search_attaches_to_enchanted_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);

    let misfortunes_id = {
        let mut builder = scenario.add_creature_from_oracle(
            P0,
            "Curse of Misfortunes",
            0,
            0,
            CURSE_OF_MISFORTUNES,
        );
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.with_keyword(Keyword::Enchant(TargetFilter::Player));
        builder.id()
    };

    let searched_curse_id = scenario
        .add_spell_to_library_top(P0, "Curse of Thirst", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura", "Curse"])
        .with_keyword(Keyword::Enchant(TargetFilter::Player))
        .id();

    for _ in 0..20 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;

    attach_to_player(runner.state_mut(), misfortunes_id, P0);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), misfortunes_id);

    runner.advance_to_upkeep();

    advance_until_optional_or_settled(&mut runner);
    match &runner.state().waiting_for {
        WaitingFor::OptionalEffectChoice { player, .. } => {
            assert_eq!(
                *player, P0,
                "Misfortunes optional search must prompt P0 (curse controller)"
            );
        }
        other => {
            panic!("expected OptionalEffectChoice after Misfortunes upkeep trigger, got {other:?}")
        }
    }

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept Misfortunes optional search");

    advance_until_search_or_settled(&mut runner);
    match &runner.state().waiting_for {
        WaitingFor::SearchChoice { player, cards, .. } => {
            assert_eq!(*player, P0, "Misfortunes search must be P0's library");
            assert!(
                cards.contains(&searched_curse_id),
                "Curse of Thirst must be a legal different-name Curse candidate"
            );
        }
        other => panic!("expected SearchChoice after accepting Misfortunes, got {other:?}"),
    }

    runner
        .act(GameAction::SelectCards {
            cards: vec![searched_curse_id],
        })
        .expect("submit Curse of Thirst from search");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&searched_curse_id].zone,
        Zone::Battlefield,
        "searched Curse must enter from the library"
    );
    assert_eq!(
        runner.state().objects[&searched_curse_id].attached_to,
        Some(AttachTarget::Player(P0)),
        "searched Curse must attach to enchanted player (P0) without host prompt"
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReturnAsAuraTarget { .. } | WaitingFor::TargetSelection { .. }
        ),
        "search put must not surface an Aura host prompt, got {:?}",
        runner.state().waiting_for
    );
}

/// Lynde + Misfortunes on P0's upkeep: both triggers must fire; controller may
/// order them (CR 603.3b). This test only asserts both land on the stack.
#[test]
fn lynde_and_misfortunes_both_trigger_on_controller_upkeep() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);

    let lynde_id = scenario
        .add_creature_from_oracle(P0, "Lynde, Cheerful Tormentor", 2, 4, LYNDE_ORACLE)
        .id();

    let misfortunes_id = {
        let mut builder = scenario.add_creature_from_oracle(
            P0,
            "Curse of Misfortunes",
            0,
            0,
            CURSE_OF_MISFORTUNES,
        );
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.with_keyword(Keyword::Enchant(TargetFilter::Player));
        builder.id()
    };

    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
    }

    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;

    attach_to_player(runner.state_mut(), misfortunes_id, P0);
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), misfortunes_id);

    runner.advance_to_upkeep();

    for _ in 0..64 {
        if !runner.state().stack.is_empty() {
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).ok();
            }
            _ => break,
        }
    }

    let lynde_triggers = runner
        .state()
        .stack
        .iter()
        .filter(|e| e.source_id == lynde_id)
        .count();
    let misfortunes_triggers = runner
        .state()
        .stack
        .iter()
        .filter(|e| e.source_id == misfortunes_id)
        .count();

    assert!(
        misfortunes_triggers >= 1,
        "Misfortunes must trigger at P0 upkeep"
    );
    assert!(
        lynde_triggers >= 1,
        "Lynde upkeep trigger must be on the stack with Misfortunes; stack sources={:?}",
        runner
            .state()
            .stack
            .iter()
            .map(|e| e.source_id)
            .collect::<Vec<_>>()
    );
}

// ─────────────────────────────────────────────────────────────────────────
// Issue #10143 — the NEGATED name comparison in Misfortunes' SEARCH filter:
// "search your library for a Curse card that doesn't have the same name as a
// Curse attached to enchanted player". CR 201.2a defines the
// comparison, CR 303.4b/303.4m the "enchanted player" referent, CR 109.2 the
// battlefield default for the reference population, and CR 701.23b the partial
// find a stated-quality search allows.
//
// Before the fix the base filter was an empty `Typed{}` (the subtype noun had
// been normalized away), so every library card was offered and the comparison
// was dropped without a warning.
// ─────────────────────────────────────────────────────────────────────────

struct CurseFixture {
    runner: GameRunner,
    /// The named Curse cards placed in P0's library, in the order requested.
    library_curses: Vec<ObjectId>,
}

/// P0-controlled Curse of Misfortunes attached to `enchanted`, one Aura per
/// `extra_curses` entry `(name, attached-to player)`, and a P0 library holding
/// the named Curse cards plus filler Plains.
fn build_curse_fixture(
    enchanted: PlayerId,
    extra_curses: &[(&str, PlayerId)],
    library_curses: &[&str],
) -> CurseFixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);

    let misfortunes_id = {
        let mut builder = scenario.add_creature_from_oracle(
            P0,
            "Curse of Misfortunes",
            0,
            0,
            CURSE_OF_MISFORTUNES,
        );
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.with_keyword(Keyword::Enchant(TargetFilter::Player));
        builder.id()
    };

    let extra_auras: Vec<(ObjectId, PlayerId)> = extra_curses
        .iter()
        .map(|(name, attached_to)| {
            let mut builder = scenario.add_creature(P0, name, 0, 0);
            builder.as_enchantment();
            builder.with_subtypes(vec!["Aura", "Curse"]);
            builder.with_keyword(Keyword::Enchant(TargetFilter::Player));
            (builder.id(), *attached_to)
        })
        .collect();

    let library_curses = library_curses
        .iter()
        .map(|name| {
            scenario
                .add_spell_to_library_top(P0, name, false)
                .as_enchantment()
                .with_subtypes(vec!["Aura", "Curse"])
                .with_keyword(Keyword::Enchant(TargetFilter::Player))
                .id()
        })
        .collect();

    for _ in 0..20 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;

    attach_to_player(runner.state_mut(), misfortunes_id, enchanted);
    for (aura_id, attached_to) in extra_auras {
        attach_to_player(runner.state_mut(), aura_id, attached_to);
    }
    evaluate_layers(runner.state_mut());
    reindex_object_triggers(runner.state_mut(), misfortunes_id);

    CurseFixture {
        runner,
        library_curses,
    }
}

/// Reaches Misfortunes' search prompt, accepting the optional search.
fn accept_optional_search(runner: &mut GameRunner) {
    runner.advance_to_upkeep();
    advance_until_optional_or_settled(runner);
    match &runner.state().waiting_for {
        WaitingFor::OptionalEffectChoice { player, .. } => {
            assert_eq!(*player, P0, "Misfortunes prompts its controller");
        }
        other => panic!("expected OptionalEffectChoice, got {other:?}"),
    }
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the optional search");
    advance_until_search_or_settled(runner);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::SearchChoice { .. }),
        "expected SearchChoice after accepting, got {:?}",
        runner.state().waiting_for
    );
}

fn search_choice_cards(runner: &GameRunner) -> Vec<ObjectId> {
    match &runner.state().waiting_for {
        WaitingFor::SearchChoice { cards, .. } => cards.clone(),
        other => panic!("expected SearchChoice, got {other:?}"),
    }
}

fn shuffle_events(events: &[GameEvent], player: PlayerId) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    player_id,
                    action: PlayerActionKind::ShuffledLibrary,
                    ..
                } if *player_id == player
            )
        })
        .count()
}

/// Passes priority until the trigger finishes resolving, collecting events.
fn pass_until_settled(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..40 {
        if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            continue;
        }
        if runner.state().stack.is_empty() {
            break;
        }
        match runner.act(GameAction::PassPriority) {
            Ok(result) => events.extend(result.events),
            Err(_) => break,
        }
    }
    events
}

#[test]
fn curse_of_misfortunes_search_excludes_same_name_curses_on_enchanted_player() {
    // Library: a second copy of the source (same name as the Curse attached to
    // the enchanted player) and a differently named Curse.
    let fixture = build_curse_fixture(P0, &[], &["Curse of Misfortunes", "Curse of Thirst"]);
    let same_named = fixture.library_curses[0];
    let thirst = fixture.library_curses[1];
    let mut runner = fixture.runner;

    accept_optional_search(&mut runner);
    // Discriminating: at base the empty filter offers every library card,
    // including the 20 Plains and the same-named copy.
    assert_eq!(
        search_choice_cards(&runner),
        vec![thirst],
        "only the differently named Curse is a legal candidate; same_named={same_named:?}"
    );

    // Positive reach-guard: the offered card is really put onto the battlefield
    // attached to the enchanted player.
    runner
        .act(GameAction::SelectCards {
            cards: vec![thirst],
        })
        .expect("submit Curse of Thirst from the search");
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&thirst].zone,
        Zone::Battlefield,
        "the searched Curse must enter the battlefield"
    );
    assert_eq!(
        runner.state().objects[&thirst].attached_to,
        Some(AttachTarget::Player(P0)),
        "it enters attached to the enchanted player"
    );
    assert_eq!(
        runner.state().objects[&same_named].zone,
        Zone::Library,
        "the same-named copy stays in the library"
    );
}

#[test]
fn curse_of_misfortunes_reference_is_scoped_to_curses_attached_to_the_enchanted_player() {
    // Source attached to P1 (the enchanted player). "Curse of Echoes" is also
    // attached to P1, so its name is in the reference population; "Curse of
    // Thirst" is attached to P0, so it is NOT.
    let fixture = build_curse_fixture(
        P1,
        &[("Curse of Echoes", P1), ("Curse of Thirst", P0)],
        &["Curse of Echoes", "Curse of Thirst"],
    );
    let echoes_copy = fixture.library_curses[0];
    let thirst_copy = fixture.library_curses[1];
    let mut runner = fixture.runner;

    accept_optional_search(&mut runner);
    let cards = search_choice_cards(&runner);
    // Positive reach-guard first: the copy of a Curse that is attached to the
    // NON-enchanted player is offered, so the negative below is not vacuous.
    assert!(
        cards.contains(&thirst_copy),
        "a Curse attached to P0 does not constrain a search scoped to P1, got {cards:?}"
    );
    assert!(
        !cards.contains(&echoes_copy),
        "a Curse attached to the enchanted player constrains the search by name, got {cards:?}"
    );
    // Discriminating: dropping `AttachedTo{Player{EnchantedPlayer}}` widens the
    // population to every Curse on the battlefield, which also excludes the
    // copy of Curse of Thirst.
    assert_eq!(
        cards,
        vec![thirst_copy],
        "the reference population is exactly the Curses attached to the enchanted player"
    );
}

#[test]
fn curse_of_misfortunes_search_allows_partial_find() {
    let fixture = build_curse_fixture(P0, &[], &["Curse of Thirst"]);
    let thirst = fixture.library_curses[0];
    let mut runner = fixture.runner;

    accept_optional_search(&mut runner);
    // Positive reach-guard: the stated-quality Curse is offered, so the empty
    // pick below is not vacuous.
    assert_eq!(
        search_choice_cards(&runner),
        vec![thirst],
        "the search must offer exactly the differently named Curse"
    );
    let WaitingFor::SearchChoice {
        allows_partial_find,
        ..
    } = &runner.state().waiting_for
    else {
        unreachable!("accept_optional_search asserted SearchChoice");
    };
    // Discriminating: at base the filter is empty (no stated quality), so
    // `allows_partial_find` is false and a zero-card selection is rejected.
    assert!(
        *allows_partial_find,
        "CR 701.23b: a stated-quality hidden-zone search may find zero cards"
    );

    // The shuffle happens inside the continuation, so events are collected from
    // the selection action and from every priority pass that follows it.
    let mut events = Vec::new();
    let selection = runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("find nothing — legal for a stated-quality search (CR 701.23b)");
    events.extend(selection.events);
    events.extend(pass_until_settled(&mut runner));

    assert_eq!(
        runner.state().objects[&thirst].zone,
        Zone::Library,
        "an empty find moves nothing"
    );
    assert!(
        shuffle_events(&events, P0) >= 1,
        "the chain still shuffles after an empty find (CR 609.3), events={events:?}"
    );
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the trigger must finish resolving, got {:?}",
        runner.state().waiting_for
    );
}
