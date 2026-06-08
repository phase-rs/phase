//! Encore (CR 702.141) — graveyard-activated, per-opponent token-copy generator.
//!
//! CR 702.141a: "Encore [cost]" is an activated ability that functions only
//! while the card with encore is in a graveyard. It means:
//!
//! > "[Cost], Exile this card from your graveyard: For each opponent, create a
//! > token that's a copy of this card that attacks that opponent this turn if
//! > able. The tokens gain haste. Sacrifice them at the beginning of the next
//! > end step. Activate only as a sorcery."
//!
//! The exile-from-graveyard is paid as part of the activation cost (composed in
//! `database/encore.rs`), so by the time this resolver runs the source card is
//! in exile. It keeps its `ObjectId` across the graveyard → exile cost move
//! (`game/zones.rs::move_to_zone` mutates the zone in place), so
//! `Effect::CopyTokenOf { target: SelfRef }` still resolves to the exiled card
//! and `compute_current_copiable_values` reads its printed copiable values from
//! the exile zone — exactly as Embalm / Eternalize rely on.
//!
//! This resolver mirrors `effects/myriad.rs` (CR 702.116a), Encore's closest
//! sibling: both create one copy token per opponent and bind each token to that
//! opponent. The differences are encoded here rather than in any shared effect:
//!
//! - **Per-opponent must-attack, not enters-attacking.** Encore is activated at
//!   sorcery speed (typically in a main phase, outside combat), so the tokens
//!   are *not* created already attacking. Instead each token gains a
//!   `MustAttackPlayer { player }` requirement (CR 508.1d) bound to the opponent
//!   it was created for, lasting until end of turn (CR 702.141a "this turn").
//!   Binding the requirement per opponent — instead of via the generic
//!   `Effect::ForceAttack`, whose `required_player` context ref has no
//!   "the opponent" resolution and would fall back to the controller — is the
//!   reason Encore needs a dedicated resolver.
//! - **Haste is baked into the copy** via `extra_keywords` (CR 707.2, the
//!   Twinflame "…except it has haste" channel), so each token has haste from
//!   the moment it is created.
//! - **Sacrifice at the next end step**, composed as a one-shot delayed trigger
//!   (CR 603.7d) over the exact token IDs created here, rather than Myriad's
//!   end-of-combat exile.

use crate::game::players;
use crate::types::ability::{
    ContinuousModification, DelayedTriggerCondition, Duration, Effect, EffectError, EffectKind,
    QuantityExpr, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{DelayedTrigger, GameState};
use crate::types::keywords::Keyword;
use crate::types::phase::Phase;
use crate::types::statics::StaticMode;

/// CR 702.141a: Resolve a card's Encore ability — for each opponent of the
/// activating player, create a haste-bearing token copy of the exiled source
/// card that must attack that opponent this turn, then sacrifice every created
/// token at the beginning of the next end step.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    if !matches!(ability.effect, Effect::Encore) {
        return Err(EffectError::MissingParam("Encore".to_string()));
    }

    // CR 702.141a + CR 102.1: "for each opponent" — every other player who is
    // an opponent of the activating player at resolution.
    let opponents = players::opponents(state, ability.controller);
    let mut created = Vec::new();

    for opponent in opponents {
        // CR 707.2: Create a token that's a copy of the exiled source card
        // (`SelfRef`), under the activating player's control (`Controller`).
        // CR 702.141a: "The tokens gain haste" — granted as a copy exception via
        // `extra_keywords`, so each token has haste from creation.
        let copy_effect = Effect::CopyTokenOf {
            target: TargetFilter::SelfRef,
            owner: TargetFilter::Controller,
            source_filter: None,
            enters_attacking: false,
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            extra_keywords: vec![Keyword::Haste],
            additional_modifications: vec![],
        };
        let copy_ability =
            ResolvedAbility::new(copy_effect, vec![], ability.source_id, ability.controller);
        crate::game::effects::token_copy::resolve(state, &copy_ability, events)?;

        // CR 702.141a + CR 508.1d: each token created for this opponent "attacks
        // that opponent this turn if able." Bind a `MustAttackPlayer` requirement
        // to the freshly-created token(s) for the rest of the turn.
        for token_id in state.last_created_token_ids.clone() {
            state.add_transient_continuous_effect(
                ability.source_id,
                ability.controller,
                Duration::UntilEndOfTurn,
                TargetFilter::SpecificObject { id: token_id },
                vec![ContinuousModification::AddStaticMode {
                    mode: StaticMode::MustAttackPlayer { player: opponent },
                }],
                None,
            );
            created.push(token_id);
        }
    }

    // CR 702.141a + CR 603.7d + CR 513.2: "Sacrifice them at the beginning of the
    // next end step." A one-shot delayed trigger that sacrifices exactly the
    // tokens created above (bound as explicit object targets, so no player
    // choice is involved — the `Effect::Sacrifice` resolver sacrifices each
    // targeted permanent directly).
    if !created.is_empty() {
        let sacrifice = ResolvedAbility::new(
            Effect::Sacrifice {
                target: TargetFilter::Any,
                count: QuantityExpr::Fixed {
                    value: created.len() as i32,
                },
                min_count: 0,
            },
            created.iter().copied().map(TargetRef::Object).collect(),
            ability.source_id,
            ability.controller,
        );
        state.delayed_triggers.push(DelayedTrigger {
            condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
            ability: sacrifice,
            controller: ability.controller,
            source_id: ability.source_id,
            one_shot: true,
        });
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Encore,
        source_id: ability.source_id,
    });
    Ok(())
}
