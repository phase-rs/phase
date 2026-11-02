//! Issue #5242: Mystic Reflection's delayed replacement must make the next
//! creature/planeswalker entry copy the creature chosen when Mystic resolved.
//!
//! CR 616.1g + CR 701.7b: on a token-creation event, Mystic Reflection modifies
//! the tokens' entry, an event contained within the creation, so it can't be
//! chosen until the creation's replacements (Parallel Lives, Moonlit Meditation,
//! Halving Season) have been; a creation they leave with no token contains no
//! entry, so it does not use Mystic Reflection up (CR 614.3).

use engine::game::game_object::GameObject;
use engine::game::sba::check_state_based_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{ReplacementChoiceKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MYSTIC_REFLECTION: &str = "Choose target nonlegendary creature. The next time one or more creatures or planeswalkers enter this turn, they enter as copies of the chosen creature.";
// Verbatim Oracle text (MTGJSON).
const RAISE_THE_ALARM: &str = "Create two 1/1 white Soldier creature tokens.";
const SPROUT: &str = "Create a 1/1 green Saproling creature token.";
const PARALLEL_LIVES: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.";
const HALVING_SEASON: &str = "If an opponent would create one or more tokens, they create half that many of each of those kinds of tokens instead, rounded down.\nIf an opponent would put one or more counters on a permanent or player, they put half that many of each of those kinds of counters on that permanent or player instead, rounded down.";
const MOONLIT_MEDITATION: &str = "Enchant artifact or creature you control\nThe first time you would create one or more tokens each turn, you may instead create that many tokens that are copies of enchanted permanent.";
const GRUMGULLY: &str =
    "Each other non-Human creature you control enters with an additional +1/+1 counter on it.";
const HARDENED_SCALES: &str = "If one or more +1/+1 counters would be put on a creature you control, that many plus one +1/+1 counters are put on it instead.";
const BRANCHING_EVOLUTION: &str = "If one or more +1/+1 counters would be put on a creature you control, twice that many +1/+1 counters are put on that creature instead.";

/// How one expected `ReplacementChoice` prompt is answered.
#[derive(Debug, Clone, Copy)]
enum Pick {
    /// An ordering prompt: apply the candidate from this source next.
    Source(&'static str),
    /// An optional replacement's accept branch.
    Accept,
    /// An optional replacement's decline branch.
    Decline,
}

/// Answer exactly `prompts` — each P0's, offering the given number of options —
/// passing priority in between, until priority with an empty stack.
fn settle(runner: &mut GameRunner, prompts: &[(usize, Pick)]) {
    let mut prompts = prompts.iter();
    for _ in 0..200 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice {
                player,
                candidates,
                kind,
                ..
            } => {
                let &(offered, pick) = prompts.next().unwrap_or_else(|| {
                    panic!("unexpected extra prompt: {:?}", runner.state().waiting_for)
                });
                let names: Vec<&str> = candidates.iter().map(|c| c.source_name.as_str()).collect();
                assert_eq!(player, P0, "P0 makes the choice, offered {names:?}");
                assert_eq!(
                    candidates.len(),
                    offered,
                    "prompt {pick:?} offers {names:?}"
                );
                let index = match pick {
                    Pick::Source(name) => {
                        assert_eq!(kind, ReplacementChoiceKind::Order);
                        names
                            .iter()
                            .position(|n| *n == name)
                            .unwrap_or_else(|| panic!("{name} is offered: {names:?}"))
                    }
                    Pick::Accept => {
                        assert_eq!(kind, ReplacementChoiceKind::OptionalBranch);
                        0
                    }
                    Pick::Decline => {
                        assert_eq!(kind, ReplacementChoiceKind::OptionalBranch);
                        assert_eq!(candidates[1].description, "Decline");
                        1
                    }
                };
                runner
                    .act(GameAction::ChooseReplacement { index })
                    .expect("the choice is accepted");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority is legal");
            }
            other => panic!("unexpected decision {other:?}"),
        }
    }
    let unanswered: Vec<_> = prompts.collect();
    assert!(
        unanswered.is_empty(),
        "prompts never raised: {unanswered:?}"
    );
}

/// Battlefield tokens named `name`.
fn tokens_named<'a>(runner: &'a GameRunner, name: &str) -> Vec<&'a GameObject> {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| o.is_token && o.name == name)
        .collect()
}

/// Battlefield tokens with the subtype `subtype`.
fn tokens_of_subtype<'a>(runner: &'a GameRunner, subtype: &str) -> Vec<&'a GameObject> {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| o.is_token && o.card_types.subtypes.iter().any(|s| s == subtype))
        .collect()
}

fn plus_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

#[test]
fn mystic_reflection_makes_next_creature_token_copy_chosen_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let chosen = scenario
        .add_creature(P0, "Colossal Dreadmaw", 6, 6)
        .with_subtypes(vec!["Dinosaur"])
        .id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let token_spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Raise the Alarm",
            true,
            "Create a 1/1 white Soldier creature token.",
        )
        .id();

    let mut runner = scenario.build();
    runner.cast(mystic).target_object(chosen).resolve();
    runner.cast(token_spell).resolve();

    let copied_token = runner
        .state()
        .last_created_token_ids
        .first()
        .copied()
        .expect("token spell must create one token");
    let obj = runner
        .state()
        .objects
        .get(&copied_token)
        .expect("created token must exist");

    assert!(obj.is_token, "the entering object must still be a token");
    assert_eq!(obj.power, Some(6), "token must copy chosen creature power");
    assert_eq!(
        obj.toughness,
        Some(6),
        "token must copy chosen creature toughness"
    );
    assert!(
        obj.card_types.subtypes.iter().any(|s| s == "Dinosaur"),
        "token must copy chosen creature subtype, got {:?}",
        obj.card_types.subtypes
    );
}

#[test]
fn mystic_reflection_copies_every_token_in_one_multi_token_entry_event() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let chosen = scenario
        .add_creature(P0, "Colossal Dreadmaw", 6, 6)
        .with_subtypes(vec!["Dinosaur"])
        .id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let token_spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Raise the Alarm",
            true,
            "Create two 1/1 white Soldier creature tokens.",
        )
        .id();

    let mut runner = scenario.build();
    runner.cast(mystic).target_object(chosen).resolve();
    runner.cast(token_spell).resolve();

    // CR 614.12: One replacement effect modifies every permanent entering in
    // this simultaneous two-token event before the one-shot shield is consumed.
    assert_eq!(
        runner.state().last_created_token_ids.len(),
        2,
        "the production token pipeline must create both tokens in one event"
    );
    for token_id in &runner.state().last_created_token_ids {
        let obj = runner
            .state()
            .objects
            .get(token_id)
            .expect("each created token must exist");
        assert!(obj.is_token, "the entering object must remain a token");
        assert_eq!(obj.name, "Colossal Dreadmaw");
        assert_eq!(obj.power, Some(6));
        assert_eq!(obj.toughness, Some(6));
        assert!(
            obj.card_types.subtypes.iter().any(|s| s == "Dinosaur"),
            "every token in the entry event must copy the chosen subtype, got {:?}",
            obj.card_types.subtypes
        );
    }
}

#[test]
fn mystic_reflection_makes_next_nontoken_creature_enter_as_chosen_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let chosen = scenario
        .add_creature(P0, "Colossal Dreadmaw", 6, 6)
        .with_subtypes(vec!["Dinosaur"])
        .id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let entering = scenario
        .add_creature_to_hand(P0, "Runeclaw Bear", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    let follow_up = scenario
        .add_creature_to_hand(P0, "Silvercoat Lion", 2, 2)
        .with_subtypes(vec!["Cat"])
        .id();

    let mut runner = scenario.build();
    runner.cast(mystic).target_object(chosen).resolve();
    runner.cast(entering).resolve();

    let obj = runner
        .state()
        .objects
        .get(&entering)
        .expect("creature card must enter the battlefield");
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(!obj.is_token, "copied entering card must remain non-token");
    assert_eq!(obj.name, "Colossal Dreadmaw");
    assert_eq!(obj.power, Some(6));
    assert_eq!(obj.toughness, Some(6));
    assert!(
        obj.card_types.core_types.contains(&CoreType::Creature),
        "copied entering card must be a creature, got {:?}",
        obj.card_types.core_types
    );
    assert!(
        obj.card_types.subtypes.iter().any(|s| s == "Dinosaur"),
        "copied entering card must copy subtype, got {:?}",
        obj.card_types.subtypes
    );

    runner.cast(follow_up).resolve();
    let second = runner
        .state()
        .objects
        .get(&follow_up)
        .expect("follow-up creature must enter the battlefield");
    assert_eq!(second.name, "Silvercoat Lion");
    assert_eq!(second.power, Some(2));
    assert_eq!(second.toughness, Some(2));
    assert!(
        second.card_types.subtypes.iter().any(|s| s == "Cat"),
        "one-shot replacement must be consumed before the next entry"
    );
}

#[test]
fn mystic_reflection_makes_next_planeswalker_enter_as_chosen_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let chosen = scenario
        .add_creature(P0, "Colossal Dreadmaw", 6, 6)
        .with_subtypes(vec!["Dinosaur"])
        .id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let entering = scenario
        .add_creature_to_hand(P0, "Jace Beleren", 0, 0)
        .as_planeswalker_with_loyalty("Jace", 3)
        .id();
    let follow_up = scenario
        .add_creature_to_hand(P0, "Mesa Lynx", 2, 1)
        .with_subtypes(vec!["Cat"])
        .id();

    let mut runner = scenario.build();
    runner.cast(mystic).target_object(chosen).resolve();
    runner.cast(entering).resolve();

    let obj = runner
        .state()
        .objects
        .get(&entering)
        .expect("planeswalker card must enter the battlefield");
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(!obj.is_token, "copied entering card must remain non-token");
    assert_eq!(obj.name, "Colossal Dreadmaw");
    assert_eq!(obj.power, Some(6));
    assert_eq!(obj.toughness, Some(6));
    assert!(
        obj.card_types.core_types.contains(&CoreType::Creature),
        "planeswalker entry must copy creature card type, got {:?}",
        obj.card_types.core_types
    );
    assert!(
        !obj.card_types.core_types.contains(&CoreType::Planeswalker),
        "planeswalker entry must not retain its original card type"
    );
    assert!(
        obj.card_types.subtypes.iter().any(|s| s == "Dinosaur"),
        "planeswalker entry must copy subtype, got {:?}",
        obj.card_types.subtypes
    );
    assert_eq!(
        obj.loyalty, None,
        "copied creature characteristics have no loyalty"
    );
    assert_eq!(
        obj.counters.get(&CounterType::Loyalty).copied(),
        None,
        "intrinsic planeswalker counters must be retargeted to copied characteristics"
    );

    runner.cast(follow_up).resolve();
    let second = runner
        .state()
        .objects
        .get(&follow_up)
        .expect("follow-up creature must enter the battlefield");
    assert_eq!(second.name, "Mesa Lynx");
    assert_eq!(second.power, Some(2));
    assert_eq!(second.toughness, Some(1));
    assert!(
        second.card_types.subtypes.iter().any(|s| s == "Cat"),
        "one-shot replacement must be consumed before the next entry"
    );
}

#[test]
fn mystic_reflection_uses_lki_when_chosen_token_ceases_to_exist() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let source_token_spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Bestial Menace",
            true,
            "Create a 3/3 green Elephant creature token.",
        )
        .id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let token_spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Raise the Alarm",
            true,
            "Create a 1/1 white Soldier creature token.",
        )
        .id();

    let mut runner = scenario.build();
    runner.cast(source_token_spell).resolve();
    let chosen_token = runner
        .state()
        .last_created_token_ids
        .first()
        .copied()
        .expect("source token spell must create a token");

    runner.cast(mystic).target_object(chosen_token).resolve();

    let mut events = Vec::new();
    move_to_zone(
        runner.state_mut(),
        chosen_token,
        Zone::Graveyard,
        &mut events,
    );
    check_state_based_actions(runner.state_mut(), &mut events);
    assert!(
        !runner.state().objects.contains_key(&chosen_token),
        "the chosen token must cease to exist before the replacement applies"
    );

    runner.cast(token_spell).resolve();

    let copied_token = runner
        .state()
        .last_created_token_ids
        .first()
        .copied()
        .expect("second token spell must create one token");
    let obj = runner
        .state()
        .objects
        .get(&copied_token)
        .expect("created token must exist");

    assert_eq!(obj.power, Some(3), "token must copy LKI power");
    assert_eq!(obj.toughness, Some(3), "token must copy LKI toughness");
    assert!(
        obj.card_types.subtypes.iter().any(|s| s == "Elephant"),
        "token must copy LKI subtype, got {:?}",
        obj.card_types.subtypes
    );
}

/// Grizzly Bears enchanted by Moonlit Meditation, Colossal Dreadmaw chosen by
/// Mystic Reflection, then Raise the Alarm cast up to Moonlit Meditation's
/// optional prompt.
fn moonlit_and_reflection_board() -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let dreadmaw = scenario.add_creature(P0, "Colossal Dreadmaw", 6, 6).id();
    let moonlit = scenario
        .add_enchantment_from_oracle(P0, "Moonlit Meditation", MOONLIT_MEDITATION)
        .id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Raise the Alarm", true, RAISE_THE_ALARM)
        .id();
    let mut runner = scenario.build();
    runner.attach_as_bestowed_aura(moonlit, bears);
    runner.cast(mystic).target_object(dreadmaw).resolve();
    runner.cast(spell).resolve();
    runner
}

/// CR 616.1g + CR 701.7a + CR 614.3: Moonlit Meditation, a creation replacement,
/// is chosen first: the event becomes two copies of Grizzly Bears, and the
/// original two Soldiers are not created. Those copies then enter, and Mystic
/// Reflection makes them copies of Colossal Dreadmaw. The zeroed original event
/// put nothing onto the battlefield, so it did not use Mystic Reflection up.
#[test]
fn mystic_reflection_copies_moonlit_meditation_copies() {
    let mut runner = moonlit_and_reflection_board();
    settle(&mut runner, &[(2, Pick::Accept)]);

    assert_eq!(
        tokens_named(&runner, "Colossal Dreadmaw").len(),
        2,
        "Moonlit Meditation's two copies enter as Dreadmaw copies"
    );
    assert!(tokens_named(&runner, "Grizzly Bears").is_empty());
    assert!(tokens_of_subtype(&runner, "Soldier").is_empty());
}

/// Sibling: Moonlit Meditation declined, so the two Soldiers enter and Mystic
/// Reflection makes them copies of Colossal Dreadmaw.
#[test]
fn mystic_reflection_after_declined_moonlit_soldiers_become_copies() {
    let mut runner = moonlit_and_reflection_board();
    settle(&mut runner, &[(2, Pick::Decline)]);

    assert_eq!(tokens_named(&runner, "Colossal Dreadmaw").len(), 2);
    assert!(tokens_of_subtype(&runner, "Soldier").is_empty());
    assert!(tokens_named(&runner, "Grizzly Bears").is_empty());
}

/// CR 701.7a + CR 614.3: an opponent's Halving Season halves Sprout's one
/// Saproling to none, so no creature enters and Mystic Reflection ("the next
/// time one or more creatures or planeswalkers enter this turn") is not used up.
/// The next creature cast, Silvercoat Lion, enters as a copy of the chosen
/// Colossal Dreadmaw.
#[test]
fn mystic_reflection_waits_when_no_token_is_created() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dreadmaw = scenario.add_creature(P0, "Colossal Dreadmaw", 6, 6).id();
    let halving = scenario
        .add_enchantment_from_oracle(P1, "Halving Season", HALVING_SEASON)
        .id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let sprout = scenario
        .add_spell_to_hand_from_oracle(P0, "Sprout", true, SPROUT)
        .id();
    let lion = scenario
        .add_creature_to_hand(P0, "Silvercoat Lion", 2, 2)
        .with_subtypes(vec!["Cat"])
        .id();
    let mut runner = scenario.build();
    runner.cast(mystic).target_object(dreadmaw).resolve();
    let sprout_events = runner.cast(sprout).resolve().events().to_vec();
    settle(&mut runner, &[]);

    // Reach guards: Sprout resolved, and Halving Season applied to its token.
    assert_eq!(runner.state().objects[&sprout].zone, Zone::Graveyard);
    assert!(
        sprout_events.iter().any(|e| matches!(
            e,
            GameEvent::ReplacementApplied { source_id, .. } if *source_id == halving
        )),
        "Halving Season applied to Sprout's token"
    );
    assert!(tokens_of_subtype(&runner, "Saproling").is_empty());

    runner.cast(lion).resolve();
    let lion_obj = &runner.state().objects[&lion];
    assert_eq!(lion_obj.zone, Zone::Battlefield);
    assert_eq!(lion_obj.name, "Colossal Dreadmaw");
    assert_eq!(lion_obj.power, Some(6));
}

/// CR 616.1g + CR 614.1a: Parallel Lives doubles Raise the Alarm's two Soldiers
/// to four before Mystic Reflection, a replacement of their entry, applies; all
/// four enter as copies of Colossal Dreadmaw.
#[test]
fn mystic_reflection_after_parallel_lives_copies_all() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dreadmaw = scenario.add_creature(P0, "Colossal Dreadmaw", 6, 6).id();
    scenario.add_enchantment_from_oracle(P0, "Parallel Lives", PARALLEL_LIVES);
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Raise the Alarm", true, RAISE_THE_ALARM)
        .id();
    let mut runner = scenario.build();
    runner.cast(mystic).target_object(dreadmaw).resolve();
    runner.cast(spell).resolve();
    settle(&mut runner, &[]);

    assert_eq!(tokens_named(&runner, "Colossal Dreadmaw").len(), 4);
    assert!(tokens_of_subtype(&runner, "Soldier").is_empty());
}

/// CR 707.2 + CR 608.2c: both Soldiers enter as Grizzly Bears copies on the
/// plain token route, and each copy's Grumgully counter pauses for its ordering.
/// After the second pause the created-token referent still names both copies,
/// in order, not only the last one.
#[test]
fn mystic_reflection_paused_copy_batch_keeps_whole_referent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Grumgully, the Generous", 3, 3, GRUMGULLY);
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES);
    scenario.add_enchantment_from_oracle(P0, "Branching Evolution", BRANCHING_EVOLUTION);
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Raise the Alarm", true, RAISE_THE_ALARM)
        .id();
    let mut runner = scenario.build();
    runner.cast(mystic).target_object(bears).resolve();
    runner.cast(spell).resolve();
    settle(
        &mut runner,
        &[
            (2, Pick::Source("Hardened Scales")),
            (2, Pick::Source("Hardened Scales")),
        ],
    );

    let copies: Vec<ObjectId> = tokens_named(&runner, "Grizzly Bears")
        .iter()
        .map(|o| o.id)
        .collect();
    assert_eq!(copies.len(), 2, "both Soldiers enter as Bears copies");
    for &id in &copies {
        // Grumgully's one counter, Hardened Scales first: (1 + 1) × 2.
        assert_eq!(plus_counters(&runner, id), 4);
    }
    assert_eq!(runner.state().last_created_token_ids, copies);
}
