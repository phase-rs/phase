//! Breeches, the Blastmaker — "Whenever you cast your second spell each turn,
//! you may sacrifice an artifact. If you do, flip a coin. When you win the flip,
//! copy that spell. You may choose new targets for the copy. When you lose the
//! flip, Breeches deals damage equal to that spell's mana value to any target."
//!
//! CR 603.12: the "When you win/lose the flip, [effect]" templating is a
//! *reflexive triggered ability*, NOT Krark's inline "If you win/lose the flip"
//! wording (CR 705). A reflexive trigger follows delayed-triggered-ability rules
//! (CR 603.3 / CR 603.7): its branch effect is put on the stack and resolves with
//! its own priority window, NOT inline during the flip's own resolution.
//!
//! The parser lowers each "When you win/lose the flip" clause to an
//! `Effect::CreateDelayedTrigger` whose embedded `TriggerMode::FlippedCoin`
//! trigger (filtered by `coin_flip_result`) fires on the `CoinFlipped` event
//! emitted earlier in the flip's resolution. The existing `check_delayed_triggers`
//! path then puts that branch on the stack as its own object. These tests drive
//! the genuine resolution pipeline and assert the on-stack ordering, not just the
//! parsed AST shape.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::engine::apply_as_current;
use engine::game::triggers::{push_pending_trigger_to_stack, PendingTrigger};
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, CoinFlipResult, DelayedTriggerCondition, Effect, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastingVariant, GameState, StackEntry, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaCost;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

const BREECHES: &str = "Menace\nWhenever you cast your second spell each turn, you may sacrifice an artifact. If you do, flip a coin. When you win the flip, copy that spell. You may choose new targets for the copy. When you lose the flip, Breeches deals damage equal to that spell's mana value to any target.";

fn parse_breeches() -> engine::parser::oracle::ParsedAbilities {
    parse_oracle_text(
        BREECHES,
        "Breeches, the Blastmaker",
        &["Menace".to_string()],
        &["Legendary".to_string(), "Creature".to_string()],
        &[],
    )
}

/// Walk every `Effect` reachable from a definition's effect + sub_ability +
/// else chain + FlipCoin branch chain, invoking `f` on each.
fn walk_effects(def: &AbilityDefinition, f: &mut impl FnMut(&Effect)) {
    f(def.effect.as_ref());
    if let Some(sub) = def.sub_ability.as_deref() {
        walk_effects(sub, f);
    }
    if let Some(els) = def.else_ability.as_deref() {
        walk_effects(els, f);
    }
    if let Effect::CreateDelayedTrigger { effect, .. } = def.effect.as_ref() {
        walk_effects(effect, f);
    }
    if let Effect::FlipCoin {
        win_effect,
        lose_effect,
        ..
    }
    | Effect::FlipCoins {
        win_effect,
        lose_effect,
        ..
    } = def.effect.as_ref()
    {
        for branch in [win_effect.as_deref(), lose_effect.as_deref()]
            .into_iter()
            .flatten()
        {
            walk_effects(branch, f);
        }
    }
}

/// CR 603.12 + CR 705.2: Breeches parses with zero residual `Effect::Unimplemented`
/// — both reflexive "When you win/lose the flip" clauses lower to real
/// `CreateDelayedTrigger`s carrying a `FlippedCoin` trigger filtered by
/// `coin_flip_result`, with the win branch's copy carrying the
/// `MayChooseNewTargets` rider patched in from "You may choose new targets for
/// the copy."
///
/// Revert-discriminating: the prior (rejected) fail-closed implementation emitted
/// `Effect::unimplemented("reflexive_coin_flip_trigger", ..)` for each clause and
/// `Effect::unimplemented("orphaned_copy_retarget", ..)` for the retarget. This
/// test's `unimplemented == 0` assertion and the structural delayed-trigger
/// assertions both fail if that fail-closed path is restored. The inline fold
/// (folding "when" into `FlipCoin.win_effect`) is rejected by
/// `flip_branches_stay_empty`.
#[test]
fn breeches_reflexive_flip_clauses_become_delayed_triggers() {
    let parsed = parse_breeches();

    assert_eq!(
        serde_json::to_string(&parsed)
            .unwrap()
            .matches("Unimplemented")
            .count(),
        0,
        "Breeches must parse with zero residual Unimplemented gaps"
    );

    let trigger = parsed
        .triggers
        .first()
        .expect("Breeches should parse a second-spell trigger");
    let execute = trigger.execute.as_ref().expect("trigger execute");

    let mut won_copy = 0usize;
    let mut lost_damage = 0usize;
    let mut copy_retargets = false;
    let mut flip_branch_populated = false;

    walk_effects(execute, &mut |effect| match effect {
        Effect::CreateDelayedTrigger {
            condition:
                DelayedTriggerCondition::WhenNextEvent {
                    trigger: t,
                    or_trigger: None,
                },
            effect: inner,
            ..
        } if t.mode == TriggerMode::FlippedCoin => match t.coin_flip_result {
            Some(CoinFlipResult::Won) => {
                won_copy += 1;
                assert!(
                    matches!(inner.effect.as_ref(), Effect::CopySpell { .. }),
                    "the won-flip delayed trigger must copy the spell, got {:?}",
                    inner.effect
                );
                if let Effect::CopySpell { retarget, .. } = inner.effect.as_ref() {
                    copy_retargets = matches!(
                        retarget,
                        engine::types::ability::CopyRetargetPermission::MayChooseNewTargets
                    );
                }
                // CR 705.2: "when YOU win the flip" — scoped to the controller.
                assert_eq!(t.valid_target, Some(TargetFilter::Controller));
            }
            Some(CoinFlipResult::Lost) => {
                lost_damage += 1;
                assert!(
                    matches!(inner.effect.as_ref(), Effect::DealDamage { .. }),
                    "the lost-flip delayed trigger must deal damage, got {:?}",
                    inner.effect
                );
                assert_eq!(t.valid_target, Some(TargetFilter::Controller));
            }
            None => panic!("reflexive flip trigger must carry a coin_flip_result filter"),
        },
        Effect::FlipCoin {
            win_effect,
            lose_effect,
            ..
        }
        | Effect::FlipCoins {
            win_effect,
            lose_effect,
            ..
        } if win_effect.is_some() || lose_effect.is_some() => {
            flip_branch_populated = true;
        }
        _ => {}
    });

    assert_eq!(
        won_copy, 1,
        "exactly one won-flip reflexive copy delayed trigger expected"
    );
    assert_eq!(
        lost_damage, 1,
        "exactly one lost-flip reflexive damage delayed trigger expected"
    );
    assert!(
        copy_retargets,
        "the 'you may choose new targets for the copy' rider must patch the \
         delayed trigger's CopySpell to MayChooseNewTargets"
    );
    // CR 603.12: the reflexive 'when' clauses must NOT fold into the inline
    // FlipCoin win/lose branches (that is the wrong CR 705 ordering).
    assert!(
        !flip_branch_populated,
        "reflexive 'when' clauses must stay delayed triggers, not inline FlipCoin branches"
    );
}

/// CR 705: the inline Krark wording ("If you win/lose the flip, ...") is
/// unchanged — it still folds into the preceding `FlipCoin` because it is part of
/// the same one-shot resolution (CR 705), not a reflexive trigger. Guards against
/// the reflexive-flip change accidentally swallowing the inline form.
#[test]
fn krark_inline_flip_branches_still_fold() {
    const KRARK: &str = "Whenever you cast an instant or sorcery spell, flip a coin. \
        If you lose the flip, return that spell to its owner's hand. \
        If you win the flip, copy that spell, and you may choose new targets for the copy.";

    let parsed = parse_oracle_text(
        KRARK,
        "Krark, the Thumbless",
        &[],
        &["Legendary".to_string(), "Creature".to_string()],
        &[],
    );
    assert_eq!(
        serde_json::to_string(&parsed)
            .unwrap()
            .matches("Unimplemented")
            .count(),
        0,
        "Krark's inline if-you-win/lose wording must still parse with zero gaps"
    );

    let trigger = parsed.triggers.first().expect("Krark trigger");
    let execute = trigger.execute.as_ref().expect("Krark trigger execute");
    let Effect::FlipCoin {
        win_effect,
        lose_effect,
        ..
    } = execute.effect.as_ref()
    else {
        panic!("expected FlipCoin execute, got {:?}", execute.effect);
    };
    assert!(
        win_effect.is_some() && lose_effect.is_some(),
        "Krark's inline branches must populate both FlipCoin slots"
    );
}

// --- Runtime: on-stack reflexive ordering (CR 603.3 priority window) ----------

/// Build a battlefield Breeches + a spell on the stack to copy, with the
/// SpellCast trigger event in context, then push Breeches' parsed reflexive
/// trigger `execute` onto the stack as a triggered ability. Returns
/// `(state, spell_id, breeches_id)`. The flip's own coin RNG is seeded via
/// `seed`; seed 0 wins, seed 1 loses (mirrors flip_coin.rs's seed convention).
fn setup_breeches_runtime(seed: u64) -> (GameState, ObjectId, ObjectId) {
    let mut state = GameState::new_two_player(seed);
    state.rng = ChaCha20Rng::seed_from_u64(seed);
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    let breeches = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Breeches, the Blastmaker".to_string(),
        Zone::Battlefield,
    );

    // An artifact P0 can sacrifice (the optional additional cost).
    let artifact = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Treasure".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&artifact)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Artifact);

    // The "second spell" on the stack to copy — a 3-mana instant so its mana
    // value (3) is observable in the lose branch's damage.
    let spell = create_object(
        &mut state,
        CardId(3),
        PlayerId(0),
        "Big Spell".to_string(),
        Zone::Stack,
    );
    {
        let obj = state.objects.get_mut(&spell).unwrap();
        obj.card_types.core_types.push(CoreType::Instant);
        obj.mana_cost = ManaCost::generic(3);
    }
    state.stack.push_back(StackEntry {
        id: spell,
        source_id: spell,
        controller: PlayerId(0),
        kind: StackEntryKind::Spell {
            card_id: CardId(3),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 3,
        },
    });
    state.current_trigger_event = Some(GameEvent::SpellCast {
        controller: PlayerId(0),
        object_id: spell,
        card_id: CardId(3),
    });

    let parsed = parse_breeches();
    let execute = parsed.triggers[0]
        .execute
        .as_ref()
        .expect("Breeches trigger execute");
    let ability = build_resolved_from_def(execute, breeches, PlayerId(0));

    let trigger_event = state.current_trigger_event.clone();
    let mut events = Vec::new();
    push_pending_trigger_to_stack(
        &mut state,
        PendingTrigger {
            source_id: breeches,
            controller: PlayerId(0),
            condition: None,
            ability,
            timestamp: 0,
            target_constraints: Vec::new(),
            distribute: None,
            trigger_event,
            modal: None,
            mode_abilities: vec![],
            description: None,
            may_trigger_origin: None,
            subject_match_count: None,
            die_result: None,
        },
        &mut events,
    );

    (state, spell, breeches)
}

/// Drive the engine until it settles back to `Priority` or the stack empties or
/// it pauses on a non-Priority `WaitingFor`. Returns all collected events.
fn pass_priority_until_settle(state: &mut GameState) -> Vec<GameEvent> {
    let mut all = Vec::new();
    for _ in 0..40 {
        match &state.waiting_for {
            WaitingFor::Priority { .. } => {
                if state.stack.is_empty() {
                    return all;
                }
                let result = apply_as_current(state, GameAction::PassPriority)
                    .expect("pass priority should succeed");
                all.extend(result.events);
            }
            _ => return all,
        }
    }
    panic!(
        "stack did not settle: waiting_for = {:?}",
        state.waiting_for
    );
}

/// Count the `TriggeredAbility` entries currently on the stack.
fn triggered_ability_entries(state: &GameState) -> usize {
    state
        .stack
        .iter()
        .filter(|e| matches!(e.kind, StackEntryKind::TriggeredAbility { .. }))
        .count()
}

/// Pass priority until the Breeches trigger begins resolving and pauses on its
/// optional "you may sacrifice an artifact" prompt.
fn pass_priority_until_optional(state: &mut GameState) -> Vec<GameEvent> {
    let mut all = Vec::new();
    for _ in 0..10 {
        match &state.waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => return all,
            WaitingFor::Priority { .. } => {
                let result = apply_as_current(state, GameAction::PassPriority)
                    .expect("pass priority to resolve trigger");
                all.extend(result.events);
            }
            other => panic!("unexpected waiting_for before optional prompt: {other:?}"),
        }
    }
    panic!(
        "Breeches trigger never reached its optional sacrifice prompt: {:?}",
        state.waiting_for
    );
}

/// CR 603.12 + CR 603.3: On a won flip, the reflexive "copy that spell" must go
/// on the stack as its OWN triggered-ability object AFTER the flip's resolution
/// (a priority window), not inline during the flip. Discriminating: with the
/// inline fold the copy would be created during the flip's resolution and NO new
/// triggered-ability stack entry would appear — this test asserts that a fresh
/// reflexive trigger entry is on the stack while the spell is still uncopied,
/// then that resolving it produces the copy.
#[test]
fn breeches_won_flip_copy_goes_on_stack_as_separate_object() {
    // Seed 0 wins the flip.
    let (mut state, spell, _breeches) = setup_breeches_runtime(0);

    // Resolve the trigger up to its "you may sacrifice an artifact" prompt.
    pass_priority_until_optional(&mut state);

    // Accept the optional sacrifice; this resolves the Sacrifice (artifact),
    // then the FlipCoin (emitting CoinFlipped), then both CreateDelayedTriggers
    // register their reflexive flip triggers — all inside the trigger's
    // resolution. EffectResolved for the flip happens here, before any reflexive
    // branch.
    let accept = apply_as_current(
        &mut state,
        GameAction::DecideOptionalEffect { accept: true },
    )
    .expect("accept optional sacrifice");

    // The coin came up heads.
    let won = accept
        .events
        .iter()
        .find_map(|e| match e {
            GameEvent::CoinFlipped { won, .. } => Some(*won),
            _ => None,
        })
        .expect("a coin was flipped during the trigger resolution");
    assert!(won, "seed 0 must win the flip");

    // CR 603.12 + CR 603.3: the won-flip reflexive ("copy that spell") must now
    // be a SEPARATE triggered-ability object on the stack. The original Breeches
    // trigger has finished resolving; the spell has NOT been copied yet (no copy
    // on the stack), because the copy waits for the reflexive trigger to resolve
    // through its own priority window.
    let stack_spells_before = state
        .stack
        .iter()
        .filter(|e| matches!(e.kind, StackEntryKind::Spell { .. }))
        .count();
    assert_eq!(
        stack_spells_before, 1,
        "before the reflexive resolves, only the original spell is on the stack \
         (the copy must NOT have been created inline during the flip): {:?}",
        state.stack
    );
    assert_eq!(
        triggered_ability_entries(&state),
        1,
        "the won-flip reflexive copy trigger must be on the stack as its own \
         object after the flip resolved (CR 603.3 priority window): {:?}",
        state.stack
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "a priority window must be open with the reflexive trigger on the stack, got {:?}",
        state.waiting_for
    );

    // The won branch must NOT bounce the original spell — it stays on the stack
    // beneath the reflexive copy trigger and resolves normally.
    assert_eq!(
        state.objects.get(&spell).map(|o| o.zone),
        Some(Zone::Stack),
        "the original spell must remain on the stack while the reflexive resolves \
         (won branch does not bounce)"
    );

    // Resolve the reflexive copy trigger (and everything below it). It must put a
    // copy of the original spell on the stack — a `SpellCopied` event — proving
    // the won-flip "copy that spell" reflexive resolved as its own stack object,
    // copying the spell named by the snapshotted `TriggeringSource`.
    let resolution_events = pass_priority_until_settle(&mut state);
    let copied = resolution_events.iter().any(|e| {
        matches!(
            e,
            GameEvent::SpellCopied { original_id, .. } if *original_id == spell
        )
    });
    assert!(
        copied,
        "resolving the won-flip reflexive trigger must copy the original spell \
         (SpellCopied event); events = {resolution_events:?}"
    );
}

/// CR 603.12 + CR 603.3 + CR 705.2: On a lost flip, the reflexive "Breeches deals
/// damage equal to that spell's mana value to any target" must go on the stack as
/// its own triggered-ability object (a priority window) — not resolve inline.
/// Discriminating in two ways: (1) a separate triggered-ability entry must exist
/// after the flip; (2) the damage equals the snapshotted mana value (3), which
/// would be impossible if the damage resolved inline before "that spell"'s mana
/// value were snapshotted at delayed-trigger creation.
#[test]
fn breeches_lost_flip_damage_goes_on_stack_and_uses_spell_mana_value() {
    // Seed 1 loses the flip.
    let (mut state, _spell, _breeches) = setup_breeches_runtime(1);
    let p1_life_before = state.players[1].life;

    pass_priority_until_optional(&mut state);

    let accept = apply_as_current(
        &mut state,
        GameAction::DecideOptionalEffect { accept: true },
    )
    .expect("accept optional sacrifice");

    let won = accept
        .events
        .iter()
        .find_map(|e| match e {
            GameEvent::CoinFlipped { won, .. } => Some(*won),
            _ => None,
        })
        .expect("a coin was flipped");
    assert!(!won, "seed 1 must lose the flip");

    // CR 603.3: the lost-flip reflexive damage must be on the stack as its own
    // object now — NOT yet resolved (P1 has taken no damage).
    assert_eq!(
        triggered_ability_entries(&state),
        1,
        "the lost-flip reflexive damage trigger must be on the stack as its own \
         object after the flip resolved: {:?}",
        state.stack
    );
    assert_eq!(
        state.players[1].life, p1_life_before,
        "no damage may be dealt inline during the flip — it waits for the \
         reflexive trigger's own resolution"
    );

    // CR 603.3d + CR 115.1: the reflexive "deals damage … to any target" trigger
    // prompts for its target while on the stack (its own priority window). Choose
    // P1, then resolve.
    assert!(
        matches!(state.waiting_for, WaitingFor::TriggerTargetSelection { .. }),
        "the lost-flip reflexive damage trigger must prompt for its 'any target' \
         while on the stack, got {:?}",
        state.waiting_for
    );
    apply_as_current(
        &mut state,
        GameAction::ChooseTarget {
            target: Some(engine::types::ability::TargetRef::Player(PlayerId(1))),
        },
    )
    .expect("choose damage target");
    pass_priority_until_settle(&mut state);

    // CR 705.2 + CR 603.7c: damage equals the spell's mana value (3), snapshotted
    // at delayed-trigger creation. If the lose branch had resolved inline (before
    // the snapshot), or the mana value were not snapshotted, this would differ.
    assert_eq!(
        state.players[1].life,
        p1_life_before - 3,
        "lost-flip reflexive must deal damage equal to the copied spell's mana value (3)"
    );

    // CR 603.12: the lost flip resolved, so the "win the flip" reflexive trigger
    // (created in the same resolution) NEVER triggered and must have been
    // DISCARDED — not left pending to fire on a later coin flip this turn. No
    // reflexive delayed trigger may remain.
    assert!(
        state.delayed_triggers.is_empty(),
        "the non-matching 'win the flip' reflexive must be discarded after the lost \
         flip resolution (CR 603.12), not left pending: {:?}",
        state.delayed_triggers
    );
}

/// CR 603.12: A reflexive coin-flip trigger that does not trigger (the flip came
/// up the other way) must be DISCARDED, not left pending to fire on a later coin
/// flip this turn. This drives the discard path directly: register a "win the
/// flip" reflexive, emit a LOST `CoinFlipped`, then a WON `CoinFlipped`, and
/// assert the reflexive neither fired on the lost flip nor lingered to fire on
/// the later won flip.
///
/// Revert-discriminating: without the `reflexive_coin_flip_resolved_without_match`
/// discard, the one-shot `WhenNextEvent` would survive the lost flip and fire on
/// the subsequent won flip — `state.delayed_triggers` would be non-empty after
/// the lost flip and the won flip would push a spurious reflexive trigger.
#[test]
fn nonmatching_reflexive_coin_flip_trigger_is_discarded_not_left_pending() {
    use engine::game::triggers::check_delayed_triggers;
    use engine::types::game_state::DelayedTrigger;

    let mut state = GameState::new_two_player(0);
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Reflexive Source".to_string(),
        Zone::Battlefield,
    );

    // A "When you WIN the flip" reflexive delayed trigger (won → gain 5 life).
    let won_inner = AbilityDefinition::new(
        engine::types::ability::AbilityKind::Spell,
        Effect::GainLife {
            amount: engine::types::ability::QuantityExpr::Fixed { value: 5 },
            player: TargetFilter::Controller,
        },
    );
    // Build the won-flip reflexive via the same WhenNextEvent/FlippedCoin
    // condition the parser emits (asserted shape in
    // `breeches_reflexive_flip_clauses_become_delayed_triggers`).
    let mut trigger_def = engine::types::ability::TriggerDefinition::new(TriggerMode::FlippedCoin);
    trigger_def.coin_flip_result = Some(CoinFlipResult::Won);
    trigger_def.valid_target = Some(TargetFilter::Controller);
    let condition = DelayedTriggerCondition::WhenNextEvent {
        trigger: Box::new(trigger_def),
        or_trigger: None,
    };
    state.delayed_triggers.push(DelayedTrigger {
        condition,
        ability: build_resolved_from_def(&won_inner, source, PlayerId(0)),
        controller: PlayerId(0),
        source_id: source,
        one_shot: true,
    });

    let life_before = state.players[0].life;

    // A LOST flip by the controller: the "win" reflexive does NOT trigger.
    let lost_events = check_delayed_triggers(
        &mut state,
        &[GameEvent::CoinFlipped {
            player_id: PlayerId(0),
            won: false,
        }],
    );
    assert!(
        !lost_events
            .iter()
            .any(|e| matches!(e, GameEvent::StackPushed { .. })),
        "the win reflexive must not fire on a lost flip"
    );
    assert!(
        state.delayed_triggers.is_empty(),
        "CR 603.12: the non-matching win reflexive must be discarded after the \
         lost flip, not left pending: {:?}",
        state.delayed_triggers
    );

    // A subsequent WON flip later this turn must NOT resurrect the discarded
    // reflexive.
    let won_events = check_delayed_triggers(
        &mut state,
        &[GameEvent::CoinFlipped {
            player_id: PlayerId(0),
            won: true,
        }],
    );
    assert!(
        won_events.is_empty(),
        "a later won flip must not fire the discarded reflexive: {won_events:?}"
    );
    assert_eq!(
        state.players[0].life, life_before,
        "no reflexive gain-life may resolve from the discarded trigger"
    );
}
