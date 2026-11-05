//! A token created mid-chain must not join a tracked set whose consumer reads
//! CARDS (#9225 companion). CR 108.2b: tokens aren't cards.
//!
//! The publish-site rule (`tracked_set_consumer_reads_only_cards`) applies when
//! the chain's first tracked-set consumer casts from the set or COUNTS it —
//! through an effect quantity or through a `repeat_for` loop. Each test here
//! fails if that rule is disabled, because the created token is then unioned
//! into the population the card names as cards.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::combat::AttackTarget;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, CastingPermission};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

fn battlefield_tokens_named(state: &GameState, name: &str) -> Vec<ObjectId> {
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| state.objects[id].is_token && state.objects[id].name == name)
        .collect()
}

fn library_card(state: &mut GameState, card_id: u64, is_land: bool) -> ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        P0,
        "Library Card".to_string(),
        Zone::Library,
    );
    let object = state.objects.get_mut(&id).expect("just created");
    let core_type = if is_land {
        CoreType::Land
    } else {
        CoreType::Sorcery
    };
    object.card_types.core_types.push(core_type);
    object.base_card_types = object.card_types.clone();
    id
}

/// CR 608.2c + CR 108.2b: a `repeat_for` count ("For each nonland card revealed
/// this way, draw a card") reads cards. SYNTHETIC Oracle text: the shape of
/// Culmination of Studies' clauses with a reveal producer, where nothing else
/// masks the token. One land and one nonland card revealed: one Treasure, and
/// exactly ONE card drawn. With the Treasure in the set (a nonland object) the
/// loop would draw two.
#[test]
fn repeat_for_count_of_revealed_cards_excludes_the_created_treasure() {
    const TEXT: &str = "Reveal the top two cards of your library. For each land card \
         revealed this way, create a Treasure token. For each nonland card revealed this \
         way, draw a card.";
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let source = create_object(
        &mut state,
        CardId(1),
        P0,
        "Reveal Probe".to_string(),
        Zone::Battlefield,
    );
    // `create_object` appends to the bottom, so the land (created first) is on
    // top, the nonland card second, and two spares sit below to draw.
    let land = library_card(&mut state, 10, true);
    let nonland = library_card(&mut state, 11, false);
    library_card(&mut state, 12, false);
    library_card(&mut state, 13, false);
    let hand_before = state.players[0].hand.len();

    let def = parse_effect_chain(TEXT, AbilityKind::Spell);
    let ability = build_resolved_from_def(&def, source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).expect("the probe resolves");

    let treasures = battlefield_tokens_named(&state, "Treasure");
    assert_eq!(
        treasures.len(),
        1,
        "reach guard: one land revealed creates one Treasure"
    );
    let sets: Vec<&Vec<ObjectId>> = state.tracked_object_sets.values().collect();
    assert_eq!(sets.len(), 1, "reach guard: the reveal published one set");
    assert!(
        sets[0].contains(&land) && sets[0].contains(&nonland),
        "reach guard: the set holds both revealed cards"
    );
    assert!(
        !sets[0].contains(&treasures[0]),
        "the Treasure is not a card revealed this way"
    );
    assert_eq!(
        state.players[0].hand.len(),
        hand_before + 1,
        "one nonland card was revealed this way, so exactly one card is drawn"
    );
}

/// Verbatim Oracle text (Scryfall `cards/named?exact=Phabine, Boss's Confidant`).
const PHABINE: &str = "Creature tokens you control have haste.\n\
     Parley — At the beginning of combat on your turn, each player reveals the top card \
     of their library. For each land card revealed this way, you create a 1/1 green and \
     white Citizen creature token. Then creatures you control get +1/+1 until end of turn \
     for each nonland card revealed this way. Then each player draws a card.";

/// CR 608.2c + CR 108.2b: three players reveal two lands and one nonland card.
/// Phabine's controller creates two Citizens, and creatures get +1/+1 for the
/// ONE nonland card — the two Citizens are tokens, not nonland cards revealed
/// this way. With the Citizens in the set the pump would be +3/+3.
#[test]
fn phabine_pumps_once_per_nonland_card_not_per_citizen() {
    let mut scenario = GameScenario::new_n_player(3, 9225);
    scenario.at_phase(Phase::PreCombatMain);
    let phabine = scenario
        .add_creature_from_oracle(P0, "Phabine, Boss's Confidant", 3, 6, PHABINE)
        .id();
    scenario.add_land_to_library_top(P0, "Revealed Land");
    scenario.add_land_to_library_top(P1, "Revealed Land");
    scenario.add_spell_to_library_top(P2, "Revealed Spell", false);
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner.advance_until_stack_empty();

    let citizens = battlefield_tokens_named(runner.state(), "Citizen");
    assert_eq!(
        citizens.len(),
        2,
        "reach guard: two land cards revealed create two Citizens"
    );
    assert_eq!(
        runner.state().objects[&phabine].power,
        Some(4),
        "one nonland card was revealed this way — Phabine gets +1/+1, not +3/+3"
    );
    assert_eq!(runner.state().objects[&phabine].toughness, Some(7));
}

/// Verbatim Oracle text (Scryfall `cards/named?exact=Kamachal, Ship's Mascot`).
const KAMACHAL: &str = "Menace\n\
     {R}: Kamachal, Ship's Mascot gets +1/+0 until end of turn.\n\
     Whenever Kamachal deals combat damage to a player, create a Treasure token. Then \
     exile from that player's library a random card with mana value equal to the damage \
     dealt. You may cast that card this turn.";

/// Drive combat until Kamachal's damage trigger has resolved.
fn resolve_combat_damage_trigger(runner: &mut GameRunner) {
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .declare_blockers(&[])
                    .expect("the defending player may decline to block");
            }
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .expect("order the damage trigger");
            }
            WaitingFor::Priority { .. }
                if runner.state().stack.is_empty()
                    && !battlefield_tokens_named(runner.state(), "Treasure").is_empty() =>
            {
                return;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority is always legal");
            }
            other => panic!("Kamachal's combat stalled on an unexpected prompt: {other:?}"),
        }
    }
    panic!("Kamachal's damage trigger did not resolve");
}

fn has_cast_grant(state: &GameState, id: ObjectId) -> bool {
    state.objects[&id]
        .casting_permissions
        .iter()
        .any(|permission| matches!(permission, CastingPermission::PlayFromExile { .. }))
}

/// CR 608.2c + CR 108.2b: "You may cast that card this turn" names the exiled
/// card. The Treasure created earlier in the same instruction is a token and
/// must not receive the grant.
#[test]
fn kamachal_grants_the_exiled_card_and_not_the_treasure() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kamachal = scenario
        .add_creature_from_oracle(P0, "Kamachal, Ship's Mascot", 2, 2, KAMACHAL)
        .id();
    // The only card in P1's library with mana value 2 (Kamachal deals 2).
    let exiled = scenario
        .add_spell_to_library_top(P1, "Two-Drop", false)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(kamachal, AttackTarget::Player(P1))])
        .expect("Kamachal must be able to attack");
    resolve_combat_damage_trigger(&mut runner);

    let state = runner.state();
    assert_eq!(
        state.objects[&exiled].zone,
        Zone::Exile,
        "reach guard: the mana-value-2 card was exiled"
    );
    assert!(
        has_cast_grant(state, exiled),
        "reach guard: the exiled card may be cast this turn"
    );
    let treasures = battlefield_tokens_named(state, "Treasure");
    assert_eq!(
        treasures.len(),
        1,
        "reach guard: Kamachal created its Treasure"
    );
    assert!(
        !has_cast_grant(state, treasures[0]),
        "the Treasure is a token, not \"that card\" — it must not receive the cast grant"
    );
}
