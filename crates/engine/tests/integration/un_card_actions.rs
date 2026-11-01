//! Un-card actions (silver-border sets; no CR entry): say-it costs (Rod of
//! Spanking, Impounding Lot-Bot, Infernius Spawnington III) and the Gotcha
//! cycle (Touch and Go and the other ten Unhinged Gotcha cards).
//!
//! The rules shape underneath is ordinary: "unless that player says X" is a
//! CR 118.12a unless-payment whose cost is a spoken phrase, and "you may say
//! \"Gotcha!\" When you do, return ..." is a CR 603.12 reflexive trigger.

use engine::game::engine::{apply, EngineError};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{AbilityCost, Effect, StaticDefinition, TargetRef, TriggerDefinition};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::{StaticMode, TriggerCause};
use engine::types::triggers::TriggerMode;
use engine::types::un_actions::{GotchaCondition, PhysicalTell, UnAction, UnTell};
use engine::types::zones::Zone;

const ROD: &str = "{2}, {T}: This artifact deals 1 damage to target player. Then untap this artifact unless that player says \"Thank you, sir. May I have another?\"";
const TOUCH_AND_GO: &str = "Destroy target land.\nGotcha — If an opponent touches their face, you may say \"Gotcha!\" When you do, return this card from your graveyard to your hand.";
const NAME_DROPPING: &str = "Gotcha — If an opponent says a word that's in the name of a card in your graveyard, you may say \"Gotcha!\" When you do, return that card to your hand.";
const FELIDAR_GUARDIAN: &str = "When this creature enters, you may exile another target permanent you control, then return that card to the battlefield under its owner's control.";

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

/// The engine finished the action: priority is settled and nothing waits on the stack.
fn assert_settled(runner: &GameRunner, context: &str) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{context}: expected settled priority, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "{context}: the stack is empty"
    );
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
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
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
    un_action(&mut runner, P1, UnAction::SetEnabled { enabled: true });
    tell(&mut runner, P1, UnTell::Physical(PhysicalTell::TouchFace));
    let offers = runner.state().un_actions.gotcha_offers.clone();
    assert_eq!(offers.len(), 1, "the gesture opens one Gotcha window");
    assert_eq!(
        (offers[0].player, offers[0].source.reference.object_id),
        (P0, card)
    );

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
fn a_player_who_did_not_opt_in_cannot_perform_a_tell() {
    let (mut runner, _card) = touch_and_go_scenario();
    un_action(&mut runner, P0, UnAction::SetEnabled { enabled: true });
    let denied = apply(
        runner.state_mut(),
        P1,
        GameAction::UnAction {
            action: UnAction::PerformTell {
                tell: UnTell::Physical(PhysicalTell::TouchFace),
            },
        },
    );
    assert!(
        denied.is_err(),
        "P1 never opted in, so the tell is rejected"
    );
    assert!(
        runner.state().un_actions.gotcha_offers.is_empty()
            && runner.state().un_actions.pending_offers.is_empty(),
        "a rejected tell opens no Gotcha window"
    );

    // Reach guard: once P1 opts in, the same tell opens P0's window.
    un_action(&mut runner, P1, UnAction::SetEnabled { enabled: true });
    tell(&mut runner, P1, UnTell::Physical(PhysicalTell::TouchFace));
    assert_eq!(runner.state().un_actions.gotcha_offers.len(), 1);
}

#[test]
fn gotcha_is_not_offered_for_a_different_gesture_or_to_a_player_who_did_not_opt_in() {
    let (mut runner, card) = touch_and_go_scenario();
    un_action(&mut runner, P1, UnAction::SetEnabled { enabled: true });
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
    un_action(&mut runner, P1, UnAction::SetEnabled { enabled: true });
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
    un_action(&mut runner, P1, UnAction::SetEnabled { enabled: true });
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
fn eliminated_player_cannot_perform_tells_or_open_gotcha_offers_in_multiplayer() {
    const P2: engine::types::player::PlayerId = engine::types::player::PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let card = scenario
        .add_spell_to_graveyard(P0, "Touch and Go", false)
        .from_oracle_text(TOUCH_AND_GO)
        .id();
    let mut runner = scenario.build();
    un_action(&mut runner, P0, UnAction::SetEnabled { enabled: true });
    un_action(&mut runner, P1, UnAction::SetEnabled { enabled: true });
    un_action(&mut runner, P2, UnAction::SetEnabled { enabled: true });

    // P1 concedes; multiplayer game continues between P0 and P2 per CR 800.4.
    runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("P1 concedes");
    assert!(
        !engine::game::players::is_alive(runner.state(), P1),
        "P1 has left the game"
    );
    assert!(
        engine::game::players::is_alive(runner.state(), P2),
        "P2 is still alive in the game"
    );

    // CR 102.1, CR 800.4: departed P1 cannot take game actions or produce rules-relevant tells.
    let tell_result = apply(
        runner.state_mut(),
        P1,
        GameAction::UnAction {
            action: UnAction::PerformTell {
                tell: UnTell::Physical(PhysicalTell::TouchFace),
            },
        },
    );
    assert!(
        matches!(&tell_result, Err(EngineError::InvalidAction(message)) if message == "Player has left the game"),
        "eliminated player cannot perform tells, got {tell_result:?}"
    );
    assert!(
        runner.state().un_actions.gotcha_offers.is_empty(),
        "eliminated player's tell opens no Gotcha offers"
    );

    // Departed P1 also cannot say Gotcha.
    let gotcha_result = apply(
        runner.state_mut(),
        P1,
        GameAction::UnAction {
            action: UnAction::SayGotcha {
                source_id: card,
                returns: card,
            },
        },
    );
    assert!(
        matches!(&gotcha_result, Err(EngineError::InvalidAction(message)) if message == "Player has left the game"),
        "eliminated player cannot say Gotcha, got {gotcha_result:?}"
    );

    // Paired control: living opponent P2 performs the tell and opens P0's Touch and Go offer.
    tell(&mut runner, P2, UnTell::Physical(PhysicalTell::TouchFace));
    let offers = runner.state().un_actions.gotcha_offers.clone();
    assert_eq!(
        offers.len(),
        1,
        "living opponent's tell opens one Gotcha window"
    );
    assert_eq!(
        (offers[0].player, offers[0].source.reference.object_id),
        (P0, card),
        "the offer is for P0's Touch and Go"
    );

    // Living P0 says Gotcha, putting the reflexive trigger on the stack.
    un_action(
        &mut runner,
        P0,
        UnAction::SayGotcha {
            source_id: card,
            returns: card,
        },
    );
    assert_eq!(runner.state().stack.len(), 1);
    runner.resolve_top();
    assert_eq!(
        runner.state().objects[&card].zone,
        Zone::Hand,
        "Touch and Go returned to P0's hand"
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
    un_action(&mut runner, P1, UnAction::SetEnabled { enabled: true });

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
    // A digit run wider than `u32` is still a number.
    tell(
        &mut runner,
        P1,
        UnTell::Said {
            text: "99999999999999999999".to_string(),
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
    un_action(&mut runner, P1, UnAction::SetEnabled { enabled: true });

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
        (
            offers[0].source.reference.object_id,
            offers[0].returns.reference.object_id
        ),
        (name_dropping, bolt),
        "the offer returns the card whose name was spoken"
    );
    assert_eq!(offers[0].returns.expected_zone, Zone::Graveyard);

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
        // Printed Name Dropping text; the graveyard card is a synthetic fixture
        // whose name shares a word ("another") with the Rod's spoken phrase.
        let card = scenario
            .add_enchantment_from_oracle(
                P0,
                "Name Dropping",
                "Gotcha — If an opponent says a word that's in the name of a card in your graveyard, you may say \"Gotcha!\" When you do, return that card to your hand.",
            )
            .id();
        scenario.add_spell_to_graveyard(P0, "Another Round", true);
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
    assert_eq!(
        (offers[0].player, offers[0].source.reference.object_id),
        (P0, card)
    );
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
    assert_settled(&runner, "Infernius accepted");

    let mut declined = resolve_infernius_etb(true);
    assert!(
        matches!(
            declined.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player, .. } if player == P0
        ),
        "the optional say-it prompt appears before it is declined, got {:?}",
        declined.state().waiting_for
    );
    declined
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining is accepted");
    assert_eq!(declined.life(P1), 20, "unsaid: no damage");
    assert_settled(&declined, "Infernius declined");
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
    assert_settled(&runner, "Infernius opted out");
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
    un_action(&mut runner, P0, UnAction::SetEnabled { enabled: true });
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
    assert_settled(&runner, "Lot-Bot accepted");

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
    assert_settled(&runner_decline, "Lot-Bot declined");

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
    assert_settled(&runner_opt_out, "Lot-Bot opted out");
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
    un_action(&mut runner, P1, UnAction::SetEnabled { enabled: true });

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

/// Name Dropping with P0's Lightning Bolt in `zone`, and Rod of Spanking
/// waiting on P1's spoken payment. P0 and P1 are both opted in.
fn name_dropping_during_unless_prompt(bolt_zone: Zone) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let rod = scenario
        .add_artifact_from_oracle(P0, "Rod of Spanking", ROD)
        .id();
    let name_dropping = scenario
        .add_enchantment_from_oracle(P0, "Name Dropping", NAME_DROPPING)
        .id();
    let bolt = match bolt_zone {
        Zone::Graveyard => scenario
            .add_spell_to_graveyard(P0, "Lightning Bolt", true)
            .id(),
        _ => scenario.add_spell_to_hand(P0, "Lightning Bolt", true).id(),
    };
    scenario.with_mana_pool(P0, generic_mana(2));
    let mut runner = scenario.build();
    un_action(&mut runner, P0, UnAction::SetEnabled { enabled: true });
    un_action(&mut runner, P1, UnAction::SetEnabled { enabled: true });
    spank_p1(&mut runner, rod);
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "reach guard: P1's payment decision is pending, got {:?}",
        runner.state().waiting_for
    );
    (runner, name_dropping, bolt)
}

#[test]
fn a_tell_matches_cards_in_the_graveyard_when_it_is_made_not_when_announced() {
    // CR 603.10: the word "bolt" is said while Lightning Bolt is still in hand;
    // the discard-like move that follows must not retroactively match it.
    let (mut runner, _name_dropping, bolt) = name_dropping_during_unless_prompt(Zone::Hand);
    tell(
        &mut runner,
        P1,
        UnTell::Said {
            text: "bolt".to_string(),
        },
    );
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), bolt, Zone::Graveyard, &mut events);
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("declining the payment is accepted");
    assert_settled(&runner, "after the prompt");
    assert!(
        runner.state().un_actions.gotcha_offers.is_empty(),
        "a card that was not in the graveyard when the word was said is never offered"
    );
}

#[test]
fn a_tell_made_during_a_decision_is_announced_once_priority_settles() {
    let (mut runner, name_dropping, bolt) = name_dropping_during_unless_prompt(Zone::Graveyard);
    tell(
        &mut runner,
        P1,
        UnTell::Said {
            text: "bolt".to_string(),
        },
    );
    assert!(
        runner.state().un_actions.gotcha_offers.is_empty(),
        "the announcement waits for the decision to finish"
    );
    for _ in 0..5 {
        tell(
            &mut runner,
            P1,
            UnTell::Said {
                text: "bolt".to_string(),
            },
        );
    }
    assert_eq!(
        runner.state().un_actions.pending_offers.len(),
        1,
        "repeated gestures coalesce into one pending offer per card"
    );
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("declining the payment is accepted");
    let offers = &runner.state().un_actions.gotcha_offers;
    assert_eq!(offers.len(), 1);
    assert_eq!(
        (
            offers[0].source.reference.object_id,
            offers[0].returns.reference.object_id
        ),
        (name_dropping, bolt)
    );
    assert_eq!(offers[0].returns.expected_zone, Zone::Graveyard);
    assert!(runner.state().un_actions.pending_offers.is_empty());
}

/// P0 controls Name Dropping with Lightning Bolt in the graveyard and has cast
/// Felidar Guardian, whose enters trigger targeting Name Dropping is at its
/// "you may" prompt. P1 says "bolt" during that prompt, so the offer is queued.
fn name_dropping_told_during_felidar_guardian_prompt() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let name_dropping = scenario
        .add_enchantment_from_oracle(P0, "Name Dropping", NAME_DROPPING)
        .id();
    let bolt = scenario
        .add_spell_to_graveyard(P0, "Lightning Bolt", true)
        .id();
    let guardian = scenario
        .add_creature_to_hand_from_oracle(P0, "Felidar Guardian", 1, 4, FELIDAR_GUARDIAN)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 3,
        })
        .id();
    let mut mana = generic_mana(3);
    mana.push(ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]));
    scenario.with_mana_pool(P0, mana);
    let mut runner = scenario.build();
    un_action(&mut runner, P0, UnAction::SetEnabled { enabled: true });
    un_action(&mut runner, P1, UnAction::SetEnabled { enabled: true });

    runner.cast(guardian).commit();
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "Felidar Guardian's {{3}}{{W}} was paid"
    );
    for _ in 0..12 {
        match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority passes");
            }
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(name_dropping)),
                    })
                    .expect("Name Dropping is a legal target");
            }
            other => panic!("unexpected decision before the blink prompt: {other:?}"),
        }
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "reach guard: Felidar Guardian's \"you may\" is pending, got {:?}",
        runner.state().waiting_for
    );
    tell(
        &mut runner,
        P1,
        UnTell::Said {
            text: "bolt".to_string(),
        },
    );
    assert_eq!(
        runner.state().un_actions.pending_offers.len(),
        1,
        "reach guard: the tell queued an offer while the prompt is open"
    );
    (runner, name_dropping, bolt)
}

#[test]
fn a_queued_gotcha_offer_does_not_survive_its_source_being_blinked() {
    // CR 400.7: the Name Dropping that returns is a new object, not the one the
    // tell matched, so the queued offer is dropped.
    let (mut runner, name_dropping, bolt) = name_dropping_told_during_felidar_guardian_prompt();
    let matched_incarnation = runner.state().objects[&name_dropping].incarnation;

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting the blink is accepted");
    assert_settled(&runner, "after the blink");
    let returned = &runner.state().objects[&name_dropping];
    assert_eq!(returned.zone, Zone::Battlefield, "Name Dropping came back");
    assert_ne!(
        returned.incarnation, matched_incarnation,
        "reach guard: Name Dropping really changed zones"
    );
    assert!(
        runner.state().un_actions.pending_offers.is_empty()
            && runner.state().un_actions.gotcha_offers.is_empty(),
        "the offer matched against the old object is gone"
    );

    // The returned Name Dropping functions: a fresh tell opens a window bound to
    // it, so the empty list above is the stale binding and nothing else.
    tell(
        &mut runner,
        P1,
        UnTell::Said {
            text: "bolt".to_string(),
        },
    );
    let offers = &runner.state().un_actions.gotcha_offers;
    assert_eq!(offers.len(), 1);
    assert_ne!(offers[0].source.reference.incarnation, matched_incarnation);
    un_action(
        &mut runner,
        P0,
        UnAction::SayGotcha {
            source_id: name_dropping,
            returns: bolt,
        },
    );
    runner.resolve_top();
    assert_eq!(runner.state().objects[&bolt].zone, Zone::Hand);
}

#[test]
fn a_queued_gotcha_offer_opens_when_its_source_is_not_blinked() {
    // Paired control: declining Felidar Guardian leaves the same Name Dropping in
    // play, so the queued offer opens and works.
    let (mut runner, name_dropping, bolt) = name_dropping_told_during_felidar_guardian_prompt();
    let matched_incarnation = runner.state().objects[&name_dropping].incarnation;

    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining the blink is accepted");
    assert_settled(&runner, "after declining the blink");
    assert_eq!(
        runner.state().objects[&name_dropping].incarnation,
        matched_incarnation
    );
    let offers = &runner.state().un_actions.gotcha_offers;
    assert_eq!(offers.len(), 1, "the queued offer opened");
    assert_eq!(offers[0].source.reference.object_id, name_dropping);
    assert_eq!(offers[0].source.reference.incarnation, matched_incarnation);
    assert_eq!(offers[0].source.expected_zone, Zone::Battlefield);
    assert_eq!(offers[0].returns.reference.object_id, bolt);
    un_action(
        &mut runner,
        P0,
        UnAction::SayGotcha {
            source_id: name_dropping,
            returns: bolt,
        },
    );
    runner.resolve_top();
    assert_eq!(runner.state().objects[&bolt].zone, Zone::Hand);
}

const PUCAS_MISCHIEF: &str = "At the beginning of your upkeep, you may exchange control of target nonland permanent you control and target nonland permanent an opponent controls with equal or lesser mana value.";

/// P0 controls Puca's Mischief and a {1}{W} enchantment to offer in exchange; P1 controls Name Dropping ({1}{G}) with Lightning Bolt in their
/// graveyard. At P0's upkeep the exchange trigger reaches its "you may" prompt,
/// and P0 says "bolt" there, queueing P1's Gotcha offer.
fn puca_exchange_told_during_prompt() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    scenario
        .add_enchantment_from_oracle(P0, "Puca's Mischief", PUCAS_MISCHIEF)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 3,
        });
    let offered = scenario
        .add_enchantment_from_oracle(
            P0,
            "Intangible Virtue",
            "Creature tokens you control get +1/+1 and have vigilance.",
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 1,
        })
        .id();
    let name_dropping = scenario
        .add_enchantment_from_oracle(P1, "Name Dropping", NAME_DROPPING)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();
    let bolt = scenario
        .add_spell_to_graveyard(P1, "Lightning Bolt", true)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().turn_number = 2;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    un_action(&mut runner, P0, UnAction::SetEnabled { enabled: true });
    un_action(&mut runner, P1, UnAction::SetEnabled { enabled: true });

    runner.advance_to_upkeep();
    for _ in 0..12 {
        match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => break,
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                let target = if runner.state().objects[&offered].controller == P0
                    && !matches!(
                        &runner.state().waiting_for,
                        WaitingFor::TriggerTargetSelection { selection, .. }
                            if selection.current_legal_targets.contains(&TargetRef::Object(name_dropping))
                    ) {
                    offered
                } else {
                    name_dropping
                };
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("legal exchange target");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority passes");
            }
            other => panic!("unexpected decision before the exchange prompt: {other:?}"),
        }
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "reach guard: Puca's Mischief's \"you may\" is pending, got {:?}",
        runner.state().waiting_for
    );
    tell(
        &mut runner,
        P0,
        UnTell::Said {
            text: "bolt".to_string(),
        },
    );
    assert_eq!(
        runner.state().un_actions.pending_offers.len(),
        1,
        "reach guard: the tell queued P1's offer while the prompt is open"
    );
    (runner, name_dropping, bolt)
}

#[test]
fn a_queued_gotcha_offer_does_not_survive_its_source_changing_controller() {
    // CR 109.5: once P0 controls Name Dropping, "you" on it is P0, so P1 (who
    // was the controller when the tell matched) can no longer say its Gotcha.
    let (mut runner, name_dropping, bolt) = puca_exchange_told_during_prompt();
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting the exchange is accepted");
    assert_eq!(
        runner.state().objects[&name_dropping].controller,
        P0,
        "reach guard: the exchange gave P0 control of Name Dropping"
    );
    assert!(
        runner.state().un_actions.pending_offers.is_empty()
            && runner.state().un_actions.gotcha_offers.is_empty(),
        "P1's offer is gone once they no longer control the source"
    );
    let result = apply(
        runner.state_mut(),
        P1,
        GameAction::UnAction {
            action: UnAction::SayGotcha {
                source_id: name_dropping,
                returns: bolt,
            },
        },
    );
    assert!(result.is_err(), "P1 cannot say Gotcha for P0's permanent");
    assert_eq!(runner.state().objects[&bolt].zone, Zone::Graveyard);
}

#[test]
fn a_queued_gotcha_offer_opens_when_its_source_keeps_its_controller() {
    // Paired control: declining the exchange leaves P1 in control, so the
    // queued offer opens and the return works.
    let (mut runner, name_dropping, bolt) = puca_exchange_told_during_prompt();
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining the exchange is accepted");
    assert_eq!(runner.state().objects[&name_dropping].controller, P1);
    assert_eq!(
        runner.state().un_actions.gotcha_offers.len(),
        1,
        "the queued offer opened"
    );
    un_action(
        &mut runner,
        P1,
        UnAction::SayGotcha {
            source_id: name_dropping,
            returns: bolt,
        },
    );
    runner.resolve_top();
    assert_eq!(runner.state().objects[&bolt].zone, Zone::Hand);
}

#[test]
fn a_tell_by_another_player_leaves_a_pending_decision_answerable() {
    let (mut runner, _name_dropping, _bolt) = name_dropping_during_unless_prompt(Zone::Graveyard);
    engine::game::interaction::bind_interaction_authority(
        runner.state_mut(),
        engine::types::interaction::InteractionSessionId("un-tell".to_string()),
    )
    .expect("the interaction authority binds");
    let slots_before = runner.state().active_interaction_slots.clone();
    assert!(
        !slots_before.is_empty(),
        "reach guard: the pending payment holds a bound interaction slot"
    );
    let waiting_before = runner.state().waiting_for.clone();
    tell(&mut runner, P0, UnTell::Physical(PhysicalTell::TouchFace));
    assert_eq!(
        runner.state().active_interaction_slots,
        slots_before,
        "a gesture does not rebind the open interaction"
    );
    assert_eq!(runner.state().waiting_for, waiting_before);
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("P1's payment answer is still accepted after P0's gesture");
}

#[test]
fn name_dropping_matches_real_plurals_of_name_words_and_rejects_invented_ones() {
    // Unhinged FAQ: "The word must be used exactly. The only exception is
    // plurals." — the real plural ("wolves"), not a regularized one ("wolfs").
    for (name, invented, real) in [
        ("Young Wolf", "wolfs", "wolves"),
        ("Thief of Sanity", "thiefs", "thieves"),
        ("Leaf Gilder", "leafs", "leaves"),
        ("Sanity Check", "sanitys", "sanities"),
        ("Box Trap", "boxs", "boxes"),
        ("Monarch Lord", "monarches", "monarchs"),
        ("Pop Quiz", "quizes", "quizzes"),
        // Words ending in "o" with standard "-oes" plural.
        ("Hero of Bladehold", "heros", "heroes"),
        ("Echo of Eons", "echos", "echoes"),
        ("Torpedo Fish", "torpedos", "torpedoes"),
        ("Veto Power", "vetos", "vetoes"),
        ("Embargo Order", "embargos", "embargoes"),
        ("Mosquito Bite", "mosquitos", "mosquitoes"),
        // Words with two accepted spellings: both "-oes" and plain "+s" are standard.
        // Use invalid plural "volcanoses" as the invented form since both "volcanos" and "volcanoes" are valid.
        ("Volcano Hellion", "volcanoses", "volcanoes"),
        ("Volcano Hellion", "volcanoses", "volcanos"),
        // Same for tornado - both "tornados" and "tornadoes" are valid.
        ("Tornado Elemental", "tornadoeses", "tornados"),
        ("Tornado Elemental", "tornadoeses", "tornadoes"),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_enchantment_from_oracle(P0, "Name Dropping", NAME_DROPPING);
        let card = scenario.add_creature_to_graveyard(P0, name, 1, 1).id();
        let mut runner = scenario.build();
        un_action(&mut runner, P0, UnAction::SetEnabled { enabled: true });
        un_action(&mut runner, P1, UnAction::SetEnabled { enabled: true });

        tell(
            &mut runner,
            P1,
            UnTell::Said {
                text: format!("Look at all those {invented}"),
            },
        );
        assert!(
            runner.state().un_actions.gotcha_offers.is_empty(),
            "{name}: '{invented}' is not the plural of a word in its name"
        );
        tell(
            &mut runner,
            P1,
            UnTell::Said {
                text: format!("Look at all those {real}"),
            },
        );
        let offers = &runner.state().un_actions.gotcha_offers;
        assert_eq!(offers.len(), 1, "{name}: '{real}' opens the window");
        assert_eq!(offers[0].returns.reference.object_id, card);
    }
}

#[test]
fn name_droppings_return_leaves_a_card_reanimated_in_response_alone() {
    // CR 400.7: the Gotcha return names the graveyard card. If that card is
    // reanimated while the reflexive trigger waits on the stack, the creature
    // on the battlefield is a new object and the trigger does nothing to it.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let name_dropping = scenario
        .add_enchantment_from_oracle(P0, "Name Dropping", NAME_DROPPING)
        .id();
    let bear = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .id();
    let reanimate = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Miraculous Recovery",
            true,
            "Return target creature card from your graveyard to the battlefield. Put a +1/+1 counter on it.",
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 4,
        })
        .id();
    let mut mana = generic_mana(4);
    mana.push(ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]));
    scenario.with_mana_pool(P0, mana);
    let mut runner = scenario.build();
    un_action(&mut runner, P0, UnAction::SetEnabled { enabled: true });
    un_action(&mut runner, P1, UnAction::SetEnabled { enabled: true });

    tell(
        &mut runner,
        P1,
        UnTell::Said {
            text: "bears".to_string(),
        },
    );
    un_action(
        &mut runner,
        P0,
        UnAction::SayGotcha {
            source_id: name_dropping,
            returns: bear,
        },
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach guard: the Gotcha return waits on the stack"
    );

    // Respond: reanimate the named card above the Gotcha trigger.
    drop(runner.cast(reanimate).target_object(bear).commit());
    assert_eq!(
        runner.state().stack.len(),
        2,
        "the response is on the stack"
    );
    assert!(
        runner.state().players[0].mana_pool.is_empty(),
        "the response's mana cost was paid"
    );
    runner.resolve_top();
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Battlefield,
        "reach guard: the response reanimated the card"
    );
    assert_eq!(runner.state().stack.len(), 1, "the Gotcha return is next");

    runner.resolve_top();
    assert!(
        runner.state().stack.is_empty(),
        "reach guard: the Gotcha return resolved"
    );
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Battlefield,
        "the reanimated creature is a new object; the return does not bounce it"
    );
}

const INFERNAL_SPAWN: &str = "Flying, first strike\n{1}{B}, Reveal this card from your hand, Say \"It's coming\": This card deals 1 damage to target opponent or planeswalker. Activate only during your upkeep and only once each turn.";

/// Infernal Spawn of Evil in P0's hand with {1}{B} available.
fn infernal_spawn_scenario() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);
    let spawn = scenario
        .add_creature_to_hand_from_oracle(P0, "Infernal Spawn of Evil", 4, 4, INFERNAL_SPAWN)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    (scenario.build(), spawn)
}

fn activate_infernal_spawn(
    runner: &mut GameRunner,
    spawn: ObjectId,
) -> Result<engine::types::game_state::ActionResult, engine::game::engine::EngineError> {
    let index = runner.state().objects[&spawn]
        .abilities
        .iter()
        .position(|a| a.cost.is_some())
        .expect("Infernal Spawn has its activated ability");
    runner.act(GameAction::ActivateAbility {
        source_id: spawn,
        ability_index: index,
        payment_mode: engine::types::game_state::CastPaymentMode::Auto,
    })
}

#[test]
fn a_spoken_activation_cost_parses_to_say_phrase_not_an_effect_cost() {
    let (runner, spawn) = infernal_spawn_scenario();
    let ability = runner.state().objects[&spawn]
        .abilities
        .iter()
        .find(|a| a.cost.is_some())
        .expect("activated ability");
    let AbilityCost::Composite { costs } = ability.cost.as_ref().expect("cost") else {
        panic!("expected a composite cost, got {:?}", ability.cost);
    };
    assert!(
        costs.contains(&AbilityCost::SayPhrase {
            phrase: "It's coming".to_string()
        }),
        "the spoken cost is a direct SayPhrase component: {costs:?}"
    );
    assert!(
        !costs
            .iter()
            .any(|c| matches!(c, AbilityCost::EffectCost { .. })),
        "no unpayable effect-cost wrapper: {costs:?}"
    );
}

#[test]
fn infernal_spawn_of_evil_pays_its_spoken_cost_and_deals_damage_when_opted_in() {
    let (mut runner, spawn) = infernal_spawn_scenario();
    un_action(&mut runner, P0, UnAction::SetEnabled { enabled: true });

    let result = activate_infernal_spawn(&mut runner, spawn).expect("activation is accepted");
    let mut events = result.events;
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        events.extend(
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(P1)),
                })
                .expect("P1 is a legal target")
                .events,
        );
    }
    for _ in 0..6 {
        if runner.state().stack.is_empty() {
            break;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("priority passes")
                .events,
        );
    }
    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::PhraseSaid { player, phrase } if *player == P0 && phrase == "It's coming"
        )),
        "the spoken cost was paid: {events:?}"
    );
    assert_eq!(runner.life(P1), 19, "the ability dealt 1 damage");
    assert_settled(&runner, "Infernal Spawn activation");
}

#[test]
fn infernal_spawn_of_evil_cannot_be_activated_without_the_un_actions_opt_in() {
    let (mut runner, spawn) = infernal_spawn_scenario();
    let offers_spawn = |runner: &GameRunner, spawn: ObjectId| {
        engine::ai_support::legal_actions(runner.state())
            .iter()
            .any(|a| matches!(a, GameAction::ActivateAbility { source_id, .. } if *source_id == spawn))
    };
    // Activation legality gates the spoken cost on the opt-in before any dry-run
    // payment: the opted-out activation is never offered.
    assert!(
        !offers_spawn(&runner, spawn),
        "an opted-out player is not offered the spoken activation"
    );
    // Reach guard: the same board offers it once P0 opts in, so the negative is
    // the opt-in gate and not some other missing resource.
    let (mut opted_in, opted_in_spawn) = infernal_spawn_scenario();
    un_action(&mut opted_in, P0, UnAction::SetEnabled { enabled: true });
    assert!(
        offers_spawn(&opted_in, opted_in_spawn),
        "opting in makes the spoken activation legal"
    );
    assert!(
        activate_infernal_spawn(&mut runner, spawn).is_err(),
        "a spoken cost is unpayable for a player who did not opt in"
    );
    assert_eq!(runner.life(P1), 20, "no damage was dealt");
    assert_settled(&runner, "Infernal Spawn opted out");
}
