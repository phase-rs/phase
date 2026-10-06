//! PR #8494 review finding: a `TargetFilter::LastCreated` perpetual grant whose
//! antecedent does not exist must apply to NOTHING — never to the ability source.
//!
//! `publishes_chain_created_referent` (`parser/oracle_effect/lower.rs`) makes
//! `Effect::Conjure` publish the chain-created referent, so a following bare "it"
//! ("Conjure a duplicate … into your hand. It perpetually gains \"~ can't
//! block.\"") lowers to `Effect::ApplyPerpetual { target: LastCreated, .. }`.
//! When the conjure creates ZERO objects, `conjure::resolve` still ASSIGNS
//! `state.last_created_token_ids = created_ids` at its tail — an empty vec — so
//! the anaphor resolves to nothing.
//!
//! CR 609.3 ("If an effect attempts to do something impossible, it does only as
//! much as possible"): the grant then does nothing. The pre-fix code fell through
//! `perpetual_target_object_ids`' empty-result guard (which exempted only
//! `ParentTarget`) into `ids.push(ability.source_id)` and installed the grant on
//! the ability's own source — a different object the sentence never mentions.
//!
//! Fixture shape mirrors the in-file
//! `perpetual_grant_after_conjure_installs_on_conjured_object` unit test
//! (`game/effects/perpetual.rs`): the two effects are resolved directly, because
//! the pronoun binding under test lives in the resolver, not in the cast pipeline.
//! `targets` is deliberately EMPTY on the grant — `perpetual_target_object_ids`
//! short-circuits on a non-empty `ability.targets` and would never reach the
//! `LastCreated` lookup. Conjure declares no targets, so an empty vec is the
//! production-faithful fixture.

use engine::game::effects::{conjure, perpetual};
use engine::game::zones::create_object;
use engine::types::ability::{
    ConjureCard, ConjureSource, Effect, PerpetualGrantModification, PerpetualModification,
    QuantityExpr, ResolvedAbility, TargetFilter,
};
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

/// The Karlach/Agent of Raffine rider shape: a quoted body classifying to
/// `AddStaticMode { CantBlock }` (CR 509.1b).
fn cant_block_grant() -> PerpetualModification {
    PerpetualModification::GrantAbility {
        modifications: vec![PerpetualGrantModification::AddStaticMode {
            mode: StaticMode::CantBlock,
        }],
    }
}

fn conjure_ability(source_id: ObjectId, count: i32) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::Conjure {
            cards: vec![ConjureCard {
                source: ConjureSource::Named {
                    name: "Conjured Duplicate".to_string(),
                },
                count: QuantityExpr::Fixed { value: count },
            }],
            destination: Zone::Hand,
            tapped: false,
            library_position: None,
            library_players: None,
        },
        vec![],
        source_id,
        PlayerId(0),
    )
}

fn last_created_grant(source_id: ObjectId) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::ApplyPerpetual {
            target: TargetFilter::LastCreated,
            modification: cant_block_grant(),
        },
        vec![],
        source_id,
        PlayerId(0),
    )
}

fn has_cant_block(state: &GameState, id: ObjectId) -> bool {
    // `iter_unchecked` (not the crate-private `iter_all`) is the public,
    // classification-side accessor available at the integration-test boundary.
    state.objects[&id]
        .static_definitions
        .iter_unchecked()
        .any(|sd| sd.mode == StaticMode::CantBlock)
}

/// A conjure that creates NOTHING leaves the "it" anaphor without a referent, so
/// the perpetual grant must install on no object at all — in particular not on
/// the ability source (CR 609.3).
#[test]
fn zero_count_conjure_leaves_last_created_perpetual_grant_with_no_subject() {
    let mut state = GameState::new_two_player(7);
    let source_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Agent of Raffine".to_string(),
        Zone::Battlefield,
    );

    let mut events = Vec::new();
    conjure::resolve(&mut state, &conjure_ability(source_id, 0), &mut events).unwrap();

    // Reach-guard: the conjure really produced nothing AND really cleared the
    // referent slot, so the assertion below is about the empty-anaphor path and
    // not about a stale ledger entry from some earlier resolution.
    assert!(
        state.last_created_token_ids.is_empty(),
        "a zero-count conjure must publish no chain-created referent"
    );
    assert!(
        !has_cant_block(&state, source_id),
        "baseline: the source has no CantBlock static before the grant resolves"
    );

    perpetual::resolve(&mut state, &last_created_grant(source_id), &mut events).unwrap();

    assert!(
        !has_cant_block(&state, source_id),
        "an unbound LastCreated anaphor must not redirect the perpetual grant onto \
         the ability source (CR 609.3: the effect does only as much as possible)"
    );
    assert!(
        state.objects[&source_id].perpetual_mods.is_empty(),
        "no perpetual modification may be recorded when the grant has no subject"
    );
}

/// Discriminating control for the test above: with the SAME fixture and a
/// non-zero count, the grant DOES land — on the conjured object, never on the
/// source. Without this, the zero-count assertion would also pass if
/// `LastCreated` had simply stopped resolving at all.
#[test]
fn nonzero_conjure_still_binds_the_last_created_perpetual_grant() {
    let mut state = GameState::new_two_player(7);
    let source_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Agent of Raffine".to_string(),
        Zone::Battlefield,
    );

    let mut events = Vec::new();
    conjure::resolve(&mut state, &conjure_ability(source_id, 1), &mut events).unwrap();

    let conjured_id = *state
        .last_created_token_ids
        .first()
        .expect("a one-count conjure publishes the conjured object as the referent");
    assert_ne!(
        conjured_id, source_id,
        "the conjured object must be distinct from the ability source"
    );

    perpetual::resolve(&mut state, &last_created_grant(source_id), &mut events).unwrap();

    assert!(
        has_cant_block(&state, conjured_id),
        "the CONJURED object receives the grant"
    );
    assert!(
        !has_cant_block(&state, source_id),
        "the ability source must never receive a grant meant for the conjured card"
    );
}
