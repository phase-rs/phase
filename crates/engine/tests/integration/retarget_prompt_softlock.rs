//! CR 115.7 — a parked retarget prompt must always be answerable.
//!
//! Three propositions, all reached through production entry points:
//!
//! 1. the retarget pool comes from the stack entry's OWN targeting authority,
//!    not from an Aura host's enchant filter (an Aura's triggered ability is a
//!    different object on the stack than the Aura spell, CR 303.4a vs CR 115.7);
//! 2. an empty pool resolves as a CR 115.7a no-change instead of parking a
//!    prompt nothing can discharge;
//! 3. every submission the AI proposes for a parked prompt is accepted by the
//!    reducer, because both consult the same per-slot authority
//!    (`retarget_slot_violation`).

use engine::ai_support::candidate_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    mana_multi_role, ControllerRef, Effect, EffectKind, ManaProduction, ManaTargetRole,
    QuantityExpr, ResolvedAbility, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastingVariant, GameState, RetargetScope, StackEntry, StackEntryKind, WaitingFor,
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

/// Rows 1a + 1b — CR 115.7 + CR 303.4a: an Aura's *triggered* ability declares
/// its own target ("any other target"), so its retarget pool must come from that
/// effect's filter. Keying the CR 303.4a Aura substitution on the source object
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
        "CR 115.7: the pool must come from the TRIGGER's own target filter, which \
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
    // illegal after the spell was cast. Unit 1 does not eliminate this state.
    runner.state_mut().waiting_for = WaitingFor::RetargetChoice {
        player: P0,
        stack_entry_index: 0,
        scope: RetargetScope::Single,
        current_targets: vec![TargetRef::Object(victim)],
        legal_new_targets: vec![TargetRef::Object(alternative)],
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
/// ENUMERATION SHAPE for a node OUTSIDE the per-slot admitted class
/// (`mana_multi_role` is `None`, so `retarget_slot_violation` early-outs and
/// nothing filters the enumeration). It does NOT claim every enumerated
/// submission is CR-legal, and it must not be cited as evidence that it is:
///
///   * `expected` contains `[b, b]` and `[a, a]`. The generator substitutes one
///     slot at a time from the FLAT pool with no distinctness check, so a pool
///     member already held by another slot produces a duplicate. Whether a
///     duplicate is legal is a per-INSTANCE question (CR 601.2c: the same target
///     can't be chosen twice for any ONE instance of "target", but may be chosen
///     once for EACH instance if the spell uses "target" in several places), and
///     the flat pool carries no instance structure to answer it with. That is
///     the same pre-existing flat-pool gap `retarget_slot_violation`'s SCOPE
///     note records for `Attach` / `MoveCounters` / `Fight`. These two entries
///     are therefore recorded as OBSERVED CURRENT BEHAVIOUR, deliberately not
///     endorsed.
///   * The fixture is synthetic in the same direction: Lightning Bolt's "any
///     target" is ONE instance of "target", so a genuine Bolt cannot hold two
///     targets at all. The two-target list is fabricated to isolate enumeration,
///     which is why per-slot legality is out of this row's reach rather than
///     merely unasserted.
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
    let ability = ResolvedAbility::new(
        parsed.abilities[0].effect.as_ref().clone(),
        vec![TargetRef::Object(a), TargetRef::Object(b)],
        source,
        P0,
    );
    assert!(
        mana_multi_role(&ability.effect).is_none(),
        "fixture guard: this row's node is outside the per-slot admitted class"
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
    runner.state_mut().waiting_for = WaitingFor::RetargetChoice {
        player: P0,
        stack_entry_index: 0,
        scope: RetargetScope::All,
        current_targets: current_targets.clone(),
        legal_new_targets: vec![
            TargetRef::Object(a),
            TargetRef::Object(b),
            TargetRef::Object(c),
        ],
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
        vec![TargetRef::Object(a), TargetRef::Object(a)],
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

/// Both structural reach-guards for the synthetic multi-role rows. Without the
/// first, `retarget_actions`' `is_none_or` returns `true` and every candidate
/// passes unfiltered while `apply_retarget`'s per-slot stage is skipped
/// entirely; without the second, the fixture can silently degrade into a node
/// `mana_multi_role` rejects. Either degradation makes these rows vacuous.
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
/// Applying it assigns `ability.targets = [P1]` and TRUNCATES the count-source
/// slot. Both subrules that reach this arm say otherwise, and they prescribe
/// DIFFERENT remedies — `RetargetScope::Single` is produced by two oracle
/// templates (`try_parse_change_targets`, parser/oracle_effect/mod.rs), so the
/// deferred fix must DISPATCH ON THE TEMPLATE rather than apply CR 115.7b's
/// remedy uniformly:
///   - "change a target of " → CR 115.7b: "the process described in rule 115.7a
///     is followed, except that only one of those targets may be changed (rather
///     than all of them or none of them)". Remedy: one slot changes, every other
///     declared target stays in place.
///   - "change the target of " → CR 115.7a, which ends: "If all the targets
///     aren't changed to other legal targets, none of them are changed." Remedy
///     for a multi-target entry: ALL-OR-NONE, not one-changes-rest-stay. This is
///     Bolt Bend's and Redirect's wording.
///
/// Under neither remedy may an undisturbed slot simply be dropped. The length-1
/// acceptance is recorded here as OBSERVED CURRENT BEHAVIOUR, deliberately NOT
/// endorsed:
///
///   DEFERRED(out-of-run): interactive Single-scope retarget collapses
///   multi-target lists (CR 115.7a / CR 115.7b) — upstream cause filter.rs
///   FilterProp::HasSingleTarget is permissive with no resolution-time
///   validation; fix needs filter.rs + interaction.rs, both outside phase 1's
///   frozen scope.
///
/// The honest behavioural delta this phase knowingly takes: at BASE the AI froze
/// on this class — the generator's sole proposal was rejected, so no actor could
/// discharge the prompt. AFTER this change it PROGRESSES and TRUNCATES. Progress
/// with a corrupted target list is the trade; it is not a claim that the result
/// is rules-correct. A genuinely CR-115.7a / CR-115.7b-correct submission is not
/// expressible against today's reducer contract at all, which is why the
/// deferral above names two paths outside this run's frozen scope rather than a
/// local repair.
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
    runner.state_mut().waiting_for = WaitingFor::RetargetChoice {
        player: P0,
        stack_entry_index: 0,
        scope: RetargetScope::Single,
        current_targets,
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
    // rules-correctness. Applying this length-1 submission to a length-2 target
    // list truncates the count-source slot, contrary to CR 115.7a / CR 115.7b
    // alike — neither remedy permits dropping an undisturbed slot. See this
    // row's SCOPE note and the DEFERRED(out-of-run) entry it carries; that
    // truncation is why this loop deliberately asserts only that the submission
    // is accepted, and never asserts the resulting `ability.targets`.
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
