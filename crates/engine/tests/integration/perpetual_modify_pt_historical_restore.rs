//! Restoring a game saved BEFORE `PerpetualModification::ModifyPowerToughness`
//! carried `power`/`toughness: QuantityExpr` (#7495).
//!
//! Through v108 the edit stored bare-integer `power_delta`/`toughness_delta`;
//! v109 retyped the fields to required tagged exprs (plus a defaulted
//! `keywords` rider). The historical keys survive as serde aliases on the new
//! fields, with the bare ints riding the existing `QuantityExpr`
//! legacy-integer decoder into `Fixed` — no JSON migration layer.
//!
//! Both tests below drive a hand-authored historical payload through the
//! production `PersistedGameState` boundary (`from_value` +
//! `into_game_state`, the same choke point the WASM restore calls). A
//! current-schema round trip cannot discriminate the alias path, because it
//! would serialize canonical keys on both sides of the save.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{PerpetualModification, QuantityExpr};
use engine::types::game_state::PersistedGameState;

/// Rewrite every persisted perpetual P/T edit into the PRE-RETYPE wire shape:
/// the historical `power_delta`/`toughness_delta` keys holding bare integers.
///
/// Keyed on BOTH field names so the payload stays historical no matter which
/// key the current schema serializes under — that keeps a revert experiment
/// on the aliases honest. Only `Fixed` deltas are rewritten: v108 could only
/// ever emit bare integers, and a live expr has no historical spelling.
fn rewrite_edits_to_legacy_deltas(value: &mut serde_json::Value) -> usize {
    match value {
        serde_json::Value::Array(values) => {
            values.iter_mut().map(rewrite_edits_to_legacy_deltas).sum()
        }
        serde_json::Value::Object(object) => {
            let mut rewritten = 0;
            let is_modify_pt = object.get("kind").and_then(serde_json::Value::as_str)
                == Some("ModifyPowerToughness");
            if is_modify_pt {
                for (legacy, canonical) in
                    [("power_delta", "power"), ("toughness_delta", "toughness")]
                {
                    if let Some(expr) = object.remove(canonical) {
                        let fixed = expr
                            .get("value")
                            .and_then(serde_json::Value::as_i64)
                            .expect("rewritten fixture deltas are Fixed");
                        object.insert(legacy.to_string(), serde_json::Value::Number(fixed.into()));
                        rewritten += 1;
                    }
                }
            }
            rewritten
                + object
                    .values_mut()
                    .map(rewrite_edits_to_legacy_deltas)
                    .sum::<usize>()
        }
        _ => 0,
    }
}

fn collect_modify_pt_payloads<'a>(
    value: &'a serde_json::Value,
    out: &mut Vec<&'a serde_json::Map<String, serde_json::Value>>,
) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                collect_modify_pt_payloads(value, out);
            }
        }
        serde_json::Value::Object(object) => {
            if object.get("kind").and_then(serde_json::Value::as_str)
                == Some("ModifyPowerToughness")
            {
                out.push(object);
            }
            for value in object.values() {
                collect_modify_pt_payloads(value, out);
            }
        }
        _ => {}
    }
}

fn fixed_deltas(modification: &PerpetualModification) -> Option<(i32, i32)> {
    match modification {
        PerpetualModification::ModifyPowerToughness {
            power: QuantityExpr::Fixed { value: p },
            toughness: QuantityExpr::Fixed { value: t },
            ..
        } => Some((*p, *t)),
        _ => None,
    }
}

#[test]
fn legacy_delta_keys_restore_through_both_wire_shapes() {
    let mut scenario = GameScenario::new();
    let bear = scenario.add_vanilla(P0, 2, 2);
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&bear)
        .expect("vanilla is on the battlefield")
        .perpetual_mods
        .push(PerpetualModification::ModifyPowerToughness {
            power: QuantityExpr::Fixed { value: 1 },
            toughness: QuantityExpr::Fixed { value: -2 },
            keywords: Vec::new(),
        });

    for mut wire in [
        serde_json::to_value(PersistedGameState::Raw(Box::new(runner.state().clone())))
            .expect("raw snapshot serializes"),
        serde_json::to_value(PersistedGameState::capture(runner.state().clone()))
            .expect("trusted snapshot serializes"),
    ] {
        let rewritten = rewrite_edits_to_legacy_deltas(&mut wire);
        assert_eq!(
            rewritten, 2,
            "the historical payload must carry both legacy delta keys"
        );
        let mut legacy_edits = Vec::new();
        collect_modify_pt_payloads(&wire, &mut legacy_edits);
        assert_eq!(
            legacy_edits.len(),
            1,
            "exactly the planted edit must be present"
        );
        assert!(
            legacy_edits[0].contains_key("power_delta")
                && legacy_edits[0].contains_key("toughness_delta")
                && !legacy_edits[0].contains_key("power")
                && !legacy_edits[0].contains_key("toughness"),
            "no canonical key may survive into the historical payload, got {:?}",
            legacy_edits[0]
        );

        let restored = serde_json::from_value::<PersistedGameState>(wire)
            .expect("legacy delta keys decode")
            .into_game_state()
            .expect("legacy perpetual edit restores");
        let mods = &restored.objects[&bear].perpetual_mods;
        assert_eq!(mods.len(), 1, "the edit must survive restore, got {mods:?}");
        assert_eq!(
            fixed_deltas(&mods[0]),
            Some((1, -2)),
            "bare-int deltas must decode to Fixed, got {:?}",
            mods[0]
        );

        // Canonical re-emission: the restored state serializes under the new
        // keys only — aliases are a decode-side compat, never emitted.
        let reemitted = serde_json::to_value(PersistedGameState::Raw(Box::new(restored)))
            .expect("restored state serializes");
        let reemitted_text = serde_json::to_string(&reemitted).expect("re-emitted wire serializes");
        assert!(
            !reemitted_text.contains("power_delta") && !reemitted_text.contains("toughness_delta"),
            "re-emission must not contain legacy keys"
        );
    }
}
