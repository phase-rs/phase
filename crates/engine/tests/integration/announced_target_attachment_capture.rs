//! CR 303.4 + CR 301.5 + CR 601.2c + CR 608.2h: an announcement records what a
//! target WAS when the caster chose it — its public characteristics, the zone it
//! was legal in, the token bit it held, and the ATTACHMENT relation it carried.
//!
//! The relation is the one part of that record the generic snapshot cannot state:
//! `GameObject::snapshot_public_characteristics` has only `&self`, and
//! classifying an attached object as an Aura or an Equipment requires looking it
//! up in `GameState`. So it left `LKISnapshot::attachments` empty — "empty by
//! construction", which a reader cannot tell apart from "nothing was attached".
//!
//! Printed **Venomous Vines** is `{2}{G}{G}` for "Destroy target enchanted
//! permanent." (Scryfall, oracle text verified), whose declared slot filter is
//! `EnchantedBy` on a `Permanent` (CR 303.4b: the object an Aura is attached to is
//! enchanted). Parking that legal cast at manual mana payment and saving it is
//! the ordinary case: the recorded fact said "no Aura attached", `EnchantedBy`
//! answered a definite `false`, and
//! `GameState::reject_contradictory_announced_target_facts` refused the honest
//! save with `SnapshotContradictsDeclaredFilter`. A missing recorded fact must not
//! read as a failed predicate — but neither may the check be weakened so that a
//! CONTRADICTED recorded answer passes. The announcement capture therefore states
//! the relation, and the same guard still refuses a save whose recorded relation
//! contradicts the slot it was announced into.
//!
//! Witnesses:
//!
//! * honest — Venomous Vines announced at an Aura-enchanted creature, parked at
//!   manual mana payment, saved and restored, then paid and resolved: the
//!   enchanted permanent is destroyed and the Aura follows it (CR 704.5m);
//! * contradicted — the same save with the recorded attachment set emptied: the
//!   slot declares "enchanted permanent" and the record now says otherwise, so
//!   the save is refused.

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AttachmentKind, FilterProp, TargetRef, TypeFilter, TypedFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::{
    ActivationTargetFact, AnnouncedTargetFactConflict, CastPaymentMode, GameState,
    PersistedGameState, PersistedRestoreError, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim Oracle text (Scryfall): Venomous Vines, a sorcery.
const VENOMOUS_VINES: &str = "Destroy target enchanted permanent.";

/// Verbatim Oracle text (Scryfall): Pacifism, an Aura. Only its type line
/// ("Enchant creature" + the Aura subtype) matters to this file: it makes the
/// Bears "enchanted" (CR 303.4b).
const PACIFISM: &str = "Enchant creature\nEnchanted creature can't attack or block.";

fn pool(types: &[ManaType]) -> Vec<ManaUnit> {
    types
        .iter()
        .map(|t| ManaUnit::new(*t, ObjectId(0), false, vec![]))
        .collect()
}

/// CR 303.4: attach `attachment` to `host` the way the battlefield holds a real
/// attachment — both back-edges, so the Aura is what enchants the host and the
/// host is what carries the Aura.
fn attach(runner: &mut GameRunner, attachment: ObjectId, host: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&attachment)
        .unwrap()
        .attached_to = Some(AttachTarget::Object(host));
    let host_obj = runner.state_mut().objects.get_mut(&host).unwrap();
    if !host_obj.attachments.contains(&attachment) {
        host_obj.attachments.push(attachment);
    }
    runner.state_mut().layers_dirty.mark_full();
}

struct ParkedVines {
    runner: GameRunner,
    vines: ObjectId,
    host: ObjectId,
    aura: ObjectId,
}

/// CR 601.2c + CR 601.2h: announce Venomous Vines at the only legal target — P1's
/// Aura-enchanted creature — with `CastPaymentMode::Manual`, so the cast parks at
/// `WaitingFor::ManaPayment` holding its announced-target latch. That park is the
/// state a save taken during mana payment captures.
fn park_venomous_vines_at_manual_payment() -> ParkedVines {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let aura = scenario
        .add_creature(P0, "Pacifism", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["enchant"], PACIFISM)
        .id();
    let vines = scenario
        .add_spell_to_hand_from_oracle(P0, "Venomous Vines", false, VENOMOUS_VINES)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green, ManaCostShard::Green],
            generic: 2,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        pool(&[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Green,
            ManaType::Green,
        ]),
    );
    let mut runner = scenario.build();
    attach(&mut runner, aura, host);

    let card_id = runner.state().objects[&vines].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: vines,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the manual Venomous Vines cast is announced");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach guard: the sole legal target is announced and the cast parks on the manual mana \
         payment, got {:?}",
        runner.state().waiting_for
    );
    let pending = runner
        .state()
        .pending_cast
        .as_deref()
        .expect("reach-guard: the ManaPayment prompt owns the cast");
    assert_eq!(
        pending.ability.targets,
        vec![TargetRef::Object(host)],
        "reach-guard: the park announced the only legal target — the enchanted Bears"
    );
    ParkedVines {
        runner,
        vines,
        host,
        aura,
    }
}

/// CR 115.1 + CR 601.2c: the declaring slot's filter is `EnchantedBy` on a
/// permanent, which is the predicate the recorded announcement has to answer.
fn declares_enchanted_permanent(state: &GameState, spell: ObjectId) -> bool {
    state.objects[&spell].abilities.iter().any(|ability| {
        ability
            .effect
            .target_filter()
            .is_some_and(|filter| match filter {
                engine::types::ability::TargetFilter::Typed(TypedFilter {
                    type_filters,
                    properties,
                    ..
                }) => {
                    type_filters.contains(&TypeFilter::Permanent)
                        && properties.contains(&FilterProp::EnchantedBy)
                }
                _ => false,
            })
    })
}

/// The one object fact the parked cast's announcement latch carries.
fn parked_latch_fact(runner: &GameRunner) -> &ActivationTargetFact {
    let pending = runner
        .state()
        .pending_cast
        .as_deref()
        .expect("reach-guard: the manual payment park keeps its pending cast");
    let facts = pending
        .committed_target_facts
        .as_deref()
        .expect("reach-guard: the announcement latch is populated at the payment park");
    assert_eq!(
        facts.len(),
        1,
        "reach-guard: this cast declares exactly one target"
    );
    &facts[0]
}

/// Save the parked cast through the authoritative capture and read it back.
fn save_and_restore(runner: &GameRunner) -> Result<GameState, PersistedRestoreError> {
    let saved = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("the parked cast serializes");
    let restored: PersistedGameState =
        serde_json::from_str(&saved).expect("the parked cast deserializes");
    restored.into_game_state()
}

/// WITNESS (honest direction): an UNTOUCHED save of a Venomous Vines announced at
/// an Aura-enchanted permanent, taken during mana payment, restores and resolves —
/// and the resolved spell destroys the permanent it announced.
///
/// Revert-probe: with `capture_target_facts` recording the generic
/// `snapshot_public_characteristics` (whose `attachments` is empty by
/// construction), the latch assertion below fails on the empty set — and, with
/// that assertion removed, the save is refused at `save_and_restore` with
/// `ContradictoryAnnouncedTargetFacts { reason: SnapshotContradictsDeclaredFilter }`
/// because `EnchantedBy` answers a definite `false` for a target that IS
/// enchanted.
#[test]
fn venomous_vines_save_taken_during_mana_payment_restores_and_resolves() {
    let ParkedVines {
        runner,
        vines,
        host,
        aura,
    } = park_venomous_vines_at_manual_payment();

    assert!(
        declares_enchanted_permanent(runner.state(), vines),
        "reach-guard: Venomous Vines declares 'target enchanted permanent'"
    );
    let ActivationTargetFact::Object { id, lki, .. } = parked_latch_fact(&runner) else {
        panic!("reach-guard: Venomous Vines' declared target is an object");
    };
    assert_eq!(*id, host, "the latch names the announced Bears");
    assert_eq!(
        lki.attachments
            .iter()
            .map(|attachment| (attachment.object_id, attachment.kind.clone()))
            .collect::<Vec<_>>(),
        vec![(aura, AttachmentKind::Aura)],
        "CR 303.4 + CR 608.2h: the announcement's own record states the Aura that \
         made the Bears an 'enchanted permanent'"
    );

    let restored = save_and_restore(&runner)
        .expect("CR 601.2c: an untouched Venomous Vines save is admitted, not refused");
    assert_eq!(
        restored.objects[&host].zone,
        Zone::Battlefield,
        "reach-guard: the announced target is still a battlefield permanent"
    );
    assert_eq!(
        restored.objects[&aura].zone,
        Zone::Battlefield,
        "reach-guard: the restoring save keeps the Aura on the battlefield"
    );
    assert_eq!(
        restored.objects[&aura].attached_to,
        Some(AttachTarget::Object(host)),
        "reach-guard: the Aura is still attached to the announced target"
    );

    let mut runner = GameRunner::from_state(restored);
    let pending = runner
        .state()
        .pending_cast
        .as_deref()
        .expect("the restored park keeps its pending cast");
    assert_eq!(
        pending.ability.targets,
        vec![TargetRef::Object(host)],
        "the restored cast still names the permanent it announced"
    );

    // CR 601.2h: the mana the park was waiting for.
    runner
        .act(GameAction::PassPriority)
        .expect("the restored manual payment finalizes");
    assert_eq!(
        runner.state().objects[&vines].zone,
        Zone::Stack,
        "reach-guard: the resumed cast reached the stack"
    );
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty(), "the spell resolved");
    assert_eq!(
        runner.state().objects[&vines].zone,
        Zone::Graveyard,
        "the resolved sorcery is in its owner's graveyard"
    );
    assert_eq!(
        runner.state().objects[&host].zone,
        Zone::Graveyard,
        "CR 701.7a: the resumed Venomous Vines destroys the enchanted permanent it \
         announced; got zone {:?}",
        runner.state().objects[&host].zone
    );
    assert_eq!(
        runner.state().objects[&aura].zone,
        Zone::Graveyard,
        "CR 704.5m: an Aura that is no longer attached to anything is put into its \
         owner's graveyard"
    );
}

/// THE DISCRIMINATING TWIN: the same save with the recorded attachment set emptied
/// is refused. The relation IS recorded — it is the announcement's own fact — and
/// the slot declared an "enchanted permanent", so the record now states the
/// opposite of its own declaration. An unrecorded relation may be treated as "no
/// check"; a recorded empty one may not.
///
/// The first half re-proves the honest direction, so this test also fails when the
/// capture fix is reverted.
#[test]
fn venomous_vines_save_whose_recorded_attachments_contradict_the_slot_is_refused() {
    let ParkedVines {
        runner,
        vines,
        host,
        ..
    } = park_venomous_vines_at_manual_payment();

    // Honest direction first: the untouched save is admitted.
    save_and_restore(&runner).expect("CR 601.2c: an untouched save is admitted");
    assert!(
        declares_enchanted_permanent(runner.state(), vines),
        "reach-guard: the slot this save is judged against declares an enchanted permanent"
    );

    let mut state = runner.state().clone();
    {
        let pending = state
            .pending_cast
            .as_deref_mut()
            .expect("reach-guard: the park owns its pending cast");
        let latched = pending
            .committed_target_facts
            .as_deref_mut()
            .expect("reach-guard: the engine-written park carries its announced-target latch");
        let fact = latched
            .iter_mut()
            .find(|fact| matches!(fact, ActivationTargetFact::Object { .. }))
            .expect("reach-guard: the latch names an object target");
        let ActivationTargetFact::Object { lki, .. } = fact else {
            unreachable!("the find above matched an object fact")
        };
        lki.attachments.clear();
    }

    let saved = serde_json::to_string(&PersistedGameState::capture(state))
        .expect("the restated park serializes");
    let restored: PersistedGameState =
        serde_json::from_str(&saved).expect("the restated park decodes");
    match restored.into_game_state() {
        Err(PersistedRestoreError::ContradictoryAnnouncedTargetFacts {
            spell_object_id,
            reason: AnnouncedTargetFactConflict::SnapshotContradictsDeclaredFilter { target },
        }) => {
            assert_eq!(spell_object_id, vines, "the refusal names the parked cast");
            assert_eq!(
                target,
                TargetRef::Object(host),
                "the refusal names the announced Bears"
            );
        }
        other => panic!(
            "CR 601.2c: a save whose recorded announcement states NO attachment for a target \
             its own slot requires to be enchanted is refused; got {other:?}"
        ),
    }
}
