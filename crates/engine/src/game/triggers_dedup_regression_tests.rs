use super::*;
use crate::game::zones::create_object;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Effect, QuantityExpr, ResolvedAbility,
    TargetFilter, TargetRef, TriggerDefinition, TypedFilter,
};
use crate::types::actions::GameAction;
use crate::types::card_type::{CoreType, Supertype};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    AutoMayChoice, GameState, MayTriggerAutoChoiceKey, MayTriggerOrigin, WaitingFor,
    ZoneChangeRecord,
};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

fn setup() -> GameState {
    GameState::new_two_player(42)
}

fn live_trigger_origin(
    state: &GameState,
    source_id: ObjectId,
    live_index: usize,
) -> MayTriggerOrigin {
    let source = state.objects.get(&source_id).unwrap();
    let entry = source.trigger_definitions.get(live_index).unwrap();
    MayTriggerOrigin::Definition {
        definition_ref: source.trigger_definition_ref(entry),
    }
}

fn make_creature(
    state: &mut GameState,
    player: PlayerId,
    name: &str,
    power: i32,
    toughness: i32,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(state.next_object_id),
        player,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    obj.base_power = Some(power);
    obj.base_toughness = Some(toughness);
    obj.power = Some(power);
    obj.toughness = Some(toughness);
    id
}

/// Build a minimal `Draw 1` triggered ability that matches a given mode.
fn draw_one_trigger(mode: TriggerMode) -> TriggerDefinition {
    TriggerDefinition::new(mode)
        .valid_card(TargetFilter::SelfRef)
        .execute(AbilityDefinition::new(
            AbilityKind::Database,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        ))
}

fn setup_with_observer(mode: TriggerMode) -> (GameState, ObjectId) {
    let mut state = GameState::new_two_player(42);
    let observer = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Observer".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&observer).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.entered_battlefield_turn = Some(1);
        // Self-ref-only valid_card would restrict to ETB of self; for observer
        // triggers we want to match any qualifying event. Swap to TargetFilter::Any.
        let mut trigger = draw_one_trigger(mode);
        trigger.valid_card = Some(TargetFilter::Any);
        obj.trigger_definitions.push(trigger);
    }
    (state, observer)
}

/// ETB observer trigger: one creature entering produces exactly one trigger.
/// Regression: Mischievous Mystic's ETB trigger used to double-register when
/// synthesis ran twice, producing two tokens from one ETB.
#[test]
fn etb_observer_fires_once_per_event() {
    let (mut state, observer) = setup_with_observer(TriggerMode::ChangesZone);
    state
        .objects
        .get_mut(&observer)
        .unwrap()
        .trigger_definitions[0]
        .definition
        .destination = Some(Zone::Battlefield);

    let new_etb = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Newcomer".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&new_etb)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);

    let event = GameEvent::ZoneChanged {
        object_id: new_etb,
        from: Some(Zone::Hand),
        to: Zone::Battlefield,
        record: Box::new(ZoneChangeRecord::test_minimal(
            new_etb,
            Some(Zone::Hand),
            Zone::Battlefield,
        )),
    };

    process_triggers(&mut state, &[event]);
    assert_eq!(
        state.stack.len(),
        1,
        "ETB observer should register exactly one trigger per ETB event"
    );
}

/// Attacks observer: a non-batched "whenever a creature attacks" trigger
/// registers once per AttackersDeclared event. Regression: Najeela-style
/// triggers registered multiply when zone scanners double-visited.
#[test]
fn attacks_observer_fires_once_per_event() {
    let (mut state, observer) = setup_with_observer(TriggerMode::Attacks);
    let attacker = create_object(
        &mut state,
        CardId(3),
        PlayerId(0),
        "Attacker".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&attacker)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);

    let event = GameEvent::AttackersDeclared {
        attacker_ids: vec![attacker],
        defending_player: PlayerId(1),
        attacks: vec![(
            attacker,
            crate::game::combat::AttackTarget::Player(PlayerId(1)),
        )],
    };

    process_triggers(&mut state, &[event]);
    // CR 603.3b (#531): drain the per-controller ordering prompt.
    super::drain_order_triggers_with_identity(&mut state);
    let observer_triggers = state
        .stack
        .iter()
        .filter(|e| e.source_id == observer)
        .count();
    assert_eq!(
        observer_triggers, 1,
        "Attack observer should register exactly one trigger per AttackersDeclared"
    );
}

/// SpellCast observer: spell-cast triggers register once per SpellCast event.
#[test]
fn spell_cast_observer_fires_once_per_event() {
    let (mut state, observer) = setup_with_observer(TriggerMode::SpellCast);
    let spell = create_object(
        &mut state,
        CardId(4),
        PlayerId(0),
        "Spell".to_string(),
        Zone::Stack,
    );
    state
        .objects
        .get_mut(&spell)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Instant);

    let event = GameEvent::SpellCast {
        card_id: CardId(4),
        controller: PlayerId(0),
        object_id: spell,
    };

    process_triggers(&mut state, &[event]);
    // CR 603.3b (#531): drain the per-controller ordering prompt.
    super::drain_order_triggers_with_identity(&mut state);
    let observer_triggers = state
        .stack
        .iter()
        .filter(|e| e.source_id == observer)
        .count();
    assert_eq!(
        observer_triggers, 1,
        "SpellCast observer should register exactly one trigger per SpellCast event"
    );
}

/// CR 104.3e + CR 119 + CR 603.4 + CR 603.7c: Ezio Auditore da Firenze —
/// "Whenever ~ deals combat damage to a player, if that player has 10 or
/// less life, you may pay {W}{U}{B}{R}{G}. When you do, that player loses
/// the game."
///
/// Issue #1962 regression for the game-state-corruption half:
/// the parser must lift "if that player has 10 or less life" to a
/// `TriggerCondition` (CR 603.4), and `Effect::LoseTheGame` must carry
/// `TargetFilter::TriggeringPlayer` so the resolver eliminates the
/// **damaged** player (CR 603.7c), not Ezio's controller.
///
/// This integration test stops short of driving combat damage through
/// the full combat runner; it parses Ezio's trigger, fires the
/// observer via `process_triggers` with a synthetic combat-damage
/// event, and asserts the trigger gate honors the life predicate.
#[test]
fn ezio_combat_damage_trigger_does_not_fire_when_damaged_player_above_10() {
    use crate::parser::oracle_trigger::parse_trigger_line;

    const EZIO_ORACLE: &str = "Whenever ~ deals combat damage to a player, if that player has 10 or less life, you may pay {W}{U}{B}{R}{G}. When you do, that player loses the game.";

    let mut state = setup();
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    // P1 has 15 life — above the 10-or-less gate, so the trigger must
    // NOT fire (CR 603.4 — intervening-if checks at fire time).
    state.players[1].life = 15;

    let ezio = make_creature(&mut state, PlayerId(0), "Ezio Auditore da Firenze", 4, 4);
    let trigger = parse_trigger_line(EZIO_ORACLE, "Ezio Auditore da Firenze");
    {
        let obj = state.objects.get_mut(&ezio).unwrap();
        obj.trigger_definitions.push(trigger.clone());
        obj.base_trigger_definitions = std::sync::Arc::new(vec![trigger.clone()]);
    }
    // CR 603.6a: re-register the trigger index for the new trigger.
    state.trigger_index.remove(ezio);
    let defs: smallvec::SmallVec<[TriggerDefinition; 4]> = smallvec::smallvec![trigger];
    state.trigger_index.add(ezio, &defs, false);

    // Synthesize the combat damage event Ezio observes.
    let event = GameEvent::DamageDealt {
        source_id: ezio,
        target: TargetRef::Player(PlayerId(1)),
        amount: 4,
        is_combat: true,
        excess: 0,
    };
    process_triggers(&mut state, &[event]);

    // CR 603.4: intervening-if false → no ability lands on the stack at
    // all, and no optional prompt is queued.
    assert!(
        state.stack.is_empty(),
        "trigger must not fire when damaged player has > 10 life; stack was {:?}",
        state.stack,
    );
    assert!(
        !matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
        "no optional choice should be pending; waiting_for was {:?}",
        state.waiting_for,
    );
    assert!(
        !state.players[1].is_eliminated,
        "P1 must not be eliminated when the intervening-if blocks the trigger",
    );
    assert!(
        !state.players[0].is_eliminated,
        "P0 (Ezio's controller) must not be eliminated either",
    );
}

/// CR 104.3e + CR 603.7c: companion case to the gate test above.
/// When the damaged player's life ≤ 10, the trigger fires and an
/// optional `you may pay {WUBRG}` lands on the stack. Specifically
/// validates that `Effect::LoseTheGame.target` is wired through the
/// trigger machinery so the damaged player (`TriggeringPlayer`), not
/// Ezio's controller, is bound for the eventual elimination.
#[test]
fn ezio_combat_damage_trigger_fires_when_damaged_player_at_or_below_10() {
    use crate::parser::oracle_trigger::parse_trigger_line;

    const EZIO_ORACLE: &str = "Whenever ~ deals combat damage to a player, if that player has 10 or less life, you may pay {W}{U}{B}{R}{G}. When you do, that player loses the game.";

    let mut state = setup();
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    // P1 at 5 life — the intervening-if (LE 10) is satisfied.
    state.players[1].life = 5;

    let ezio = make_creature(&mut state, PlayerId(0), "Ezio Auditore da Firenze", 4, 4);
    let trigger = parse_trigger_line(EZIO_ORACLE, "Ezio Auditore da Firenze");

    // Sanity: the parsed trigger must carry the directed-loss target on
    // its reflexive sub-ability. The full structural assertion lives in
    // `parse_ezio_damage_trigger_full_structure`; here we re-check the
    // single load-bearing invariant for this integration path.
    let execute = trigger
        .execute
        .as_ref()
        .expect("Ezio trigger must have an execute body");
    let sub = execute
        .sub_ability
        .as_deref()
        .expect("Ezio trigger must have a reflexive 'When you do' sub-ability");
    assert!(
            matches!(
                &*sub.effect,
                Effect::LoseTheGame { target: Some(f) } if *f == TargetFilter::TriggeringPlayer
            ),
            "Ezio's reflexive sub-ability must lower to LoseTheGame {{ target: Some(TriggeringPlayer) }}; \
             got {:?} — without this binding, win_lose::resolve_lose routes elimination to ability.controller",
            sub.effect,
        );

    {
        let obj = state.objects.get_mut(&ezio).unwrap();
        obj.trigger_definitions.push(trigger.clone());
        obj.base_trigger_definitions = std::sync::Arc::new(vec![trigger.clone()]);
    }
    state.trigger_index.remove(ezio);
    let defs: smallvec::SmallVec<[TriggerDefinition; 4]> = smallvec::smallvec![trigger];
    state.trigger_index.add(ezio, &defs, false);

    let event = GameEvent::DamageDealt {
        source_id: ezio,
        target: TargetRef::Player(PlayerId(1)),
        amount: 4,
        is_combat: true,
        excess: 0,
    };
    process_triggers(&mut state, &[event]);
    super::drain_order_triggers_with_identity(&mut state);

    // CR 603.4 + CR 603.3: the intervening-if is satisfied, so the
    // ability lands on the stack. At minimum exactly one trigger from
    // Ezio must be pending (it may either sit at priority or be on the
    // way to an OptionalEffectChoice, depending on the dispatch state).
    assert!(
        !state.stack.is_empty()
            || matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
        "trigger must fire when damaged player has ≤ 10 life; stack/waiting_for: {:?} / {:?}",
        state.stack,
        state.waiting_for,
    );
}

/// CR 104.3e + CR 608.2c + CR 603.7c + CR 603.12: Ezio Auditore da
/// Firenze — VERBATIM printed Oracle text (post-effect `if` form):
/// "Whenever ~ deals combat damage to a player, you may pay
/// {W}{U}{B}{R}{G} if that player has 10 or less life. When you do,
/// that player loses the game."
///
/// Issue #1962 hardening (TEST-ONLY): the companion integration tests
/// `ezio_combat_damage_trigger_does_not_fire_when_damaged_player_above_10`
/// and `ezio_combat_damage_trigger_fires_when_damaged_player_at_or_below_10`
/// exercise the *normalized* leading-`if` form (CR 603.4
/// intervening-if, detection-time gate). This test locks the
/// *verbatim* printed Oracle text, which uses the post-effect `if`
/// form and is re-homed onto `execute.condition` (CR 608.2c,
/// resolution-time gate) by `strip_suffix_conditional`. Without this
/// test, a regression in the post-effect re-homer would silently
/// strand the life predicate (allowing the loss at any life total)
/// while the leading-`if` regression tests above continue to pass.
///
/// The key invariant is the **elimination outcome**: at any life
/// total above 10, P1 must not be eliminated regardless of which
/// re-home path the parser uses (def.condition vs execute.condition).
#[test]
fn ezio_verbatim_oracle_text_does_not_eliminate_damaged_player_above_10_life() {
    use crate::parser::oracle_trigger::parse_trigger_line;

    const EZIO_VERBATIM: &str = "Whenever ~ deals combat damage to a player, you may pay {W}{U}{B}{R}{G} if that player has 10 or less life. When you do, that player loses the game.";

    let mut state = setup();
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    // P1 has 15 life — above the 10-or-less gate, so the
    // resolution-time `execute.condition` must block the cost and the
    // reflexive sub-ability. P1 must NOT be eliminated.
    state.players[1].life = 15;

    let ezio = make_creature(&mut state, PlayerId(0), "Ezio Auditore da Firenze", 4, 4);
    let trigger = parse_trigger_line(EZIO_VERBATIM, "Ezio Auditore da Firenze");
    {
        let obj = state.objects.get_mut(&ezio).unwrap();
        obj.trigger_definitions.push(trigger.clone());
        obj.base_trigger_definitions = std::sync::Arc::new(vec![trigger.clone()]);
    }
    state.trigger_index.remove(ezio);
    let defs: smallvec::SmallVec<[TriggerDefinition; 4]> = smallvec::smallvec![trigger];
    state.trigger_index.add(ezio, &defs, false);

    let event = GameEvent::DamageDealt {
        source_id: ezio,
        target: TargetRef::Player(PlayerId(1)),
        amount: 4,
        is_combat: true,
        excess: 0,
    };
    process_triggers(&mut state, &[event]);
    super::drain_order_triggers_with_identity(&mut state);

    // Drive the stack to completion, declining any optional prompts
    // (controller would never voluntarily pay an unpayable cost
    // anyway — P0 has zero mana). The exact drain path doesn't
    // matter — the load-bearing invariant is that *no path*
    // through resolution ends with P1 eliminated when life > 10.
    for _ in 0..40 {
        match state.waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                if crate::game::engine::apply_as_current(
                    &mut state,
                    GameAction::DecideOptionalEffect { accept: false },
                )
                .is_err()
                {
                    break;
                }
            }
            WaitingFor::GameOver { .. } => break,
            _ => {
                if state.stack.is_empty()
                    && matches!(state.waiting_for, WaitingFor::Priority { .. })
                {
                    break;
                }
                if crate::game::engine::apply_as_current(&mut state, GameAction::PassPriority)
                    .is_err()
                {
                    break;
                }
            }
        }
    }

    // The load-bearing invariant: regardless of whether the parser
    // hoisted the gate to def.condition (intervening-if, no stack
    // entry) or re-homed it to execute.condition (resolution-time
    // failure), the elimination outcome must be the same — P1 not
    // eliminated.
    assert!(
        !state.players[1].is_eliminated,
        "P1 (15 life) must NOT be eliminated — the life-total gate \
             must block the loss whether evaluated at detection (CR 603.4) \
             or at resolution (CR 608.2c); waiting_for = {:?}",
        state.waiting_for,
    );
    assert!(
        !state.players[0].is_eliminated,
        "P0 (Ezio's controller) must NOT be eliminated either — \
             the directed-loss target (TriggeringPlayer) must never fall \
             through to the ability controller (issue #1962 root cause)",
    );
}

/// CR 104.3e + CR 608.2c + CR 603.7c + CR 603.12: Ezio Auditore da
/// Firenze — VERBATIM printed Oracle text, low-life path. Same setup
/// as the above test but P1 starts at 5 life and P0 holds {WUBRG}.
/// After accepting the optional and paying the cost, the reflexive
/// "When you do, that player loses the game" sub-ability must fire
/// and eliminate P1 (the damaged player — `TriggeringPlayer`), not
/// P0 (the ability controller).
///
/// Issue #1962 hardening (TEST-ONLY): paired with the above
/// high-life test, this locks both sides of the elimination outcome
/// for the verbatim Oracle text — without it, a regression that
/// dropped the directed-loss target (the original root cause) would
/// silently let the controller eliminate themselves.
#[test]
fn ezio_verbatim_oracle_text_eliminates_damaged_player_when_optional_paid() {
    use crate::game::engine::apply_as_current;
    use crate::parser::oracle_trigger::parse_trigger_line;
    use crate::types::mana::{ManaType, ManaUnit};

    const EZIO_VERBATIM: &str = "Whenever ~ deals combat damage to a player, you may pay {W}{U}{B}{R}{G} if that player has 10 or less life. When you do, that player loses the game.";

    let mut state = setup();
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.players[1].life = 5;
    // Seed P0's mana pool with WUBRG so the optional cost is payable.
    for color in [
        ManaType::White,
        ManaType::Blue,
        ManaType::Black,
        ManaType::Red,
        ManaType::Green,
    ] {
        state.players[0].mana_pool.add(ManaUnit {
            color,
            source_id: ObjectId(0),
            pip_id: crate::types::mana::ManaPipId(0),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: Vec::new(),
            grants: vec![],
            expiry: None,
        });
    }

    let ezio = make_creature(&mut state, PlayerId(0), "Ezio Auditore da Firenze", 4, 4);
    let trigger = parse_trigger_line(EZIO_VERBATIM, "Ezio Auditore da Firenze");
    {
        let obj = state.objects.get_mut(&ezio).unwrap();
        obj.trigger_definitions.push(trigger.clone());
        obj.base_trigger_definitions = std::sync::Arc::new(vec![trigger.clone()]);
    }
    state.trigger_index.remove(ezio);
    let defs: smallvec::SmallVec<[TriggerDefinition; 4]> = smallvec::smallvec![trigger];
    state.trigger_index.add(ezio, &defs, false);

    let event = GameEvent::DamageDealt {
        source_id: ezio,
        target: TargetRef::Player(PlayerId(1)),
        amount: 4,
        is_combat: true,
        excess: 0,
    };
    process_triggers(&mut state, &[event]);
    super::drain_order_triggers_with_identity(&mut state);

    // Drive the resolution: accept every optional prompt (the
    // controller pays the WUBRG cost), and otherwise pass priority
    // until either game-over fires or the stack drains. The drive
    // loop is bounded to prevent runaway state on regression.
    for _ in 0..80 {
        if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            break;
        }
        match state.waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                if apply_as_current(
                    &mut state,
                    GameAction::DecideOptionalEffect { accept: true },
                )
                .is_err()
                {
                    break;
                }
            }
            WaitingFor::Priority { .. } => {
                if state.stack.is_empty() && state.players[1].is_eliminated {
                    break;
                }
                if apply_as_current(&mut state, GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => {
                // Unexpected/unhandled waiting_for — break and assert
                // outcome below. If the engine can't reach P1
                // elimination through this path, the assert will fail
                // and surface the broken state.
                break;
            }
        }
    }

    // The load-bearing invariant for the verbatim-text path: with
    // life ≤ 10 + optional accepted + cost paid, the directed loss
    // (CR 603.7c — `TargetFilter::TriggeringPlayer`) must land on
    // P1 (the damaged player), NOT P0 (the ability controller).
    assert!(
        state.players[1].is_eliminated,
        "P1 (damaged player, 5 life, with WUBRG paid) must be eliminated; \
             waiting_for = {:?}, stack = {:?}",
        state.waiting_for, state.stack,
    );
    assert!(
        !state.players[0].is_eliminated,
        "P0 (Ezio's controller) must NOT be eliminated — issue #1962 root cause \
             was that LoseTheGame fell through to ability.controller when the \
             directed-loss target was dropped; verbatim Oracle text must wire \
             `TargetFilter::TriggeringPlayer` end-to-end",
    );
}

/// CR 702.173a + CR 608.2i: The Freerunning eligibility ledger
/// (`assassin_or_commander_dealt_combat_damage_this_turn`) must
/// observe the **type/role gate** in `collect_pending_triggers` — a
/// generic (non-Assassin, non-commander) creature dealing combat
/// damage to a player must NOT seed the ledger. Otherwise every
/// combat damage event would unlock Freerunning for every spell,
/// silently breaking the keyword's gating semantics.
///
/// Issue #1962 hardening (TEST-ONLY): the type-and-commander gate at
/// triggers.rs:1696-1709 is currently exercised only indirectly
/// (through casting tests that assume the ledger is populated). This
/// test pins down the **negative** branch directly: a vanilla
/// Creature with no Assassin subtype and `is_commander == false`
/// must leave the ledger empty after a combat-damage event.
#[test]
fn vanilla_creature_combat_damage_does_not_seed_freerunning_ledger() {
    let mut state = setup();
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    // A vanilla creature: no Assassin subtype, not the commander.
    // `make_creature` (this mod's helper) builds a bare Creature with
    // no subtypes attached.
    let vanilla = make_creature(&mut state, PlayerId(0), "Grizzly Bears", 2, 2);
    assert!(
        !state.objects[&vanilla].is_commander,
        "test fixture sanity: vanilla creature must not be the commander"
    );
    assert!(
        !state.objects[&vanilla]
            .card_types
            .subtypes
            .iter()
            .any(|s| s == "Assassin"),
        "test fixture sanity: vanilla creature must not be an Assassin"
    );

    // Pre-event sanity: the ledger starts empty.
    assert!(
        state
            .assassin_or_commander_dealt_combat_damage_this_turn
            .is_empty(),
        "ledger must start empty"
    );

    let event = GameEvent::DamageDealt {
        source_id: vanilla,
        target: TargetRef::Player(PlayerId(1)),
        amount: 2,
        is_combat: true,
        excess: 0,
    };
    process_triggers(&mut state, &[event]);

    // The key invariant: the ledger must NOT contain the vanilla
    // creature's controller, because the source is neither an
    // Assassin creature nor a commander.
    assert!(
        !state
            .assassin_or_commander_dealt_combat_damage_this_turn
            .contains(&PlayerId(0)),
        "vanilla (non-Assassin, non-commander) combat damage must NOT seed the \
             Freerunning eligibility ledger; ledger = {:?}",
        state.assassin_or_commander_dealt_combat_damage_this_turn,
    );
}

/// CR 702.173a + CR 608.2i: Companion to the vanilla-creature ledger
/// test above — locks the **affirmative** branch of the type gate.
/// An Assassin creature dealing combat damage to a player MUST seed
/// the ledger with its controller, enabling Freerunning casts that
/// turn.
///
/// Issue #1962 hardening (TEST-ONLY): paired with the vanilla test,
/// this fences in the full Assassin gate — a regression that flipped
/// the polarity of the type check (or accidentally widened it to
/// all creatures) would be caught by exactly one of the two tests.
#[test]
fn assassin_creature_combat_damage_seeds_freerunning_ledger() {
    let mut state = setup();
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    let assassin = make_creature(&mut state, PlayerId(0), "Royal Assassin", 1, 1);
    {
        // `make_creature` (this mod's helper) stamps `base_card_types`
        // before subtypes are attached. `process_triggers` calls
        // `flush_layers`, which restores `card_types` from
        // `base_card_types` — so the Assassin subtype must live on
        // BOTH the current and base type rows to survive the flush.
        let obj = state.objects.get_mut(&assassin).unwrap();
        obj.card_types.subtypes.push("Assassin".to_string());
        obj.base_card_types = obj.card_types.clone();
    }

    let event = GameEvent::DamageDealt {
        source_id: assassin,
        target: TargetRef::Player(PlayerId(1)),
        amount: 1,
        is_combat: true,
        excess: 0,
    };
    process_triggers(&mut state, &[event]);

    assert!(
        state
            .assassin_or_commander_dealt_combat_damage_this_turn
            .contains(&PlayerId(0)),
        "Assassin combat damage must seed the Freerunning eligibility ledger \
             with the source's controller (P0); ledger = {:?}",
        state.assassin_or_commander_dealt_combat_damage_this_turn,
    );
}

/// CR 702.76a + CR 608.2i: A creature with a creature type dealing combat
/// damage to a player seeds the Prowl creature-type ledger under its
/// controller, snapshot at damage time. (Unlike the Freerunning ledger, this
/// is recorded for any controlled source's types, not gated on Assassin.)
#[test]
fn typed_creature_combat_damage_seeds_prowl_creature_type_ledger() {
    let mut state = setup();
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    let rogue = make_creature(&mut state, PlayerId(0), "Rogue Test", 1, 1);
    {
        // Subtype must live on both rows to survive the layer flush (see the
        // assassin test above).
        let obj = state.objects.get_mut(&rogue).unwrap();
        obj.card_types.subtypes.push("Rogue".to_string());
        obj.base_card_types = obj.card_types.clone();
    }

    let event = GameEvent::DamageDealt {
        source_id: rogue,
        target: TargetRef::Player(PlayerId(1)),
        amount: 1,
        is_combat: true,
        excess: 0,
    };
    process_triggers(&mut state, &[event]);

    assert!(
        state
            .creature_types_dealt_combat_damage_this_turn
            .contains(&(PlayerId(0), "Rogue".to_string())),
        "Rogue combat damage must seed the Prowl creature-type ledger under P0; ledger = {:?}",
        state.creature_types_dealt_combat_damage_this_turn,
    );
}

/// CR 702.76a: Non-combat damage must NOT seed the Prowl ledger — the
/// predicate is "was dealt COMBAT damage this turn".
#[test]
fn noncombat_damage_does_not_seed_prowl_ledger() {
    let mut state = setup();
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    let rogue = make_creature(&mut state, PlayerId(0), "Rogue Test", 1, 1);
    {
        let obj = state.objects.get_mut(&rogue).unwrap();
        obj.card_types.subtypes.push("Rogue".to_string());
        obj.base_card_types = obj.card_types.clone();
    }

    let event = GameEvent::DamageDealt {
        source_id: rogue,
        target: TargetRef::Player(PlayerId(1)),
        amount: 1,
        is_combat: false,
        excess: 0,
    };
    process_triggers(&mut state, &[event]);

    assert!(
        state
            .creature_types_dealt_combat_damage_this_turn
            .is_empty(),
        "non-combat damage must not seed the Prowl ledger; ledger = {:?}",
        state.creature_types_dealt_combat_damage_this_turn,
    );
}

/// DamageDealt observer: damage-event triggers register once per DamageDealt.
/// Regression: Mana Cannons damage fired 4-6× due to multi-path zone scans.
#[test]
fn damage_observer_fires_once_per_event() {
    let (mut state, observer) = setup_with_observer(TriggerMode::DamageDone);
    let source = create_object(
        &mut state,
        CardId(5),
        PlayerId(0),
        "Damage Source".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&source)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);

    let event = GameEvent::DamageDealt {
        source_id: source,
        target: TargetRef::Player(PlayerId(1)),
        amount: 3,
        is_combat: false,
        excess: 0,
    };

    process_triggers(&mut state, &[event]);
    // CR 603.3b (#531): drain the per-controller ordering prompt.
    super::drain_order_triggers_with_identity(&mut state);
    let observer_triggers = state
        .stack
        .iter()
        .filter(|e| e.source_id == observer)
        .count();
    assert_eq!(
        observer_triggers, 1,
        "DamageDone observer should register exactly one trigger per DamageDealt event"
    );
}

/// Sacrifice observer: "whenever a permanent is sacrificed" fires once per
/// PermanentSacrificed event, not once per zone scan.
#[test]
fn sacrifice_observer_fires_once_per_event() {
    let (mut state, observer) = setup_with_observer(TriggerMode::Sacrificed);
    let victim = create_object(
        &mut state,
        CardId(6),
        PlayerId(0),
        "Victim".to_string(),
        Zone::Graveyard,
    );
    state
        .objects
        .get_mut(&victim)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);

    let event = GameEvent::PermanentSacrificed {
        object_id: victim,
        player_id: PlayerId(0),
    };

    process_triggers(&mut state, &[event]);
    // CR 603.3b (#531): drain the per-controller ordering prompt.
    super::drain_order_triggers_with_identity(&mut state);
    let observer_triggers = state
        .stack
        .iter()
        .filter(|e| e.source_id == observer)
        .count();
    assert_eq!(
        observer_triggers, 1,
        "Sacrifice observer should register exactly one trigger per PermanentSacrificed"
    );
}

/// Landfall: "whenever a land enters the battlefield under your control"
/// fires once per land ETB. Regression: Icetill Explorer's landfall fired
/// multiple times when multi-zone scans visited the same trigger_def.
#[test]
fn landfall_fires_once_per_land_etb() {
    let (mut state, observer) = setup_with_observer(TriggerMode::ChangesZone);
    state
        .objects
        .get_mut(&observer)
        .unwrap()
        .trigger_definitions[0]
        .definition
        .destination = Some(Zone::Battlefield);
    // Narrow the valid_card to lands to mimic landfall's filter.
    state
        .objects
        .get_mut(&observer)
        .unwrap()
        .trigger_definitions[0]
        .definition
        .valid_card = Some(TargetFilter::Typed(
        crate::types::ability::TypedFilter::land(),
    ));

    let land = create_object(
        &mut state,
        CardId(7),
        PlayerId(0),
        "Mountain".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&land)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Land);

    let event = GameEvent::ZoneChanged {
        object_id: land,
        from: Some(Zone::Hand),
        to: Zone::Battlefield,
        record: Box::new(ZoneChangeRecord {
            name: "Mountain".to_string(),
            core_types: vec![CoreType::Land],
            subtypes: vec!["Mountain".to_string()],
            ..ZoneChangeRecord::test_minimal(land, Some(Zone::Hand), Zone::Battlefield)
        }),
    };

    process_triggers(&mut state, &[event]);
    assert_eq!(
        state.stack.len(),
        1,
        "Landfall should register exactly one trigger per land ETB"
    );
}

/// Panharmonicon-style trigger doubling must still produce exactly 2 stack
/// instances from 1 matching event — the per-event dedup applies to
/// *registration* of trigger definitions, not to the post-registration
/// `apply_trigger_doubling` cloning pass.
#[test]
fn panharmonicon_still_doubles_after_dedup() {
    use crate::types::ability::ControllerRef;
    use crate::types::statics::{StaticMode, TriggerCause};

    let (mut state, _observer) = setup_with_observer(TriggerMode::ChangesZone);
    // Scope the observer trigger to ETB.
    // Find the first battlefield object (our observer) to seed.
    let observer_id = *state.battlefield.iter().next().unwrap();
    state
        .objects
        .get_mut(&observer_id)
        .unwrap()
        .trigger_definitions[0]
        .definition
        .destination = Some(Zone::Battlefield);

    // Put a Panharmonicon on the battlefield with its static.
    let panh = create_object(
        &mut state,
        CardId(8),
        PlayerId(0),
        "Panharmonicon".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&panh).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.static_definitions.push(
            crate::types::ability::StaticDefinition::new(StaticMode::DoubleTriggers {
                cause: TriggerCause::EntersBattlefield {
                    core_types: vec![CoreType::Artifact, CoreType::Creature],
                },
            })
            .affected(TargetFilter::Typed(
                crate::types::ability::TypedFilter::creature().controller(ControllerRef::You),
            )),
        );
    }

    // A creature enters.
    let new_etb = create_object(
        &mut state,
        CardId(9),
        PlayerId(0),
        "Entering Creature".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&new_etb)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);

    let event = GameEvent::ZoneChanged {
        object_id: new_etb,
        from: Some(Zone::Hand),
        to: Zone::Battlefield,
        record: Box::new(ZoneChangeRecord {
            name: "Entering Creature".to_string(),
            core_types: vec![CoreType::Creature],
            ..ZoneChangeRecord::test_minimal(new_etb, Some(Zone::Hand), Zone::Battlefield)
        }),
    };

    process_triggers(&mut state, &[event]);
    // CR 603.3b (#531): doubled triggers fire as 2 in the same controller's
    // group, prompting OrderTriggers. Drain with identity to recover the
    // pre-#531 deterministic stack-placement that this assertion expects.
    super::drain_order_triggers_with_identity(&mut state);
    let observer_triggers = state
        .stack
        .iter()
        .filter(|e| e.source_id == observer_id)
        .count();
    assert_eq!(
        observer_triggers, 2,
        "Panharmonicon must still double the observer's ETB trigger to 2 instances"
    );
}

/// Helper: install a `DoubleTriggers` static on a new battlefield object
/// with the supplied cause, controlled by PlayerId(0).
fn install_doubler(state: &mut GameState, cause: TriggerCause) -> ObjectId {
    use crate::types::statics::StaticMode;
    let id = create_object(
        state,
        CardId(100),
        PlayerId(0),
        "Doubler".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.static_definitions
        .push(crate::types::ability::StaticDefinition::new(
            StaticMode::DoubleTriggers { cause },
        ));
    id
}

/// CR 603.2d + CR 603.6a + CR 603.6c: Gandalf the White-class doubler.
fn install_gandalf_doubler(state: &mut GameState) -> ObjectId {
    use crate::types::statics::{TriggerCause, ZoneChangeQualifier};
    install_doubler(
        state,
        TriggerCause::BattlefieldTransition {
            enter: true,
            leave: true,
            qualifiers: vec![
                ZoneChangeQualifier::Supertype(Supertype::Legendary),
                ZoneChangeQualifier::CoreType(CoreType::Artifact),
            ],
        },
    )
}

/// CR 603.2d: Gandalf doubles ETB triggers caused by a legendary permanent.
#[test]
fn gandalf_doubles_legendary_etb_triggers() {
    let (mut state, observer) = setup_with_observer(TriggerMode::ChangesZone);
    state
        .objects
        .get_mut(&observer)
        .unwrap()
        .trigger_definitions[0]
        .definition
        .destination = Some(Zone::Battlefield);
    let _gandalf = install_gandalf_doubler(&mut state);

    let norin = create_object(
        &mut state,
        CardId(5332),
        PlayerId(0),
        "Norin the Wary".to_string(),
        Zone::Battlefield,
    );
    state.objects.get_mut(&norin).unwrap().card_types = crate::types::card_type::CardType {
        core_types: vec![CoreType::Creature],
        supertypes: vec![Supertype::Legendary],
        subtypes: vec![],
    };

    let event = GameEvent::ZoneChanged {
        object_id: norin,
        from: Some(Zone::Exile),
        to: Zone::Battlefield,
        record: Box::new(ZoneChangeRecord {
            name: "Norin the Wary".to_string(),
            core_types: vec![CoreType::Creature],
            supertypes: vec![Supertype::Legendary],
            ..ZoneChangeRecord::test_minimal(norin, Some(Zone::Exile), Zone::Battlefield)
        }),
    };

    process_triggers(&mut state, &[event]);
    super::drain_order_triggers_with_identity(&mut state);
    let observer_triggers = state
        .stack
        .iter()
        .filter(|e| e.source_id == observer)
        .count();
    assert_eq!(
        observer_triggers, 2,
        "Gandalf must double ETB triggers caused by a legendary permanent re-entering"
    );
}

/// CR 603.2d: Gandalf doubles ETB triggers caused by an artifact.
#[test]
fn gandalf_doubles_artifact_etb_triggers() {
    let (mut state, observer) = setup_with_observer(TriggerMode::ChangesZone);
    state
        .objects
        .get_mut(&observer)
        .unwrap()
        .trigger_definitions[0]
        .definition
        .destination = Some(Zone::Battlefield);
    let _gandalf = install_gandalf_doubler(&mut state);

    let genesis = create_object(
        &mut state,
        CardId(5333),
        PlayerId(0),
        "Genesis Chamber".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&genesis)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Artifact);

    let event = GameEvent::ZoneChanged {
        object_id: genesis,
        from: Some(Zone::Hand),
        to: Zone::Battlefield,
        record: Box::new(ZoneChangeRecord {
            name: "Genesis Chamber".to_string(),
            core_types: vec![CoreType::Artifact],
            ..ZoneChangeRecord::test_minimal(genesis, Some(Zone::Hand), Zone::Battlefield)
        }),
    };

    process_triggers(&mut state, &[event]);
    super::drain_order_triggers_with_identity(&mut state);
    let observer_triggers = state
        .stack
        .iter()
        .filter(|e| e.source_id == observer)
        .count();
    assert_eq!(
        observer_triggers, 2,
        "Gandalf must double ETB triggers caused by an artifact entering"
    );
}

/// CR 603.2d: Gandalf does not double ETB triggers from ordinary non-artifact creatures.
#[test]
fn gandalf_does_not_double_ordinary_creature_etb_triggers() {
    let (mut state, observer) = setup_with_observer(TriggerMode::ChangesZone);
    state
        .objects
        .get_mut(&observer)
        .unwrap()
        .trigger_definitions[0]
        .definition
        .destination = Some(Zone::Battlefield);
    let _gandalf = install_gandalf_doubler(&mut state);

    let greeter = create_object(
        &mut state,
        CardId(5334),
        PlayerId(0),
        "Gala Greeters".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&greeter)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);

    let event = GameEvent::ZoneChanged {
        object_id: greeter,
        from: Some(Zone::Hand),
        to: Zone::Battlefield,
        record: Box::new(ZoneChangeRecord {
            name: "Gala Greeters".to_string(),
            core_types: vec![CoreType::Creature],
            ..ZoneChangeRecord::test_minimal(greeter, Some(Zone::Hand), Zone::Battlefield)
        }),
    };

    process_triggers(&mut state, &[event]);
    super::drain_order_triggers_with_identity(&mut state);
    let observer_triggers = state
        .stack
        .iter()
        .filter(|e| e.source_id == observer)
        .count();
    assert_eq!(
        observer_triggers, 1,
        "Gandalf must not double ETB triggers caused by a non-legendary non-artifact creature"
    );
}

/// CR 603.2d: Isshin (CreatureAttacking cause) doubles attack triggers
/// of a permanent the controller owns.
#[test]
fn isshin_doubles_attack_triggers() {
    use crate::types::statics::TriggerCause;

    let (mut state, observer) = setup_with_observer(TriggerMode::Attacks);
    let _isshin = install_doubler(&mut state, TriggerCause::CreatureAttacking);

    // Ensure observer is a creature so it can attack and its trigger is for ITS attack.
    state
        .objects
        .get_mut(&observer)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);

    let event = GameEvent::AttackersDeclared {
        attacker_ids: vec![observer],
        defending_player: PlayerId(1),
        attacks: vec![(
            observer,
            crate::game::combat::AttackTarget::Player(PlayerId(1)),
        )],
    };

    process_triggers(&mut state, &[event]);
    // CR 603.3b (#531): drain the per-controller ordering prompt.
    super::drain_order_triggers_with_identity(&mut state);
    let observer_triggers = state
        .stack
        .iter()
        .filter(|e| e.source_id == observer)
        .count();
    assert_eq!(
        observer_triggers, 2,
        "Isshin must double the observer's attack trigger to 2 instances"
    );
}

/// CR 603.2d: Isshin does NOT double ETB triggers — the cause predicate
/// is `CreatureAttacking`, not `EntersBattlefield`.
#[test]
fn isshin_does_not_double_etb_triggers() {
    use crate::types::statics::TriggerCause;

    let (mut state, observer) = setup_with_observer(TriggerMode::ChangesZone);
    state
        .objects
        .get_mut(&observer)
        .unwrap()
        .trigger_definitions[0]
        .definition
        .destination = Some(Zone::Battlefield);
    let _isshin = install_doubler(&mut state, TriggerCause::CreatureAttacking);

    let new_etb = create_object(
        &mut state,
        CardId(9),
        PlayerId(0),
        "Entering Creature".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&new_etb)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);

    let event = GameEvent::ZoneChanged {
        object_id: new_etb,
        from: Some(Zone::Hand),
        to: Zone::Battlefield,
        record: Box::new(ZoneChangeRecord {
            name: "Entering Creature".to_string(),
            core_types: vec![CoreType::Creature],
            ..ZoneChangeRecord::test_minimal(new_etb, Some(Zone::Hand), Zone::Battlefield)
        }),
    };

    process_triggers(&mut state, &[event]);
    // CR 603.3b (#531): drain the per-controller ordering prompt.
    super::drain_order_triggers_with_identity(&mut state);
    let observer_triggers = state
        .stack
        .iter()
        .filter(|e| e.source_id == observer)
        .count();
    assert_eq!(
        observer_triggers, 1,
        "Isshin must NOT double ETB triggers — cause is CreatureAttacking"
    );
}

/// CR 603.2d: Panharmonicon (EntersBattlefield cause) does NOT double
/// attack triggers — the cause predicate filters to ETB only.
#[test]
fn panharmonicon_does_not_double_attack_triggers() {
    use crate::types::statics::TriggerCause;

    let (mut state, observer) = setup_with_observer(TriggerMode::Attacks);
    state
        .objects
        .get_mut(&observer)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);
    let _panh = install_doubler(
        &mut state,
        TriggerCause::EntersBattlefield {
            core_types: vec![CoreType::Artifact, CoreType::Creature],
        },
    );

    let event = GameEvent::AttackersDeclared {
        attacker_ids: vec![observer],
        defending_player: PlayerId(1),
        attacks: vec![(
            observer,
            crate::game::combat::AttackTarget::Player(PlayerId(1)),
        )],
    };

    process_triggers(&mut state, &[event]);
    // CR 603.3b (#531): drain the per-controller ordering prompt.
    super::drain_order_triggers_with_identity(&mut state);
    let observer_triggers = state
        .stack
        .iter()
        .filter(|e| e.source_id == observer)
        .count();
    assert_eq!(
        observer_triggers, 1,
        "Panharmonicon must NOT double attack triggers — cause is EntersBattlefield"
    );
}

/// CR 603.2d + CR 601.2 + CR 707.10: Veyran, Voice of Duality
/// (`ControllerCastOrCopiedSpell { [Instant, Sorcery] }`) doubles a
/// magecraft-style trigger when its controller casts an instant.
#[test]
fn veyran_doubles_trigger_caused_by_controller_casting_instant() {
    use crate::types::statics::TriggerCause;

    let (mut state, observer) = setup_with_observer(TriggerMode::SpellCastOrCopy);
    let _veyran = install_doubler(
        &mut state,
        TriggerCause::ControllerCastOrCopiedSpell {
            core_types: vec![CoreType::Instant, CoreType::Sorcery],
        },
    );

    // An instant spell on the stack, cast by the doubler's controller.
    let spell = create_object(
        &mut state,
        CardId(200),
        PlayerId(0),
        "Opt".to_string(),
        Zone::Stack,
    );
    state
        .objects
        .get_mut(&spell)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Instant);

    let event = GameEvent::SpellCast {
        card_id: CardId(200),
        controller: PlayerId(0),
        object_id: spell,
    };

    process_triggers(&mut state, &[event]);
    // CR 603.3b (#531): drain the per-controller ordering prompt.
    super::drain_order_triggers_with_identity(&mut state);
    let observer_triggers = state
        .stack
        .iter()
        .filter(|e| e.source_id == observer)
        .count();
    assert_eq!(
        observer_triggers, 2,
        "Veyran must double a trigger caused by its controller casting an instant"
    );
}

/// CR 603.2d: Veyran does NOT double attack triggers — the cause predicate is
/// `ControllerCastOrCopiedSpell`, not `CreatureAttacking`. Regression for
/// issue #5291 (Veyran was copying an attack trigger).
#[test]
fn veyran_does_not_double_attack_triggers() {
    use crate::types::statics::TriggerCause;

    let (mut state, observer) = setup_with_observer(TriggerMode::Attacks);
    state
        .objects
        .get_mut(&observer)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);
    let _veyran = install_doubler(
        &mut state,
        TriggerCause::ControllerCastOrCopiedSpell {
            core_types: vec![CoreType::Instant, CoreType::Sorcery],
        },
    );

    let event = GameEvent::AttackersDeclared {
        attacker_ids: vec![observer],
        defending_player: PlayerId(1),
        attacks: vec![(
            observer,
            crate::game::combat::AttackTarget::Player(PlayerId(1)),
        )],
    };

    process_triggers(&mut state, &[event]);
    // CR 603.3b (#531): drain the per-controller ordering prompt.
    super::drain_order_triggers_with_identity(&mut state);
    let observer_triggers = state
        .stack
        .iter()
        .filter(|e| e.source_id == observer)
        .count();
    assert_eq!(
        observer_triggers, 1,
        "Veyran must NOT double attack triggers — cause is ControllerCastOrCopiedSpell (#5291)"
    );
}

/// CR 603.2d + CR 601.2: Veyran does NOT double a trigger caused by an
/// OPPONENT casting a spell — "you casting or copying" scopes the cause to
/// the doubler's controller.
#[test]
fn veyran_does_not_double_opponent_cast_trigger() {
    use crate::types::statics::TriggerCause;

    let (mut state, observer) = setup_with_observer(TriggerMode::SpellCastOrCopy);
    let _veyran = install_doubler(
        &mut state,
        TriggerCause::ControllerCastOrCopiedSpell {
            core_types: vec![CoreType::Instant, CoreType::Sorcery],
        },
    );

    // An instant spell on the stack, cast by the OPPONENT.
    let spell = create_object(
        &mut state,
        CardId(201),
        PlayerId(1),
        "Opt".to_string(),
        Zone::Stack,
    );
    state
        .objects
        .get_mut(&spell)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Instant);

    let event = GameEvent::SpellCast {
        card_id: CardId(201),
        controller: PlayerId(1),
        object_id: spell,
    };

    process_triggers(&mut state, &[event]);
    super::drain_order_triggers_with_identity(&mut state);
    let observer_triggers = state
        .stack
        .iter()
        .filter(|e| e.source_id == observer)
        .count();
    assert_eq!(
        observer_triggers, 1,
        "Veyran must NOT double a trigger caused by an opponent's cast"
    );
}

/// Helper: install a source-restricted `DoubleTriggers` static
/// (Splinter-class) — cause `Any`, narrowed by an `affected` source filter —
/// controlled by PlayerId(0).
fn install_source_restricted_doubler(state: &mut GameState, affected: TargetFilter) -> ObjectId {
    use crate::types::statics::{StaticMode, TriggerCause};
    let id = create_object(
        state,
        CardId(101),
        PlayerId(0),
        "Splinter, Radical Rat".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.static_definitions.push(
        crate::types::ability::StaticDefinition::new(StaticMode::DoubleTriggers {
            cause: TriggerCause::Any,
        })
        .affected(affected),
    );
    id
}

fn install_harmonic_prodigy(state: &mut GameState) -> ObjectId {
    let id = create_object(
        state,
        CardId(102),
        PlayerId(0),
        "Harmonic Prodigy".to_string(),
        Zone::Battlefield,
    );
    state
            .objects
            .get_mut(&id)
            .unwrap()
            .static_definitions
            .push(
                crate::parser::oracle_static::parse_static_line(
                    "If a triggered ability of a Shaman or another Wizard you control triggers, that ability triggers an additional time.",
                )
                .expect("expected Harmonic Prodigy trigger-doubler static"),
            );
    id
}

fn install_delney(state: &mut GameState) -> ObjectId {
    let id = create_object(
        state,
        CardId(103),
        PlayerId(0),
        "Delney, Streetwise Lookout".to_string(),
        Zone::Battlefield,
    );
    state
            .objects
            .get_mut(&id)
            .unwrap()
            .static_definitions
            .push(
                crate::parser::oracle_static::parse_static_line(
                    "If a triggered ability of a creature you control with power 2 or less triggers, that ability triggers an additional time.",
                )
                .expect("expected Delney trigger-doubler static"),
            );
    id
}

/// CR 603.2d: Splinter's source filter ("a Ninja creature you control")
/// doubles a Ninja source's trigger to 2 instances.
#[test]
fn splinter_doubles_ninja_source_trigger() {
    use crate::types::ability::{ControllerRef, TypedFilter};

    let (mut state, observer) = setup_with_observer(TriggerMode::Attacks);
    {
        let obj = state.objects.get_mut(&observer).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.subtypes.push("Ninja".to_string());
    }
    let _splinter = install_source_restricted_doubler(
        &mut state,
        TargetFilter::Typed(
            TypedFilter::creature()
                .subtype("Ninja".to_string())
                .controller(ControllerRef::You),
        ),
    );

    let event = GameEvent::AttackersDeclared {
        attacker_ids: vec![observer],
        defending_player: PlayerId(1),
        attacks: vec![(
            observer,
            crate::game::combat::AttackTarget::Player(PlayerId(1)),
        )],
    };

    process_triggers(&mut state, &[event]);
    super::drain_order_triggers_with_identity(&mut state);
    let observer_triggers = state
        .stack
        .iter()
        .filter(|e| e.source_id == observer)
        .count();
    assert_eq!(
        observer_triggers, 2,
        "Splinter must double a Ninja source's trigger to 2 instances"
    );
}

/// CR 603.2d: Splinter's source filter must NOT double a non-Ninja source's
/// trigger — this is the reported bug (all triggers doubling). With the
/// `affected` filter populated, a non-Ninja creature's trigger stays at 1.
#[test]
fn splinter_does_not_double_non_ninja_source_trigger() {
    use crate::types::ability::{ControllerRef, TypedFilter};

    let (mut state, observer) = setup_with_observer(TriggerMode::Attacks);
    // Observer is a creature, but NOT a Ninja.
    state
        .objects
        .get_mut(&observer)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);
    let _splinter = install_source_restricted_doubler(
        &mut state,
        TargetFilter::Typed(
            TypedFilter::creature()
                .subtype("Ninja".to_string())
                .controller(ControllerRef::You),
        ),
    );

    let event = GameEvent::AttackersDeclared {
        attacker_ids: vec![observer],
        defending_player: PlayerId(1),
        attacks: vec![(
            observer,
            crate::game::combat::AttackTarget::Player(PlayerId(1)),
        )],
    };

    process_triggers(&mut state, &[event]);
    super::drain_order_triggers_with_identity(&mut state);
    let observer_triggers = state
        .stack
        .iter()
        .filter(|e| e.source_id == observer)
        .count();
    assert_eq!(
        observer_triggers, 1,
        "Splinter must NOT double a non-Ninja source's trigger — only Ninja sources qualify"
    );
}

/// CR 603.2d: Harmonic Prodigy's parsed disjunctive source filter must
/// double triggers from another Wizard you control.
#[test]
fn harmonic_prodigy_parsed_static_doubles_wizard_source_trigger() {
    let (mut state, observer) = setup_with_observer(TriggerMode::Attacks);
    {
        let obj = state.objects.get_mut(&observer).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.subtypes.push("Wizard".to_string());
    }

    let _harmonic = install_harmonic_prodigy(&mut state);

    let event = GameEvent::AttackersDeclared {
        attacker_ids: vec![observer],
        defending_player: PlayerId(1),
        attacks: vec![(
            observer,
            crate::game::combat::AttackTarget::Player(PlayerId(1)),
        )],
    };

    process_triggers(&mut state, &[event]);
    super::drain_order_triggers_with_identity(&mut state);
    let observer_triggers = state
        .stack
        .iter()
        .filter(|e| e.source_id == observer)
        .count();
    assert_eq!(
        observer_triggers, 2,
        "Harmonic Prodigy's parsed Wizard branch must double the source trigger"
    );
}

/// CR 603.2d: Harmonic Prodigy's parsed disjunctive source filter must not
/// fall back to the controller-only `affected: None` shape; unrelated
/// controlled sources still produce one trigger.
#[test]
fn harmonic_prodigy_parsed_static_does_not_double_unrelated_source_trigger() {
    let (mut state, observer) = setup_with_observer(TriggerMode::Attacks);
    {
        let obj = state.objects.get_mut(&observer).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.subtypes.push("Cleric".to_string());
    }

    let _harmonic = install_harmonic_prodigy(&mut state);

    let event = GameEvent::AttackersDeclared {
        attacker_ids: vec![observer],
        defending_player: PlayerId(1),
        attacks: vec![(
            observer,
            crate::game::combat::AttackTarget::Player(PlayerId(1)),
        )],
    };

    process_triggers(&mut state, &[event]);
    super::drain_order_triggers_with_identity(&mut state);
    let observer_triggers = state
        .stack
        .iter()
        .filter(|e| e.source_id == observer)
        .count();
    assert_eq!(
        observer_triggers, 1,
        "Harmonic Prodigy must not double unrelated controlled source triggers"
    );
}

/// CR 603.2d: Delney's parsed power-filtered source clause doubles a
/// controlled creature with power 2 or less.
#[test]
fn delney_parsed_static_doubles_low_power_creature_trigger() {
    let (mut state, observer) = setup_with_observer(TriggerMode::Attacks);
    {
        let obj = state.objects.get_mut(&observer).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);
    }

    let _delney = install_delney(&mut state);

    let event = GameEvent::AttackersDeclared {
        attacker_ids: vec![observer],
        defending_player: PlayerId(1),
        attacks: vec![(
            observer,
            crate::game::combat::AttackTarget::Player(PlayerId(1)),
        )],
    };

    process_triggers(&mut state, &[event]);
    super::drain_order_triggers_with_identity(&mut state);
    let observer_triggers = state
        .stack
        .iter()
        .filter(|e| e.source_id == observer)
        .count();
    assert_eq!(
        observer_triggers, 2,
        "Delney must double a power-2-or-less creature source's trigger"
    );
}

/// CR 603.2d: Delney must not double triggers from creatures with power
/// greater than 2.
#[test]
fn delney_parsed_static_does_not_double_high_power_creature_trigger() {
    let (mut state, observer) = setup_with_observer(TriggerMode::Attacks);
    {
        let obj = state.objects.get_mut(&observer).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(4);
        obj.toughness = Some(4);
    }

    let _delney = install_delney(&mut state);

    let event = GameEvent::AttackersDeclared {
        attacker_ids: vec![observer],
        defending_player: PlayerId(1),
        attacks: vec![(
            observer,
            crate::game::combat::AttackTarget::Player(PlayerId(1)),
        )],
    };

    process_triggers(&mut state, &[event]);
    super::drain_order_triggers_with_identity(&mut state);
    let observer_triggers = state
        .stack
        .iter()
        .filter(|e| e.source_id == observer)
        .count();
    assert_eq!(
        observer_triggers, 1,
        "Delney must not double a power-greater-than-2 creature source's trigger"
    );
}

/// CR 603.2d: Delney must not double triggered abilities from non-creature
/// permanents you control.
#[test]
fn delney_parsed_static_does_not_double_non_creature_source_trigger() {
    let (mut state, observer) = setup_with_observer(TriggerMode::Attacks);
    {
        let obj = state.objects.get_mut(&observer).unwrap();
        obj.card_types.core_types = vec![CoreType::Enchantment];
        obj.card_types.subtypes.clear();
    }

    let _delney = install_delney(&mut state);

    let event = GameEvent::AttackersDeclared {
        attacker_ids: vec![observer],
        defending_player: PlayerId(1),
        attacks: vec![(
            observer,
            crate::game::combat::AttackTarget::Player(PlayerId(1)),
        )],
    };

    process_triggers(&mut state, &[event]);
    super::drain_order_triggers_with_identity(&mut state);
    let observer_triggers = state
        .stack
        .iter()
        .filter(|e| e.source_id == observer)
        .count();
    assert_eq!(
        observer_triggers, 1,
        "Delney must not double non-creature source triggers"
    );
}

/// CR 603.2d: Isshin + Panharmonicon — only Isshin matches an attack
/// event, so the total is 2 (original + 1 from Isshin).
#[test]
fn isshin_and_panharmonicon_only_isshin_matches_attack_event() {
    use crate::types::statics::TriggerCause;

    let (mut state, observer) = setup_with_observer(TriggerMode::Attacks);
    state
        .objects
        .get_mut(&observer)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);
    let _isshin = install_doubler(&mut state, TriggerCause::CreatureAttacking);
    let _panh = install_doubler(
        &mut state,
        TriggerCause::EntersBattlefield {
            core_types: vec![CoreType::Artifact, CoreType::Creature],
        },
    );

    let event = GameEvent::AttackersDeclared {
        attacker_ids: vec![observer],
        defending_player: PlayerId(1),
        attacks: vec![(
            observer,
            crate::game::combat::AttackTarget::Player(PlayerId(1)),
        )],
    };

    process_triggers(&mut state, &[event]);
    // CR 603.3b (#531): drain the per-controller ordering prompt.
    super::drain_order_triggers_with_identity(&mut state);
    let observer_triggers = state
        .stack
        .iter()
        .filter(|e| e.source_id == observer)
        .count();
    assert_eq!(
        observer_triggers, 2,
        "Only Isshin's cause matches the attack event — total should be 2 (original + 1 clone)"
    );
}

/// CR 603.2d + CR 603.6c: Drivnod (CreatureDying cause) doubles a
/// dies-triggered ability of a permanent the controller owns.
#[test]
fn drivnod_doubles_dies_triggers() {
    use crate::types::statics::TriggerCause;

    let (mut state, observer) = setup_with_observer(TriggerMode::ChangesZone);
    state
        .objects
        .get_mut(&observer)
        .unwrap()
        .trigger_definitions[0]
        .definition
        .destination = Some(Zone::Graveyard);
    let _drivnod = install_doubler(&mut state, TriggerCause::CreatureDying);

    let dying = create_object(
        &mut state,
        CardId(20),
        PlayerId(0),
        "Dying Creature".to_string(),
        Zone::Graveyard,
    );
    state
        .objects
        .get_mut(&dying)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);

    let event = GameEvent::ZoneChanged {
        object_id: dying,
        from: Some(Zone::Battlefield),
        to: Zone::Graveyard,
        record: Box::new(ZoneChangeRecord {
            name: "Dying Creature".to_string(),
            core_types: vec![CoreType::Creature],
            ..ZoneChangeRecord::test_minimal(dying, Some(Zone::Battlefield), Zone::Graveyard)
        }),
    };

    process_triggers(&mut state, &[event]);
    // CR 603.3b (#531): drain the per-controller ordering prompt.
    super::drain_order_triggers_with_identity(&mut state);
    let observer_triggers = state
        .stack
        .iter()
        .filter(|e| e.source_id == observer)
        .count();
    assert_eq!(
        observer_triggers, 2,
        "Drivnod must double the observer's dies trigger to 2 instances"
    );
}

/// CR 603.2d: Wayta (ControlledCreatureDealtDamage cause) doubles only
/// triggers caused by a creature you control being dealt damage.
#[test]
fn wayta_doubles_damage_caused_triggers() {
    use crate::types::statics::TriggerCause;

    let (mut state, observer) = setup_with_observer(TriggerMode::DamageDone);
    let _wayta = install_doubler(&mut state, TriggerCause::ControlledCreatureDealtDamage);
    let damaged = create_object(
        &mut state,
        CardId(21),
        PlayerId(0),
        "Damaged Creature".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&damaged)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);
    let source = create_object(
        &mut state,
        CardId(22),
        PlayerId(1),
        "Damage Source".to_string(),
        Zone::Battlefield,
    );

    let event = GameEvent::DamageDealt {
        source_id: source,
        target: TargetRef::Object(damaged),
        amount: 2,
        is_combat: false,
        excess: 0,
    };

    process_triggers(&mut state, &[event]);
    super::drain_order_triggers_with_identity(&mut state);
    let observer_triggers = state
        .stack
        .iter()
        .filter(|e| e.source_id == observer)
        .count();
    assert_eq!(
        observer_triggers, 2,
        "Wayta must double damage-caused triggers of permanents the controller owns"
    );
}

/// CR 603.2d: Wayta must not double triggers unrelated to controlled-creature damage.
#[test]
fn wayta_does_not_double_unrelated_triggers() {
    use crate::types::statics::TriggerCause;

    let (mut state, observer) = setup_with_observer(TriggerMode::ChangesZone);
    state
        .objects
        .get_mut(&observer)
        .unwrap()
        .trigger_definitions[0]
        .definition
        .destination = Some(Zone::Battlefield);
    let _wayta = install_doubler(&mut state, TriggerCause::ControlledCreatureDealtDamage);

    let new_etb = create_object(
        &mut state,
        CardId(23),
        PlayerId(0),
        "Entering Creature".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&new_etb)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);

    let event = GameEvent::ZoneChanged {
        object_id: new_etb,
        from: Some(Zone::Hand),
        to: Zone::Battlefield,
        record: Box::new(ZoneChangeRecord::test_minimal(
            new_etb,
            Some(Zone::Hand),
            Zone::Battlefield,
        )),
    };

    process_triggers(&mut state, &[event]);
    super::drain_order_triggers_with_identity(&mut state);
    let observer_triggers = state
        .stack
        .iter()
        .filter(|e| e.source_id == observer)
        .count();
    assert_eq!(
        observer_triggers, 1,
        "Wayta must not double ETB triggers when the cause is damage to your creature"
    );
}

/// Install Cloud, Midgar Mercenary with its parsed trigger-doubler static.
/// The static is `DoubleTriggers{cause:Any}` scoped to `affected =
/// Or[SelfRef, Typed(Equipment, AttachedToSource)]` and gated on
/// `condition = SourceIsEquipped` — exercising the real parser output.
fn install_cloud(state: &mut GameState) -> ObjectId {
    let id = create_object(
        state,
        CardId(120),
        PlayerId(0),
        "Cloud, Midgar Mercenary".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.static_definitions.push(
        crate::parser::oracle_static::parse_static_line(
            "As long as ~ is equipped, if a triggered ability of ~ or an Equipment attached to it triggers, that ability triggers an additional time.",
        )
        .expect("expected Cloud trigger-doubler static"),
    );
    id
}

/// Attach an Equipment (subtype "Equipment") to `host`, optionally carrying an
/// attacks-mode draw trigger that fires on any AttackersDeclared event. When
/// `with_trigger` is false it only satisfies the `SourceIsEquipped` gate.
fn attach_equipment(
    state: &mut GameState,
    host: ObjectId,
    card: CardId,
    with_trigger: bool,
) -> ObjectId {
    let id = create_object(
        state,
        card,
        PlayerId(0),
        "Buster Sword".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.card_types.subtypes.push("Equipment".to_string());
        obj.attached_to = Some(host.into());
        if with_trigger {
            let mut trigger = draw_one_trigger(TriggerMode::Attacks);
            trigger.valid_card = Some(TargetFilter::Any);
            obj.trigger_definitions.push(trigger);
        }
    }
    state.objects.get_mut(&host).unwrap().attachments.push(id);
    id
}

fn count_triggers_from(state: &GameState, source: ObjectId) -> usize {
    state.stack.iter().filter(|e| e.source_id == source).count()
}

fn attack_event(attacker: ObjectId) -> GameEvent {
    GameEvent::AttackersDeclared {
        attacker_ids: vec![attacker],
        defending_player: PlayerId(1),
        attacks: vec![(
            attacker,
            crate::game::combat::AttackTarget::Player(PlayerId(1)),
        )],
    }
}

/// CR 603.2d + CR 301.5a: An Equipment attached to an equipped Cloud has its
/// trigger doubled — the `affected` `Typed(Equipment, AttachedToSource)` arm
/// matches the attachment, and the `SourceIsEquipped` gate holds. Confirms the
/// narrowed affected filter still covers the attached-Equipment case.
#[test]
fn cloud_doubles_attached_equipment_trigger_when_equipped() {
    let mut state = setup();
    let cloud = install_cloud(&mut state);
    let equip = attach_equipment(&mut state, cloud, CardId(121), true);
    let attacker = make_creature(&mut state, PlayerId(0), "Soldier", 1, 1);

    process_triggers(&mut state, &[attack_event(attacker)]);
    super::drain_order_triggers_with_identity(&mut state);
    assert_eq!(
        count_triggers_from(&state, equip),
        2,
        "Cloud must double an attached Equipment's trigger while equipped"
    );
}

/// CR 603.2d: Self-inclusion — Cloud's OWN triggered ability is doubled while
/// equipped, because its `affected` filter references the source (`~`), so the
/// self-exclusion carve-out fires. Discriminating: reverting the runtime
/// `affected_references_self` gate (or the SelfRef affected arm) re-applies the
/// unconditional self-exclusion and this drops to 1.
#[test]
fn cloud_doubles_its_own_trigger_when_equipped_self_inclusion() {
    let mut state = setup();
    let cloud = install_cloud(&mut state);
    {
        let mut trigger = draw_one_trigger(TriggerMode::Attacks);
        trigger.valid_card = Some(TargetFilter::Any);
        state
            .objects
            .get_mut(&cloud)
            .unwrap()
            .trigger_definitions
            .push(trigger);
    }
    // Equip Cloud (plain Equipment, no trigger of its own) to satisfy the gate.
    let _equip = attach_equipment(&mut state, cloud, CardId(122), false);
    let attacker = make_creature(&mut state, PlayerId(0), "Soldier", 1, 1);

    process_triggers(&mut state, &[attack_event(attacker)]);
    super::drain_order_triggers_with_identity(&mut state);
    assert_eq!(
        count_triggers_from(&state, cloud),
        2,
        "equipped Cloud must double its OWN triggered ability (self-inclusion)"
    );
}

/// CR 301.5a: Unequipped Cloud does NOT double — the `SourceIsEquipped`
/// condition gates the static off. Discriminating: reverting the dispatch
/// condition re-attachment leaves `condition: None`, the static stays active,
/// self-inclusion fires, and Cloud's own trigger doubles to 2.
#[test]
fn cloud_does_not_double_when_unequipped() {
    let mut state = setup();
    let cloud = install_cloud(&mut state);
    {
        let mut trigger = draw_one_trigger(TriggerMode::Attacks);
        trigger.valid_card = Some(TargetFilter::Any);
        state
            .objects
            .get_mut(&cloud)
            .unwrap()
            .trigger_definitions
            .push(trigger);
    }
    // No Equipment attached → SourceIsEquipped is false.
    let attacker = make_creature(&mut state, PlayerId(0), "Soldier", 1, 1);

    process_triggers(&mut state, &[attack_event(attacker)]);
    super::drain_order_triggers_with_identity(&mut state);
    assert_eq!(
        count_triggers_from(&state, cloud),
        1,
        "unequipped Cloud must not double — SourceIsEquipped gate is unmet"
    );
}

/// CR 603.2d: An unrelated permanent you control (not Cloud, not an Equipment
/// attached to Cloud) is NOT doubled even while Cloud is equipped — the
/// `affected` filter narrows to self + attached Equipment. Discriminating:
/// reverting the SelfRef/Equipment affected arms leaves `affected: None`, which
/// over-doubles every controlled trigger and this rises to 2.
#[test]
fn cloud_does_not_double_unrelated_controlled_trigger() {
    let mut state = setup();
    let cloud = install_cloud(&mut state);
    let _equip = attach_equipment(&mut state, cloud, CardId(123), false);
    // Unrelated creature you control with its own attacks-mode trigger.
    let unrelated = make_creature(&mut state, PlayerId(0), "Chocobo", 2, 2);
    {
        let mut trigger = draw_one_trigger(TriggerMode::Attacks);
        trigger.valid_card = Some(TargetFilter::Any);
        state
            .objects
            .get_mut(&unrelated)
            .unwrap()
            .trigger_definitions
            .push(trigger);
    }

    process_triggers(&mut state, &[attack_event(unrelated)]);
    super::drain_order_triggers_with_identity(&mut state);
    assert_eq!(
        count_triggers_from(&state, unrelated),
        1,
        "Cloud must not double an unrelated controlled permanent's trigger"
    );
}

/// CR 603.4 + CR 701.9: Intervening-if "if an opponent discarded a card this
/// turn" evaluates against the per-turn discard counts. Verifies both the
/// positive (opponent discarded → condition met) and negative (no opponent
/// discarded → condition unmet, as well as only-controller-discarded →
/// condition unmet) paths for Tinybones, Trinket Thief.
#[test]
fn intervening_if_opponent_discarded_this_turn_gates_trigger() {
    use crate::types::ability::{
        AggregateFunction, Comparator, PlayerScope, QuantityExpr, QuantityRef, TriggerCondition,
    };

    let mut state = GameState::new_two_player(42);
    let controller = PlayerId(0);
    let opponent = PlayerId(1);

    let condition = TriggerCondition::QuantityComparison {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::CardsDiscardedThisTurn {
                player: PlayerScope::Opponent {
                    aggregate: AggregateFunction::Sum,
                },
            },
        },
        comparator: Comparator::GE,
        rhs: QuantityExpr::Fixed { value: 1 },
    };

    // No one has discarded yet → condition not met.
    assert!(
        !check_trigger_condition(&state, &condition, controller, None, None),
        "empty discard set must fail the intervening-if"
    );

    // Only the controller discarded → still no opponent discard → condition unmet.
    crate::game::restrictions::record_discard(&mut state, controller);
    assert!(
        !check_trigger_condition(&state, &condition, controller, None, None),
        "self-discard must not satisfy 'an opponent discarded a card this turn'"
    );

    // Opponent discarded → condition met.
    crate::game::restrictions::record_discard(&mut state, opponent);
    assert!(
        check_trigger_condition(&state, &condition, controller, None, None),
        "opponent-discard must satisfy 'an opponent discarded a card this turn'"
    );
}

/// Regression test for GitHub issue #1356: Tinybones, Trinket Thief end step trigger
/// should fire when an opponent discards a card this turn. This test verifies the
/// specific card's trigger works correctly with the discard tracking system.
#[test]
fn tinybones_end_step_trigger_fires_when_opponent_discards() {
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AggregateFunction, Comparator, PlayerScope, QuantityExpr, QuantityRef, TriggerCondition,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;

    let mut state = GameState::new_two_player(42);
    let controller = PlayerId(0);
    let opponent = PlayerId(1);

    // Create Tinybones with its end step trigger
    let tinybones = create_object(
        &mut state,
        CardId(100),
        controller,
        "Tinybones, Trinket Thief".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&tinybones).unwrap();
        obj.card_types.core_types = vec![CoreType::Creature];
        // Tinybones trigger: "At the beginning of each end step, if an opponent discarded a card this turn, you draw a card and you lose 1 life"
        obj.trigger_definitions.push(
            TriggerDefinition::new(TriggerMode::Phase)
                .phase(Phase::End)
                .condition(TriggerCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::CardsDiscardedThisTurn {
                            player: PlayerScope::Opponent {
                                aggregate: AggregateFunction::Sum,
                            },
                        },
                    },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 1 },
                })
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                ))
                .description("Tinybones end step trigger".to_string()),
        );
    }

    // Record that opponent discarded a card
    crate::game::restrictions::record_discard(&mut state, opponent);

    // Verify the condition is met
    let condition = TriggerCondition::QuantityComparison {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::CardsDiscardedThisTurn {
                player: PlayerScope::Opponent {
                    aggregate: AggregateFunction::Sum,
                },
            },
        },
        comparator: Comparator::GE,
        rhs: QuantityExpr::Fixed { value: 1 },
    };
    assert!(
        check_trigger_condition(&state, &condition, controller, None, None),
        "Tinybones trigger condition should be met when opponent discarded"
    );
}

/// Regression test for GitHub issue #2022: Mangara the Diplomat trigger
/// should fire when exactly one creature attacks the controller. This test
/// verifies the AttackersDeclaredCount trigger condition works correctly.
#[test]
fn mangara_trigger_fires_when_exactly_one_attacker() {
    use crate::game::combat::AttackTarget;
    use crate::game::zones::create_object;
    use crate::types::ability::{Comparator, ControllerRef, TriggerCondition};
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;

    let mut state = GameState::new_two_player(42);
    let controller = PlayerId(0);
    let opponent = PlayerId(1);

    // Create Mangara with its attackers declared trigger
    let mangara = create_object(
        &mut state,
        CardId(100),
        controller,
        "Mangara, the Diplomat".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&mangara).unwrap();
        obj.card_types.core_types = vec![CoreType::Creature];
        // Mangara trigger: "Whenever an opponent attacks with exactly one creature, that creature can't block this combat"
        obj.trigger_definitions.push(
            TriggerDefinition::new(TriggerMode::AttackersDeclared)
                .condition(TriggerCondition::AttackersDeclaredCount {
                    subject: crate::types::ability::AttackersDeclaredCountSubject::Controller {
                        scope: ControllerRef::Opponent,
                        filter: None,
                    },
                    comparator: Comparator::EQ,
                    count: 1,
                })
                .description("Mangara attackers declared trigger".to_string()),
        );
    }

    // Create an attacking creature
    let attacker = create_object(
        &mut state,
        CardId(101),
        opponent,
        "Attacker".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&attacker)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Creature];

    // Simulate exactly one attacker being declared
    let event = GameEvent::AttackersDeclared {
        attacker_ids: vec![attacker],
        defending_player: controller,
        attacks: vec![(attacker, AttackTarget::Player(controller))],
    };

    // Verify the condition is met
    let condition = TriggerCondition::AttackersDeclaredCount {
        subject: crate::types::ability::AttackersDeclaredCountSubject::Controller {
            scope: ControllerRef::Opponent,
            filter: None,
        },
        comparator: Comparator::EQ,
        count: 1,
    };
    assert!(
        check_trigger_condition(&state, &condition, controller, Some(mangara), Some(&event)),
        "Mangara trigger condition should be met when exactly one creature attacks"
    );
}

/// Regression test for GitHub issue #2022: Mangara the Diplomat trigger
/// should NOT fire when two or more creatures attack. This test verifies
/// the "exactly one" condition is enforced correctly.
#[test]
fn mangara_trigger_does_not_fire_when_two_attackers() {
    use crate::game::combat::AttackTarget;
    use crate::game::zones::create_object;
    use crate::types::ability::{Comparator, ControllerRef, TriggerCondition};
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;

    let mut state = GameState::new_two_player(42);
    let controller = PlayerId(0);
    let opponent = PlayerId(1);

    // Create Mangara with its attackers declared trigger
    let mangara = create_object(
        &mut state,
        CardId(100),
        controller,
        "Mangara, the Diplomat".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&mangara).unwrap();
        obj.card_types.core_types = vec![CoreType::Creature];
        obj.trigger_definitions.push(
            TriggerDefinition::new(TriggerMode::AttackersDeclared)
                .condition(TriggerCondition::AttackersDeclaredCount {
                    subject: crate::types::ability::AttackersDeclaredCountSubject::Controller {
                        scope: ControllerRef::Opponent,
                        filter: None,
                    },
                    comparator: Comparator::EQ,
                    count: 1,
                })
                .description("Mangara attackers declared trigger".to_string()),
        );
    }

    // Create two attacking creatures
    let attacker1 = create_object(
        &mut state,
        CardId(101),
        opponent,
        "Attacker1".to_string(),
        Zone::Battlefield,
    );
    let attacker2 = create_object(
        &mut state,
        CardId(102),
        opponent,
        "Attacker2".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&attacker1)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Creature];
    state
        .objects
        .get_mut(&attacker2)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Creature];

    // Simulate two attackers being declared
    let event = GameEvent::AttackersDeclared {
        attacker_ids: vec![attacker1, attacker2],
        defending_player: controller,
        attacks: vec![
            (attacker1, AttackTarget::Player(controller)),
            (attacker2, AttackTarget::Player(controller)),
        ],
    };

    // Verify the condition is NOT met
    let condition = TriggerCondition::AttackersDeclaredCount {
        subject: crate::types::ability::AttackersDeclaredCountSubject::Controller {
            scope: ControllerRef::Opponent,
            filter: None,
        },
        comparator: Comparator::EQ,
        count: 1,
    };
    assert!(
        !check_trigger_condition(&state, &condition, controller, Some(mangara), Some(&event)),
        "Mangara trigger condition should NOT be met when two creatures attack"
    );
}

/// Issue #451 — RUNTIME PIPELINE TEST. CR 603.4 + CR 701.21: A who-controls
/// sacrifice trigger ("Whenever an opponent who controls an artifact
/// sacrifices a permanent, ...") must parse the relative clause into an
/// `ObjectCount >= 1` intervening-if and gate the trigger correctly at
/// runtime.
///
/// This drives the real pipeline: the parser produces the `TriggerMode`
/// and `TriggerDefinition.condition`, then `check_trigger_condition` (the
/// exact evaluator `apply` uses for intervening-ifs) is run against a real
/// `GameState`. The triggering player (the sacrificer) is bound from a
/// `PermanentSacrificed` event. NOT a shape test — the condition under test
/// is the parser's actual output, evaluated by the runtime evaluator.
#[test]
fn issue_451_who_controls_sacrifice_trigger_gates_at_runtime() {
    let mut ctx = crate::parser::oracle_ir::context::ParseContext::default();
    let (mode, def) = crate::parser::oracle_trigger::parse_trigger_condition(
        "Whenever an opponent who controls an artifact sacrifices a permanent",
        &mut ctx,
    );
    assert_eq!(
        mode,
        TriggerMode::Sacrificed,
        "who-controls sacrifice line must parse to Sacrificed (not Unknown)",
    );
    let condition = def
        .condition
        .expect("the who-controls clause must be lifted into def.condition");

    let mut state = GameState::new_two_player(42);
    let controller = PlayerId(0); // the trigger source's controller
    let sacrificer = PlayerId(1); // the opponent who sacrifices

    // Sacrifice event — the triggering player is the sacrificer (P1).
    let sac_event = GameEvent::PermanentSacrificed {
        object_id: ObjectId(777),
        player_id: sacrificer,
    };

    // No one controls an artifact → the who-controls intervening-if fails.
    assert!(
        !check_trigger_condition(&state, &condition, controller, None, Some(&sac_event)),
        "with no artifact in play the who-controls clause must fail the trigger",
    );

    // The CONTROLLER (P0) controls an artifact, but the triggering player
    // is P1 → the clause (scoped to TriggeringPlayer) still fails.
    let p0_artifact = create_object(
        &mut state,
        CardId(300),
        controller,
        "Some Artifact".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&p0_artifact)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Artifact);
    assert!(
        !check_trigger_condition(&state, &condition, controller, None, Some(&sac_event)),
        "an artifact controlled by the trigger's controller (not the \
             sacrificer) must NOT satisfy 'who controls an artifact'",
    );

    // The SACRIFICER (P1, the triggering player) controls an artifact →
    // the who-controls clause is satisfied and the trigger fires.
    let p1_artifact = create_object(
        &mut state,
        CardId(301),
        sacrificer,
        "Some Artifact".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&p1_artifact)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Artifact);
    assert!(
        check_trigger_condition(&state, &condition, controller, None, Some(&sac_event)),
        "an artifact controlled by the sacrificing (triggering) player \
             must satisfy 'who controls an artifact' and fire the trigger",
    );
}

#[test]
fn breena_triggers_when_defending_opponent_has_more_life_than_another_opponent() {
    use crate::game::combat::AttackTarget;
    use crate::types::format::FormatConfig;

    let mut state = GameState::new(FormatConfig::commander(), 3, 42);
    let breena_controller = PlayerId(0);
    let attacking_player = PlayerId(1);
    let defending_player = PlayerId(2);

    state.players[0].life = 40;
    state.players[1].life = 30;
    state.players[2].life = 35;

    let breena = create_object(
        &mut state,
        CardId(1),
        breena_controller,
        "Breena, the Demagogue".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&breena).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        let trig_def = crate::parser::oracle_trigger::parse_trigger_line(
                "Whenever a player attacks one of your opponents, if that opponent has more life than another of your opponents, that attacking player draws a card and you put two +1/+1 counters on a creature you control.",
                "Breena, the Demagogue",
            );
        obj.trigger_definitions.push(trig_def.clone());
        std::sync::Arc::make_mut(&mut obj.base_trigger_definitions).push(trig_def);
    }

    let attacker = create_object(
        &mut state,
        CardId(2),
        attacking_player,
        "Attacker".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&attacker)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);
    let second_attacker = create_object(
        &mut state,
        CardId(3),
        attacking_player,
        "Second Attacker".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&second_attacker)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);

    process_triggers(
        &mut state,
        &[GameEvent::AttackersDeclared {
            attacker_ids: vec![attacker, second_attacker],
            defending_player,
            attacks: vec![
                (attacker, AttackTarget::Player(defending_player)),
                (second_attacker, AttackTarget::Player(defending_player)),
            ],
        }],
    );

    let breena_trigger_count = state
        .stack
        .iter()
        .filter(|entry| {
            entry.source_id == breena
                && entry.controller == breena_controller
                && matches!(
                    &entry.kind,
                    StackEntryKind::TriggeredAbility { ability, .. }
                        if matches!(ability.effect, Effect::Draw { .. })
                )
        })
        .count();
    assert_eq!(
            breena_trigger_count, 1,
            "Breena must trigger when a player attacks an opponent with more life than another opponent"
        );

    state.stack.clear();
    state
        .players
        .iter_mut()
        .find(|p| p.id == defending_player)
        .unwrap()
        .life = 25;

    process_triggers(
        &mut state,
        &[GameEvent::AttackersDeclared {
            attacker_ids: vec![attacker, second_attacker],
            defending_player,
            attacks: vec![
                (attacker, AttackTarget::Player(defending_player)),
                (second_attacker, AttackTarget::Player(defending_player)),
            ],
        }],
    );

    assert!(
        state.stack.is_empty(),
        "Breena must not trigger when the defending opponent fails the intervening-if"
    );
}

#[test]
fn defending_player_life_quantity_reads_attack_event_player_target() {
    use crate::game::combat::AttackTarget;
    use crate::types::ability::{
        AggregateFunction, Comparator, PlayerScope, QuantityExpr, QuantityRef, TriggerCondition,
    };
    use crate::types::format::FormatConfig;

    let mut state = GameState::new(FormatConfig::commander(), 3, 42);
    let controller = PlayerId(0);
    let attacked_player = PlayerId(1);
    let other_opponent = PlayerId(2);
    let attacker = create_object(
        &mut state,
        CardId(1),
        controller,
        "Commander".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&attacker)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);

    state.players[0].life = 40;
    state.players[1].life = 35;
    state.players[2].life = 40;

    let condition = TriggerCondition::QuantityComparison {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::LifeTotal {
                player: PlayerScope::Opponent {
                    aggregate: AggregateFunction::Max,
                },
            },
        },
        comparator: Comparator::LE,
        rhs: QuantityExpr::Ref {
            qty: QuantityRef::LifeTotal {
                player: PlayerScope::DefendingPlayer,
            },
        },
    };
    let event = GameEvent::AttackersDeclared {
        attacker_ids: vec![attacker],
        defending_player: attacked_player,
        attacks: vec![(attacker, AttackTarget::Player(attacked_player))],
    };

    assert!(
            !check_trigger_condition(&state, &condition, controller, Some(attacker), Some(&event)),
            "another opponent with more life than the attacked player must fail Guild Artisan's intervening-if"
        );

    state
        .players
        .iter_mut()
        .find(|p| p.id == other_opponent)
        .unwrap()
        .life = 35;
    assert!(
        check_trigger_condition(&state, &condition, controller, Some(attacker), Some(&event)),
        "condition must pass when no opponent has more life than the attacked player"
    );
}

/// CR 603.4 + CR 109.3: Valakut-style "if you control at least five other
/// Mountains" must exclude the triggering (newly-entered) Mountain from the
/// count. With exactly 5 Mountains on the battlefield where one of them is
/// the trigger object, the condition is *not* met (only 4 "other" Mountains).
/// With 6 Mountains (5 others + triggering), the condition *is* met.
#[test]
fn intervening_if_other_than_trigger_object_excludes_triggering_mountain() {
    use crate::types::ability::{
        Comparator, ControllerRef, FilterProp, QuantityExpr, QuantityRef, TargetFilter,
        TriggerCondition, TypeFilter, TypedFilter,
    };

    // Helper: create a Mountain on the battlefield under `player`.
    fn make_mountain(state: &mut GameState, player: PlayerId, n: usize) -> ObjectId {
        let id = create_object(
            state,
            CardId(0),
            player,
            format!("Mountain {n}"),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push("Mountain".to_string());
        obj.base_card_types = obj.card_types.clone();
        id
    }

    let mut state = GameState::new_two_player(42);
    let controller = PlayerId(0);

    // Valakut source (not a Mountain subtype).
    let valakut_id = create_object(
        &mut state,
        CardId(1),
        controller,
        "Valakut".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&valakut_id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.base_card_types = obj.card_types.clone();
    }
    // 4 pre-existing Mountains.
    for n in 0..4 {
        make_mountain(&mut state, controller, n);
    }
    // The triggering (newly-entered) Mountain — 5th Mountain total.
    let trigger_id = make_mountain(&mut state, controller, 100);

    let condition = TriggerCondition::QuantityComparison {
        lhs: QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Subtype("Mountain".to_string())],
                    controller: Some(ControllerRef::You),
                    properties: vec![FilterProp::OtherThanTriggerObject],
                }),
            },
        },
        comparator: Comparator::GE,
        rhs: QuantityExpr::Fixed { value: 5 },
    };

    let event = GameEvent::ZoneChanged {
        object_id: trigger_id,
        from: Some(Zone::Library),
        to: Zone::Battlefield,
        record: Box::new(ZoneChangeRecord::test_minimal(
            trigger_id,
            Some(Zone::Library),
            Zone::Battlefield,
        )),
    };

    // 4 other Mountains + 1 triggering = 5 total. Excluding the triggering
    // Mountain leaves 4, which is NOT ≥ 5 — the trigger condition must fail.
    assert!(
        !check_trigger_condition(
            &state,
            &condition,
            controller,
            Some(valakut_id),
            Some(&event)
        ),
        "with only 4 other Mountains, the condition must fail"
    );

    // Add a 5th non-triggering Mountain → 5 others + 1 triggering = 6 total.
    make_mountain(&mut state, controller, 200);
    assert!(
        check_trigger_condition(
            &state,
            &condition,
            controller,
            Some(valakut_id),
            Some(&event)
        ),
        "with 5 other Mountains, the condition must pass"
    );
}

// ── CR 603.3b — Trigger-order choice for simultaneous triggers (issue #531) ──

/// Helper: install a permanent with a `TriggerMode::Phase` trigger whose
/// effect draws `n` cards for the controller (no targets, no input). Used
/// by the simultaneous-trigger ordering tests.
fn make_phase_trigger_source(
    state: &mut GameState,
    owner: PlayerId,
    name: &str,
    draw_count: i32,
) -> ObjectId {
    let id = make_creature(state, owner, name, 1, 1);
    let trig_def = TriggerDefinition::new(TriggerMode::Phase)
        .phase(Phase::Upkeep)
        .execute(AbilityDefinition::new(
            AbilityKind::Database,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: draw_count },
                target: TargetFilter::Controller,
            },
        ))
        .description(format!("{name}: at the beginning of upkeep, draw a card."));
    let obj = state.objects.get_mut(&id).unwrap();
    obj.trigger_definitions.push(trig_def.clone());
    std::sync::Arc::make_mut(&mut obj.base_trigger_definitions).push(trig_def);
    obj.materialize_base_trigger_definitions();
    id
}

/// Helper: install an optional upkeep trigger whose accepted resolution
/// would target an opponent's creature. With no such object available, an
/// auto-accepted instance is inert and should be
/// suppressed before simultaneous-trigger ordering.
fn make_optional_phase_trigger_with_no_legal_target(
    state: &mut GameState,
    owner: PlayerId,
    name: &str,
) -> ObjectId {
    let id = make_creature(state, owner, name, 1, 1);
    let target = TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::Opponent));
    let trig_def = TriggerDefinition::new(TriggerMode::Phase)
        .phase(Phase::Upkeep)
        .optional()
        .execute(
            AbilityDefinition::new(
                AbilityKind::Database,
                Effect::SetTapState {
                    target,
                    scope: EffectScope::Single,
                    state: TapStateChange::Tap,
                },
            )
            .optional(),
        )
        .description(format!("{name}: tap target creature card in your hand."));
    let obj = state.objects.get_mut(&id).unwrap();
    obj.trigger_definitions.push(trig_def.clone());
    std::sync::Arc::make_mut(&mut obj.base_trigger_definitions).push(trig_def);
    obj.materialize_base_trigger_definitions();
    id
}

/// Helper: install an optional upkeep trigger whose accepted resolution runs
/// `effect`. Used with effects that surface no stack-time target slot (CR 115.1):
/// context-refs whose "target" is the controller / "you" (`Effect::Draw`,
/// `Effect::Token`), and resolution-time selections (`Effect::Sacrifice`). All
/// are ALWAYS resolvable, so an auto-accepted instance must NOT be suppressed as
/// inert.
fn make_optional_phase_trigger_effect(
    state: &mut GameState,
    owner: PlayerId,
    name: &str,
    effect: Effect,
) -> ObjectId {
    let id = make_creature(state, owner, name, 1, 1);
    let trig_def = TriggerDefinition::new(TriggerMode::Phase)
        .phase(Phase::Upkeep)
        .optional()
        .execute(AbilityDefinition::new(AbilityKind::Database, effect).optional())
        .description(format!("{name}: at the beginning of upkeep, you may."));
    let obj = state.objects.get_mut(&id).unwrap();
    obj.trigger_definitions.push(trig_def.clone());
    std::sync::Arc::make_mut(&mut obj.base_trigger_definitions).push(trig_def);
    id
}

/// A context-ref "you may draw a card" effect (target is the controller).
fn optional_draw_effect() -> Effect {
    Effect::Draw {
        count: QuantityExpr::Fixed { value: 1 },
        target: TargetFilter::Controller,
    }
}

/// A context-ref "you may create a 1/1 token" effect (owner is the controller) —
/// the Brood Sliver class. Goes through a DIFFERENT `target_filter()` match arm
/// than `Effect::Draw`, so it independently exercises the context-ref guard.
fn optional_create_token_effect() -> Effect {
    Effect::Token {
        name: "Sliver".into(),
        power: crate::types::ability::PtValue::Fixed(1),
        toughness: crate::types::ability::PtValue::Fixed(1),
        types: vec!["Creature".into()],
        colors: vec![],
        keywords: vec![],
        tapped: false,
        count: QuantityExpr::Fixed { value: 1 },
        owner: TargetFilter::Controller,
        attach_to: None,
        enters_attacking: false,
        supertypes: vec![],
        static_abilities: vec![],
        enter_with_counters: vec![],
    }
}

/// A "you may sacrifice a creature" effect. CR 701.21a: Sacrifice chooses its
/// permanents at RESOLUTION (via `EffectZoneChoice`), so
/// `extract_target_filter_from_effect` returns `None` (no stack-time slot) even
/// though raw `target_filter()` yields a NON-context-ref filter. This is the
/// discriminator between the single-authority fix and a narrow context-ref-only
/// guard: only delegating to the slot builder keeps this trigger off the inert
/// list.
fn optional_sacrifice_effect() -> Effect {
    Effect::Sacrifice {
        target: TargetFilter::Typed(TypedFilter::creature()),
        count: QuantityExpr::Fixed { value: 1 },
        min_count: 0,
    }
}

/// Read the source IDs of the current stack entries in stack-bottom-to-top
/// order. Each `StackEntry::source_id` lets the test discriminate which
/// trigger ended up where.
fn stack_source_ids(state: &GameState) -> Vec<ObjectId> {
    state.stack.iter().map(|e| e.source_id).collect()
}

/// CR 603.3b: When the active player controls two simultaneously-firing
/// triggers, `process_triggers` must surface `WaitingFor::OrderTriggers`
/// rather than placing them on the stack in a fixed deterministic order.
/// **Discriminator**: submitting two different permutations produces two
/// different stacks. A deterministic-ordering engine would yield the same
/// stack for both inputs and fail this test.
#[test]
fn order_triggers_two_distinct_orders_produce_distinct_stacks() {
    let run = |order: Vec<usize>| -> Vec<ObjectId> {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.phase = Phase::Upkeep;
        let src_a = make_phase_trigger_source(&mut state, PlayerId(0), "Source A", 1);
        let src_b = make_phase_trigger_source(&mut state, PlayerId(0), "Source B", 1);
        // Pre-stamp entered timestamps so collect_pending_triggers has a
        // deterministic placement seed.
        state
            .objects
            .get_mut(&src_a)
            .unwrap()
            .entered_battlefield_turn = Some(1);
        state
            .objects
            .get_mut(&src_b)
            .unwrap()
            .entered_battlefield_turn = Some(2);

        let event = GameEvent::PhaseChanged {
            phase: Phase::Upkeep,
        };
        process_triggers(&mut state, &[event]);

        // The active player must be prompted to order the two triggers.
        let WaitingFor::OrderTriggers { player, triggers } = state.waiting_for.clone() else {
            panic!(
                "expected WaitingFor::OrderTriggers, got {:?}",
                state.waiting_for
            );
        };
        assert_eq!(player, PlayerId(0));
        assert_eq!(triggers.len(), 2, "both triggers must be in the prompt");

        crate::game::engine::apply_as_current(&mut state, GameAction::OrderTriggers { order })
            .expect("submit chosen order");

        stack_source_ids(&state)
    };

    let stack_identity = run(vec![0, 1]);
    let stack_reversed = run(vec![1, 0]);
    assert_eq!(stack_identity.len(), 2);
    assert_eq!(stack_reversed.len(), 2);
    assert_ne!(
        stack_identity, stack_reversed,
        "different OrderTriggers permutations must yield distinct stack orderings — \
             a deterministic engine (no player choice) would produce identical stacks"
    );
    // And the reversed input is literally the identity's reverse.
    let mut expected = stack_identity.clone();
    expected.reverse();
    assert_eq!(
        stack_reversed, expected,
        "stack-bottom-to-top ordering must mirror the submitted permutation"
    );
}

/// CR 603.3b: A player with exactly one trigger needs no ordering choice.
/// `process_triggers` must NOT emit `WaitingFor::OrderTriggers`; the
/// trigger goes straight to the stack via the existing dispatch loop.
#[test]
fn order_triggers_single_trigger_does_not_prompt() {
    let mut state = setup();
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.phase = Phase::Upkeep;
    let _src = make_phase_trigger_source(&mut state, PlayerId(0), "Solo Source", 1);

    process_triggers(
        &mut state,
        &[GameEvent::PhaseChanged {
            phase: Phase::Upkeep,
        }],
    );

    assert!(
        !matches!(state.waiting_for, WaitingFor::OrderTriggers { .. }),
        "single trigger must not prompt for ordering; got {:?}",
        state.waiting_for
    );
    assert!(
        state.pending_trigger_order.is_none(),
        "no in-flight ordering state for a single trigger"
    );
    assert_eq!(
        state.stack.len(),
        1,
        "the single trigger reaches the stack directly"
    );
}

/// CR 603.3b + CR 603.3d: Auto-accepted optional triggers that have no legal
/// resolution target are inert. A group of only inert triggers must not
/// surface an `OrderTriggers` prompt; otherwise large repeated observer
/// piles can spend the fast-forward budget asking the player to order
/// triggers that cannot do anything.
#[test]
fn auto_accepted_optional_triggers_without_legal_targets_are_suppressed() {
    let mut state = setup();
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.phase = Phase::Upkeep;
    let src_a =
        make_optional_phase_trigger_with_no_legal_target(&mut state, PlayerId(0), "Source A");
    let src_b =
        make_optional_phase_trigger_with_no_legal_target(&mut state, PlayerId(0), "Source B");

    for source_id in [src_a, src_b] {
        state.set_may_trigger_auto_choice(
            MayTriggerAutoChoiceKey {
                player: PlayerId(0),
                source_id,
                origin: live_trigger_origin(&state, source_id, 0),
            },
            AutoMayChoice::Accept,
        );
    }

    process_triggers(
        &mut state,
        &[GameEvent::PhaseChanged {
            phase: Phase::Upkeep,
        }],
    );

    assert!(
        !matches!(state.waiting_for, WaitingFor::OrderTriggers { .. }),
        "inert auto-accepted optional triggers must not prompt for ordering"
    );
    assert!(
        state.pending_trigger_order.is_none(),
        "suppressed inert triggers must not leave an ordering pass"
    );
    assert!(
        state.stack.is_empty(),
        "suppressed inert triggers must not reach the stack"
    );
}

/// CR 603.3d + CR 115.1 — regression for the "don't ask again → Yes silently
/// answers No" report. An auto-ACCEPTED optional trigger that surfaces no
/// stack-time target slot is ALWAYS resolvable, so it MUST reach the stack — it
/// is not an inert no-op.
///
/// The perf refactor (#3917) gated inert-suppression on raw
/// `target_filter().is_some() && slots.is_empty()`. But `target_filter()` is not
/// the authority for "surfaces a chooseable slot": `extract_target_filter_from_effect`
/// is, and it returns `None` for both context-refs (CR 121.1 "you may draw a
/// card", the controller is never a declared target) AND resolution-time
/// selections (CR 701.21a "you may sacrifice a creature", chosen during
/// resolution). All report a `Some(non-context-ref)` from raw `target_filter()`
/// yet build zero slots, so the gate mistook a remembered Accept for "no legal
/// targets" and dropped the trigger — indistinguishable from a decline.
///
/// Three arms cover the class through THREE distinct `target_filter()`/authority
/// paths: "you may draw a card" (`Effect::Draw`), Brood Sliver's "you may create
/// a Sliver token" (`Effect::Token`), and "you may sacrifice a creature"
/// (`Effect::Sacrifice`). The Sacrifice arm is the discriminator that a
/// context-ref-only guard would still drop — it forces the fix to delegate to
/// the slot-builder authority rather than re-derive a subset.
///
/// This drives `process_triggers` — the trigger-collection/stack path where the
/// drop happens — so it cannot be satisfied by the resolution-path coverage in
/// `effects/mod.rs` (`saved_accept_for_may_trigger_resolves_without_prompt`).
#[test]
fn auto_accepted_optional_context_ref_effect_reaches_stack() {
    for (label, effect) in [
        ("you may draw a card", optional_draw_effect()),
        ("you may create a token", optional_create_token_effect()),
        ("you may sacrifice a creature", optional_sacrifice_effect()),
    ] {
        let mut state = setup();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.phase = Phase::Upkeep;
        let src = make_optional_phase_trigger_effect(&mut state, PlayerId(0), "Remembered", effect);

        // Player remembered "yes, do it every time" for this trigger.
        state.set_may_trigger_auto_choice(
            MayTriggerAutoChoiceKey {
                player: PlayerId(0),
                source_id: src,
                origin: live_trigger_origin(&state, src, 0),
            },
            AutoMayChoice::Accept,
        );

        process_triggers(
            &mut state,
            &[GameEvent::PhaseChanged {
                phase: Phase::Upkeep,
            }],
        );

        assert!(
            !matches!(state.waiting_for, WaitingFor::OrderTriggers { .. }),
            "[{label}] a single auto-accepted trigger needs no ordering prompt; got {:?}",
            state.waiting_for
        );
        assert_eq!(
            state.stack.len(),
            1,
            "[{label}] an auto-accepted context-ref trigger is always resolvable \
             (its controller target is never chosen) and must reach the stack — \
             not be dropped as an inert no-op"
        );
    }
}

/// CR 603.3b: Two genuinely INDISTINGUISHABLE no-input triggers (same
/// controller, same name → identical `format!("{name}: ...")` description →
/// byte-identical normalized ability, no targets/modes/division) commute
/// under any permutation, so the engine auto-orders them with NO
/// `OrderTriggers` prompt (matching MTG Arena). Both still reach the stack.
#[test]
fn order_triggers_identical_no_input_triggers_auto_order() {
    let mut state = setup();
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.phase = Phase::Upkeep;
    // SAME name on both → identical descriptions → indistinguishable.
    let _src_a = make_phase_trigger_source(&mut state, PlayerId(0), "Twin Source", 1);
    let _src_b = make_phase_trigger_source(&mut state, PlayerId(0), "Twin Source", 1);

    process_triggers(
        &mut state,
        &[GameEvent::PhaseChanged {
            phase: Phase::Upkeep,
        }],
    );

    assert!(
        !matches!(state.waiting_for, WaitingFor::OrderTriggers { .. }),
        "indistinguishable no-input triggers must auto-order without a prompt; got {:?}",
        state.waiting_for
    );
    assert!(
        state.pending_trigger_order.is_none(),
        "no in-flight ordering state when the group auto-orders"
    );
    assert_eq!(
        state.stack.len(),
        2,
        "both auto-ordered triggers reach the stack directly"
    );
}

/// CR 603.3b + CR 603.7c: Two triggers whose normalized abilities are
/// byte-identical but whose firing event context differs
/// (`subject_match_count`) resolve differently, so they are NOT
/// indistinguishable and MUST still prompt for ordering. Guards the
/// `subject_match_count` comparison in `group_is_order_independent` from a
/// silent regression that would collapse them.
#[test]
fn order_triggers_distinct_event_context_still_prompt() {
    let mut state = setup();
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.phase = Phase::Upkeep;
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };

    // A bare no-input draw ability shared by both pending triggers.
    let ability = ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
        Vec::new(),
        ObjectId(0),
        PlayerId(0),
    );
    // Two PendingTriggers identical in every ordering-relevant field EXCEPT
    // `subject_match_count` (Some(1) vs Some(2)) — the CR 603.2c batched
    // event-context divergence that makes them distinguishable.
    let make_ctx = |source: ObjectId, count: u32| {
        PendingTriggerContext::single(PendingTrigger {
            source_id: source,
            controller: PlayerId(0),
            condition: None,
            ability: ability.clone(),
            timestamp: count,
            target_constraints: Vec::new(),
            distribute: None,
            trigger_event: None,
            modal: None,
            mode_abilities: Vec::new(),
            description: Some("Twin: draw a card.".to_string()),
            may_trigger_origin: None,
            subject_match_count: Some(count),
            die_result: None,
        })
    };
    let ctx_a = make_ctx(ObjectId(1), 1);
    let ctx_b = make_ctx(ObjectId(2), 2);

    let disposition = begin_trigger_ordering(&mut state, vec![ctx_a, ctx_b]);
    assert!(
        matches!(disposition, TriggerOrderingDisposition::PromptForChoice(_)),
        "distinct subject_match_count must still prompt (CR 603.2c event context)"
    );
    assert!(
        state.pending_trigger_order.is_some(),
        "a live ordering pass must back the prompt"
    );
}

/// CR 603.3b + CR 603.7c: Different firing events may be ignored only when
/// the resolved ability does not read event context. If the ability resolves
/// through `TriggeringSource`, the concrete event is visible at resolution,
/// so otherwise-identical no-input triggers must still prompt.
#[test]
fn order_triggers_event_context_ability_still_prompts_on_distinct_events() {
    let mut state = setup();
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.phase = Phase::Upkeep;
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };

    let ability = ResolvedAbility::new(
        Effect::SetTapState {
            target: TargetFilter::TriggeringSource,
            scope: EffectScope::Single,
            state: TapStateChange::Tap,
        },
        Vec::new(),
        ObjectId(0),
        PlayerId(0),
    );
    let make_ctx = |source: ObjectId, event_object: ObjectId| {
        PendingTriggerContext::single(PendingTrigger {
            source_id: source,
            controller: PlayerId(0),
            condition: None,
            ability: ability.clone(),
            timestamp: source.0 as u32,
            target_constraints: Vec::new(),
            distribute: None,
            trigger_event: Some(GameEvent::PermanentTapped {
                object_id: event_object,
                caused_by: None,
            }),
            modal: None,
            mode_abilities: Vec::new(),
            description: Some("Twin: tap the triggering source.".to_string()),
            may_trigger_origin: None,
            subject_match_count: None,
            die_result: None,
        })
    };
    let ctx_a = make_ctx(ObjectId(1), ObjectId(11));
    let ctx_b = make_ctx(ObjectId(2), ObjectId(22));

    let disposition = begin_trigger_ordering(&mut state, vec![ctx_a, ctx_b]);
    assert!(
        matches!(disposition, TriggerOrderingDisposition::PromptForChoice(_)),
        "distinct trigger_event must still prompt when the ability reads TriggeringSource"
    );
    assert!(
        state.pending_trigger_order.is_some(),
        "a live ordering pass must back the prompt"
    );
}

#[test]
fn archenemy_hero_team_orders_triggers_from_multiple_heroes_together() {
    let mut state = GameState::new(crate::types::format::FormatConfig::archenemy(), 4, 42);
    state.active_player = PlayerId(1);
    state.priority_player = PlayerId(1);
    state.phase = Phase::Upkeep;
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(1),
    };

    let make_ctx = |source: ObjectId, controller: PlayerId, description: &str| {
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(0),
            controller,
        );
        PendingTriggerContext::single(PendingTrigger {
            source_id: source,
            controller,
            condition: None,
            ability,
            timestamp: source.0 as u32,
            target_constraints: Vec::new(),
            distribute: None,
            trigger_event: None,
            modal: None,
            mode_abilities: Vec::new(),
            description: Some(description.to_string()),
            may_trigger_origin: None,
            subject_match_count: None,
            die_result: None,
        })
    };

    let disposition = begin_trigger_ordering(
        &mut state,
        vec![
            make_ctx(ObjectId(1), PlayerId(1), "Hero one trigger."),
            make_ctx(ObjectId(2), PlayerId(2), "Hero two trigger."),
        ],
    );

    let TriggerOrderingDisposition::PromptForChoice(prompt) = disposition else {
        panic!("hero-team trigger group must prompt for ordering");
    };
    assert!(matches!(
        *prompt,
        WaitingFor::OrderTriggers {
            player: PlayerId(1),
            ..
        }
    ));
    let order = state.pending_trigger_order.as_ref().unwrap();
    assert_eq!(order.groups.len(), 1);
    assert_eq!(order.groups[0].controller, PlayerId(1));
    assert_eq!(
        order.groups[0]
            .triggers
            .iter()
            .map(|ctx| ctx.pending.controller)
            .collect::<Vec<_>>(),
        vec![PlayerId(1), PlayerId(2)]
    );
}

/// CR 603.3b: A group needs an ordering prompt when its triggers are
/// distinguishable. Two `make_phase_trigger_source` permanents with
/// DIFFERENT names produce distinct `format!("{name}: ...")` descriptions,
/// so the same-controller upkeep group still surfaces `OrderTriggers` even
/// though identical suspend-style triggers now auto-order. Guards the
/// auto_advance / upkeep prompt path covered formerly by the suspend test.
#[test]
fn multiple_distinct_upkeep_triggers_still_prompt() {
    let mut state = setup();
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.phase = Phase::Upkeep;
    let _src_a = make_phase_trigger_source(&mut state, PlayerId(0), "Upkeep Source A", 1);
    let _src_b = make_phase_trigger_source(&mut state, PlayerId(0), "Upkeep Source B", 1);

    process_triggers(
        &mut state,
        &[GameEvent::PhaseChanged {
            phase: Phase::Upkeep,
        }],
    );

    let WaitingFor::OrderTriggers { player, triggers } = state.waiting_for.clone() else {
        panic!(
            "distinct same-controller upkeep triggers must still prompt; got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(player, PlayerId(0), "controller orders own triggers");
    assert_eq!(
        triggers.len(),
        2,
        "both distinct upkeep triggers await ordering"
    );
    assert!(
        state.pending_trigger_order.is_some(),
        "the ordering pass must be live while the prompt is up"
    );
}

/// CR 603.3b + CR 101.4: With the active player NOT in seat 0, two
/// non-active players' simultaneous triggers must be placed in turn order
/// from the active player — not by timestamp. Regression for the binary
/// active/non-active sort key that lumped every non-active player into one
/// timestamp-ordered bucket: here P0's source is older than P2's, so the old
/// key placed P0 before P2 by timestamp, but turn order from active P1 is
/// P1, P2, P0, so P2 must be lower on the stack than P0.
#[test]
fn order_triggers_apnap_two_nonactive_players_use_turn_order() {
    let mut state = GameState::new(crate::types::format::FormatConfig::commander(), 3, 123);
    // Active player is P1 (seat 1) — the case the binary key gets wrong.
    state.active_player = PlayerId(1);
    state.priority_player = PlayerId(1);
    state.phase = Phase::Upkeep;
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(1),
    };

    // One trigger each for two non-active players, so neither is prompted to
    // order and both reach the stack directly. P0's source is OLDER than
    // P2's, so a timestamp-based NAP ordering would place P0 first.
    let p2 = make_phase_trigger_source(&mut state, PlayerId(2), "P2 Source", 1);
    let p0 = make_phase_trigger_source(&mut state, PlayerId(0), "P0 Source", 1);
    state.objects.get_mut(&p0).unwrap().entered_battlefield_turn = Some(1);
    state.objects.get_mut(&p2).unwrap().entered_battlefield_turn = Some(2);

    process_triggers(
        &mut state,
        &[GameEvent::PhaseChanged {
            phase: Phase::Upkeep,
        }],
    );

    // Neither player controls 2+ triggers, so there is no ordering prompt.
    assert!(
        !matches!(state.waiting_for, WaitingFor::OrderTriggers { .. }),
        "single trigger per player must not prompt; got {:?}",
        state.waiting_for
    );

    // Turn order from active P1 is P1, P2, P0. The engine stores the stack
    // bottom-to-top, so P2 is lower and P0 is above it. The old binary key
    // ordered the two NAPs by timestamp instead, yielding [P0, P2].
    let stack_sources = stack_source_ids(&state);
    assert_eq!(stack_sources.len(), 2, "both triggers reach the stack");
    assert_eq!(
        stack_sources,
        vec![p2, p0],
        "non-active players must be placed by turn order (P2 below P0), not timestamp"
    );
}

/// CR 603.3b + CR 101.4 + CR 405.3: In a 3-player game with both AP and
/// NAP controlling 2 simultaneous triggers each, the active player is
/// prompted FIRST (CR 101.4 — APNAP choice order), then each NAP in turn
/// order. The final stack reflects the placement order (AP first = bottom
/// of stack) per CR 405.3.
#[test]
fn order_triggers_apnap_three_players() {
    let mut state = GameState::new(crate::types::format::FormatConfig::commander(), 3, 123);
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.phase = Phase::Upkeep;
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };

    let p0_a = make_phase_trigger_source(&mut state, PlayerId(0), "P0 Source A", 1);
    let p0_b = make_phase_trigger_source(&mut state, PlayerId(0), "P0 Source B", 1);
    let p1_a = make_phase_trigger_source(&mut state, PlayerId(1), "P1 Source A", 1);
    let p1_b = make_phase_trigger_source(&mut state, PlayerId(1), "P1 Source B", 1);
    for (i, id) in [p0_a, p0_b, p1_a, p1_b].iter().enumerate() {
        state.objects.get_mut(id).unwrap().entered_battlefield_turn = Some(i as u32 + 1);
    }

    process_triggers(
        &mut state,
        &[GameEvent::PhaseChanged {
            phase: Phase::Upkeep,
        }],
    );

    // CR 101.4: active player (P0) is prompted FIRST.
    let WaitingFor::OrderTriggers { player, .. } = state.waiting_for.clone() else {
        panic!(
            "expected OrderTriggers for P0 first, got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(player, PlayerId(0), "AP must choose before NAPs (CR 101.4)");

    // P0 submits identity order.
    crate::game::engine::apply_as_current(
        &mut state,
        GameAction::OrderTriggers { order: vec![0, 1] },
    )
    .expect("P0 submits");

    // Next prompt: P1 (next NAP in turn order).
    let WaitingFor::OrderTriggers { player, .. } = state.waiting_for.clone() else {
        panic!(
            "expected OrderTriggers for P1 after P0, got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(player, PlayerId(1));

    crate::game::engine::apply_as_current(
        &mut state,
        GameAction::OrderTriggers { order: vec![0, 1] },
    )
    .expect("P1 submits");

    // Now all four triggers must be on the stack; AP's pair must be placed
    // FIRST (bottom of stack per CR 405.3 + 603.3b APNAP).
    let stack_sources = stack_source_ids(&state);
    assert_eq!(stack_sources.len(), 4, "four triggers on the stack");
    // Bottom two are the AP (P0)'s pair; top two are the NAP (P1)'s pair.
    let p1_ids = [p1_a, p1_b];
    let p0_ids = [p0_a, p0_b];
    for id in &stack_sources[0..2] {
        assert!(
            p0_ids.contains(id),
            "stack bottom must contain AP triggers (CR 405.3 + 603.3b)"
        );
    }
    for id in &stack_sources[2..4] {
        assert!(
            p1_ids.contains(id),
            "stack top must contain NAP triggers (CR 405.3 + 603.3b)"
        );
    }
}
