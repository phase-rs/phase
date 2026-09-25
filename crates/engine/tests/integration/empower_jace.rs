//! CR 701.71a: `Empower Jace N`, end to end.
//!
//! "If you don't control a Jace planeswalker token, create a blue Jace
//! planeswalker token with 0 loyalty, '[−1]: Surveil 1,' and '[−3]: Draw a
//! card.' Choose a Jace planeswalker token you control. Put N loyalty counters
//! on it."
//!
//! Every card below uses its verbatim MTGJSON Oracle text, printed reminder
//! parenthetical included. The one synthetic grammar row is the unbindable
//! where-X binder in `a2_12_where_x_binders_bind_or_stay_honest`; the synthetic
//! replacement definitions are not Oracle text.

use std::collections::BTreeSet;
use std::sync::Arc;

use engine::ai_support::candidate_actions_broad;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::token_presets::known_token_preset_by_id;
use engine::parser::oracle::{parse_oracle_text, ParsedAbilities};
use engine::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, DamageChannel,
    Effect, EffectKind, QuantityExpr, QuantityModification, QuantityRef, ReplacementDefinition,
    ReplacementMode, TargetFilter,
};
use engine::types::actions::{DebugAction, DebugTokenRequest, GameAction};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, ReplacementChoiceKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

/// FRA Jace planeswalker token (MTGJSON uuid). Same value as the phase-1
/// catalog row the created token must link to.
const JACE_TOKEN_PRESET_ID: &str = "635f825d-d6fb-59ac-a807-af08713a3794";

/// Protege's Awakening (verbatim MTGJSON Oracle text).
const PROTEGES_AWAKENING: &str = "Empower Jace 6. (Put six loyalty counters on a Jace token you control. If you don't control one, first create a blue Jace planeswalker token with \"[−1]: Surveil 1\" and \"[−3]: Draw a card.\")\nDraw a card.";
/// Way of the Necromancer (verbatim MTGJSON Oracle text).
const WAY_OF_THE_NECROMANCER: &str = "When Way of the Necromancer enters, empower Jace 2. (Put two loyalty counters on a Jace token you control. If you don't control one, first create a blue Jace planeswalker token with \"[−1]: Surveil 1\" and \"[−3]: Draw a card.\")\nWhenever a creature you control dies, put a loyalty counter on each planeswalker you control.";
/// Way of the Mentor (verbatim MTGJSON Oracle text).
const WAY_OF_THE_MENTOR: &str = "When Way of the Mentor enters, empower Jace 5. (Put five loyalty counters on a Jace token you control. If you don't control one, first create a blue Jace planeswalker token with \"[−1]: Surveil 1\" and \"[−3]: Draw a card.\")\nWhenever you gain life, put a loyalty counter on each planeswalker you control.";
/// Inspired Tethermage (verbatim MTGJSON Oracle text).
const INSPIRED_TETHERMAGE: &str = "Whenever you put one or more loyalty counters on a planeswalker, put a +1/+1 counter on this creature.\n{6}: Empower Jace 2. (Put two loyalty counters on a Jace token you control. If you don't control one, first create a blue Jace planeswalker token with \"[−1]: Surveil 1\" and \"[−3]: Draw a card.\")";
/// Repurposed Enforcer (verbatim MTGJSON Oracle text).
const REPURPOSED_ENFORCER: &str = "Whenever this creature attacks, empower Jace X, where X is the number of creatures you control. (Put that many loyalty counters on a Jace token you control. If you don't control one, first create a blue Jace planeswalker token with \"[−1]: Surveil 1\" and \"[−3]: Draw a card.\")";
/// Jace, Reality Sculptor (verbatim MTGJSON Oracle text).
const JACE_REALITY_SCULPTOR: &str = "[+1]: Empower Jace X, where X is the number of Islands you control.\n[−3]: Until your next turn, whenever a creature attacks you or a planeswalker you control, it gets -5/-0 until end of turn.\n[0]: Exile all but the bottom card of each opponent's library. Activate only if there are twenty-five or more loyalty counters among Jaces you control.";
/// Overwrite the Multiverse (verbatim MTGJSON Oracle text).
const OVERWRITE_THE_MULTIVERSE: &str = "Exile all creatures. Empower Jace X, where X is the number of creatures exiled this way. (Put that many loyalty counters on a Jace token you control. If you don't control one, first create a blue Jace planeswalker token with \"[−1]: Surveil 1\" and \"[−3]: Draw a card.\")";
/// Violent Echoes (verbatim MTGJSON Oracle text).
const VIOLENT_ECHOES: &str = "Violent Echoes deals 6 damage to target creature or planeswalker. If excess damage was dealt to that permanent this way, empower Jace X, where X is that excess damage. (Put that many loyalty counters on a Jace token you control. If you don't control one, first create a blue Jace planeswalker token with \"[−1]: Surveil 1\" and \"[−3]: Draw a card.\")";
/// Fatehold Charm (verbatim MTGJSON Oracle text).
const FATEHOLD_CHARM: &str = "Choose one —\n• Draw a card. Empower Jace 2.\n• Return target spell or creature to its owner's hand.\n• Creatures you control get +1/+2 until end of turn.";
/// Vraska's Final Mercy (verbatim MTGJSON Oracle text).
const VRASKAS_FINAL_MERCY: &str = "Choose one —\n• You lose 2 life. Destroy target creature or planeswalker.\n• You lose 2 life. Empower Jace 6. (Put six loyalty counters on a Jace token you control. If you don't control one, first create a blue Jace planeswalker token with \"[−1]: Surveil 1\" and \"[−3]: Draw a card.\")";
/// Avatar of Burgeoning Echoes (verbatim MTGJSON Oracle text).
const AVATAR_OF_BURGEONING_ECHOES: &str = "Landfall — Whenever a land you control enters, empower Jace 2. (Put two loyalty counters on a Jace token you control. If you don't control one, first create a blue Jace planeswalker token with \"[−1]: Surveil 1\" and \"[−3]: Draw a card.\")\nPlaneswalkers you control have \"[−10]: Put a +1/+1 counter on target creature for each land you control.\"";
/// Plan for All Outcomes (verbatim MTGJSON Oracle text).
const PLAN_FOR_ALL_OUTCOMES: &str = "When this enchantment enters, the owner of up to one other target nonland permanent puts it on their choice of the top or bottom of their library.\nWhenever you cast your first noncreature spell each turn, empower Jace 1. (Put a loyalty counter on a Jace token you control. If you don't control one, first create a blue Jace planeswalker token with \"[−1]: Surveil 1\" and \"[−3]: Draw a card.\")";
/// Theorist's Sanctum (verbatim MTGJSON Oracle text).
const THEORISTS_SANCTUM: &str = "({T}: Add {U}.)\nAs this land enters, you may behold a Jace. If you don't, this land enters tapped. (To behold a Jace, choose a Jace you control or reveal a Jace card from your hand.)\n{2}{U}, {T}: Empower Jace 2.";
/// Campus Crier (verbatim MTGJSON Oracle text).
const CAMPUS_CRIER: &str = "{1}, Exile this card from your graveyard: Empower Jace 2. (Put two loyalty counters on a Jace token you control. If you don't control one, first create a blue Jace planeswalker token with \"[−1]: Surveil 1\" and \"[−3]: Draw a card.\")";
/// Jace, the Mind Sculptor (verbatim MTGJSON Oracle text).
const JACE_THE_MIND_SCULPTOR: &str = "[+2]: Look at the top card of target player's library. You may put that card on the bottom of that player's library.\n[0]: Draw three cards, then put two cards from your hand on top of your library in any order.\n[−1]: Return target creature to its owner's hand.\n[−12]: Exile all cards from target player's library, then that player shuffles their hand into their library.";
/// Oath of Gideon (verbatim MTGJSON Oracle text).
const OATH_OF_GIDEON: &str = "When Oath of Gideon enters, create two 1/1 white Kor Ally creature tokens.\nEach planeswalker you control enters with an additional loyalty counter on it.";
/// Parallel Lives (verbatim MTGJSON Oracle text).
const PARALLEL_LIVES: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn main_phase_scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Library cards so a "Draw a card." rider has something to draw.
    scenario.with_library_top(P0, &["Library Card A", "Library Card B", "Library Card C"]);
    scenario
}

/// Seed one Jace planeswalker token for `owner` through the real debug-preset
/// `CreateToken` pipeline (the phase-1 `create_preset` shape), entering with
/// `loyalty` loyalty counters so it survives state-based actions. Debug mode is
/// on only for the seeding.
fn seed_jace_token(runner: &mut GameRunner, owner: PlayerId, loyalty: u32) -> ObjectId {
    runner.state_mut().debug_mode = true;
    runner
        .act(GameAction::Debug(DebugAction::CreateToken {
            request: DebugTokenRequest::Preset {
                preset_id: JACE_TOKEN_PRESET_ID.to_string(),
                owner,
                power_override: None,
                toughness_override: None,
                enter_with_counters: vec![(CounterType::Loyalty, loyalty)],
            },
            count: 1,
            run_etb: false,
        }))
        .expect("debug CreateToken must succeed");
    runner.state_mut().debug_mode = false;
    let created = runner.state().last_created_token_ids.clone();
    assert_eq!(created.len(), 1, "exactly one seeded Jace token");
    assert_eq!(
        runner.state().objects[&created[0]].loyalty,
        Some(loyalty),
        "reach-guard: the seeded token carries its entry loyalty"
    );
    created[0]
}

/// Jace planeswalker tokens `controller` controls on the battlefield.
fn jace_tokens(state: &GameState, controller: PlayerId) -> Vec<ObjectId> {
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            let obj = &state.objects[id];
            obj.is_token
                && obj.controller == controller
                && obj.card_types.core_types.contains(&CoreType::Planeswalker)
                && obj.card_types.subtypes.iter().any(|s| s == "Jace")
        })
        .collect()
}

fn loyalty(state: &GameState, id: ObjectId) -> Option<u32> {
    state.objects[&id].loyalty
}

/// Ids named by `TokenCreated` events, in event order.
fn created_token_ids(events: &[GameEvent]) -> Vec<ObjectId> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::TokenCreated { object_id, .. } => Some(*object_id),
            _ => None,
        })
        .collect()
}

fn empower_resolved_count(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::EmpowerJace,
                    ..
                }
            )
        })
        .count()
}

fn card_drawn_count(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, GameEvent::CardDrawn { .. }))
        .count()
}

fn loyalty_added(events: &[GameEvent], id: ObjectId, n: u32) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            GameEvent::CounterAdded { object_id, counter_type: CounterType::Loyalty, count, .. }
                if *object_id == id && *count == n
        )
    })
}

fn position_of(events: &[GameEvent], pred: impl Fn(&GameEvent) -> bool) -> Option<usize> {
    events.iter().position(pred)
}

fn loyalty_added_pos(events: &[GameEvent], id: ObjectId, n: u32) -> Option<usize> {
    position_of(events, |event| {
        matches!(
            event,
            GameEvent::CounterAdded { object_id, counter_type: CounterType::Loyalty, count, .. }
                if *object_id == id && *count == n
        )
    })
}

fn empower_resolved_pos(events: &[GameEvent]) -> Option<usize> {
    position_of(events, |event| {
        matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::EmpowerJace,
                ..
            }
        )
    })
}

fn card_drawn_pos(events: &[GameEvent]) -> Option<usize> {
    position_of(events, |event| matches!(event, GameEvent::CardDrawn { .. }))
}

fn colorless_mana(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

/// Add an "Empower Jace N" host spell to P0's hand as a sorcery.
fn add_sorcery(scenario: &mut GameScenario, name: &str, text: &str) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, name, false, text)
        .id()
}

/// Add a (legendary) enchantment card to P0's hand, castable as a permanent
/// spell whose own triggers are parsed with its real type line.
fn add_enchantment_to_hand(scenario: &mut GameScenario, name: &str, text: &str) -> ObjectId {
    scenario
        .add_spell_to_hand(P0, name, false)
        .as_enchantment()
        .from_oracle_text(text)
        .id()
}

/// CR 616.1: an optional "you may" token-creation replacement for P0's tokens
/// (the R-NC fixture). Index 0 accepts, index 1 declines.
fn optional_create_token_replacement() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::CreateToken)
        .token_owner_scope(ControllerRef::You)
        .mode(ReplacementMode::Optional { decline: None })
}

/// CR 614.1 + CR 616.1: an optional counter-doubling replacement (the P-a
/// fixture). Index 0 accepts, doubling the placed counters.
fn optional_add_counter_doubler() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::AddCounter)
        .valid_card(TargetFilter::Any)
        .quantity_modification(QuantityModification::DOUBLE)
        .mode(ReplacementMode::Optional { decline: None })
}

/// Install a replacement on an existing battlefield object after the runner
/// is built (used when Jace tokens must be seeded before the replacement
/// exists, so the seeding's own entry counters are not offered to it).
fn install_replacement(runner: &mut GameRunner, host: ObjectId, def: ReplacementDefinition) {
    let obj = runner.state_mut().objects.get_mut(&host).unwrap();
    obj.replacement_definitions.push(def.clone());
    Arc::make_mut(&mut obj.base_replacement_definitions).push(def);
}

fn act_events(runner: &mut GameRunner, action: GameAction) -> Vec<GameEvent> {
    runner
        .act(action)
        .expect("action must be accepted by the engine")
        .events
}

fn select(runner: &mut GameRunner, id: ObjectId) -> Vec<GameEvent> {
    act_events(runner, GameAction::SelectCards { cards: vec![id] })
}

fn choose_replacement(runner: &mut GameRunner, index: usize) -> Vec<GameEvent> {
    act_events(runner, GameAction::ChooseReplacement { index })
}

fn empower_choices(waiting_for: &WaitingFor) -> BTreeSet<ObjectId> {
    match waiting_for {
        WaitingFor::EmpowerJaceChoice { choices, .. } => choices.iter().copied().collect(),
        other => panic!("expected EmpowerJaceChoice, got {other:?}"),
    }
}

fn parse(
    text: &str,
    name: &str,
    keywords: &[&str],
    types: &[&str],
    subtypes: &[&str],
) -> ParsedAbilities {
    let keywords: Vec<String> = keywords.iter().map(|s| s.to_string()).collect();
    let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
    let subtypes: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
    parse_oracle_text(text, name, &keywords, &types, &subtypes)
}

/// Every effect in an ability chain (`effect`, then `sub_ability` /
/// `else_ability`, depth first).
fn chain_effects(def: &AbilityDefinition) -> Vec<&Effect> {
    let mut out = vec![def.effect.as_ref()];
    if let Some(sub) = &def.sub_ability {
        out.extend(chain_effects(sub));
    }
    if let Some(other) = &def.else_ability {
        out.extend(chain_effects(other));
    }
    out
}

fn is_empower(effect: &Effect) -> bool {
    matches!(effect, Effect::EmpowerJace { .. })
}

fn empower_count(effect: &Effect) -> &QuantityExpr {
    match effect {
        Effect::EmpowerJace { count } => count,
        other => panic!("expected EmpowerJace, got {other:?}"),
    }
}

/// Every top-level ability chain of a parse: spell/activated abilities and
/// trigger executes.
fn all_chains(parsed: &ParsedAbilities) -> Vec<&AbilityDefinition> {
    parsed
        .abilities
        .iter()
        .chain(parsed.triggers.iter().filter_map(|t| t.execute.as_deref()))
        .collect()
}

/// The single chain carrying an EmpowerJace node, asserting exactly one
/// EmpowerJace node across the whole parse and no `Unimplemented` in that
/// chain.
fn sole_empower_chain<'a>(parsed: &'a ParsedAbilities, card: &str) -> &'a AbilityDefinition {
    let chains = all_chains(parsed);
    let total: usize = chains
        .iter()
        .map(|c| {
            chain_effects(c)
                .into_iter()
                .filter(|e| is_empower(e))
                .count()
        })
        .sum();
    assert_eq!(
        total, 1,
        "{card}: exactly one EmpowerJace node, parse: {parsed:#?}"
    );
    let chain = chains
        .into_iter()
        .find(|c| chain_effects(c).into_iter().any(is_empower))
        .unwrap();
    assert!(
        !chain_effects(chain)
            .into_iter()
            .any(|e| matches!(e, Effect::Unimplemented { .. })),
        "{card}: no Unimplemented under the Empower ability: {chain:#?}"
    );
    chain
}

// ---------------------------------------------------------------------------
// A2.1 – A2.4: existence test, creation, choice, candidate predicate
// ---------------------------------------------------------------------------

/// A2.1 — CR 701.71a: with no Jace token, empower creates one and puts N
/// loyalty counters on it. Both the creation event and the counter event are
/// asserted, and the created body links to the phase-1 catalog row.
#[test]
fn a2_1_no_jace_token_creates_one_with_n_loyalty() {
    let mut scenario = main_phase_scenario();
    let spell = add_sorcery(&mut scenario, "Protege's Awakening", PROTEGES_AWAKENING);
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).resolve();
    let events = outcome.events();
    let state = outcome.state();

    let tokens = jace_tokens(state, P0);
    assert_eq!(tokens.len(), 1, "exactly one Jace token");
    let token = tokens[0];
    assert_eq!(
        created_token_ids(events),
        vec![token],
        "TokenCreated names the Jace token"
    );
    assert!(
        loyalty_added(events, token, 6),
        "CounterAdded {{ Loyalty, 6 }} on the token"
    );
    assert_eq!(loyalty(state, token), Some(6));

    // Phase-1 injection reached: the two registry loyalty abilities.
    let costs: Vec<i32> = state.objects[&token]
        .abilities
        .iter()
        .filter_map(|a| match a.cost {
            Some(AbilityCost::Loyalty { amount }) => Some(amount),
            _ => None,
        })
        .collect();
    assert_eq!(costs, vec![-1, -3]);

    // The literal token body matches the catalog body, so the created token
    // links to the FRA Jace preset.
    let obj = &state.objects[&token];
    assert_eq!(
        obj.token_image_ref.as_ref().map(|r| r.preset_id.as_str()),
        Some(JACE_TOKEN_PRESET_ID)
    );
    assert_eq!(
        obj.token_image_ref,
        known_token_preset_by_id(JACE_TOKEN_PRESET_ID)
            .unwrap()
            .token_image_ref
    );

    assert_eq!(empower_resolved_count(events), 1);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

/// A2.2 — CR 701.71a: with exactly one Jace token, no second token is created
/// and the counters land on the existing one.
#[test]
fn a2_2_existing_jace_token_receives_the_counters() {
    let mut scenario = main_phase_scenario();
    let way = add_enchantment_to_hand(
        &mut scenario,
        "Way of the Necromancer",
        WAY_OF_THE_NECROMANCER,
    );
    let mut runner = scenario.build();
    let seeded = seed_jace_token(&mut runner, P0, 1);

    let outcome = runner.cast(way).resolve();
    let events = outcome.events();
    let state = outcome.state();

    // Reach guard for "creation absent": the counter event is present.
    assert!(
        loyalty_added(events, seeded, 2),
        "CounterAdded on the seeded token"
    );
    assert!(created_token_ids(events).is_empty(), "no TokenCreated");
    assert_eq!(jace_tokens(state, P0), vec![seeded]);
    assert_eq!(loyalty(state, seeded), Some(3));
    assert_eq!(empower_resolved_count(events), 1);
}

/// A2.2b — CR 701.71a: "a Jace planeswalker token you control" — an
/// opponent's Jace token neither satisfies the existence test nor receives
/// the counters.
#[test]
fn a2_2b_opponents_jace_token_does_not_count() {
    let mut scenario = main_phase_scenario();
    let way = add_enchantment_to_hand(
        &mut scenario,
        "Way of the Necromancer",
        WAY_OF_THE_NECROMANCER,
    );
    let mut runner = scenario.build();
    let theirs = seed_jace_token(&mut runner, P1, 1);

    let outcome = runner.cast(way).resolve();
    let events = outcome.events();
    let state = outcome.state();

    let mine = jace_tokens(state, P0);
    assert_eq!(mine.len(), 1, "P0's empower created P0's own token");
    assert_eq!(created_token_ids(events), mine);
    assert_eq!(loyalty(state, mine[0]), Some(2));
    assert_eq!(loyalty(state, theirs), Some(1), "P1's token is untouched");
    assert!(!events.iter().any(|e| matches!(
        e,
        GameEvent::CounterAdded { object_id, .. } if *object_id == theirs
    )));
}

/// Two seeded Jace tokens, Protege's Awakening cast and resolved to the
/// EmpowerJaceChoice pause. Returns (runner-owned ids A, B, cast events).
fn two_tokens_at_pause(
    runner: &mut GameRunner,
    spell: ObjectId,
) -> (ObjectId, ObjectId, Vec<GameEvent>) {
    let a = seed_jace_token(runner, P0, 1);
    let b = seed_jace_token(runner, P0, 1);
    let outcome = runner.cast(spell).resolve();
    // Reach guard: the pause is observed.
    assert_eq!(
        empower_choices(outcome.final_waiting_for()),
        BTreeSet::from([a, b]),
        "EmpowerJaceChoice offers exactly the two Jace tokens"
    );
    (a, b, outcome.events().to_vec())
}

/// A2.3 — CR 701.71a + CR 608.2d: with two Jace tokens, resolution pauses on a
/// real choice and the counters land on the token actually chosen (the SECOND
/// candidate), not the first.
#[test]
fn a2_3_two_tokens_pause_and_the_chosen_one_gets_the_counters() {
    let mut scenario = main_phase_scenario();
    let spell = add_sorcery(&mut scenario, "Protege's Awakening", PROTEGES_AWAKENING);
    let mut runner = scenario.build();
    let (a, b, _) = two_tokens_at_pause(&mut runner, spell);

    let events = select(&mut runner, b);
    assert!(loyalty_added(&events, b, 6));
    assert_eq!(loyalty(runner.state(), b), Some(7));
    assert_eq!(
        loyalty(runner.state(), a),
        Some(1),
        "the unchosen token is unchanged"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// A2.3b — CR 608.2d: a non-candidate submission is rejected and the pause
/// stays open; a candidate is then accepted.
#[test]
fn a2_3b_non_member_submission_is_rejected() {
    let mut scenario = main_phase_scenario();
    let bystander = scenario.add_creature(P0, "Bystander", 2, 2).id();
    let spell = add_sorcery(&mut scenario, "Protege's Awakening", PROTEGES_AWAKENING);
    let mut runner = scenario.build();
    let (_a, b, _) = two_tokens_at_pause(&mut runner, spell);
    let before = runner.state().waiting_for.clone();

    let result = runner.act(GameAction::SelectCards {
        cards: vec![bystander],
    });
    assert!(
        result.is_err(),
        "a creature is not a Jace token you control"
    );
    assert_eq!(runner.state().waiting_for, before, "the pause is unchanged");

    // The pause is still live: a real candidate is accepted.
    select(&mut runner, b);
    assert_eq!(loyalty(runner.state(), b), Some(7));
}

/// A2.4 / C2.6 — CR 701.71a + CR 111.1: a card-backed Jace planeswalker does
/// not satisfy the existence test and is not a legal choice: empower creates a
/// token (no pause — the token is the single candidate) and the card is
/// untouched.
#[test]
fn a2_4_card_backed_jace_is_not_a_candidate() {
    let mut scenario = main_phase_scenario();
    let jtms = scenario
        .add_planeswalker_from_oracle(
            P0,
            "Jace, the Mind Sculptor",
            "Jace",
            3,
            JACE_THE_MIND_SCULPTOR,
        )
        .id();
    let spell = add_sorcery(&mut scenario, "Protege's Awakening", PROTEGES_AWAKENING);
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).resolve();
    let events = outcome.events();
    let state = outcome.state();

    let tokens = jace_tokens(state, P0);
    assert_eq!(tokens.len(), 1);
    assert_eq!(
        created_token_ids(events),
        tokens,
        "TokenCreated despite the card-backed Jace"
    );
    assert_eq!(loyalty(state, tokens[0]), Some(6));
    assert_eq!(
        loyalty(state, jtms),
        Some(3),
        "the card-backed Jace is unchanged"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "no pause"
    );
}

// ---------------------------------------------------------------------------
// A2.5: dynamic N
// ---------------------------------------------------------------------------

/// Repurposed Enforcer attacks with `others` other creatures on P0's side;
/// returns the created Jace token's loyalty.
fn enforcer_attack_loyalty(others: usize) -> u32 {
    let mut scenario = main_phase_scenario();
    let enforcer = scenario
        .add_creature_from_oracle(P0, "Repurposed Enforcer", 2, 2, REPURPOSED_ENFORCER)
        .id();
    for i in 0..others {
        scenario.add_creature(P0, &format!("Other {i}"), 1, 1);
    }
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(enforcer, AttackTarget::Player(P1))])
        .expect("declare attackers");
    runner.advance_until_stack_empty();
    let tokens = jace_tokens(runner.state(), P0);
    assert_eq!(tokens.len(), 1, "the attack trigger created one Jace token");
    loyalty(runner.state(), tokens[0]).unwrap()
}

/// Jace, Reality Sculptor's [+1] with `islands` Islands; returns the created
/// Jace token's loyalty.
fn reality_sculptor_plus_one_loyalty(islands: usize) -> u32 {
    let mut scenario = main_phase_scenario();
    let rs = scenario
        .add_planeswalker_from_oracle(
            P0,
            "Jace, Reality Sculptor",
            "Jace",
            5,
            JACE_REALITY_SCULPTOR,
        )
        .id();
    for _ in 0..islands {
        scenario.add_basic_land(P0, ManaColor::Blue);
    }
    let mut runner = scenario.build();
    let outcome = runner.activate(rs, 0).resolve();
    let tokens = jace_tokens(outcome.state(), P0);
    assert_eq!(tokens.len(), 1, "[+1] created one Jace token");
    loyalty(outcome.state(), tokens[0]).unwrap()
}

/// A2.5 — CR 608.2h + CR 107.3c: N is the dynamic form, not a constant that
/// happens to match — the parsed count is an object-count reference, and two
/// board states yield two counts on each host.
#[test]
fn a2_5_dynamic_n_resolves_from_game_state() {
    // SHAPE: the attack trigger's count is the "creatures you control" count.
    let parsed = parse(
        REPURPOSED_ENFORCER,
        "Repurposed Enforcer",
        &["Surveil"],
        &["Creature"],
        &["Human", "Soldier"],
    );
    let chain = sole_empower_chain(&parsed, "Repurposed Enforcer");
    assert!(
        matches!(
            empower_count(&chain.effect),
            QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { .. }
            }
        ),
        "dynamic count, got {:?}",
        empower_count(&chain.effect)
    );

    // Runtime: two creatures vs four.
    assert_eq!(enforcer_attack_loyalty(1), 2);
    assert_eq!(enforcer_attack_loyalty(3), 4);

    // Loyalty host: one Island vs three.
    assert_eq!(reality_sculptor_plus_one_loyalty(1), 1);
    assert_eq!(reality_sculptor_plus_one_loyalty(3), 3);

    // Spell host: three creatures exiled this way.
    let mut scenario = main_phase_scenario();
    scenario.add_creature(P0, "Mine", 1, 1);
    scenario.add_creature(P1, "Theirs A", 1, 1);
    scenario.add_creature(P1, "Theirs B", 1, 1);
    let spell = add_sorcery(
        &mut scenario,
        "Overwrite the Multiverse",
        OVERWRITE_THE_MULTIVERSE,
    );
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();
    let tokens = jace_tokens(outcome.state(), P0);
    assert_eq!(tokens.len(), 1);
    assert_eq!(loyalty(outcome.state(), tokens[0]), Some(3));
}

// ---------------------------------------------------------------------------
// A2.6 / A2.7 / A2.12: parser — the same leaf in every host shape
// ---------------------------------------------------------------------------

fn assert_root_empower(def: &AbilityDefinition, card: &str) {
    assert!(
        is_empower(&def.effect),
        "{card}: EmpowerJace at the chain root, got {def:#?}"
    );
}

/// Violent Echoes (C2.7 / M6): the Empower consequent is the damage's
/// `sub_ability`, gated on `PreviousEffectAmount { channel: Excess }`, and is an
/// honest `Unimplemented` node — no EmpowerJace anywhere in the parse.
fn assert_violent_echoes_consequent_is_honest_and_nested() {
    let p = parse(
        VIOLENT_ECHOES,
        "Violent Echoes",
        &["Surveil"],
        &["Instant"],
        &[],
    );
    assert!(
        !all_chains(&p)
            .into_iter()
            .flat_map(chain_effects)
            .any(is_empower),
        "Violent Echoes: no EmpowerJace with an unbound X: {p:#?}"
    );
    let root = &p.abilities[0];
    assert!(
        matches!(*root.effect, Effect::DealDamage { .. }),
        "Violent Echoes: damage leads: {p:#?}"
    );
    let sub = root
        .sub_ability
        .as_deref()
        .expect("the empower clause is the damage's sub_ability");
    assert!(
        matches!(*sub.effect, Effect::Unimplemented { .. }),
        "Violent Echoes: the consequent is an honest Unimplemented node: {sub:#?}"
    );
    assert!(
        matches!(
            sub.condition,
            Some(AbilityCondition::PreviousEffectAmount {
                channel: DamageChannel::Excess,
                ..
            })
        ),
        "Violent Echoes: the consequent is gated on excess damage, got {:?}",
        sub.condition
    );
}

/// A2.6 + C2.9 — CR 701.71a: every host shape the corpus uses parses the full
/// printed line (reminder parenthetical included) to exactly one EmpowerJace
/// leaf at its expected position.
#[test]
fn a2_6_every_host_shape_parses_to_the_empower_leaf() {
    // Trailing spell effect: "Exile all creatures. Empower Jace X, …".
    let p = parse(
        OVERWRITE_THE_MULTIVERSE,
        "Overwrite the Multiverse",
        &["Surveil"],
        &["Sorcery"],
        &[],
    );
    let chain = sole_empower_chain(&p, "Overwrite");
    assert_eq!(chain.kind, AbilityKind::Spell);
    assert!(
        !is_empower(&chain.effect),
        "Overwrite: the exile leads, empower trails"
    );
    assert!(chain_effects(chain).into_iter().skip(1).any(is_empower));

    // Leading spell effect: "Empower Jace 6." then "Draw a card.".
    let p = parse(
        PROTEGES_AWAKENING,
        "Protege's Awakening",
        &["Surveil"],
        &["Sorcery"],
        &[],
    );
    let chain = sole_empower_chain(&p, "Protege");
    assert_eq!(chain.kind, AbilityKind::Spell);
    assert_root_empower(chain, "Protege");
    assert_eq!(
        empower_count(&chain.effect),
        &QuantityExpr::Fixed { value: 6 }
    );

    // Enters trigger.
    let p = parse(
        WAY_OF_THE_MENTOR,
        "Way of the Mentor",
        &["Surveil"],
        &["Enchantment"],
        &[],
    );
    let chain = sole_empower_chain(&p, "Way of the Mentor");
    assert!(
        p.triggers[0].execute.as_deref() == Some(chain),
        "Mentor: the ETB trigger's effect"
    );
    assert_root_empower(chain, "Way of the Mentor");
    assert_eq!(
        empower_count(&chain.effect),
        &QuantityExpr::Fixed { value: 5 }
    );

    // Attack trigger.
    let p = parse(
        REPURPOSED_ENFORCER,
        "Repurposed Enforcer",
        &["Surveil"],
        &["Creature"],
        &["Human", "Soldier"],
    );
    let chain = sole_empower_chain(&p, "Repurposed Enforcer");
    assert!(
        p.triggers[0].execute.as_deref() == Some(chain),
        "Enforcer: the attack trigger's effect"
    );
    assert_root_empower(chain, "Repurposed Enforcer");

    // Landfall.
    let p = parse(
        AVATAR_OF_BURGEONING_ECHOES,
        "Avatar of Burgeoning Echoes",
        &["Landfall", "Surveil"],
        &["Creature"],
        &["Avatar"],
    );
    let chain = sole_empower_chain(&p, "Avatar");
    assert!(
        p.triggers[0].execute.as_deref() == Some(chain),
        "Avatar: the landfall trigger's effect"
    );
    assert_root_empower(chain, "Avatar");

    // Cast trigger (the enchantment's second trigger).
    let p = parse(
        PLAN_FOR_ALL_OUTCOMES,
        "Plan for All Outcomes",
        &["Surveil"],
        &["Enchantment"],
        &[],
    );
    let chain = sole_empower_chain(&p, "Plan for All Outcomes");
    assert!(
        p.triggers[1].execute.as_deref() == Some(chain),
        "Plan: the cast trigger's effect"
    );
    assert_root_empower(chain, "Plan for All Outcomes");
    assert_eq!(
        empower_count(&chain.effect),
        &QuantityExpr::Fixed { value: 1 }
    );

    // Activated, mana cost.
    let p = parse(
        INSPIRED_TETHERMAGE,
        "Inspired Tethermage",
        &["Surveil"],
        &["Creature"],
        &["Elf", "Warrior"],
    );
    let chain = sole_empower_chain(&p, "Tethermage");
    assert_eq!(chain.kind, AbilityKind::Activated);
    assert!(
        matches!(chain.cost, Some(AbilityCost::Mana { .. })),
        "Tethermage cost: {:?}",
        chain.cost
    );
    assert_root_empower(chain, "Tethermage");

    // Activated, mana + tap.
    let p = parse(
        THEORISTS_SANCTUM,
        "Theorist's Sanctum",
        &["Behold"],
        &["Land"],
        &["Island"],
    );
    let chain = sole_empower_chain(&p, "Theorist's Sanctum");
    assert_eq!(chain.kind, AbilityKind::Activated);
    let cost = format!("{:?}", chain.cost);
    assert!(
        cost.contains("Mana") && cost.contains("Tap"),
        "Sanctum cost: {cost}"
    );
    assert_root_empower(chain, "Theorist's Sanctum");

    // Activated, graveyard exile cost.
    let p = parse(
        CAMPUS_CRIER,
        "Campus Crier",
        &["Surveil"],
        &["Creature"],
        &["Human", "Advisor"],
    );
    let chain = sole_empower_chain(&p, "Campus Crier");
    assert_eq!(chain.kind, AbilityKind::Activated);
    let cost = format!("{:?}", chain.cost);
    assert!(cost.contains("Exile"), "Crier cost: {cost}");
    assert_eq!(chain.activation_zone, Some(Zone::Graveyard));
    assert_root_empower(chain, "Campus Crier");

    // Loyalty ability (relies on the walker mask in `normalize_card_name_refs`).
    let p = parse(
        JACE_REALITY_SCULPTOR,
        "Jace, Reality Sculptor",
        &[],
        &["Planeswalker"],
        &["Jace"],
    );
    let chain = sole_empower_chain(&p, "Jace, Reality Sculptor");
    assert_eq!(chain.cost, Some(AbilityCost::Loyalty { amount: 1 }));
    assert_root_empower(chain, "Jace, Reality Sculptor");

    // Modal bullets: mode 0 of Fatehold Charm, mode 1 of Vraska's Final Mercy.
    let p = parse(FATEHOLD_CHARM, "Fatehold Charm", &[], &["Instant"], &[]);
    assert!(p.modal.is_some(), "Fatehold Charm is modal");
    let chain = sole_empower_chain(&p, "Fatehold Charm");
    assert!(std::ptr::eq(chain, &p.abilities[0]), "Fatehold: mode 0");
    assert!(
        matches!(*chain.effect, Effect::Draw { .. }),
        "Fatehold: draw leads"
    );
    assert!(chain_effects(chain).into_iter().skip(1).any(is_empower));
    let p = parse(
        VRASKAS_FINAL_MERCY,
        "Vraska's Final Mercy",
        &["Surveil"],
        &["Sorcery"],
        &[],
    );
    assert!(p.modal.is_some(), "Vraska's Final Mercy is modal");
    let chain = sole_empower_chain(&p, "Vraska's Final Mercy");
    assert!(std::ptr::eq(chain, &p.abilities[1]), "Vraska: mode 1");
    assert!(
        matches!(*chain.effect, Effect::LoseLife { .. }),
        "Vraska: life loss leads"
    );
    assert!(chain_effects(chain).into_iter().skip(1).any(is_empower));

    // Conditional consequent (C2.7 / M6): the Empower clause sits nested under
    // the excess-damage condition, not as a sibling of the damage. Its "where X
    // is that excess damage" binder reaches the recogniser unstripped on this
    // path, so the consequent stays an honest Unimplemented node (the card stays
    // unsupported) rather than an EmpowerJace carrying a bare X.
    assert_violent_echoes_consequent_is_honest_and_nested();
}

/// A2.7 — CR 701.71a: a clause naming another walker is not this keyword
/// action, and the matching Jace clause is — in the same test.
#[test]
fn a2_7_another_walker_is_not_empower_jace() {
    let bolas = parse("Empower Bolas 2.", "Test Sorcery", &[], &["Sorcery"], &[]);
    assert!(
        !all_chains(&bolas)
            .into_iter()
            .flat_map(chain_effects)
            .any(is_empower),
        "Empower Bolas must not parse as EmpowerJace: {bolas:#?}"
    );
    let jace = parse("Empower Jace 2.", "Test Sorcery", &[], &["Sorcery"], &[]);
    let chain = sole_empower_chain(&jace, "Empower Jace 2.");
    assert_eq!(
        empower_count(&chain.effect),
        &QuantityExpr::Fixed { value: 2 }
    );
}

/// A2.12 — CR 701.71a + CR 107.3c: a resolvable binder binds N; an
/// unresolvable binder stays an honest unimplemented node rather than an
/// EmpowerJace carrying a bare X (which would resolve to 0).
#[test]
fn a2_12_where_x_binders_bind_or_stay_honest() {
    // A2.12a: "exiled this way".
    let p = parse(
        OVERWRITE_THE_MULTIVERSE,
        "Overwrite the Multiverse",
        &["Surveil"],
        &["Sorcery"],
        &[],
    );
    let chain = sole_empower_chain(&p, "Overwrite");
    let count = chain_effects(chain)
        .into_iter()
        .find(|e| is_empower(e))
        .map(empower_count)
        .unwrap();
    assert!(
        matches!(
            count,
            QuantityExpr::Ref {
                qty: QuantityRef::TrackedSetSize | QuantityRef::FilteredTrackedSetSize { .. }
            }
        ),
        "Overwrite: N is the exiled-this-way count, got {count:?}"
    );
    // A2.12a: "that excess damage" (M6 direction): `parse_count_expr` cannot
    // bind this description, so the recogniser rejects the clause and the
    // consequent stays honest.
    assert_violent_echoes_consequent_is_honest_and_nested();

    // A2.12b (synthetic grammar row): an unbindable binder.
    let p = parse(
        "Empower Jace X, where X is the number of glorbs.",
        "Test Sorcery",
        &[],
        &["Sorcery"],
        &[],
    );
    // The recogniser's fail-closed tail rejects the unstripped binder residue
    // (`parse_count_expr` binds "where X is" only when the description parses),
    // so the clause stays an honest `Unimplemented` gap carrying the binder text
    // — never an EmpowerJace with a surviving `Variable("X")`.
    let effects: Vec<&Effect> = all_chains(&p).into_iter().flat_map(chain_effects).collect();
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::Unimplemented { description, .. }
                if description.as_deref().is_some_and(|d| d.to_lowercase().contains("where x is the number of glorbs"))
        )),
        "the unbindable binder stays an honest Unimplemented gap: {p:#?}"
    );
    assert!(
        !effects.iter().any(|e| is_empower(e)),
        "no EmpowerJace with a bare X survives: {p:#?}"
    );
}

// ---------------------------------------------------------------------------
// A2.8 – A2.11: N = 0, observer, rider ordering, AI enumeration
// ---------------------------------------------------------------------------

/// A2.8 — CR 701.71a + CR 704.5i: N = 0 still creates the token, which state-
/// based actions then put into its owner's graveyard.
#[test]
fn a2_8_zero_n_creates_a_token_that_dies_to_state_based_actions() {
    let mut scenario = main_phase_scenario();
    let rs = scenario
        .add_planeswalker_from_oracle(
            P0,
            "Jace, Reality Sculptor",
            "Jace",
            5,
            JACE_REALITY_SCULPTOR,
        )
        .id();
    let mut runner = scenario.build();

    let outcome = runner.activate(rs, 0).resolve();
    let events = outcome.events();
    let created = created_token_ids(events);
    assert_eq!(
        created.len(),
        1,
        "TokenCreated fired for the 0-loyalty token"
    );
    let token = created[0];
    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), to: Zone::Graveyard, .. }
                if *object_id == token
        )),
        "CR 704.5i moved the 0-loyalty token to the graveyard"
    );
    assert!(jace_tokens(outcome.state(), P0).is_empty());
}

/// A2.8 control — CR 614.1c + CR 614.12: Oath of Gideon's enters-with
/// replacement gives the N = 0 token one loyalty counter, so it survives.
#[test]
fn a2_8_control_oath_of_gideon_keeps_the_zero_n_token_alive() {
    let mut scenario = main_phase_scenario();
    let rs = scenario
        .add_planeswalker_from_oracle(
            P0,
            "Jace, Reality Sculptor",
            "Jace",
            5,
            JACE_REALITY_SCULPTOR,
        )
        .id();
    scenario.add_enchantment_from_oracle(P0, "Oath of Gideon", OATH_OF_GIDEON);
    let mut runner = scenario.build();

    let outcome = runner.activate(rs, 0).resolve();
    let events = outcome.events();
    let created = created_token_ids(events);
    assert_eq!(created.len(), 1, "TokenCreated fired");
    let token = created[0];
    // Reach guard (M9): Oath applied to the token entry.
    assert_eq!(loyalty(outcome.state(), token), Some(1));
    assert_eq!(
        outcome.zone_of(token),
        Zone::Battlefield,
        "survives the SBA pass"
    );
    assert!(!events.iter().any(|e| matches!(
        e,
        GameEvent::ZoneChanged { object_id, to: Zone::Graveyard, .. } if *object_id == token
    )));
}

/// A2.9 — CR 122.1 + CR 603.2c: the counters are one placement event of N
/// counters, so a "whenever you put one or more loyalty counters on a
/// planeswalker" observer triggers once, not N times.
#[test]
fn a2_9_loyalty_observer_triggers_once_per_empower() {
    let mut scenario = main_phase_scenario();
    let tethermage = scenario
        .add_creature_from_oracle(P0, "Inspired Tethermage", 3, 3, INSPIRED_TETHERMAGE)
        .id();
    scenario.with_mana_pool(P0, colorless_mana(6));
    let mut runner = scenario.build();
    let index = runner.state().objects[&tethermage]
        .abilities
        .iter()
        .position(|a| a.kind == AbilityKind::Activated)
        .expect("Tethermage's {6} ability");

    let outcome = runner.activate(tethermage, index).resolve();
    let state = outcome.state();
    let tokens = jace_tokens(state, P0);
    assert_eq!(tokens.len(), 1);
    assert_eq!(
        loyalty(state, tokens[0]),
        Some(2),
        "reach guard: two loyalty counters placed"
    );
    assert_eq!(
        state.objects[&tethermage]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied(),
        Some(1),
        "the observer triggered exactly once"
    );
}

/// A2.10 — CR 608.2c: the "Draw a card." rider runs after the empower, across
/// the choice pause: nothing of it is observable at the pause.
#[test]
fn a2_10_rider_runs_after_the_choice() {
    // AST reach guard (M10): the rider follows the empower in one chain.
    let p = parse(
        PROTEGES_AWAKENING,
        "Protege's Awakening",
        &["Surveil"],
        &["Sorcery"],
        &[],
    );
    let chain = sole_empower_chain(&p, "Protege");
    assert_root_empower(chain, "Protege");
    assert!(
        matches!(
            chain.sub_ability.as_deref().map(|s| s.effect.as_ref()),
            Some(Effect::Draw { .. })
        ),
        "M10: Draw is the EmpowerJace node's sub_ability: {chain:#?}"
    );

    let mut scenario = main_phase_scenario();
    let spell = add_sorcery(&mut scenario, "Protege's Awakening", PROTEGES_AWAKENING);
    let mut runner = scenario.build();
    let a = seed_jace_token(&mut runner, P0, 1);
    let b = seed_jace_token(&mut runner, P0, 1);
    let outcome = runner.cast(spell).resolve();
    assert_eq!(
        empower_choices(outcome.final_waiting_for()),
        BTreeSet::from([a, b])
    );
    // At the pause: hand is the pre-cast hand minus the cast card, no draw.
    assert_eq!(outcome.hand_drawn(P0), 0);
    assert_eq!(card_drawn_count(outcome.events()), 0);
    let hand_at_pause = runner.state().players[0].hand.len();

    let events = select(&mut runner, b);
    assert_eq!(card_drawn_count(&events), 1);
    assert_eq!(runner.state().players[0].hand.len(), hand_at_pause + 1);
    assert_eq!(loyalty(runner.state(), b), Some(7));
    assert!(
        loyalty_added_pos(&events, b, 6).unwrap() < card_drawn_pos(&events).unwrap(),
        "the draw follows the counters"
    );
}

/// A2.11 — CR 608.2d: with K = 2 candidates, AI enumeration offers exactly the
/// two single-object submissions.
#[test]
fn a2_11_ai_enumerates_each_candidate() {
    let mut scenario = main_phase_scenario();
    let spell = add_sorcery(&mut scenario, "Protege's Awakening", PROTEGES_AWAKENING);
    let mut runner = scenario.build();
    let (a, b, _) = two_tokens_at_pause(&mut runner, spell);

    let actions: Vec<GameAction> = candidate_actions_broad(runner.state())
        .into_iter()
        .map(|c| c.action)
        .collect();
    assert_eq!(actions.len(), 2, "K = 2 distinct submissions: {actions:?}");
    let picked: BTreeSet<ObjectId> = actions
        .iter()
        .map(|action| match action {
            GameAction::SelectCards { cards } if cards.len() == 1 => cards[0],
            other => panic!("unexpected candidate {other:?}"),
        })
        .collect();
    assert_eq!(picked, BTreeSet::from([a, b]));
}

// ---------------------------------------------------------------------------
// C2.8: state-based actions during the pause
// ---------------------------------------------------------------------------

/// C2.8 — CR 704.4: state-based actions pay no attention to what happens
/// during resolution, so two 0-loyalty tokens created mid-resolution (Parallel
/// Lives doubles the creation) are both still present at the choice pause.
/// After the choice the unchosen one dies (CR 704.5i).
#[test]
fn c2_8_zero_loyalty_tokens_survive_the_pause() {
    let mut scenario = main_phase_scenario();
    scenario.add_enchantment_from_oracle(P0, "Parallel Lives", PARALLEL_LIVES);
    let way = add_enchantment_to_hand(
        &mut scenario,
        "Way of the Necromancer",
        WAY_OF_THE_NECROMANCER,
    );
    let mut runner = scenario.build();

    let outcome = runner.cast(way).resolve();
    let tokens = jace_tokens(outcome.state(), P0);
    assert_eq!(tokens.len(), 2, "Parallel Lives doubled the creation");
    // Reach guard: the pause.
    assert_eq!(
        empower_choices(outcome.final_waiting_for()),
        tokens.iter().copied().collect::<BTreeSet<_>>()
    );
    for &t in &tokens {
        assert_eq!(
            outcome.zone_of(t),
            Zone::Battlefield,
            "present at the pause"
        );
        assert_eq!(loyalty(outcome.state(), t), Some(0));
    }

    let (chosen, other) = (tokens[0], tokens[1]);
    select(&mut runner, chosen);
    runner.advance_until_stack_empty();
    assert_eq!(loyalty(runner.state(), chosen), Some(2));
    assert!(
        !runner.state().battlefield.contains(&other),
        "the unchosen 0-loyalty token dies once state-based actions are checked"
    );
}

// ---------------------------------------------------------------------------
// R-NC, P-a – P-d: replacement pauses
// ---------------------------------------------------------------------------

/// R-NC — CR 614.1 + CR 616.1: a token-creation replacement choice pauses the
/// creation; the post-action resumes it, and the token gets N counters.
#[test]
fn r_nc_token_creation_choice_resumes_into_the_counters() {
    let mut scenario = main_phase_scenario();
    scenario
        .add_creature(P0, "Replacement Host", 1, 1)
        .with_replacement_definition(optional_create_token_replacement());
    let spell = add_sorcery(&mut scenario, "Protege's Awakening", PROTEGES_AWAKENING);
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).resolve();
    // Reach guard.
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::ReplacementChoice { .. }
        ),
        "the token creation paused on a replacement choice, got {:?}",
        outcome.final_waiting_for()
    );
    assert!(jace_tokens(outcome.state(), P0).is_empty());

    let events = choose_replacement(&mut runner, 1);
    let tokens = jace_tokens(runner.state(), P0);
    assert_eq!(tokens.len(), 1, "declined: the token is created");
    assert!(loyalty_added(&events, tokens[0], 6));
    assert_eq!(loyalty(runner.state(), tokens[0]), Some(6));
    assert_eq!(empower_resolved_count(&events), 1);
    assert_eq!(card_drawn_count(&events), 1);
}

/// P-a — CR 614.1 + CR 616.1 + CR 608.2c: a counter-replacement choice inside
/// the EmpowerJaceChoice handler leaves resolution paused — the rider does not
/// run and EmpowerJace does not resolve until the replacement settles.
#[test]
fn p_a_counter_replacement_pause_inside_the_choice_handler() {
    let mut scenario = main_phase_scenario();
    let host = scenario.add_creature(P0, "Replacement Host", 1, 1).id();
    let spell = add_sorcery(&mut scenario, "Protege's Awakening", PROTEGES_AWAKENING);
    let mut runner = scenario.build();
    let a = seed_jace_token(&mut runner, P0, 1);
    let b = seed_jace_token(&mut runner, P0, 1);
    install_replacement(&mut runner, host, optional_add_counter_doubler());

    let outcome = runner.cast(spell).resolve();
    assert_eq!(
        empower_choices(outcome.final_waiting_for()),
        BTreeSet::from([a, b])
    );
    let mut all: Vec<GameEvent> = outcome.events().to_vec();
    let hand_before = runner.state().players[0].hand.len();

    let submit = select(&mut runner, b);
    all.extend(submit.iter().cloned());
    // Reach guard: the counter placement paused on the replacement choice.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "placement paused on ReplacementChoice, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(card_drawn_count(&all), 0, "no draw at the pause");
    assert_eq!(runner.state().players[0].hand.len(), hand_before);
    assert_eq!(loyalty(runner.state(), b), Some(1), "no counters yet");
    assert_eq!(
        empower_resolved_count(&all),
        0,
        "EmpowerJace not resolved at the pause"
    );

    let accept = choose_replacement(&mut runner, 0);
    all.extend(accept.iter().cloned());
    assert_eq!(
        loyalty(runner.state(), b),
        Some(13),
        "accepted doubling: 1 + 12"
    );
    assert_eq!(loyalty(runner.state(), a), Some(1));
    assert_eq!(card_drawn_count(&all), 1);
    assert_eq!(empower_resolved_count(&all), 1);
    let placed = loyalty_added_pos(&all, b, 12).expect("CounterAdded { B, Loyalty, 12 }");
    let resolved = empower_resolved_pos(&all).unwrap();
    let drawn = card_drawn_pos(&all).unwrap();
    assert!(placed < resolved, "EffectResolved follows the counters");
    assert!(resolved < drawn, "the draw follows EffectResolved");
}

/// Answer token-creation replacement prompts so the optional R-NC replacement
/// is declined and Parallel Lives applies. Returns the index sequence.
fn decline_optional_apply_parallel_lives(
    runner: &mut GameRunner,
    all: &mut Vec<GameEvent>,
) -> Vec<usize> {
    let mut sequence = Vec::new();
    while let WaitingFor::ReplacementChoice {
        kind, candidates, ..
    } = runner.state().waiting_for.clone()
    {
        let index = match kind {
            ReplacementChoiceKind::OptionalBranch => 1,
            ReplacementChoiceKind::Order => candidates
                .iter()
                .position(|c| c.source_name == "Parallel Lives")
                .expect("Parallel Lives is an ordering candidate"),
            ReplacementChoiceKind::SearchFoundDestination => {
                panic!("a token creation offers no found-card destination")
            }
        };
        sequence.push(index);
        all.extend(choose_replacement(runner, index));
        assert!(sequence.len() < 8, "replacement prompts must terminate");
    }
    sequence
}

/// P-b — CR 701.71a + CR 608.2d: after a paused token creation, the post-action
/// opens EmpowerJaceChoice (Parallel Lives made two tokens); the rider waits
/// for the choice.
#[test]
fn p_b_post_action_opens_the_choice() {
    let mut scenario = main_phase_scenario();
    scenario.add_enchantment_from_oracle(P0, "Parallel Lives", PARALLEL_LIVES);
    scenario
        .add_creature(P0, "Replacement Host", 1, 1)
        .with_replacement_definition(optional_create_token_replacement());
    let spell = add_sorcery(&mut scenario, "Protege's Awakening", PROTEGES_AWAKENING);
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).resolve();
    let mut all: Vec<GameEvent> = outcome.events().to_vec();
    // Reach guard 1.
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::ReplacementChoice { .. }
        ),
        "creation paused on ReplacementChoice, got {:?}",
        outcome.final_waiting_for()
    );
    let sequence = decline_optional_apply_parallel_lives(&mut runner, &mut all);
    println!("P-b replacement index sequence: {sequence:?}");

    // Reach guard 2.
    let tokens = jace_tokens(runner.state(), P0);
    assert_eq!(
        tokens.len(),
        2,
        "R-NC declined, Parallel Lives applied (sequence {sequence:?})"
    );
    assert_eq!(
        empower_choices(&runner.state().waiting_for),
        tokens.iter().copied().collect::<BTreeSet<_>>()
    );
    assert_eq!(card_drawn_count(&all), 0);
    assert_eq!(empower_resolved_count(&all), 0);

    all.extend(select(&mut runner, tokens[1]));
    assert_eq!(loyalty(runner.state(), tokens[1]), Some(6));
    assert_eq!(card_drawn_count(&all), 1);
    assert_eq!(empower_resolved_count(&all), 1);
    assert!(loyalty_added_pos(&all, tokens[1], 6).unwrap() < card_drawn_pos(&all).unwrap());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// P-c — CR 614.1c + CR 122.6: the token's own entry pauses (Oath of Gideon's
/// entry counter meets an optional counter replacement); the choose-and-place
/// tail runs only after the entry settles.
#[test]
fn p_c_token_entry_pause_defers_the_placement() {
    let mut scenario = main_phase_scenario();
    scenario.add_enchantment_from_oracle(P0, "Oath of Gideon", OATH_OF_GIDEON);
    scenario
        .add_creature(P0, "Replacement Host", 1, 1)
        .with_replacement_definition(optional_add_counter_doubler());
    let spell = add_sorcery(&mut scenario, "Protege's Awakening", PROTEGES_AWAKENING);
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).resolve();
    let mut all: Vec<GameEvent> = outcome.events().to_vec();
    // Reach guard: the entry paused before any Empower placement.
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::ReplacementChoice { .. }
        ),
        "the token entry paused on ReplacementChoice, got {:?}",
        outcome.final_waiting_for()
    );
    assert!(!all.iter().any(|e| matches!(
        e,
        GameEvent::CounterAdded {
            counter_type: CounterType::Loyalty,
            count: 6 | 12,
            ..
        }
    )));

    // Accept every offered doubling (index 0). The accept branch is the
    // unambiguous CR 614.1 outcome: the entry counter becomes 2 and the Empower
    // placement 12. (The decline branch of an optional non-damage
    // `quantity_modification` still applies the modification in the shared
    // replacement pipeline, so it cannot witness "unchanged" counts here.)
    let mut accepts = 0;
    while matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ) {
        all.extend(choose_replacement(&mut runner, 0));
        accepts += 1;
        assert!(accepts < 8, "replacement prompts must terminate");
    }
    assert_eq!(
        accepts, 2,
        "one prompt for the entry, one for the placement"
    );

    let tokens = jace_tokens(runner.state(), P0);
    assert_eq!(tokens.len(), 1);
    let token = tokens[0];
    assert_eq!(
        loyalty(runner.state(), token),
        Some(14),
        "Oath's 1 doubled + Empower's 6 doubled"
    );
    assert_eq!(empower_resolved_count(&all), 1);
    let created = position_of(&all, |e| {
        matches!(
            e,
            GameEvent::TokenCreated { object_id, .. } if *object_id == token
        )
    })
    .expect("TokenCreated for the token");
    let entry = loyalty_added_pos(&all, token, 2).expect("the entry CounterAdded { Loyalty, 2 }");
    let placed =
        loyalty_added_pos(&all, token, 12).expect("the Empower CounterAdded { Loyalty, 12 }");
    let resolved = empower_resolved_pos(&all).unwrap();
    let drawn = card_drawn_pos(&all).expect("CardDrawn");
    assert!(
        created < placed && entry < placed,
        "the entry settles before the placement"
    );
    assert!(placed < resolved && resolved < drawn);
}

/// P-d — CR 614.1 + CR 616.1 + CR 608.2c: after a paused token creation, the
/// post-action's single-candidate placement itself pauses on a counter
/// replacement; EmpowerJace resolves, and the rider runs, only after it
/// settles.
#[test]
fn p_d_counter_replacement_pause_inside_the_post_action() {
    let mut scenario = main_phase_scenario();
    scenario
        .add_creature(P0, "Token Replacement Host", 1, 1)
        .with_replacement_definition(optional_create_token_replacement());
    scenario
        .add_creature(P0, "Counter Replacement Host", 1, 1)
        .with_replacement_definition(optional_add_counter_doubler());
    let spell = add_sorcery(&mut scenario, "Protege's Awakening", PROTEGES_AWAKENING);
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).resolve();
    let mut all: Vec<GameEvent> = outcome.events().to_vec();
    // Reach guard 1: the token-creation choice, before any Jace token exists.
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::ReplacementChoice { .. }
        ),
        "creation paused on ReplacementChoice, got {:?}",
        outcome.final_waiting_for()
    );
    assert!(jace_tokens(outcome.state(), P0).is_empty());

    let decline = choose_replacement(&mut runner, 1);
    all.extend(decline.iter().cloned());
    // Reach guard 2: one token, 0 loyalty, and a second (AddCounter) choice.
    let tokens = jace_tokens(runner.state(), P0);
    assert_eq!(tokens.len(), 1);
    let token = tokens[0];
    assert_eq!(created_token_ids(&decline), vec![token]);
    assert_eq!(loyalty(runner.state(), token), Some(0));
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the Empower placement paused on ReplacementChoice, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        empower_resolved_count(&all),
        0,
        "EmpowerJace not resolved at the pause"
    );
    assert_eq!(card_drawn_count(&all), 0, "no draw at the pause");

    all.extend(choose_replacement(&mut runner, 0));
    assert_eq!(loyalty(runner.state(), token), Some(12), "N = 6 doubled");
    assert_eq!(
        all.iter()
            .filter(|e| matches!(
                e,
                GameEvent::CounterAdded { object_id, counter_type: CounterType::Loyalty, count: 12, .. }
                    if *object_id == token
            ))
            .count(),
        1
    );
    assert_eq!(empower_resolved_count(&all), 1);
    assert_eq!(card_drawn_count(&all), 1);
    let placed = loyalty_added_pos(&all, token, 12).unwrap();
    let resolved = empower_resolved_pos(&all).unwrap();
    let drawn = card_drawn_pos(&all).unwrap();
    assert!(placed < resolved, "EffectResolved follows the counters");
    assert!(resolved < drawn, "the draw follows EffectResolved");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}
