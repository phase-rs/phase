//! Discriminating test for the rest-pile stranding bug in the dig
//! (`WaitingFor::DigChoice`) resolution path: when a kept card routed onto the
//! battlefield pauses on an as-enters aura-attachment choice (CR 303.4f — the
//! Aura has more than one legal host, so the controller must choose), the unkept
//! "rest pile" must still reach its destination once the choice resolves — it
//! must not be stranded in the library.
//!
//! Before the fix the handler bailed with an early `return` on the pause,
//! skipping `route_rest_partition`, so the unkept cards never left the library;
//! and the aura-attachment resume path (`WaitingFor::ReturnAsAuraTarget`) never
//! drained the parked batch tail. The fix defers the rest-pile move onto a
//! parked batch-completion continuation AND drains it on the aura resume path,
//! mirroring the surveil/manifest-dread rest-pile pattern.

use engine::game::scenario::{GameScenario, P1};
use engine::types::ability::{TargetFilter, TargetRef, TypedFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::keywords::Keyword;
use engine::types::zones::Zone;

#[test]
fn dig_rest_pile_not_stranded_when_kept_aura_pauses_on_attachment_choice() {
    let mut scenario = GameScenario::new();

    // Two creatures P1 controls — both legal hosts for the kept Aura. Two legal
    // hosts means CR 303.4f forces an attachment choice when the Aura enters.
    let host_a = scenario.add_creature(P1, "Host A", 2, 2).id();
    let host_b = scenario.add_creature(P1, "Host B", 2, 2).id();

    // P1's library top-to-bottom: [kept_aura, rest0, rest1]. The dig looks at all
    // three, keeps the Aura to the battlefield, and the rest go to the graveyard.
    let rest1 = scenario.add_card_to_library_top(P1, "Rest 1");
    let rest0 = scenario.add_card_to_library_top(P1, "Rest 0");
    let kept = scenario.add_card_to_library_top(P1, "Kept Aura");

    let mut runner = scenario.build();

    // Make `kept` a genuine Aura enchantment with "enchant creature": subtype
    // "Aura", an Enchant(creature) keyword, and no Creature core type.
    {
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&kept)
            .expect("kept exists");
        obj.card_types.core_types = vec![CoreType::Enchantment];
        obj.card_types.subtypes = vec!["Aura".to_string()];
        obj.keywords.push(Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature(),
        )));
    }

    // Drive the DigChoice prompt directly: keep the Aura (entering the
    // battlefield), the rest pile goes to the graveyard.
    runner.state_mut().waiting_for = WaitingFor::DigChoice {
        player: P1,
        library_owner: P1,
        cards: vec![kept, rest0, rest1],
        keep_count: 1,
        up_to: false,
        // No effect filter: every looked-at card is selectable.
        selectable_cards: vec![kept, rest0, rest1],
        kept_destination: Some(Zone::Battlefield),
        rest_destination: Some(Zone::Graveyard),
        source_id: None,
        enter_tapped: false,
    };

    runner
        .act(GameAction::SelectCards { cards: vec![kept] })
        .expect("submit the dig keep selection");

    // CR 303.4f: the Aura's entry surfaces an attachment-target choice (two legal
    // hosts).
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReturnAsAuraTarget { .. }
        ),
        "the kept Aura's battlefield entry must surface a CR 303.4f attachment choice, got {}",
        runner.waiting_for_kind()
    );

    // Answer the attachment choice — attach to host A.
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(host_a)),
        })
        .expect("answer the aura attachment choice");

    let state = runner.state();
    assert!(
        !matches!(state.waiting_for, WaitingFor::ReturnAsAuraTarget { .. }),
        "attachment choice must be resolved"
    );

    // The kept Aura entered the battlefield and attached.
    assert_eq!(
        state.objects[&kept].zone,
        Zone::Battlefield,
        "kept Aura must have entered the battlefield"
    );
    let _ = host_b;

    // The discriminating assertion: the rest pile must NOT be stranded in the
    // library. Both unkept cards must reach the graveyard.
    for &id in &[rest0, rest1] {
        assert_eq!(
            state.objects[&id].zone,
            Zone::Graveyard,
            "unkept dig card must reach the graveyard — it must not strand in the library on the kept-card pause"
        );
    }
    assert!(
        !state.players[1].library.contains(&rest0) && !state.players[1].library.contains(&rest1),
        "no unkept card may remain in the library"
    );
}
