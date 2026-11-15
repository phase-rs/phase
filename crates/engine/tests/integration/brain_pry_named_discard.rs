//! Brain Pry — "Choose a nonland card name. Target player reveals their hand.
//! That player discards a card with that name. If they can't, you draw a card."
//!
//! CR 608.2d: the chosen name is public information, so the game log must state
//! it. CR 608.2c: the discard is limited to cards with that name (CR 201.2a), and
//! the "if they can't" rider draws only when no such card was discarded.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCondition, AbilityDefinition, ChoiceType, Effect, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::log::LogSegment;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const BRAIN_PRY: &str = "Choose a nonland card name. Target player reveals their hand. That \
player discards a card with that name. If they can't, you draw a card.";

const REGROWTH: &str = "Return target card from your graveyard to your hand.";

struct Setup {
    runner: GameRunner,
    hand: Vec<ObjectId>,
}

fn setup(opponent_hand: &[&str]) -> (Setup, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 11);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Library Card"]);
    let hand = opponent_hand
        .iter()
        .map(|name| scenario.add_card_to_hand(P1, name))
        .collect();
    let mut builder = scenario.add_spell_to_hand_from_oracle(P0, "Brain Pry", false, BRAIN_PRY);
    builder.with_mana_cost(ManaCost::Cost {
        generic: 1,
        shards: vec![ManaCostShard::Black],
    });
    let spell = builder.id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Black, spell, false, vec![]),
            ManaUnit::new(ManaType::Black, spell, false, vec![]),
        ],
    );
    let mut runner = scenario.build();
    runner.state_mut().all_card_names = ["Opt", "Shock", "Lightning Bolt", "Éowyn, Shieldmaiden"]
        .map(String::from)
        .into();
    (Setup { runner, hand }, spell)
}

/// Casts Brain Pry at P1 naming `named`, answering every prompt, and returns
/// every event raised.
fn cast_naming(setup: &mut Setup, spell: ObjectId, named: &str) -> Vec<GameEvent> {
    let card_id = setup.runner.state().objects[&spell].card_id;
    let mut events = Vec::new();
    let mut result = setup
        .runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast");
    for _ in 0..32 {
        events.extend(result.events.clone());
        let action = match &result.waiting_for {
            WaitingFor::TargetSelection { .. } => GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P1)],
            },
            WaitingFor::NamedChoice { .. } => GameAction::ChooseOption {
                choice: named.to_string(),
            },
            WaitingFor::DiscardChoice { cards, .. } => GameAction::SelectCards {
                cards: cards.iter().take(1).copied().collect(),
            },
            WaitingFor::Priority { .. } if setup.runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            _ => GameAction::PassPriority,
        };
        result = setup.runner.act(action).expect("prompt answer");
    }
    events
}

fn walk<'a>(def: &'a AbilityDefinition, out: &mut Vec<&'a AbilityDefinition>) {
    out.push(def);
    if let Some(sub) = def.sub_ability.as_deref() {
        walk(sub, out);
    }
}

#[test]
fn parses_discard_filter_and_if_they_cant_rider() {
    let parsed = engine::parser::oracle::parse_oracle_text(
        BRAIN_PRY,
        "Brain Pry",
        &[],
        &["Sorcery".to_owned()],
        &[],
    );
    let mut chain = Vec::new();
    walk(&parsed.abilities[0], &mut chain);
    assert!(matches!(
        &*chain[0].effect,
        Effect::Choose {
            choice_type: ChoiceType::CardName,
            ..
        }
    ));
    let Effect::Discard { filter, .. } = &*chain[2].effect else {
        panic!("expected Discard, got {:?}", chain[2].effect);
    };
    assert_eq!(filter.as_ref(), Some(&TargetFilter::HasChosenName));
    assert!(matches!(&*chain[3].effect, Effect::Draw { .. }));
    assert!(matches!(
        chain[3].condition,
        Some(AbilityCondition::Not { .. })
    ));
}

#[test]
fn naming_a_card_is_logged_and_only_that_card_is_discarded() {
    let (mut setup, spell) = setup(&["Opt", "Shock"]);
    let events = cast_naming(&mut setup, spell, "Shock");
    let state = setup.runner.state();

    assert!(events.iter().any(|event| matches!(
        event,
        GameEvent::CardNamed { player_id, name, .. } if *player_id == P0 && name == "Shock"
    )));
    let entries = engine::game::log::resolve_log_entries(&events, state, state);
    assert!(
        entries.iter().any(|entry| entry
            .segments
            .iter()
            .any(|segment| matches!(segment, LogSegment::Text(text) if text == "Shock"))),
        "the log must state the named card"
    );
    assert_eq!(state.objects[&setup.hand[1]].zone, Zone::Graveyard);
    assert_eq!(state.objects[&setup.hand[0]].zone, Zone::Hand);
    assert_eq!(
        state.players[0].hand.len(),
        0,
        "a discard happened: no draw"
    );
}

#[test]
fn no_matching_card_discards_nothing_and_controller_draws() {
    let (mut setup, spell) = setup(&["Opt", "Shock"]);
    cast_naming(&mut setup, spell, "Lightning Bolt");
    let state = setup.runner.state();

    for id in &setup.hand {
        assert_eq!(
            state.objects[id].zone,
            Zone::Hand,
            "nothing may be discarded"
        );
    }
    assert_eq!(
        state.players[0].hand.len(),
        1,
        "\"if they can't\" draws a card"
    );
}

/// CR 201.2a: the naming prompt accepts any casing, so a name typed in a
/// different Unicode case ("éowyn" for "Éowyn") still names that card and the
/// discard must match it rather than falling through to the draw.
#[test]
fn accented_name_typed_in_lowercase_matches_the_card_in_hand() {
    let (mut setup, spell) = setup(&["Opt", "Éowyn, Shieldmaiden"]);
    let (opt, eowyn) = (setup.hand[0], setup.hand[1]);
    cast_naming(&mut setup, spell, "éowyn, shieldmaiden");
    let state = setup.runner.state();

    assert_eq!(state.objects[&eowyn].zone, Zone::Graveyard);
    assert_eq!(state.objects[&opt].zone, Zone::Hand);
    assert_eq!(
        state.players[0].hand.len(),
        0,
        "a discard happened: no draw"
    );
}

#[test]
fn two_matching_cards_prompt_the_discarding_player_to_choose_one() {
    let (mut setup, spell) = setup(&["Opt", "Shock", "Shock"]);
    let (opt, shock_a, shock_b) = (setup.hand[0], setup.hand[1], setup.hand[2]);
    let card_id = setup.runner.state().objects[&spell].card_id;

    let result = setup
        .runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast");
    let mut result = result;
    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        result = setup
            .runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P1)],
            })
            .expect("target");
    }
    // CR 601.2i: the spell is on the stack; it resolves once priority passes.
    while matches!(result.waiting_for, WaitingFor::Priority { .. }) {
        result = setup.runner.act(GameAction::PassPriority).expect("resolve");
    }
    assert!(
        matches!(result.waiting_for, WaitingFor::NamedChoice { .. }),
        "expected NamedChoice, got {:?}",
        result.waiting_for
    );
    let mut result = setup
        .runner
        .act(GameAction::ChooseOption {
            choice: "Shock".to_string(),
        })
        .expect("name");
    // Resolve the spell if the stack still holds it.
    while matches!(result.waiting_for, WaitingFor::Priority { .. })
        && !setup.runner.state().stack.is_empty()
    {
        result = setup.runner.act(GameAction::PassPriority).expect("pass");
        if let WaitingFor::NamedChoice { .. } = result.waiting_for {
            panic!("unexpected second NamedChoice");
        }
    }

    // CR 701.9b: the discarding player chooses among both same-name cards.
    let WaitingFor::DiscardChoice {
        player,
        count,
        cards,
        ..
    } = &result.waiting_for
    else {
        panic!("expected DiscardChoice, got {:?}", result.waiting_for);
    };
    assert_eq!(*player, P1);
    assert_eq!(*count, 1);
    let mut eligible = cards.clone();
    eligible.sort();
    let mut expected = vec![shock_a, shock_b];
    expected.sort();
    assert_eq!(eligible, expected, "only the Shocks are eligible, not Opt");

    let result = setup
        .runner
        .act(GameAction::SelectCards {
            cards: vec![shock_b],
        })
        .expect("select discard");
    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { .. }),
        "got {:?}",
        result.waiting_for
    );
    let state = setup.runner.state();
    assert!(state.stack.is_empty());
    assert_eq!(state.objects[&shock_b].zone, Zone::Graveyard);
    assert_eq!(state.objects[&shock_a].zone, Zone::Hand);
    assert_eq!(state.objects[&opt].zone, Zone::Hand);
    assert_eq!(
        state.players[0].hand.len(),
        0,
        "a discard happened: no draw"
    );
}

/// CR 608.2c + CR 608.2d: When a source that makes a card-name choice resolves
/// multiple times on the same stored object (via Regrowth recursion), each
/// resolution's named-discard restriction must consume that specific
/// resolution's chosen answer rather than the first historical choice stored
/// across past casts.
#[test]
fn recast_on_same_object_via_regrowth_binds_new_chosen_name() {
    let mut scenario = GameScenario::new_n_player(2, 11);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Top Deck 1", "Top Deck 2"]);

    // Opponent has two distinct Opts and one Lightning Bolt (no Shock).
    let opt_a = scenario.add_card_to_hand(P1, "Opt");
    let opt_b = scenario.add_card_to_hand(P1, "Opt");
    let bolt = scenario.add_card_to_hand(P1, "Lightning Bolt");

    let brain_pry = scenario
        .add_spell_to_hand_from_oracle(P0, "Brain Pry", false, BRAIN_PRY)
        .with_mana_cost(ManaCost::zero())
        .id();
    let regrowth = scenario
        .add_spell_to_hand_from_oracle(P0, "Regrowth", false, REGROWTH)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    runner.state_mut().all_card_names = ["Opt", "Shock", "Lightning Bolt"].map(String::from).into();

    let brain_pry_card_id = runner.state().objects[&brain_pry].card_id;

    // --- FIRST CAST: name absent Shock ---
    let mut result = runner
        .act(GameAction::CastSpell {
            object_id: brain_pry,
            card_id: brain_pry_card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast 1");
    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        result = runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P1)],
            })
            .expect("target P1");
    }
    while matches!(result.waiting_for, WaitingFor::Priority { .. }) {
        result = runner
            .act(GameAction::PassPriority)
            .expect("pass to resolution");
    }
    assert!(
        matches!(result.waiting_for, WaitingFor::NamedChoice { .. }),
        "expected NamedChoice on first cast, got {:?}",
        result.waiting_for
    );
    let mut result = runner
        .act(GameAction::ChooseOption {
            choice: "Shock".to_string(),
        })
        .expect("name Shock");
    while matches!(result.waiting_for, WaitingFor::Priority { .. })
        && !runner.state().stack.is_empty()
    {
        result = runner
            .act(GameAction::PassPriority)
            .expect("pass to finish resolution");
    }

    // Reach guards: first cast completed, opponent discarded nothing (no Shock in hand),
    // P0 drew a card via "if they can't", Brain Pry is in graveyard.
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&brain_pry].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&opt_a].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&opt_b].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&bolt].zone, Zone::Hand);
    assert_eq!(
        runner.state().players[0].hand.len(),
        2, // Regrowth + Top Deck 1
        "first cast draws via 'if they can't'"
    );

    // --- RECURSION: parsed Regrowth returns the same Brain Pry object ---
    let regrowth_card_id = runner.state().objects[&regrowth].card_id;
    let mut result = runner
        .act(GameAction::CastSpell {
            object_id: regrowth,
            card_id: regrowth_card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Regrowth");
    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        result = runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(brain_pry)],
            })
            .expect("target Brain Pry in graveyard");
    }
    while matches!(result.waiting_for, WaitingFor::Priority { .. })
        && !runner.state().stack.is_empty()
    {
        result = runner
            .act(GameAction::PassPriority)
            .expect("resolve Regrowth");
    }

    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&brain_pry].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&regrowth].zone, Zone::Graveyard);

    // --- SECOND CAST: name Opt ---
    let mut result = runner
        .act(GameAction::CastSpell {
            object_id: brain_pry,
            card_id: brain_pry_card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast 2");
    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        result = runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Player(P1)],
            })
            .expect("target P1 again");
    }
    while matches!(result.waiting_for, WaitingFor::Priority { .. }) {
        result = runner
            .act(GameAction::PassPriority)
            .expect("pass to resolution 2");
    }
    assert!(
        matches!(result.waiting_for, WaitingFor::NamedChoice { .. }),
        "expected NamedChoice on second cast, got {:?}",
        result.waiting_for
    );
    let mut result = runner
        .act(GameAction::ChooseOption {
            choice: "Opt".to_string(),
        })
        .expect("name Opt");
    while matches!(result.waiting_for, WaitingFor::Priority { .. })
        && !runner.state().stack.is_empty()
    {
        result = runner
            .act(GameAction::PassPriority)
            .expect("pass to prompt");
    }

    // Require the actual mandatory discard prompt containing exactly both Opt IDs.
    let WaitingFor::DiscardChoice {
        player,
        count,
        cards,
        ..
    } = &result.waiting_for
    else {
        panic!("expected DiscardChoice, got {:?}", result.waiting_for);
    };
    assert_eq!(*player, P1);
    assert_eq!(*count, 1);
    let mut eligible = cards.clone();
    eligible.sort();
    let mut expected = vec![opt_a, opt_b];
    expected.sort();
    assert_eq!(
        eligible, expected,
        "discard prompt must contain exactly both Opt IDs, excluding Lightning Bolt"
    );

    // Select one (opt_a)
    let mut result = runner
        .act(GameAction::SelectCards { cards: vec![opt_a] })
        .expect("select opt_a for discard");
    while matches!(result.waiting_for, WaitingFor::Priority { .. })
        && !runner.state().stack.is_empty()
    {
        result = runner
            .act(GameAction::PassPriority)
            .expect("settle priority");
    }

    // Assert only that card discarded, no second draw and settled Priority/empty resolution state.
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().objects[&opt_a].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&opt_b].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&bolt].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&brain_pry].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().players[0].hand.len(),
        1, // only Top Deck 1 from the first cast; no second draw!
        "mandatory discard succeeded: no second draw"
    );
}
