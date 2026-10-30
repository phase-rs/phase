//! CR 603.3b + CR 117.3b + CR 605.3a: the Amulet of Vigor "bounce land" line.
//!
//! 1. Activate Patron of the Moon, putting Guildless Commons from hand onto
//!    the battlefield tapped.
//! 2. Commons' "return a land you control" trigger and Amulet of Vigor's
//!    untap trigger fire together; their controller orders them (CR 603.3b)
//!    with Amulet's on top.
//! 3. Amulet's trigger resolves, untapping Commons.
//! 4. With Commons' own trigger still on the stack, its controller receives
//!    priority (CR 117.3b) and taps Commons for mana (CR 605.3a).
//! 5. Commons' trigger resolves and returns Commons itself (the choice is made
//!    on resolution, CR 608.2d), leaving its mana in the pool.
//!
//! Every priority window of the acting player is driven the way the client
//! drives it: the window is passed for the player exactly when the engine's
//! `auto_pass_recommended` says so. Step 4 therefore fails if the engine
//! recommends passing a window in which the player's own top-of-stack object
//! can remove one of their activatable mana sources from the battlefield.

use engine::ai_support::{auto_pass_recommended, legal_actions, legal_actions_full};
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const PATRON_OF_THE_MOON: &str = "Moonfolk offering (You may cast this spell any time you could cast an instant by sacrificing a Moonfolk and paying the difference in mana costs between this and the sacrificed Moonfolk. Mana cost includes color.)\nFlying\n{1}: Put up to two land cards from your hand onto the battlefield tapped.";
const AMULET_OF_VIGOR: &str = "Whenever a permanent you control enters tapped, untap it.";
const GUILDLESS_COMMONS: &str = "This land enters tapped.\nWhen this land enters, return a land you control to its owner's hand.\n{T}: Add {C}{C}.";

struct Board {
    runner: GameRunner,
    patron: ObjectId,
    commons: ObjectId,
}

fn board() -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let patron = scenario
        .add_creature(P0, "Patron of the Moon", 5, 4)
        .from_oracle_text_with_keywords(&["Flying", "Offering"], PATRON_OF_THE_MOON)
        .id();
    scenario.add_artifact_from_oracle(P0, "Amulet of Vigor", AMULET_OF_VIGOR);
    // Pays Patron's {1}; it is also a second "land you control" for Commons'
    // resolution-time choice, so returning Commons itself is a real choice.
    scenario.add_basic_land(P0, ManaColor::Blue);
    let commons = scenario
        .add_land_to_hand(P0, "Guildless Commons")
        .from_oracle_text(GUILDLESS_COMMONS)
        .id();
    Board {
        runner: scenario.build(),
        patron,
        commons,
    }
}

fn top_source_name(runner: &GameRunner) -> Option<String> {
    let state = runner.state();
    let top = state.stack.back()?;
    state
        .objects
        .get(&top.source_id)
        .map(|obj| obj.name.clone())
}

/// The client's Standard-mode auto-pass: pass the acting player's priority
/// window exactly when the engine recommends it.
fn recommends_pass(runner: &GameRunner) -> bool {
    let state = runner.state();
    auto_pass_recommended(state, &legal_actions(state))
}

fn commons_mana_action(runner: &GameRunner, commons: ObjectId) -> Option<GameAction> {
    let (_, _, grouped) = legal_actions_full(runner.state());
    grouped.get(&commons).and_then(|actions| {
        actions
            .iter()
            .find(|action| {
                matches!(
                    action,
                    GameAction::TapLandForMana { .. } | GameAction::ActivateManaSource { .. }
                )
            })
            .cloned()
    })
}

fn colorless_from(runner: &GameRunner, source: ObjectId) -> usize {
    runner.state().players[0]
        .mana_pool
        .units()
        .filter(|unit| unit.color == ManaType::Colorless && unit.source_id == source)
        .count()
}

#[test]
fn amulet_untaps_bounce_land_and_its_mana_survives_returning_it() {
    let Board {
        mut runner,
        patron,
        commons,
    } = board();

    // Step 1: activate Patron of the Moon (its {1} is auto-paid by the Island).
    runner
        .act(GameAction::ActivateAbility {
            source_id: patron,
            ability_index: 0,
            payment_mode: Default::default(),
        })
        .expect("Patron of the Moon's ability is activatable");

    let mut amulet_window_checked = false;
    let mut floated_mana = false;
    let mut post_tap_window_checked = false;
    let mut chose_commons = false;
    let mut ordered = false;
    for _ in 0..40 {
        let waiting_for = runner.state().waiting_for.clone();
        match waiting_for {
            // Patron resolves: put Commons onto the battlefield tapped.
            WaitingFor::EffectZoneChoice {
                zone: Zone::Hand,
                cards,
                ..
            } => {
                assert!(cards.contains(&commons), "Commons is a land card in hand");
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![commons],
                    })
                    .expect("put Commons onto the battlefield");
            }
            // Step 2 (CR 603.3b): both enter triggers are offered for ordering;
            // index 0 is placed first, so Amulet's trigger goes last (on top).
            WaitingFor::OrderTriggers { player, triggers } => {
                assert_eq!(player, P0);
                let names: Vec<&str> = triggers.iter().map(|t| t.source_name.as_str()).collect();
                assert_eq!(
                    {
                        let mut sorted = names.clone();
                        sorted.sort_unstable();
                        sorted
                    },
                    vec!["Amulet of Vigor", "Guildless Commons"],
                    "both simultaneous enter triggers must be ordered together"
                );
                let amulet = names.iter().position(|n| *n == "Amulet of Vigor").unwrap();
                let order = vec![1 - amulet, amulet];
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order the triggers");
                ordered = true;
            }
            // Step 5 (CR 608.2d): Commons' trigger resolves; return Commons itself.
            WaitingFor::EffectZoneChoice {
                zone: Zone::Battlefield,
                cards,
                ..
            } => {
                assert!(
                    floated_mana,
                    "Commons' trigger must not resolve before Commons was tapped for mana"
                );
                assert!(cards.contains(&commons), "Commons is a land you control");
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![commons],
                    })
                    .expect("return Commons to its owner's hand");
                chose_commons = true;
            }
            WaitingFor::Priority { player } if player == P0 => {
                let commons_obj = &runner.state().objects[&commons];
                let commons_untapped_on_battlefield =
                    commons_obj.zone == Zone::Battlefield && !commons_obj.tapped;
                match top_source_name(&runner).as_deref() {
                    None => break,
                    // Own Amulet trigger on top: nothing it does can remove a mana
                    // source, so the own-object standing pass still applies.
                    Some("Amulet of Vigor") => {
                        assert!(
                            recommends_pass(&runner),
                            "own untap trigger on top keeps the own-object standing pass"
                        );
                        amulet_window_checked = true;
                        runner.act(GameAction::PassPriority).unwrap();
                    }
                    // Step 4: Commons' trigger on top with Commons untapped.
                    Some("Guildless Commons") if commons_untapped_on_battlefield => {
                        // Step 3 already happened: Commons entered tapped and
                        // Amulet's trigger, resolved first, untapped it.
                        assert!(amulet_window_checked, "Amulet's trigger resolved first");
                        assert!(
                            !recommends_pass(&runner),
                            "CR 117.3b + CR 605.3a: the player must keep this window to \
                             tap Commons before its own trigger returns it"
                        );
                        let tap = commons_mana_action(&runner, commons)
                            .expect("Commons' mana ability is activatable with priority");
                        runner.act(tap).expect("tap Commons for {C}{C}");
                        floated_mana = true;
                    }
                    // Patron's ability (before any land is untapped), or Commons'
                    // trigger once Commons is tapped: no activatable mana source
                    // is left to lose, so the own-object standing pass applies.
                    Some(name) => {
                        assert!(
                            recommends_pass(&runner),
                            "{name}: with no mana source left to lose, the own-object \
                             standing pass applies"
                        );
                        post_tap_window_checked |= name == "Guildless Commons";
                        runner.act(GameAction::PassPriority).unwrap();
                    }
                }
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }

    assert!(ordered, "the simultaneous triggers were ordered");
    assert!(
        amulet_window_checked,
        "reach guard: Amulet's trigger window was seen"
    );
    assert!(
        floated_mana,
        "Commons was tapped for mana with its trigger on the stack"
    );
    assert!(
        post_tap_window_checked,
        "reach guard: the window over Commons' trigger after tapping was seen"
    );
    assert!(
        chose_commons,
        "Commons' trigger chose Commons on resolution"
    );
    assert!(runner.state().stack.is_empty());

    // End state: Commons back in hand, its {C}{C} still floating.
    assert_eq!(runner.state().objects[&commons].zone, Zone::Hand);
    assert_eq!(
        colorless_from(&runner, commons),
        2,
        "Commons' {{C}}{{C}} stays in the pool after Commons leaves the battlefield"
    );
}

const AMARETH_THE_LUSTROUS: &str = "Flying\nWhenever another permanent you control enters, look at the top card of your library. If it shares a card type with that permanent, you may reveal that card and put it into your hand.";

/// No spurious hold: an own trigger whose "put it into your hand" names the
/// card it looks at (an object the chain produces as it resolves) cannot move
/// the land that just entered, so its window keeps the own-object standing
/// pass even with that land untapped.
#[test]
fn own_look_at_top_card_trigger_keeps_the_pass_over_an_untapped_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Amareth, the Lustrous", 6, 6)
        .from_oracle_text_with_keywords(&["Flying"], AMARETH_THE_LUSTROUS);
    scenario.with_library_top(P0, &["Library Card"]);
    let forest = scenario
        .add_land_to_hand(P0, "Forest")
        .from_oracle_text("{T}: Add {G}.")
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&forest].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: forest,
            card_id,
        })
        .expect("play the Forest");

    // Reach guard: Amareth's trigger is on top, P0 has priority, and the Forest
    // is an untapped land whose mana ability P0 can activate now.
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
    assert_eq!(
        top_source_name(&runner).as_deref(),
        Some("Amareth, the Lustrous")
    );
    assert!(!runner.state().objects[&forest].tapped);
    let (_, _, grouped) = legal_actions_full(runner.state());
    assert!(grouped.get(&forest).is_some_and(|actions| actions
        .iter()
        .any(|action| matches!(action, GameAction::TapLandForMana { .. }))));

    assert!(
        recommends_pass(&runner),
        "nothing this trigger does can move the Forest, so the own-object pass stands"
    );
}
