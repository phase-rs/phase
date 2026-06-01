use rand::Rng;

use crate::game::quantity::resolve_quantity;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

use super::resolve_ability_chain;

/// CR 705: Flip a coin and optionally execute win/lose effects.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (win_effect, lose_effect) = match &ability.effect {
        Effect::FlipCoin {
            win_effect,
            lose_effect,
        } => (win_effect.as_deref(), lose_effect.as_deref()),
        _ => return Err(EffectError::MissingParam("FlipCoin".to_string())),
    };

    // CR 705.1: Flip a coin using the game's seeded RNG.
    let won = state.rng.random_bool(0.5);

    events.push(GameEvent::CoinFlipped {
        player_id: ability.controller,
        won,
    });

    // CR 705.2: Execute the appropriate branch. Use the canonical converter so
    // the branch's `optional`, `sub_ability`, `condition`, and `duration` survive
    // — `ResolvedAbility::new` would discard them, dropping e.g. Ral, Monsoon
    // Mage's "you may exile Ral" prompt and his return-transformed sub-ability
    // (CR 712.8e: a nonmodal double-faced permanent put onto the battlefield
    // transformed has its back face up).
    let branch = if won { win_effect } else { lose_effect };
    let prior_waiting_for = state.waiting_for.clone();
    if let Some(def) = branch {
        let sub = crate::game::ability_utils::build_resolved_from_def_with_targets(
            def,
            ability.source_id,
            ability.controller,
            ability.targets.clone(),
        );
        resolve_ability_chain(state, &sub, events, 0)?;
    }

    // CR 608.2c: if an optional branch suspended for `WaitingFor::OptionalEffectChoice`,
    // the controller has not yet finished following the instructions in order — defer
    // `EffectResolved` until the player has chosen. Mirrors the `prior_waiting_for`
    // guard in `pay.rs::resolve_ability_cost_payment`.
    if state.waiting_for == prior_waiting_for {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::FlipCoin,
            source_id: ability.source_id,
        });
    }

    Ok(())
}

/// CR 705: Flip N coins. For each flip that comes up heads (won), execute
/// `win_effect`; for each that comes up tails (lost), execute `lose_effect`.
/// Generalization of `resolve` for "flip N coins" patterns where the Oracle
/// text binds the heads count to a downstream effect (e.g., Ral Zarek's -7:
/// target opponent skips one turn per heads).
pub fn resolve_flip_coins(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (count_expr, win_effect, lose_effect) = match &ability.effect {
        Effect::FlipCoins {
            count,
            win_effect,
            lose_effect,
        } => (count, win_effect.as_deref(), lose_effect.as_deref()),
        _ => return Err(EffectError::MissingParam("FlipCoins".to_string())),
    };

    // CR 107.1: resolve `count` in the ability's context; clamp at zero.
    let n =
        resolve_quantity(state, count_expr, ability.controller, ability.source_id).max(0) as u32;

    // CR 705.1: Flip each coin with the game's seeded RNG, routing each
    // outcome through the appropriate branch exactly as the single-flip
    // resolver does — so downstream `win_effect`/`lose_effect` see the same
    // stacking/target semantics whether they ran once or N times.
    let prior_waiting_for = state.waiting_for.clone();
    for _ in 0..n {
        let won = state.rng.random_bool(0.5);
        events.push(GameEvent::CoinFlipped {
            player_id: ability.controller,
            won,
        });
        let branch = if won { win_effect } else { lose_effect };
        if let Some(def) = branch {
            // CR 705.2: preserve the branch's `optional`/`sub_ability`/`condition`
            // via the canonical converter rather than the lossy `ResolvedAbility::new`.
            let sub = crate::game::ability_utils::build_resolved_from_def_with_targets(
                def,
                ability.source_id,
                ability.controller,
                ability.targets.clone(),
            );
            resolve_ability_chain(state, &sub, events, 0)?;
        }
        // CR 608.2c: a branch may suspend for an optional choice; stop flipping
        // until the player resolves it.
        if state.waiting_for != prior_waiting_for {
            break;
        }
    }

    // CR 608.2c: defer `EffectResolved` if a branch suspended for a player choice.
    if state.waiting_for == prior_waiting_for {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::FlipCoins,
            source_id: ability.source_id,
        });
    }

    Ok(())
}

/// CR 705: Flip coins until you lose a flip, then execute effect.
pub fn resolve_until_lose(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let win_effect = match &ability.effect {
        Effect::FlipCoinUntilLose { win_effect } => win_effect.as_ref(),
        _ => return Err(EffectError::MissingParam("FlipCoinUntilLose".to_string())),
    };

    // CR 705: Flip coins until a flip is lost. Count the wins.
    // Safety cap prevents infinite loops with pathological RNG seeds.
    const MAX_FLIPS: u32 = 1000;
    let mut win_count = 0u32;
    for _ in 0..MAX_FLIPS {
        let won = state.rng.random_bool(0.5);
        events.push(GameEvent::CoinFlipped {
            player_id: ability.controller,
            won,
        });
        if !won {
            break;
        }
        win_count += 1;
    }

    // Execute the win effect once for each win (via repeat_for-like iteration).
    let prior_waiting_for = state.waiting_for.clone();
    if win_count > 0 {
        for _ in 0..win_count {
            // CR 705.2: preserve the win effect's `optional`/`sub_ability`/`condition`
            // via the canonical converter rather than the lossy `ResolvedAbility::new`.
            let sub = crate::game::ability_utils::build_resolved_from_def_with_targets(
                win_effect,
                ability.source_id,
                ability.controller,
                ability.targets.clone(),
            );
            resolve_ability_chain(state, &sub, events, 0)?;
            // CR 608.2c: a win effect may suspend for an optional choice.
            if state.waiting_for != prior_waiting_for {
                break;
            }
        }
    }

    // CR 608.2c: defer `EffectResolved` if the win effect suspended for a player choice.
    if state.waiting_for == prior_waiting_for {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::FlipCoinUntilLose,
            source_id: ability.source_id,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{AbilityDefinition, AbilityKind, QuantityExpr};
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    #[test]
    fn flip_coin_emits_event() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::FlipCoin {
                win_effect: None,
                lose_effect: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::CoinFlipped { .. })));
    }

    #[test]
    fn flip_coin_with_branches_resolves_one() {
        let mut state = GameState::new_two_player(42);

        let win_effect = Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 5 },
                player: crate::types::ability::TargetFilter::Controller,
            },
        ));
        let lose_effect = Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 3 },
                target: None,
            },
        ));

        let ability = ResolvedAbility::new(
            Effect::FlipCoin {
                win_effect: Some(win_effect),
                lose_effect: Some(lose_effect),
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let initial_life = state.players[0].life;
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Exactly one branch should have fired — life changed
        let new_life = state.players[0].life;
        assert_ne!(new_life, initial_life, "One branch should have fired");
        // Either gained 5 (won) or lost 3 (lost)
        assert!(
            new_life == initial_life + 5 || new_life == initial_life - 3,
            "Expected +5 or -3, got {}",
            new_life - initial_life
        );
    }

    #[test]
    fn flip_coin_until_lose_emits_multiple_events() {
        let mut state = GameState::new_two_player(42);
        // Add cards to library to draw from
        for i in 0..10 {
            crate::game::zones::create_object(
                &mut state,
                crate::types::identifiers::CardId(i + 1),
                PlayerId(0),
                format!("Card {}", i),
                crate::types::zones::Zone::Library,
            );
        }

        let ability = ResolvedAbility::new(
            Effect::FlipCoinUntilLose {
                win_effect: Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: crate::types::ability::TargetFilter::Controller,
                    },
                )),
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        let result = resolve_until_lose(&mut state, &ability, &mut events);
        assert!(result.is_ok());

        // Must have at least one CoinFlipped event (the losing flip)
        let flip_count = events
            .iter()
            .filter(|e| matches!(e, GameEvent::CoinFlipped { .. }))
            .count();
        assert!(flip_count >= 1);

        // The last CoinFlipped should be a loss
        let last_flip = events
            .iter()
            .rev()
            .find(|e| matches!(e, GameEvent::CoinFlipped { .. }));
        assert!(matches!(
            last_flip,
            Some(GameEvent::CoinFlipped { won: false, .. })
        ));
    }

    #[test]
    fn flip_coins_emits_n_coin_flip_events() {
        // CR 705.1: FlipCoins with count=5 emits exactly 5 CoinFlipped events.
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::FlipCoins {
                count: QuantityExpr::Fixed { value: 5 },
                win_effect: None,
                lose_effect: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_flip_coins(&mut state, &ability, &mut events).unwrap();

        let flip_count = events
            .iter()
            .filter(|e| matches!(e, GameEvent::CoinFlipped { .. }))
            .count();
        assert_eq!(flip_count, 5);
    }

    #[test]
    fn flip_coins_zero_count_is_noop() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::FlipCoins {
                count: QuantityExpr::Fixed { value: 0 },
                win_effect: None,
                lose_effect: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_flip_coins(&mut state, &ability, &mut events).unwrap();
        let flip_count = events
            .iter()
            .filter(|e| matches!(e, GameEvent::CoinFlipped { .. }))
            .count();
        assert_eq!(flip_count, 0);
    }

    #[test]
    fn flip_coins_runs_win_effect_per_heads() {
        // CR 705.2: `win_effect` fires once per heads. With a deterministic
        // seed and 4 coins, the exact heads count is stable; assert that the
        // win_effect ran exactly that many times.
        let mut state = GameState::new_two_player(42);
        let initial_life = state.players[0].life;

        let win_effect = Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: crate::types::ability::TargetFilter::Controller,
            },
        ));

        let ability = ResolvedAbility::new(
            Effect::FlipCoins {
                count: QuantityExpr::Fixed { value: 4 },
                win_effect: Some(win_effect),
                lose_effect: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_flip_coins(&mut state, &ability, &mut events).unwrap();

        let heads = events
            .iter()
            .filter(|e| matches!(e, GameEvent::CoinFlipped { won: true, .. }))
            .count() as i32;
        assert_eq!(state.players[0].life - initial_life, heads);
    }

    // --- Issue #432: Ral, Monsoon Mage coin-flip transform ---------------------
    //
    // Ral's trigger is `FlipCoin { win_effect, lose_effect }` carried on an
    // `AbilityDefinition` whose own `sub_ability` is the return-transformed
    // `ChangeZone` gated by `IfYouDo`. `win_effect` is an OPTIONAL
    // `ChangeZone(Exile, SelfRef)` ("you may exile Ral"). The handler used to
    // rebuild the branch with the lossy `ResolvedAbility::new`, dropping
    // `win_effect.optional` so the player was never prompted and the
    // return-transformed chain never keyed off the exile. These tests drive the
    // genuine resolution pipeline (`build_resolved_from_def` → `resolve_ability_chain`,
    // exactly as `game/triggers.rs` + `game/stack.rs` do) and the genuine
    // `apply(DecideOptionalEffect)` pipeline, with the RNG deterministically
    // seeded for a win or a loss.

    use crate::game::ability_utils::build_resolved_from_def;
    use crate::game::effects::resolve_ability_chain;
    use crate::game::engine::apply;
    use crate::game::game_object::BackFaceData;
    use crate::game::zones::create_object;
    use crate::types::ability::{AbilityCondition, TargetFilter};
    use crate::types::actions::GameAction;
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::game_state::WaitingFor;
    use crate::types::identifiers::CardId;
    use crate::types::mana::ManaCost;
    use crate::types::zones::Zone;
    use rand::SeedableRng;
    use rand_chacha::ChaCha20Rng;

    /// Build Ral, Monsoon Mage as a battlefield permanent with a back face so
    /// `enter_transformed` has a face to flip to (CR 712.8e).
    fn setup_ral(state: &mut GameState) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            PlayerId(0),
            "Ral, Monsoon Mage".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.power = Some(1);
        obj.toughness = Some(3);
        obj.base_power = Some(1);
        obj.base_toughness = Some(3);
        obj.back_face = Some(BackFaceData {
            name: "Ral, Leyline Prodigy".to_string(),
            power: None,
            toughness: None,
            loyalty: Some(3),
            defense: None,
            card_types: CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Planeswalker],
                subtypes: vec!["Ral".to_string()],
            },
            mana_cost: ManaCost::default(),
            keywords: vec![],
            abilities: vec![],
            trigger_definitions: Default::default(),
            replacement_definitions: Default::default(),
            static_definitions: Default::default(),
            color: vec![],
            printed_ref: None,
            modal: None,
            additional_cost: None,
            strive_cost: None,
            casting_restrictions: vec![],
            casting_options: vec![],
            layout_kind: None,
        });
        id
    }

    /// Reproduce Ral's parsed trigger `execute` `AbilityDefinition`:
    /// `FlipCoin` whose `win_effect` is an optional self-exile, with the
    /// return-transformed `ChangeZone` as the definition's `sub_ability`,
    /// gated `IfYouDo`.
    fn ral_trigger_definition() -> AbilityDefinition {
        let win_effect = Box::new({
            let mut def = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    origin: None,
                    destination: Zone::Exile,
                    target: TargetFilter::SelfRef,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: false,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                },
            );
            def.optional = true;
            def
        });
        let lose_effect = Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
                damage_source: None,
            },
        ));
        let return_transformed = {
            let mut def = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    origin: None,
                    destination: Zone::Battlefield,
                    target: TargetFilter::ParentTarget,
                    owner_library: false,
                    enter_transformed: true,
                    enters_under: None,
                    enter_tapped: false,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                },
            );
            def.condition = Some(AbilityCondition::effect_performed());
            def
        };
        let mut def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::FlipCoin {
                win_effect: Some(win_effect),
                lose_effect: Some(lose_effect),
            },
        );
        def.sub_ability = Some(Box::new(return_transformed));
        def
    }

    #[test]
    fn ral_wins_flip_and_accepts_exile_returns_transformed() {
        let mut state = GameState::new_two_player(0);
        // Seed 0 → first `random_bool(0.5)` is a WIN.
        state.rng = ChaCha20Rng::seed_from_u64(0);
        let ral = setup_ral(&mut state);

        let ability = build_resolved_from_def(&ral_trigger_definition(), ral, PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Win branch is `optional` → the chain must SUSPEND for the player's
        // "you may exile Ral" choice. Pre-fix, `optional` was dropped and the
        // chain ran straight through with no prompt.
        assert!(
            matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
            "expected OptionalEffectChoice, got {:?}",
            state.waiting_for
        );
        // The premature `EffectResolved` guard: while suspended, FlipCoin must
        // NOT have reported itself resolved.
        assert!(
            !events.iter().any(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::FlipCoin,
                    ..
                }
            )),
            "FlipCoin EffectResolved fired before the optional choice was made"
        );

        // Accept the optional exile through the real `apply` pipeline.
        let result = apply(
            &mut state,
            PlayerId(0),
            GameAction::DecideOptionalEffect { accept: true },
        )
        .expect("DecideOptionalEffect should succeed");

        // Ral was exiled, then the `IfYouDo` sub-ability returned him to the
        // battlefield transformed (CR 712.8e — back face up).
        let obj = state
            .objects
            .get(&ral)
            .expect("Ral object should still exist");
        assert_eq!(
            obj.zone,
            Zone::Battlefield,
            "Ral should have returned to the battlefield"
        );
        assert!(
            obj.transformed,
            "Ral should be on his back face after returning transformed; events: {:?}",
            result.events
        );
    }

    #[test]
    fn ral_wins_flip_and_declines_exile_stays_front_face() {
        let mut state = GameState::new_two_player(0);
        state.rng = ChaCha20Rng::seed_from_u64(0);
        let ral = setup_ral(&mut state);

        let ability = build_resolved_from_def(&ral_trigger_definition(), ral, PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert!(
            matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
            "expected OptionalEffectChoice, got {:?}",
            state.waiting_for
        );

        // Decline the optional exile.
        apply(
            &mut state,
            PlayerId(0),
            GameAction::DecideOptionalEffect { accept: false },
        )
        .expect("DecideOptionalEffect should succeed");

        let obj = state.objects.get(&ral).expect("Ral object should exist");
        assert_eq!(
            obj.zone,
            Zone::Battlefield,
            "Ral should remain on the battlefield when the exile is declined"
        );
        assert!(
            !obj.transformed,
            "Ral should stay on his front face when the exile is declined"
        );
    }

    #[test]
    fn ral_loses_flip_takes_one_damage() {
        let mut state = GameState::new_two_player(1);
        // Seed 1 → first `random_bool(0.5)` is a LOSS.
        state.rng = ChaCha20Rng::seed_from_u64(1);
        let ral = setup_ral(&mut state);
        let initial_life = state.players[0].life;

        let ability = build_resolved_from_def(&ral_trigger_definition(), ral, PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Lose branch is non-optional → resolves inline, no suspension.
        assert!(
            !matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
            "lose branch should not suspend for an optional choice, got {:?}",
            state.waiting_for
        );
        assert_eq!(
            state.players[0].life,
            initial_life - 1,
            "controller should take 1 damage on a lost flip"
        );
        let obj = state.objects.get(&ral).expect("Ral object should exist");
        assert_eq!(obj.zone, Zone::Battlefield, "Ral should not be exiled");
        assert!(!obj.transformed, "Ral should not transform on a loss");
    }
}
