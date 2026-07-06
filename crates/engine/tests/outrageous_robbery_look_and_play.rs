//! Discriminating runtime regressions for **Outrageous Robbery**:
//!
//! > {X}{B}{B} Instant — Target opponent exiles the top X cards of their library
//! > face down. You may look at and play those cards for as long as they remain
//! > exiled. If you cast a spell this way, you may spend mana as though it were
//! > mana of any type to cast it.
//!
//! Unlike the imperative-voice sibling Expensive Taste ("Exile the top two
//! cards…"), Robbery uses the subject-voice "target opponent EXILES the top X …
//! face down", which routed through the `<player> exiles the top` parser arm.
//! Three gaps were fixed there (`oracle_effect/mod.rs`): the arm hard-coded
//! `face_down: false`, `parse_subject_exile_top_count` did not recognise the
//! cost's `X`, and the "any type" mana rider (vs. "any color") was unrecognised
//! and fell to `Effect::Unimplemented`. The runtime look-permission is a shared
//! building block: a `PlayFromExile` grantee may look at the face-down card they
//! may play (`casting::player_may_look_at_facedown_exile`, consumed by
//! `visibility.rs`) — CR 406.3a / CR 406.3b.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::visibility::filter_state_for_viewer;
use engine::game::zones::create_object;
use engine::types::ability::{CastingPermission, Duration, ManaSpendPermission};
use engine::types::identifiers::CardId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const ORACLE: &str = "Target opponent exiles the top X cards of their library face down. \
You may look at and play those cards for as long as they remain exiled. \
If you cast a spell this way, you may spend mana as though it were mana of any type to cast it.";

fn play_from_exile_for(
    permissions: &[CastingPermission],
    grantee: PlayerId,
) -> Option<&CastingPermission> {
    permissions.iter().find(|p| {
        matches!(
            p,
            CastingPermission::PlayFromExile { granted_to, .. } if *granted_to == grantee
        )
    })
}

/// Claim 1: X + face-down + any-type play grant (drives the real cast pipeline).
///
/// Revert-failing:
///   * X-count fix reverted   → only 1 card exiled → the `top_b` zone assert fails.
///   * face-down fix reverted → `face_down` is false → the face-down assert fails.
///   * "any type" scan reverted → `mana_spend_permission` is `None` → assert fails.
#[test]
fn robbery_exiles_x_facedown_and_grants_any_type_play() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let deep = scenario.add_card_to_library_top(P1, "Opp Deep");
    let exiled: Vec<_> = ["Opp Top B", "Opp Top A"]
        .into_iter()
        .map(|n| scenario.add_card_to_library_top(P1, n))
        .collect();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Outrageous Robbery", false, ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                ManaType::Colorless,
                engine::types::identifiers::ObjectId(9_999),
                false,
                vec![]
            );
            2
        ],
    );

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).x(2).target_player(P1).resolve();

    for id in &exiled {
        assert_eq!(
            outcome.zone_of(*id),
            Zone::Exile,
            "X=2 must exile the top TWO opponent library cards"
        );
        let obj = &outcome.state().objects[id];
        assert!(obj.face_down, "exiled cards must be face down (CR 406.3)");
        let perm = play_from_exile_for(&obj.casting_permissions, P0).unwrap_or_else(|| {
            panic!(
                "exiled card must carry P0's PlayFromExile grant, got {:?}",
                obj.casting_permissions
            )
        });
        match perm {
            CastingPermission::PlayFromExile {
                duration,
                mana_spend_permission,
                ..
            } => {
                assert_eq!(
                    *duration,
                    Duration::Permanent,
                    "grant persists while exiled"
                );
                assert_eq!(
                    *mana_spend_permission,
                    Some(ManaSpendPermission::AnyTypeOrColor),
                    "the 'spend mana as though any type' rider must ride the grant (CR 609.4b)"
                );
            }
            _ => unreachable!(),
        }
    }

    // The buried card stays put and ungranted.
    assert_eq!(outcome.zone_of(deep), Zone::Library);
    assert!(outcome.state().objects[&deep]
        .casting_permissions
        .is_empty());
}

/// Claim 2 (PROVENANCE — load-bearing): the look permission is grant-scoped.
///
/// The caster (P0, the grantee) sees the two Robbery-exiled cards in their
/// redacted view; the opponent (P1 — the OWNER, but no grant) does not; and a
/// face-down card exiled in the SAME zone by a different source (no grant to P0)
/// stays hidden from P0. This proves the reveal is driven by the `granted_to`
/// scoped `PlayFromExile` grant, not a blanket "any face-down exile is visible"
/// nor an owner-based reveal.
///
/// Revert-failing: revert the `visibility.rs` predicate consumption and P0's
/// view redacts the Robbery cards to "Hidden Card" → the P0 reveal asserts flip.
#[test]
fn robbery_look_permission_is_grant_scoped() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P1, "Opp Deep");
    let exiled: Vec<_> = ["Opp Top B", "Opp Top A"]
        .into_iter()
        .map(|n| scenario.add_card_to_library_top(P1, n))
        .collect();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Outrageous Robbery", false, ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                ManaType::Colorless,
                engine::types::identifiers::ObjectId(9_999),
                false,
                vec![]
            );
            2
        ],
    );

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).x(2).target_player(P1).resolve();

    // Original names captured pre-redaction.
    let names: Vec<String> = exiled
        .iter()
        .map(|id| outcome.state().objects[id].name.clone())
        .collect();

    // P0 (grantee) view: the Robbery cards keep their real identity.
    let p0_view = filter_state_for_viewer(outcome.state(), P0);
    for (id, name) in exiled.iter().zip(&names) {
        assert_eq!(
            &p0_view.objects[id].name, name,
            "caster P0 must see the cards they may play (CR 406.3b), not 'Hidden Card'"
        );
    }

    // P1 (owner, but no grant) view: the same cards are redacted.
    let p1_view = filter_state_for_viewer(outcome.state(), P1);
    for id in &exiled {
        assert_eq!(
            p1_view.objects[id].name, "Hidden Card",
            "the targeted opponent has no grant and must NOT see the face-down cards"
        );
    }

    // A face-down card exiled by a DIFFERENT source (no grant to P0) stays hidden
    // from P0 — proves the reveal is grant-scoped, not blanket.
    let mut injected = outcome.state().clone();
    let control = create_object(
        &mut injected,
        CardId(9_999),
        P1,
        "Control FaceDown".to_string(),
        Zone::Exile,
    );
    injected.objects.get_mut(&control).unwrap().face_down = true;
    let p0_view_injected = filter_state_for_viewer(&injected, P0);
    assert_eq!(
        p0_view_injected.objects[&control].name, "Hidden Card",
        "a face-down exile with no P0 grant must stay hidden from P0 (no blanket reveal)"
    );
}

/// Claim 3 (DURATION): the play + look permission holds "for as long as they
/// remain exiled". After crossing into the opponent's turn, an un-played exiled
/// card still carries the grant and is still lookable by the caster.
///
/// Revert-failing: revert the `visibility.rs` predicate and the card is no
/// longer lookable next turn; a duration regression to until-end-of-turn would
/// drop the grant at cleanup and flip the permission assert.
#[test]
fn robbery_play_permission_persists_into_opponent_turn() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P1, "Opp Deep");
    let exiled: Vec<_> = ["Opp Top B", "Opp Top A"]
        .into_iter()
        .map(|n| scenario.add_card_to_library_top(P1, n))
        .collect();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Outrageous Robbery", false, ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(
                ManaType::Colorless,
                engine::types::identifiers::ObjectId(9_999),
                false,
                vec![]
            );
            2
        ],
    );

    let mut runner = scenario.build();
    let name = {
        let outcome = runner.cast(spell).x(2).target_player(P1).resolve();
        outcome.state().objects[&exiled[0]].name.clone()
    };

    // Cross the turn boundary into P1's upkeep (past P0's cleanup step).
    runner.advance_to_upkeep();

    let card = &runner.state().objects[&exiled[0]];
    assert_eq!(card.zone, Zone::Exile, "card is still exiled");
    assert!(
        play_from_exile_for(&card.casting_permissions, P0).is_some(),
        "the PlayFromExile grant persists into the next turn (Permanent duration)"
    );

    let p0_view = filter_state_for_viewer(runner.state(), P0);
    assert_eq!(
        p0_view.objects[&exiled[0]].name, name,
        "the caster can still look at the un-played exiled card next turn"
    );
}
