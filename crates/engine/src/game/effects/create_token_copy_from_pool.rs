//! CR 707.2 + CR 111.1 + CR 701.9a (analogous): `Effect::CreateTokenCopyFromPool`
//! resolver. Creates a token that's a copy of a creature card drawn from the
//! loaded card database (`GameState::card_db`) whose mana value satisfies the
//! effect's comparator against `mv_bound`. The canonical card is the Momir Basic
//! emblem; the comparator makes the same primitive express "copy a creature card
//! with mana value N or less" (Oko-style) via `LE`.
//!
//! The draw happens HERE, at resolution, rather than from a pool materialized
//! into `GameState` up front: the candidate set is the entire creature corpus
//! (~19,500 faces), which the card database already owns, and the ability needs
//! exactly one face per activation. Selection walks
//! `CardDatabase::faces_in_scan_order` — a fixed order for any two loads of the
//! same card data — and consumes exactly one RNG word, so every peer and every
//! replay of a given seed draws the same creature.
//!
//! The copy source exists only as a `CardFace` (no battlefield object), so the
//! resolver builds `CopiableValues` directly from the face via
//! `copiable_values_from_face`, then routes the result through the SHARED copy-
//! token apply path (`token_copy::drive_copy_token_batches`) so the replacement
//! pipeline and token construction are never duplicated.

use crate::game::effects::token::resolve_token_owner;
use crate::game::effects::token_copy::drive_copy_token_batches;
use crate::game::filter::matches_target_filter_against_face;
use crate::game::game_object::DisplaySource;
use crate::game::printed_cards::{copiable_values_from_face, printed_ref_from_face};
use crate::game::quantity::resolve_quantity_with_targets;
use crate::types::ability::{Comparator, EffectError, EffectKind, ResolvedAbility};
use crate::types::card::CardFace;
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::game_state::PendingCopyTokenBatch;
use crate::types::mana::ManaCost;
use crate::types::proposed_event::CopyTokenSpec;
use rand::Rng;
use std::collections::VecDeque;

pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // 1. Destructure the typed fields from the effect.
    let crate::types::ability::Effect::CreateTokenCopyFromPool {
        owner,
        type_filter,
        mv,
        mv_bound,
        selection,
        count,
        tapped,
        enters_attacking,
    } = &ability.effect
    else {
        return Err(EffectError::MissingParam(
            "CreateTokenCopyFromPool".to_string(),
        ));
    };
    let (mv, selection, tapped, enters_attacking) = (*mv, *selection, *tapped, *enters_attacking);
    let owner_filter = owner.clone();
    let type_filter = type_filter.clone();

    // 2. CR 202.3: Resolve the mana-value bound. `resolve_quantity_with_targets`
    // threads `ability.chosen_x`, so the Momir `Variable { "X" }` bound reads the
    // X the activator paid.
    let bound = resolve_quantity_with_targets(state, mv_bound, ability);

    // 3. CR 707.2 + CR 202.3: the draw source. Cloning the handle is an `Arc`
    // bump; it also releases the borrow on `state` so `state.rng` is free below.
    let Some(db) = state.card_db.clone() else {
        // A missing handle is an installation bug, not a game situation:
        // `install_card_db` runs on every path that builds or restores a game.
        // Resolve as CR 609.3 "do as much as possible" rather than aborting a
        // live game, but say so loudly — silently making no token is exactly
        // the failure mode that would otherwise go unnoticed.
        tracing::error!(
            "CreateTokenCopyFromPool resolved with no card database installed on GameState; \
             no token created. Every path that builds or restores a game must call \
             game::install_card_db."
        );
        return no_token(state, ability, events);
    };

    // 4. CR 205 + CR 111.5 + CR 202.3: one predicate decides eligibility, used
    // by BOTH the count and the pick below so they can never disagree.
    let eligible = |face: &&CardFace| face_is_eligible(face, mv, bound, &type_filter);

    // 5. CR 608.2d: "chosen at random" has no dedicated CR, so the selection
    // rule CR 608.2d governs. Count first, draw one index, then walk to it —
    // this allocates nothing and consumes exactly one RNG word regardless of
    // how large the candidate set is.
    let face = match selection {
        crate::types::ability::CardSelectionMode::Random => {
            let candidates = db.faces_in_scan_order().filter(eligible).count();
            // CR 609.3: a mana value with no qualifying creatures creates no
            // token. Not an error.
            if candidates == 0 {
                return no_token(state, ability, events);
            }
            let index = state.rng.random_range(0..candidates);
            db.faces_in_scan_order()
                .filter(eligible)
                .nth(index)
                .expect("index drawn from the counted candidate set")
                .clone()
        }
        crate::types::ability::CardSelectionMode::Chosen => {
            // RUNTIME: Momir Basic never uses `Chosen` selection. Interactive
            // "choose a creature card from the pool" is not built; this typed-but-
            // unhandled arm is a benign no-op so the primitive stays total.
            return no_token(state, ability, events);
        }
    };

    // 6. CR 707.2: Build copiable values directly from the face (no battlefield
    // source object exists for a pool pick).
    let values = copiable_values_from_face(&face);
    let printed_ref = printed_ref_from_face(&face);

    // 7. CR 109.4 + CR 111.2: Resolve the token's creator/owner and
    // the token count. NOTE: `count > 1` replicates the SINGLE random pick above
    // `count` times (all N tokens copy the same chosen face), NOT N independent
    // random picks. Momir Basic uses `count = 1`, so this is inert today.
    // Independent per-token picks ("create N random creature tokens") are a future
    // change: loop the step-5 draw `count` times, consuming `state.rng` in
    // order for determinism, and enqueue one `PendingCopyTokenBatch { count: 1 }`
    // per pick.
    let token_owner = resolve_token_owner(state, ability, &owner_filter);
    let count = resolve_quantity_with_targets(state, count, ability).max(0) as u32;

    // 8. Emit the copy through the SHARED replacement + apply path. The drain
    // (`drive_copy_token_batches` -> `drain_copy_token_resolution`) rebuilds the
    // probe `TokenSpec` from `copy.values` via `copy_probe_spec_for` internally,
    // so we only assemble the `PendingCopyTokenBatch` here.
    let mut remaining = VecDeque::with_capacity(1);
    remaining.push_back(PendingCopyTokenBatch {
        owner: token_owner,
        count,
        copy: Box::new(CopyTokenSpec {
            values: Box::new(values),
            display_source: DisplaySource::Card,
            printed_ref,
            token_image_ref: None,
            extra_keywords: Vec::new(),
            additional_modifications: Vec::new(),
            tapped,
            enters_attacking,
            sacrifice_at: ability.duration.clone(),
            source_id: ability.source_id,
            controller: ability.controller,
        }),
    });

    drive_copy_token_batches(
        state,
        remaining,
        EffectKind::from(&ability.effect),
        ability.source_id,
        events,
    );

    Ok(())
}

/// CR 609.3 "do as much as possible": resolve without creating a token.
///
/// Shared by every branch that finds nothing to copy so they emit an identical
/// `EffectResolved` and clear `last_created_token_ids` the same way.
fn no_token(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    state.last_created_token_ids = Vec::new();
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    });
    Ok(())
}

/// Whether `face` may be copied by this effect. The SINGLE authority for
/// candidate eligibility — public so tests can assert the class-general
/// property ("no costless back face is ever pickable") directly, instead of
/// sampling the random draw and hoping to hit an offender — the random draw counts and picks through it, so a
/// change here can never make the two disagree.
///
/// - CR 111.5: a token copy of a non-creature card makes no creature, and the
///   caller's pool is creature-only; instants/sorceries are excluded explicitly
///   so a future caller's `type_filter` cannot admit them.
/// - CR 202.1b + CR 202.3b + CR 712.8a: `faces_in_scan_order` yields BOTH faces
///   of every multi-face card, so a transform/flip/meld BACK face (no printed
///   mana cost -> `ManaCost::NoCost`, mana value 0) would otherwise be drawable
///   at mana value 0. Outside the battlefield a DFC has only its front face's
///   characteristics, so a back face is never a valid pick. Excluded by its data
///   signal: only an ABSENT manaCost maps to `NoCost`, so modal-DFC creature
///   backs (explicit cost -> `Cost{..}`) and genuine `{0}` creatures
///   (`Cost{generic:0}`) are preserved.
/// - CR 202.3: the effect's comparator against the resolved mana-value bound.
pub fn face_is_eligible(
    face: &CardFace,
    mv: Comparator,
    bound: i32,
    type_filter: &crate::types::ability::TargetFilter,
) -> bool {
    if !face.card_type.core_types.contains(&CoreType::Creature) {
        return false;
    }
    if face
        .card_type
        .core_types
        .iter()
        .any(|t| matches!(t, CoreType::Instant | CoreType::Sorcery))
    {
        return false;
    }
    if matches!(face.mana_cost, ManaCost::NoCost) {
        return false;
    }
    let mana_value = face.mana_cost.mana_value() as i32;
    let matches_bound = match mv {
        Comparator::EQ => mana_value == bound,
        Comparator::NE => mana_value != bound,
        Comparator::LE => mana_value <= bound,
        Comparator::LT => mana_value < bound,
        Comparator::GE => mana_value >= bound,
        Comparator::GT => mana_value > bound,
    };
    matches_bound && matches_target_filter_against_face(face, type_filter)
}
