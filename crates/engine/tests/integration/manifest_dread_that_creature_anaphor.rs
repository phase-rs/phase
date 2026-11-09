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
//! A bare "it" host after cloak, manifest or manifest dread binds the same
//! referent, and a source attach joined by a bare "and" gets its own clause.
//!
//! Cards are built from Oracle text (CI has no card database). The Equipment
//! subtype is stamped explicitly: without it CR 301.5 makes the attachment
//! illegal and state-based actions unattach it again, which would make every
//! assertion here vacuous — the token control test is what proves the harness
//! attaches at all.

use engine::game::combat::has_cant_be_blocked_static;
use engine::game::layers;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

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
/// something when the Equipment's own producer runs. Both Equipment start in
/// hand; the Blade is cast and resolved, leaving its Soldier token as the
/// chain's most-recent referent.
fn board_with_a_prior_referent(
    name: &str,
    oracle: &str,
    library: usize,
) -> (GameRunner, ObjectId, ObjectId) {
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
    let equipment = scenario
        .add_artifact_to_hand_from_oracle(P0, name, oracle)
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

    (runner, equipment, prior)
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
    let (mut runner, machete, prior) =
        board_with_a_prior_referent("Conductive Machete", MACHETE, 2);
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
    let (mut runner, machete, prior) =
        board_with_a_prior_referent("Conductive Machete", MACHETE, 0);

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

/// CR 608.2c + CR 701.62a: a bare "it" host after manifest dread names the
/// manifested creature, as the demonstrative does.
#[test]
fn the_bare_pronoun_recipient_binds_the_manifested_creature() {
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
                target: engine::types::ability::TargetFilter::LastCreated,
                ..
            }
        ),
        "bare \"it\" must bind the manifested creature, got {:?}",
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

const CRYPTIC_COAT: &str = "When this Equipment enters, cloak the top card of your library, then attach this Equipment to it. (To cloak a card, put it onto the battlefield face down as a 2/2 creature with ward {2}. Turn it face up any time for its mana cost if it's a creature card.)\nEquipped creature gets +1/+0 and can't be blocked.\n{1}{U}: Return this Equipment to its owner's hand.";

const CLOUDFORM: &str = "When this enchantment enters, it becomes an Aura with enchant creature. Manifest the top card of your library and attach this enchantment to it. (To manifest a card, put it onto the battlefield face down as a 2/2 creature. Turn it face up any time for its mana cost if it's a creature card.)\nEnchanted creature has flying and hexproof.";

const AUXILIARY_BOOSTERS: &str = "When this Equipment enters, create a 2/2 colorless Robot artifact creature token and attach this Equipment to it.\nEquipped creature gets +1/+2 and has flying.\nEquip {3} ({3}: Attach to target creature you control. Equip only as a sorcery.)";

const FIELD_TESTED_FRYING_PAN: &str = "When this Equipment enters, create a Food token, then create a 1/1 white Halfling creature token and attach this Equipment to it.\nEquipped creature has \"Whenever you gain life, this creature gets +X/+X until end of turn, where X is the amount of life you gained.\"\nEquip {2}";

const NIM_DEATHMANTLE: &str = "Equipped creature gets +2/+2, has intimidate, and is a black Zombie. (A creature with intimidate can't be blocked except by artifact creatures and/or creatures that share a color with it.)\nWhenever a nontoken creature is put into your graveyard from the battlefield, you may pay {4}. If you do, return that card to the battlefield and attach this Equipment to it.\nEquip {4}";

fn after_layer_pass(runner: &GameRunner) -> GameState {
    let mut state = runner.state().clone();
    layers::mark_layers_full(&mut state);
    layers::flush_layers(&mut state);
    state
}

fn tokens(runner: &GameRunner) -> Vec<ObjectId> {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| state.objects[id].is_token)
        .collect()
}

/// CR 701.58a + CR 608.2c: Cryptic Coat equips the card it just cloaked.
#[test]
fn cryptic_coat_equips_the_card_it_cloaked() {
    let (mut runner, coat) = board("Cryptic Coat", CRYPTIC_COAT, 1);
    let cloaked = runner.state().players[0].library[0];

    runner.cast(coat).resolve();
    runner.advance_until_stack_empty();

    assert!(runner.state().stack.is_empty());
    assert!(runner.state().objects[&cloaked].face_down);
    assert_eq!(host_of(&runner, coat), Some(cloaked));
    let state = after_layer_pass(&runner);
    let obj = &state.objects[&cloaked];
    assert_eq!((obj.power, obj.toughness), (Some(3), Some(2)));
    assert!(has_cant_be_blocked_static(&state, cloaked));
}

/// CR 608.2c: a cloak of an empty library produces nothing, so the Coat does
/// not reach back to the earlier producer's token.
#[test]
fn cryptic_coat_with_an_empty_library_does_not_equip_a_prior_referent() {
    let (mut runner, coat, prior) = board_with_a_prior_referent("Cryptic Coat", CRYPTIC_COAT, 0);

    runner.cast(coat).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        host_of(&runner, coat),
        None,
        "nothing was cloaked, so the Coat must not equip {prior:?}"
    );
}

/// CR 701.40a + CR 701.3a: Cloudform becomes an Aura and enchants the creature
/// it manifested.
#[test]
fn cloudform_enchants_the_creature_it_manifested() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Library 0");
    let cloudform = {
        let mut builder = scenario.add_spell_to_hand(P0, "Cloudform", false);
        builder
            .as_enchantment()
            .from_oracle_text(CLOUDFORM)
            .with_mana_cost(ManaCost::generic(0));
        builder.id()
    };
    let mut runner = scenario.build();
    let manifested = runner.state().players[0].library[0];

    runner.cast(cloudform).resolve();
    runner.advance_until_stack_empty();

    assert!(runner.state().objects[&manifested].face_down);
    assert_eq!(host_of(&runner, cloudform), Some(manifested));
    let state = after_layer_pass(&runner);
    assert_eq!(state.objects[&cloudform].zone, Zone::Battlefield);
    let keywords = &state.objects[&manifested].keywords;
    assert!(keywords.contains(&Keyword::Flying));
    assert!(keywords.contains(&Keyword::Hexproof));
}

/// CR 111.1 + CR 701.3a: Auxiliary Boosters equips the Robot it created.
#[test]
fn auxiliary_boosters_equips_the_robot_it_created() {
    let (mut runner, boosters) = board("Auxiliary Boosters", AUXILIARY_BOOSTERS, 0);

    runner.cast(boosters).resolve();
    runner.advance_until_stack_empty();

    let created = tokens(&runner);
    assert_eq!(created.len(), 1);
    assert_eq!(runner.state().objects[&created[0]].name, "Robot");
    assert_eq!(host_of(&runner, boosters), Some(created[0]));
}

/// CR 608.2c: Field-Tested Frying Pan equips the Halfling, the most recent
/// token, not the Food.
#[test]
fn frying_pan_equips_the_halfling_not_the_food() {
    let (mut runner, pan) = board("Field-Tested Frying Pan", FIELD_TESTED_FRYING_PAN, 0);

    runner.cast(pan).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(tokens(&runner).len(), 2);
    let host = host_of(&runner, pan).expect("the Pan equips a token");
    assert_eq!(runner.state().objects[&host].name, "Halfling");
}

/// CR 603.6c + CR 608.2c: Nim Deathmantle returns the dead creature and equips it.
#[test]
fn nim_deathmantle_equips_the_creature_it_returned() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let nim = scenario
        .add_artifact_from_oracle(P0, "Nim Deathmantle", NIM_DEATHMANTLE)
        .with_subtypes(vec!["Equipment"])
        .id();
    let bolt = scenario.add_bolt_to_hand(P0);
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();

    let outcome = runner
        .cast(bolt)
        .target_object(bear)
        .accept_optional()
        .resolve();
    runner.advance_until_stack_empty();

    assert!(
        outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged { object_id, to: Zone::Graveyard, .. } if *object_id == bear
        )),
        "reach guard: the Bear died"
    );
    assert_eq!(runner.state().objects[&bear].zone, Zone::Battlefield);
    assert_eq!(host_of(&runner, nim), Some(bear));
}
