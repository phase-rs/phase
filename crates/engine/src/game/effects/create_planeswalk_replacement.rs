use crate::types::ability::{
    Effect, EffectError, EffectKind, ReplacementDefinition, ReplacementPlayerScope, ResolvedAbility,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::replacements::ReplacementEvent;

/// CR 614.1a + CR 611.2 + CR 901.9c + CR 614.5/614.6: Resolve
/// `Effect::CreatePlaneswalkReplacement` — install the floating, duration-bound
/// "if a player would planeswalk as a result of rolling the planar die,
/// [replacement_effect] instead" replacement (Fixed Point in Time).
///
/// Mirrors `create_draw_replacement::resolve` for the planar-die planeswalk
/// event class: it builds a `ReplacementDefinition` for
/// `ReplacementEvent::Planeswalk` whose substitute is carried in
/// `runtime_execute` (a `ResolvedAbility`, so the heterogeneous payload Effect
/// resolves through the post-replacement continuation drain in
/// `effects::planeswalk::resolve`'s `Prevented` arm).
///
/// CR 614.5: the shield is continuous, not one-shot (`consume_on_apply: false`)
/// — every planar-die planeswalk within the "until your next turn" window is
/// replaced. CR 614.5 only blocks a replacement from re-applying to a single
/// event's own modified forms; it does not consume this shield after one fire.
///
/// Player scope: `AnyPlayer` ("a player would planeswalk"). Expiry comes from
/// the installing ability's `Duration` (`UntilNextTurnOf { Controller }` →
/// `RestrictionExpiry::UntilPlayerNextTurn`). `source_controller` is anchored at
/// resolution time (CR 611.2a) so the shield outlives the phenomenon.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::CreatePlaneswalkReplacement { replacement_effect } = &ability.effect else {
        return Err(EffectError::InvalidParam(
            "expected CreatePlaneswalkReplacement effect".to_string(),
        ));
    };

    // CR 614.6: the substitute (chaos ensues) that runs in place of the replaced
    // planeswalk. Captured as a `ResolvedAbility` so the post-replacement
    // continuation drain dispatches it with source/controller bound at install
    // time (CR 611.2a).
    let substitute = ResolvedAbility::new(
        (**replacement_effect).clone(),
        vec![],
        ability.source_id,
        ability.controller,
    );

    let mut shield = ReplacementDefinition::new(ReplacementEvent::Planeswalk);
    shield.runtime_execute = Some(Box::new(substitute)); // CR 614.6: substitute
    shield.consume_on_apply = false; // CR 614.5: continuous within the window
    shield.valid_player = Some(ReplacementPlayerScope::AnyPlayer); // "a player"
    shield.planeswalk_scope =
        Some(crate::types::ability::PlaneswalkReplacementScope::PlanarDieOnly);
    // Duration::UntilNextTurnOf { Controller } → RestrictionExpiry::UntilPlayerNextTurn.
    shield.expiry = crate::game::effects::add_target_replacement::expiry_from_duration(
        ability.duration.as_ref(),
        ability.controller,
    );
    shield.source_controller = Some(ability.controller);

    state.pending_damage_replacements.push(shield);
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::CreatePlaneswalkReplacement,
        source_id: ability.source_id,
        subject: None,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{Duration, PlayerScope, RestrictionExpiry};
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    /// The install effect for Fixed Point in Time: "chaos ensues instead".
    fn install(source: ObjectId, controller: PlayerId) -> ResolvedAbility {
        let mut ability = ResolvedAbility::new(
            Effect::CreatePlaneswalkReplacement {
                replacement_effect: Box::new(Effect::ChaosEnsues),
            },
            vec![],
            source,
            controller,
        );
        // "until your next turn," → the shield expires at the controller's next turn.
        ability.duration = Some(Duration::UntilNextTurnOf {
            player: PlayerScope::Controller,
        });
        ability
    }

    /// The install builds a continuous `Planeswalk` shield in pending state,
    /// scoped to any player, carrying the chaos-ensues substitute, expiring at
    /// the controller's next turn. DISCRIMINATING: a one-shot design would set
    /// `consume_on_apply`, and a wrong scope would set `You`.
    #[test]
    fn installs_continuous_anyplayer_planeswalk_shield() {
        let mut state = GameState::default();
        let source = ObjectId(42);
        let controller = PlayerId(0);

        let mut events = Vec::new();
        resolve(&mut state, &install(source, controller), &mut events).unwrap();

        assert_eq!(
            state.pending_damage_replacements.len(),
            1,
            "the planeswalk-replacement shield must be installed in pending state"
        );
        let shield = &state.pending_damage_replacements[0];
        assert_eq!(shield.event, ReplacementEvent::Planeswalk);
        assert!(
            !shield.consume_on_apply,
            "CR 614.5: the shield is continuous, not one-shot"
        );
        assert_eq!(
            shield.valid_player,
            Some(ReplacementPlayerScope::AnyPlayer),
            "\"a player would planeswalk\" is any-player scoped"
        );
        assert_eq!(
            shield.planeswalk_scope,
            Some(crate::types::ability::PlaneswalkReplacementScope::PlanarDieOnly),
            "CR 901.9c: Fixed Point matches only planar-die planeswalks"
        );
        // CR 514.2: expiry is armed from the ability's UntilNextTurnOf
        // { Controller } duration so the shared untap-step prune drops it at the
        // controller's next turn (Test D in planechase_tests.rs proves the sweep).
        assert_eq!(
            shield.expiry,
            Some(RestrictionExpiry::UntilPlayerNextTurn { player: controller }),
            "expiry comes from the ability's until-next-turn-of-controller duration"
        );
        assert_eq!(shield.source_controller, Some(controller));
        // CR 614.6: the substitute (chaos ensues) rides in runtime_execute.
        let runtime = shield
            .runtime_execute
            .as_ref()
            .expect("substitute must ride in runtime_execute");
        assert!(matches!(runtime.effect, Effect::ChaosEnsues));
    }
}
