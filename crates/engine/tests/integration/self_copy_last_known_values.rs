//! CR 608.2h + CR 707.2: a copy effect whose copy source is its ability's own
//! source ("create a token that's a copy of this creature", `CopyTokenOf`, or
//! "it becomes a copy of this creature", `BecomeCopy`) copies the source's
//! current copiable values while the source is still the incarnation the
//! ability captured, and its last-known copiable values once that object has
//! left the public zone it was identified in (Scute Swarm, Polyraptor, Pack
//! Rat and Myr Propagator carry that ruling). The last-known values belong to
//! the exact incarnation the ability captured: a card that returns under the
//! same storage id is a new object (CR 400.7) and is never the source. For
//! Embalm, Eternalize and Encore the cost moves the card into exile, and the
//! effect finds it there (CR 400.7j).

use engine::game::effects::{become_copy, resolve_effect, token_copy};
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zones::{create_object, move_to_zone};
use engine::types::ability::{
    CopyRecipient, Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::{GameState, PayCostKind, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const POLYRAPTOR: &str = "Enrage — Whenever this creature is dealt damage, create a token that's a copy of this creature.";
const PACK_RAT: &str = "Pack Rat's power and toughness are each equal to the number of Rats you control.\n{2}{B}, Discard a card: Create a token that's a copy of this creature.";
const MYR_PROPAGATOR: &str = "{3}, {T}: Create a token that's a copy of this creature.";
const SACRED_CAT: &str = "Lifelink\nEmbalm {W} ({W}, Exile this card from your graveyard: Create a token that's a copy of it, except it's a white Zombie Cat with no mana cost. Embalm only as a sorcery.)";
const BRIARBLADE_ADEPT: &str = "Whenever this creature attacks, target creature an opponent controls gets -1/-1 until end of turn.\nEncore {3}{B} ({3}{B}, Exile this card from your graveyard: For each opponent, create a token copy that attacks that opponent this turn if able. They gain haste. Sacrifice them at the beginning of the next end step. Activate only as a sorcery.)";
const BROODBIRTH_VIPER: &str = "Myriad (Whenever this creature attacks, for each opponent other than defending player, you may create a token copy that's tapped and attacking that player or a planeswalker they control. Exile the tokens at end of combat.)\nWhenever this creature deals combat damage to a player, you may draw a card.";
const FLOOD_OF_MARS: &str = "Islandwalk (This creature can't be blocked as long as defending player controls an Island.)\nWater Always Wins — Whenever this creature attacks, put a flood counter on another target creature or land. If it's a creature, it becomes a copy of this creature. If it's a land, it becomes an Island in addition to its other types.";

fn creature(state: &mut GameState, card_id: u64, name: &str, power: i32) -> ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        PlayerId(0),
        name.to_string(),
        Zone::Battlefield,
    );
    let object = state.objects.get_mut(&id).unwrap();
    object.base_name = name.to_string();
    object.base_power = Some(power);
    object.base_toughness = Some(power);
    object.base_card_types = CardType {
        supertypes: vec![],
        core_types: vec![CoreType::Creature],
        subtypes: vec![],
    };
    id
}

/// `recipient` becomes a copy of `original` (CR 707.2, layer 1), through the
/// production `BecomeCopy` handler.
fn copy_onto(state: &mut GameState, recipient: ObjectId, original: ObjectId) {
    let mut events = Vec::new();
    become_copy::resolve(
        state,
        &ResolvedAbility::new(
            Effect::BecomeCopy {
                recipient: CopyRecipient::Source,
                target: TargetFilter::Any,
                duration: None,
                mana_value_limit: None,
                additional_modifications: Vec::new(),
            },
            vec![TargetRef::Object(original)],
            recipient,
            PlayerId(0),
        ),
        &mut events,
    )
    .unwrap();
    evaluate_layers(state);
    assert_eq!(
        state.objects[&recipient].name, state.objects[&original].name,
        "reach guard: the copy effect applies to the recipient"
    );
}

/// A "Copy Source" (1/1) that a copy effect (CR 707.2, layer 1) has turned
/// into a copy of "Target Bear" (2/2), so its copiable values differ from the
/// card's own: the ruling's "if any copy effects have affected the original".
fn source_copying_a_bear(state: &mut GameState) -> ObjectId {
    let bear = creature(state, 1, "Target Bear", 2);
    let source = creature(state, 2, "Copy Source", 1);
    copy_onto(state, source, bear);
    source
}

/// The source's self-copy trigger, latched to the source's current
/// incarnation on the battlefield.
fn self_copy_trigger(state: &GameState, source: ObjectId) -> ResolvedAbility {
    let mut ability = ResolvedAbility::new(
        Effect::CopyTokenOf {
            target: TargetFilter::SelfRef,
            owner: TargetFilter::Controller,
            source_filter: None,
            enters_attacking: false,
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            extra_keywords: vec![],
            additional_modifications: vec![],
        },
        vec![],
        source,
        PlayerId(0),
    );
    let object = &state.objects[&source];
    ability.set_test_trigger_source_recursive(object.incarnation, object.card_id);
    ability
}

/// The names of the tokens one resolution created.
fn resolve_copy(state: &mut GameState, ability: &ResolvedAbility) -> Vec<String> {
    let before: Vec<ObjectId> = state.battlefield.iter().copied().collect();
    let mut events = Vec::new();
    token_copy::resolve(state, ability, &mut events).unwrap();
    state
        .battlefield
        .iter()
        .filter(|id| !before.contains(id) && state.objects[id].is_token)
        .map(|id| state.objects[id].name.clone())
        .collect()
}

/// The tokens on the battlefield, in battlefield order.
fn tokens(state: &GameState) -> Vec<ObjectId> {
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| state.objects[id].is_token)
        .collect()
}

fn token_names(state: &GameState) -> Vec<String> {
    tokens(state)
        .iter()
        .map(|id| state.objects[id].name.clone())
        .collect()
}

fn mana(color: ManaType, count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect()
}

/// The index of the source's `CopyTokenOf` activated ability.
fn copy_ability_index(state: &GameState, source: ObjectId) -> usize {
    state.objects[&source]
        .abilities
        .iter()
        .position(|a| matches!(&*a.effect, Effect::CopyTokenOf { .. } | Effect::Encore))
        .expect("reach guard: the source has its copy ability")
}

/// Activate `source`'s copy ability through the production activation path
/// and pay its costs; returns once the ability is on the stack and a player
/// holds priority.
fn activate_and_pay(runner: &mut GameRunner, source: ObjectId, discard: Option<ObjectId>) {
    let ability_index = copy_ability_index(runner.state(), source);
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index,
        })
        .expect("the copy ability is activatable");
    for _ in 0..16 {
        match &runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("finalize mana");
            }
            WaitingFor::PayCost {
                kind: PayCostKind::Discard,
                ..
            } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![discard.expect("a discard cost needs a card")],
                    })
                    .expect("pay the discard cost");
            }
            WaitingFor::Priority { .. } => {
                assert_eq!(
                    runner.state().stack.len(),
                    1,
                    "reach guard: the costs were paid and the ability is on the stack"
                );
                return;
            }
            other => panic!("unexpected window while activating: {other:?}"),
        }
    }
    panic!("the activation did not reach priority");
}

/// The source incarnation the ability on top of the stack captured as it was
/// put on the stack (CR 400.7).
fn captured_source_incarnation(state: &GameState) -> Option<u64> {
    state
        .stack
        .last()
        .and_then(|entry| entry.ability())
        .and_then(|ability| ability.source_incarnation)
}

/// Pass priority (accepting "you may" choices and ordering triggers by
/// identity) until the stack is empty.
fn drive_to_empty_stack(runner: &mut GameRunner) {
    for _ in 0..64 {
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } | WaitingFor::ManaPayment { .. } => {
                GameAction::PassPriority
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            other => panic!("unexpected window while resolving: {other:?}"),
        };
        runner.act(action).expect("the drive's action is legal");
    }
    panic!("the stack did not empty");
}

/// The source leaves the battlefield (or the zone it was in) through the
/// production zone-move primitive, which records last-known information
/// (CR 400.7, CR 608.2h) as a removal spell cast in response would.
fn depart(runner: &mut GameRunner, id: ObjectId, to: Zone) {
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), id, to, &mut events);
}

/// Pass priority once, so state-based actions (CR 704.3) run before the next
/// player receives priority; the ability stays on the stack.
fn pass_once(runner: &mut GameRunner) {
    runner.act(GameAction::PassPriority).expect("pass priority");
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach guard: the ability has not resolved yet"
    );
}

// ---------------------------------------------------------------------------
// L1–L4: triggered own source (`CopyTokenOf { target: SelfRef }`).
// ---------------------------------------------------------------------------

/// L3 (preservation; sibling L1): a source still on the battlefield is copied
/// with its current copiable values, the copy effect included.
#[test]
fn self_copy_of_a_source_on_the_battlefield_copies_its_current_values() {
    let mut state = GameState::new_two_player(42);
    let source = source_copying_a_bear(&mut state);
    let ability = self_copy_trigger(&state, source);
    assert_eq!(resolve_copy(&mut state, &ability), vec!["Target Bear"]);
}

/// L1: a source that left the battlefield before its trigger resolved is
/// copied from its last-known copiable values (CR 608.2h), which include the
/// copy effect. The card now in the graveyard is a plain "Copy Source".
#[test]
fn self_copy_of_a_departed_source_copies_its_last_known_values() {
    let mut state = GameState::new_two_player(42);
    let source = source_copying_a_bear(&mut state);
    let ability = self_copy_trigger(&state, source);
    let mut events = Vec::new();
    move_to_zone(&mut state, source, Zone::Graveyard, &mut events);
    assert_eq!(
        state.objects[&source].name, "Copy Source",
        "reach guard: the card in the graveyard no longer has the copied values"
    );
    assert_eq!(resolve_copy(&mut state, &ability), vec!["Target Bear"]);
}

/// L2: a source that was exiled and returned (CR 400.7: a new object under the
/// same storage id) is not the trigger's source. The token copies the departed
/// incarnation's last-known values, never the returned "Copy Source", and
/// never the id-keyed record that the second departure (from exile)
/// overwrote.
#[test]
fn self_copy_of_a_returned_source_copies_the_departed_object() {
    let mut state = GameState::new_two_player(42);
    let source = source_copying_a_bear(&mut state);
    let ability = self_copy_trigger(&state, source);
    let latched = state.objects[&source].incarnation;
    let mut events = Vec::new();
    move_to_zone(&mut state, source, Zone::Exile, &mut events);
    move_to_zone(&mut state, source, Zone::Battlefield, &mut events);
    evaluate_layers(&mut state);
    let returned = &state.objects[&source];
    assert_eq!(returned.zone, Zone::Battlefield);
    assert!(
        returned.incarnation > latched,
        "CR 400.7 reach guard: the returned card keeps the storage id under a new incarnation"
    );
    assert_eq!(returned.name, "Copy Source");
    assert_eq!(
        state.lki_copiable_values[&source].name, "Copy Source",
        "hostile reach guard: the id-keyed record now describes the later object"
    );
    assert_eq!(resolve_copy(&mut state, &ability), vec!["Target Bear"]);
}

/// L4: Polyraptor is dealt lethal damage, so it is in the graveyard when its
/// enrage trigger resolves. The ruling: "the token will still enter the
/// battlefield as a copy of Polyraptor, using Polyraptor's copiable values
/// from when it was last on the battlefield."
#[test]
fn polyraptor_dealt_lethal_damage_still_creates_its_copy() {
    let mut scenario = GameScenario::new();
    let raptor = scenario
        .add_creature_from_oracle(P0, "Polyraptor", 5, 5, POLYRAPTOR)
        .with_subtypes(vec!["Dinosaur"])
        .with_damage_marked(2)
        .id();
    let bolt = scenario.add_bolt_to_hand(P0);
    let mut runner = scenario.build();
    runner.cast(bolt).target_object(raptor).resolve();
    runner.advance_until_stack_empty();
    let state = runner.state();
    assert_eq!(
        state.objects[&raptor].zone,
        Zone::Graveyard,
        "reach guard: the lethal damage killed the source"
    );
    let copies: Vec<_> = state
        .battlefield
        .iter()
        .map(|id| &state.objects[id])
        .filter(|object| object.is_token && object.name == "Polyraptor")
        .collect();
    assert_eq!(copies.len(), 1);
    assert_eq!((copies[0].power, copies[0].toughness), (Some(5), Some(5)));
    assert_eq!(copies[0].damage_marked, 0);
}

// ---------------------------------------------------------------------------
// L9–L11, L16: activated own source, through the production activation path.
// ---------------------------------------------------------------------------

/// L9: a Pack Rat token's ability is on the stack, and the token is destroyed
/// in response; it ceases to exist (CR 704.5d). Pack Rat's ruling: the ability
/// still creates a token, using the copiable values of Pack Rat as it last
/// existed on the battlefield.
#[test]
fn pack_rat_token_destroyed_in_response_still_creates_its_copy() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(ManaType::Black, 3));
    let rat = scenario
        .add_creature_from_oracle(P0, "Pack Rat", 1, 1, PACK_RAT)
        .with_subtypes(vec!["Rat"])
        .id();
    let fodder = scenario.add_card_to_hand(P0, "Fodder");
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&rat).unwrap().is_token = true;

    let latched = runner.state().objects[&rat].incarnation;
    activate_and_pay(&mut runner, rat, Some(fodder));
    assert_eq!(captured_source_incarnation(runner.state()), Some(latched));
    assert_eq!(
        runner.state().objects[&fodder].zone,
        Zone::Graveyard,
        "reach guard: the discard cost was paid"
    );
    depart(&mut runner, rat, Zone::Graveyard);
    pass_once(&mut runner);
    assert!(
        !runner.state().objects.contains_key(&rat),
        "reach guard: the token ceased to exist (CR 704.5d) before the ability resolved"
    );
    drive_to_empty_stack(&mut runner);

    assert_eq!(token_names(runner.state()), vec!["Pack Rat"]);
}

/// L10: a Myr Propagator under a copy effect (layer 1) leaves the battlefield
/// while its ability waits, and the card returns as a new object under the
/// same storage id (CR 400.7). The token copies the departed incarnation's
/// last-known values, which are the copied creature's (Myr Propagator's
/// rulings), not the returned object.
#[test]
fn myr_propagator_departed_and_returned_copies_the_departed_object() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(ManaType::Colorless, 3));
    let propagator = scenario
        .add_creature_from_oracle(P0, "Myr Propagator", 1, 1, MYR_PROPAGATOR)
        .with_subtypes(vec!["Myr"])
        .as_artifact_creature()
        .id();
    let bear = scenario.add_creature(P0, "Target Bear", 2, 2).id();
    let mut runner = scenario.build();
    let latched = runner.state().objects[&propagator].incarnation;

    activate_and_pay(&mut runner, propagator, None);
    assert_eq!(
        captured_source_incarnation(runner.state()),
        Some(latched),
        "C5b1.6: the activated ability captured the battlefield incarnation"
    );
    // The ruling's Cytoshape case: the source becomes a copy of another
    // creature after the activation and before the ability resolves.
    copy_onto(runner.state_mut(), propagator, bear);
    depart(&mut runner, propagator, Zone::Exile);
    depart(&mut runner, propagator, Zone::Battlefield);
    evaluate_layers(runner.state_mut());
    let returned = &runner.state().objects[&propagator];
    assert!(
        returned.incarnation > latched,
        "CR 400.7 reach guard: same storage id, later incarnation"
    );
    assert_eq!(returned.name, "Myr Propagator");
    drive_to_empty_stack(&mut runner);

    assert_eq!(token_names(runner.state()), vec!["Target Bear"]);
}

fn embalm_card(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_graveyard(P0, "Sacred Cat", 1, 1)
        .with_subtypes(vec!["Cat"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![engine::types::mana::ManaCostShard::White],
            generic: 0,
        })
        .with_color(vec![ManaColor::White])
        .from_oracle_text_with_keywords(&["Lifelink", "Embalm"], SACRED_CAT)
        .id()
}

/// The embalmed token is a copy of Sacred Cat with Embalm's exceptions
/// (CR 702.128a): white, a Zombie in addition to its other types, no mana
/// cost.
fn assert_embalmed_sacred_cat(state: &GameState) {
    let [token] = tokens(state)[..] else {
        panic!("one embalmed token, found {:?}", token_names(state));
    };
    let token = &state.objects[&token];
    assert_eq!(token.name, "Sacred Cat");
    assert_eq!((token.power, token.toughness), (Some(1), Some(1)));
    assert!(token.card_types.subtypes.iter().any(|s| s == "Zombie"));
    assert!(token.card_types.subtypes.iter().any(|s| s == "Cat"));
    assert_eq!(token.color, vec![ManaColor::White]);
    assert!(token.has_keyword(&Keyword::Lifelink));
}

/// L11 (preservation; sibling of L9 and L16): an embalmed card's token copies
/// the card's printed values with Embalm's exceptions, as at PHASE_BASE. The
/// card is in exile, where the cost put it, when the ability resolves.
#[test]
fn embalm_with_the_card_still_in_exile_copies_the_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(ManaType::White, 1));
    let cat = embalm_card(&mut scenario);
    let mut runner = scenario.build();

    activate_and_pay(&mut runner, cat, None);
    assert_eq!(
        runner.state().objects[&cat].zone,
        Zone::Exile,
        "reach guard: the cost exiled the card (CR 702.128a)"
    );
    drive_to_empty_stack(&mut runner);
    assert_eq!(
        runner.state().objects[&cat].zone,
        Zone::Exile,
        "reach guard: the card was in exile as the ability resolved"
    );
    assert_embalmed_sacred_cat(runner.state());
}

/// L16: the Embalm cost exiled the card; in response it returns from exile to
/// the battlefield, where it becomes a copy of another creature (layer 1).
/// The token copies the card's printed values with Embalm's exceptions
/// ("The token copies exactly what was printed on the original card"), its
/// last-known values in exile (CR 608.2h + CR 400.7j), not the object now
/// under the card's storage id.
#[test]
fn embalm_card_that_left_exile_still_copies_its_printed_values() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(ManaType::White, 1));
    let cat = embalm_card(&mut scenario);
    let bear = scenario.add_creature(P0, "Target Bear", 2, 2).id();
    let mut runner = scenario.build();

    activate_and_pay(&mut runner, cat, None);
    let exiled = runner.state().objects[&cat].incarnation;
    assert_eq!(runner.state().objects[&cat].zone, Zone::Exile);
    assert_eq!(
        captured_source_incarnation(runner.state()),
        Some(exiled),
        "C5b1.6: the ability captured the exile incarnation its cost created (CR 400.7j)"
    );
    depart(&mut runner, cat, Zone::Battlefield);
    copy_onto(runner.state_mut(), cat, bear);
    let returned = &runner.state().objects[&cat];
    assert!(returned.incarnation > exiled, "reach guard: a new object");
    assert_eq!(returned.name, "Target Bear");
    drive_to_empty_stack(&mut runner);

    assert_ne!(
        runner.state().objects[&cat].zone,
        Zone::Exile,
        "reach guard: the card was not in exile as the ability resolved"
    );
    let named_sacred_cat: Vec<_> = tokens(runner.state())
        .into_iter()
        .filter(|id| runner.state().objects[id].name == "Sacred Cat")
        .collect();
    assert_eq!(
        named_sacred_cat.len(),
        1,
        "tokens: {:?}",
        token_names(runner.state())
    );
    let token = &runner.state().objects[&named_sacred_cat[0]];
    assert!(token.card_types.subtypes.iter().any(|s| s == "Zombie"));
    assert_eq!(token.color, vec![ManaColor::White]);
}

// ---------------------------------------------------------------------------
// L12: handler-built own-source copy abilities (Myriad, Encore).
// ---------------------------------------------------------------------------

/// L12-ENCORE: L16's case for an Encore card (CR 702.141a). The Encore cost
/// exiled Briarblade Adept; in response the card returns to the battlefield
/// and becomes a copy of another creature. The token for the one opponent
/// copies the card's printed values in exile and gains haste.
#[test]
fn encore_card_that_left_exile_still_copies_its_printed_values() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(ManaType::Black, 4));
    let adept = scenario
        .add_creature_to_graveyard(P0, "Briarblade Adept", 3, 4)
        .with_subtypes(vec!["Elf", "Assassin"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![engine::types::mana::ManaCostShard::Black],
            generic: 4,
        })
        .from_oracle_text_with_keywords(&["Encore"], BRIARBLADE_ADEPT)
        .id();
    let bear = scenario.add_creature(P0, "Target Bear", 2, 2).id();
    let mut runner = scenario.build();

    activate_and_pay(&mut runner, adept, None);
    let exiled = runner.state().objects[&adept].incarnation;
    assert_eq!(
        captured_source_incarnation(runner.state()),
        Some(exiled),
        "C5b1.6: the ability captured the exile incarnation its cost created (CR 400.7j)"
    );
    assert_eq!(
        runner.state().objects[&adept].zone,
        Zone::Exile,
        "reach guard: the Encore cost exiled the card"
    );
    depart(&mut runner, adept, Zone::Battlefield);
    copy_onto(runner.state_mut(), adept, bear);
    assert!(runner.state().objects[&adept].incarnation > exiled);
    drive_to_empty_stack(&mut runner);

    let adepts: Vec<_> = tokens(runner.state())
        .into_iter()
        .filter(|id| runner.state().objects[id].name == "Briarblade Adept")
        .collect();
    assert_eq!(adepts.len(), 1, "tokens: {:?}", token_names(runner.state()));
    let token = &runner.state().objects[&adepts[0]];
    assert_eq!((token.power, token.toughness), (Some(3), Some(4)));
    assert!(token.has_keyword(&Keyword::Haste));
}

/// L12-MYRIAD: a Broodbirth Viper token attacks one of two opponents; its
/// myriad trigger (CR 702.116a) is on the stack and the token is destroyed in
/// response, so it ceases to exist (CR 704.5d). Myriad still creates one
/// token copy, tapped and attacking the other opponent, from the source's
/// last-known copiable values (CR 608.2h + CR 707.2).
#[test]
fn myriad_token_destroyed_in_response_still_creates_its_copy() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let viper = scenario
        .add_creature(P0, "Broodbirth Viper", 3, 3)
        .with_subtypes(vec!["Snake"])
        .from_oracle_text_with_keywords(&["Myriad"], BROODBIRTH_VIPER)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&viper).unwrap().is_token = true;
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(viper, engine::game::combat::AttackTarget::Player(P1))])
        .expect("the Viper attacks");
    for _ in 0..8 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => break,
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order");
            }
            other => panic!("unexpected window before myriad resolves: {other:?}"),
        }
    }
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach guard: myriad is on the stack"
    );
    depart(&mut runner, viper, Zone::Graveyard);
    pass_once(&mut runner);
    assert!(
        !runner.state().objects.contains_key(&viper),
        "reach guard: the token ceased to exist (CR 704.5d)"
    );
    drive_to_empty_stack(&mut runner);

    let state = runner.state();
    let [copy] = tokens(state)[..] else {
        panic!("one myriad copy, found {:?}", token_names(state));
    };
    assert_eq!(state.objects[&copy].name, "Broodbirth Viper");
    assert!(state.objects[&copy].tapped);
}

// ---------------------------------------------------------------------------
// L13–L15: `BecomeCopy` whose copy source is the ability's own source.
// ---------------------------------------------------------------------------

/// The Flood of Mars on P0's battlefield and "Target Bear" (another creature);
/// the Flood attacks and its trigger targets the Bear.
fn flood_attacks(token: bool) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let flood = scenario
        .add_creature_from_oracle(P0, "The Flood of Mars", 3, 3, FLOOD_OF_MARS)
        .with_subtypes(vec!["Alien", "Zombie", "Horror"])
        .id();
    let bear = scenario.add_creature(P0, "Target Bear", 2, 2).id();
    let giant = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&flood).unwrap().is_token = token;
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(flood, engine::game::combat::AttackTarget::Player(P1))])
        .expect("the Flood attacks");
    for _ in 0..8 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => break,
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(bear)),
                    })
                    .expect("target the Bear");
            }
            other => panic!("unexpected window before the trigger resolves: {other:?}"),
        }
    }
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach guard: the trigger waits"
    );
    // Labelled fixture: the card's "If it's a creature" parses as a reveal
    // condition (`RevealedHasCardType`), a pre-existing parse defect, so the
    // copy instruction never runs. Restore the condition the parser gives the
    // sibling "If it's a land" clause, so the row reaches `BecomeCopy`'s
    // source step through the production stack.
    let copy_step = runner
        .state_mut()
        .stack
        .get_mut(0)
        .and_then(|entry| entry.ability_mut())
        .and_then(|ability| ability.sub_ability.as_deref_mut())
        .expect("reach guard: the copy instruction follows the counter");
    assert!(matches!(&copy_step.effect, Effect::BecomeCopy { .. }));
    assert!(
        matches!(
            copy_step.condition,
            Some(engine::types::ability::AbilityCondition::RevealedHasCardType { .. })
        ),
        "reach guard: the PHASE_BASE parse of \"If it's a creature\""
    );
    copy_step.condition = Some(
        serde_json::from_str(
            r#"{"type":"TargetMatchesFilter","filter":{"type":"Typed","type_filters":["Creature"],"controller":null,"properties":[]},"use_lki":true}"#,
        )
        .expect("the land clause's condition shape"),
    );
    (runner, flood, bear, giant)
}

fn flood_counters(state: &GameState, id: ObjectId) -> u32 {
    state.objects[&id]
        .counters
        .iter()
        .filter(|(kind, _)| format!("{kind:?}").contains("lood"))
        .map(|(_, n)| *n)
        .sum()
}

/// L15 (sibling of L13): a live The Flood of Mars. The target creature becomes
/// a copy of its current copiable values. Red at PHASE_BASE: the source step
/// read the declared target (the creature itself) as the copy source.
#[test]
fn flood_of_mars_on_the_battlefield_turns_its_target_into_a_copy() {
    let (mut runner, _flood, bear, _giant) = flood_attacks(false);
    drive_to_empty_stack(&mut runner);
    let state = runner.state();
    assert_eq!(flood_counters(state, bear), 1, "reach guard: the counter");
    assert_eq!(state.objects[&bear].name, "The Flood of Mars");
}

/// L13: a token copy of The Flood of Mars attacks; the token is destroyed in
/// response and ceases to exist (CR 704.5d). When the trigger resolves, the
/// target creature becomes a copy of The Flood of Mars from its last-known
/// copiable values (CR 608.2h + CR 707.2).
#[test]
fn flood_of_mars_token_destroyed_in_response_still_turns_its_target_into_a_copy() {
    let (mut runner, flood, bear, _giant) = flood_attacks(true);
    depart(&mut runner, flood, Zone::Graveyard);
    pass_once(&mut runner);
    assert!(
        !runner.state().objects.contains_key(&flood),
        "reach guard: the token ceased to exist (CR 704.5d)"
    );
    drive_to_empty_stack(&mut runner);
    let state = runner.state();
    assert_eq!(flood_counters(state, bear), 1, "reach guard: the counter");
    assert_eq!(state.objects[&bear].name, "The Flood of Mars");
}

/// L14: a nontoken The Flood of Mars's trigger waits; the card leaves the
/// battlefield and returns as a new object under the same storage id
/// (CR 400.7), which then becomes a copy of Hill Giant (layer 1). The target
/// creature becomes a copy of The Flood of Mars's departed incarnation, not
/// of what the returned object is copying.
#[test]
fn flood_of_mars_departed_and_returned_turns_its_target_into_the_departed_object() {
    let (mut runner, flood, bear, giant) = flood_attacks(false);
    let latched = runner.state().objects[&flood].incarnation;
    depart(&mut runner, flood, Zone::Exile);
    depart(&mut runner, flood, Zone::Battlefield);
    copy_onto(runner.state_mut(), flood, giant);
    let returned = &runner.state().objects[&flood];
    assert!(returned.incarnation > latched, "reach guard: a new object");
    assert_eq!(returned.name, "Hill Giant");
    drive_to_empty_stack(&mut runner);
    let state = runner.state();
    assert_eq!(flood_counters(state, bear), 1, "reach guard: the counter");
    assert_eq!(state.objects[&bear].name, "The Flood of Mars");
}

// ---------------------------------------------------------------------------
// L17 (structural): the stack exit is written into the incarnation record.
// ---------------------------------------------------------------------------

/// Counter `spell` through the production `Counter` effect (CR 701.6a).
fn counter(runner: &mut GameRunner, spell: ObjectId) {
    let effect: Effect = serde_json::from_str(r#"{"type":"Counter","target":{"type":"Any"}}"#)
        .expect("a Counter effect");
    let ability = ResolvedAbility::new(effect, vec![TargetRef::Object(spell)], ObjectId(9_001), P1);
    let mut events = Vec::new();
    resolve_effect(runner.state_mut(), &ability, &mut events).expect("the counter resolves");
}

fn stack_record(state: &GameState, id: ObjectId, incarnation: u64) -> Option<String> {
    state
        .lki_copiable_values_by_incarnation
        .get(&id)
        .and_then(|by_incarnation| by_incarnation.get(&incarnation))
        .map(|values| values.name.clone())
}

/// L17-COUNTERED: a creature spell is countered; the record holds its stack
/// incarnation's copiable values, as the id-keyed record does.
#[test]
fn countered_spell_stack_exit_is_recorded_by_incarnation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(ManaType::Colorless, 2));
    let spell = scenario
        .add_creature_to_hand(P0, "Stack Bear", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let mut runner = scenario.build();
    runner.cast(spell).commit();
    let on_stack = runner.state().objects[&spell].incarnation;
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    counter(&mut runner, spell);
    let state = runner.state();
    assert_eq!(state.objects[&spell].zone, Zone::Graveyard, "reach guard");
    assert_eq!(
        stack_record(state, spell, on_stack).as_deref(),
        Some("Stack Bear")
    );
    assert_eq!(state.lki_copiable_values[&spell].name, "Stack Bear");
}

/// L17-RESOLVED: a creature spell resolves; its stack exit is recorded too.
#[test]
fn resolved_spell_stack_exit_is_recorded_by_incarnation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, mana(ManaType::Colorless, 2));
    let spell = scenario
        .add_creature_to_hand(P0, "Stack Bear", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let mut runner = scenario.build();
    runner.cast(spell).commit();
    let on_stack = runner.state().objects[&spell].incarnation;
    runner.advance_until_stack_empty();
    let state = runner.state();
    assert_eq!(state.objects[&spell].zone, Zone::Battlefield, "reach guard");
    assert_eq!(
        stack_record(state, spell, on_stack).as_deref(),
        Some("Stack Bear")
    );
}

/// L17-MDFC: a modal double-faced card cast as its back face (CR 712.11b) is
/// countered; the record holds the back face's values on the stack.
#[test]
fn countered_back_face_spell_is_recorded_with_its_cast_face() {
    let db = crate::support::shared_card_db().expect("the committed fixture loads");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let card = scenario.add_real_card(P0, "Peter Parker", Zone::Hand, db);
    scenario.with_mana_pool(
        P0,
        [
            ManaType::Green,
            ManaType::White,
            ManaType::Blue,
            ManaType::Green,
        ]
        .into_iter()
        .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect(),
    );
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    let card_id = runner.state().objects[&card].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: card,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("cast Peter Parker");
    runner
        .act(GameAction::ChooseModalFace { back_face: true })
        .expect("choose the back face");
    for _ in 0..8 {
        if let WaitingFor::ManaPayment { .. } = runner.state().waiting_for {
            runner.act(GameAction::PassPriority).expect("finalize mana");
        }
    }
    let object = &runner.state().objects[&card];
    assert_eq!(object.zone, Zone::Stack, "reach guard");
    assert_eq!(
        object.name, "Amazing Spider-Man",
        "reach guard: back face on the stack"
    );
    let on_stack = object.incarnation;
    counter(&mut runner, card);
    let state = runner.state();
    assert_eq!(
        stack_record(state, card, on_stack).as_deref(),
        Some("Amazing Spider-Man")
    );
}
