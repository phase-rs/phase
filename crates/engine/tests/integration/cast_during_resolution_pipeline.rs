//! Pipeline-level regression for casting a Cascade/Discover hit DURING the
//! resolution of its source spell (CR 608.2g), rather than granting a lingering
//! `ExileWithAltCost` permission that requires a separate later `CastSpell`.
//!
//! CR 608.2g: "If an effect specifically instructs or allows a player to cast a
//! spell during resolution, they do so by following the steps in rules
//! 601.2a–i, except no player receives priority after it's cast." Accepting the
//! offer must put the hit directly onto the stack; the active player legitimately
//! retains priority (CR 117.3b) with the hit on the stack, and the opponent only
//! gets priority later via normal passing.
//!
//! These tests drive `apply()` end-to-end (CastSpell → PassPriority → the
//! CastOffer accept/decline) so that the `resolve_top` + CastOffer-accept gate is
//! exercised. They never call `resolve()` directly — that bypass is the exact
//! reason this class of bug shipped.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, CastingPermission, Effect, ReplacementDefinition, TargetFilter,
};
use engine::types::actions::{CastChoice, GameAction};
use engine::types::game_state::{
    CastOfferKind, CastPaymentMode, GameState, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::{EtbTapState, Zone};

const THRUMMING_STONE_ORACLE: &str = "Spells you cast have ripple 4. (When you cast a spell, you may reveal the top four cards of your library. You may cast any revealed cards with the same name as the cast spell without paying their mana costs. Put the rest on the bottom of your library in any order.)";
const RAT_COLONY_ORACLE: &str = "Rat Colony gets +1/+0 for each other Rat you control named Rat Colony.\nA deck can have any number of cards named Rat Colony.";
const AETHERFLUX_RESERVOIR_ORACLE: &str = "Whenever you cast a spell, you gain 1 life for each spell you've cast this turn.\nPay 50 life: This artifact deals 50 damage to any target.";

/// CR 614.1a: a synthetic global Library-destination redirect used to force a
/// genuine CR 616.1 ordering choice in the terminal Ripple bottom batch.
fn redirect_library_move_to(destination: Zone) -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Library)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                destination,
                origin: None,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        ))
}

fn red_pool(scenario: &mut GameScenario, count: usize) {
    let units: Vec<ManaUnit> = (0..count)
        .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
        .collect();
    scenario.with_mana_pool(P0, units);
}

/// A high-mana-value nonland on the library top: a cascade/discover MISS,
/// because its MV is above the source MV (cascade) / discover N. It is exiled
/// before the real hit since `add_spell_to_library_top` inserts at the top.
fn high_mv_miss(scenario: &mut GameScenario, name: &str) -> ObjectId {
    scenario
        .add_spell_to_library_top(P0, name, true)
        .with_mana_cost(ManaCost::generic(9))
        .id()
}

/// Returns true when `obj` retains any `ExileWithAltCost` permission — the
/// lingering-grant leak this fix eliminates.
fn has_exile_alt_cost(state: &GameState, id: ObjectId) -> bool {
    state.objects[&id]
        .casting_permissions
        .iter()
        .any(|p| matches!(p, CastingPermission::ExileWithAltCost { .. }))
}

/// CR 702.60a-b + CR 603.3b: a Ripple-found Rat Colony is cast while the
/// parent Ripple still has another offer open. Its own Ripple trigger must be
/// deferred, then put above the newly cast spell after that parent finishes.
#[test]
fn thrumming_stone_ripple_retriggers_for_each_cast_rat_colony() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    red_pool(&mut scenario, 2);

    let stone = scenario
        .add_creature_from_oracle(P0, "Thrumming Stone", 1, 1, THRUMMING_STONE_ORACLE)
        .id();
    let rat_a = scenario
        .add_creature_to_hand_from_oracle(P0, "Rat Colony", 2, 1, RAT_COLONY_ORACLE)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    // Add bottom first: Ripple reveals B then C, leaving C available for B's
    // own trigger after A's outstanding offer is declined.
    let rat_c = scenario
        .add_creature_to_hand_from_oracle(P0, "Rat Colony", 2, 1, RAT_COLONY_ORACLE)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let rat_b = scenario
        .add_creature_to_hand_from_oracle(P0, "Rat Colony", 2, 1, RAT_COLONY_ORACLE)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        engine::game::zones::remove_from_zone(state, rat_b, Zone::Hand, P0);
        engine::game::zones::add_to_zone(state, rat_b, Zone::Library, P0);
        state.objects.get_mut(&rat_b).expect("rat B").zone = Zone::Library;
        engine::game::zones::remove_from_zone(state, rat_c, Zone::Hand, P0);
        engine::game::zones::add_to_zone(state, rat_c, Zone::Library, P0);
        state.objects.get_mut(&rat_c).expect("rat C").zone = Zone::Library;
    }
    assert!(
        !runner.state().objects[&stone].static_definitions.is_empty(),
        "exact Thrumming Stone Oracle text must parse to its static Ripple grant"
    );
    assert!(
        !runner.state().objects[&rat_a].static_definitions.is_empty(),
        "exact Rat Colony Oracle text must parse both of its static abilities"
    );
    assert!(
        runner.state().objects[&rat_a]
            .abilities
            .iter()
            .all(|ability| { !matches!(ability.effect.as_ref(), Effect::Unimplemented { .. }) }),
        "exact Rat Colony Oracle text must not leak an unimplemented spell ability"
    );

    let card_id = runner.state().objects[&rat_a].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: rat_a,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast first Rat Colony");
    runner.act(GameAction::PassPriority).expect("p0 pass");
    runner.act(GameAction::PassPriority).expect("p1 pass");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::CastOffer {
            kind: CastOfferKind::Ripple { hit_card, .. },
            ..
        } if hit_card == rat_b
    ));
    runner
        .act(GameAction::RippleChoice {
            choice: CastChoice::Cast,
        })
        .expect("cast first revealed Rat Colony");

    // B was announced, but A's Ripple resolution is still offering C. B's
    // trigger must be parked rather than lost or placed early.
    assert_eq!(runner.state().objects[&rat_b].zone, Zone::Stack);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::CastOffer {
            kind: CastOfferKind::Ripple { hit_card, .. },
            ..
        } if hit_card == rat_c
    ));
    assert!(
        !runner.state().stack.iter().any(|entry| matches!(
            &entry.kind,
            StackEntryKind::TriggeredAbility { source_id, ability, .. }
                if *source_id == rat_b && matches!(ability.effect, Effect::Ripple { .. })
        )),
        "B's Ripple trigger cannot be put on the stack before A's Ripple finishes"
    );

    runner
        .act(GameAction::RippleChoice {
            choice: CastChoice::Cast,
        })
        .expect("cast A's final revealed Rat Colony");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OrderTriggers { ref triggers, .. } if triggers.len() == 2
    ));
    runner
        .act(GameAction::OrderTriggers { order: vec![0, 1] })
        .expect("order the combined B/C Ripple trigger batch");
    let retrigger_sources: Vec<_> = runner
        .state()
        .stack
        .iter()
        .filter_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility {
                source_id, ability, ..
            } if matches!(ability.effect, Effect::Ripple { .. }) => Some(*source_id),
            _ => None,
        })
        .collect();
    assert_eq!(
        retrigger_sources.iter().filter(|&&id| id == rat_b).count(),
        1,
        "B's parked Ripple must join the terminal cast batch exactly once"
    );
    assert_eq!(
        retrigger_sources.iter().filter(|&&id| id == rat_c).count(),
        1,
        "C's SpellCast Ripple must join B's parked trigger before either is ordered"
    );
    assert!(
        runner.state().deferred_triggers.is_empty(),
        "the terminal accepted cast must settle the one deferred trigger batch"
    );
    assert!(
        runner.state().pending_resolution_completion.is_none(),
        "the terminal settlement marker must clear only after C is announced"
    );
    let b_stack_index = runner
        .state()
        .stack
        .iter()
        .position(|entry| entry.id == rat_b)
        .expect("B remains on the stack beneath its trigger");
    let c_stack_index = runner
        .state()
        .stack
        .iter()
        .position(|entry| entry.id == rat_c)
        .expect("C remains on the stack beneath its trigger");
    let first_retrigger_index = runner
        .state()
        .stack
        .iter()
        .position(|entry| {
            matches!(
                &entry.kind,
                StackEntryKind::TriggeredAbility { source_id, ability, .. }
                    if (*source_id == rat_b || *source_id == rat_c)
                        && matches!(ability.effect, Effect::Ripple { .. })
            )
        })
        .expect("the combined Ripple trigger batch is on the stack");
    assert!(
        first_retrigger_index > b_stack_index && first_retrigger_index > c_stack_index,
        "the combined B/C trigger batch must be above both cast Rat Colonies"
    );
    assert_eq!(runner.state().objects[&rat_c].zone, Zone::Stack);
}

/// CR 702.60a + CR 603.3b + CR 616.1: a terminal accepted Ripple cast whose
/// miss bottom batch pauses for competing replacements keeps the typed terminal
/// completion through the prompt, then puts B/C's exactly-once Ripple triggers
/// above both spells after the final cast actually completes.
#[test]
fn thrumming_stone_terminal_ripple_waits_through_bottom_replacement_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    red_pool(&mut scenario, 2);
    scenario.add_creature_from_oracle(P0, "Thrumming Stone", 1, 1, THRUMMING_STONE_ORACLE);
    let rat_a = scenario
        .add_creature_to_hand_from_oracle(P0, "Rat Colony", 2, 1, RAT_COLONY_ORACLE)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let rat_c = scenario
        .add_creature_to_hand_from_oracle(P0, "Rat Colony", 2, 1, RAT_COLONY_ORACLE)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let miss = scenario
        .add_spell_to_hand_from_oracle(P0, "Terminal Ripple Miss", true, "Draw a card.")
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let rat_b = scenario
        .add_creature_to_hand_from_oracle(P0, "Rat Colony", 2, 1, RAT_COLONY_ORACLE)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    for (name, destination) in [
        ("Ripple Library Redirect to Hand", Zone::Hand),
        ("Ripple Library Redirect to Graveyard", Zone::Graveyard),
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_library_move_to(destination));
    }
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        for id in [rat_b, miss, rat_c] {
            engine::game::zones::remove_from_zone(state, id, Zone::Hand, P0);
            engine::game::zones::add_to_zone(state, id, Zone::Library, P0);
            state.objects.get_mut(&id).expect("library card").zone = Zone::Library;
        }
    }

    let card_id = runner.state().objects[&rat_a].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: rat_a,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast first Rat Colony");
    runner.act(GameAction::PassPriority).expect("p0 pass");
    runner.act(GameAction::PassPriority).expect("p1 pass");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::CastOffer {
            kind: CastOfferKind::Ripple { hit_card, .. },
            ..
        } if hit_card == rat_b
    ));
    runner
        .act(GameAction::RippleChoice {
            choice: CastChoice::Cast,
        })
        .expect("accept B");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::CastOffer {
            kind: CastOfferKind::Ripple { hit_card, .. },
            ..
        } if hit_card == rat_c
    ));

    runner
        .act(GameAction::RippleChoice {
            choice: CastChoice::Cast,
        })
        .expect("accept terminal C until bottom replacement choice");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    assert!(
        matches!(
            runner
                .state()
                .pending_batch_deliveries
                .as_ref()
                .and_then(|batch| batch.completion.as_ref()),
            Some(engine::types::game_state::BatchCompletion::RippleTerminalComplete {
                player: P0,
                source_id: _,
                final_cast: Some(id),
            }) if *id == rat_c
        ),
        "the terminal batch completion must survive the replacement prompt"
    );
    assert!(
        matches!(
            runner.state().pending_resolution_completion,
            Some(engine::types::game_state::PendingResolutionCompletion {
                player: P0,
                final_cast: Some(id),
                ..
            }) if id == rat_c
        ),
        "the terminal marker must survive the replacement prompt until C reaches the stack"
    );

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("resolve terminal bottom replacement");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OrderTriggers { ref triggers, .. } if triggers.len() == 2
    ));
    runner
        .act(GameAction::OrderTriggers { order: vec![0, 1] })
        .expect("order the combined B/C Ripple trigger batch");
    let retrigger_sources: Vec<_> = runner
        .state()
        .stack
        .iter()
        .filter_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility {
                source_id, ability, ..
            } if matches!(ability.effect, Effect::Ripple { .. }) => Some(*source_id),
            _ => None,
        })
        .collect();
    for id in [rat_b, rat_c] {
        assert_eq!(
            retrigger_sources
                .iter()
                .filter(|&&source| source == id)
                .count(),
            1,
            "each accepted Ripple cast must contribute exactly one trigger after the pause"
        );
        let spell_index = runner
            .state()
            .stack
            .iter()
            .position(|entry| entry.id == id)
            .expect("accepted Rat remains on the stack");
        let trigger_index = runner
            .state()
            .stack
            .iter()
            .position(|entry| {
                matches!(
                    &entry.kind,
                    StackEntryKind::TriggeredAbility { source_id, ability, .. }
                        if *source_id == id && matches!(ability.effect, Effect::Ripple { .. })
                )
            })
            .expect("accepted Rat's Ripple trigger is on the stack");
        assert!(
            trigger_index > spell_index,
            "trigger must be above its spell"
        );
    }
    assert!(runner.state().deferred_triggers.is_empty());
    assert!(runner.state().pending_resolution_completion.is_none());
    assert!(runner.state().pending_batch_deliveries.is_none());
}

/// CR 603.2 + CR 603.3b + CR 608.2g + CR 616.1: terminal Ripple settlement
/// must not re-collect the resumed final cast merely because the earlier,
/// parked observer is a different source. Aetherflux Reservoir is a distinct
/// battlefield source, so its exactly-once B/C triggers prove the synthetic
/// terminal SpellCast collector keys on the event, not `source_id == final_cast`.
#[test]
fn terminal_ripple_replacement_resume_collects_distinct_spell_cast_observer() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    red_pool(&mut scenario, 2);
    let observer = scenario
        .add_creature_from_oracle(
            P0,
            "Aetherflux Reservoir",
            0,
            0,
            AETHERFLUX_RESERVOIR_ORACLE,
        )
        .as_enchantment()
        .id();
    let rat_a = scenario
        .add_creature_to_hand_from_oracle(P0, "Rat Colony", 2, 1, RAT_COLONY_ORACLE)
        .with_mana_cost(ManaCost::generic(2))
        .with_keyword(engine::types::keywords::Keyword::Ripple(4))
        .id();
    let rat_c = scenario
        .add_creature_to_hand_from_oracle(P0, "Rat Colony", 2, 1, RAT_COLONY_ORACLE)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let miss = scenario
        .add_spell_to_hand_from_oracle(P0, "Terminal Observer Miss", true, "Draw a card.")
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let rat_b = scenario
        .add_creature_to_hand_from_oracle(P0, "Rat Colony", 2, 1, RAT_COLONY_ORACLE)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    for (name, destination) in [
        ("Observer Library Redirect to Hand", Zone::Hand),
        ("Observer Library Redirect to Graveyard", Zone::Graveyard),
    ] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_library_move_to(destination));
    }

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        for id in [rat_b, miss, rat_c] {
            engine::game::zones::remove_from_zone(state, id, Zone::Hand, P0);
            engine::game::zones::add_to_zone(state, id, Zone::Library, P0);
            state.objects.get_mut(&id).expect("library card").zone = Zone::Library;
        }
    }
    assert!(
        runner.state().objects[&observer]
            .trigger_definitions
            .as_slice()
            .iter()
            .any(|trigger| matches!(
                trigger.definition.mode,
                engine::types::triggers::TriggerMode::SpellCast
            )),
        "exact Aetherflux Reservoir Oracle text must parse to its SpellCast observer"
    );
    assert_ne!(
        observer, rat_c,
        "the observer must be a distinct battlefield object from the terminal Rat"
    );

    let card_id = runner.state().objects[&rat_a].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: rat_a,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast first Rat Colony");

    // Put Aetherflux's initial-cast trigger below A's Ripple so it remains a
    // live, distinct observer while Ripple performs the terminal free cast.
    let initial_order = match &runner.state().waiting_for {
        WaitingFor::OrderTriggers { triggers, .. } => {
            let observer_index = triggers
                .iter()
                .position(|trigger| trigger.source_id == observer)
                .expect("Aetherflux observes the initial Rat cast");
            let ripple_index = triggers
                .iter()
                .position(|trigger| trigger.source_id == rat_a)
                .expect("Rat A's granted Ripple observes its own cast");
            vec![observer_index, ripple_index]
        }
        waiting_for => panic!("expected initial observer/Ripple ordering, got {waiting_for:?}"),
    };
    runner
        .act(GameAction::OrderTriggers {
            order: initial_order,
        })
        .expect("place initial observer below Ripple");
    runner.act(GameAction::PassPriority).expect("p0 pass");
    runner.act(GameAction::PassPriority).expect("p1 pass");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::CastOffer {
            kind: CastOfferKind::Ripple { hit_card, .. },
            ..
        } if hit_card == rat_b
    ));

    runner
        .act(GameAction::RippleChoice {
            choice: CastChoice::Cast,
        })
        .expect("accept B while A's Ripple still offers C");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::CastOffer {
            kind: CastOfferKind::Ripple { hit_card, .. },
            ..
        } if hit_card == rat_c
    ));
    assert_eq!(
        runner
            .state()
            .deferred_triggers
            .iter()
            .filter(|context| context.pending.source_id == observer)
            .count(),
        1,
        "B's observer trigger must park while A's Ripple keeps offering cards"
    );

    runner
        .act(GameAction::RippleChoice {
            choice: CastChoice::Cast,
        })
        .expect("accept terminal C until bottom replacement choice");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("resolve the terminal bottom replacement");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));

    let first_accepted_rat_index = runner
        .state()
        .stack
        .iter()
        .position(|entry| entry.id == rat_b)
        .expect("B remains on the stack");
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .skip(first_accepted_rat_index + 1)
            .filter(|entry| entry.source_id == observer)
            .count(),
        2,
        "Aetherflux contributes exactly one observer trigger for each accepted B/C cast"
    );
}

/// CR 113.2c + CR 702.60b: two independent Thrumming Stones grant two Ripple
/// instances, each of which must become a distinct trigger when a Rat is cast.
#[test]
fn two_thrumming_stones_preserve_two_ripple_instances() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    red_pool(&mut scenario, 2);
    scenario.add_creature_from_oracle(P0, "Thrumming Stone", 1, 1, THRUMMING_STONE_ORACLE);
    scenario.add_creature_from_oracle(P0, "Thrumming Stone", 1, 1, THRUMMING_STONE_ORACLE);
    let rat = scenario
        .add_creature_to_hand_from_oracle(P0, "Rat Colony", 2, 1, RAT_COLONY_ORACLE)
        .with_mana_cost(ManaCost::generic(2))
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&rat].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: rat,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Rat Colony");
    let ripple_triggers = runner
        .state()
        .stack
        .iter()
        .filter(|entry| {
            matches!(
                &entry.kind,
                StackEntryKind::TriggeredAbility { source_id, ability, .. }
                    if *source_id == rat && matches!(ability.effect, Effect::Ripple { count: 4 })
            )
        })
        .count();
    assert_eq!(
        ripple_triggers, 2,
        "two Stones must create two Ripple triggers"
    );
}

/// CR 608.2g + CR 702.85a: accepting a cascade offer casts the hit DURING
/// resolution — the hit lands on the stack (not in Exile with a lingering
/// permission), and priority stays with the active player.
#[test]
fn cascade_accept_casts_hit_during_resolution() {
    // {2}{R} = MV 3 source.
    let cascade_cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 2,
    };
    // {R} = MV 1 nonland hit (< source MV 3).
    let hit_cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 0,
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    red_pool(&mut scenario, 3);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Cascade Sorcery", false, "Cascade")
        .with_mana_cost(cascade_cost)
        .id();
    let hit = scenario
        .add_spell_to_library_top(P0, "Cheap Hit", true)
        .with_mana_cost(hit_cost)
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast");
    runner.act(GameAction::PassPriority).expect("p0 pass");
    runner.act(GameAction::PassPriority).expect("p1 pass");

    // The cascade offer is pending.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CastOffer {
                kind: CastOfferKind::Cascade { hit_card, .. },
                ..
            } if hit_card == hit
        ),
        "expected a pending cascade CastOffer; got {:?}",
        runner.state().waiting_for
    );

    let result = runner
        .act(GameAction::CascadeChoice {
            choice: CastChoice::Cast,
        })
        .expect("accept cascade offer");

    // CR 608.2g: the hit is cast DURING resolution — it is on the stack, NOT in
    // Exile with a lingering permission.
    assert_eq!(
        runner.state().objects[&hit].zone,
        Zone::Stack,
        "accepting cascade must put the hit on the stack during resolution; \
         zone = {:?}",
        runner.state().objects[&hit].zone
    );
    assert!(
        !has_exile_alt_cost(runner.state(), hit),
        "the hit must not retain a lingering ExileWithAltCost permission"
    );

    // CR 117.3b: the active player retains priority with the hit on the stack;
    // the opponent does NOT get priority here.
    assert_eq!(
        result.waiting_for,
        WaitingFor::Priority { player: P0 },
        "active player must retain priority after the cast"
    );
    assert_ne!(
        result.waiting_for,
        WaitingFor::Priority { player: P1 },
        "opponent must not receive priority right after the cast"
    );
    assert_eq!(runner.state().priority_player, P0);
}

/// REPRO (Sleight of Hand no-op): the hit has a real SPELL ability_def (not a
/// keyword), so it exercises the full `continue_with_prepared` path rather than
/// the ability-less fast path every other test here uses. This is the path the
/// in-game Sleight of Hand cascade hit takes.
#[test]
fn cascade_hit_with_spell_ability_casts_during_resolution() {
    let cascade_cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 2,
    };
    let hit_cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 0,
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    red_pool(&mut scenario, 3);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Tumbling Spell", false, "Cascade")
        .with_mana_cost(cascade_cost)
        .id();
    // Hit is a SORCERY (is_instant: false) with Sleight of Hand's resolution-
    // CHOICE effect — the in-game card that no-op'd on accept. A sorcery hit is
    // SORCERY-SPEED, so casting it mid-resolution (stack non-empty) hits the
    // `check_spell_timing` "stack is empty" gate UNLESS CR 608.2g is honored.
    // Every other test here used `is_instant: true`, which has no timing gate —
    // the fixture path-divergence that let this ship.
    let hit = scenario
        .add_spell_to_library_top(P0, "Cantrip Hit", false)
        .with_mana_cost(hit_cost)
        .from_oracle_text(
            "Look at the top two cards of your library. Put one of them into \
             your hand and the other on the bottom of your library.",
        )
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast");
    runner.act(GameAction::PassPriority).expect("p0 pass");
    runner.act(GameAction::PassPriority).expect("p1 pass");

    let result = runner
        .act(GameAction::CascadeChoice {
            choice: CastChoice::Cast,
        })
        .expect("accept cascade offer for a hit with a spell ability");

    // The hit (a real spell with a resolution-choice ability_def) must land on
    // the stack via the full `continue_with_prepared` path — not the ability-less
    // `continue_with_no_ability` shortcut the sibling tests happen to exercise.
    assert_eq!(
        runner.state().objects[&hit].zone,
        Zone::Stack,
        "a hit WITH a spell ability must also be cast onto the stack; \
         zone = {:?}, waiting_for = {:?}",
        runner.state().objects[&hit].zone,
        result.waiting_for,
    );
    // CR 117.3b: the active player retains priority with the hit on the stack —
    // NOT a leftover sub-prompt the frontend can't render (would read as a no-op).
    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { player } if player == P0),
        "post-accept must be Priority for the active player, got {:?}",
        result.waiting_for
    );
}

/// V1 REGRESSION GUARD (CR 608.2g): the hit's OWN cast-triggered abilities must
/// fire when it is cast during resolution. Here the hit ITSELF has Cascade — its
/// inner cascade must exile a card and present an inner offer, proving
/// `run_post_action_pipeline` ran and cast-triggers were not dropped (the
/// regression PR #1728 guards against bypassing the pipeline).
#[test]
fn cascade_hit_cast_triggers_fire_during_resolution() {
    // Outer source {3}{R} = MV 4.
    let outer_cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 3,
    };
    // Hit {1}{R} = MV 2 (< 4) and ITSELF has Cascade.
    let hit_cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 1,
    };
    // Inner cascade hit {R} = MV 1 (< 2).
    let inner_cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 0,
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    red_pool(&mut scenario, 4);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Outer Cascade", false, "Cascade")
        .with_mana_cost(outer_cost)
        .id();
    // Library order: `add_spell_to_library_top` inserts at the front, so the
    // last card added is on top. We want top-to-bottom: [hit (MV2, Cascade),
    // inner (MV1)]. Add `inner` first, then `hit`, so `hit` is exiled first by
    // the OUTER cascade and its OWN cascade then digs to `inner`.
    let inner = scenario
        .add_spell_to_library_top(P0, "Inner Hit", true)
        .with_mana_cost(inner_cost)
        .id();
    let hit = scenario
        .add_spell_to_library_top(P0, "Cascading Hit", true)
        .with_mana_cost(hit_cost)
        .from_oracle_text("Cascade")
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast outer");
    runner.act(GameAction::PassPriority).expect("p0 pass");
    runner.act(GameAction::PassPriority).expect("p1 pass");

    // Outer cascade offers `hit`.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CastOffer {
                kind: CastOfferKind::Cascade { hit_card, .. },
                ..
            } if hit_card == hit
        ),
        "expected outer cascade offer for the hit; got {:?}",
        runner.state().waiting_for
    );

    // Accept — the hit is cast during resolution; its own Cascade cast-trigger
    // must FIRE (go on the stack) as `run_post_action_pipeline` processes the
    // SpellCast event. The trigger lands above the still-resolving outer cascade
    // and does not resolve until priority is passed, so we assert it is present
    // on the stack rather than already resolved.
    runner
        .act(GameAction::CascadeChoice {
            choice: CastChoice::Cast,
        })
        .expect("accept outer cascade");

    // CR 608.2g + CR 702.85c: the hit's Cascade cast-trigger fired and is on the
    // stack as a triggered ability sourced from the hit. If the pipeline had been
    // bypassed (the regression PR #1728 guards against this), this trigger would
    // never have been collected.
    let cascade_trigger_on_stack = runner.state().stack.iter().any(|entry| {
        matches!(
            &entry.kind,
            StackEntryKind::TriggeredAbility { source_id, ability, .. }
                if *source_id == hit && ability.effect == Effect::Cascade
        )
    });

    // Stronger end-to-end guard: pass priority so the inner cascade trigger
    // resolves. It must dig and exile `inner` (the inner cascade hit) and/or
    // present an inner cascade offer — conclusive evidence the cast-trigger not
    // only fired but ran its effect.
    runner.act(GameAction::PassPriority).expect("p0 pass");
    runner.act(GameAction::PassPriority).expect("p1 pass");
    let inner_exiled = runner.state().objects[&inner].zone == Zone::Exile;
    let inner_offer_pending = matches!(
        runner.state().waiting_for,
        WaitingFor::CastOffer {
            kind: CastOfferKind::Cascade { hit_card, .. },
            ..
        } if hit_card == inner
    );

    assert!(
        cascade_trigger_on_stack || inner_exiled || inner_offer_pending,
        "the hit's own Cascade cast-trigger must fire during resolution \
         (cascade_trigger_on_stack={cascade_trigger_on_stack}, \
         inner_exiled={inner_exiled}, inner_offer_pending={inner_offer_pending}); \
         waiting_for={:?}",
        runner.state().waiting_for
    );
}

/// CR 702.85a: caster declines the cascade offer — the hit and all misses go to
/// the bottom of the library together.
#[test]
fn cascade_decline_bottoms_hit_and_misses() {
    let cascade_cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 2,
    };
    let hit_cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 0,
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    red_pool(&mut scenario, 3);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Cascade Sorcery", false, "Cascade")
        .with_mana_cost(cascade_cost)
        .id();
    // Hit added first (ends up below), then a high-MV nonland miss on top so it
    // is exiled (and missed) before the cascade reaches the hit.
    let hit = scenario
        .add_spell_to_library_top(P0, "Cheap Hit", true)
        .with_mana_cost(hit_cost)
        .id();
    let miss = high_mv_miss(&mut scenario, "High MV Miss");

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast");
    runner.act(GameAction::PassPriority).expect("p0 pass");
    runner.act(GameAction::PassPriority).expect("p1 pass");

    runner
        .act(GameAction::CascadeChoice {
            choice: CastChoice::Decline,
        })
        .expect("decline cascade");

    assert_eq!(
        runner.state().objects[&hit].zone,
        Zone::Library,
        "declined cascade hit must go to the library bottom"
    );
    assert_eq!(
        runner.state().objects[&miss].zone,
        Zone::Library,
        "cascade misses must go to the library bottom"
    );
}

/// CR 701.57a: discover decline — the hit goes to the discovering player's HAND
/// (not the library), and the misses go to the library bottom.
#[test]
fn discover_decline_sends_hit_to_hand() {
    let discover_cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Blue],
        generic: 2,
    };
    let hit_cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Blue],
        generic: 0,
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
        ],
    );

    let spell = scenario
        // The card NAME must not contain "Discover": self-ref normalization
        // rewrites the card's own name tokens to `~` BEFORE effect parsing, so a
        // "Discover …"-named card turns its "Discover 3." line into "~ 3." and
        // parses to `Effect::Unimplemented`. Real discover cards (e.g. Daring
        // Discovery) are unaffected because their name yields no "Discover" token.
        .add_spell_to_hand_from_oracle(P0, "Cavern Ritual", false, "Discover 3.")
        .with_mana_cost(discover_cost)
        .id();
    // Library top-to-bottom: [high-MV miss, hit (MV1 <= N=3)].
    let hit = scenario
        .add_spell_to_library_top(P0, "Discovered Hit", true)
        .with_mana_cost(hit_cost)
        .id();
    let miss = high_mv_miss(&mut scenario, "High MV Miss");

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast discover");
    runner.act(GameAction::PassPriority).expect("p0 pass");
    runner.act(GameAction::PassPriority).expect("p1 pass");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CastOffer {
                kind: CastOfferKind::Discover { hit_card, .. },
                ..
            } if hit_card == hit
        ),
        "expected a discover CastOffer; got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::DiscoverChoice {
            choice: CastChoice::Decline,
        })
        .expect("decline discover");

    assert_eq!(
        runner.state().objects[&hit].zone,
        Zone::Hand,
        "declined discover hit must go to the discovering player's hand (CR 701.57a)"
    );
    assert_eq!(
        runner.state().objects[&miss].zone,
        Zone::Library,
        "discover misses must go to the library bottom"
    );
}

/// CR 701.57a: discover accept where the hit's MV is EXACTLY N (3) — the gate
/// is `<= N` (LE), so a hit with MV == N must be cast onto the stack. This
/// proves the discover gate uses LE, not the cascade `< source` (LT) bound.
#[test]
fn discover_accept_hit_mv_equals_n_casts_to_stack() {
    let discover_cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Blue],
        generic: 2,
    };
    // Hit MV == N == 3 ({2}{U}). Cast for free (cost zeroed), so the empty pool
    // after the discover spell is fine.
    let hit_cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Blue],
        generic: 2,
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
        ],
    );

    let spell = scenario
        // The card NAME must not contain "Discover": self-ref normalization
        // rewrites the card's own name tokens to `~` BEFORE effect parsing, so a
        // "Discover …"-named card turns its "Discover 3." line into "~ 3." and
        // parses to `Effect::Unimplemented`. Real discover cards (e.g. Daring
        // Discovery) are unaffected because their name yields no "Discover" token.
        .add_spell_to_hand_from_oracle(P0, "Cavern Ritual", false, "Discover 3.")
        .with_mana_cost(discover_cost)
        .id();
    let hit = scenario
        .add_spell_to_library_top(P0, "MV3 Hit", true)
        .with_mana_cost(hit_cost)
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast discover");
    runner.act(GameAction::PassPriority).expect("p0 pass");
    runner.act(GameAction::PassPriority).expect("p1 pass");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CastOffer {
                kind: CastOfferKind::Discover { hit_card, .. },
                ..
            } if hit_card == hit
        ),
        "expected a discover CastOffer; got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::DiscoverChoice {
            choice: CastChoice::Cast,
        })
        .expect("accept discover");

    assert_eq!(
        runner.state().objects[&hit].zone,
        Zone::Stack,
        "discover hit with MV == N (LE gate) must be cast onto the stack; \
         zone = {:?}",
        runner.state().objects[&hit].zone
    );
    assert!(
        !has_exile_alt_cost(runner.state(), hit),
        "the cast hit must not retain a lingering ExileWithAltCost permission"
    );
}

// NOTE — pipeline X-rejection cases intentionally omitted. The brief's cases 3
// and 6a (an `{X}` cascade/discover hit whose chosen X pushes the resulting MV
// past the gate → rejection) are NOT reachable through the cast-during-resolution
// pipeline. Casting a hit "without paying its mana cost" zeroes the cost in
// `prepare_spell_cast_with_variant_override`, so the `{X}` shard is gone before
// `cost_has_x` is consulted — X is never prompted and is forced to 0
// (CR 107.3b: X is 0 unless an effect sets it). A free-cast hit's resulting MV
// therefore equals its printed MV, which already satisfied the gate at exile
// time, so the resulting-MV rejection can never fire on this path. The
// `evaluate_cascade_constraint_with_resulting_mv` rejection + chosen-X logic is
// still correct and is exercised at the `finalize_cast_with_phyrexian_choices`
// boundary by the `casting_costs.rs` unit tests (which set `chosen_x` directly
// for the standing Maralen/Beseech permission path).

/// CR 608.2g leak closure: accepting a cast-during-resolution offer and letting
/// the stack resolve must leave NO exiled object retaining an
/// `ExileWithAltCost` permission.
#[test]
fn accept_then_resolve_leaves_no_lingering_permission() {
    let cascade_cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 2,
    };
    let hit_cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 0,
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    red_pool(&mut scenario, 3);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Cascade Sorcery", false, "Cascade")
        .with_mana_cost(cascade_cost)
        .id();
    let hit = scenario
        .add_spell_to_library_top(P0, "Cheap Hit", true)
        .with_mana_cost(hit_cost)
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast");
    runner.act(GameAction::PassPriority).expect("p0 pass");
    runner.act(GameAction::PassPriority).expect("p1 pass");
    runner
        .act(GameAction::CascadeChoice {
            choice: CastChoice::Cast,
        })
        .expect("accept cascade");

    runner.advance_until_stack_empty();

    let leaked: Vec<ObjectId> = runner
        .state()
        .objects
        .iter()
        .filter(|(_, obj)| {
            obj.zone == Zone::Exile
                && obj
                    .casting_permissions
                    .iter()
                    .any(|p| matches!(p, CastingPermission::ExileWithAltCost { .. }))
        })
        .map(|(id, _)| *id)
        .collect();
    assert!(
        leaked.is_empty(),
        "no exiled object may retain an ExileWithAltCost permission after the \
         stack resolves; leaked = {leaked:?}"
    );
    let _ = hit;
}
