//! Issue #5278 — Vizier of Many Faces Embalm copy panic.
//!
//! Vizier of Many Faces is a 0/0 Clone whose printed replacement lets it enter
//! as a copy of any creature, "except if this creature was embalmed, the token
//! has no mana cost, it's white, and it's a Zombie in addition to its other
//! types." Its Embalm ability synthesizes an activated ability that creates a
//! token that's a copy of Vizier (`CopyTokenOf { target: SelfRef,
//! additional_modifications: [SetColor White, RemoveManaCost, AddSubtype Zombie] }`).
//!
//! Because Vizier's copiable values carry its OWN "enter as a copy" replacement,
//! the freshly created Embalm token pauses at `ReplacementChoice` (accept the
//! enter-as-copy) and then `CopyTargetChoice` (pick a creature to copy). On
//! accept, the liminal branch of `handle_copy_target_choice` resolves
//! `BecomeCopy` (copying the chosen creature onto the token) and then had to
//! apply the Embalm copy exceptions.
//!
//! THE BUG (pre-fix): the exceptions were re-applied as a `Duration::Permanent`
//! LAYERED transient continuous effect. `RemoveManaCost` is a stamp-only copy
//! exception (CR 707.9b — copy "except" clauses are part of the token's
//! COPIABLE values, not a layered effect), so the layer pass hit
//! `unreachable!("RemoveManaCost is consumed at copy resolution; never layered")`
//! and PANICKED.
//!
//! THE FIX: fold the Embalm exceptions into the `BecomeCopy` ability's own
//! `additional_modifications` before it resolves, so `become_copy::resolve`
//! (the single authority) CONSUMES `RemoveManaCost` into the copy's copiable
//! values (CR 707.9b) and layers the remaining CR 613.1 continuous copy
//! exceptions atop the `CopyValues` clone — never layering the stamp-only
//! `RemoveManaCost`. On revert (re-applying the exceptions as a separate
//! `Duration::Permanent` transient), the accept case panics again.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{ContinuousModification, Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim Oracle text for Vizier of Many Faces (Amonkhet). The Embalm line is
/// carried as an explicit keyword hint so the scenario's parse pipeline
/// synthesizes the graveyard-activated token-copy ability.
const VIZIER_ORACLE: &str = "You may have this creature enter as a copy of any creature on the battlefield, except if this creature was embalmed, the token has no mana cost, it's white, and it's a Zombie in addition to its other types.\nEmbalm {3}{U}{U}";

fn add_mana(runner: &mut GameRunner, mana: &[ManaType]) {
    let dummy = engine::types::identifiers::ObjectId(0);
    let pool = &mut runner.state_mut().players[0].mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

/// Index of the synthesized Embalm activated ability on the graveyard Vizier.
fn embalm_ability_index(
    runner: &GameRunner,
    vizier: engine::types::identifiers::ObjectId,
) -> usize {
    runner.state().objects[&vizier]
        .abilities
        .iter()
        .position(|a| matches!(&*a.effect, Effect::CopyTokenOf { .. }))
        .expect("synthesized Embalm ability must be present on the graveyard Vizier")
}

/// `RemoveManaCost` is a stamp-only copy exception: it is consumed into the
/// copy's copiable values (CR 707.9b) and MUST NEVER appear as a layered
/// continuous modification (`types/layers.rs` panics if one is layered). This
/// is the exact modification whose layering triggered the #5278 panic.
fn is_stamp_only_exception(m: &ContinuousModification) -> bool {
    matches!(
        m,
        ContinuousModification::RemoveManaCost
            | ContinuousModification::SetStartingLoyalty { .. }
            | ContinuousModification::AddCounterOnEnter { .. }
    )
}

fn build_scenario() -> (
    GameRunner,
    engine::types::identifiers::ObjectId,
    engine::types::identifiers::ObjectId,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Vizier of Many Faces in P0's graveyard, built from its verbatim Oracle
    // text with the Embalm keyword hint. Printed as a 0/0 Clone.
    let vizier = scenario
        .add_creature_to_graveyard(P0, "Vizier of Many Faces", 0, 0)
        .with_mana_cost(engine::types::mana::ManaCost::Cost {
            generic: 3,
            shards: vec![engine::types::mana::ManaCostShard::Blue],
        })
        .from_oracle_text_with_keywords(&["Embalm"], VIZIER_ORACLE)
        .id();

    // A vanilla 3/3 Bear on the battlefield: the creature the Embalm token will
    // copy. Distinct P/T (3/3) discriminates the copy from Vizier's own 0/0;
    // the "Bear" subtype lets us prove Embalm's Zombie is ADDED "in addition to
    // its other types" (CR 702.128a) rather than replacing them.
    let bear = scenario
        .add_creature(P0, "Grizzly Bears", 3, 3)
        .with_subtypes(vec!["Bear"])
        .id();

    let mut runner = scenario.build();
    // Plenty of mana for the {3}{U}{U} Embalm cost.
    add_mana(
        &mut runner,
        &[
            ManaType::Blue,
            ManaType::Blue,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ],
    );
    (runner, vizier, bear)
}

/// Activate the Embalm ability and drain any mana-payment prompt so the token
/// is created and parked on its first entry-choice prompt.
fn activate_embalm(runner: &mut GameRunner, vizier: engine::types::identifiers::ObjectId) {
    let index = embalm_ability_index(runner, vizier);
    runner
        .act(GameAction::ActivateAbility {
            source_id: vizier,
            ability_index: index,
        })
        .expect("activate Embalm ability");
    // Resolve the token-copy ability on the stack: pass priority until an entry
    // choice (ReplacementChoice / CopyTargetChoice) surfaces.
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pay embalm mana");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass to resolve");
            }
            WaitingFor::ReplacementChoice { .. } | WaitingFor::CopyTargetChoice { .. } => return,
            other => panic!("unexpected waiting_for before entry choice: {other:?}"),
        }
    }
    panic!("Embalm activation never reached an entry choice");
}

#[test]
fn embalm_copy_of_bear_survives_and_carries_stamped_exceptions() {
    let (mut runner, vizier, _bear) = build_scenario();
    activate_embalm(&mut runner, vizier);

    // Accept the enter-as-copy replacement, then pick the Bear to copy. This is
    // the exact sequence that panicked before the fix.
    let token = 'drive: loop {
        match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice { .. } => {
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("accept enter-as-copy replacement");
            }
            WaitingFor::CopyTargetChoice {
                source_id,
                valid_targets,
                ..
            } => {
                let target = *valid_targets
                    .first()
                    .expect("Bear must be a legal copy target");
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("choose copy target (Bear)");
                break 'drive source_id;
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected waiting_for during entry: {other:?}"),
        }
    };

    // Drain any residual priority so SBAs run.
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            _ => break,
        }
    }

    // (a) No panic reached here. The token is on the battlefield as a 3/3 copy
    // of the Bear (survives SBAs — a 0/0 would have died to CR 704.5f).
    let obj = runner
        .state()
        .objects
        .get(&token)
        .expect("Embalm token must still exist");
    assert_eq!(
        obj.zone,
        Zone::Battlefield,
        "the Embalm copy token must survive on the battlefield, not die to SBA"
    );
    assert_eq!(
        obj.name, "Grizzly Bears",
        "the token must be a copy of the chosen creature"
    );
    assert_eq!(
        (obj.power, obj.toughness),
        (Some(3), Some(3)),
        "the copy must carry the Bear's 3/3 (a 0/0 would die to SBA)"
    );

    // Embalm exceptions, STAMPED into the token's copiable values:
    // it's white, it's a Zombie in addition to its other types, no mana cost.
    assert_eq!(
        obj.color,
        vec![ManaColor::White],
        "Embalm makes the token white (CR 702.128a), got {:?}",
        obj.color
    );
    assert!(
        obj.card_types.subtypes.iter().any(|s| s == "Zombie"),
        "Embalm adds Zombie in addition to its other types, got {:?}",
        obj.card_types.subtypes
    );
    assert!(
        obj.card_types.subtypes.iter().any(|s| s == "Bear"),
        "the Bear subtype from the copied creature must be retained, got {:?}",
        obj.card_types.subtypes
    );
    assert_eq!(
        obj.mana_cost,
        engine::types::mana::ManaCost::NoCost,
        "Embalm gives the token no mana cost, got {:?}",
        obj.mana_cost
    );

    // The stamp-only exception (`RemoveManaCost`) — the exact modification whose
    // layering panicked #5278 — must be consumed into the copy's copiable values
    // by `become_copy::resolve` (CR 707.9b), NEVER left as a layered continuous
    // modification. Vizier is a Clone, so `SetColor`/`AddSubtype` are correctly
    // carried as CR 613.1 continuous copy exceptions atop the `CopyValues` clone;
    // only the stamp-only class must be absent from the transient layer set.
    let leaked: Vec<&ContinuousModification> = runner
        .state()
        .transient_continuous_effects
        .iter()
        .flat_map(|tce| tce.modifications.iter())
        .filter(|m| is_stamp_only_exception(m))
        .collect();
    assert!(
        leaked.is_empty(),
        "stamp-only copy exceptions (RemoveManaCost etc.) must be consumed into \
         copiable values, never layered; found leaked transient modifications: {leaked:?}"
    );
}

#[test]
fn embalm_copy_declined_enters_as_zero_zero_and_dies() {
    let (mut runner, vizier, _bear) = build_scenario();
    activate_embalm(&mut runner, vizier);

    // Decline the enter-as-copy replacement (index 1 = decline on an optional
    // replacement). The token stays a copy of Vizier (0/0) with the Embalm
    // exceptions already stamped at creation, then dies to CR 704.5f.
    let token = loop {
        match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice { candidates, .. } => {
                // Positive reach-guard: the enter-as-copy replacement really
                // surfaced (proving the token carried Vizier's replacement and
                // we are on the entry-choice path, not a short-circuit).
                assert!(
                    !candidates.is_empty(),
                    "the enter-as-copy replacement must be offered"
                );
                // Capture the entering token id before declining.
                let entering = runner
                    .state()
                    .liminal_entries
                    .keys()
                    .copied()
                    .next()
                    .expect("a liminal Embalm token must be pending entry");
                runner
                    .act(GameAction::ChooseReplacement { index: 1 })
                    .expect("decline enter-as-copy replacement");
                break entering;
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected waiting_for before decline: {other:?}"),
        }
    };

    // Drain to let SBAs run.
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            _ => break,
        }
    }

    // A declined 0/0 copy of Vizier is a token that dies to SBA and ceases to
    // exist (CR 704.5f + CR 111.7): it must NOT be on the battlefield.
    let on_battlefield = runner.state().battlefield.contains(&token)
        || runner
            .state()
            .objects
            .get(&token)
            .is_some_and(|o| o.zone == Zone::Battlefield);
    assert!(
        !on_battlefield,
        "a declined 0/0 Embalm copy of Vizier must die to SBA, not remain on the battlefield"
    );
}
