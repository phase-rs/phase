//! Token-art descriptors: the engine owns the intrinsic token body used for
//! shape-based art lookup. When no exact `token_image_ref` matched (e.g. the
//! ambiguous 1/1 red Goblin body from #7552), the client falls back to a
//! name+shape search — and that search must be keyed by PRINTED (base)
//! characteristics, never live values. These tests prove the descriptor is
//! populated at creation with the printed body and survives the JSON
//! snapshot/transport round-trip under its `token_art` wire key.

use engine::game::game_object::{reset_token_art_derivation_count, token_art_derivation_count};
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;

fn create_via_etb(source_name: &str, oracle: &str) -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let caster = scenario
        .add_creature_to_hand_from_oracle(P0, source_name, 1, 1, oracle)
        .id();
    (scenario, caster)
}

fn token_named(runner: &GameRunner, name: &str) -> ObjectId {
    runner
        .state()
        .battlefield
        .iter()
        .find(|id| runner.state().objects[*id].name == name)
        .copied()
        .unwrap_or_else(|| panic!("the {name} token exists"))
}

#[test]
fn vanilla_token_carries_its_printed_shape_without_an_exact_ref() {
    // The ambiguous 1/1 red Goblin body resolves NO image ref from an
    // unlisted source (#7552) — exactly the case the descriptor rescues.
    let (scenario, caster) = create_via_etb(
        "Backstreet Recruiter",
        "When this creature enters, create a 1/1 red Goblin creature token.",
    );
    let mut runner = scenario.build();
    runner.cast(caster).resolve();
    runner.advance_until_stack_empty();

    let goblin = token_named(&runner, "Goblin");
    let obj = &runner.state().objects[&goblin];
    assert!(
        obj.token_image_ref.is_none(),
        "precondition: the ambiguous body carries no exact ref"
    );
    let art = obj
        .token_art
        .as_ref()
        .expect("creation populates the art descriptor");
    assert_eq!(art.power, Some(1));
    assert_eq!(art.toughness, Some(1));
    assert_eq!(art.colors, vec![ManaColor::Red]);
    assert_eq!(art.subtypes, vec!["Goblin".to_string()]);
    assert!(art.keywords.is_empty());
    assert!(!art.has_abilities);
}

#[test]
fn keyword_token_carries_keyword_families() {
    let (scenario, caster) = create_via_etb(
        "Soulcatcher",
        "When this creature enters, create a 1/1 white Spirit creature token with flying.",
    );
    let mut runner = scenario.build();
    runner.cast(caster).resolve();
    runner.advance_until_stack_empty();

    let spirit = token_named(&runner, "Spirit");
    let art = runner.state().objects[&spirit]
        .token_art
        .as_ref()
        .expect("creation populates the art descriptor");
    assert_eq!(art.power, Some(1));
    assert_eq!(art.toughness, Some(1));
    assert_eq!(art.colors, vec![ManaColor::White]);
    assert_eq!(art.subtypes, vec!["Spirit".to_string()]);
    assert_eq!(art.keywords, vec!["Flying".to_string()]);
    assert!(art.has_abilities);
}

#[test]
fn predefined_treasure_reports_its_mana_ability() {
    let (scenario, caster) = create_via_etb(
        "Prosperous Innkeeper",
        "When this creature enters, create a Treasure token.",
    );
    let mut runner = scenario.build();
    runner.cast(caster).resolve();
    runner.advance_until_stack_empty();

    let treasure = token_named(&runner, "Treasure");
    let art = runner.state().objects[&treasure]
        .token_art
        .as_ref()
        .expect("creation populates the art descriptor");
    assert_eq!(art.power, None);
    assert_eq!(art.toughness, None);
    assert!(art.colors.is_empty());
    assert!(art.has_abilities, "the mana ability is a printed ability");
}

#[test]
fn enter_as_copy_recipient_rides_the_source_token_descriptor() {
    // Clone keeps its own base (a 0/0 Shapeshifter) and never runs the
    // token creation injectors, so it must ride the source token's
    // captured descriptor — here the ambiguous Goblin body, which carries
    // no exact ref.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let recruiter = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Backstreet Recruiter",
            1,
            1,
            "When this creature enters, create a 1/1 red Goblin creature token.",
        )
        .id();
    let clone = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Clone",
            0,
            0,
            "You may have this creature enter as a copy of any creature on the battlefield.",
        )
        .id();
    let mut runner = scenario.build();
    runner.cast(recruiter).resolve();
    runner.advance_until_stack_empty();
    let goblin = token_named(&runner, "Goblin");

    let _ = runner.cast(clone).resolve();
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::CopyTargetChoice { .. } => break,
            WaitingFor::ReplacementChoice { .. } => {
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("accept enter-as-copy replacement");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept optional enter-as-copy");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt while entering as a copy: {other:?}"),
        }
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CopyTargetChoice { .. }
        ),
        "Clone must ask what to copy"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(goblin)),
        })
        .expect("copy the Goblin token");
    runner.advance_until_stack_empty();

    let clone_obj = &runner.state().objects[&clone];
    assert_eq!(clone_obj.name, "Goblin");
    assert!(
        clone_obj.token_image_ref.is_none(),
        "precondition: the ambiguous source carries no exact ref"
    );
    let art = clone_obj
        .token_art
        .as_ref()
        .expect("enter-as-copy rides the source descriptor");
    assert_eq!(art.power, Some(1));
    assert_eq!(art.toughness, Some(1));
    assert_eq!(art.colors, vec![ManaColor::Red]);
    assert_eq!(art.subtypes, vec!["Goblin".to_string()]);
    assert!(!art.has_abilities);
}

#[test]
fn true_token_restores_its_own_descriptor_after_a_temporary_copy_expires() {
    // The Saheeli pattern: a Servo token temporarily copies a Goblin
    // token. While copying it rides the Goblin descriptor; once the
    // until-end-of-turn effect expires (CR 514.2), the layer baseline
    // re-derives the Servo's own descriptor from its printed base — the
    // copy layer only ever wrote the live axes. (The nontoken control —
    // a Mockingbird recipient resetting to `None` — lives in
    // `become_copy::tests::copy_of_token_carries_source_art_descriptor_and_reverts`.)
    const COPY: &str =
        "Target creature you control becomes a copy of target creature until end of turn.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let servo_maker = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Servo Schematic",
            2,
            1,
            "When this creature enters, create a 1/1 colorless Servo artifact creature token.",
        )
        .id();
    let goblin_maker = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Backstreet Recruiter",
            1,
            1,
            "When this creature enters, create a 1/1 red Goblin creature token.",
        )
        .id();
    let copy_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Temporary Copy", true, COPY)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner.cast(servo_maker).resolve();
    runner.advance_until_stack_empty();
    runner.cast(goblin_maker).resolve();
    runner.advance_until_stack_empty();
    let servo = token_named(&runner, "Servo");
    let goblin = token_named(&runner, "Goblin");

    let own = runner.state().objects[&servo]
        .token_art
        .clone()
        .expect("precondition: the Servo carries its own descriptor");
    assert_eq!(own.subtypes, vec!["Servo".to_string()]);

    runner
        .cast(copy_spell)
        .target_objects(&[servo, goblin])
        .resolve();
    runner.advance_until_stack_empty();

    // Positive copied observation: the Servo rides the Goblin body.
    assert_eq!(runner.state().objects[&servo].name, "Goblin");
    let copying = runner.state().objects[&servo]
        .token_art
        .clone()
        .expect("while copying, the Servo rides a descriptor");
    assert_eq!(copying.power, Some(1));
    assert_eq!(copying.toughness, Some(1));
    assert_eq!(copying.colors, vec![ManaColor::Red]);
    assert_eq!(copying.subtypes, vec!["Goblin".to_string()]);
    assert!(!copying.has_abilities);

    // CR 514.2: the until-end-of-turn copy expires in cleanup.
    runner.advance_to_phase(Phase::End);
    runner.advance_to_phase(Phase::Upkeep);

    assert_eq!(runner.state().objects[&servo].name, "Servo");
    let restored = runner.state().objects[&servo]
        .token_art
        .clone()
        .expect("expiry restores a descriptor");
    assert_eq!(
        restored, own,
        "the Servo's own intrinsic body returns after expiry"
    );
}

#[test]
fn ordinary_layer_passes_reuse_the_live_descriptor_without_rederiving() {
    // Allocation/reuse boundary: the layer baseline reseed must not
    // materialize fresh keyword/subtype state for an unchanged token. The
    // derivation counter observes materializations while the values prove
    // the reused descriptor stays correct — including across a temporary
    // copy, where derivation legitimately resumes at the boundary.
    const COPY: &str =
        "Target creature you control becomes a copy of target creature until end of turn.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let servo_maker = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Servo Schematic",
            2,
            1,
            "When this creature enters, create a 1/1 colorless Servo artifact creature token.",
        )
        .id();
    let goblin_maker = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Backstreet Recruiter",
            1,
            1,
            "When this creature enters, create a 1/1 red Goblin creature token.",
        )
        .id();
    let copy_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Temporary Copy", true, COPY)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner.cast(servo_maker).resolve();
    runner.advance_until_stack_empty();
    runner.cast(goblin_maker).resolve();
    runner.advance_until_stack_empty();
    let servo = token_named(&runner, "Servo");
    let goblin = token_named(&runner, "Goblin");

    // Quiesce, then start counting.
    evaluate_layers(runner.state_mut());
    reset_token_art_derivation_count();

    // Idle full evaluations re-derive nothing for either token.
    evaluate_layers(runner.state_mut());
    evaluate_layers(runner.state_mut());
    assert_eq!(
        token_art_derivation_count(),
        0,
        "ordinary passes reuse live descriptors"
    );
    assert_eq!(
        runner.state().objects[&servo]
            .token_art
            .as_ref()
            .expect("servo keeps its descriptor")
            .subtypes,
        vec!["Servo".to_string()]
    );

    // A temporary copy crosses the boundary: the Servo rides the Goblin
    // body (derivation resumes to restore-then-overwrite each pass).
    runner
        .cast(copy_spell)
        .target_objects(&[servo, goblin])
        .resolve();
    runner.advance_until_stack_empty();
    evaluate_layers(runner.state_mut());
    let copying_derivations = token_art_derivation_count();
    assert!(
        copying_derivations > 0,
        "copy activity re-derives at the boundary"
    );
    assert_eq!(
        runner.state().objects[&servo]
            .token_art
            .as_ref()
            .expect("copying servo rides a descriptor")
            .subtypes,
        vec!["Goblin".to_string()]
    );

    // Expiry restores the Servo body, then reuse resumes.
    runner.advance_to_phase(Phase::End);
    runner.advance_to_phase(Phase::Upkeep);
    assert!(
        token_art_derivation_count() > copying_derivations,
        "expiry restores via re-derivation"
    );
    assert_eq!(
        runner.state().objects[&servo]
            .token_art
            .as_ref()
            .expect("expired copy restores a descriptor")
            .subtypes,
        vec!["Servo".to_string()]
    );
    reset_token_art_derivation_count();
    evaluate_layers(runner.state_mut());
    evaluate_layers(runner.state_mut());
    assert_eq!(
        token_art_derivation_count(),
        0,
        "reuse resumes after expiry"
    );
}

#[test]
fn descriptor_survives_the_json_snapshot_round_trip_under_its_wire_key() {
    let (scenario, caster) = create_via_etb(
        "Backstreet Recruiter",
        "When this creature enters, create a 1/1 red Goblin creature token.",
    );
    let mut runner = scenario.build();
    runner.cast(caster).resolve();
    runner.advance_until_stack_empty();

    let goblin = token_named(&runner, "Goblin");
    let before = runner.state().objects[&goblin].token_art.clone();
    assert!(before.is_some(), "precondition: a descriptor exists");

    // The snapshot/transport contract: full object JSON and back.
    let json = serde_json::to_value(&runner.state().objects[&goblin]).unwrap();
    assert!(
        json.get("token_art").is_some(),
        "the wire key is `token_art`: {json}"
    );
    let revived: engine::game::game_object::GameObject = serde_json::from_value(json).unwrap();
    assert_eq!(revived.token_art, before);

    // Old snapshots without the key still load, degrading to the legacy
    // live-field client lookup.
    let mut legacy = serde_json::to_value(&runner.state().objects[&goblin]).unwrap();
    legacy.as_object_mut().unwrap().remove("token_art");
    let revived_legacy: engine::game::game_object::GameObject =
        serde_json::from_value(legacy).unwrap();
    assert!(revived_legacy.token_art.is_none());
}
