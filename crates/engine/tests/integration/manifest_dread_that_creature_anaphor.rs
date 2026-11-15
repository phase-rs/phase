//! CR 608.2c + CR 701.62a: "manifest dread, then attach this Equipment to that
//! creature" — the demonstrative names the permanent the previous instruction
//! produced (#7531).
//!
//! Conductive Machete, Cursed Windbreaker, Dissection Tools and Killer's Mask
//! all print this line. Before the fix the anaphor fell through to
//! `TargetFilter::ParentTarget`, which resolves off the parent ability's
//! TARGETS — and `Effect::ManifestDread` declares none, so the set was empty
//! and the attach was a silent no-op.
//!
//! Two halves had to line up, and each test below is red without both: the
//! parser must bind the anaphor to `TargetFilter::LastCreated`, and the
//! face-down entry must publish itself into that referent slot the way a token
//! producer does.
//!
//! The same binding covers the plain-manifest sorceries — Fierce Invocation,
//! Formless Nurturing and Wildcall print "Manifest the top card of your
//! library, then put N +1/+1 counters on **it**", where the anaphor used to
//! bind to the SORCERY (`SelfRef`) and the counters went nowhere. Measured over
//! `client/public/card-data.json`: 7 cards change parse, these three plus the
//! four Equipment.
//!
//! Cards are built from Oracle text (CI has no card database). The Equipment
//! subtype is stamped explicitly: without it CR 301.5 makes the attachment
//! illegal and state-based actions unattach it again, which would make every
//! assertion here vacuous — the token control test is what proves the harness
//! attaches at all.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const MACHETE: &str = "When this Equipment enters, manifest dread, then attach this Equipment to that creature.\nEquipped creature gets +2/+1.\nEquip {4}";

/// The already-working sibling shape: a token producer with the same
/// "then attach this Equipment to it" continuation.
const ANCESTRAL_BLADE: &str = "When this Equipment enters, create a 1/1 white Soldier creature token, then attach this Equipment to it.\nEquipped creature gets +1/+1.\nEquip {3}";

/// An Equipment in hand with `oracle`, plus `library` library cards so manifest
/// dread has something to look at.
fn board(name: &str, oracle: &str, library: usize) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..library {
        scenario.add_card_to_library_top(P0, &format!("Library {i}"));
    }
    let equipment = scenario
        .add_artifact_to_hand_from_oracle(P0, name, oracle)
        .with_subtypes(vec!["Equipment"])
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_mana_pool(P0, vec![]);
    (scenario.build(), equipment)
}

fn host_of(runner: &GameRunner, equipment: ObjectId) -> Option<ObjectId> {
    runner.state().objects[&equipment]
        .attached_to
        .as_ref()
        .and_then(|attached| attached.as_object())
}

/// Conductive Machete: the Equipment attaches to the creature its own manifest
/// dread just produced.
///
/// Manifest dread looks at two cards, so it parks
/// `WaitingFor::ManifestDreadChoice` and the manifested card enters from the
/// CONTINUATION — the arm that had no referent publish at all.
///
/// Reverting either half turns this red: without the parser binding the attach
/// target stays `ParentTarget` and `attached_to` is `None`; without the publish
/// `LastCreated` resolves to a stale (or empty) id.
#[test]
fn the_equipment_attaches_to_the_creature_manifest_dread_produced() {
    let (mut runner, machete) = board("Conductive Machete", MACHETE, 2);
    let manifested = runner.state().players[0].library[0];

    runner.cast(machete).resolve();
    runner.advance_until_stack_empty();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ManifestDreadChoice { .. }
        ),
        "manifest dread must pause for the two-card choice, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![manifested],
        })
        .expect("choose the card to manifest");
    runner.advance_until_stack_empty();

    assert!(
        runner.state().objects[&manifested].face_down,
        "the chosen card must be on the battlefield face down"
    );
    assert_eq!(
        host_of(&runner, machete),
        Some(manifested),
        "the Machete must equip the creature it just manifested"
    );
}

/// CR 609.3 counter-direction: with an EMPTY library manifest dread produces
/// nothing, so the anaphor has no referent and the Equipment stays unattached
/// rather than latching onto some earlier object.
///
/// This is the guard against the stale-referent failure mode: `LastCreated`
/// reads a game-lifetime slot, so a fix that published the wrong thing would
/// show up here as an unexpected host.
#[test]
fn an_empty_library_manifests_nothing_and_attaches_nothing() {
    let (mut runner, machete) = board("Conductive Machete", MACHETE, 0);

    runner.cast(machete).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        host_of(&runner, machete),
        None,
        "with nothing manifested there is no creature to equip"
    );
}

/// A board that plays a REAL producer first, so the referent slot already holds
/// something when the Machete's own producer runs. Both Equipment start in
/// hand; the Blade is cast and resolved, leaving its Soldier token as the
/// chain's most-recent referent.
fn board_with_a_prior_referent(library: usize) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..library {
        scenario.add_card_to_library_top(P0, &format!("Library {i}"));
    }
    let blade = scenario
        .add_artifact_to_hand_from_oracle(P0, "Ancestral Blade", ANCESTRAL_BLADE)
        .with_subtypes(vec!["Equipment"])
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let machete = scenario
        .add_artifact_to_hand_from_oracle(P0, "Conductive Machete", MACHETE)
        .with_subtypes(vec!["Equipment"])
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_mana_pool(P0, vec![]);
    let mut runner = scenario.build();

    runner.cast(blade).resolve();
    runner.advance_until_stack_empty();
    let prior = host_of(&runner, blade).expect("setup: the Blade equips its own token");
    assert_eq!(
        runner.state().last_created_token_ids,
        vec![prior],
        "setup: the prior producer left its token as the chain referent"
    );

    (runner, machete, prior)
}

/// The reviewer's continuation case, accepted: with a PRIOR referent already in
/// the slot, the paused two-card choice must leave the newly manifested card as
/// the referent — not the token the previous instruction produced.
///
/// The starting slot is non-empty on purpose. A test that starts empty cannot
/// tell "published the new entrant" from "retained whatever was there", because
/// both leave a slot that happens to be right.
#[test]
fn a_paused_continuation_overwrites_a_prior_referent() {
    let (mut runner, machete, prior) = board_with_a_prior_referent(2);
    let manifested = runner.state().players[0].library[0];

    runner.cast(machete).resolve();
    runner.advance_until_stack_empty();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ManifestDreadChoice { .. }
        ),
        "manifest dread must pause for the two-card choice, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![manifested],
        })
        .expect("choose the card to manifest");
    runner.advance_until_stack_empty();

    assert_eq!(
        host_of(&runner, machete),
        Some(manifested),
        "the Machete equips what its OWN manifest dread produced"
    );
    assert_ne!(
        host_of(&runner, machete),
        Some(prior),
        "and never the earlier instruction's token"
    );
    assert_eq!(
        runner.state().last_created_token_ids,
        vec![manifested],
        "the slot names the most recent producer's output"
    );
}

/// The same board, declined: manifest dread produces NOTHING, so the
/// demonstrative names nothing and the Equipment stays unattached.
///
/// This is the row the stale-referent failure mode shows up in. `LastCreated`
/// is a game-lifetime slot, so a producer that runs and produces nothing must
/// leave it EMPTY — otherwise "that creature" silently reaches back to the
/// previous instruction's token and the Machete equips a creature the sentence
/// never mentioned. Without the producer's up-front clear this row equips
/// `prior`.
#[test]
fn a_producer_that_produces_nothing_does_not_leave_a_prior_referent_standing() {
    let (mut runner, machete, prior) = board_with_a_prior_referent(0);

    runner.cast(machete).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        host_of(&runner, machete),
        None,
        "nothing was manifested, so there is no creature to equip — and \
         certainly not {prior:?} from the previous instruction"
    );
    assert!(
        runner.state().last_created_token_ids.is_empty(),
        "a producer that produced nothing leaves nothing behind, got {:?}",
        runner.state().last_created_token_ids
    );
}

/// The token sibling must keep working — and it is the reach guard for the
/// negative assertion above: if this harness could not attach an Equipment at
/// all, this test would fail too.
#[test]
fn the_token_producer_sibling_still_attaches() {
    let (mut runner, blade) = board("Ancestral Blade", ANCESTRAL_BLADE, 0);

    runner.cast(blade).resolve();
    runner.advance_until_stack_empty();

    let host = host_of(&runner, blade).expect("Ancestral Blade must equip its token");
    let host_obj = &runner.state().objects[&host];
    // Printed 1/1; the equipped bonus is already applied to `power`, so assert
    // the printed value and the token flag instead of the live P/T.
    assert!(
        host_obj.is_token && host_obj.base_power == Some(1),
        "the host must be the 1/1 Soldier token, got name={:?} is_token={} base_power={:?}",
        host_obj.name,
        host_obj.is_token,
        host_obj.base_power
    );
}

/// The plain-manifest sibling, and the other runtime publish site: Formless
/// Nurturing's "Manifest the top card of your library, then put a +1/+1 counter
/// on **it**" manifests SYNCHRONOUSLY through `morph::manifest_card` — no
/// `WaitingFor` pause, no continuation.
///
/// Before the fix the anaphor bound to `TargetFilter::SelfRef`, i.e. the
/// SORCERY itself, so the counter went nowhere. Fierce Invocation and Wildcall
/// print the same line with a different count.
const FORMLESS_NURTURING: &str =
    "Manifest the top card of your library, then put a +1/+1 counter on it.";

#[test]
fn a_plain_manifest_puts_its_counter_on_the_manifested_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Library Top");
    let spell = scenario
        .add_spell_to_hand(P0, "Formless Nurturing", false)
        .from_oracle_text(FORMLESS_NURTURING)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_mana_pool(P0, vec![]);
    let mut runner = scenario.build();
    let manifested = runner.state().players[0].library[0];

    runner.cast(spell).resolve();
    runner.advance_until_stack_empty();

    assert!(
        runner.state().objects[&manifested].face_down,
        "the top card must be manifested face down"
    );
    assert_eq!(
        runner.state().objects[&manifested]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "the +1/+1 counter belongs on the manifested creature, not on the sorcery"
    );
}

/// The bare object pronoun is NOT re-routed. "…then attach this Equipment to
/// **it**" keeps the binding its own subject-aware authority
/// (`attach_neuter_recipient_resolves_via_subject`) gives it, which for this
/// shape is `SelfRef` — Cryptic Coat prints exactly this line after a cloak.
///
/// The demonstrative-only entry point is what makes that true: routing bare "it"
/// through the chain-created binding as well would change attachment for chains
/// that have nothing to do with a chain-created referent.
#[test]
fn the_bare_pronoun_recipient_is_left_to_its_own_authority() {
    let parsed = engine::parser::parse_oracle_text(
        "When this Equipment enters, manifest dread, then attach this Equipment to it.",
        "Bare Pronoun Coat",
        &[],
        &["Artifact".to_string()],
        &["Equipment".to_string()],
    );
    let attach = parsed.triggers[0]
        .execute
        .as_deref()
        .and_then(|execute| execute.sub_ability.as_deref())
        .expect("the attach clause is the producer's continuation");
    assert!(
        matches!(
            &*attach.effect,
            engine::types::ability::Effect::Attach {
                target: engine::types::ability::TargetFilter::SelfRef,
                ..
            }
        ),
        "bare \"it\" must keep its pre-existing binding, got {:?}",
        attach.effect
    );
}

/// CR 603.12 + CR 608.2c: a face-down producer under an AFFIRMATIVE reflexive
/// gate seeds the referent, and its consumer must stay inside the gated
/// instruction — carrying the same condition — rather than becoming an
/// independent sibling that reads the game-lifetime `last_created_token_ids`
/// ledger when the gate is false.
///
/// This is the shape the widened producer predicate has to cover: seeding
/// (`chain_prior_referent_is_created_token`), gated relinking
/// (`relink_gated_token_referent_consumers`) and clone transplanting
/// (`clone_would_transplant_gated_referent`) now ask one question, so a gated
/// manifest cannot seed a referent that the relink pass then declines to protect.
#[test]
fn a_gated_face_down_producer_keeps_its_consumer_under_the_gate() {
    let parsed = engine::parser::parse_oracle_text(
        "When this Equipment enters, you may pay {1}. If you do, manifest dread, then attach this Equipment to that creature.",
        "Gated Machete",
        &[],
        &["Artifact".to_string()],
        &["Equipment".to_string()],
    );
    let producer = parsed.triggers[0]
        .execute
        .as_deref()
        .and_then(|execute| execute.sub_ability.as_deref())
        .expect("the gated manifest dread is the payment's continuation");
    assert!(
        producer.condition.is_some(),
        "the producer must carry the reflexive gate, got {:?}",
        producer.condition
    );
    let attach = producer
        .sub_ability
        .as_deref()
        .expect("the attach clause must sit INSIDE the gated instruction");
    assert!(
        matches!(
            &*attach.effect,
            engine::types::ability::Effect::Attach {
                target: engine::types::ability::TargetFilter::LastCreated,
                ..
            }
        ),
        "the gated producer's consumer binds the chain-created referent, got {:?}",
        attach.effect
    );
    assert_eq!(
        attach.condition, producer.condition,
        "and it cannot resolve when the gate is false"
    );
}
