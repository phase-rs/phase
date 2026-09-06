//! CR 115.7 — a parked retarget prompt must always be answerable.
//!
//! Three propositions, all reached through production entry points:
//!
//! 1. the retarget pool comes from the stack entry's OWN targeting authority,
//!    not from an Aura host's enchant filter (an Aura's triggered ability is a
//!    different object on the stack than the Aura spell — CR 115.1b + CR 113.7a,
//!    against the Aura SPELL's own rule, CR 303.4a);
//! 2. an empty pool resolves as a CR 115.7a no-change instead of parking a
//!    prompt nothing can discharge;
//! 3. every submission the AI proposes for a parked prompt is accepted by the
//!    reducer, because both consult the same per-slot authority
//!    (`retarget_slot_violation`).

use engine::ai_support::candidate_actions;
use engine::game::effects::change_targets;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    mana_multi_role, ControllerRef, Effect, EffectKind, ManaProduction, ManaTargetRole,
    MultiTargetSpec, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastingVariant, GameState, RetargetScope, RetargetSlotAddress, StackEntry, StackEntryKind,
    WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Scryfall, fetched verbatim — not paraphrased. The second line is the one this
/// file's headline test retargets.
const PAIN_FOR_ALL_ORACLE: &str = "Enchant creature you control\n\
     When this Aura enters, enchanted creature deals damage equal to its power to any other target.\n\
     Whenever enchanted creature is dealt damage, it deals that much damage to each opponent.";

const BOLT_BEND_ORACLE: &str =
    "This spell costs {3} less to cast if you control a creature with power 4 or greater.\n\
     Change the target of target spell or ability with a single target.";

const BLOSSOMING_DEFENSE_ORACLE: &str =
    "Target creature you control gets +2/+2 and gains hexproof until end of turn. \
     (It can't be the target of spells or abilities your opponents control.)";

const LIGHTNING_BOLT_ORACLE: &str = "Lightning Bolt deals 3 damage to any target.";

// phase-rs/phase#8355 round-8 review LOW finding: the two "Lava Axe"-named
// fixtures below previously parsed `LIGHTNING_BOLT_ORACLE` under that card
// name, contradicting this file's own "fetched verbatim, not paraphrased"
// premise — real Lava Axe reads "deals 5 damage to target PLAYER OR
// PLANESWALKER", a narrower filter than Lightning Bolt's "any target".
// Outcome-neutral for both fixtures (each declares a fixed `TargetRef::
// Player(P0)` target rather than dynamically re-evaluating this filter), but
// the premise should be true regardless of whether a given use happens to be
// insensitive to it.
const LAVA_AXE_ORACLE: &str = "Lava Axe deals 5 damage to target player or planeswalker.";

fn add_mana(runner: &mut GameRunner, player: PlayerId, mana: &[ManaType]) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

/// Pass priority until `done`, returning every event the engine emitted on the
/// way. Mirrors `issue_2938_deflecting_swat.rs`'s driver, but keeps the events
/// so a resolution can be asserted on rather than inferred.
fn pass_priority_until<F>(runner: &mut GameRunner, mut done: F) -> Vec<GameEvent>
where
    F: FnMut(&GameState) -> bool,
{
    let mut events = Vec::new();
    for _ in 0..32 {
        if done(runner.state()) {
            return events;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                let result = runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority must succeed while driving resolution");
                events.extend(result.events);
            }
            other => panic!("unexpected wait state while driving resolution: {other:?}"),
        }
    }
    panic!("priority loop exhausted before reaching the expected state");
}

fn parked_retarget_pool(runner: &GameRunner) -> Vec<TargetRef> {
    let WaitingFor::RetargetChoice {
        legal_new_targets, ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected a parked RetargetChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    legal_new_targets
}

fn retarget_candidates(state: &GameState) -> Vec<Vec<TargetRef>> {
    candidate_actions(state)
        .into_iter()
        .filter_map(|candidate| match candidate.action {
            GameAction::RetargetSpell { new_targets } => Some(new_targets),
            _ => None,
        })
        .collect()
}

/// Rows 1a + 1b — CR 115.1b + CR 303.4a: an Aura's *triggered* ability declares
/// its own target ("any other target"), so its retarget pool must come from that
/// effect's filter. CR 115.1b is the on-point rule: "An Aura permanent doesn't
/// target anything; only the spell is targeted. (An activated or triggered
/// ability of an Aura permanent can also be targeted.)" Keying the CR 303.4a Aura substitution on the source object
/// instead of on the stack entry handed back the Aura's "creature you control"
/// enchant pool, which cannot even contain the trigger's current target — so no
/// submission was legal and the prompt could never be discharged.
#[test]
fn aura_hosted_trigger_retarget_pool_uses_the_abilitys_own_filter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Power 4+ so Bolt Bend costs {R} rather than {3}{R}.
    let host = scenario
        .add_creature(P0, "Smaug the Impenetrable", 8, 7)
        .id();
    let bystander = scenario.add_creature(P0, "Goblin", 1, 1).id();
    let victim = scenario.add_creature(P1, "Bear", 2, 2).id();
    let aura = scenario
        .add_enchantment_from_oracle(P0, "Pain for All", PAIN_FOR_ALL_ORACLE)
        .id();
    let bolt_bend = scenario
        .add_spell_to_hand_from_oracle(P0, "Bolt Bend", true, BOLT_BEND_ORACLE)
        .id();

    let mut runner = scenario.build();
    add_mana(&mut runner, P0, &[ManaType::Red]);

    // Pain for All is a PRINTED Aura ("Enchant creature you control"), not a
    // bestow creature. `attach_as_bestowed_aura` is the only attach helper the
    // scenario builder exposes, and it routes through
    // `casting::apply_bestow_aura_form`, which GRANTS the broad bestow
    // `Enchant(creature)` to any object carrying no `Keyword::Enchant` — and
    // `add_enchantment_from_oracle` installs none. Seed the printed filter first
    // so the grant is skipped (it is idempotent on an existing `Enchant`) and the
    // Aura keeps the filter the card actually prints.
    //
    // BOTH fields, deliberately. `layers::seed_live_characteristics_from_base`
    // resets `obj.keywords = obj.base_keywords.clone()` at the top of every full
    // layer pass, and `attach_as_bestowed_aura` itself calls
    // `layers_dirty.mark_full()`, so a write to `keywords` alone cannot survive
    // its own attach call. A previous revision wrote only `keywords`, and only
    // AFTER attaching: the grant had already fired into both fields, so that
    // write was overwritten by the broad filter and row 1b below was VACUOUS —
    // a broad `Enchant(creature)` pool contains the victim, so its assertion
    // passed at base too. Verified by the guard below, which failed against
    // `controller: None` before this seed existed.
    {
        let printed_enchant = vec![Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You),
        ))];
        let aura_obj = runner.state_mut().objects.get_mut(&aura).unwrap();
        aura_obj.base_keywords = printed_enchant.clone();
        aura_obj.keywords = printed_enchant;
    }
    runner.attach_as_bestowed_aura(aura, host);

    // The ETB trigger, already on the stack targeting P1's creature.
    // `&["Enchant"]` is the MTGJSON keyword-name list the `card-test` convention
    // passes for an Aura; the previous `&[]` parsed only via
    // `parse_keyword_line`'s non-MTGJSON fallback, which is a different code
    // path from the one a real card takes.
    let parsed = parse_oracle_text(
        PAIN_FOR_ALL_ORACLE,
        "Pain for All",
        &["Enchant".to_string()],
        &["Enchantment".to_string()],
        &["Aura".to_string()],
    );
    let targeting_triggers: Vec<&Effect> = parsed
        .triggers
        .iter()
        .filter_map(|trigger| trigger.execute.as_ref())
        .map(|ability| ability.effect.as_ref())
        .filter(|effect| effect.target_filter().is_some())
        .collect();
    assert_eq!(
        targeting_triggers.len(),
        1,
        "fixture guard: exactly one Pain for All trigger declares a target — the \
         ETB damage trigger this row retargets"
    );
    let trigger_effect = targeting_triggers[0].clone();

    let trigger_id = ObjectId(901);
    runner.state_mut().stack.push_back(StackEntry {
        id: trigger_id,
        source_id: aura,
        controller: P0,
        kind: StackEntryKind::TriggeredAbility {
            source_id: aura,
            ability: Box::new(ResolvedAbility::new(
                trigger_effect,
                vec![TargetRef::Object(victim)],
                aura,
                P0,
            )),
            condition: None,
            trigger_event: None,
            description: None,
            source_name: String::new(),
            subject_match_count: None,
            die_result: None,
            provenance: None,
        },
    });

    runner
        .cast(bolt_bend)
        .target_objects(&[trigger_id])
        .commit();
    pass_priority_until(&mut runner, |state| {
        matches!(state.waiting_for, WaitingFor::RetargetChoice { .. })
    });

    // Structural guard: the entry under test really is a triggered ability, so
    // this row cannot silently degrade into re-testing the Aura SPELL branch.
    assert!(
        matches!(
            runner.state().stack[0].kind,
            StackEntryKind::TriggeredAbility { .. }
        ),
        "fixture guard: stack[0] must be the Aura's triggered ability"
    );

    // Premise guard — pins the LIVE enchant filter, which is row 1b's whole
    // discriminator: the narrow "creature you control" filter excludes P1's
    // creatures, so a pool derived from it cannot contain the victim, and it
    // yields no player, so it cannot contain row 1a's players either.
    //
    // ASSERTED, never assigned — the assignment happens once, above the attach,
    // where it can actually take. This guard is what caught that the previous
    // revision's post-attach write to `keywords` alone did NOT take: it failed
    // here with `controller: None`, the broad bestow grant, proving row 1b had
    // been passing against a pool that contains the victim at base too. Nothing
    // else between the fixture and the pool pins this filter, so without this
    // assertion the same regression is silent.
    assert_eq!(
        runner.state().objects[&aura].keywords,
        vec![Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You),
        ))],
        "fixture guard: the Aura's live enchant filter must be the narrow printed \
         one, or this row's pool discriminator is vacuous"
    );

    let pool = parked_retarget_pool(&runner);

    // Positive reach-guard: a P0 creature is in the pool under BOTH the old
    // enchant-filter derivation and the new effect-filter one, so the two
    // discriminating assertions below cannot pass merely by the pool being
    // populated with anything at all.
    assert!(
        pool.contains(&TargetRef::Object(bystander)),
        "reach guard: the pool must be populated, got {pool:?}"
    );

    // Row 1a — the trigger's own "any other target" filter enumerates players
    // (CR 115.4). The Aura's "creature you control" enchant filter cannot.
    assert!(
        pool.contains(&TargetRef::Player(P1)),
        "CR 115.1b: the pool must come from the TRIGGER's own target filter, which \
         reaches players; got {pool:?}"
    );

    // Row 1b — the ability's current target must stay retargetable-to. It is a
    // creature P1 controls, which the "creature you control" enchant filter
    // excludes outright.
    assert!(
        pool.contains(&TargetRef::Object(victim)),
        "CR 115.7a: the current target must remain in the pool; got {pool:?}"
    );
}

/// Row 1d — CR 115.7a: "If a target can't be changed to another legal target,
/// the original target is unchanged." An empty pool IS that case, so the effect
/// must resolve as a no-change rather than park a prompt with zero candidates,
/// which neither a human nor the AI can discharge.
#[test]
fn retarget_with_no_legal_alternative_resolves_as_no_change() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P0 controls NO creature — that is what empties "target creature you
    // control". P1 controls one, so the battlefield is not globally empty.
    scenario.add_creature(P1, "Bear", 2, 2);
    let bolt_bend = scenario
        .add_spell_to_hand_from_oracle(P0, "Bolt Bend", true, BOLT_BEND_ORACLE)
        .id();

    let mut runner = scenario.build();
    // P0 controls no power-4 creature, so Bolt Bend costs the full {3}{R}.
    add_mana(
        &mut runner,
        P0,
        &[
            ManaType::Red,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ],
    );

    // The removal, modeled: Blossoming Defense's target left the battlefield in
    // an earlier priority window and is now a P0 creature card in the graveyard.
    let doomed = create_object(
        runner.state_mut(),
        CardId(801),
        P0,
        "Doomed Bear".to_string(),
        Zone::Graveyard,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&doomed)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Creature];

    let defense_parsed = parse_oracle_text(
        BLOSSOMING_DEFENSE_ORACLE,
        "Blossoming Defense",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let defense_id = create_object(
        runner.state_mut(),
        CardId(802),
        P0,
        "Blossoming Defense".to_string(),
        Zone::Stack,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&defense_id)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Instant];
    let defense_ability = ResolvedAbility::new(
        defense_parsed.abilities[0].effect.as_ref().clone(),
        // CR 115.7a: an illegal-but-recorded current target. A stack entry with
        // NO current targets no-ops earlier in `change_targets::resolve`, which
        // would satisfy this row's assertion without the empty-pool guard ever
        // running.
        vec![TargetRef::Object(doomed)],
        defense_id,
        P0,
    );
    runner.state_mut().stack.push_back(StackEntry {
        id: defense_id,
        source_id: defense_id,
        controller: P0,
        kind: StackEntryKind::Spell {
            card_id: CardId(802),
            ability: Some(Box::new(defense_ability)),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    // Structural guard: pins WHY the pool is empty — the filter matched nothing,
    // rather than the battlefield being empty so any filter would have.
    let controls_creature = |state: &GameState, player: PlayerId| {
        state
            .objects
            .values()
            .filter(|obj| obj.zone == Zone::Battlefield && obj.controller == player)
            .any(|obj| obj.card_types.core_types.contains(&CoreType::Creature))
    };
    assert!(
        !controls_creature(runner.state(), P0),
        "fixture guard: P0 must control no creature, which is what empties the pool"
    );
    // Non-vacuity control for the guard above, NOT a production discriminator:
    // it cannot catch an engine regression, only a fixture that has degraded
    // into a globally empty battlefield, where "the pool is empty" would be
    // uninformative. Kept deliberately and labelled so it is not read as
    // coverage. A third assertion (`objects[&bystander].zone == Battlefield`)
    // stood here and was removed as strictly entailed: P1's Bear is the only P1
    // creature in this fixture, so this assertion is true exactly when that one
    // was.
    assert!(
        controls_creature(runner.state(), P1),
        "fixture guard: P1 must control a creature, so the battlefield is not \
         globally empty"
    );

    runner
        .cast(bolt_bend)
        .target_objects(&[defense_id])
        .commit();

    // Stop the moment ChangeTargets resolves — passing further would resolve
    // Blossoming Defense itself and remove the entry this row asserts on.
    let events = pass_priority_until(&mut runner, |state| {
        matches!(state.waiting_for, WaitingFor::RetargetChoice { .. })
            || !state.stack.iter().any(|entry| entry.id == bolt_bend)
    });

    // Discriminating: at base the prompt parks here.
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::RetargetChoice { .. }
        ),
        "CR 115.7a: an empty pool must not park an unanswerable prompt"
    );

    // Positive reach-guard: "no prompt parked" is a negative, so prove the
    // ChangeTargets effect actually resolved rather than fizzling en route.
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::ChangeTargets,
                ..
            }
        )),
        "reach guard: the ChangeTargets effect must have resolved, got {events:?}"
    );

    // Positive reach-guard: CR 115.7a's "the original target is unchanged".
    let defense_entry = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == defense_id)
        .expect("the retargeted entry is still on the stack");
    assert_eq!(
        defense_entry.ability().unwrap().targets,
        vec![TargetRef::Object(doomed)],
        "CR 115.7a: the original target is unchanged"
    );
}

const HALLOW_ORACLE: &str = "Prevent all damage target spell would deal this turn. \
     You gain life equal to the damage prevented this way.";

/// Scryfall, fetched verbatim — Row N22's second stack member.
const SHOCK_ORACLE: &str = "Shock deals 2 damage to any target.";

/// Shared fixture for row N23 (phase-rs/phase#8355 round-6 defect B10) and its
/// mandatory paired positive control. Builds a board where Hallow (verbatim
/// Oracle text) targets a printed damage instant ("Lava Axe"-shaped) on the
/// stack, and drives `change_targets::resolve` DIRECTLY for a `Single`-scope
/// Bolt Bend targeting Hallow — the same direct-call style this file's row 1d
/// (`retarget_with_no_legal_alternative_resolves_as_no_change`)'s sibling
/// fixtures in `change_targets.rs`'s own test module use, rather than driving
/// Bolt Bend through the real cast + priority pipeline. That is deliberate:
/// once Bolt Bend is genuinely CAST, `stack::resolve_top` pops it into
/// `state.resolving_stack_entry` before running its effect, and
/// `targeting.rs`'s stack-spell enumeration (`filter_targets_stack_spells`)
/// chains over `resolving_stack_entry` too (CR 608: a spell remains "on the
/// stack" while resolving) — so Bolt Bend, itself an Instant, would become a
/// SECOND legal candidate for Hallow's own "instant or sorcery" source slot
/// and the pool this row means to empty would never actually empty. Calling
/// `resolve` directly is exactly the reviewer's own probe methodology
/// (a bare function call with a hand-built `ChangeTargets` ability), which is
/// why it does not hit this artifact.
///
/// When `remove_source_before_resolution` is true, the axe is popped from
/// `state.stack` alone (its `state.objects` row untouched) BEFORE `resolve`
/// runs — mirroring the reviewer's D1 board: Hallow's own per-slot authority
/// (`node_slot_filters`' `PreventDamage` source arm) requires an instant or
/// sorcery ON THE STACK, so its `slot_pools[0]` empties; but its GENERIC
/// `target_filter()` (the recipient field, unconstrained — "prevent ALL
/// damage" — `TargetFilter::Any`) still drives `legal_new_targets_for_entry`'s
/// branch 3, which stays non-empty (every player and creature on the board).
/// That divergence — a `Single` prompt whose addressed position's pool is
/// empty while the union is not — is exactly B10's shape.
fn hallow_targets_axe_via_bolt_bend(
    remove_source_before_resolution: bool,
) -> (GameRunner, Vec<GameEvent>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P1, "Bystander", 2, 2);

    let mut runner = scenario.build();

    // The declared source spell: a printed damage instant, placed directly on
    // the stack so it can be removed from `state.stack` alone.
    let axe_parsed = parse_oracle_text(
        LAVA_AXE_ORACLE,
        "Lava Axe",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let axe_id = create_object(
        runner.state_mut(),
        CardId(811),
        P1,
        "Lava Axe".to_string(),
        Zone::Stack,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&axe_id)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Instant];
    let axe_ability = ResolvedAbility::new(
        axe_parsed.abilities[0].effect.as_ref().clone(),
        vec![TargetRef::Player(P0)],
        axe_id,
        P1,
    );
    runner.state_mut().stack.push_back(StackEntry {
        id: axe_id,
        source_id: axe_id,
        controller: P1,
        kind: StackEntryKind::Spell {
            card_id: CardId(811),
            ability: Some(Box::new(axe_ability)),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    // Hallow, targeting the axe — verbatim Oracle text.
    let hallow_parsed =
        parse_oracle_text(HALLOW_ORACLE, "Hallow", &[], &["Sorcery".to_string()], &[]);
    let hallow_id = create_object(
        runner.state_mut(),
        CardId(812),
        P0,
        "Hallow".to_string(),
        Zone::Stack,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&hallow_id)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Sorcery];
    let hallow_ability = ResolvedAbility::new(
        hallow_parsed.abilities[0].effect.as_ref().clone(),
        vec![TargetRef::Object(axe_id)],
        hallow_id,
        P0,
    );
    runner.state_mut().stack.push_back(StackEntry {
        id: hallow_id,
        source_id: hallow_id,
        controller: P0,
        kind: StackEntryKind::Spell {
            card_id: CardId(812),
            ability: Some(Box::new(hallow_ability)),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    if remove_source_before_resolution {
        // Remove ONLY the stack entry — `state.objects[&axe_id]` is left
        // untouched, matching R20's measured mechanism (`StackSpell`
        // enumeration is keyed on `state.stack` membership, not object zone).
        runner.state_mut().stack.retain(|entry| entry.id != axe_id);
    }

    // Bolt Bend, built directly (not cast) and driven straight through
    // `change_targets::resolve` — see this function's doc for why.
    let bolt_bend_ability = ResolvedAbility::new(
        Effect::ChangeTargets {
            target: TargetFilter::StackSpell,
            scope: RetargetScope::Single,
            forced_to: None,
        },
        vec![TargetRef::Object(hallow_id)],
        ObjectId(9000),
        P0,
    );
    let mut events = Vec::new();
    change_targets::resolve(runner.state_mut(), &bolt_bend_ability, &mut events)
        .expect("Bolt Bend's ChangeTargets effect must resolve without error");

    (runner, events)
}

/// N23 — B10's DISCRIMINATING ROW. A `Single` prompt whose addressed
/// position's pool is empty while the union is not must NOT park — the
/// effect resolves as CR 115.7a's no-change instead of an unanswerable
/// prompt (phase-rs/phase#8355 round-6 defect B10).
///
/// REVERT-FAILING in two directions: at `bb28b0e8b` the prompt PARKS here
/// (the flat `legal_new_targets.is_empty()` guard never fires, because the
/// union is non-empty) and offers a pool no `Single` submission can ever
/// satisfy for this position — an unanswerable prompt. Against a collapse
/// that narrows `Single`'s admit set to `slot_pools[0]` WITHOUT this gate, the
/// prompt also parks, but admits ZERO submissions (not even the no-change
/// one) — B10 exactly.
#[test]
fn hallow_single_scope_prompt_with_empty_own_pool_resolves_as_no_change() {
    let (runner, events) = hallow_targets_axe_via_bolt_bend(true);

    // THE DISCRIMINATOR: the prompt must not park.
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::RetargetChoice { .. }
        ),
        "CR 115.7a: an addressed position with an empty pool must not park an \
         unanswerable prompt, got {:?}",
        runner.state().waiting_for
    );

    // Positive reach-guard: the ChangeTargets effect resolved rather than
    // being skipped or fizzling silently.
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::ChangeTargets,
                ..
            }
        )),
        "reach guard: the ChangeTargets effect must have resolved, got {events:?}"
    );

    // Positive reach-guard: CR 115.7a's "the original target is unchanged" —
    // Hallow's declared target is still the (now-vanished) axe id.
    let hallow_entry = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.ability().is_some_and(|a| a.source_id == entry.id))
        .and_then(|entry| entry.ability())
        .filter(|ability| {
            matches!(
                ability.effect,
                engine::types::ability::Effect::PreventDamage { .. }
            )
        });
    assert!(
        hallow_entry.is_some(),
        "reach guard: Hallow must still be on the stack, unresolved"
    );
}

/// P-GATE-adjacent paired positive control (mandatory): with the source spell
/// STILL on the stack, the `Single` prompt DOES park and IS dischargeable —
/// so the negative row above cannot pass by suppressing everything.
#[test]
fn hallow_single_scope_prompt_with_nonempty_own_pool_still_parks() {
    let (runner, _events) = hallow_targets_axe_via_bolt_bend(false);

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::RetargetChoice { .. }
        ),
        "paired positive control: with the source spell still on the stack, the \
         prompt must still park"
    );
}

/// Row N22's fixture — B9's hostile row. Hallow (verbatim), currently
/// targeting Lava Axe, with a SECOND instant (Shock, verbatim) also on the
/// stack, so the `PreventDamage` source slot's own pool has TWO CR-legal
/// members instead of N23's zero. Built and driven straight through
/// `change_targets::resolve` (a real production entry point), exactly like
/// `hallow_targets_axe_via_bolt_bend`.
fn hallow_targets_axe_with_shock_also_on_stack_via_bolt_bend() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P1, "Bystander", 2, 2);

    let mut runner = scenario.build();

    let axe_parsed = parse_oracle_text(
        LAVA_AXE_ORACLE,
        "Lava Axe",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let axe_id = create_object(
        runner.state_mut(),
        CardId(811),
        P1,
        "Lava Axe".to_string(),
        Zone::Stack,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&axe_id)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Instant];
    let axe_ability = ResolvedAbility::new(
        axe_parsed.abilities[0].effect.as_ref().clone(),
        vec![TargetRef::Player(P0)],
        axe_id,
        P1,
    );
    runner.state_mut().stack.push_back(StackEntry {
        id: axe_id,
        source_id: axe_id,
        controller: P1,
        kind: StackEntryKind::Spell {
            card_id: CardId(811),
            ability: Some(Box::new(axe_ability)),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    let shock_parsed = parse_oracle_text(SHOCK_ORACLE, "Shock", &[], &["Instant".to_string()], &[]);
    let shock_id = create_object(
        runner.state_mut(),
        CardId(813),
        P1,
        "Shock".to_string(),
        Zone::Stack,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&shock_id)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Instant];
    let shock_ability = ResolvedAbility::new(
        shock_parsed.abilities[0].effect.as_ref().clone(),
        vec![TargetRef::Player(P0)],
        shock_id,
        P1,
    );
    runner.state_mut().stack.push_back(StackEntry {
        id: shock_id,
        source_id: shock_id,
        controller: P1,
        kind: StackEntryKind::Spell {
            card_id: CardId(813),
            ability: Some(Box::new(shock_ability)),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    let hallow_parsed =
        parse_oracle_text(HALLOW_ORACLE, "Hallow", &[], &["Sorcery".to_string()], &[]);
    let hallow_id = create_object(
        runner.state_mut(),
        CardId(812),
        P0,
        "Hallow".to_string(),
        Zone::Stack,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&hallow_id)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Sorcery];
    let hallow_ability = ResolvedAbility::new(
        hallow_parsed.abilities[0].effect.as_ref().clone(),
        vec![TargetRef::Object(axe_id)],
        hallow_id,
        P0,
    );
    runner.state_mut().stack.push_back(StackEntry {
        id: hallow_id,
        source_id: hallow_id,
        controller: P0,
        kind: StackEntryKind::Spell {
            card_id: CardId(812),
            ability: Some(Box::new(hallow_ability)),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    let bolt_bend_ability = ResolvedAbility::new(
        Effect::ChangeTargets {
            target: TargetFilter::StackSpell,
            scope: RetargetScope::Single,
            forced_to: None,
        },
        vec![TargetRef::Object(hallow_id)],
        ObjectId(9000),
        P0,
    );
    let mut events = Vec::new();
    change_targets::resolve(runner.state_mut(), &bolt_bend_ability, &mut events)
        .expect("Bolt Bend's ChangeTargets effect must resolve without error");

    (runner, axe_id, shock_id)
}

/// Row N22 — B9's hostile row. A `Single` prompt on a `PreventDamage` source
/// slot is offered and admits its own CR-legal pool: Hallow (verbatim) with a
/// Lava Axe (its current target) and a Shock on the stack.
///
/// REVERT-FAILING in three directions, all against `bb28b0e8b`: BASE's pool
/// authority disagreed with its enforcement authority (round-5 defect B9) — it
/// offered the four-member player/creature cascade and refused both instants
/// that were actually CR-legal for this slot.
///
/// RELABELLED per §Disagreements #5 (phase-rs/phase#8355 round 8): accepting
/// the CURRENT target (the axe) here is ordinary pool membership, not a
/// softlock fix (`retarget_prompt_is_dischargeable` owns that class), and it
/// is CR-loose under CR 115.7a while `shock` remains a legal, distinct
/// alternative (`TRACKED` #12) — the assertion below is kept because it is
/// what the code does and it is revert-failing, not because it is endorsed as
/// CR-115.7a-correct.
#[test]
fn hallow_single_scope_prompt_admits_its_own_cr_legal_pool() {
    let (runner, axe_id, shock_id) = hallow_targets_axe_with_shock_also_on_stack_via_bolt_bend();

    let WaitingFor::RetargetChoice {
        current_targets,
        slot_pools,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!("fixture must park a RetargetChoice");
    };

    // Reach guard: this must NOT be the outer-empty compatibility fallback —
    // a real production prompt always stores an aligned `slot_pools`.
    assert_eq!(
        slot_pools.len(),
        current_targets.len(),
        "N22: slot_pools must be aligned 1:1 with current_targets, not the \
         outer-empty compatibility shape"
    );

    // Discriminating: the pool is exactly [axe, shock] — no player, no
    // creature (both negative controls against BASE's four-member cascade).
    assert_eq!(
        slot_pools[0],
        vec![TargetRef::Object(axe_id), TargetRef::Object(shock_id)],
        "N22: the PreventDamage source slot's own pool must be exactly the \
         two on-stack instants, not BASE's player/creature cascade"
    );

    // shock -> Ok (RF: at BASE Err, the pool never contained an instant).
    let mut shock_probe = GameRunner::from_state(runner.state().clone());
    shock_probe
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(shock_id)),
        })
        .expect("N22: a slot-legal alternative (shock) must be accepted");

    // a player -> Err (RF: at BASE Ok, the cascade admitted every player).
    let mut player_probe = GameRunner::from_state(runner.state().clone());
    player_probe
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P0)),
        })
        .expect_err("N22: a player is never CR-legal for a PreventDamage source slot");

    // the current target (axe) -> Ok (RF: at BASE Err — BASE's cascade never
    // contained the axe itself, only the four other objects).
    let mut axe_probe = GameRunner::from_state(runner.state().clone());
    axe_probe
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(axe_id)),
        })
        .expect("N22 / TRACKED #12: the current target is ordinary pool membership");
}

/// Push a single-target Lightning-Bolt-shaped spell at stack index 0 targeting
/// `victim`, and return its id.
fn push_bolt_entry(runner: &mut GameRunner, victim: ObjectId) -> ObjectId {
    let parsed = parse_oracle_text(
        LIGHTNING_BOLT_ORACLE,
        "Lightning Bolt",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let bolt_id = create_object(
        runner.state_mut(),
        CardId(77),
        P1,
        "Lightning Bolt".to_string(),
        Zone::Stack,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&bolt_id)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Instant];
    let ability = ResolvedAbility::new(
        parsed.abilities[0].effect.as_ref().clone(),
        vec![TargetRef::Object(victim)],
        bolt_id,
        P1,
    );
    runner.state_mut().stack.push_back(StackEntry {
        id: bolt_id,
        source_id: bolt_id,
        controller: P1,
        kind: StackEntryKind::Spell {
            card_id: CardId(77),
            ability: Some(Box::new(ability)),
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    bolt_id
}

/// Row 2a — CR 115.7a: every candidate the AI generates for a parked
/// `RetargetChoice` must be a submission the reducer accepts. At base the sole
/// candidate is `current_targets`, which `apply_retarget` rejects whenever the
/// current target has dropped out of the pool (hexproof gained, protection
/// granted) — three rejections in a row halt the AI controller.
#[test]
fn ai_retarget_candidates_are_accepted_by_the_reducer() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let alternative = scenario.add_creature(P0, "Goblin", 1, 1).id();
    scenario.add_creature(P0, "Bystander", 1, 1);
    let victim = scenario.add_creature(P1, "Bear", 2, 2).id();

    let mut runner = scenario.build();
    push_bolt_entry(&mut runner, victim);

    // The current target is deliberately ABSENT from the pool: it became
    // illegal after the spell was cast (hexproof gained, protection granted,
    // etc.) — modeled directly on the payload below, not derived from board
    // state.
    //
    // phase-rs/phase#8355 round-8 review finding H2 (second pass):
    // production-shaped `slots`/`slot_pools`, not the outer-empty compat
    // shorthand this row used before. The compat shorthand's own
    // `legal_new_targets` is a hand-written stand-in for "the pool" that only
    // holds while nothing re-derives it — under H3, `engine::apply_retarget`
    // and `ai_support::candidates::retarget_actions` re-derive per-position
    // pools from the LIVE board for an outer-empty `slot_pools`, so a
    // fixture in that shape stops pinning which pool either side actually
    // reads (measured: with `victim` a plain creature and no counter-keyword,
    // re-derivation reads it back off the live board as legal, the generator
    // proposes it among five candidates, and the reducer accepts it — the
    // exact BASE defect this row exists to catch). A production prompt of
    // ANY scope is never built with an empty `slot_pools` (INVARIANT SC) —
    // pin the fixture to that shape so both sides read the SAME declared
    // pool this test controls, with no board-state proxy required to keep
    // `victim` out of it.
    let slot_pool = vec![TargetRef::Object(alternative)];
    runner.state_mut().waiting_for = WaitingFor::RetargetChoice {
        player: P0,
        stack_entry_index: 0,
        scope: RetargetScope::Single,
        current_targets: vec![TargetRef::Object(victim)],
        slots: vec![RetargetSlotAddress {
            path: vec![],
            slot: 0,
        }],
        slot_pools: vec![slot_pool.clone()],
        legal_new_targets: slot_pool,
    };

    let candidates = retarget_candidates(runner.state());

    // Positive reach-guard: the arm was reached and produced retarget actions.
    assert!(
        !candidates.is_empty(),
        "reach guard: the RetargetChoice arm must produce candidates"
    );

    // Discriminating: at base the single candidate is `[victim]`, which the
    // reducer rejects with "chosen target not in legal alternatives".
    for new_targets in &candidates {
        let mut probe = GameRunner::from_state(runner.state().clone());
        probe
            .act(GameAction::RetargetSpell {
                new_targets: new_targets.clone(),
            })
            .unwrap_or_else(|err| {
                panic!("candidate {new_targets:?} must be accepted by the reducer: {err:?}")
            });
    }

    runner
        .act(GameAction::RetargetSpell {
            new_targets: vec![TargetRef::Object(alternative)],
        })
        .expect("the retarget submission must be accepted");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(
        runner.state().stack[0].ability().unwrap().targets,
        vec![TargetRef::Object(alternative)],
        "CR 115.7a: the accepted submission is applied to the stack entry"
    );
}

/// Row 2c — CR 115.7d: an "All" retarget offers the unchanged anchor AND every
/// single-slot substitution. At base exactly one candidate is produced, so a
/// multi-slot prompt could only ever be answered one way — and if that way is
/// rejected, not at all.
///
/// SCOPE OF THE CLAIM — read before citing this row. It pins the generator's
/// ENUMERATION SHAPE for a `multi_target` node.
///
/// phase-rs/phase#8355 (Invariant SC): `ai_support::candidates::retarget_actions`
/// now derives `bindings` via `ability_utils::chain_retarget_slots` UNCONDITIONALLY
/// (needed for the CR 115.3 run check even when the payload's `slot_pools` is the
/// outer-empty fallback), and that enumerator's alignment gate requires a node's
/// declared per-slot filter count to agree with its OWN target count — which for
/// a real card is guaranteed by `multi_target` always being set alongside a
/// multi-target run (no printed card reaches a 2-target `DealDamage{Any}` node
/// WITHOUT one). This fixture is updated to carry that same `multi_target` spec,
/// so it now exercises `NodeSlotFilters::UniformRun` (`mana_multi_role` is still
/// `None`, so this remains outside the mana/paired-subject admitted class) rather
/// than a malformed shape no real card can produce.
///
/// ONE CONSEQUENCE, PARTIALLY closing a gap this row used to observe rather
/// than endorse: `UniformRun` now shares ONE `run` id (CR 115.3) across both
/// slots. `retarget_slot_violation`'s CR 115.3 check scans EVERY prior
/// position's `(binding, submitted)` pair regardless of whether that prior
/// position was itself exempt (unchanged) — exemption skips the REJECTION
/// check for that position, not its contribution to the run-tracking scan.
/// The two duplicate candidates are therefore asymmetric:
///   * `[a, a]` (slot 1 substituted to `a`): position 0 is exempt (already
///     `a`) but still recorded; position 1's scan sees position 0 already
///     holding `a` under the SAME run and is REJECTED. `expected` below no
///     longer contains it.
///   * `[b, b]` (slot 0 substituted to `b`): position 0 is CHECKED FIRST,
///     before position 1 (still `b`, unchanged) has contributed anything to
///     scan — so nothing has recorded `b` under the shared run yet, and it
///     survives. Still recorded as OBSERVED CURRENT BEHAVIOUR, deliberately
///     not endorsed: it is an ORDER-DEPENDENT survival, not a closed gap.
#[test]
fn all_scope_retarget_candidates_cover_every_slot() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let a = scenario.add_creature(P0, "Alpha", 1, 1).id();
    let b = scenario.add_creature(P0, "Beta", 1, 1).id();
    let c = scenario.add_creature(P0, "Gamma", 1, 1).id();

    let mut runner = scenario.build();

    // A non-mana two-target node: `mana_multi_role` returns `None`, so
    // `retarget_slot_violation` early-outs and this row isolates enumeration.
    let parsed = parse_oracle_text(
        LIGHTNING_BOLT_ORACLE,
        "Lightning Bolt",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let source = create_object(
        runner.state_mut(),
        CardId(803),
        P0,
        "Two-Slot Source".to_string(),
        Zone::Battlefield,
    );
    let mut ability = ResolvedAbility::new(
        parsed.abilities[0].effect.as_ref().clone(),
        vec![TargetRef::Object(a), TargetRef::Object(b)],
        source,
        P0,
    );
    // A real 2-target `DealDamage{Any}` node always carries `multi_target` —
    // required for `chain_retarget_slots`' alignment gate (see this row's SCOPE
    // note above).
    ability.multi_target = Some(MultiTargetSpec::fixed(2, 2));
    assert!(
        mana_multi_role(&ability.effect).is_none(),
        "fixture guard: this row's node is outside the mana/paired-subject admitted class"
    );
    runner.state_mut().stack.push_back(StackEntry {
        id: ObjectId(902),
        source_id: source,
        controller: P0,
        kind: StackEntryKind::ActivatedAbility {
            source_id: source,
            ability: Box::new(ability),
        },
    });

    let current_targets = vec![TargetRef::Object(a), TargetRef::Object(b)];
    // phase-rs/phase#8355 round-8 review finding H3: an outer-empty
    // `slot_pools` no longer falls back to the flat `legal_new_targets` union
    // — it re-derives the REAL "any target" pool from the live board (every
    // player and creature), which would make this row's enumeration-shape
    // assertion noisy with board membership rather than isolating the
    // `UniformRun` shape it exists to pin. Production-shaped instead: both
    // `UniformRun` positions share ONE stored pool, `[a, b, c]`, matching what
    // this row's fabricated `legal_new_targets` modelled all along.
    let slot_pool = vec![
        TargetRef::Object(a),
        TargetRef::Object(b),
        TargetRef::Object(c),
    ];
    runner.state_mut().waiting_for = WaitingFor::RetargetChoice {
        player: P0,
        stack_entry_index: 0,
        scope: RetargetScope::All,
        current_targets: current_targets.clone(),
        slots: vec![
            RetargetSlotAddress {
                path: vec![],
                slot: 0,
            },
            RetargetSlotAddress {
                path: vec![],
                slot: 1,
            },
        ],
        slot_pools: vec![slot_pool.clone(), slot_pool.clone()],
        legal_new_targets: slot_pool,
    };

    let candidates = retarget_candidates(runner.state());

    // Positive reach-guards — each covers half the claim. Ordered BEFORE the
    // exact-equality assert deliberately: after it they are strictly entailed by
    // it and can never fire, which is how they previously stood. Row 2e already
    // uses this ordering. They localize a failure to "the anchor was dropped" or
    // "no substitution was offered" before the exact list is compared.
    assert!(
        candidates.iter().any(|c| *c != current_targets),
        "reach guard: at least one per-slot substitution was offered"
    );
    assert!(
        candidates.contains(&current_targets),
        "reach guard: CR 115.7d's unchanged anchor survived"
    );

    // Derived from the arm's own shape: the anchor, plus one substitution per
    // (slot, pool member) pair minus the two identity pairs. `[b, b]` and
    // `[a, a]` are the duplicate-producing entries; per this row's SCOPE note
    // they are pinned as observed behaviour, NOT asserted to be CR-legal.
    let expected = vec![
        vec![TargetRef::Object(a), TargetRef::Object(b)],
        vec![TargetRef::Object(b), TargetRef::Object(b)],
        vec![TargetRef::Object(c), TargetRef::Object(b)],
        vec![TargetRef::Object(a), TargetRef::Object(c)],
    ];
    assert_eq!(
        candidates, expected,
        "the anchor plus one substitution per (slot, pool member) pair — \
         enumeration shape only; see this row's SCOPE note on the duplicates"
    );
}

/// Build the synthetic multi-role mana fixture rows 2e and 2g share: an
/// `Effect::Mana` whose `ManaTargetRole::Both` declares a recipient slot legal
/// only for an OPPONENT of P0 and a count-source slot legal for any player.
///
/// `current_targets` is per row and load-bearing in opposite directions — 2e
/// needs slot 0 legal-but-different, 2g needs slot 0 currently illegal — so it
/// is a parameter, never a shared constant.
fn push_multi_role_mana_entry(runner: &mut GameRunner, current_targets: Vec<TargetRef>) {
    let role = ManaTargetRole::Both {
        // Slot 0 (recipient, surfaced first): only an opponent of P0, i.e. P1.
        recipient: TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
        // Slot 1 (count source): any player.
        count_source: TargetFilter::Player,
    };
    let source = create_object(
        runner.state_mut(),
        CardId(901),
        P0,
        "Multi-Role Mana Source".to_string(),
        Zone::Battlefield,
    );
    let entry_id = create_object(
        runner.state_mut(),
        CardId(901),
        P0,
        "Multi-Role Mana Ability".to_string(),
        Zone::Stack,
    );
    let ability = ResolvedAbility::new(
        Effect::Mana {
            produced: ManaProduction::Colorless {
                count: QuantityExpr::Fixed { value: 1 },
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: Some(role),
        },
        current_targets,
        source,
        P0,
    );
    runner.state_mut().stack.push_back(StackEntry {
        id: entry_id,
        source_id: source,
        controller: P0,
        kind: StackEntryKind::ActivatedAbility {
            source_id: source,
            ability: Box::new(ability),
        },
    });
}

/// Both structural reach-guards for the synthetic multi-role rows.
/// (phase-rs/phase#8355 round-8 review LOW finding: corrects a justification
/// that named `retarget_actions`' `is_none_or` — no such call exists in that
/// function; the actual mechanism is below.) Without the first,
/// `retarget_actions`' own `entry.ability()` lookup fails and it returns NO
/// candidates (`let Some(stack_ability) = entry.ability() else { return
/// Vec::new(); }`) — this row's own emptiness reach-guard would then fail for
/// the wrong reason (no ability at all) rather than by the per-slot filtering
/// this row means to test. Without the second, `chain_retarget_slots` no
/// longer produces the per-role-pair bindings this fixture's `slots` address,
/// so `retarget_slots_aligned` rejects the mismatch and `retarget_actions`
/// again returns nothing. Either degradation makes these rows vacuous.
fn assert_multi_role_entry_is_live(runner: &GameRunner) {
    assert!(
        runner.state().stack[0].ability().is_some(),
        "reach guard: stack index 0 must carry the ability under test"
    );
    assert!(
        mana_multi_role(&runner.state().stack[0].ability().unwrap().effect).is_some(),
        "reach guard: the node must be inside the per-slot admitted class"
    );
}

/// Row 2e — CR 115.7a: the retarget pool is a FLAT union over both role filters,
/// so it contains members legal only for the *other* slot. The generator must
/// consult the same per-slot authority the reducer does, rather than proposing
/// a pool member the reducer will reject.
///
/// SCOPE OF THE CLAIM — read before citing this row. It pins SLOT LEGALITY: that
/// the generator filters the flat pool through the same authority the reducer
/// applies. It does NOT claim the submission it accepts is CR-115.7a /
/// CR-115.7b-legal, and must not be cited as if it did.
///
/// This fixture's `current_targets` has LENGTH 2 and the accepted submission has
/// LENGTH 1, because the reducer's `Single` arm hard-requires exactly one target.
/// (phase-rs/phase#8355 round-8 review LOW finding, correcting this note: with
/// `slots` populated — as this fixture does, see "UPDATED" below —
/// `apply_retarget`'s write is PER-ADDRESS, not the compat branch's blanket
/// `ability.targets = new_targets`. Applying `[P1]` writes ONLY slot 0;
/// `ability.targets` stays LENGTH 2, and slot 1 keeps its prior value
/// untouched. It is the outer-empty `slots` COMPAT branch alone that
/// overwrites the whole vector and would truncate to length 1 — not this
/// fixture's write, and not any LIVE production `Single` prompt, which is
/// never built with an empty `slots` under Invariant SC.) Both subrules that
/// reach this arm still prescribe DIFFERENT remedies for what a Single-scope
/// submission may leave unaddressed — `RetargetScope::Single` is produced by
/// two oracle templates (`try_parse_change_targets`,
/// parser/oracle_effect/mod.rs), so a correctness fix must DISPATCH ON THE
/// TEMPLATE rather than apply CR 115.7b's remedy uniformly:
///   - "change a target of " → CR 115.7b: "the process described in rule 115.7a
///     is followed, except that only one of those targets may be changed (rather
///     than all of them or none of them)". This fixture's per-address write
///     already matches this remedy exactly: one slot changes, the other stays
///     in place, untouched.
///   - "change the target of " → CR 115.7a, which ends: "If all the targets
///     aren't changed to other legal targets, none of them are changed." Remedy
///     for a multi-target entry: ALL-OR-NONE — changing only slot 0 while slot 1
///     stays as it was is WRONG under this subrule, even though nothing is lost
///     or corrupted. This is Bolt Bend's wording — the WORDING only. Bolt Bend
///     itself reads "with a single target", and of the 22 printed cards matching
///     `o:"change the target of"`, the six omitting that literal phrase each
///     restrict to one target by the equivalent "targets ONLY <x>" / "a single
///     <x>" construction. No printed card on this template can therefore present
///     a multi-target entry, which is why the fixture below is synthetic.
///
/// So the length-1 acceptance below is CR-115.7b-correct for THIS fixture and
/// only a concern under the unreachable-by-printed-card CR-115.7a template.
/// Recorded here as OBSERVED CURRENT BEHAVIOUR rather than a blanket
/// endorsement, because `RetargetScope::Single` itself cannot tell the two
/// templates apart at this seam:
///
///   DEFERRED(out-of-run): interactive Single-scope retarget cannot honor CR
///   115.7a's all-or-none remedy for a synthetic multi-target entry reached
///   through the "change the target of" template — upstream cause filter.rs
///   FilterProp::HasSingleTarget is permissive with no resolution-time
///   validation; fix needs filter.rs + interaction.rs, both outside phase 1's
///   frozen scope.
///
/// The honest behavioural delta this phase knowingly takes: at BASE the AI froze
/// on this class — the generator's sole proposal was rejected, so no actor could
/// discharge the prompt. AFTER this change it PROGRESSES with a per-address,
/// single-slot write that does not truncate or corrupt the target list; it is
/// simply not a claim of CR-115.7a correctness for a synthetic "change the
/// target of" entry. A genuinely CR-115.7a-correct submission for that template
/// is not expressible against today's reducer contract at all, which is why the
/// deferral above names two paths outside this run's frozen scope rather than a
/// local repair.
///
/// UPDATED (phase-rs/phase#8355, round 8): under Invariant SC a production
/// `Single`-scope prompt is no longer built with an empty `slot_pools` — every
/// exposed position, under EVERY scope, gets its pool from the one computation
/// (`change_targets::slot_pool`). The outer-empty `slot_pools` this fixture
/// used to hand-build no longer models a REACHABLE production `Single` prompt
/// (only a payload predating the field, or a `slot_pools` this test never
/// populated) — under that fallback `retarget_slot_violation` correctly falls
/// back to the FLAT union for every position, since there is no per-position
/// record to consult, and per-slot enforcement is lost. This fixture now
/// populates `slots`/`slot_pools` itself, matching what
/// `ability_utils::chain_retarget_slots` + the per-slot pool computation would
/// produce for this ability, so it exercises the SAME per-slot authority a real
/// prompt would have stored (CR 115.7a; B9/B10).
#[test]
fn multi_role_mana_single_retarget_candidates_are_slot_legal() {
    let mut runner = GameScenario::new().build();

    // Slot 0 holds P1 (legal for the opponent-only recipient slot); slot 1 holds
    // P0 (legal for the any-player count-source slot). Slot 0 must NOT already
    // hold P0, or submitting `[P0]` would be an exempt non-change and would
    // survive for a reason unrelated to slot legality.
    let current_targets = vec![TargetRef::Player(P1), TargetRef::Player(P0)];
    push_multi_role_mana_entry(&mut runner, current_targets.clone());
    assert_multi_role_entry_is_live(&runner);

    let legal_new_targets = vec![TargetRef::Player(P0), TargetRef::Player(P1)];
    // The per-position pools a real prompt would have stored: slot 0
    // (recipient, opponent-only) admits only P1; slot 1 (count source, any
    // player) admits both.
    let slot_pools = vec![vec![TargetRef::Player(P1)], legal_new_targets.clone()];
    runner.state_mut().waiting_for = WaitingFor::RetargetChoice {
        player: P0,
        stack_entry_index: 0,
        scope: RetargetScope::Single,
        current_targets,
        slots: vec![
            RetargetSlotAddress {
                path: vec![],
                slot: 0,
            },
            RetargetSlotAddress {
                path: vec![],
                slot: 1,
            },
        ],
        slot_pools,
        legal_new_targets: legal_new_targets.clone(),
    };

    let candidates = retarget_candidates(runner.state());

    // Reach-guard (a): something survived — excludes the world where the
    // generator returns nothing and the discriminator holds by emptiness.
    assert!(
        !candidates.is_empty(),
        "reach guard: a slot-legal submission must still be offered"
    );
    // Reach-guard (b): something was dropped — excludes the opposite world where
    // the slot filter never engaged and every pool member was proposed.
    assert!(
        candidates.len() < legal_new_targets.len(),
        "reach guard: the slot-legality filter must have removed a pool member, \
         got {candidates:?}"
    );

    // Discriminating: P0 is in the flat pool but is legal only for slot 1, and a
    // `Single` submission lands in slot 0.
    assert_eq!(
        candidates,
        vec![vec![TargetRef::Player(P1)]],
        "CR 115.7a: only the slot-0-legal pool member may be proposed"
    );

    // Discriminating: every candidate is accepted by the reducer. At base the
    // sole candidate is `[P1, P0]` (length 2), which the `Single` arm rejects.
    //
    // "Accepted by the reducer" is the WHOLE claim here — acceptance, not
    // rules-correctness. (phase-rs/phase#8355 round-8 review LOW finding,
    // correcting this note: with `slots` populated, as this fixture's payload
    // above does, `apply_retarget`'s write is PER-ADDRESS and does NOT
    // truncate — slot 1 keeps its prior value untouched; only the outer-empty
    // `slots` compat branch overwrites the whole vector.) This length-1
    // submission is CR-115.7b-correct; it is only a possible CR-115.7a
    // all-or-none concern under the unreachable-by-printed-card "change the
    // target of" template — see this row's SCOPE note and the
    // DEFERRED(out-of-run) entry it carries. This loop deliberately asserts
    // only that the submission is accepted, and never asserts the resulting
    // `ability.targets`, because that template-dependent legality question is
    // exactly what `RetargetScope::Single` cannot answer at this seam.
    for new_targets in &candidates {
        let mut probe = GameRunner::from_state(runner.state().clone());
        probe
            .act(GameAction::RetargetSpell {
                new_targets: new_targets.clone(),
            })
            .unwrap_or_else(|err| {
                panic!("candidate {new_targets:?} must be accepted by the reducer: {err:?}")
            });
    }
}

/// Row 2g — CR 115.7d: "the player may leave any number of the targets
/// unchanged, even if those targets would be illegal." End-to-end proof that the
/// generator proposes the unchanged anchor AND the reducer accepts it. At base
/// the two disagree: pool membership exempts the anchor while per-slot
/// validation rejects it, so the prompt is unanswerable.
#[test]
fn all_scope_unchanged_anchor_is_proposed_and_accepted_when_current_targets_are_slot_illegal() {
    let mut runner = GameScenario::new().build();

    // The mirror image of row 2e: slot 0 holds P0, which is ILLEGAL for the
    // opponent-only recipient slot, so the CR 115.7d exemption is the only thing
    // that can let the unchanged anchor through.
    let current_targets = vec![TargetRef::Player(P0), TargetRef::Player(P1)];
    push_multi_role_mana_entry(&mut runner, current_targets.clone());
    assert_multi_role_entry_is_live(&runner);

    runner.state_mut().waiting_for = WaitingFor::RetargetChoice {
        player: P0,
        stack_entry_index: 0,
        scope: RetargetScope::All,
        current_targets: current_targets.clone(),
        slots: vec![],
        slot_pools: vec![],
        legal_new_targets: vec![TargetRef::Player(P0), TargetRef::Player(P1)],
    };

    let candidates = retarget_candidates(runner.state());

    // Positive reach-guard: without this, "the anchor is accepted" could pass in
    // a world where the generator emits only the anchor and slot validation
    // never engaged at all.
    assert!(
        candidates.len() >= 2,
        "reach guard: substitutions must be offered alongside the anchor, got {candidates:?}"
    );
    assert!(
        candidates.iter().any(|c| *c != current_targets),
        "reach guard: at least one slot substitution was offered"
    );

    // Discriminating (generator half): the unchanged anchor is proposed.
    assert!(
        candidates.contains(&current_targets),
        "CR 115.7d: the unchanged anchor must be proposed, got {candidates:?}"
    );

    // Reducer half. Stated as a COUNTERFACTUAL because the straightforward
    // reading was measured and refuted: at base this line is NOT REACHED. The
    // base generator proposes only the anchor, so the reach guard above
    // (`candidates.len() >= 2`) fails first and is the row's actual base
    // discriminator. WERE it reached at base, base would fail here with
    // "Retarget: chosen target is not legal for target slot 0" — that is a
    // derivation from base source (base `retarget_slot_violation` has no CR
    // 115.7d exemption, so this fixture's slot 0 returns `Some(0)`), not an
    // observation.
    //
    // What this call uniquely guards at candidate is therefore NARROWER than a
    // second independent discriminator: the generator and the reducer now
    // consult one authority, so it can only fail if the REDUCER drifts strictly
    // more restrictive than the GENERATOR. That is an anti-drift consistency
    // check between the two consumers, and it is worth keeping as one — but it
    // is not independent evidence, and must not be cited as such.
    runner
        .act(GameAction::RetargetSpell {
            new_targets: current_targets.clone(),
        })
        .expect("CR 115.7d: leaving every target unchanged must be accepted");

    // Behavioural: the unchanged submission is a completed retarget, not a skip.
    // `WaitingFor::Priority` is the load-bearing half — only `apply_retarget`'s
    // tail sets it, so it cannot hold unless the reducer ran to completion.
    //
    // A `stack[0].ability().targets == current_targets` assertion stood here and
    // was REMOVED as trivially satisfied: `push_multi_role_mana_entry` already
    // set those targets, and the submission IS `current_targets`, so the write
    // `apply_retarget` performs is a no-op and the assertion holds whether or
    // not the reducer ever ran. It read as behavioural evidence while proving
    // nothing. The `.expect()` above plus this check are the real evidence.
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// Row N16 — THE ONLY compatibility pin (phase-rs/phase#8355 round-8 review
/// finding H3). `slots`/`slot_pools` are `#[serde(default)]` because
/// `GameState` crosses the multiplayer/WASM boundary against a hand-written TS
/// mirror, and a payload predating both fields (or version-skewed) must still
/// load, defaulting both to `[]`.
///
/// "Behaves as at BASE" is narrower than it first reads: it is the WRITE only
/// — `apply_retarget`'s `slots.is_empty()` branch, an unconditional root-level
/// `ability.targets = new_targets`, untouched by H3. Per-slot ADMISSION does
/// NOT fall back to the flat union: H3 found that degrading every position to
/// `legal_new_targets` reopens round-5 defect B2 (a candidate legal only for
/// ANOTHER slot gets admitted), so admission instead re-derives REAL
/// per-position pools from the live stack entry
/// (`change_targets::derive_slot_pools`), the same one computation a live
/// prompt would have stored.
///
/// The SIBLING pin, same test: a payload WITH an all-empty `slot_pools` of the
/// CORRECT length is a different case entirely (every addressed position has
/// no legal alternative) and must admit nothing — the outer-empty vs
/// inner-empty distinction must not be collapsed.
#[test]
fn n16_outer_empty_slot_pools_rederives_real_pools_not_the_union() {
    let mut runner = GameScenario::new().build();
    // Slot 0 holds P1 (legal for the opponent-only recipient slot); slot 1
    // holds P0 (legal for the any-player count-source slot).
    let current_targets = vec![TargetRef::Player(P1), TargetRef::Player(P0)];
    push_multi_role_mana_entry(&mut runner, current_targets.clone());
    assert_multi_role_entry_is_live(&runner);

    let addresses = vec![
        RetargetSlotAddress {
            path: vec![],
            slot: 0,
        },
        RetargetSlotAddress {
            path: vec![],
            slot: 1,
        },
    ];
    let live = WaitingFor::RetargetChoice {
        player: P0,
        stack_entry_index: 0,
        scope: RetargetScope::Single,
        current_targets: current_targets.clone(),
        slots: addresses.clone(),
        // The per-position pools a real prompt would have stored: slot 0
        // (recipient, opponent-only) admits only P1; slot 1 (count source,
        // any player) admits both.
        slot_pools: vec![
            vec![TargetRef::Player(P1)],
            vec![TargetRef::Player(P0), TargetRef::Player(P1)],
        ],
        legal_new_targets: vec![TargetRef::Player(P0), TargetRef::Player(P1)],
    };

    // Strip `slots`/`slot_pools` from the wire shape (`#[serde(tag = "type",
    // content = "data")]`), simulating a payload predating both fields.
    let mut wire = serde_json::to_value(&live).expect("serialize");
    let data = wire
        .get_mut("data")
        .expect("RetargetChoice must serialize as {type, data}")
        .as_object_mut()
        .expect("data must be a JSON object");
    assert!(
        data.remove("slots").is_some(),
        "fixture sanity: the slots key must exist before removal"
    );
    assert!(
        data.remove("slot_pools").is_some(),
        "fixture sanity: the slot_pools key must exist before removal"
    );

    let predating: WaitingFor = serde_json::from_value(wire)
        .expect("a payload predating slots/slot_pools must still deserialize");
    let WaitingFor::RetargetChoice {
        slots: predating_slots,
        slot_pools: predating_slot_pools,
        ..
    } = &predating
    else {
        panic!("must deserialize back into RetargetChoice");
    };
    assert_eq!(
        predating_slots,
        &Vec::<RetargetSlotAddress>::new(),
        "an omitted slots key must default to []"
    );
    assert_eq!(
        predating_slot_pools,
        &Vec::<Vec<TargetRef>>::new(),
        "an omitted slot_pools key must default to []"
    );

    // ---- Discriminating (H3): admission re-derives REAL per-position pools,
    // not the flat union. P0 is in the flat union (`legal_new_targets`) but is
    // illegal for slot 0 (P0 is never its own opponent) — under the pre-H3
    // union fallback this submission was wrongly ACCEPTED.
    let mut illegal_probe = GameRunner::from_state(runner.state().clone());
    illegal_probe.state_mut().waiting_for = predating.clone();
    assert!(
        illegal_probe
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(P0)),
            })
            .is_err(),
        "CR 115.7a / H3: an outer-empty payload must still enforce slot 0's REAL \
         per-position pool (opponent-only), not the flat union that also contains P0"
    );

    // ---- The write lands as at BASE for a slot-0-legal submission: root-
    // level, unconditional (the `slots.is_empty()` compatibility branch itself
    // is untouched by H3).
    let mut legal_probe = GameRunner::from_state(runner.state().clone());
    legal_probe.state_mut().waiting_for = predating;
    legal_probe
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("a slot-0-legal submission must still be accepted through the compat write path");
    assert_eq!(
        legal_probe.state().stack[0].ability().unwrap().targets,
        vec![TargetRef::Player(P1)],
        "BASE's compatibility write is root-level and unconditional: ability.targets \
         becomes exactly the submitted new_targets"
    );

    // ---- Sibling: an all-empty INNER `slot_pools` of the correct length must
    // NOT be treated as the outer-empty compatibility case — every position
    // has no legal alternative, and it must admit nothing.
    let inner_empty = WaitingFor::RetargetChoice {
        player: P0,
        stack_entry_index: 0,
        scope: RetargetScope::Single,
        current_targets: current_targets.clone(),
        slots: addresses,
        slot_pools: vec![vec![], vec![]],
        legal_new_targets: vec![TargetRef::Player(P0), TargetRef::Player(P1)],
    };
    for candidate in [TargetRef::Player(P0), TargetRef::Player(P1)] {
        let mut probe = GameRunner::from_state(runner.state().clone());
        probe.state_mut().waiting_for = inner_empty.clone();
        assert!(
            probe
                .act(GameAction::ChooseTarget {
                    target: Some(candidate.clone()),
                })
                .is_err(),
            "an all-empty INNER slot_pools of the correct length must admit nothing; \
             {candidate:?} must be rejected, not fall back to the union"
        );
    }
}

/// HIGH-1 (phase-rs/phase#8355 round-8 review, second pass): a compat
/// `Single` payload (outer-empty `slots`/`slot_pools`, the SAME shape N16
/// models) parked on a B10-shaped board — Hallow whose declared source spell
/// has left the stack — must still be dischargeable through
/// `engine::apply_retarget` AND propose something through
/// `ai_support::candidates::retarget_actions`.
///
/// PRE-FIX MECHANISM: `derive_slot_pools` alone re-derives REAL per-position
/// pools from the live board — for this board that is `[[]]` (Hallow's own
/// `PreventDamage` source slot has no legal instant/sorcery once the axe is
/// gone). Both call sites used that result UNCONDITIONALLY: `pool_for(0)`
/// then admits NOTHING AT ALL, and `retarget_slot_violation`/`retarget_
/// actions`'s `Single` arm agree there is nothing to accept or propose — an
/// unconditional hang, even though the payload's OWN `legal_new_targets`
/// (computed by the same `legal_new_targets_for_stack_entry` authority a live
/// prompt would have used) is non-empty. `retarget_prompt_is_dischargeable`
/// must be asked of the re-derived pools at BOTH call sites, falling back to
/// `legal_new_targets` when they fail it — exactly as `change_targets::
/// resolve` already does before parking a FRESH prompt (row N23, above). NOT
/// claimed: that the UNCHANGED current target (the vanished axe) becomes
/// acceptable — it was never a member of `legal_new_targets` either, and CR
/// 115.7a makes a change mandatory once a legal alternative exists; see the
/// negative check at the end of this test.
#[test]
fn compat_single_payload_on_b10_board_discharges_via_apply_retarget() {
    let (mut runner, _events) = hallow_targets_axe_via_bolt_bend(true);

    // Positive control (independent of the fix): the board is genuinely the
    // B10 shape — Hallow is the sole, UNRESOLVED stack entry, still targeting
    // its now-vanished source spell, and the board's own flat union (the SAME
    // production authority a live prompt's `legal_new_targets` comes from)
    // has real candidates to discharge against. All three hold regardless of
    // whether the HIGH-1 fix is present, so a failure here means the SETUP is
    // wrong, not the fix.
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach guard: Hallow is the sole stack entry"
    );
    let hallow_ability = runner.state().stack[0]
        .ability()
        .cloned()
        .expect("reach guard: Hallow must still carry its ability, unresolved");
    assert!(
        matches!(hallow_ability.effect, Effect::PreventDamage { .. }),
        "reach guard: the stack entry must be Hallow's PreventDamage effect"
    );
    let axe_current = hallow_ability.targets[0].clone();
    let legal_new_targets = change_targets::legal_new_targets_for_stack_entry(runner.state(), 0);
    assert!(
        legal_new_targets.len() >= 2,
        "reach guard: the board must offer a real flat union to discharge against, \
         got {legal_new_targets:?}"
    );

    // A compat payload: outer-empty `slots`/`slot_pools`, exactly N16's shape,
    // for this Filtered (not multi-role-mana) node.
    runner.state_mut().waiting_for = WaitingFor::RetargetChoice {
        player: P0,
        stack_entry_index: 0,
        scope: RetargetScope::Single,
        current_targets: vec![axe_current.clone()],
        slots: vec![],
        slot_pools: vec![],
        legal_new_targets: legal_new_targets.clone(),
    };

    // Discriminating (generator side, `ai_support::candidates::retarget_actions`):
    // pre-fix, `pool_for(0)` on the re-derived `[[]]` is empty, so `Single`'s
    // arm (`pool_for(0).iter().map(...)`) proposes NOTHING.
    let candidates = retarget_candidates(runner.state());
    assert!(
        !candidates.is_empty(),
        "HIGH-1: the AI generator must propose something for a compat payload \
         on a B10-shaped board, got none"
    );
    for new_targets in &candidates {
        let mut probe = GameRunner::from_state(runner.state().clone());
        probe
            .act(GameAction::RetargetSpell {
                new_targets: new_targets.clone(),
            })
            .unwrap_or_else(|err| {
                panic!("candidate {new_targets:?} must be accepted by the reducer: {err:?}")
            });
    }

    // Discriminating (reducer side, `engine::apply_retarget`): every member of
    // the payload's own union must be accepted — pre-fix every one of these
    // is rejected with "chosen target not in legal alternatives" because
    // `pool_for(0)` admits nothing (`derive_slot_pools` alone returns
    // `[[]]`). Post-fix each is accepted through the fallback to
    // `legal_new_targets` (`retarget_prompt_is_dischargeable` says no, so
    // `effective_pools` is empty and `pool_for` degrades uniformly).
    for target in &legal_new_targets {
        let mut probe = GameRunner::from_state(runner.state().clone());
        probe
            .act(GameAction::RetargetSpell {
                new_targets: vec![target.clone()],
            })
            .unwrap_or_else(|err| {
                panic!(
                    "HIGH-1: a compat Single payload on a B10-shaped board must discharge — \
                     {target:?} must be accepted, got {err:?}"
                )
            });
    }

    // NOT part of this fix's claim, stated to preempt a wrong reading: the
    // UNCHANGED current target (the vanished axe) is correctly rejected here
    // too, in BOTH pre- and post-fix worlds. It was never a member of
    // `legal_new_targets` (an instant is not a legal "any target" recipient),
    // so `pool_for(0)`'s union fallback does not admit it either — and CR
    // 115.7a makes a change to a legal alternative MANDATORY once one exists
    // (this board has `legal_new_targets`), so there is no independent
    // "resubmit the current target" escape to preserve.
    let mut unchanged_probe = GameRunner::from_state(runner.state().clone());
    assert!(
        unchanged_probe
            .act(GameAction::RetargetSpell {
                new_targets: vec![axe_current],
            })
            .is_err(),
        "CR 115.7a: with a legal alternative available, resubmitting the vanished \
         axe as the new target must NOT be accepted"
    );
}

/// MED-1 (phase-rs/phase#8355 round-8 review, second pass): `pool_for`'s union
/// fallback is PER-INDEX (`.get(idx)`), so a NON-EMPTY `effective_pools`
/// strictly shorter than `current_targets`/the submission would mix two
/// authorities inside ONE `All` submission — positions within
/// `effective_pools`'s bounds enforced against their real per-position pool,
/// positions past the end silently degrading to the flat union. Reproduces the
/// exact measured shape: a length-3 `All` payload (a stale/mismatched
/// `current_targets`) against a LIVE 2-binding multi-role-mana node.
///
/// `slots`/`slot_pools` are outer-empty (compat) so `effective_pools` becomes
/// `derive_slot_pools`'s re-derivation over the LIVE node's bindings — length
/// 2, strictly shorter than this payload's length-3 `current_targets`.
#[test]
fn med1_all_scope_mismatched_pool_length_is_rejected_outright() {
    let mut runner = GameScenario::new().build();

    // The LIVE node: 2 bindings (slot 0 opponent-only admits P1; slot 1
    // any-player admits both), current targets `[P1, P0]`.
    push_multi_role_mana_entry(
        &mut runner,
        vec![TargetRef::Player(P1), TargetRef::Player(P0)],
    );
    assert_multi_role_entry_is_live(&runner);

    // The PAYLOAD's own `current_targets`: length 3, a stale/mismatched shape
    // (e.g. version-skewed) the live 2-binding node no longer matches.
    let current_targets = vec![
        TargetRef::Player(P1),
        TargetRef::Player(P0),
        TargetRef::Player(P1),
    ];
    runner.state_mut().waiting_for = WaitingFor::RetargetChoice {
        player: P0,
        stack_entry_index: 0,
        scope: RetargetScope::All,
        current_targets: current_targets.clone(),
        slots: vec![],
        slot_pools: vec![],
        legal_new_targets: vec![TargetRef::Player(P0), TargetRef::Player(P1)],
    };

    // Reach guard: the generator must not propose from a mismatched-length
    // payload either (candidates.rs carries the same MED-1 guard) — otherwise
    // it would offer something the reducer's own guard below then rejects,
    // breaking "every proposal is accepted by construction."
    let candidates = retarget_candidates(runner.state());
    assert!(
        candidates.is_empty(),
        "MED-1: the generator must not propose anything for a mismatched-length \
         All payload, got {candidates:?}"
    );

    // Discriminating: positions 0 and 1 are UNCHANGED (exempt from
    // `retarget_slot_violation`'s check) and position 2 is CHANGED to a
    // member of the flat union — pre-fix this is accepted via the per-index
    // union fallback and then written unconditionally (the compat
    // `slots.is_empty()` branch), corrupting a 2-target node with a 3-target
    // list. Post-fix the whole submission is rejected before any position is
    // individually checked.
    let mut probe = GameRunner::from_state(runner.state().clone());
    let result = probe.act(GameAction::RetargetSpell {
        new_targets: vec![
            TargetRef::Player(P1),
            TargetRef::Player(P0),
            TargetRef::Player(P0),
        ],
    });
    assert!(
        result.is_err(),
        "MED-1: a submission whose effective_pools is shorter than current_targets \
         must be rejected outright, got {result:?}"
    );
}
