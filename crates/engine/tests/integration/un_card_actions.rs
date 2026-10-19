//! Un-card actions (silver-border sets; no CR entry): say-it costs (Rod of
//! Spanking, Impounding Lot-Bot, Infernius Spawnington III) and the Gotcha
//! cycle (Touch and Go and the other ten Unhinged Gotcha cards).
//!
//! The rules shape underneath is ordinary: "unless that player says X" is a
//! CR 118.12a unless-payment whose cost is a spoken phrase, and "you may say
//! \"Gotcha!\" When you do, return ..." is a CR 603.12 reflexive trigger.

use engine::game::engine::apply;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{AbilityCost, Effect, StaticDefinition, TargetRef, TriggerDefinition};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::{StaticMode, TriggerCause};
use engine::types::triggers::TriggerMode;
use engine::types::un_actions::{GotchaCondition, PhysicalTell, UnAction, UnTell};
use engine::types::zones::Zone;

const ROD: &str = "{2}, {T}: This artifact deals 1 damage to target player. Then untap this artifact unless that player says \"Thank you, sir. May I have another?\"";
const TOUCH_AND_GO: &str = "Destroy target land.\nGotcha — If an opponent touches their face, you may say \"Gotcha!\" When you do, return this card from your graveyard to your hand.";

fn generic_mana(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn parse(name: &str, text: &str, types: &[&str]) -> engine::parser::oracle::ParsedAbilities {
    let types: Vec<String> = types.iter().map(|t| t.to_string()).collect();
    parse_oracle_text(text, name, &[], &types, &[])
}

fn gotcha_trigger(parsed: &engine::parser::oracle::ParsedAbilities) -> &TriggerDefinition {
    let mut gotchas = parsed
        .triggers
        .iter()
        .filter(|t| t.mode == TriggerMode::GotchaSaid);
    let trigger = gotchas
        .next()
        .unwrap_or_else(|| panic!("a GotchaSaid trigger is parsed from {parsed:#?}"));
    assert!(gotchas.next().is_none(), "exactly one Gotcha trigger");
    trigger
}

fn un_action(runner: &mut GameRunner, actor: engine::types::player::PlayerId, action: UnAction) {
    apply(runner.state_mut(), actor, GameAction::UnAction { action })
        .expect("Un actions are legal in any state");
}

/// Rod of Spanking on P0's side, P0 holding priority in main phase with {2}.
fn rod_scenario() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let rod = scenario
        .add_artifact_from_oracle(P0, "Rod of Spanking", ROD)
        .id();
    scenario.with_mana_pool(P0, generic_mana(2));
    (scenario.build(), rod)
}

/// Activate the Rod at P1 and resolve the stack until the engine stops.
fn spank_p1(runner: &mut GameRunner, rod: ObjectId) {
    let index = runner.state().objects[&rod]
        .abilities
        .iter()
        .position(|a| a.cost.is_some())
        .expect("the Rod has its activated ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: rod,
            ability_index: index,
        })
        .expect("activation is accepted");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(P1)),
            })
            .expect("P1 is a legal target");
    }
    for _ in 0..6 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            || runner.state().stack.is_empty()
        {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority passes");
    }
}

#[test]
fn rod_of_spanking_parses_an_unless_say_phrase_cost() {
    let parsed = parse("Rod of Spanking", ROD, &["Artifact"]);
    assert_eq!(parsed.abilities.len(), 1);
    let untap = parsed.abilities[0]
        .sub_ability
        .as_ref()
        .expect("the untap rider is a sub-ability");
    assert!(matches!(&*untap.effect, Effect::SetTapState { .. }));
    let unless = untap
        .unless_pay
        .as_ref()
        .expect("untap is gated by an unless cost");
    assert_eq!(
        unless.cost,
        AbilityCost::SayPhrase {
            phrase: "Thank you, sir. May I have another?".to_string()
        }
    );
}

#[test]
fn rod_of_spanking_offers_the_damaged_player_the_phrase_and_stays_tapped_when_said() {
    let (mut runner, rod) = rod_scenario();
    un_action(&mut runner, P1, UnAction::SetEnabled { enabled: true });
    spank_p1(&mut runner, rod);

    assert_eq!(
        runner.life(P1),
        19,
        "the Rod deals 1 damage to the target player"
    );
    let WaitingFor::UnlessPayment { player, cost, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "expected the damaged player's say-it prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P1, "only the player being dealt damage is asked");
    assert!(matches!(cost, AbilityCost::SayPhrase { .. }));

    let result = runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("saying the phrase is accepted");
    assert!(
        result.events.iter().any(|e| matches!(
            e,
            GameEvent::PhraseSaid { player, phrase }
                if *player == P1 && phrase == "Thank you, sir. May I have another?"
        )),
        "saying it is announced so every client can show it"
    );
    assert!(
        runner.state().objects[&rod].tapped,
        "said it: the Rod does not untap"
    );
}

#[test]
fn rod_of_spanking_untaps_when_the_phrase_is_declined() {
    let (mut runner, rod) = rod_scenario();
    un_action(&mut runner, P1, UnAction::SetEnabled { enabled: true });
    spank_p1(&mut runner, rod);
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("declining is accepted");
    assert!(
        !runner.state().objects[&rod].tapped,
        "didn't say it: the Rod untaps"
    );
}

#[test]
fn rod_of_spanking_never_prompts_a_player_who_did_not_opt_in() {
    let (mut runner, rod) = rod_scenario();
    spank_p1(&mut runner, rod);
    assert_eq!(
        runner.life(P1),
        19,
        "reach guard: the damage still happened"
    );
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "no prompt without opting in"
    );
    assert!(
        !runner.state().objects[&rod].tapped,
        "unsaid: the Rod untaps"
    );
}

#[test]
fn say_it_payments_parse_for_optional_you_may_say_cards() {
    let lot_bot = parse(
        "Impounding Lot-Bot",
        "When this creature enters, you may say \"INCARCERATE!\" in a robot voice. If you do, exile another target artifact or enchantment until this creature leaves the battlefield.",
        &["Artifact", "Creature"],
    );
    let infernius = parse(
        "Infernius Spawnington III, Esq.",
        "When this creature enters, you may say \"I'm here.\" If you do, it deals 3 damage to target player.",
        &["Creature"],
    );
    for (parsed, phrase) in [(lot_bot, "INCARCERATE!"), (infernius, "I'm here.")] {
        let trigger = parsed.triggers.first().expect("an ETB trigger");
        let execute = trigger.execute.as_ref().expect("trigger has an effect");
        assert!(
            matches!(
                &*execute.effect,
                Effect::PayCost { cost: AbilityCost::SayPhrase { phrase: p }, .. } if p == phrase
            ),
            "{phrase}: expected a SayPhrase payment, got {:?}",
            execute.effect
        );
        assert!(
            execute.sub_ability.is_some(),
            "{phrase}: the 'If you do' rider follows"
        );
    }
}

#[test]
fn every_gotcha_card_parses_to_a_typed_condition() {
    let cases: [(&str, &str, &[&str], GotchaCondition); 11] = [
        (
            "Touch and Go",
            TOUCH_AND_GO,
            &["Sorcery"],
            GotchaCondition::Physical(PhysicalTell::TouchFace),
        ),
        (
            "Cardpecker",
            "Flying\nGotcha — If an opponent touches the table with their hand, you may say \"Gotcha!\" When you do, return this card from your graveyard to your hand.",
            &["Creature"],
            GotchaCondition::Physical(PhysicalTell::TouchTable),
        ),
        (
            "Laughing Hyena",
            "Gotcha — If an opponent laughs, you may say \"Gotcha!\" When you do, return this card from your graveyard to your hand.",
            &["Creature"],
            GotchaCondition::Physical(PhysicalTell::Laugh),
        ),
        (
            "Stop That",
            "Target player discards a card.\nGotcha — If an opponent audibly flicks the cards in their hand, you may say \"Gotcha!\" When you do, return this card from your graveyard to your hand.",
            &["Sorcery"],
            GotchaCondition::Physical(PhysicalTell::FlickCards),
        ),
        (
            "Number Crunch",
            "Return target permanent to its owner's hand.\nGotcha — If an opponent says a number, you may say \"Gotcha!\" When you do, return this card from your graveyard to your hand.",
            &["Instant"],
            GotchaCondition::SaysNumber,
        ),
        (
            "Creature Guy",
            "Gotcha — If an opponent says \"creature\" or \"guy,\" you may say \"Gotcha!\" When you do, return Creature Guy from your graveyard to your hand.",
            &["Creature"],
            GotchaCondition::SaysAnyOf { words: vec!["creature".into(), "guy".into()] },
        ),
        (
            "Deal Damage",
            "Deal Damage deals 4 damage to any target.\nGotcha — If an opponent says \"deal\" or \"damage,\" you may say \"Gotcha!\" When you do, return Deal Damage from your graveyard to your hand.",
            &["Instant"],
            GotchaCondition::SaysAnyOf { words: vec!["deal".into(), "damage".into()] },
        ),
        (
            "Kill! Destroy!",
            "Destroy target nonblack creature.\nGotcha — If an opponent says \"kill\" or \"destroy,\" you may say \"Gotcha!\" When you do, return Kill! Destroy! from your graveyard to your hand.",
            &["Instant"],
            GotchaCondition::SaysAnyOf { words: vec!["kill".into(), "destroy".into()] },
        ),
        (
            "Save Life",
            "Choose one —\n• Target player gains 2½ life.\n• Prevent the next 2½ damage that would be dealt to target creature this turn.\nGotcha — If an opponent says \"save\" or \"life,\" you may say \"Gotcha!\" When you do, return Save Life from your graveyard to your hand.",
            &["Instant"],
            GotchaCondition::SaysAnyOf { words: vec!["save".into(), "life".into()] },
        ),
        (
            "Spell Counter",
            "Counter target spell.\nGotcha — If an opponent says \"spell\" or \"counter,\" you may say \"Gotcha!\" When you do, return Spell Counter from your graveyard to your hand.",
            &["Instant"],
            GotchaCondition::SaysAnyOf { words: vec!["spell".into(), "counter".into()] },
        ),
        (
            "Name Dropping",
            "Gotcha — If an opponent says a word that's in the name of a card in your graveyard, you may say \"Gotcha!\" When you do, return that card to your hand.",
            &["Enchantment"],
            GotchaCondition::SaysWordInGraveyardCardName,
        ),
    ];
    for (name, text, types, expected) in cases {
        let parsed = parse(name, text, types);
        let trigger = gotcha_trigger(&parsed);
        assert_eq!(trigger.gotcha_condition.as_ref(), Some(&expected), "{name}");
        let execute = trigger.execute.as_ref().expect("the 'When you do' effect");
        assert!(
            matches!(
                &*execute.effect,
                Effect::ChangeZone { .. } | Effect::Bounce { .. }
            ),
            "{name}: expected the return to hand, got {:?}",
            execute.effect
        );
    }
}

fn tell(runner: &mut GameRunner, actor: engine::types::player::PlayerId, tell: UnTell) {
    un_action(runner, actor, UnAction::PerformTell { tell });
}

/// P0 (opted in) holds Touch and Go in the graveyard; the table is at priority.
fn touch_and_go_scenario() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let card = scenario
        .add_spell_to_graveyard(P0, "Touch and Go", false)
        .from_oracle_text(TOUCH_AND_GO)
        .id();
    let mut runner = scenario.build();
    un_action(&mut runner, P0, UnAction::SetEnabled { enabled: true });
    (runner, card)
}

#[test]
fn touch_and_go_returns_to_hand_when_gotcha_is_called_after_the_opponent_touches_their_face() {
    let (mut runner, card) = touch_and_go_scenario();
    tell(&mut runner, P1, UnTell::Physical(PhysicalTell::TouchFace));
    let offers = runner.state().un_actions.gotcha_offers.clone();
    assert_eq!(offers.len(), 1, "the gesture opens one Gotcha window");
    assert_eq!((offers[0].player, offers[0].source_id), (P0, card));

    un_action(
        &mut runner,
        P0,
        UnAction::SayGotcha {
            source_id: card,
            returns: card,
        },
    );
    assert_eq!(
        runner.state().objects[&card].zone,
        Zone::Graveyard,
        "the return is a reflexive trigger: it waits on the stack"
    );
    assert_eq!(runner.state().stack.len(), 1);
    runner.resolve_top();
    assert_eq!(runner.state().objects[&card].zone, Zone::Hand);
    assert!(
        runner.state().un_actions.gotcha_offers.is_empty(),
        "the window is spent"
    );
}

#[test]
fn gotcha_is_not_offered_for_a_different_gesture_or_to_a_player_who_did_not_opt_in() {
    let (mut runner, card) = touch_and_go_scenario();
    tell(&mut runner, P1, UnTell::Physical(PhysicalTell::Laugh));
    assert!(
        runner.state().un_actions.gotcha_offers.is_empty(),
        "wrong gesture"
    );
    let denied = apply(
        runner.state_mut(),
        P0,
        GameAction::UnAction {
            action: UnAction::SayGotcha {
                source_id: card,
                returns: card,
            },
        },
    );
    assert!(denied.is_err(), "no open window, no Gotcha");

    // Reach guard: the same gesture DOES open a window once P0 has opted in.
    tell(&mut runner, P1, UnTell::Physical(PhysicalTell::TouchFace));
    assert_eq!(runner.state().un_actions.gotcha_offers.len(), 1);

    un_action(&mut runner, P0, UnAction::SetEnabled { enabled: false });
    assert!(
        runner.state().un_actions.gotcha_offers.is_empty(),
        "opting out closes the window"
    );
    tell(&mut runner, P1, UnTell::Physical(PhysicalTell::TouchFace));
    assert!(
        runner.state().un_actions.gotcha_offers.is_empty(),
        "not opted in: no window"
    );
}

#[test]
fn a_gotcha_window_closes_at_the_next_game_action() {
    let (mut runner, _card) = touch_and_go_scenario();
    tell(&mut runner, P1, UnTell::Physical(PhysicalTell::TouchFace));
    assert_eq!(runner.state().un_actions.gotcha_offers.len(), 1);
    runner
        .act(GameAction::PassPriority)
        .expect("priority passes");
    assert!(runner.state().un_actions.gotcha_offers.is_empty());
}

#[test]
fn a_player_cannot_gotcha_their_own_gesture() {
    let (mut runner, _card) = touch_and_go_scenario();
    tell(&mut runner, P0, UnTell::Physical(PhysicalTell::TouchFace));
    assert!(
        runner.state().un_actions.gotcha_offers.is_empty(),
        "only opponents' gestures count"
    );
    // Reach guard: the same gesture performed by an opponent produces an offer.
    tell(&mut runner, P1, UnTell::Physical(PhysicalTell::TouchFace));
    assert_eq!(
        runner.state().un_actions.gotcha_offers.len(),
        1,
        "opponent's gesture produces an offer"
    );
}

#[test]
fn spoken_gotchas_match_whole_words_and_numbers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let number_crunch = scenario
        .add_spell_to_graveyard(P0, "Number Crunch", true)
        .from_oracle_text("Return target permanent to its owner's hand.\nGotcha — If an opponent says a number, you may say \"Gotcha!\" When you do, return this card from your graveyard to your hand.")
        .id();
    let mut runner = scenario.build();
    un_action(&mut runner, P0, UnAction::SetEnabled { enabled: true });

    tell(
        &mut runner,
        P1,
        UnTell::Said {
            text: "sevens".to_string(),
        },
    );
    assert!(
        runner.state().un_actions.gotcha_offers.is_empty(),
        "'sevens' is not a number word"
    );
    tell(
        &mut runner,
        P1,
        UnTell::Said {
            text: "I have seven cards".to_string(),
        },
    );
    assert_eq!(runner.state().un_actions.gotcha_offers.len(), 1);
    un_action(
        &mut runner,
        P0,
        UnAction::SayGotcha {
            source_id: number_crunch,
            returns: number_crunch,
        },
    );
    runner.resolve_top();
    assert_eq!(runner.state().objects[&number_crunch].zone, Zone::Hand);
}

#[test]
fn name_dropping_returns_the_graveyard_card_whose_name_the_opponent_said() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let name_dropping = scenario
        .add_enchantment_from_oracle(
            P0,
            "Name Dropping",
            "Gotcha — If an opponent says a word that's in the name of a card in your graveyard, you may say \"Gotcha!\" When you do, return that card to your hand.",
        )
        .id();
    let bolt = scenario
        .add_spell_to_graveyard(P0, "Lightning Bolt", true)
        .id();
    let bear = scenario
        .add_spell_to_graveyard(P0, "Grizzly Bears", false)
        .id();
    let mut runner = scenario.build();
    un_action(&mut runner, P0, UnAction::SetEnabled { enabled: true });

    tell(
        &mut runner,
        P1,
        UnTell::Said {
            text: "Ouch, that BOLT hurt".to_string(),
        },
    );
    let offers = runner.state().un_actions.gotcha_offers.clone();
    assert_eq!(
        offers.len(),
        1,
        "only the Bolt's name contains a spoken word"
    );
    assert_eq!(
        (offers[0].source_id, offers[0].returns),
        (name_dropping, bolt)
    );

    un_action(
        &mut runner,
        P0,
        UnAction::SayGotcha {
            source_id: name_dropping,
            returns: bolt,
        },
    );
    runner.resolve_top();
    assert_eq!(
        runner.state().objects[&bolt].zone,
        Zone::Hand,
        "that card returns"
    );
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Graveyard,
        "other cards stay"
    );
}

#[test]
fn a_spoken_unless_payment_opens_a_verbal_gotcha_for_the_opponent() {
    let (mut runner_setup, rod) = {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let rod = scenario
            .add_artifact_from_oracle(P0, "Rod of Spanking", ROD)
            .id();
        let card = scenario
            .add_spell_to_graveyard(P0, "Kill! Destroy!", true)
            .from_oracle_text("Destroy target nonblack creature.\nGotcha — If an opponent says \"another\" or \"destroy,\" you may say \"Gotcha!\" When you do, return Kill! Destroy! from your graveyard to your hand.")
            .id();
        scenario.with_mana_pool(P0, generic_mana(2));
        (scenario.build(), (rod, card))
    };
    let (rod, card) = rod;
    un_action(
        &mut runner_setup,
        P0,
        UnAction::SetEnabled { enabled: true },
    );
    un_action(
        &mut runner_setup,
        P1,
        UnAction::SetEnabled { enabled: true },
    );
    spank_p1(&mut runner_setup, rod);
    assert!(
        runner_setup.state().un_actions.gotcha_offers.is_empty(),
        "reach guard: nothing was said yet"
    );
    runner_setup
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("saying the phrase is accepted");
    let offers = &runner_setup.state().un_actions.gotcha_offers;
    assert_eq!(offers.len(), 1, "P1 saying the phrase opens P0's Gotcha");
    assert_eq!((offers[0].player, offers[0].source_id), (P0, card));
}

const INFERNIUS: &str = "When this creature enters, you may say \"I'm here.\" If you do, it deals 3 damage to target player.";

/// Drive Infernius through the scenario runner's real ETB trigger (selecting P1
/// as the target) and return the runner at the first decision it asks for.
fn resolve_infernius_etb(opt_in: bool) -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let infernius = scenario
        .add_creature_to_hand_from_oracle(P0, "Infernius Spawnington III, Esq.", 6, 6, INFERNIUS)
        .id();
    let mut runner = scenario.build();
    if opt_in {
        un_action(&mut runner, P0, UnAction::SetEnabled { enabled: true });
    }
    runner.cast(infernius).commit();
    runner.advance_until_stack_empty();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "Infernius ETB trigger opens TriggerTargetSelection, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("target P1 selected");
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        && !runner.state().stack.is_empty()
    {
        runner.resolve_top();
    }
    runner
}

#[test]
fn infernius_deals_damage_only_when_its_controller_says_the_phrase() {
    let mut runner = resolve_infernius_etb(true);
    let WaitingFor::OptionalEffectChoice {
        player,
        description,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected the say-it prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    assert!(
        description
            .as_deref()
            .unwrap_or_default()
            .contains("\"I'm here.\""),
        "the prompt names the phrase: {description:?}"
    );
    let result = runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("saying it is accepted");
    assert!(result.events.iter().any(|e| matches!(
        e,
        GameEvent::PhraseSaid { player, phrase } if *player == P0 && phrase == "I'm here."
    )));
    assert_eq!(
        runner.life(P1),
        17,
        "'If you do': 3 damage to the target player"
    );

    let mut declined = resolve_infernius_etb(true);
    declined
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining is accepted");
    assert_eq!(declined.life(P1), 20, "unsaid: no damage");
}

#[test]
fn infernius_never_asks_a_player_who_did_not_opt_in() {
    let runner = resolve_infernius_etb(false);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "no say-it prompt without opting in, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.life(P1), 20, "unsaid: no damage");
}

#[test]
fn a_gotcha_holder_without_priority_can_still_call_it() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let card = scenario
        .add_spell_to_graveyard(P1, "Touch and Go", false)
        .from_oracle_text(TOUCH_AND_GO)
        .id();
    let mut runner = scenario.build();
    un_action(&mut runner, P1, UnAction::SetEnabled { enabled: true });
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "reach guard: P0 holds priority while P1 holds the Gotcha card"
    );

    tell(&mut runner, P0, UnTell::Physical(PhysicalTell::TouchFace));
    assert_eq!(runner.state().un_actions.gotcha_offers.len(), 1);
    un_action(
        &mut runner,
        P1,
        UnAction::SayGotcha {
            source_id: card,
            returns: card,
        },
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the reflexive trigger reaches the stack"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "priority stays with the player who held it"
    );
    runner.resolve_top();
    assert_eq!(runner.state().objects[&card].zone, Zone::Hand);
}

#[test]
fn the_menu_lists_only_the_gestures_some_gotcha_card_answers() {
    let (mut runner, _card) = touch_and_go_scenario();
    use engine::types::un_actions::UnTellKind;
    // `refresh_menu` runs at every action boundary; the opt-in action already ran one.
    assert_eq!(
        runner.state().un_actions.menu,
        vec![UnTellKind::Physical(PhysicalTell::TouchFace)],
        "Touch and Go only answers a touched face"
    );
    un_action(&mut runner, P0, UnAction::SetEnabled { enabled: false });
    assert!(
        runner.state().un_actions.menu.is_empty(),
        "nobody opted in: no menu"
    );
}

const LOT_BOT: &str = "When this creature enters, you may say \"INCARCERATE!\" in a robot voice. If you do, exile another target artifact or enchantment until this creature leaves the battlefield.";

#[test]
fn impounding_lot_bot_exiles_target_only_when_saying_the_phrase() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sol_ring = scenario
        .add_artifact_from_oracle(P1, "Sol Ring", "{T}: Add {C}{C}.")
        .id();
    let lot_bot = scenario
        .add_creature_to_hand_from_oracle(P0, "Impounding Lot-Bot", 3, 3, LOT_BOT)
        .id();
    let mut runner = scenario.build();
    un_action(&mut runner, P0, UnAction::SetEnabled { enabled: true });

    runner
        .cast(lot_bot)
        .target_object(sol_ring)
        .commit()
        .stop_at_optional()
        .resolve();

    let WaitingFor::OptionalEffectChoice {
        player,
        description,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected OptionalEffectChoice for robot voice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    assert!(
        description
            .as_deref()
            .unwrap_or_default()
            .contains("\"INCARCERATE!\""),
        "the prompt names the phrase: {description:?}"
    );

    let result = runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("saying phrase accepted");
    assert!(result.events.iter().any(|e| matches!(
        e,
        GameEvent::PhraseSaid { player, phrase } if *player == P0 && phrase == "INCARCERATE!"
    )));
    assert_eq!(
        runner.state().objects[&sol_ring].zone,
        Zone::Exile,
        "Sol Ring is exiled"
    );

    // Declining leaves the target on the battlefield.
    let mut scenario_decline = GameScenario::new();
    scenario_decline.at_phase(Phase::PreCombatMain);
    let sol_ring_2 = scenario_decline
        .add_artifact_from_oracle(P1, "Sol Ring", "{T}: Add {C}{C}.")
        .id();
    let lot_bot_2 = scenario_decline
        .add_creature_to_hand_from_oracle(P0, "Impounding Lot-Bot", 3, 3, LOT_BOT)
        .id();
    let mut runner_decline = scenario_decline.build();
    un_action(
        &mut runner_decline,
        P0,
        UnAction::SetEnabled { enabled: true },
    );
    runner_decline
        .cast(lot_bot_2)
        .target_object(sol_ring_2)
        .commit()
        .stop_at_optional()
        .resolve();
    runner_decline
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining is accepted");
    assert_eq!(
        runner_decline.state().objects[&sol_ring_2].zone,
        Zone::Battlefield,
        "unsaid: Sol Ring remains on battlefield"
    );

    // Opted-out player is never prompted and target remains on the battlefield.
    let mut scenario_opt_out = GameScenario::new();
    scenario_opt_out.at_phase(Phase::PreCombatMain);
    let sol_ring_3 = scenario_opt_out
        .add_artifact_from_oracle(P1, "Sol Ring", "{T}: Add {C}{C}.")
        .id();
    let lot_bot_3 = scenario_opt_out
        .add_creature_to_hand_from_oracle(P0, "Impounding Lot-Bot", 3, 3, LOT_BOT)
        .id();
    let mut runner_opt_out = scenario_opt_out.build();
    // P0 does not opt in.
    runner_opt_out
        .cast(lot_bot_3)
        .target_object(sol_ring_3)
        .commit()
        .stop_at_optional()
        .resolve();
    assert!(
        matches!(
            runner_opt_out.state().waiting_for,
            WaitingFor::Priority { .. }
        ),
        "no prompt without opt-in"
    );
    assert_eq!(
        runner_opt_out.state().objects[&sol_ring_3].zone,
        Zone::Battlefield,
        "opted out: Sol Ring remains on battlefield"
    );
}

#[test]
fn gotcha_triggers_are_not_doubled_by_trigger_doublers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let card = scenario
        .add_spell_to_graveyard(P0, "Touch and Go", false)
        .from_oracle_text(TOUCH_AND_GO)
        .id();
    scenario
        .add_creature(P0, "Trigger Doubler", 2, 2)
        .with_static_definition(StaticDefinition::new(StaticMode::DoubleTriggers {
            cause: TriggerCause::Any,
        }));
    let mut runner = scenario.build();
    un_action(&mut runner, P0, UnAction::SetEnabled { enabled: true });

    tell(&mut runner, P1, UnTell::Physical(PhysicalTell::TouchFace));
    assert_eq!(runner.state().un_actions.gotcha_offers.len(), 1);

    un_action(
        &mut runner,
        P0,
        UnAction::SayGotcha {
            source_id: card,
            returns: card,
        },
    );
    // CR 603.2d: reflexive/delayed Gotcha triggers are exempt from trigger doubling.
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Gotcha trigger must not be doubled"
    );
    runner.resolve_top();
    assert_eq!(runner.state().objects[&card].zone, Zone::Hand);
}
