//! CR 707.2 + CR 111.1 + CR 701.9a (analogous): `Effect::CreateTokenCopyFromPool`
//! resolver. Creates a token that's a copy of a creature card chosen from the
//! format-defined pool (`GameState::momir_pool` / `momir_pool_faces`) whose mana
//! value satisfies the effect's comparator against `mv_bound`. The canonical card
//! is the Momir Basic emblem; the comparator makes the same primitive express
//! "copy a creature card with mana value N or less" (Oko-style) via `LE`.
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
use crate::types::proposed_event::CopyTokenSpec;
use rand::seq::IndexedRandom;
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

    // 3. Gather candidate names from the pool by the comparator. `EQ` is the
    // direct keyed lookup (Momir); other comparators range over the BTreeMap.
    let candidates: Vec<String> = match mv {
        Comparator::EQ => state.momir_pool.get(&bound).cloned().unwrap_or_default(),
        Comparator::LE => state
            .momir_pool
            .range(..=bound)
            .flat_map(|(_, names)| names.iter().cloned())
            .collect(),
        Comparator::LT => state
            .momir_pool
            .range(..bound)
            .flat_map(|(_, names)| names.iter().cloned())
            .collect(),
        Comparator::GE => state
            .momir_pool
            .range(bound..)
            .flat_map(|(_, names)| names.iter().cloned())
            .collect(),
        Comparator::GT => state
            .momir_pool
            .range((std::ops::Bound::Excluded(bound), std::ops::Bound::Unbounded))
            .flat_map(|(_, names)| names.iter().cloned())
            .collect(),
        Comparator::NE => state
            .momir_pool
            .iter()
            .filter(|(mv, _)| **mv != bound)
            .flat_map(|(_, names)| names.iter().cloned())
            .collect(),
    };

    // 4. CR 205 + CR 111.5: Hydrate each candidate's face and keep only those
    // ELIGIBLE for copying — a creature card (CR 111.5: instants/sorceries make no
    // token; the pool is creature-only, but a future caller's filter may admit
    // others, so guard defensively) that ALSO satisfies the effect's
    // `type_filter` ("additional filter applied to the hydrated face"). Filtering
    // here, during candidate gathering, ensures the random pick (step 6) respects
    // `type_filter`. For Momir `type_filter` is `Any`, so this is a no-op narrowing.
    // Faces missing from the hydration map are dropped (cannot be copied).
    let eligible_faces: Vec<(String, CardFace)> = candidates
        .into_iter()
        .filter_map(|name| {
            let face = state.momir_pool_faces.get(&name.to_lowercase()).cloned()?;
            let is_creature = face.card_type.core_types.contains(&CoreType::Creature);
            let is_instant_or_sorcery = face
                .card_type
                .core_types
                .iter()
                .any(|t| matches!(t, CoreType::Instant | CoreType::Sorcery));
            (is_creature
                && !is_instant_or_sorcery
                && matches_target_filter_against_face(&face, &type_filter))
            .then_some((name, face))
        })
        .collect();

    // 5. CR 609.3: "Do as much as possible" — no eligible face creates no token
    // (a mana value with no qualifying creatures). Not an error.
    if eligible_faces.is_empty() {
        state.last_created_token_ids = Vec::new();
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    // 6. CR 608.2d: the effect selects an eligible candidate's face (at random
    //    for Momir; "chosen at random" has no dedicated CR, so the selection
    //    rule CR 608.2d governs here).
    let face = match selection {
        crate::types::ability::CardSelectionMode::Random => eligible_faces
            .choose(&mut state.rng)
            .map(|(_, face)| face.clone())
            .expect("non-empty eligible set"),
        crate::types::ability::CardSelectionMode::Chosen => {
            // RUNTIME: Momir Basic never uses `Chosen` selection. Interactive
            // "choose a creature card from the pool" is not built; this typed-but-
            // unhandled arm is a benign no-op so the primitive stays total.
            state.last_created_token_ids = Vec::new();
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::from(&ability.effect),
                source_id: ability.source_id,
                subject: None,
            });
            return Ok(());
        }
    };

    // 7. CR 707.2: Build copiable values directly from the face (no battlefield
    // source object exists for a pool pick).
    let values = copiable_values_from_face(&face);
    let printed_ref = printed_ref_from_face(&face);

    // 8. CR 109.4 + CR 111.2: Resolve the token's creator/owner and
    // the token count. NOTE: `count > 1` replicates the SINGLE random pick above
    // `count` times (all N tokens copy the same chosen face), NOT N independent
    // random picks. Momir Basic uses `count = 1`, so this is inert today.
    // Independent per-token picks ("create N random creature tokens") are a future
    // change: loop the step-4/6 selection `count` times, consuming `state.rng` in
    // order for determinism, and enqueue one `PendingCopyTokenBatch { count: 1 }`
    // per pick.
    let token_owner = resolve_token_owner(state, ability, &owner_filter);
    let count = resolve_quantity_with_targets(state, count, ability).max(0) as u32;

    // 9. Emit the copy through the SHARED replacement + apply path. The drain
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
