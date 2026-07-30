//! CR 602.5b + CR 602.5c — Locus of Enlightenment's "You may activate each of
//! those abilities only once each turn" use-restriction, enforced end-to-end
//! through the REAL activation pipeline (`GameAction::ActivateAbility` →
//! `apply()`), not just the parsed AST shape.
//!
//! Locus grants the host *all activated abilities of the exiled cards used to
//! craft it* and caps each granted ability at one activation per turn. The cap is
//! carried on `ContinuousModification::GrantAllActivatedAbilitiesOf { cap }` and
//! injected into every donated ability's `activation_restrictions` during the
//! layer-6 expansion (`expand_granted_activated_abilities`); the existing
//! per-`(recipient, ability_index)` enforcement in `game/restrictions.rs` then
//! applies it.
//!
//! Discrimination strategy:
//!   - The donated ability is a FREE, NON-TAP mana ability ("Add {G}"). Free +
//!     non-tap means that, absent the cap, it is activatable repeatedly the same
//!     turn — so the once-per-turn cap is the ONLY thing that can block a second
//!     activation. (A tap cost would mask the cap by tapping the source; an
//!     unpayable cost would block reuse for an unrelated reason.)
//!   - CAPPED grant: first `ActivateAbility` succeeds, the SECOND the same turn is
//!     rejected by `apply()`. After the per-turn counter resets (turn boundary,
//!     CR 602.5b — `turns.rs` `activated_abilities_this_turn.clear()`), it is
//!     available again — proving the cap is per-turn, not permanent.
//!   - UNCAPPED grant (`cap: None`, the required default for Myr Welder / Agatha /
//!     Marvin / …): the second activation the same turn succeeds. This is also the
//!     revert-probe contrast — if the layer injection is reverted (cap never
//!     pushed), the CAPPED test's "second activation rejected" assertion flips to
//!     allowed and fails.

use std::sync::Arc;

use engine::game::casting::can_activate_ability_now;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::GameRunner;
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ActivationRestriction, ContinuousModification, Effect,
    ManaContribution, ManaProduction, StaticDefinition, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{ExileLink, ExileLinkKind, GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);

/// A free, non-tap mana ability — "Add {G}". No tap cost (so reuse is not masked
/// by the source tapping) and no payable cost (so reuse is not masked by running
/// out of a resource): absent the cap, it can be activated any number of times the
/// same turn.
fn donated_mana_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: ManaProduction::Fixed {
                colors: vec![ManaColor::Green],
                contribution: ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
}

/// Build a battlefield with Locus (the grant host) controlling P0, a craft-material
/// card in exile linked via `CraftMaterial` carrying [`donated_mana_ability`], and
/// P0 holding priority in their precombat main phase. Materializes the layer-6
/// grant and returns `(state, locus_id, granted_ability_index_on_locus)`.
fn build_locus_state(cap: Option<ActivationRestriction>) -> (GameState, ObjectId, usize) {
    let mut state = GameState::new_two_player(7);
    state.phase = Phase::PreCombatMain;
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };

    // Locus of Enlightenment — the recipient/host carrying the grant static.
    let locus = create_object(
        &mut state,
        CardId(1000),
        P0,
        "Locus of Enlightenment".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&locus).unwrap();
        obj.card_types.core_types = vec![CoreType::Artifact];
        obj.base_card_types = obj.card_types.clone();
        obj.static_definitions = vec![StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::GrantAllActivatedAbilitiesOf {
                source: TargetFilter::ExiledBySource,
                cap,
            }])]
        .into();
    }

    // A craft-material card in exile, linked to Locus via CraftMaterial, carrying
    // the donated mana ability (CR 702.167c — "the exiled cards used to craft it").
    let material = create_object(
        &mut state,
        CardId(2000),
        P0,
        "Craft Material".to_string(),
        Zone::Exile,
    );
    {
        let obj = state.objects.get_mut(&material).unwrap();
        obj.card_types.core_types = vec![CoreType::Creature];
        obj.base_card_types = obj.card_types.clone();
        obj.abilities = Arc::new(vec![donated_mana_ability()]);
    }
    state.exile_links.push(ExileLink {
        exiled_id: material,
        source_id: locus,
        kind: ExileLinkKind::CraftMaterial,
    });

    // Materialize the layer-6 grant so the donated ability lands on Locus.
    evaluate_layers(&mut state);
    let idx = granted_index(&state, locus);
    (state, locus, idx)
}

/// Index of the donated mana ability that the grant expansion appended to Locus.
fn granted_index(state: &GameState, locus: ObjectId) -> usize {
    state.objects[&locus]
        .abilities
        .iter()
        .position(|a| {
            matches!(a.kind, AbilityKind::Activated)
                && matches!(a.effect.as_ref(), Effect::Mana { .. })
        })
        .expect("Locus must have been granted the craft material's activated mana ability")
}

/// CR 602.5b + CR 602.5c: with the cap, the donated ability is activatable exactly
/// once per turn — the second activation the same turn is rejected by the real
/// `apply()` pipeline, and it becomes available again after the per-turn reset.
#[test]
fn locus_capped_grant_blocks_second_activation_same_turn() {
    let (state, locus, idx) = build_locus_state(Some(ActivationRestriction::OnlyOnceEachTurn));

    // The grant must actually carry the cap into the donated def (sanity: this is
    // what the layer injection produces and what enforcement reads).
    assert!(
        state.objects[&locus].abilities[idx]
            .activation_restrictions
            .contains(&ActivationRestriction::OnlyOnceEachTurn),
        "the donated ability must carry the injected OnlyOnceEachTurn restriction"
    );

    let mut runner = GameRunner::from_state(state);

    // Legal-action path agrees it is activatable before any activation.
    assert!(
        can_activate_ability_now(runner.state(), P0, locus, idx),
        "before any activation the donated ability must be activatable"
    );

    // First activation — through the REAL GameAction::ActivateAbility apply path.
    runner
        .act(GameAction::ActivateAbility {
            source_id: locus,
            ability_index: idx,
        })
        .expect("first activation of the donated ability must be accepted");

    // Second activation, SAME turn — must be rejected by the cap. This is the
    // load-bearing, revert-failing assertion: drop the layer injection and this
    // becomes Ok (the uncapped contrast test below proves the same call succeeds
    // when cap is None).
    let second = runner.act(GameAction::ActivateAbility {
        source_id: locus,
        ability_index: idx,
    });
    assert!(
        second.is_err(),
        "CR 602.5b: a SECOND same-turn activation of the once-per-turn-capped \
         donated ability must be rejected, got {second:?}"
    );
    assert!(
        !can_activate_ability_now(runner.state(), P0, locus, idx),
        "CR 602.5b: the capped donated ability must not be activatable a second \
         time this turn (legal-action path)"
    );

    // CR 602.5b: the per-turn counter resets at the turn boundary
    // (`turns.rs` clears `activated_abilities_this_turn`). After the reset the
    // ability is available again — proving the cap is per-turn, not permanent.
    runner.state_mut().activated_abilities_this_turn.clear();
    assert!(
        can_activate_ability_now(runner.state(), P0, locus, idx),
        "CR 602.5b: after the per-turn reset the donated ability is available again"
    );
    runner
        .act(GameAction::ActivateAbility {
            source_id: locus,
            ability_index: idx,
        })
        .expect("after the per-turn reset the donated ability must be activatable again");
}

/// Discriminating contrast + revert-probe: an UNCAPPED grant (`cap: None`, the
/// required default for Myr Welder / Agatha's Soul Cauldron / Marvin / …) donates
/// the SAME ability with no use-restriction, so it can be activated repeatedly the
/// same turn. If the cap layer-injection were applied unconditionally (a bug), or
/// if this default were `Some`, the second activation would wrongly fail.
#[test]
fn uncapped_grant_allows_repeated_activation_same_turn() {
    let (state, locus, idx) = build_locus_state(None);

    assert!(
        state.objects[&locus].abilities[idx]
            .activation_restrictions
            .is_empty(),
        "an uncapped grant must donate the ability with no activation restriction"
    );

    let mut runner = GameRunner::from_state(state);

    runner
        .act(GameAction::ActivateAbility {
            source_id: locus,
            ability_index: idx,
        })
        .expect("first activation of the uncapped donated ability must be accepted");

    // Second activation the SAME turn must STILL be accepted — no cap.
    runner
        .act(GameAction::ActivateAbility {
            source_id: locus,
            ability_index: idx,
        })
        .expect(
            "CR 602.5b: an uncapped donated ability must be activatable repeatedly the same turn",
        );
    assert!(
        can_activate_ability_now(runner.state(), P0, locus, idx),
        "the uncapped donated ability remains activatable after two same-turn activations"
    );
}
