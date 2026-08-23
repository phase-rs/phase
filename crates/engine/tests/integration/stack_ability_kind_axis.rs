//! CR 113.3b / CR 113.3c + CR 115.1: the ability-kind spelling in a stack-object
//! target phrase narrows the legal target set. Regression witness for the parser
//! dropping that axis on the copy path (Mister Fantastic / Strionic Resonator /
//! Kirol, Attentive First-Year) and the change-targets path (Reroute).
//!
//! Before the fix, three consumers of the stack-object grammar each kept their
//! own private ability-kind spelling list and hardcoded a kindless
//! `TargetFilter::StackAbility`, so:
//!   * "Copy target triggered ability you control" could copy an ACTIVATED
//!     ability, and
//!   * Reroute ("Change the target of target activated ability with a single
//!     target") could retarget a TRIGGERED ability.
//!
//! The engine's two legality authorities (`game::targeting` at announce time,
//! CR 601.2c; `game::filter` at resolution recheck, CR 608.2b) already honored
//! `kind` — the parser simply never set it. Both now classify through the single
//! authority `StackEntryKind::matches_stack_ability_kind`, which also closed two
//! divergences the narrowing exposed: the kind gate classified `KeywordAction`
//! (equip / crew / saddle / station) as NEITHER kind despite CR 702.6a /
//! CR 702.122a / CR 702.171a / CR 702.184a defining all four as ACTIVATED
//! abilities, and the recheck gate dropped `KeywordAction` from the stack-ability
//! set entirely while the announce gate admitted it.
//!
//! These tests drive the real engine: `find_legal_targets` (the exact path
//! `apply` uses at target declaration) and the `apply()` cast/activation pipeline
//! via `GameRunner::act`. All three ability-bearing `StackEntryKind` variants get
//! fixtures — see `AbilityEntry`.

use engine::game::casting::activated_ability_definitions;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::game::targeting::find_legal_targets;
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityKind, CopyRetargetPermission, Effect, KeywordAction, QuantityExpr, ResolvedAbility,
    StackAbilityKind, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{
    CastPaymentMode, GameState, StackEntry, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::PlayerId;

use crate::support::shared_card_db;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

/// Verbatim Oracle text (Scryfall / the card-data export). A paraphrase can take
/// a different parser branch and go green while the real card stays broken.
const MISTER_FANTASTIC_ORACLE: &str = "Reach, vigilance\n\
     At the beginning of combat on your turn, if you've cast a noncreature spell this turn, draw a card.\n\
     {R}{G}{W}{U}, {T}: Copy target triggered ability you control twice. You may choose new targets for the copies.";

/// Verbatim, including the reminder text and the second line.
const REROUTE_ORACLE: &str = "Change the target of target activated ability with a single \
     target. (Mana abilities can't be targeted.)\nDraw a card.";

/// Parse a single imperative clause and return its `Effect`.
fn effect_of(text: &str) -> Effect {
    (*parse_effect_chain(text, AbilityKind::Spell).effect).clone()
}

fn copy_target_of(text: &str) -> TargetFilter {
    match effect_of(text) {
        Effect::CopySpell { target, .. } => target,
        other => panic!("expected CopySpell, got {other:?}"),
    }
}

fn change_targets_target_of(text: &str) -> TargetFilter {
    match effect_of(text) {
        Effect::ChangeTargets { target, .. } => target,
        other => panic!("expected ChangeTargets, got {other:?}"),
    }
}

/// Which ability-bearing `StackEntryKind` variant a fixture stages.
///
/// All three are production-reachable, so enumerating only the two
/// `ResolvedAbility`-backed ones would leave the arm that real
/// equip/crew/saddle/station data takes without any fixture — every fixture
/// degenerate in the same way.
#[derive(Clone, Copy)]
enum AbilityEntry {
    /// `StackEntryKind::ActivatedAbility` carrying `targets` dummy targets.
    Activated { targets: usize },
    /// `StackEntryKind::TriggeredAbility` carrying `targets` dummy targets.
    Triggered { targets: usize },
    /// CR 702.6a: equip is an ACTIVATED ability. The engine stores it as a
    /// typed `KeywordAction` payload rather than a `ResolvedAbility`, so the
    /// entry exposes NO `targets` list — which is why it takes no `targets`
    /// parameter and why the `HasSingleTarget` arity axis cannot see its target
    /// (see `reroute_*` below). Reached in production from `GameAction::Equip`
    /// via `push_keyword_action`.
    KeywordEquip,
}

/// Push an ability stack entry and return its entry id. Mirrors the real
/// trigger/activation/keyword-action push path: the entry id is allocated fresh
/// and is NOT inserted into `state.objects`.
fn push_ability_entry(
    state: &mut GameState,
    source: ObjectId,
    controller: PlayerId,
    entry: AbilityEntry,
) -> ObjectId {
    let entry_id = ObjectId(state.next_object_id);
    state.next_object_id += 1;
    let draw = |target_count: usize| {
        Box::new(ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            (0..target_count)
                .map(|i| TargetRef::Object(ObjectId(9_000 + i as u64)))
                .collect(),
            source,
            controller,
        ))
    };
    let kind = match entry {
        AbilityEntry::Triggered { targets } => StackEntryKind::TriggeredAbility {
            source_id: source,
            ability: draw(targets),
            condition: None,
            trigger_event: None,
            description: None,
            source_name: String::new(),
            subject_match_count: None,
            die_result: None,
            provenance: None,
        },
        AbilityEntry::Activated { targets } => StackEntryKind::ActivatedAbility {
            source_id: source,
            ability: draw(targets),
        },
        AbilityEntry::KeywordEquip => StackEntryKind::KeywordAction {
            action: KeywordAction::Equip {
                equipment_id: source,
                // Never dereferenced: these tests stop at target legality and
                // never resolve the equip.
                target_creature_id: ObjectId(9_000),
            },
        },
    };
    state.stack.push_back(StackEntry {
        id: entry_id,
        source_id: source,
        controller,
        kind,
    });
    entry_id
}

fn battlefield_source(state: &mut GameState, controller: PlayerId, name: &str) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    create_object(
        state,
        card_id,
        controller,
        name.to_string(),
        Zone::Battlefield,
    )
}

/// CR 113.3b + CR 115.1 + CR 601.2c — copy path. "Copy target triggered ability
/// you control" must NOT offer an activated ability at target declaration, even
/// when that activated ability is controlled by the copying player (so the
/// controller axis matches and `kind` is the SOLE discriminator).
#[test]
fn copy_target_triggered_ability_rejects_activated_ability() {
    let filter = copy_target_of("Copy target triggered ability you control");
    assert_eq!(
        filter,
        TargetFilter::StackAbility {
            controller: Some(engine::types::ability::ControllerRef::You),
            tag: None,
            kind: Some(engine::types::ability::StackAbilityKind::Triggered),
        },
        "the triggered-only spelling must narrow kind (FLIPS to a kindless leg on revert)"
    );

    let mut state = GameState::new_two_player(42);
    let source = battlefield_source(&mut state, P0, "Ability Source");
    // All four are stack abilities; only kind and controller differ.
    let p0_activated = push_ability_entry(
        &mut state,
        source,
        P0,
        AbilityEntry::Activated { targets: 1 },
    );
    let p0_triggered = push_ability_entry(
        &mut state,
        source,
        P0,
        AbilityEntry::Triggered { targets: 1 },
    );
    let p1_triggered = push_ability_entry(
        &mut state,
        source,
        P1,
        AbilityEntry::Triggered { targets: 1 },
    );
    let p0_equip = push_ability_entry(&mut state, source, P0, AbilityEntry::KeywordEquip);

    let copier = ObjectId(1000);
    let legal = find_legal_targets(&state, &filter, P0, copier);
    let is_legal = |id: ObjectId| legal.contains(&TargetRef::Object(id));

    // Positive reach-guard FIRST: without it, "not legal" could pass on an
    // empty legal set for any unrelated reason.
    assert!(
        is_legal(p0_triggered),
        "a triggered ability its controller controls IS a legal target (CR 601.2c)"
    );
    assert!(
        !is_legal(p0_activated),
        "an ACTIVATED ability must NOT be a legal target of a triggered-only copy \
         effect — it is P0-controlled, so kind is the only thing excluding it \
         (CR 113.3b). This is the bug: pre-fix it IS offered."
    );
    assert!(
        !is_legal(p1_triggered),
        "the controller axis must still fire — an opponent's triggered ability is \
         not 'you control' (CR 109.4)"
    );
    assert!(
        !is_legal(p0_equip),
        "CR 702.6a: an equip keyword action is an ACTIVATED ability, so a \
         triggered-only copy effect must not offer it either"
    );

    // Sibling guard: the combined spelling (Lithoform Engine, Vantress Visions,
    // …) must stay widened to both kinds — and to all three stack-entry kinds
    // that carry an ability, `KeywordAction` included (CR 113.3b).
    let both = copy_target_of("Copy target activated or triggered ability you control");
    let legal_both = find_legal_targets(&state, &both, P0, copier);
    assert!(
        legal_both.contains(&TargetRef::Object(p0_activated))
            && legal_both.contains(&TargetRef::Object(p0_triggered)),
        "a combined spelling must keep accepting BOTH kinds"
    );
    assert!(
        legal_both.contains(&TargetRef::Object(p0_equip)),
        "a kindless stack-ability filter must accept an equip keyword action \
         (CR 113.3b) — the announce gate always did, and the CR 608.2b recheck \
         now agrees"
    );
}

/// CR 702.6a / CR 702.122a / CR 702.171a / CR 702.184a + CR 113.3b: equip, crew,
/// saddle and station are ACTIVATED abilities, so the `kind: Activated`
/// narrowing this change makes authoritative must admit their stack entries.
/// This is the class of card the kind gate silently excluded: Squelch, Bind,
/// Rust, Interdict, Brown Ouphe, Ouphe Vandals, Azorius Guildmage and Voidmage
/// Husher all read "target activated ability" with no arity restriction.
#[test]
fn activated_narrowed_filter_admits_keyword_action_entry() {
    let mut state = GameState::new_two_player(42);
    let source = battlefield_source(&mut state, P0, "Equipment");
    let equip = push_ability_entry(&mut state, source, P0, AbilityEntry::KeywordEquip);
    let triggered = push_ability_entry(
        &mut state,
        source,
        P0,
        AbilityEntry::Triggered { targets: 1 },
    );

    let squelch = ObjectId(1000);
    let activated_filter = TargetFilter::StackAbility {
        controller: None,
        tag: None,
        kind: Some(StackAbilityKind::Activated),
    };
    let legal = find_legal_targets(&state, &activated_filter, P0, squelch);
    assert!(
        legal.contains(&TargetRef::Object(equip)),
        "CR 702.6a: 'counter target activated ability' must be able to name an \
         equip ability on the stack (CR 115.7a makes it a legal target). Pre-fix \
         the kind gate classified KeywordAction as neither kind and excluded it."
    );
    // Reach-guard on the other side of the axis: the gate is still discriminating,
    // not simply admitting everything.
    assert!(
        !legal.contains(&TargetRef::Object(triggered)),
        "a Triggered entry must still be excluded from an Activated-narrowed filter"
    );
}

/// CR 115.7a + CR 113.3b — change-targets path. Reroute's filter is
/// `And[StackAbility{Activated}, Typed{HasSingleTarget}]`; neither axis may mask
/// the other, so this stages a hostile fixture for each.
#[test]
fn reroute_activated_ability_rejects_triggered_ability() {
    let filter = change_targets_target_of(
        "Change the target of target activated ability with a single target",
    );
    let TargetFilter::And { ref filters } = filter else {
        panic!("expected the two-leg And filter, got {filter:?}");
    };
    assert_eq!(
        filters[0],
        TargetFilter::StackAbility {
            controller: None,
            tag: None,
            kind: Some(engine::types::ability::StackAbilityKind::Activated),
        },
        "Reroute must narrow to Activated (FLIPS to a kindless leg on revert)"
    );

    let mut state = GameState::new_two_player(42);
    let source = battlefield_source(&mut state, P0, "Retarget Victim");
    let activated_one = push_ability_entry(
        &mut state,
        source,
        P0,
        AbilityEntry::Activated { targets: 1 },
    );
    let activated_two = push_ability_entry(
        &mut state,
        source,
        P0,
        AbilityEntry::Activated { targets: 2 },
    );
    let triggered_one = push_ability_entry(
        &mut state,
        source,
        P0,
        AbilityEntry::Triggered { targets: 1 },
    );
    let triggered_two = push_ability_entry(
        &mut state,
        source,
        P0,
        AbilityEntry::Triggered { targets: 2 },
    );
    let equip = push_ability_entry(&mut state, source, P0, AbilityEntry::KeywordEquip);

    let reroute = ObjectId(1000);
    let legal = find_legal_targets(&state, &filter, P0, reroute);
    let is_legal = |id: ObjectId| legal.contains(&TargetRef::Object(id));

    assert!(
        is_legal(activated_one),
        "a single-target ACTIVATED ability is the one legal Reroute target \
         (CR 115.7a) — positive reach-guard for the three negatives below"
    );
    assert!(
        !is_legal(triggered_one),
        "a single-target TRIGGERED ability must be excluded by the KIND axis \
         (CR 113.3c). This is the bug: pre-fix it IS offered."
    );
    assert!(
        !is_legal(activated_two),
        "a two-target activated ability must be excluded by the ARITY axis \
         ('with a single target'), proving kind did not mask it"
    );
    assert!(
        !is_legal(triggered_two),
        "a two-target triggered ability is excluded on both axes"
    );

    // KNOWN RESIDUAL, pinned deliberately. The KIND axis now admits the equip
    // entry (CR 702.6a — proved directly by the bare-kind assertion below), but
    // the ARITY axis still rejects it: `FilterProp::HasSingleTarget` reads
    // `StackEntry::ability().targets`, and a `KeywordAction` entry carries a
    // typed payload with no `ResolvedAbility`, so its target is invisible there.
    //
    // Excluding it is the SAFE behavior today and must not be "fixed" in
    // isolation: `effects::change_targets::resolve` also reads `entry.ability()`
    // and no-ops when it is `None`, so admitting the equip entry would let
    // Reroute legally target it and then fail to change anything — a silent
    // no-op is worse than not offering it. Making equip retargetable per
    // CR 115.7a requires exposing the keyword action's target through the same
    // seam BOTH the arity filter and `change_targets` read.
    assert!(
        !is_legal(equip),
        "an equip entry is excluded from Reroute by the ARITY axis, not the kind \
         axis (see the bare-kind assertion below)"
    );
    let bare_activated = TargetFilter::StackAbility {
        controller: None,
        tag: None,
        kind: Some(StackAbilityKind::Activated),
    };
    assert!(
        find_legal_targets(&state, &bare_activated, P0, reroute)
            .contains(&TargetRef::Object(equip)),
        "CR 702.6a: with the arity leg removed, the KIND axis alone DOES admit \
         the equip entry — pinning that the exclusion above is arity-only"
    );

    assert_eq!(
        legal.len(),
        1,
        "exactly one legal target — the single-target activated ability"
    );
}

/// CR 601.2c through the production cast pipeline: the announce-time slot the
/// real `apply()` path builds for Reroute must exclude the triggered ability.
///
/// Stops at announcement deliberately: the claim under test is announce-time
/// legal-target enumeration, which is exactly the seam the parser fix changes.
/// (`Effect::ChangeTargets` resolution opens a retarget prompt the fluent
/// `SpellCast` driver does not answer, so `runner.cast(..).resolve()` is the
/// wrong instrument for a negative legality claim.)
#[test]
fn reroute_cast_pipeline_excludes_triggered_ability() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let reroute = scenario
        .add_spell_to_hand_from_oracle(P0, "Reroute", true, REROUTE_ORACLE)
        .id();
    // {1}{R}
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    // Stage TWO legal activated abilities alongside the illegal triggered one.
    // Two legal targets is what makes the prompt observable: with a single
    // legal target the engine auto-selects (CR 601.2c) and never surfaces a
    // slot, so a one-legal-target staging would assert nothing post-fix.
    // Staging legal targets also means the negative below cannot pass by the
    // cast being rejected outright.
    let source = battlefield_source(runner.state_mut(), P0, "Retarget Victim");
    let activated = push_ability_entry(
        runner.state_mut(),
        source,
        P0,
        AbilityEntry::Activated { targets: 1 },
    );
    let activated_other = push_ability_entry(
        runner.state_mut(),
        source,
        P0,
        AbilityEntry::Activated { targets: 1 },
    );
    let triggered = push_ability_entry(
        runner.state_mut(),
        source,
        P0,
        AbilityEntry::Triggered { targets: 1 },
    );

    let card_id = runner.state().objects[&reroute].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: reroute,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P0 may cast Reroute (instant, {1}{R} in pool, a legal target exists)");

    let WaitingFor::TargetSelection {
        ref target_slots, ..
    } = runner.state().waiting_for
    else {
        panic!(
            "casting Reroute must pause on target selection, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(target_slots.len(), 1, "Reroute declares exactly one target");
    let legal = &target_slots[0].legal_targets;

    assert!(
        legal.contains(&TargetRef::Object(activated))
            && legal.contains(&TargetRef::Object(activated_other)),
        "both single-target ACTIVATED abilities must be offered (CR 601.2c) — \
         positive reach-guard"
    );
    assert!(
        !legal.contains(&TargetRef::Object(triggered)),
        "the TRIGGERED ability must NOT be offered by the production cast \
         pipeline (CR 115.7a). Pre-fix it is."
    );

    // Complete the announcement and confirm the binding reached the stack entry.
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(activated)],
        })
        .expect("the activated ability is a legal Reroute target");
    let committed = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == reroute)
        .expect("Reroute must be on the stack after announcement");
    assert_eq!(
        committed.ability().map(|a| a.targets.clone()),
        Some(vec![TargetRef::Object(activated)]),
        "the announced Reroute must be bound to the ACTIVATED ability (CR 601.2c)"
    );
}

/// CR 707.10 — end-to-end on the real card. Mister Fantastic's own parsed
/// ability must bind a triggered ability and must never bind an activated one.
/// Also pins the two sibling bindings on the same ability (CR 707.10 "twice",
/// CR 707.10c "may choose new targets"), so the kind fix is shown not to have
/// perturbed them.
#[test]
fn mister_fantastic_activation_binds_only_triggered_ability() {
    let Some(db) = shared_card_db() else {
        return;
    };

    // Export-level guards (claim: the pipeline output is correct, not merely
    // self-consistent).
    let face = db
        .get_face_by_name("Mister Fantastic")
        .expect("Mister Fantastic must be in the card database");
    let copy_ability = face
        .abilities
        .iter()
        .find(|a| matches!(a.effect.as_ref(), Effect::CopySpell { .. }))
        .expect("Mister Fantastic must parse a CopySpell ability");
    let Effect::CopySpell {
        ref target,
        ref retarget,
        ..
    } = *copy_ability.effect
    else {
        unreachable!();
    };
    assert_eq!(
        *target,
        TargetFilter::StackAbility {
            controller: Some(engine::types::ability::ControllerRef::You),
            tag: None,
            kind: Some(engine::types::ability::StackAbilityKind::Triggered),
        },
        "the EXPORTED card must carry kind: Triggered"
    );
    // CR 707.10c: the retarget permission is a separate binding on the same
    // ability and must be untouched.
    assert_eq!(*retarget, CopyRetargetPermission::MayChooseNewTargets);
    // CR 707.10: "twice" — each copy is an independent copy.
    assert_eq!(
        copy_ability.repeat_for,
        Some(QuantityExpr::Fixed { value: 2 }),
        "the 'twice' quantifier must be untouched"
    );

    // Lithoform Engine is the combined-spelling control: it must stay kindless.
    let lithoform = db
        .get_face_by_name("Lithoform Engine")
        .expect("Lithoform Engine must be in the card database");
    let lithoform_ability_target = lithoform
        .abilities
        .iter()
        .find_map(|a| match a.effect.as_ref() {
            Effect::CopySpell {
                target: t @ TargetFilter::StackAbility { .. },
                ..
            } => Some(t.clone()),
            _ => None,
        })
        .expect("Lithoform Engine must have a stack-ability copy mode");
    assert_eq!(
        lithoform_ability_target,
        TargetFilter::StackAbility {
            controller: Some(engine::types::ability::ControllerRef::You),
            tag: None,
            kind: None,
        },
        "an 'activated or triggered ability' card must stay kindless"
    );

    // Phase 1 (positive reach-guard): with a triggered ability on the stack the
    // activation succeeds and binds it.
    let (mut runner, mister, copy_idx) = mister_fantastic_runner();
    let source = battlefield_source(runner.state_mut(), P0, "Ability Source");
    let trigger_id = push_ability_entry(
        runner.state_mut(),
        source,
        P0,
        AbilityEntry::Triggered { targets: 1 },
    );
    runner
        .act(GameAction::ActivateAbility {
            source_id: mister,
            ability_index: copy_idx,
        })
        .expect("Mister Fantastic's {R}{G}{W}{U}, {T} copy ability must be activatable");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(trigger_id)],
            })
            .expect("the triggered ability is the sole legal target");
    }
    assert_eq!(
        runner
            .state()
            .stack
            .back()
            .and_then(|entry| entry.ability())
            .map(|ability| ability.targets.clone()),
        Some(vec![TargetRef::Object(trigger_id)]),
        "the copy ability must bind the TRIGGERED ability (CR 601.2c)"
    );

    // Phase 2 (the negative): identical staging, but the only stack ability is
    // an ACTIVATED one.
    let (mut runner, mister, copy_idx) = mister_fantastic_runner();
    let source = battlefield_source(runner.state_mut(), P0, "Ability Source");
    let activated_id = push_ability_entry(
        runner.state_mut(),
        source,
        P0,
        AbilityEntry::Activated { targets: 1 },
    );

    let filter = TargetFilter::StackAbility {
        controller: Some(engine::types::ability::ControllerRef::You),
        tag: None,
        kind: Some(engine::types::ability::StackAbilityKind::Triggered),
    };
    assert!(
        !find_legal_targets(runner.state(), &filter, P0, mister)
            .contains(&TargetRef::Object(activated_id)),
        "the activated ability must not be a legal target of the copy filter"
    );

    // Phrased as a negative on the BINDING, so the test is correct whether the
    // engine rejects the activation outright or announces it with no target.
    let _ = runner.act(GameAction::ActivateAbility {
        source_id: mister,
        ability_index: copy_idx,
    });
    assert!(
        runner.state().stack.iter().all(|entry| entry
            .ability()
            .is_none_or(|a| !a.targets.contains(&TargetRef::Object(activated_id)))),
        "no stack entry may be bound to the ACTIVATED ability — pre-fix the copy \
         ability announces and binds it (CR 113.3b)"
    );
}

/// Mister Fantastic on the battlefield, untapped, with {R}{G}{W}{U} available.
/// Returns the runner, the permanent's id, and the index of its copy ability.
fn mister_fantastic_runner() -> (GameRunner, ObjectId, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // CR 302.6: `add_creature` models a permanent that entered on a prior turn,
    // so it is already NOT summoning-sick — the {T} cost is payable. Do not add
    // `.with_summoning_sickness()`.
    let mister = scenario
        .add_creature(P0, "Mister Fantastic", 2, 4)
        .from_oracle_text(MISTER_FANTASTIC_ORACLE)
        .id();
    scenario.with_mana_pool(
        P0,
        [
            ManaType::Red,
            ManaType::Green,
            ManaType::White,
            ManaType::Blue,
        ]
        .into_iter()
        .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect(),
    );
    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    let copy_idx = activated_ability_definitions(runner.state(), mister)
        .into_iter()
        .find(|(_, ability)| matches!(ability.effect.as_ref(), Effect::CopySpell { .. }))
        .expect("Mister Fantastic must expose an activated copy ability")
        .0;
    (runner, mister, copy_idx)
}
