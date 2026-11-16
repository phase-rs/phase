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
//! decides whether an id is a player's or an object's.

use std::collections::BTreeMap;

use engine::analysis::decision_template::AnnouncementSubject;
use engine::game::combat::AttackTarget;
use engine::game::game_object::AttachTarget;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;
use serde::Deserialize;
use serde_json::Value;

use super::game::{object_label, seat_label};
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

/// `verb` followed by the payload's non-empty fields: `Verb (Field: value, ..)`.
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
        .map(|(field, value)| {
            format!(
                "{}: {}",
                humanize_identifier(field),
                render_value(ids, field, value)
            )
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

/// Render one payload field: the players and objects it names as `ids` names
/// them, everything else in a compact JSON-ish form.
fn render_value(ids: &impl IdNames, field: &str, value: &Value) -> String {
    if let Some(named) = ids.name(field, value) {
        return named;
    }
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
            .map(|(key, item)| {
                format!(
                    "{} {}",
                    humanize_identifier(key),
                    render_value(ids, key, item)
                )
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
            // The engine reuses three field names for different kinds of
            // value, so for these the action decides. A block pairs each
            // blocker with the attacker it blocks, where a damage assignment
            // pairs a recipient with an amount; `LearnOption::Rummage` names
            // the card it discards by object; and only `ReorderHand` orders
            // objects rather than indices.
            (GameAction::DeclareBlockers { .. }, "assignments")
            | (GameAction::LearnDecision { .. }, "card_id")
            | (GameAction::ReorderHand { .. }, "order") => Some(FieldIds::Objects),
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
        assert!(rendered.starts_with("Play Land ("), "{rendered}");
        // `card_id` must NOT be resolved through the object table.
        assert!(rendered.contains("Card ID: 3"), "{rendered}");
        assert!(rendered.contains("Object ID: object #7"), "{rendered}");
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
                    "Cast Spell As Sneak (Card ID: {}, Creature To Return: {}, Hand Object: {}, \
                     Payment Mode: Type Auto)",
                    board.technique.0,
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
}
