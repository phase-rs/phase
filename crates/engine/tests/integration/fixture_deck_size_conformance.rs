//! CR 100.5 / CR 903.5a: a persisted `format_config.deck_size` must be the rule its own
//! format defines — CR 100.5 for the minimum-deck-size axis, CR 903.5a for Commander's
//! "the minimum deck size and the maximum deck size are both 100".
//!
//! `scripts/migrate-dump-fixture.sh` takes that variant as an operator argument
//! (`--deck-size <Minimum|Exactly>:<count>`) and validates its SHAPE only, so a typo
//! produces a well-formed artifact the bash gate happily emits. This row is what holds
//! the operator to the argument, the way the row beside `load_dellian_dump` holds one to
//! `--effect-kind`.
//!
//! Two independent legs, because neither subsumes the other. The decode leg is the
//! authority for a tag naming no live variant and for a `Custom` payload, whose runtime
//! fields `FormatConfig`'s `Deserialize` re-derives from `custom_rules` and demands
//! equality of. The comparison leg is the authority for a well-formed variant the
//! declared format does not define, which decodes cleanly. Deliberately not written
//! against `DeckSizeRule::min_cards`: that accessor returns the payload for both
//! variants, so it cannot tell `Minimum(100)` from `Exactly(100)` — the exact
//! discrimination this row exists to make.

use std::path::{Path, PathBuf};

use engine::analysis::decision_template::{ChoicePoint, DecisionSlot};
use engine::types::format::{FormatConfig, GameFormat};
use engine::types::game_state::YieldTarget as DecisionSource;
use engine::types::identifiers::ObjectId;

fn gunzip(path: &Path) -> String {
    use std::io::Read;

    let bytes =
        std::fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let mut json = String::new();
    flate2::read::GzDecoder::new(bytes.as_slice())
        .read_to_string(&mut json)
        .unwrap_or_else(|error| panic!("{} must inflate to UTF-8 JSON: {error}", path.display()));
    json
}

fn collect_gz(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|error| panic!("read {}: {error}", dir.display()));
    for entry in entries {
        let path = entry.expect("read dir entry").path();
        if path.is_dir() {
            collect_gz(&path, out);
        } else if path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with(".json.gz"))
        {
            out.push(path);
        }
    }
}

/// Every object reached at a key named `format_config`, paired with its JSON path so a
/// failure names the offending site rather than only the file.
fn collect_format_configs(
    value: &serde_json::Value,
    path: &mut Vec<String>,
    out: &mut Vec<(String, serde_json::Value)>,
) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                path.push(key.clone());
                if key == "format_config" {
                    out.push((path.join("."), child.clone()));
                }
                collect_format_configs(child, path, out);
                path.pop();
            }
        }
        serde_json::Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                path.push(index.to_string());
                collect_format_configs(child, path, out);
                path.pop();
            }
        }
        _ => {}
    }
}

#[test]
fn every_persisted_deck_size_is_the_rule_its_format_defines() {
    // `crates/` rather than the engine's own fixture directory: `regenerate` in
    // `migrate-dump-fixture.sh` does `mkdir -p "$(dirname "$dest")"`, so `--out` reaches
    // every gzipped fixture directory in the workspace, and `git ls-files '*.json.gz'`
    // shows all of them live under this root.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut fixtures = Vec::new();
    collect_gz(&root, &mut fixtures);
    fixtures.sort();

    let mut compared = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for fixture in &fixtures {
        let value: serde_json::Value = serde_json::from_str(&gunzip(fixture))
            .unwrap_or_else(|error| panic!("{} parses as JSON: {error}", fixture.display()));
        let mut configs = Vec::new();
        collect_format_configs(&value, &mut Vec::new(), &mut configs);

        let name = fixture.strip_prefix(&root).unwrap_or(fixture).display();
        for (at, object) in configs {
            let config = match serde_json::from_value::<FormatConfig>(object) {
                Ok(config) => config,
                Err(error) => {
                    failures.push(format!(
                        "{name} at {at}: does not decode through FormatConfig: {error}"
                    ));
                    continue;
                }
            };
            // A `Custom` config's authority is `for_custom_rules`, which the decode above
            // already applied; `for_format` cannot answer for it at all.
            if matches!(config.format, GameFormat::Custom(_)) {
                continue;
            }
            let expected = match FormatConfig::for_format(config.format) {
                Ok(expected) => expected,
                Err(error) => {
                    failures.push(format!(
                        "{name} at {at}: {} has no config: {error}",
                        config.format
                    ));
                    continue;
                }
            };
            compared += 1;
            if config.deck_size != expected.deck_size {
                failures.push(format!(
                    "{name} at {at}: persisted {:?}, but {} defines {:?}",
                    config.deck_size, config.format, expected.deck_size
                ));
            }
        }
    }

    assert!(
        compared > 0,
        "reach-guard: the walk of {} compared no deck_size at all, so an empty verdict \
         below would be a walk that visited nothing rather than a corpus that conforms",
        root.display()
    );
    assert!(
        failures.is_empty(),
        "persisted deck_size disagrees with the format's own rule ({compared} compared):\n{}",
        failures.join("\n")
    );
}

/// Every object whose key set is a migrated `DecisionSlot`'s (`{source, point, index}`) or a
/// migrated `PinnedDecision::Order`'s (`{slot, pos}` under an `Order` key), each paired with
/// its JSON path so a failure names the offending site and so the GOVERNING NEIGHBOUR is
/// reachable from the hit itself.
fn collect_migrated_slots(
    value: &serde_json::Value,
    path: &mut Vec<String>,
    slots: &mut Vec<(String, serde_json::Value)>,
    orders: &mut Vec<(String, serde_json::Value)>,
) {
    fn keys_are(map: &serde_json::Map<String, serde_json::Value>, expected: &[&str]) -> bool {
        map.len() == expected.len() && expected.iter().all(|key| map.contains_key(*key))
    }

    // Tested at the NODE, not at the key: a `victim_slot` entry is an ARRAY ELEMENT, so a
    // walk that only inspected an object's named children would miss the very carrier whose
    // point comes from a documented contract rather than from a sibling.
    if let serde_json::Value::Object(map) = value {
        if keys_are(map, &["source", "point", "index"]) {
            slots.push((path.join("."), value.clone()));
        } else if path.last().map(String::as_str) == Some("Order")
            && keys_are(map, &["slot", "pos"])
        {
            orders.push((path.join("."), value.clone()));
        }
    }

    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                path.push(key.clone());
                collect_migrated_slots(child, path, slots, orders);
                path.pop();
            }
        }
        serde_json::Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                path.push(index.to_string());
                collect_migrated_slots(child, path, slots, orders);
                path.pop();
            }
        }
        _ => {}
    }
}

/// The `ChoicePoint` a published `DecisionPointKind` names, written independently of
/// `scripts/lib/decision-slot-point.jq` — a readback that re-used the migration's own
/// transform would certify the script's copy of the recipe rather than the value.
///
/// `None` where the kind does not name one: `pinned_decisions_to_points` sets
/// `ordered: true` unconditionally, so which choice such a point names is a property of the
/// PIN that published it and is not readable from the schema.
fn point_named_by(
    kind: &engine::analysis::decision_template::DecisionPointKind,
) -> Option<ChoicePoint> {
    use engine::analysis::decision_template::DecisionPointKind as K;

    match kind {
        // CR 601.2c via CR 603.3d: `bounded_cycle_pin_slots_for_window` publishes an
        // announcement at `ordered: false`.
        K::Targets { ordered: false, .. } => Some(ChoicePoint::AnnouncedTarget),
        K::Targets { ordered: true, .. } => None,
        K::ConvokeTaps { .. } => Some(ChoicePoint::ConvokeTaps),
        K::Mode { .. } => Some(ChoicePoint::Mode),
        K::MayChoice => Some(ChoicePoint::MayGate),
        K::UnlessBreak => Some(ChoicePoint::UnlessBreak),
        K::ManaColor { .. } => Some(ChoicePoint::ManaColor),
    }
}

/// The row's comparison for a kind-governed slot, in ONE place so the hostile legs run
/// through it rather than around it.
fn kind_governed_disagreement(
    at: &str,
    slot: &DecisionSlot,
    kind: &engine::analysis::decision_template::DecisionPointKind,
) -> Option<String> {
    match point_named_by(kind) {
        Some(expected) if expected == slot.point => None,
        Some(expected) => Some(format!(
            "{at}: slot carries {:?}, but its governing {kind:?} names {expected:?}",
            slot.point
        )),
        None => Some(format!("{at}: governing {kind:?} names no choice point")),
    }
}

/// CR 603.3b: a `PinnedDecision::Order`'s slot is minted at `TriggerOrder`, instance `0`,
/// and nowhere else — no published point carries `TriggerOrder`, and the drive's cursor,
/// not the slot, is what separates two triggers of one object.
fn order_slot_disagreement(at: &str, slot: &DecisionSlot) -> Option<String> {
    if slot.point == ChoicePoint::TriggerOrder && slot.index == 0 {
        None
    } else {
        Some(format!(
            "{at}: an Order decision's slot is {:?} at instance {}, not TriggerOrder at 0",
            slot.point, slot.index
        ))
    }
}

/// R12 — every migrated slot in the committed corpus carries the point its own GOVERNING
/// NEIGHBOUR names, read back after the shape has been accepted by the TYPE rather than by
/// the script that wrote it.
///
/// This row is the migration's only tracked end, and it is not optional: measured, the
/// dumps' own loading rows cannot be the backstop.
/// `crates/engine/tests/integration/combo_infinite_pile.rs` contains the substring `slot`
/// zero times, and the other loaders assert board properties — bounds, capacities,
/// thresholds, controllers — never a point label. Without this row a wrong stamp ships
/// green.
///
/// The population is every committed `*.json.gz` under `crates/`, with no classifier,
/// because nothing here decodes a whole document: that root also holds a card map, a
/// decklist bundle and census artifacts that are not persisted game states at all.
#[test]
fn every_migrated_slot_in_a_committed_gz_carries_the_point_its_neighbour_names() {
    use engine::analysis::decision_template::{DecisionPoint, DecisionPointKind, PinnedDecision};

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut fixtures = Vec::new();
    collect_gz(&root, &mut fixtures);
    fixtures.sort();

    let mut parsed = 0usize;
    let mut slots_seen = 0usize;
    let mut orders_seen = 0usize;
    let mut governing_kinds: Vec<String> = Vec::new();
    let mut failures: Vec<String> = Vec::new();

    for fixture in &fixtures {
        let value: serde_json::Value = serde_json::from_str(&gunzip(fixture))
            .unwrap_or_else(|error| panic!("{} parses as JSON: {error}", fixture.display()));
        parsed += 1;
        let name = fixture.strip_prefix(&root).unwrap_or(fixture).display();

        let mut slots = Vec::new();
        let mut orders = Vec::new();
        collect_migrated_slots(&value, &mut Vec::new(), &mut slots, &mut orders);

        for (at, object) in &orders {
            orders_seen += 1;
            let at = format!("{name} at {at}");
            // Decoded through the real variant, so the row asserts what the engine accepts.
            let pin: PinnedDecision =
                match serde_json::from_value(serde_json::json!({ "Order": object })) {
                    Ok(pin) => pin,
                    Err(error) => {
                        failures.push(format!(
                            "{at}: does not decode as PinnedDecision::Order: {error}"
                        ));
                        continue;
                    }
                };
            let PinnedDecision::Order { slot, .. } = &pin else {
                failures.push(format!("{at}: decoded as {pin:?}, not an Order"));
                continue;
            };
            failures.extend(order_slot_disagreement(&at, slot));
        }

        for (at, object) in &slots {
            slots_seen += 1;
            let path: Vec<&str> = at.split('.').collect();
            let at_named = format!("{name} at {at}");
            let slot: DecisionSlot = match serde_json::from_value(object.clone()) {
                Ok(slot) => slot,
                Err(error) => {
                    failures.push(format!(
                        "{at_named}: does not decode as DecisionSlot: {error}"
                    ));
                    continue;
                }
            };

            // The governing neighbour is reached from the hit's OWN path, and the path set
            // is closed here exactly as it is at the migration: a carrier none of these
            // governs fails the row by name rather than passing unasserted.
            let parent = path[..path.len() - 1].join(".");
            let parent_value = parent
                .split('.')
                .filter(|segment| !segment.is_empty())
                .try_fold(&value, |node, segment| match node {
                    serde_json::Value::Object(map) => map.get(segment),
                    serde_json::Value::Array(items) => items.get(segment.parse::<usize>().ok()?),
                    _ => None,
                });

            let is_published_point = matches!(path.last(), Some(&"slot"))
                && matches!(parent_value, Some(serde_json::Value::Object(map)) if map.contains_key("kind"));
            let pin_tag = (path.last() == Some(&"slot"))
                .then(|| path.get(path.len().wrapping_sub(2)).copied())
                .flatten()
                .filter(|tag| tag.parse::<usize>().is_err());
            let is_victim_slot = path.len() >= 3 && path[path.len() - 3] == "victim_slot";

            if is_published_point {
                // A published point: its sibling `kind` names the choice.
                let point: DecisionPoint =
                    match serde_json::from_value(parent_value.expect("checked above").clone()) {
                        Ok(point) => point,
                        Err(error) => {
                            failures
                                .push(format!("{at_named}: sibling kind does not decode: {error}"));
                            continue;
                        }
                    };
                governing_kinds.push(format!("{:?}", std::mem::discriminant(&point.kind)));
                failures.extend(kind_governed_disagreement(&at_named, &slot, &point.kind));
            } else if pin_tag == Some("Order") {
                // CR 603.3b: the `Order` tag names the point outright — there is no
                // published peer to reach for, which is the whole reason `Order`'s slot had
                // to be minted rather than matched.
                governing_kinds.push("Order".to_owned());
                failures.extend(order_slot_disagreement(&at_named, &slot));
            } else if let Some(tag) = pin_tag {
                // A `PinnedDecision`, externally tagged. Its variant key names the choice
                // through `pin_answers_point`'s own product: the pin must answer a PUBLISHED
                // point of the paired kind, and that point's kind is what names its choice.
                let Some(paired) = kind_paired_with_pin(tag) else {
                    failures.push(format!(
                        "{at_named}: PinnedDecision::{tag} publishes no read-side peer, so no point governs this slot"
                    ));
                    continue;
                };
                let mut answered: Vec<DecisionPointKind> = Vec::new();
                collect_published_points(&value, &slot, &mut answered);
                let matched: Vec<&DecisionPointKind> = answered
                    .iter()
                    .filter(|kind| kind_tag(kind) == paired)
                    .collect();
                match matched.as_slice() {
                    [kind] => {
                        governing_kinds.push(format!("{:?}", std::mem::discriminant(*kind)));
                        failures.extend(kind_governed_disagreement(&at_named, &slot, kind));
                    }
                    other => failures.push(format!(
                        "{at_named}: a {tag} pin answers {} published {paired} points, so its choice is not derivable",
                        other.len()
                    )),
                }
            } else if is_victim_slot {
                // CR 601.2c. `PeriodicDelta.victim_slot`'s own contract says these are
                // ANNOUNCED rather than published, and that an entry here with no matching
                // schema point is CORRECT — so slot equality is the wrong instrument at this
                // carrier and the field's documented contract is the right one.
                if slot.point != ChoicePoint::AnnouncedTarget {
                    failures.push(format!(
                        "{at_named}: a victim_slot entry carries {:?}, not the AnnouncedTarget its field's contract names",
                        slot.point
                    ));
                }
                governing_kinds.push("victim_slot".to_owned());
            } else {
                failures.push(format!(
                    "{at_named}: no governing neighbour — the carrier set is closed at the readback exactly as it is at the migration"
                ));
            }
        }
    }

    // Reach guard, three legs. A row that found no slot — or found slots but no `Order` —
    // would pass every per-hit assertion vacuously.
    assert!(
        parsed > 0,
        "reach-guard: the walk of {} parsed no file",
        root.display()
    );
    assert!(
        slots_seen > 0,
        "reach-guard: {parsed} files parsed, but no migrated DecisionSlot found"
    );
    assert!(
        orders_seen > 0,
        "reach-guard: {slots_seen} slots found, but no migrated Order decision"
    );

    // The ADMITTED end: the corpus carries at least two DIFFERENT governing kinds, so
    // "every hit matched" is never "every hit was the same hit".
    let distinct: std::collections::BTreeSet<&String> = governing_kinds.iter().collect();
    assert!(
        distinct.len() >= 2,
        "hostile/admitted: every governed slot in the corpus shares one neighbour ({distinct:?}), \
         so a blanket stamp would satisfy this row"
    );

    assert!(
        failures.is_empty(),
        "migrated slots disagree with their governing neighbours \
         ({slots_seen} slots, {orders_seen} orders, {parsed} files):\n{}",
        failures.join("\n")
    );

    // The REFUSED end, constructed and run THROUGH the row's own comparison rather than
    // asserted about it: a slot whose point its paired kind does not name, and an `Order`
    // at any point other than `TriggerOrder`, must each be reported.
    let source = DecisionSource::ThisObject {
        source_id: ObjectId(1),
        incarnation: None,
        trigger_description: None,
    };
    let convoke = DecisionPointKind::ConvokeTaps {
        tappable: Vec::new(),
    };
    assert!(
        kind_governed_disagreement(
            "hostile",
            &DecisionSlot::first(source.clone(), ChoicePoint::AnnouncedTarget),
            &convoke,
        )
        .is_some(),
        "hostile/refused: an AnnouncedTarget slot under a ConvokeTaps point must be reported"
    );
    assert!(
        order_slot_disagreement(
            "hostile",
            &DecisionSlot::first(source, ChoicePoint::AnnouncedTarget),
        )
        .is_some(),
        "hostile/refused: an Order slot at a published point's choice must be reported"
    );
}

/// The `DecisionPointKind` `pin_answers_point` pairs each `PinnedDecision` variant with.
/// `Order` has no read-side peer — no published point carries `ChoicePoint::TriggerOrder`.
fn kind_paired_with_pin(tag: &str) -> Option<&'static str> {
    match tag {
        "Targets" => Some("Targets"),
        "Mode" => Some("Mode"),
        "MayChoice" => Some("MayChoice"),
        "UnlessBreak" => Some("UnlessBreak"),
        "ManaColor" => Some("ManaColor"),
        "ConvokeTaps" => Some("ConvokeTaps"),
        _ => None,
    }
}

fn kind_tag(kind: &engine::analysis::decision_template::DecisionPointKind) -> &'static str {
    use engine::analysis::decision_template::DecisionPointKind as K;

    match kind {
        K::Targets { .. } => "Targets",
        K::ConvokeTaps { .. } => "ConvokeTaps",
        K::Mode { .. } => "Mode",
        K::MayChoice => "MayChoice",
        K::UnlessBreak => "UnlessBreak",
        K::ManaColor { .. } => "ManaColor",
    }
}

/// Every published `DecisionPoint` in the document whose slot equals `slot` — the whole-slot
/// equality `pin_answers_point` itself performs.
fn collect_published_points(
    value: &serde_json::Value,
    slot: &DecisionSlot,
    out: &mut Vec<engine::analysis::decision_template::DecisionPointKind>,
) {
    use engine::analysis::decision_template::DecisionPoint;

    match value {
        serde_json::Value::Object(map) => {
            if map.len() == 2 && map.contains_key("slot") && map.contains_key("kind") {
                if let Ok(point) = serde_json::from_value::<DecisionPoint>(value.clone()) {
                    if &point.slot == slot {
                        out.push(point.kind);
                    }
                }
            }
            for child in map.values() {
                collect_published_points(child, slot, out);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                collect_published_points(child, slot, out);
            }
        }
        _ => {}
    }
}
