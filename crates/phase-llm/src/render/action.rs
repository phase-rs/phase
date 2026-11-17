//! Human-readable rendering of an engine [`GameAction`].
//!
//! Written as a generic walk over the action's own serialized payload rather
//! than a match over its ~100 variants. That is deliberate and is the only
//! approach that satisfies "build for the class, not the card": a new
//! `GameAction` variant's shape is described the day it is added. The
//! engine's `#[serde(tag = "type", content = "data")]` representation and its
//! `strum::IntoStaticStr` variant name are the two facts this relies on, and
//! both are structural.
//!
//! Ids are where structure runs out: an object id, a player id, a card id and
//! an amount all serialize as bare numbers. A number is named by the field that
//! carries it, from the explicit lists below (a vote's candidate index through
//! the pending decision), so a new id field whose name is not in them must be
//! added there. The engine's target types — [`TargetRef`], [`AttackTarget`],
//! [`AttachTarget`] and an announcement's [`AnnouncementSubject`] — are instead
//! read back as those types, so their variant, not the field they sit in,
//! decides whether an id is a player's or an object's. A mana-source
//! activation's [`ManaSourceSelection`] is read back the same way and named by
//! the mana it makes, since most of its fields are the engine's matching key
//! for the activation rather than facts about it.
//!
//! A few fields are the engine's bookkeeping and say nothing a reader can use;
//! the walk leaves them out ([`INTERNAL_FIELDS`]).

use std::collections::BTreeMap;

use engine::analysis::decision_template::AnnouncementSubject;
use engine::game::combat::AttackTarget;
use engine::game::game_object::AttachTarget;
use engine::game::mana_sources::produced_mana_for_action;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{
    ManaSourceOutput, ManaSourcePenalty, ManaSourceQuantity, ManaSourceSelection,
};
use engine::types::player::PlayerId;
use serde::Deserialize;
use serde_json::Value;

use super::game::{mana_symbols, object_label, seat_label};
use super::text::one_line;

/// Payload fields whose numeric values name a player rather than an object.
/// An explicit list, not a heuristic: guessing wrong here would print
/// "Player 7" for an object id.
///
/// Checked before [`names_objects`], so `player_id` is never read as an object
/// id for its `_id` suffix.
const PLAYER_FIELDS: &[&str] = &[
    "player",
    "players",
    "player_id",
    "controller",
    "owner",
    "opponent",
    "recipient",
    "chooser",
    "representative",
    "defender",
    "attacking_player",
    "target_player",
    "protector",
];

/// Fields carrying object ids that do not follow the `_id` suffix convention.
const OBJECT_FIELDS: &[&str] = &["targets", "attackers", "blockers", "cards", "permanents"];

/// `_id` fields that hold some other engine id: a card in the card database, a
/// unit of mana in a pool, a loop-shortcut route or breakpoint, a continuous
/// effect. Resolving one through the object table would print an unrelated
/// card.
const NON_OBJECT_ID_FIELDS: &[&str] = &[
    "card_id",
    "card_ids",
    "pip_id",
    "route_id",
    "breakpoint_id",
    "continuous_effect_id",
];

/// More fields an action payload uses for object ids without the `_id` suffix.
///
/// Kept apart from [`OBJECT_FIELDS`] because the board's delayed-trigger
/// conditions are rendered through [`describe_tagged`] too, and the rules-text
/// structures they carry use some of these words for other things.
const ACTION_OBJECT_FIELDS: &[&str] = &[
    "attacks",
    "bands",
    "card",
    "choices",
    "creature",
    "creature_to_return",
    "hand_object",
    "keep",
    "kept",
    "partner",
    "pile_a",
    "selection",
    "source",
    "target",
];

/// Fields the walk leaves out, as bookkeeping that names nothing a reader can
/// act on. A card id indexes the card database: the object the action works on
/// is named beside it by its object id. A payment mode is how the engine pays a
/// cost it has already judged affordable; the engine offers one mode per
/// option, so the mode never tells two options apart.
///
/// A field the action itself names is kept: `LearnOption::Rummage` carries the
/// card it discards, an object, under `card_id` (see [`ActionIds::field_ids`]).
const INTERNAL_FIELDS: &[&str] = &["card_id", "payment_mode"];

/// A one-line description of what `action` does, with the players and objects
/// it refers to named as the seat `viewer` sees them.
///
/// `state` must be the viewer-filtered state the board is rendered from
/// (`engine::game::visibility::filter_state_for_viewer`), so the description
/// names a target in the board's own words and never names what the board
/// hides.
pub fn describe_action(state: &GameState, viewer: PlayerId, action: &GameAction) -> String {
    let verb = humanize_identifier(<&'static str>::from(action));
    let Ok(value) = serde_json::to_value(action) else {
        return verb;
    };
    let ids = ActionIds {
        state,
        viewer,
        action,
    };
    // A unit variant (`PassPriority`) has no payload; the verb is the whole
    // description.
    describe_payload(&ids, verb, value.get("data"))
}

/// A one-line description of any engine enum value, read from its serialized
/// form: a bare unit-variant string, an internally tagged object
/// (`{"type": "AtNextPhase", "phase": "End"}`), or an adjacently tagged one
/// (`{"type": .., "data": {..}}`). The same structural walk
/// [`describe_action`] uses, so a new variant of any such enum is described the
/// day it is added.
pub fn describe_tagged(state: &GameState, value: &Value) -> String {
    let ids = BoardIds { state };
    match value {
        Value::String(variant) => humanize_identifier(variant),
        Value::Object(fields) => {
            let Some(kind) = fields.get("type").and_then(Value::as_str) else {
                return render_value(&ids, "", value);
            };
            let verb = humanize_identifier(kind);
            match fields.get("data") {
                Some(data) => describe_payload(&ids, verb, Some(data)),
                None => {
                    let rest: serde_json::Map<String, Value> = fields
                        .iter()
                        .filter(|(field, _)| field.as_str() != "type")
                        .map(|(field, item)| (field.clone(), item.clone()))
                        .collect();
                    describe_payload(&ids, verb, Some(&Value::Object(rest)))
                }
            }
        }
        other => render_value(&ids, "", other),
    }
}

/// `verb` followed by the payload's non-empty fields, [`INTERNAL_FIELDS`] left
/// out: `Verb (Field: value, ..)`.
fn describe_payload(ids: &impl IdNames, verb: String, data: Option<&Value>) -> String {
    let Some(data) = data.and_then(Value::as_object) else {
        return verb;
    };

    // `BTreeMap` for a stable field order: the same decision must render
    // identically on every call, or the fingerprint that guards against a stale
    // decision would flap.
    let ordered: BTreeMap<&String, &Value> = data.iter().collect();
    let details: Vec<String> = ordered
        .into_iter()
        .filter(|(_, value)| !is_empty_value(value))
        .filter_map(|(field, value)| {
            let rendered = render_field(ids, field, value)?;
            Some(format!("{}: {rendered}", humanize_identifier(field)))
        })
        .collect();

    // Folded at the exit, not per leaf: a payload string is humanized word by
    // word and can still carry a line break, and every leaf would otherwise need
    // to remember that.
    if details.is_empty() {
        verb
    } else {
        one_line(&format!("{verb} ({})", details.join(", ")))
    }
}

/// The primary object an action operates on, when it has one. Used to lead a
/// candidate line with the card name — the highest-signal token for a model
/// that already knows the card.
///
/// `state` is the viewer-filtered state, as for [`describe_action`].
pub fn primary_object_name(state: &GameState, action: &GameAction) -> Option<String> {
    // The engine's exhaustive source-object authority, not a list kept here:
    // every action that works on one permanent or card — a mana ability on an
    // artifact or creature as much as a spell cast from hand — leads with that
    // object's name, including variants added after this was written.
    object_name(state, action.source_object()?)
}

fn object_name(state: &GameState, id: ObjectId) -> Option<String> {
    state.objects.get(&id).map(|object| one_line(&object.name))
}

/// `snake_case` / `CamelCase` -> `Title Case`, with the repository's existing
/// `id -> ID` convention preserved.
pub fn humanize_identifier(raw: &str) -> String {
    let spaced = split_camel_case(raw);
    spaced
        .split(['_', ' '])
        .filter(|word| !word.is_empty())
        .map(|word| match word.to_ascii_lowercase().as_str() {
            "id" => "ID".to_string(),
            "ids" => "IDs".to_string(),
            _ => {
                let mut chars = word.chars();
                chars.next().map_or_else(String::new, |first| {
                    first.to_uppercase().collect::<String>() + chars.as_str()
                })
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn split_camel_case(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 4);
    for (index, c) in raw.char_indices() {
        if index > 0 && c.is_uppercase() {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

fn is_empty_value(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(items) => items.is_empty(),
        Value::Object(fields) => fields.is_empty(),
        Value::String(text) => text.is_empty(),
        _ => false,
    }
}

/// Render one payload value: the players and objects it names as `ids` names
/// them, everything else in a compact JSON-ish form.
fn render_value(ids: &impl IdNames, field: &str, value: &Value) -> String {
    ids.name(field, value)
        .unwrap_or_else(|| render_unnamed(ids, field, value))
}

/// Render one field of a payload, or of an object nested in one, as
/// [`render_value`] does; `None` for one of the [`INTERNAL_FIELDS`] that `ids`
/// does not name.
fn render_field(ids: &impl IdNames, field: &str, value: &Value) -> Option<String> {
    match ids.name(field, value) {
        Some(named) => Some(named),
        None if INTERNAL_FIELDS.contains(&field) => None,
        None => Some(render_unnamed(ids, field, value)),
    }
}

/// A value `ids` does not name, by its JSON shape.
fn render_unnamed(ids: &impl IdNames, field: &str, value: &Value) -> String {
    match value {
        Value::String(text) => humanize_identifier(text),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::Null => "none".to_string(),
        Value::Array(items) => items
            .iter()
            .map(|item| render_value(ids, field, item))
            .collect::<Vec<_>>()
            .join(", "),
        Value::Object(fields) => fields
            .iter()
            .filter_map(|(key, item)| {
                let rendered = render_field(ids, key, item)?;
                Some(format!("{} {rendered}", humanize_identifier(key)))
            })
            .collect::<Vec<_>>()
            .join(", "),
    }
}

/// How a payload walk names the players and objects it meets.
///
/// The board and an option label share the walk but not the naming: the board
/// describes rules-text structures (delayed-trigger conditions) where a field
/// name is weaker evidence of an id than it is in an action's payload.
trait IdNames {
    /// `value`, met under `field`, rendered as the players or objects it names,
    /// or `None` to let the walk render it and go on into it.
    fn name(&self, field: &str, value: &Value) -> Option<String>;
}

/// The board's naming, for the delayed-trigger conditions it describes: object
/// fields by card name, player fields as `Player N`.
struct BoardIds<'a> {
    state: &'a GameState,
}

impl IdNames for BoardIds<'_> {
    fn name(&self, field: &str, value: &Value) -> Option<String> {
        if names_players(field) {
            return Some(map_scalars(value, |number| format!("Player {number}")));
        }
        if names_objects(field) {
            return Some(map_scalars(value, |number| {
                object_name(self.state, ObjectId(number))
                    .unwrap_or_else(|| format!("object #{number}"))
            }));
        }
        None
    }
}

/// What the numbers in one field of an action's payload name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldIds {
    Objects,
    Players,
    /// A list of `(object, amount)` pairs: each object and the damage it is
    /// assigned.
    ObjectAmounts,
    /// CR 701.38b: an index into the pending object-pool vote's candidates,
    /// the objects the engine offers a voter by position so same-named
    /// candidates stay apart.
    VoteCandidates,
    /// CR 605.3a: the one mana-source activation a mana action selects, named
    /// by what it makes ([`ActionIds::mana_source`]).
    ManaSource,
}

/// An option label's naming: every player and object id `action`'s payload
/// carries, and an object-pool vote's candidate index, as the seat `viewer`
/// sees them in `state`, the viewer-filtered state the board is rendered from.
/// A player is named by seat and an object by [`object_label`], whatever field
/// carries it: an option is only as clear as the least clear object it names,
/// and two same-named permanents the action could mean either of are as
/// ambiguous whether they are its targets, the attackers it declares or the
/// legend it keeps.
struct ActionIds<'a> {
    state: &'a GameState,
    viewer: PlayerId,
    action: &'a GameAction,
}

impl ActionIds<'_> {
    /// What the numbers in `field` of this action's payload name.
    fn field_ids(&self, field: &str) -> Option<FieldIds> {
        match (self.action, field) {
            // The engine reuses four field names for different kinds of
            // value, so for these the action decides. A block pairs each
            // blocker with the attacker it blocks, where a damage assignment
            // pairs a recipient with an amount; `LearnOption::Rummage` names
            // the card it discards by object; only `ReorderHand` orders
            // objects rather than indices; and a mana action's `selection` is
            // a mana-source activation, not an object.
            (GameAction::DeclareBlockers { .. }, "assignments")
            | (GameAction::LearnDecision { .. }, "card_id")
            | (GameAction::ReorderHand { .. }, "order") => Some(FieldIds::Objects),
            (
                GameAction::TapLandForMana { .. } | GameAction::ActivateManaSource { .. },
                "selection",
            ) => Some(FieldIds::ManaSource),
            (_, "assignments") => Some(FieldIds::ObjectAmounts),
            (GameAction::SubmitVoteCandidate { .. }, "candidate_index") => {
                Some(FieldIds::VoteCandidates)
            }
            (_, field) if names_players(field) => Some(FieldIds::Players),
            (_, field) if names_objects(field) || ACTION_OBJECT_FIELDS.contains(&field) => {
                Some(FieldIds::Objects)
            }
            _ => None,
        }
    }

    /// A player or object as the board names it: a player by seat (`You`,
    /// `Player N`), an object by [`object_label`].
    fn named(&self, reference: TargetRef) -> String {
        match reference {
            TargetRef::Object(id) => object_label(self.state, self.viewer, id)
                .unwrap_or_else(|| format!("object #{}", id.0)),
            TargetRef::Player(player) => seat_label(player, self.viewer),
        }
    }

    /// `object, amount` for each pair, with the object named.
    fn object_amounts(&self, pairs: &[Value]) -> Option<String> {
        let rendered = pairs
            .iter()
            .map(|pair| match pair.as_array()?.as_slice() {
                [object, amount] => Some(format!(
                    "{}, {amount}",
                    self.named(TargetRef::Object(ObjectId(object.as_u64()?)))
                )),
                _ => None,
            })
            .collect::<Option<Vec<String>>>()?;
        Some(rendered.join(", "))
    }

    /// CR 605.3a + CR 106.6: a mana-source activation as what it does: which
    /// of its source's abilities, the mana it makes, any penalty for
    /// activating it, any restriction on spending that mana, and the source.
    ///
    /// `selection` is the engine's key for one live activation (`value` its
    /// serialized form), and parts of it describe nothing: the source's
    /// incarnation (CR 400.7), and `mana_type`, which is one representative
    /// type of the output and, while the color is still to be chosen, an inert
    /// colorless marker.
    ///
    /// CR 106.3: the mana is what the activation would add, read from the
    /// engine's own authority, `mana_sources::produced_mana_for_action`, which
    /// resolves the selection as the action's reducer does. That is every unit
    /// (Lotus Field's `{R}{R}{R}`, Sol Ring's `{C}{C}`, a filter land's
    /// `{W}{U}`), not the one type the selection names. A color still to be
    /// chosen on activation is said to be. A concrete selection the engine no
    /// longer resolves, or resolves to no mana, names no live activation, so
    /// its yield is unknown and no mana is stated.
    fn mana_source(&self, selection: &ManaSourceSelection, value: &Value) -> String {
        let mana = match selection.output {
            ManaSourceOutput::DeferredColorChoice {
                quantity: ManaSourceQuantity::Fixed(amount),
            } => Some(format!("{amount}, color chosen on activation")),
            ManaSourceOutput::DeferredColorChoice {
                quantity: ManaSourceQuantity::Variable,
            } => Some("amount and color set on activation".to_string()),
            ManaSourceOutput::Concrete(_) => produced_mana_for_action(self.state, self.action)
                .filter(|produced| !produced.is_empty())
                .map(|produced| mana_symbols(produced.iter().map(|unit| unit.mana_type))),
        };
        // The walk words the restrictions and the penalty as it words any
        // other engine value.
        let walked = |field: &str| {
            value
                .get(field)
                .map(|item| render_value(self, field, item))
                .unwrap_or_default()
        };

        let mut facts = Vec::new();
        if let Some(index) = selection.ability_index {
            facts.push(format!("Ability Index {index}"));
        }
        if let Some(mana) = mana {
            facts.push(format!("Mana {mana}"));
        }
        if selection.penalty != ManaSourcePenalty::None {
            facts.push(format!("Penalty {}", walked("penalty")));
        }
        if !selection.restrictions.is_empty() {
            facts.push(format!("Restrictions {}", walked("restrictions")));
        }
        facts.push(format!(
            "Source {}",
            self.named(TargetRef::Object(selection.source.object_id))
        ));
        facts.join(", ")
    }
}

impl IdNames for ActionIds<'_> {
    fn name(&self, field: &str, value: &Value) -> Option<String> {
        if let Some(target) = engine_target(value) {
            return Some(self.named(target));
        }
        // Matched on the field's kind alone, so a new kind cannot compile
        // without its arm. A value of another shape (a list, an absent
        // optional id, a nested value) returns `None` and the walk renders it:
        // a list's elements meet this field again, and an object's members are
        // named under their own keys.
        match self.field_ids(field)? {
            FieldIds::ObjectAmounts => self.object_amounts(value.as_array()?),
            FieldIds::Objects => Some(self.named(TargetRef::Object(ObjectId(value.as_u64()?)))),
            FieldIds::Players => {
                let player = u8::try_from(value.as_u64()?).ok()?;
                Some(self.named(TargetRef::Player(PlayerId(player))))
            }
            FieldIds::VoteCandidates => {
                let WaitingFor::VoteChoice {
                    candidate_objects, ..
                } = &self.state.waiting_for
                else {
                    return None;
                };
                let index = usize::try_from(value.as_u64()?).ok()?;
                let candidate = *candidate_objects.get(index)?;
                Some(self.named(TargetRef::Object(candidate)))
            }
            FieldIds::ManaSource => {
                let selection = ManaSourceSelection::deserialize(value).ok()?;
                Some(self.mana_source(&selection, value))
            }
        }
    }
}

/// The player or object an engine target value names, read back as the
/// engine's own target types: [`TargetRef`] (`{"Object": 2}`, `{"Player":
/// 1}`), [`AttackTarget`] and [`AttachTarget`] (`{"type": "Player", "data":
/// 1}`), and the seat a loop shortcut's announcement names
/// ([`AnnouncementSubject::Seat`], `{"Seat": 1}`). Their variant decides
/// whether the id is a player's or an object's, so a player target is never
/// looked up in the object table.
fn engine_target(value: &Value) -> Option<TargetRef> {
    if !value.is_object() {
        return None;
    }
    if let Ok(target) = TargetRef::deserialize(value) {
        return Some(target);
    }
    if let Ok(target) = AttackTarget::deserialize(value) {
        return Some(match target {
            AttackTarget::Player(player) => TargetRef::Player(player),
            AttackTarget::Planeswalker(id) | AttackTarget::Battle(id) => TargetRef::Object(id),
        });
    }
    if let Ok(target) = AttachTarget::deserialize(value) {
        return Some(match target {
            AttachTarget::Object(id) => TargetRef::Object(id),
            AttachTarget::Player(player) => TargetRef::Player(player),
        });
    }
    // An announced object is a `DecisionSource` identity, not an object id; the
    // walk goes on into it and names a `ThisObject` source by its `source_id`
    // (an `AllCopies` source carries only a `card_id`, which names no object).
    match AnnouncementSubject::deserialize(value).ok()? {
        AnnouncementSubject::Seat(player) => Some(TargetRef::Player(player)),
        AnnouncementSubject::Object(_) => None,
    }
}

/// `card_id` and the other [`NON_OBJECT_ID_FIELDS`] are deliberately excluded:
/// they index other tables, not the object table.
fn names_objects(field: &str) -> bool {
    if NON_OBJECT_ID_FIELDS.contains(&field) {
        return false;
    }
    OBJECT_FIELDS.contains(&field) || field.ends_with("_id") || field.ends_with("_ids")
}

fn names_players(field: &str) -> bool {
    PLAYER_FIELDS.contains(&field)
}

/// A short description of the decision the engine is waiting on, from the
/// `WaitingFor` variant name and (when the prompt has one) the engine-latched
/// display name of the source that raised it.
pub fn describe_waiting_for(waiting_for: &engine::types::game_state::WaitingFor) -> String {
    let Ok(value) = serde_json::to_value(waiting_for) else {
        return "a decision".to_string();
    };
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .map_or_else(|| "a decision".to_string(), humanize_identifier);
    let source = value
        .get("data")
        .and_then(|data| data.get("source"))
        .and_then(|source| source.get("display_name"))
        .and_then(Value::as_str);
    match source {
        Some(name) => format!("{kind} (from {})", one_line(name)),
        None => kind,
    }
}

/// Apply `render` to every number in `value`, preserving array structure.
fn map_scalars(value: &Value, render: impl Fn(u64) -> String + Copy) -> String {
    match value {
        Value::Number(number) => number.as_u64().map_or_else(|| number.to_string(), render),
        Value::Array(items) => items
            .iter()
            .map(|item| map_scalars(item, render))
            .collect::<Vec<_>>()
            .join(", "),
        Value::Null => "none".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::identifiers::CardId;

    fn empty_state() -> GameState {
        GameState::default()
    }

    #[test]
    fn a_unit_variant_renders_as_its_verb_alone() {
        assert_eq!(
            describe_action(&empty_state(), PlayerId(0), &GameAction::PassPriority),
            "Pass Priority"
        );
    }

    #[test]
    fn a_payload_variant_lists_its_fields_in_a_stable_order() {
        let action = GameAction::PlayLand {
            object_id: ObjectId(7),
            card_id: CardId(3),
        };
        let state = empty_state();
        let rendered = describe_action(&state, PlayerId(0), &action);
        assert_eq!(rendered, describe_action(&state, PlayerId(0), &action));
        // The card id is left out: it indexes the card database, not the
        // object table, and the object id beside it names the card.
        assert_eq!(rendered, "Play Land (Object ID: object #7)");
    }

    /// Issue #10997: a cast option printed the card database's id and the
    /// engine's payment mode (`Card ID: 100, ..., Payment Mode: Type Auto`).
    #[test]
    fn a_cast_leaves_out_its_card_id_and_payment_mode() {
        let board = board();
        let cast = described(
            &board,
            GameAction::CastSpell {
                object_id: board.bolt,
                card_id: CardId(board.bolt.0),
                targets: vec![],
                payment_mode: Default::default(),
            },
        );
        assert_eq!(
            cast,
            "Cast Spell (Object ID: Lightning Bolt (hand, owned by You))"
        );
    }

    /// Every action on one object leads with that object's name — including
    /// mana-source actions, so an artifact a seat can tap for mana reads as
    /// that artifact rather than as an anonymous payload.
    #[test]
    fn the_primary_object_comes_from_the_engines_source_object_authority() {
        use engine::game::create_object;
        use engine::types::mana::ManaType;
        use engine::types::player::PlayerId;
        use engine::types::zones::Zone;

        let mut state = empty_state();
        let rock = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Mind Stone".to_string(),
            Zone::Battlefield,
        );
        let action = GameAction::TapForConvoke {
            object_id: rock,
            mana_type: ManaType::Colorless,
        };
        assert_eq!(
            primary_object_name(&state, &action).as_deref(),
            Some("Mind Stone")
        );
        assert_eq!(primary_object_name(&state, &GameAction::PassPriority), None);
    }

    #[test]
    fn tagged_values_describe_as_their_variant_and_fields() {
        let state = empty_state();
        let internal = serde_json::json!({ "type": "AtNextPhase", "phase": "End" });
        assert_eq!(
            describe_tagged(&state, &internal),
            "At Next Phase (Phase: End)"
        );
        let adjacent = serde_json::json!({ "type": "WhenLeavesPlay", "data": { "object_id": 9 } });
        assert_eq!(
            describe_tagged(&state, &adjacent),
            "When Leaves Play (Object ID: object #9)"
        );
        assert_eq!(
            describe_tagged(&state, &serde_json::json!("EndOfTurn")),
            "End Of Turn"
        );
    }

    #[test]
    fn identifier_humanization_keeps_the_id_convention() {
        assert_eq!(humanize_identifier("source_id"), "Source ID");
        assert_eq!(humanize_identifier("CastSpell"), "Cast Spell");
        assert_eq!(
            humanize_identifier("card_instance_ids"),
            "Card Instance IDs"
        );
    }

    #[test]
    fn player_fields_render_as_players_and_object_fields_do_not() {
        assert!(names_players("controller"));
        assert!(names_players("player_id"));
        assert!(!names_players("source_id"));
        assert!(names_objects("targets"));
        assert!(names_objects("source_id"));
        for other_table in NON_OBJECT_ID_FIELDS {
            assert!(!names_objects(other_table), "{other_table}");
        }
    }

    const VIEWER: PlayerId = PlayerId(0);
    const OPPONENT: PlayerId = PlayerId(1);

    /// Issue #10939's board, with printed cards for every role the tests give
    /// them, the viewer as Player 0. The viewer's Grizzly Bears is object #1,
    /// the same number as the opponent, Player 1, so a player id looked up in
    /// the object table would print "Grizzly Bears".
    struct Board {
        state: GameState,
        bears: ObjectId,
        angel: ObjectId,
        jace: ObjectId,
        invasion: ObjectId,
        giant: ObjectId,
        thalia: ObjectId,
        bonesplitter: ObjectId,
        curse: ObjectId,
        bolt: ObjectId,
        glacial_ray: ObjectId,
        ninja: ObjectId,
        island: ObjectId,
        technique: ObjectId,
        heroes: [ObjectId; 2],
        revealed: [ObjectId; 2],
    }

    fn board() -> Board {
        use engine::game::create_object;
        use engine::types::zones::Zone;

        let mut state = empty_state();
        let mut card = |owner: PlayerId, name: &str, zone: Zone| {
            let card_id = CardId(state.next_object_id);
            create_object(&mut state, card_id, owner, name.to_string(), zone)
        };
        let bears = card(VIEWER, "Grizzly Bears", Zone::Battlefield);
        let angel = card(OPPONENT, "Serra Angel", Zone::Battlefield);
        let jace = card(OPPONENT, "Jace Beleren", Zone::Battlefield);
        let invasion = card(OPPONENT, "Invasion of Zendikar", Zone::Battlefield);
        let giant = card(OPPONENT, "Hill Giant", Zone::Battlefield);
        let thalia = card(VIEWER, "Thalia, Guardian of Thraben", Zone::Battlefield);
        let bonesplitter = card(VIEWER, "Bonesplitter", Zone::Battlefield);
        let curse = card(VIEWER, "Curse of the Pierced Heart", Zone::Battlefield);
        let bolt = card(VIEWER, "Lightning Bolt", Zone::Hand);
        let glacial_ray = card(VIEWER, "Glacial Ray", Zone::Hand);
        let ninja = card(VIEWER, "Ninja of the Deep Hours", Zone::Hand);
        let island = card(VIEWER, "Island", Zone::Hand);
        let technique = card(VIEWER, "Karai's Technique", Zone::Hand);
        let heroes = [
            card(VIEWER, "Benalish Hero", Zone::Battlefield),
            card(VIEWER, "Benalish Hero", Zone::Battlefield),
        ];
        // Steam Augury: "Reveal the top five cards of your library and
        // separate them into two piles." Two of them, put in one pile.
        let revealed = [
            card(VIEWER, "Counterspell", Zone::Library),
            card(VIEWER, "Brainstorm", Zone::Library),
        ];
        assert_eq!(bears, ObjectId(u64::from(OPPONENT.0)));
        Board {
            state,
            bears,
            angel,
            jace,
            invasion,
            giant,
            thalia,
            bonesplitter,
            curse,
            bolt,
            glacial_ray,
            ninja,
            island,
            technique,
            heroes,
            revealed,
        }
    }

    fn described(board: &Board, action: GameAction) -> String {
        describe_action(&board.state, VIEWER, &action)
    }

    /// Issue #10939: a `TargetRef` serializes as `{"Object": 2}` or
    /// `{"Player": 1}`. `SelectTargets` printed that JSON, and `ChooseTarget`,
    /// `DistributeAmong` and `RetargetSpell` printed `Object 2`. Each now names
    /// an object target as the board does and a player target by seat, with
    /// object #1 and Player 1 side by side.
    #[test]
    fn target_refs_name_objects_and_players_by_their_variant() {
        use engine::types::ability::TargetRef;

        let board = board();
        assert_eq!(
            described(
                &board,
                GameAction::SelectTargets {
                    targets: vec![
                        TargetRef::Object(board.bears),
                        TargetRef::Player(OPPONENT),
                        TargetRef::Player(VIEWER),
                    ],
                },
            ),
            "Select Targets (Targets: Grizzly Bears (controlled by You), Player 1, You)"
        );
        assert_eq!(
            described(
                &board,
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(board.angel)),
                },
            ),
            "Choose Target (Target: Serra Angel (controlled by Player 1))"
        );
        assert_eq!(
            described(
                &board,
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(OPPONENT)),
                },
            ),
            "Choose Target (Target: Player 1)"
        );
        assert_eq!(
            described(
                &board,
                GameAction::DistributeAmong {
                    distribution: vec![
                        (TargetRef::Object(board.angel), 2),
                        (TargetRef::Player(OPPONENT), 1),
                    ],
                },
            ),
            "Distribute Among (Distribution: Serra Angel (controlled by Player 1), 2, Player 1, 1)"
        );
        assert_eq!(
            described(
                &board,
                GameAction::RetargetSpell {
                    new_targets: vec![
                        Some(TargetRef::Object(board.bears)),
                        None,
                        Some(TargetRef::Player(VIEWER)),
                    ],
                },
            ),
            "Retarget Spell (New Targets: Grizzly Bears (controlled by You), none, You)"
        );
    }

    /// `AttackTarget` and `AttachTarget` are adjacently tagged
    /// (`{"type": "Player", "data": 1}`) and printed as `Data 1, Type Player`.
    #[test]
    fn attack_and_attach_targets_name_their_player_or_object() {
        use engine::game::combat::AttackTarget;
        use engine::game::game_object::AttachTarget;
        use engine::types::actions::DebugAction;

        let board = board();
        let entry = |target| described(&board, GameAction::ChooseEntryAttackTarget { target });
        assert_eq!(
            entry(AttackTarget::Player(OPPONENT)),
            "Choose Entry Attack Target (Target: Player 1)"
        );
        assert_eq!(
            entry(AttackTarget::Planeswalker(board.jace)),
            "Choose Entry Attack Target (Target: Jace Beleren (controlled by Player 1))"
        );
        assert_eq!(
            entry(AttackTarget::Battle(board.invasion)),
            "Choose Entry Attack Target (Target: Invasion of Zendikar (controlled by Player 1))"
        );
        assert_eq!(
            described(
                &board,
                GameAction::DeclareAttackers {
                    attacks: vec![
                        (board.bears, AttackTarget::Player(OPPONENT)),
                        (board.thalia, AttackTarget::Planeswalker(board.jace)),
                    ],
                    bands: vec![],
                },
            ),
            "Declare Attackers (Attacks: Grizzly Bears (controlled by You), Player 1, \
             Thalia, Guardian of Thraben (controlled by You), Jace Beleren (controlled by Player 1))"
        );
        // Never offered to an AI seat, but the same engine type.
        let on_player = described(
            &board,
            GameAction::Debug(DebugAction::Attach {
                object_id: board.curse,
                target: AttachTarget::Player(OPPONENT),
            }),
        );
        assert!(on_player.contains("Target Player 1"), "{on_player}");
        let on_object = described(
            &board,
            GameAction::Debug(DebugAction::Attach {
                object_id: board.bonesplitter,
                target: AttachTarget::Object(board.bears),
            }),
        );
        assert!(
            on_object.contains("Target Grizzly Bears (controlled by You)"),
            "{on_object}"
        );
    }

    /// Fields named for a target carry a bare `ObjectId`: `ChooseEnlist` and
    /// `ChooseRingBearer` printed `Target: 1`.
    #[test]
    fn target_fields_name_their_object_as_the_board_does() {
        let board = board();
        assert_eq!(
            described(
                &board,
                GameAction::ChooseEnlist {
                    target: Some(board.bears),
                },
            ),
            "Choose Enlist (Target: Grizzly Bears (controlled by You))"
        );
        assert_eq!(
            described(
                &board,
                GameAction::ChooseRingBearer {
                    target: board.bears,
                },
            ),
            "Choose Ring Bearer (Target: Grizzly Bears (controlled by You))"
        );
        assert_eq!(
            described(
                &board,
                GameAction::Equip {
                    equipment_id: board.bonesplitter,
                    target_id: board.bears,
                },
            ),
            "Equip (Equipment ID: Bonesplitter (controlled by You), \
             Target ID: Grizzly Bears (controlled by You))"
        );
        let cast = described(
            &board,
            GameAction::CastSpell {
                object_id: board.bolt,
                card_id: CardId(board.bolt.0),
                targets: vec![board.angel],
                payment_mode: Default::default(),
            },
        );
        assert!(
            cast.contains("Object ID: Lightning Bolt (hand, owned by You)"),
            "{cast}"
        );
        assert!(
            cast.contains("Targets: Serra Angel (controlled by Player 1)"),
            "{cast}"
        );
    }

    /// Every other field that carries an object id without the `_id` suffix
    /// printed the bare id. Each now names its object as the board does.
    #[test]
    fn every_other_object_field_names_its_object() {
        use engine::game::combat::AttackTarget;
        use engine::types::actions::LearnOption;

        let board = board();
        let you = |name: &str| format!("{name} (controlled by You)");
        let theirs = |name: &str| format!("{name} (controlled by Player 1)");
        let in_hand = |name: &str| format!("{name} (hand, owned by You)");
        let in_library = |name: &str| format!("{name} (library, owned by You)");
        let cases = [
            (
                GameAction::DeclareBlockers {
                    assignments: vec![(board.bears, board.giant)],
                },
                format!(
                    "Declare Blockers (Assignments: {}, {})",
                    you("Grizzly Bears"),
                    theirs("Hill Giant")
                ),
            ),
            (
                GameAction::AssignCombatDamage {
                    mode: Default::default(),
                    assignments: vec![(board.angel, 3)],
                    trample_damage: 1,
                    controller_damage: 0,
                },
                format!(
                    "Assign Combat Damage (Assignments: {}, 3, Controller Damage: 0, \
                     Mode: Normal, Trample Damage: 1)",
                    theirs("Serra Angel")
                ),
            ),
            (
                GameAction::AssignBlockerDamage {
                    assignments: vec![(board.giant, 2), (board.angel, 1)],
                },
                format!(
                    "Assign Blocker Damage (Assignments: {}, 2, {}, 1)",
                    theirs("Hill Giant"),
                    theirs("Serra Angel")
                ),
            ),
            (
                GameAction::ChooseLegend { keep: board.thalia },
                format!(
                    "Choose Legend (Keep: {})",
                    you("Thalia, Guardian of Thraben")
                ),
            ),
            (
                GameAction::ChoosePair {
                    partner: Some(board.bears),
                },
                format!("Choose Pair (Partner: {})", you("Grizzly Bears")),
            ),
            (
                GameAction::ChooseDamageSource {
                    source: board.giant,
                },
                format!("Choose Damage Source (Source: {})", theirs("Hill Giant")),
            ),
            (
                GameAction::CipherEncode {
                    creature: Some(board.bears),
                },
                format!("Cipher Encode (Creature: {})", you("Grizzly Bears")),
            ),
            (
                GameAction::RespondToSpliceOffer {
                    card: Some(board.glacial_ray),
                },
                format!("Respond To Splice Offer (Card: {})", in_hand("Glacial Ray")),
            ),
            (
                GameAction::FreeCastWindowChoice {
                    selection: Some(board.bolt),
                },
                format!(
                    "Free Cast Window Choice (Selection: {})",
                    in_hand("Lightning Bolt")
                ),
            ),
            (
                GameAction::SubmitPilePartition {
                    pile_a: board.revealed.to_vec(),
                },
                format!(
                    "Submit Pile Partition (Pile A: {}, {})",
                    in_library("Counterspell"),
                    in_library("Brainstorm")
                ),
            ),
            (
                GameAction::ChooseKeptCreatures {
                    kept: vec![board.bears],
                },
                format!("Choose Kept Creatures (Kept: {})", you("Grizzly Bears")),
            ),
            (
                GameAction::ChooseKeptPermanents {
                    kept: vec![board.bonesplitter],
                },
                format!("Choose Kept Permanents (Kept: {})", you("Bonesplitter")),
            ),
            (
                GameAction::SelectCategoryPermanents {
                    choices: vec![Some(board.bears), None],
                },
                format!(
                    "Select Category Permanents (Choices: {}, none)",
                    you("Grizzly Bears")
                ),
            ),
            (
                GameAction::ActivateNinjutsu {
                    ninjutsu_object_id: board.ninja,
                    creature_to_return: board.bears,
                },
                format!(
                    "Activate Ninjutsu (Creature To Return: {}, Ninjutsu Object ID: {})",
                    you("Grizzly Bears"),
                    in_hand("Ninja of the Deep Hours")
                ),
            ),
            (
                GameAction::CastSpellAsSneak {
                    hand_object: board.technique,
                    card_id: CardId(board.technique.0),
                    creature_to_return: board.bears,
                    payment_mode: Default::default(),
                },
                format!(
                    "Cast Spell As Sneak (Creature To Return: {}, Hand Object: {})",
                    you("Grizzly Bears"),
                    in_hand("Karai's Technique")
                ),
            ),
            (
                // CR 702.22c: two creatures with banding attack as one band.
                GameAction::DeclareAttackers {
                    attacks: board
                        .heroes
                        .iter()
                        .map(|hero| (*hero, AttackTarget::Player(OPPONENT)))
                        .collect(),
                    bands: vec![board.heroes.to_vec()],
                },
                format!(
                    "Declare Attackers (Attacks: {hero}, Player 1, {hero}, Player 1, \
                     Bands: {hero}, {hero})",
                    hero = you("Benalish Hero")
                ),
            ),
            (
                GameAction::LearnDecision {
                    choice: LearnOption::Rummage {
                        card_id: board.island,
                    },
                },
                format!(
                    "Learn Decision (Choice: Data Card ID {}, Type Rummage)",
                    in_hand("Island")
                ),
            ),
            (
                GameAction::ReorderHand {
                    order: vec![board.island, board.bolt],
                },
                format!(
                    "Reorder Hand (Order: {}, {})",
                    in_hand("Island"),
                    in_hand("Lightning Bolt")
                ),
            ),
        ];
        for (action, expected) in cases {
            assert_eq!(described(&board, action), expected);
        }
        // The engine reuses `order` for indices: those stay numbers.
        assert_eq!(
            described(&board, GameAction::OrderTriggers { order: vec![1, 0] }),
            "Order Triggers (Order: 1, 0)"
        );
    }

    /// Requirement: a player id is never looked up in the object table, nor is
    /// an id of another engine table. `Concede` printed `Player ID: Grizzly
    /// Bears` and `UnspendPoolMana` printed `Pip ID: Grizzly Bears`, object
    /// #1's name. A player field names the seat as the board does.
    #[test]
    fn ids_of_players_and_other_tables_are_never_read_as_objects() {
        use engine::types::actions::PrecastCopyShortcutResponse;
        use engine::types::mana::ManaPipId;

        let board = board();
        assert_eq!(
            described(
                &board,
                GameAction::Concede {
                    player_id: OPPONENT,
                },
            ),
            "Concede (Player ID: Player 1)"
        );
        assert_eq!(
            described(&board, GameAction::ChooseStartingPlayer { player: VIEWER }),
            "Choose Starting Player (Player: You)"
        );
        assert_eq!(
            described(
                &board,
                GameAction::ChooseBattleProtector {
                    protector: OPPONENT,
                },
            ),
            "Choose Battle Protector (Protector: Player 1)"
        );
        assert_eq!(
            described(
                &board,
                GameAction::UnspendPoolMana {
                    pip_id: ManaPipId(1),
                },
            ),
            "Unspend Pool Mana (Pip ID: 1)"
        );
        let shortcut = described(
            &board,
            GameAction::PrecastCopyShortcut {
                epoch: 1,
                response: PrecastCopyShortcutResponse::Propose { route_id: 1 },
            },
        );
        assert!(!shortcut.contains("Grizzly Bears"), "{shortcut}");
        assert!(shortcut.contains("Route ID 1"), "{shortcut}");
        let shortened = described(
            &board,
            GameAction::PrecastCopyShortcut {
                epoch: 1,
                response: PrecastCopyShortcutResponse::Shorten { breakpoint_id: 1 },
            },
        );
        assert!(shortened.contains("Breakpoint ID 1"), "{shortened}");
        // A copy effect's id, as a mana source's copied value carries it.
        let ids = ActionIds {
            state: &board.state,
            viewer: VIEWER,
            action: &GameAction::PassPriority,
        };
        assert_eq!(
            render_value(&ids, "continuous_effect_id", &serde_json::json!(1)),
            "1"
        );
    }

    /// A loop shortcut the engine offers an AI seat carries its suggested
    /// template, whose target pins can name a seat as an engine
    /// `AnnouncementSubject` (`{"Seat": 1}`). Seat 1 is the opponent, and
    /// object #1 is the viewer's Grizzly Bears.
    #[test]
    fn a_loop_shortcuts_announced_seat_is_named_as_the_board_names_seats() {
        use engine::analysis::decision_template::{
            ChoicePoint, DecisionGroupKey, DecisionKind, DecisionSlot, DecisionTemplate,
            IterationCount, PinnedDecision, Ranking, ReplayMode, TargetPin, TargetSchedule,
        };
        use engine::game::create_object;
        use engine::types::game_state::YieldTarget;
        use engine::types::zones::Zone;

        let mut board = board();
        // The viewer's Blood Artist ("Whenever this creature or another
        // creature dies, target player loses 1 life and you gain 1 life."),
        // its trigger pinned to target one seat every time, as the engine
        // pins an announced target: once to Player 1, once to the viewer.
        let artist = create_object(
            &mut board.state,
            CardId(50),
            VIEWER,
            "Blood Artist".to_string(),
            Zone::Battlefield,
        );
        let source = YieldTarget::ThisObject {
            source_id: artist,
            incarnation: None,
            trigger_description: None,
        };
        for (seat, named) in [(OPPONENT, "Constant Player 1"), (VIEWER, "Constant You")] {
            let template = DecisionTemplate {
                owner: VIEWER,
                decisions: vec![PinnedDecision::Targets {
                    slot: DecisionSlot::first(source.clone(), ChoicePoint::AnnouncedTarget),
                    targets: vec![TargetPin::Scheduled(TargetSchedule::Constant(
                        Ranking::one(AnnouncementSubject::Seat(seat)),
                    ))],
                }],
                replay: ReplayMode::Static,
                key: DecisionGroupKey {
                    sources: vec![(source.clone(), 1)],
                    kind: DecisionKind::LoopChoice,
                },
            };
            let shortcut = described(
                &board,
                GameAction::DeclareShortcut {
                    count: IterationCount::Fixed(3),
                    template: Some(template),
                },
            );
            assert!(shortcut.contains(named), "{shortcut}");
            assert!(!shortcut.contains("Grizzly Bears"), "{shortcut}");
        }
    }

    /// Off the battlefield a target is named with its zone and its owner, and
    /// on the stack by its place in the stack section's numbered list.
    #[test]
    fn a_target_off_the_battlefield_names_its_zone() {
        use engine::game::create_object;
        use engine::types::ability::{Effect, ResolvedAbility, TargetRef};
        use engine::types::game_state::{StackEntry, StackEntryKind};
        use engine::types::zones::Zone;

        let mut board = board();
        let state = &mut board.state;
        let mut card = |owner: PlayerId, name: &str, zone: Zone| {
            create_object(state, CardId(99), owner, name.to_string(), zone)
        };
        // Scavenging Ooze: "Exile target card from a graveyard."
        let buried = card(OPPONENT, "Grizzly Bears", Zone::Graveyard);
        // Pull from Eternity: "Put target face-up exiled card into its
        // owner's graveyard."
        let exiled = card(VIEWER, "Serra Angel", Zone::Exile);
        // Counterspell: "Counter target spell." Two Lightning Bolts, the
        // second cast on top of the first.
        let first_bolt = card(OPPONENT, "Lightning Bolt", Zone::Stack);
        let second_bolt = card(OPPONENT, "Lightning Bolt", Zone::Stack);
        // Stifle: "Counter target activated or triggered ability." Prodigal
        // Sorcerer's ability on top of both: a stack entry with no entry in
        // the object table.
        let sorcerer = card(OPPONENT, "Prodigal Sorcerer", Zone::Battlefield);
        let ability = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        for bolt in [first_bolt, second_bolt] {
            state.stack.push_back(StackEntry {
                id: bolt,
                source_id: bolt,
                controller: OPPONENT,
                kind: StackEntryKind::Spell {
                    card_id: CardId(99),
                    ability: None,
                    casting_variant: Default::default(),
                    actual_mana_spent: 0,
                },
            });
        }
        state.stack.push_back(StackEntry {
            id: ability,
            source_id: sorcerer,
            controller: OPPONENT,
            kind: StackEntryKind::ActivatedAbility {
                source_id: sorcerer,
                ability: Box::new(ResolvedAbility::new(
                    Effect::NoOp,
                    vec![],
                    sorcerer,
                    OPPONENT,
                )),
            },
        });
        assert!(!state.objects.contains_key(&ability));

        let target = |id| {
            described(
                &board,
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(id)),
                },
            )
        };
        assert_eq!(
            target(buried),
            "Choose Target (Target: Grizzly Bears (graveyard, owned by Player 1))"
        );
        assert_eq!(
            target(exiled),
            "Choose Target (Target: Serra Angel (exile, owned by You))"
        );
        // CR 405.2: the last object put on the stack is on top, item 1.
        assert_eq!(
            target(ability),
            "Choose Target (Target: Prodigal Sorcerer (stack item 1, controlled by Player 1))"
        );
        assert_eq!(
            target(second_bolt),
            "Choose Target (Target: Lightning Bolt (stack item 2, controlled by Player 1))"
        );
        assert_eq!(
            target(first_bolt),
            "Choose Target (Target: Lightning Bolt (stack item 3, controlled by Player 1))"
        );
    }

    /// The board's delayed-trigger conditions keep the board's naming: a
    /// player field is `Player N` even for the viewer's own seat.
    #[test]
    fn the_board_path_keeps_its_own_naming() {
        let board = board();
        let condition = serde_json::json!({
            "type": "AtNextPhaseForPlayer",
            "phase": "End",
            "player": 0,
        });
        assert_eq!(
            describe_tagged(&board.state, &condition),
            "At Next Phase For Player (Phase: End, Player: Player 0)"
        );
    }

    /// Issue #10997: a mana option printed the engine's whole selection
    /// (`Atomic Combination none, Mana Type White, Output Data White, Type
    /// Concrete, Penalty None, ..., Source Incarnation 2, ...`). The engine's
    /// own land-tap action for a Plains now reads as the mana it makes.
    #[test]
    fn a_lands_mana_option_names_the_mana_it_makes() {
        use engine::game::mana_sources::activatable_mana_actions_for_player;
        use engine::game::scenario::GameScenario;
        use engine::game::visibility::filter_state_for_viewer;
        use engine::types::mana::ManaColor;
        use engine::types::phase::Phase;

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_basic_land(VIEWER, ManaColor::White);
        let runner = scenario.build();
        let visible = filter_state_for_viewer(runner.state(), VIEWER);

        let actions = activatable_mana_actions_for_player(runner.state(), VIEWER);
        let [action @ GameAction::TapLandForMana { .. }] = actions.as_slice() else {
            panic!("a Plains offers one land tap: {actions:?}");
        };
        let label = describe_action(&visible, VIEWER, action);
        assert!(
            label.starts_with("Tap Land For Mana (Selection: "),
            "{label}"
        );
        assert!(label.contains("Mana {W},"), "{label}");
        assert!(
            label.contains("Source Plains (controlled by You)"),
            "{label}"
        );
        for internal in [
            "Incarnation",
            "Mana Type",
            "Output",
            "Atomic Combination",
            "Penalty",
            "Taps For Mana",
        ] {
            assert!(!label.contains(internal), "{internal} in {label}");
        }
    }

    /// CR 106.3: a mana option states every mana the activation adds. Lotus
    /// Field's land tap for red named one `{R}`, the one type its selection
    /// carries, though the engine adds three.
    #[test]
    fn a_mana_option_states_all_the_mana_it_adds() {
        use engine::game::mana_sources::activatable_mana_actions_for_player;
        use engine::game::scenario::GameScenario;
        use engine::game::visibility::filter_state_for_viewer;
        use engine::types::mana::ManaType;
        use engine::types::phase::Phase;

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let lotus = scenario
            .add_land_from_oracle(
                VIEWER,
                "Lotus Field",
                "Hexproof\nThis land enters tapped.\nWhen this land enters, sacrifice two \
                 lands.\n{T}: Add three mana of any one color.",
            )
            .id();
        let runner = scenario.build();
        let state = runner.state();
        let visible = filter_state_for_viewer(state, VIEWER);
        assert!(!state.objects[&lotus].tapped, "Lotus Field can be tapped");

        // The engine's own land-tap actions for Lotus Field, one per color.
        let labels: Vec<(ManaType, String)> = activatable_mana_actions_for_player(state, VIEWER)
            .iter()
            .filter_map(|action| match action {
                GameAction::TapLandForMana { selection } => {
                    let ManaSourceOutput::Concrete(color) = selection.output else {
                        return None;
                    };
                    // Reach guard: the engine records the complete same-color activation.
                    // Keep that source contract alongside the exact rendered-mana checks.
                    assert_eq!(
                        selection.atomic_combination,
                        Some(vec![color; 3]),
                        "{selection:?}"
                    );
                    Some((color, describe_action(&visible, VIEWER, action)))
                }
                _ => None,
            })
            .collect();
        let red = labels
            .iter()
            .find(|(color, _)| *color == ManaType::Red)
            .map(|(_, label)| label)
            .unwrap_or_else(|| panic!("Lotus Field offers a red land tap: {labels:?}"));
        assert!(red.starts_with("Tap Land For Mana (Selection: "), "{red}");
        assert!(red.contains("Mana {R}{R}{R},"), "{red}");
        assert!(
            red.contains("Source Lotus Field (controlled by You)"),
            "{red}"
        );
        // One row per color, each its own three mana.
        assert_eq!(labels.len(), 5, "{labels:?}");
        let distinct: std::collections::BTreeSet<&String> =
            labels.iter().map(|(_, label)| label).collect();
        assert_eq!(distinct.len(), 5, "{labels:?}");
    }

    /// The same authority reads an artifact's `ActivateManaSource`
    /// activation: Sol Ring's option states both `{C}` it adds.
    #[test]
    fn a_mana_artifacts_option_states_all_the_mana_it_adds() {
        use engine::game::mana_sources::activatable_mana_source_selections;
        use engine::game::scenario::GameScenario;
        use engine::game::visibility::filter_state_for_viewer;
        use engine::types::mana::ManaType;
        use engine::types::phase::Phase;

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_artifact_from_oracle(VIEWER, "Sol Ring", "{T}: Add {C}{C}.");
        let runner = scenario.build();
        let visible = filter_state_for_viewer(runner.state(), VIEWER);

        let selections = activatable_mana_source_selections(runner.state(), VIEWER);
        let [selection] = selections.as_slice() else {
            panic!("Sol Ring offers one activation: {selections:?}");
        };
        // Reach guard: a concrete selection, which the label reads through the
        // engine's output rather than as a color still to be chosen. The
        // planner records Sol Ring's two mana as one atomic combination row.
        assert_eq!(
            selection.output,
            ManaSourceOutput::Concrete(ManaType::Colorless)
        );
        assert_eq!(
            selection.atomic_combination,
            Some(vec![ManaType::Colorless; 2])
        );
        let label = describe_action(
            &visible,
            VIEWER,
            &GameAction::ActivateManaSource {
                selection: selection.clone(),
            },
        );
        assert!(label.contains("Mana {C}{C},"), "{label}");
        assert!(
            label.contains("Source Sol Ring (controlled by You)"),
            "{label}"
        );
    }

    /// While a source's color is still to be chosen, the engine marks the
    /// selection's mana type colorless. The option says the color is chosen
    /// on activation rather than calling the mana colorless.
    #[test]
    fn a_color_chosen_on_activation_is_never_called_colorless() {
        use engine::game::mana_sources::activatable_mana_source_selections;
        use engine::game::scenario::GameScenario;
        use engine::game::visibility::filter_state_for_viewer;
        use engine::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaContribution, ManaProduction,
            QuantityExpr,
        };
        use engine::types::mana::{ManaColor, ManaType};
        use engine::types::phase::Phase;

        // Manalith: "{T}: Add one mana of any color."
        let any_color = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: ManaColor::ALL.to_vec(),
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap);
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario
            .add_creature(VIEWER, "Manalith", 0, 0)
            .as_artifact()
            .with_ability_definition(any_color);
        let runner = scenario.build();
        let visible = filter_state_for_viewer(runner.state(), VIEWER);

        let selections = activatable_mana_source_selections(runner.state(), VIEWER);
        let [selection] = selections.as_slice() else {
            panic!("Manalith offers one activation: {selections:?}");
        };
        // Reach guard: the engine's selection is the deferred one this test is
        // about, with its colorless marker.
        assert!(
            matches!(
                selection.output,
                ManaSourceOutput::DeferredColorChoice { .. }
            ),
            "{selection:?}"
        );
        assert_eq!(selection.mana_type, ManaType::Colorless);

        let label = describe_action(
            &visible,
            VIEWER,
            &GameAction::ActivateManaSource {
                selection: selection.clone(),
            },
        );
        assert!(
            label.contains("Mana 1, color chosen on activation"),
            "{label}"
        );
        assert!(
            !label.contains("Colorless") && !label.contains("{C}"),
            "{label}"
        );
    }

    /// A painland's colored activation carries its damage, Cavern of Souls'
    /// colored mana its spend restriction, and Selvala's an amount set on
    /// activation; each option says so. These are manual activations whose
    /// color is chosen on activation, so their selections carry everything
    /// the label states.
    #[test]
    fn mana_options_state_their_penalty_and_restrictions() {
        use engine::game::create_object;
        use engine::types::identifiers::ObjectIncarnationRef;
        use engine::types::mana::{ManaRestriction, ManaType};
        use engine::types::zones::Zone;

        let mut board = board();
        let mut permanent = |name: &str| {
            create_object(
                &mut board.state,
                CardId(80),
                VIEWER,
                name.to_string(),
                Zone::Battlefield,
            )
        };
        // "{T}: Add {W} or {U}. This land deals 1 damage to you."
        let wastes = permanent("Adarkar Wastes");
        // "{T}: Add one mana of any color. Spend this mana only to cast a
        // creature spell of the chosen type, and that spell can't be
        // countered."
        let cavern = permanent("Cavern of Souls");
        // "{G}, {T}: Add X mana in any combination of colors, where X is the
        // greatest power among creatures you control."
        let selvala = permanent("Selvala, Heart of the Wilds");
        // Dark Depths has no mana ability, so no live activation matches a
        // concrete selection of it.
        let depths = permanent("Dark Depths");

        let selection = |source: ObjectId, quantity: ManaSourceQuantity| ManaSourceSelection {
            source: ObjectIncarnationRef::of(source, 2),
            ability_index: Some(1),
            mana_type: ManaType::Colorless,
            output: ManaSourceOutput::DeferredColorChoice { quantity },
            atomic_combination: None,
            restrictions: Vec::new(),
            penalty: ManaSourcePenalty::None,
            taps_for_mana: Vec::new(),
        };
        let activated = |chosen: ManaSourceSelection| {
            described(&board, GameAction::ActivateManaSource { selection: chosen })
        };

        let painful = ManaSourceSelection {
            penalty: ManaSourcePenalty::DealsDamageOnResolution {
                fixed_amount: Some(1),
            },
            ..selection(wastes, ManaSourceQuantity::Fixed(1))
        };
        assert_eq!(
            activated(painful),
            "Activate Mana Source (Selection: Ability Index 1, \
             Mana 1, color chosen on activation, \
             Penalty Deals Damage On Resolution Fixed Amount 1, \
             Source Adarkar Wastes (controlled by You))"
        );

        let restricted = ManaSourceSelection {
            restrictions: vec![ManaRestriction::OnlyForCreatureType("Angel".to_string())],
            ..selection(cavern, ManaSourceQuantity::Fixed(1))
        };
        assert_eq!(
            activated(restricted),
            "Activate Mana Source (Selection: Ability Index 1, \
             Mana 1, color chosen on activation, \
             Restrictions Only For Creature Type Angel, \
             Source Cavern of Souls (controlled by You))"
        );

        assert_eq!(
            activated(selection(selvala, ManaSourceQuantity::Variable)),
            "Activate Mana Source (Selection: Ability Index 1, \
             Mana amount and color set on activation, \
             Source Selvala, Heart of the Wilds (controlled by You))"
        );

        // A concrete selection no live activation matches has no known yield,
        // and the option states no mana rather than the one type it names.
        let stale = ManaSourceSelection {
            mana_type: ManaType::Black,
            output: ManaSourceOutput::Concrete(ManaType::Black),
            ..selection(depths, ManaSourceQuantity::Fixed(1))
        };
        assert_eq!(
            described(&board, GameAction::TapLandForMana { selection: stale }),
            "Tap Land For Mana (Selection: Ability Index 1, \
             Source Dark Depths (controlled by You))"
        );
    }
}
