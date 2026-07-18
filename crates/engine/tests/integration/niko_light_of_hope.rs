//! Niko, Light of Hope (DSK) — middle clause of the {2},{T} activated ability:
//! "Shards you control become copies of it until the next end step."
//!
//! Mass become-copy (`Effect::BecomeCopy.recipient`) with a turn-agnostic
//! end-step expiry (`PlayerScope::AnyTurn`). Runtime tests drive the real
//! activation pipeline (GameScenario + GameRunner::activate(..).resolve()) and
//! assert measured state deltas, never AST-internal flags.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, ControllerRef, Duration, Effect, PlayerScope, TargetFilter, TypeFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::CardId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::ObjectId;

/// Niko's activated ability, verbatim (DSK). Only the MIDDLE clause is the gap
/// this change closes; exile + return already parsed.
const NIKO_ABILITY: &str = "{2}, {T}: Exile target nonlegendary creature you control. \
     Shards you control become copies of it until the next end step. \
     Return it to the battlefield under its owner's control at the beginning of the next end step.";

/// Shifting Woodland (single-subject become-copy) — the byte-identity regression
/// card for the `recipient` field.
const SHIFTING_WOODLAND_ORACLE: &str = concat!(
    "This land enters tapped unless you control a Forest.\n",
    "{T}: Add {G}.\n",
    "Delirium — {2}{G}{G}: This land becomes a copy of target permanent card in your graveyard until end of turn. ",
    "Activate only if there are four or more card types among cards in your graveyard."
);

/// Walk a chained `AbilityDefinition`, collecting one effect per node
/// (parent then each nested `sub_ability`).
fn flatten_effects(def: &AbilityDefinition) -> Vec<&Effect> {
    let mut out = Vec::new();
    let mut node = Some(def);
    while let Some(d) = node {
        out.push(&*d.effect);
        node = d.sub_ability.as_deref();
    }
    out
}

fn parse_niko() -> engine::parser::oracle::ParsedAbilities {
    parse_oracle_text(
        NIKO_ABILITY,
        "Niko, Light of Hope",
        &[],
        &["Legendary".into(), "Creature".into()],
        &["Human".into(), "Wizard".into()],
    )
}

/// Extract the (single) `BecomeCopy` effect from Niko's parsed activated ability.
fn niko_become_copy(parsed: &engine::parser::oracle::ParsedAbilities) -> Effect {
    let ability = parsed
        .abilities
        .iter()
        .find(|a| {
            flatten_effects(a)
                .iter()
                .any(|e| matches!(e, Effect::BecomeCopy { .. }))
        })
        .expect("Niko's activated ability must contain a BecomeCopy");
    flatten_effects(ability)
        .into_iter()
        .find(|e| matches!(e, Effect::BecomeCopy { .. }))
        .cloned()
        .expect("BecomeCopy node")
}

/// Add a colorless Shard enchantment (subtype "Shard") controlled by P0. Pure
/// enchantment (no P/T) so it survives on the battlefield until it copies a
/// creature; mirrors Niko's real Shard tokens.
fn add_shard(scenario: &mut GameScenario, name: &str) -> ObjectId {
    let mut b = scenario.add_creature(P0, name, 0, 0);
    b.as_enchantment().with_subtypes(vec!["Shard"]);
    b.id()
}

fn add_niko(scenario: &mut GameScenario) -> ObjectId {
    let mut b = scenario.add_creature(P0, "Niko, Light of Hope", 3, 4);
    b.as_legendary();
    b.from_oracle_text(NIKO_ABILITY);
    b.id()
}

/// Two white mana — enough for the {2} generic activation cost.
fn fund_two(scenario: &mut GameScenario) {
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]); 2],
    );
}

/// Advance the active player's turn to its end step, answering combat's
/// turn-based actions with "no attackers/blockers" so a player who controls
/// creatures still reaches the End step (plain `advance_to_end_step` halts at
/// the DeclareAttackers prompt).
fn drive_to_end_step(runner: &mut GameRunner) {
    runner.advance_to_end_step();
    for _ in 0..24 {
        if runner.state().phase == Phase::End {
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => {
                let _ = runner.declare_attackers(&[]);
            }
            WaitingFor::DeclareBlockers { .. } => {
                let _ = runner.declare_blockers(&[]);
            }
            WaitingFor::Priority { .. } => {
                let _ = runner.act(GameAction::PassPriority);
            }
            _ => break,
        }
    }
}

// ── (a) Parser shape ──────────────────────────────────────────────────────
//
// Niko's activated ability lowers to a 3-effect chain: exile → BecomeCopy →
// return delayed trigger, with the middle clause fully supported (no
// `Effect::Unimplemented`). Reach-guard: the exile + return clauses are still
// present/unchanged, so the new plural arm did not steal or drop them.
#[test]
fn a_parser_lowers_middle_clause_between_exile_and_return() {
    let parsed = parse_niko();
    let ability = parsed
        .abilities
        .iter()
        .find(|a| {
            flatten_effects(a)
                .iter()
                .any(|e| matches!(e, Effect::BecomeCopy { .. }))
        })
        .expect("activated ability with BecomeCopy");
    let chain = flatten_effects(ability);
    assert_eq!(chain.len(), 3, "exile → become-copy → return: {chain:#?}");

    // Reach-guard #1: clause 0 is still the exile (ChangeZone → Exile).
    match chain[0] {
        Effect::ChangeZone { destination, .. } => {
            assert_eq!(*destination, Zone::Exile, "clause 0 must exile the donor");
        }
        other => panic!("clause 0 expected exile ChangeZone, got {other:#?}"),
    }

    // The middle clause — the gap this change closes.
    match chain[1] {
        Effect::BecomeCopy {
            target,
            recipient,
            duration,
            ..
        } => {
            assert_eq!(
                *target,
                TargetFilter::ParentTarget,
                "donor = the exiled 'it'"
            );
            match recipient {
                TargetFilter::Typed(tf) => {
                    assert!(
                        tf.type_filters
                            .contains(&TypeFilter::Subtype("Shard".to_string())),
                        "recipient must be Shard-typed: {tf:#?}"
                    );
                    assert_eq!(
                        tf.controller,
                        Some(ControllerRef::You),
                        "recipient = Shards YOU control"
                    );
                }
                other => panic!("recipient must be a typed group, got {other:#?}"),
            }
            assert_eq!(
                *duration,
                Some(Duration::UntilNextStepOf {
                    step: Phase::End,
                    player: PlayerScope::AnyTurn,
                }),
                "turn-agnostic 'until the next end step'"
            );
        }
        other => panic!("clause 1 expected BecomeCopy, got {other:#?}"),
    }

    // Reach-guard #2: clause 2 is still the return delayed trigger.
    assert!(
        matches!(chain[2], Effect::CreateDelayedTrigger { .. }),
        "clause 2 must be the return delayed trigger, got {:#?}",
        chain[2]
    );

    // The gap is closed: no Unimplemented anywhere in the chain.
    for e in &chain {
        assert!(
            !matches!(e, Effect::Unimplemented { .. }),
            "no clause may lower to Unimplemented: {e:#?}"
        );
    }
}

// ── (b) Locked recipient set (CR 611.2c) ──────────────────────────────────
//
// A pre-existing Shard becomes a copy of the exiled creature (the ParentTarget
// donor); a bystander creature is untouched; and a Shard that ENTERS AFTER
// resolution is NOT retroactively swept into the copy set.
#[test]
fn b_locked_recipient_set_cr_611_2c() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    fund_two(&mut scenario);
    let niko = add_niko(&mut scenario);
    let shard1 = add_shard(&mut scenario, "Shard");
    let donor_a = {
        let mut b = scenario.add_creature(P0, "Ogre Warrior", 5, 5);
        b.with_subtypes(vec!["Ogre"]);
        b.id()
    };
    let donor_b = {
        let mut b = scenario.add_creature(P0, "Grizzly Bear", 2, 2);
        b.with_subtypes(vec!["Bear"]);
        b.id()
    };
    let mut runner = scenario.build();

    let outcome = runner.activate(niko, 0).target_object(donor_a).resolve();

    // Reach-guard: the pre-existing Shard DID become a copy of donor A.
    let s1 = &outcome.state().objects[&shard1];
    assert_eq!(s1.name, "Ogre Warrior", "pre-existing Shard copies donor A");
    assert_eq!(s1.power, Some(5));
    assert_eq!(s1.toughness, Some(5));

    // Bystander B (not targeted) is untouched.
    let b = &outcome.state().objects[&donor_b];
    assert_eq!(b.name, "Grizzly Bear");
    assert_eq!(b.power, Some(2));

    // Donor A was exiled (keep-green exile clause).
    assert_eq!(outcome.zone_of(donor_a), Zone::Exile);

    // CR 611.2c: a Shard that enters AFTER the effect began is not part of the
    // locked set — it stays a plain Shard.
    let shard2 = create_object(
        runner.state_mut(),
        CardId(9999),
        P0,
        "Shard".to_string(),
        Zone::Battlefield,
    );
    {
        let o = runner.state_mut().objects.get_mut(&shard2).unwrap();
        o.card_types.subtypes.push("Shard".to_string());
        o.base_card_types = o.card_types.clone();
    }
    evaluate_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&shard2].name,
        "Shard",
        "a Shard entering after resolution must stay a plain Shard (locked set)"
    );
}

// ── (d→F) Donor-≠-Niko discriminator ──────────────────────────────────────
//
// The copy is of the exiled DONOR, not of Niko. Fails if someone implemented
// the wrong "copy Niko" premise: the Shard must be the donor's 5/5 Ogre, never
// Niko's 3/4 Human Wizard.
#[test]
fn d_copies_the_donor_not_niko() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    fund_two(&mut scenario);
    let niko = add_niko(&mut scenario);
    let shard = add_shard(&mut scenario, "Shard");
    let donor = {
        let mut b = scenario.add_creature(P0, "Ogre Warrior", 5, 5);
        b.with_subtypes(vec!["Ogre"]);
        b.id()
    };
    let mut runner = scenario.build();

    let outcome = runner.activate(niko, 0).target_object(donor).resolve();

    let copy = &outcome.state().objects[&shard];
    // Matches the DONOR.
    assert_eq!(copy.name, "Ogre Warrior");
    assert_eq!(copy.power, Some(5));
    assert_eq!(copy.toughness, Some(5));
    assert!(copy.card_types.subtypes.iter().any(|s| s == "Ogre"));
    // Explicitly NOT Niko's identity.
    assert_ne!(copy.name, "Niko, Light of Hope");
    assert_ne!(copy.power, Some(3));
    assert_ne!(copy.toughness, Some(4));
}

// ── (c) Opponent-turn co-fire — the AnyTurn discriminator ─────────────────
//
// Activated at instant speed on an OPPONENT's turn: at THAT opponent's end step
// the Shards revert to plain Shards AND the exiled creature returns — the same
// end step, not a turn later. Under a `PlayerScope::Controller` scope the copies
// would survive the opponent's end step (pruned only on the controller's own),
// persisting a rotation past the return; this test fails in that world.
#[test]
fn c_opponent_turn_co_fire_reverts_and_returns() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    fund_two(&mut scenario);
    let niko = add_niko(&mut scenario);
    let shard = add_shard(&mut scenario, "Shard");
    let donor = {
        let mut b = scenario.add_creature(P0, "Ogre Warrior", 5, 5);
        b.with_subtypes(vec!["Ogre"]);
        b.id()
    };
    let mut runner = scenario.build();
    // Make it the OPPONENT's (P1's) turn, with Niko's controller (P0) holding
    // priority — a legal instant-speed activation window.
    {
        let st = runner.state_mut();
        st.active_player = P1;
        st.priority_player = P0;
        st.waiting_for = WaitingFor::Priority { player: P0 };
    }

    let outcome = runner.activate(niko, 0).target_object(donor).resolve();
    // Sanity: the Shard IS a copy right after resolution (before any end step).
    assert_eq!(
        outcome.state().objects[&shard].name,
        "Ogre Warrior",
        "copy active while the effect persists"
    );
    assert_eq!(outcome.zone_of(donor), Zone::Exile);

    // Advance to the opponent's (P1's) end step. The end-step prune runs at End
    // entry (before end-step triggers), so the turn-agnostic copy expires here.
    runner.advance_to_end_step();
    assert_eq!(runner.state().phase, Phase::End);
    assert_eq!(
        runner.state().active_player,
        P1,
        "we are at the OPPONENT's end step"
    );
    evaluate_layers(runner.state_mut());

    // Discriminator: the Shard reverted at the OPPONENT's end step.
    assert_eq!(
        runner.state().objects[&shard].name,
        "Shard",
        "turn-agnostic copy must revert at the FIRST (opponent's) end step"
    );

    // Co-fire: the return delayed trigger (AtNextPhase{End}) resolves at the same
    // end step, so the exiled creature is back on the battlefield.
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&donor].zone,
        Zone::Battlefield,
        "the exiled creature returns at the same (opponent's) end step"
    );
}

// Positive reach-guard for (c): on the controller's OWN turn the copy co-fires
// and reverts at the controller's end step — proves the revert mechanism fires
// at all (both AnyTurn and Controller pass this; it is the reach-guard).
#[test]
fn c_reach_guard_your_turn_reverts_at_own_end_step() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    fund_two(&mut scenario);
    let niko = add_niko(&mut scenario);
    let shard = add_shard(&mut scenario, "Shard");
    let donor = {
        let mut b = scenario.add_creature(P0, "Ogre Warrior", 5, 5);
        b.with_subtypes(vec!["Ogre"]);
        b.id()
    };
    let mut runner = scenario.build();

    let outcome = runner.activate(niko, 0).target_object(donor).resolve();
    assert_eq!(outcome.state().objects[&shard].name, "Ogre Warrior");

    drive_to_end_step(&mut runner);
    assert_eq!(runner.state().phase, Phase::End);
    assert_eq!(runner.state().active_player, P0);
    evaluate_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&shard].name,
        "Shard",
        "copy reverts at the controller's own next end step"
    );
}

// ── (e) Recipient is not a target ─────────────────────────────────────────
//
// `Effect::target_filter()` surfaces only the donor (`ParentTarget`); the
// recipient set is never a target slot. Fails if someone wired `recipient`
// through the targeting machinery.
#[test]
fn e_recipient_is_not_a_target() {
    let parsed = parse_niko();
    let become_copy = niko_become_copy(&parsed);
    assert_eq!(
        become_copy.target_filter(),
        Some(&TargetFilter::ParentTarget),
        "only the donor is a target; the recipient set is not"
    );
    // Guard: the recipient really is the distinct Shard group (so the assertion
    // above is not vacuously matching a SelfRef donor).
    match &become_copy {
        Effect::BecomeCopy { recipient, .. } => assert!(
            matches!(recipient, TargetFilter::Typed(_)),
            "recipient is the typed Shard group, distinct from the donor"
        ),
        _ => unreachable!(),
    }
}

// ── (f) Single-subject regression — byte-identical card-data ──────────────
//
// A real existing single-subject BecomeCopy card (Shifting Woodland) keeps
// `recipient == SelfRef`, and SelfRef is skip-serialized, so its serialized
// effect omits `recipient` entirely — existing card-data is byte-identical.
#[test]
fn f_single_subject_recipient_selfref_omitted_from_json() {
    let parsed = parse_oracle_text(
        SHIFTING_WOODLAND_ORACLE,
        "Shifting Woodland",
        &[],
        &["Land".to_string()],
        &[],
    );
    let copy = parsed
        .abilities
        .iter()
        .find(|a| matches!(&*a.effect, Effect::BecomeCopy { .. }))
        .expect("Shifting Woodland has a Delirium BecomeCopy");

    match &*copy.effect {
        Effect::BecomeCopy { recipient, .. } => {
            assert_eq!(
                *recipient,
                TargetFilter::SelfRef,
                "single-subject copy recipient defaults to SelfRef"
            );
        }
        other => panic!("expected BecomeCopy, got {other:?}"),
    }

    let json = serde_json::to_string(&*copy.effect).expect("serialize effect");
    assert!(
        !json.contains("recipient"),
        "SelfRef recipient must be skip-serialized (byte-identical card-data): {json}"
    );
}

/// Collect every `BecomeCopy` effect across a card's abilities and triggers.
fn all_become_copies(parsed: &engine::parser::oracle::ParsedAbilities) -> Vec<Effect> {
    let mut out = Vec::new();
    let mut scan = |def: &AbilityDefinition| {
        for e in flatten_effects(def) {
            if matches!(e, Effect::BecomeCopy { .. }) {
                out.push(e.clone());
            }
        }
    };
    for a in &parsed.abilities {
        scan(a);
    }
    for trg in &parsed.triggers {
        if let Some(exec) = trg.execute.as_deref() {
            scan(exec);
        }
    }
    out
}

// ── Build-for-the-class: the plural arm covers a category, not just Niko ──
//
// The two other DB cards whose Oracle text contains "become copies of" — both
// Unimplemented before this change — now lower to a `BecomeCopy` with the
// correct typed recipient group (Shapeshifters / other creatures). This is the
// measured coverage GAIN behind REGRESSED=0: the only cards the new plural
// "copies of" arm touches are the three that gain coverage.
#[test]
fn plural_arm_covers_recipient_class_not_just_niko() {
    // Absorb Identity: "You may have Shapeshifters you control become copies of
    // that creature until end of turn."
    let absorb = parse_oracle_text(
        "Return target creature to its owner's hand. You may have Shapeshifters you control become copies of that creature until end of turn.",
        "Absorb Identity",
        &[],
        &["Creature".to_string()],
        &["Shapeshifter".to_string(), "Rogue".to_string()],
    );
    let ac = all_become_copies(&absorb);
    assert_eq!(
        ac.len(),
        1,
        "Absorb Identity's clause lowers to one BecomeCopy"
    );
    match &ac[0] {
        Effect::BecomeCopy {
            recipient,
            duration,
            ..
        } => {
            match recipient {
                TargetFilter::Typed(tf) => assert!(
                    tf.type_filters
                        .contains(&TypeFilter::Subtype("Shapeshifter".to_string()))
                        && tf.controller == Some(ControllerRef::You),
                    "recipient = Shapeshifters you control: {tf:#?}"
                ),
                other => panic!("recipient must be a typed group: {other:#?}"),
            }
            assert_eq!(*duration, Some(Duration::UntilEndOfTurn));
        }
        other => panic!("expected BecomeCopy, got {other:#?}"),
    }

    // Deceiver of Form: "you may have creatures you control other than this
    // creature become copies of that card until end of turn."
    let deceiver = parse_oracle_text(
        "At the beginning of combat on your turn, reveal the top card of your library. If a creature card is revealed this way, you may have creatures you control other than this creature become copies of that card until end of turn. You may put that card on the bottom of your library.",
        "Deceiver of Form",
        &[],
        &["Creature".to_string()],
        &["Eldrazi".to_string()],
    );
    let dc = all_become_copies(&deceiver);
    assert_eq!(
        dc.len(),
        1,
        "Deceiver of Form's clause lowers to one BecomeCopy"
    );
    match &dc[0] {
        Effect::BecomeCopy { recipient, .. } => match recipient {
            TargetFilter::Typed(tf) => assert!(
                tf.type_filters.contains(&TypeFilter::Creature)
                    && tf.controller == Some(ControllerRef::You),
                "recipient = other creatures you control: {tf:#?}"
            ),
            other => panic!("recipient must be a typed group: {other:#?}"),
        },
        other => panic!("expected BecomeCopy, got {other:#?}"),
    }
}
