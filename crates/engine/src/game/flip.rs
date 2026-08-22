//! CR 710: Flip cards (Kamigawa block).
//!
//! A flip card is a **single-faced** card with a two-part frame. Its top half
//! carries the normal characteristics (CR 710.1a); its bottom half carries an
//! alternative name, text box, type line, power, and toughness that apply only
//! while the permanent is on the battlefield **and** flipped (CR 710.1b).
//!
//! Flipping is therefore NOT transforming: CR 701.27a restricts transforming to
//! permanents represented by double-faced cards/tokens, and CR 710.1c fixes a
//! flip card's color and mana cost across the flip where transforming swaps
//! them. This module keeps its own applicator ([`apply_flipped_face_to_object`])
//! for exactly that reason — reusing the double-faced applicator
//! (`printed_cards::apply_back_face_to_object`) would swap mana cost and color
//! and break CR 710.1c.
//!
//! Not modeled here, deliberately:
//! - CR 710.5 — a player choosing a flip card's *alternative* name for a
//!   "choose a card name" effect. That is a name-choice-menu concern, not a
//!   permanent-status concern, and no current name-choice path consults
//!   alternative faces.
//! - CR 110.5b's "unless a spell or ability says otherwise" entry override,
//!   i.e. Homura, Human Ascendant's "return it to the battlefield flipped".
//!   That is an entry-time replacement rider (the flip-card analogue of
//!   `enter_transformed`), not a flip instruction, and it is the only printed
//!   card in the class. Every permanent therefore enters unflipped, which
//!   [`revert_flip_on_zone_exit`] already guarantees on every zone exit.
//! - Turning a *face-down* flipped permanent back FACE UP (CR 708.8) restores
//!   the NORMAL half, not the alternative one. `GameObject` has a single
//!   `back_face` slot, and a flipped permanent that is turned face down
//!   (Ixidron, Cyber Conversion) must keep the normal half there so the
//!   CR 710.2 zone-exit result stays correct (see `effects::turn_face_down`
//!   and the ordering note in `zones::apply_zone_exit_cleanup`). The
//!   alternative half is therefore not recoverable on a later turn-up; the
//!   permanent's `flipped` status is still retained (CR 710.4 — flipping is
//!   one-way and is never cleared while the permanent stays on the
//!   battlefield). Storing both halves at once needs a second stash slot,
//!   which is a `GameObject`/serialization change beyond this module.
//! - CR 707.3's *second-order* flip copy case (CR 110.5c's Dimir Doppelganger
//!   example): a permanent that is ALREADY flipped and then becomes a copy of
//!   another flip card should show the copied card's ALTERNATIVE half. The
//!   copy pipeline installs copiable values only (CR 707.2 — the normal half,
//!   see [`flipped_normal_copiable_values`]) and never re-derives the copied
//!   card's other half, which again needs a second stash slot.

use std::sync::Arc;

use crate::game::game_object::{BackFaceData, GameObject};
use crate::types::ability::CopiableValues;
use crate::types::card::LayoutKind;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::zones::Zone;

use super::engine::EngineError;
use super::printed_cards::snapshot_object_base_face;

/// CR 710.1: True when `obj` is represented by a CR 710 flip card — i.e. its
/// `back_face` slot holds the *other half* of a flip card rather than the back
/// face of a double-faced card.
///
/// This is the single authority for "is this a flip card?" and the reason
/// [`stash_flip_face`] re-stamps `LayoutKind::Flip` on **both** halves: whether
/// the permanent is currently flipped or not, the half sitting in `back_face`
/// always carries the tag, so no double-faced path (CR 701.27a transform,
/// CR 712.16 turn-face-down, MDFC/Adventure face choice) can ever mistake a
/// flip card for a DFC — not even after a flip → leave-the-battlefield →
/// return round trip.
pub(crate) fn is_flip_permanent(obj: &GameObject) -> bool {
    matches!(
        obj.back_face.as_ref().and_then(|face| face.layout_kind),
        Some(LayoutKind::Flip)
    )
}

/// CR 710.1b + CR 710.1c: Snapshot the half of a flip card that is about to be
/// hidden, for storage in the shared `back_face` slot.
///
/// Two deliberate differences from a plain `snapshot_object_face`:
/// - It reads the **printed/base** characteristics (CR 613.1): the live fields
///   may carry continuous-effect modifications (an anthem, a granted keyword —
///   Student of Elements' own trigger condition is "when this creature has
///   flying"). [`apply_flipped_face_to_object`] writes the stash into both the
///   live and the `base_*` fields, so stashing inflated values would bake a
///   temporary effect into the other half's printed baseline permanently.
/// - It re-stamps `layout_kind: Some(LayoutKind::Flip)` (both snapshot helpers
///   hard-code `None`). Without it, a permanent that flipped and then left the
///   battlefield would sit in its new zone with `flipped == false` AND an
///   untagged `back_face`, and every flip guard keyed on either signal would go
///   false — reopening the CR 710.1c hole where a "transform each ..." effect
///   runs the double-faced applicator on a flip card and swaps the mana cost
///   and color CR 710.1c holds fixed.
fn stash_flip_face(obj: &GameObject) -> BackFaceData {
    let mut face = snapshot_object_base_face(obj);
    face.layout_kind = Some(LayoutKind::Flip);
    face
}

/// CR 710.4: Flip `object_id` — a one-way status change (CR 110.5) after which
/// the permanent's alternative characteristics apply (CR 710.1b).
///
/// Silent no-op (returns `Ok(())`) when the instruction cannot apply, mirroring
/// CR 701.27c's "nothing happens" for the analogous transform instruction:
/// - the object is not on the battlefield (CR 710.1b + CR 710.2: the
///   alternative characteristics exist only for a battlefield permanent),
/// - the permanent is already flipped (CR 710.4: flipping is one-way, so a
///   second instruction has nothing to do),
/// - the card carries no alternative face (not a flip card).
///
/// The pre-flip (normal) characteristics are stashed in `back_face` so
/// `zones::apply_zone_exit_cleanup` can restore them when the permanent leaves
/// the battlefield (CR 710.4 + CR 110.5: a flipped permanent that leaves the
/// battlefield retains no memory of its status).
pub fn flip_permanent(
    state: &mut GameState,
    object_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    let obj = state
        .objects
        .get(&object_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;

    // CR 710.1b + CR 710.2: the alternative characteristics are used only if the
    // permanent is on the battlefield. In every other zone a flip card has only
    // its normal characteristics, so there is nothing to flip.
    if obj.zone != Zone::Battlefield {
        return Ok(());
    }

    // CR 710.4: flipping a permanent is a one-way process — once flipped, it's
    // impossible for it to become unflipped, and a further flip instruction has
    // no effect.
    if obj.flipped {
        return Ok(());
    }

    // CR 710.1: only a flip card has alternative characteristics to flip to.
    let Some(alternative_face) = obj.back_face.clone() else {
        return Ok(());
    };

    let obj = state.objects.get_mut(&object_id).unwrap();

    // CR 710.4 + CR 110.5: stash the normal characteristics so the zone-exit
    // cleanup can restore them — a flipped permanent that leaves the
    // battlefield retains no memory of its flipped status.
    //
    // CR 613.7: the object deliberately keeps its EXISTING timestamp. CR 613.7
    // enumerates every event that grants a new one — 613.7d zone entry, 613.7e
    // attachment, 613.7f turning face up or face down, 613.7g transforming or
    // converting — and flipping is not among them. (CR 613.7a grants nothing on
    // its own; it only says a static ability's continuous effect inherits the
    // object's timestamp, so the alternative text box's statics simply take the
    // timestamp the permanent already has.)
    let normal_face = stash_flip_face(obj);
    apply_flipped_face_to_object(obj, alternative_face);
    obj.back_face = Some(normal_face);
    obj.flipped = true;

    crate::game::layers::mark_layers_full(state);

    events.push(GameEvent::Flipped { object_id });

    Ok(())
}

/// CR 710.4 + CR 110.5 + CR 710.2: Restore a flipped permanent's normal
/// characteristics as it leaves the battlefield. A flipped permanent that
/// leaves the battlefield retains no memory of its status (CR 710.4), and in
/// every zone other than the battlefield a flip card has only the normal
/// characteristics of the card (CR 710.2).
///
/// Called from `zones::apply_zone_exit_cleanup`, which owns the zone-exit
/// status reset for every status category (CR 110.5). It runs this AFTER the
/// CR 708.9 face-down restore — see the ordering note there.
pub(crate) fn revert_flip_on_zone_exit(obj: &mut GameObject) {
    if !obj.flipped {
        return;
    }
    let Some(normal_face) = obj.back_face.clone() else {
        // CR 708.9 + CR 710.4: reached when the permanent was flipped AND then
        // turned face down. `effects::turn_face_down` left the flip stash (the
        // normal half) in `back_face`, and the face-down restore that runs just
        // before this already consumed it — the object is showing the normal
        // half again, so only the status is left to clear (CR 110.5: the new
        // object is unflipped).
        obj.flipped = false;
        return;
    };
    let alternative_face = stash_flip_face(obj);
    apply_flipped_face_to_object(obj, normal_face);
    obj.back_face = Some(alternative_face);
    obj.flipped = false;
}

/// CR 707.2 + CR 707.3: the copiable values of a permanent that is currently
/// flipped are its **normal** (top-half) printed values, not the alternative
/// ones now showing.
///
/// CR 707.2 lists the copiable values as "the values derived from the text
/// printed on the object" and states that status is NOT copied; flipped is a
/// status (CR 110.5). CR 707.3's worked example is literally a flip card
/// (Tomoya the Revealer copying Nezumi Shortfang gets Nezumi's values and its
/// own flipped status then selects Stabwhisker). So a Clone / Phyrexian
/// Metamorph / Kiki-Jiki / token copy of a flipped Kenzo the Hardhearted must
/// be an UNFLIPPED Bushi Tenderfoot 1/1 — reading the object's `base_*` fields
/// would instead yield an unflipped 3/4 legendary Kenzo, because
/// [`apply_flipped_face_to_object`] wrote the alternative half into `base_*`.
///
/// Returns `None` for any object that is not a flipped flip permanent, so
/// `printed_cards::intrinsic_copiable_values` keeps its `base_*` fast path.
pub(crate) fn flipped_normal_copiable_values(obj: &GameObject) -> Option<CopiableValues> {
    if !obj.flipped {
        return None;
    }
    let normal_face = obj.back_face.as_ref()?;
    Some(CopiableValues {
        name: normal_face.name.clone(),
        // CR 710.1c: cost and color never changed across the flip, so the
        // stash and the live object agree here — taken from the stash anyway so
        // every copiable value has one source.
        mana_cost: normal_face.mana_cost.clone(),
        color: normal_face.color.clone(),
        card_types: normal_face.card_types.clone(),
        power: normal_face.power,
        toughness: normal_face.toughness,
        loyalty: normal_face.loyalty,
        printed_loyalty: normal_face.printed_loyalty,
        keywords: normal_face.keywords.clone(),
        abilities: Arc::new(normal_face.abilities.clone()),
        trigger_definitions: Arc::new(
            normal_face
                .trigger_definitions
                .iter_all()
                .cloned()
                .collect(),
        ),
        // CR 707.2 + CR 611.2b: runtime replacements durably parked in the
        // definition set are not printed characteristics — same exclusion the
        // unflipped path applies via `copiable_replacement_definitions`.
        replacement_definitions: Arc::new(
            normal_face
                .replacement_definitions
                .iter_all()
                .filter(|def| !crate::game::printed_cards::is_runtime_non_copiable_replacement(def))
                .cloned()
                .collect(),
        ),
        static_definitions: Arc::new(normal_face.static_definitions.iter_all().cloned().collect()),
        // CR 710.1 + CR 710.2: a flip card is a single card whose normal and
        // alternative characteristics share one face — never one of CR 709.5's
        // shared-type-line Room permanents, so there is no half data to carry.
        room_halves: None,
        name_origin: Default::default(),
    })
}

/// CR 710.1c: Re-assert a flipped permanent's unchanged color and mana cost
/// from the normal half stashed in `back_face`.
///
/// [`apply_flipped_face_to_object`] never touches those four fields, so this is
/// a no-op for a live flip. It exists for the ONE seam that can overwrite them:
/// `printed_cards::reapply_printed_faces_from_card_db` re-applies the printed
/// face named by `printed_ref` — which for a flipped permanent is the
/// alternative half, and an alternative half carries no printed mana cost. On
/// every state reload that would blank the cost CR 710.1c preserves.
pub(crate) fn restore_normal_cost_and_color_if_flipped(obj: &mut GameObject) {
    if !obj.flipped {
        return;
    }
    let Some(normal_face) = obj.back_face.as_ref() else {
        return;
    };
    let mana_cost = normal_face.mana_cost.clone();
    let color = normal_face.color.clone();
    obj.mana_cost = mana_cost.clone();
    obj.base_mana_cost = mana_cost;
    obj.color = color.clone();
    obj.base_color = color;
}

/// CR 710.1b + CR 710.1c: Apply a flip card's *alternative* characteristics to
/// a battlefield permanent.
///
/// This deliberately does NOT delegate to
/// `printed_cards::apply_back_face_to_object` (the double-faced-card
/// applicator). That function swaps mana cost and color, which is correct for
/// CR 712 double-faced cards and **wrong** for CR 710 flip cards.
///
/// Copied from the alternative face (CR 710.1b — "an alternative name, text
/// box, type line, power, and toughness"), each alongside its `base_*` twin so
/// the layer system (CR 613) recomputes from the new printed values:
/// - `name` / `base_name`
/// - `power` / `base_power`, `toughness` / `base_toughness`
/// - `card_types` / `base_card_types` (the alternative type line, which for
///   every printed flip card adds the Legendary supertype)
/// - `keywords` / `base_keywords`
/// - `abilities` / `base_abilities`, `trigger_definitions` (via
///   `install_trigger_base_definitions`), `replacement_definitions` /
///   `base_replacement_definitions`, `static_definitions` /
///   `base_static_definitions` — the alternative text box
/// - `printed_ref` / `base_printed_ref` — the display identity of the half now
///   showing
/// - `loyalty` / `base_loyalty` and `defense` / `base_defense`: not enumerated
///   in CR 710.1b (and no printed flip card has either), but they are
///   type-line-derived printed values (CR 306.5b / CR 310.4b), so they follow
///   the type line rather than leaving a stale top-half value behind.
///
/// Deliberately NOT copied:
/// - `mana_cost` / `base_mana_cost` — CR 710.1c: a flip card's mana cost
///   doesn't change if the permanent is flipped.
/// - `color` / `base_color` — CR 710.1c: a flip card's color doesn't change if
///   the permanent is flipped.
/// - `modal`, `additional_cost`, `strive_cost`, `casting_restrictions`,
///   `casting_options` — CR 710.2: a flip card is cast using only its normal
///   characteristics (it has only those in every zone other than the
///   battlefield), so the alternative half carries no casting properties and
///   must not clobber the normal half's.
///
/// External effects applied to the permanent are untouched (CR 710.1c: "any
/// changes to it by external effects will still apply") — this writes only
/// printed/base values plus their live mirrors, exactly as the layer system
/// expects; `mark_layers_full` then reapplies every continuous effect.
pub(crate) fn apply_flipped_face_to_object(obj: &mut GameObject, face: BackFaceData) {
    // CR 710.1b: alternative name.
    obj.name = face.name.clone();
    obj.base_name = face.name;

    // CR 710.1b: alternative power and toughness.
    obj.power = face.power;
    obj.base_power = face.power;
    obj.layer_base_power = face.power;
    obj.toughness = face.toughness;
    obj.base_toughness = face.toughness;
    obj.layer_base_toughness = face.toughness;

    // CR 306.5b + CR 310.4b: loyalty/defense track the alternative type line.
    obj.loyalty = face.loyalty;
    obj.base_loyalty = face.loyalty;
    obj.defense = face.defense;
    obj.base_defense = face.defense;

    // CR 710.1b: alternative type line.
    obj.card_types = face.card_types.clone();
    obj.base_card_types = face.card_types;

    // CR 710.1b: alternative text box — keywords, abilities, triggers,
    // replacements, and statics all come from the half now showing.
    obj.keywords = face.keywords.clone();
    obj.base_keywords = face.keywords;
    obj.abilities = Arc::new(face.abilities.clone());
    obj.base_abilities = Arc::new(face.abilities);
    obj.replacement_definitions = face.replacement_definitions.clone();
    obj.base_replacement_definitions =
        Arc::new(face.replacement_definitions.iter_all().cloned().collect());
    obj.static_definitions = face.static_definitions.clone();
    obj.base_static_definitions = Arc::new(face.static_definitions.iter_all().cloned().collect());
    obj.install_trigger_base_definitions(Arc::new(
        face.trigger_definitions.iter_all().cloned().collect(),
    ))
    .expect("trigger base-set generation must not overflow");
    obj.base_characteristics_initialized = true;

    // CR 710.1b: the alternative half is what's now shown. Cloned before the
    // move so both the display baseline and the live pointer are set.
    obj.base_printed_ref = face.printed_ref.clone();
    obj.printed_ref = face.printed_ref;

    // CR 710.1c: a flip card's color and mana cost don't change when flipped —
    // `mana_cost`, `base_mana_cost`, `color`, and `base_color` are deliberately
    // left untouched (`face.mana_cost` / `face.color` go unread). This is the
    // single behavioral difference from the double-faced applicator and the
    // reason this function exists rather than delegating to it.

    // CR 710.2: the card is cast using only its normal characteristics, so the
    // normal half's casting properties (`modal`, `additional_cost`,
    // `strive_cost`, `casting_restrictions`, `casting_options`) are deliberately
    // left untouched.
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{AbilityDefinition, AbilityKind, Effect};
    use crate::types::card_type::{CardType, CoreType, Supertype};
    use crate::types::identifiers::CardId;
    use crate::types::keywords::Keyword;
    use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};
    use crate::types::player::PlayerId;

    /// `{W}` — Bushi Tenderfoot's printed mana cost (CR 202.1).
    fn white_mana_cost() -> ManaCost {
        ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        }
    }

    /// Bushi Tenderfoot // Kenzo the Hardhearted, the canonical CR 710 flip
    /// card: a {W} 1/1 Creature — Human Soldier whose alternative half is a 3/4
    /// Legendary Creature — Human Samurai with double strike.
    fn setup_flip_card(state: &mut GameState) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            PlayerId(0),
            "Bushi Tenderfoot".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.power = Some(1);
        obj.toughness = Some(1);
        obj.base_power = Some(1);
        obj.base_toughness = Some(1);
        obj.card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Human".to_string(), "Soldier".to_string()],
        };
        obj.base_card_types = obj.card_types.clone();
        obj.keywords = vec![Keyword::Bushido(1)];
        obj.base_keywords = obj.keywords.clone();
        obj.abilities = Arc::new(vec![AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::FlipPermanent {
                target: crate::types::ability::TargetFilter::SelfRef,
            },
        )]);
        obj.base_abilities = Arc::clone(&obj.abilities);
        obj.mana_cost = white_mana_cost();
        obj.base_mana_cost = obj.mana_cost.clone();
        obj.color = vec![ManaColor::White];
        obj.base_color = vec![ManaColor::White];

        obj.back_face = Some(BackFaceData {
            name: "Kenzo the Hardhearted".to_string(),
            power: Some(3),
            toughness: Some(4),
            loyalty: None,
            printed_loyalty: None,
            defense: None,
            card_types: CardType {
                supertypes: vec![Supertype::Legendary],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Human".to_string(), "Samurai".to_string()],
            },
            // The alternative half of a flip card has no printed mana cost and
            // no printed color indicator — proof that reusing the double-faced
            // applicator would blank both (CR 710.1c).
            mana_cost: ManaCost::default(),
            keywords: vec![Keyword::DoubleStrike, Keyword::Bushido(2)],
            abilities: Vec::new(),
            trigger_definitions: Default::default(),
            replacement_definitions: Default::default(),
            static_definitions: Default::default(),
            color: Vec::new(),
            printed_ref: None,
            modal: None,
            additional_cost: None,
            strive_cost: None,
            casting_restrictions: vec![],
            casting_options: vec![],
            layout_kind: Some(crate::types::card::LayoutKind::Flip),
            parse_warnings: vec![],
        });

        id
    }

    /// CR 710.1b: the alternative name, type line, power, toughness, and text
    /// box replace the normal ones once the permanent is flipped.
    #[test]
    fn flip_applies_the_alternative_face() {
        let mut state = GameState::new_two_player(42);
        let id = setup_flip_card(&mut state);
        let mut events = Vec::new();

        flip_permanent(&mut state, id, &mut events).unwrap();

        let obj = &state.objects[&id];
        assert!(obj.flipped);
        assert_eq!(obj.name, "Kenzo the Hardhearted");
        assert_eq!(obj.base_name, "Kenzo the Hardhearted");
        assert_eq!(obj.power, Some(3));
        assert_eq!(obj.toughness, Some(4));
        assert!(obj.card_types.supertypes.contains(&Supertype::Legendary));
        assert!(obj.card_types.subtypes.iter().any(|s| s == "Samurai"));
        assert!(crate::game::keywords::has_keyword(
            obj,
            &Keyword::DoubleStrike
        ));
        assert!(state.layers_dirty.is_dirty());
        assert_eq!(events, vec![GameEvent::Flipped { object_id: id }]);
    }

    /// CR 710.1c: a flip card's color and mana cost don't change if the
    /// permanent is flipped. Reverting to the double-faced applicator
    /// (`apply_back_face_to_object`) blanks both and fails this test.
    #[test]
    fn flip_preserves_color_and_mana_cost() {
        let mut state = GameState::new_two_player(42);
        let id = setup_flip_card(&mut state);
        let mut events = Vec::new();
        let cost_before = state.objects[&id].mana_cost.clone();

        flip_permanent(&mut state, id, &mut events).unwrap();

        let obj = &state.objects[&id];
        assert_eq!(
            obj.mana_cost, cost_before,
            "CR 710.1c: mana cost must not change when the permanent flips"
        );
        assert_eq!(obj.base_mana_cost, cost_before);
        assert_eq!(
            obj.color,
            vec![ManaColor::White],
            "CR 710.1c: color must not change when the permanent flips"
        );
        assert_eq!(obj.base_color, vec![ManaColor::White]);
    }

    /// CR 710.4: flipping is one-way — a second flip instruction is a no-op and
    /// emits no event.
    #[test]
    fn flipping_an_already_flipped_permanent_is_a_no_op() {
        let mut state = GameState::new_two_player(42);
        let id = setup_flip_card(&mut state);
        let mut events = Vec::new();

        flip_permanent(&mut state, id, &mut events).unwrap();
        let timestamp_after_first = state.objects[&id].timestamp;
        events.clear();

        flip_permanent(&mut state, id, &mut events).unwrap();

        let obj = &state.objects[&id];
        assert!(obj.flipped, "CR 710.4: the permanent stays flipped");
        assert_eq!(obj.name, "Kenzo the Hardhearted");
        assert_eq!(obj.power, Some(3));
        assert!(events.is_empty(), "no second Flipped event");
        assert_eq!(
            obj.timestamp, timestamp_after_first,
            "a no-op flip must not draw a timestamp"
        );
    }

    /// CR 710.1b + CR 710.2: a flip card that is not on the battlefield has
    /// only its normal characteristics — the instruction does nothing.
    #[test]
    fn off_battlefield_object_cannot_flip() {
        let mut state = GameState::new_two_player(42);
        let id = setup_flip_card(&mut state);
        state.objects.get_mut(&id).unwrap().zone = Zone::Graveyard;
        let mut events = Vec::new();

        flip_permanent(&mut state, id, &mut events).unwrap();

        let obj = &state.objects[&id];
        assert!(!obj.flipped);
        assert_eq!(obj.name, "Bushi Tenderfoot");
        assert!(events.is_empty());
    }

    /// CR 710.4 + CR 110.5: a flipped permanent that leaves the battlefield
    /// retains no memory of its status — the graveyard card shows only the
    /// normal characteristics (CR 710.2).
    #[test]
    fn zone_change_resets_flipped_status() {
        let mut state = GameState::new_two_player(42);
        let id = setup_flip_card(&mut state);
        let mut events = Vec::new();

        flip_permanent(&mut state, id, &mut events).unwrap();
        assert!(state.objects[&id].flipped);

        crate::game::zones::move_to_zone(&mut state, id, Zone::Graveyard, &mut events);

        let obj = &state.objects[&id];
        assert!(!obj.flipped, "CR 110.5: the new object is unflipped");
        assert_eq!(obj.name, "Bushi Tenderfoot");
        assert_eq!(obj.power, Some(1));
        assert_eq!(obj.toughness, Some(1));
        assert!(!obj.card_types.supertypes.contains(&Supertype::Legendary));
        assert_eq!(
            obj.mana_cost,
            white_mana_cost(),
            "CR 710.1c: the mana cost was never changed, so the revert restores it unchanged"
        );
    }

    /// A permanent with no alternative face is not a flip card — CR 710.1's
    /// alternative characteristics don't exist, so nothing happens.
    #[test]
    fn non_flip_card_cannot_flip() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(7),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Battlefield,
        );
        let timestamp_before = state.objects[&id].timestamp;
        let mut events = Vec::new();

        flip_permanent(&mut state, id, &mut events).unwrap();

        assert!(!state.objects[&id].flipped);
        assert!(events.is_empty());
        assert_eq!(state.objects[&id].timestamp, timestamp_before);
    }

    /// CR 701.27a + CR 701.27c: a flip card is not represented by a
    /// double-faced card, so an instruction to TRANSFORM it does nothing —
    /// in particular it must not run the double-faced applicator and blank the
    /// mana cost and color that CR 710.1c preserves.
    #[test]
    fn transform_instruction_does_nothing_to_a_flip_card() {
        let mut state = GameState::new_two_player(42);
        let id = setup_flip_card(&mut state);
        let mut events = Vec::new();

        crate::game::transform::transform_permanent(&mut state, id, &mut events).unwrap();

        let obj = &state.objects[&id];
        assert!(!obj.transformed);
        assert!(!obj.flipped);
        assert_eq!(obj.name, "Bushi Tenderfoot");
        assert_eq!(obj.mana_cost, white_mana_cost());
        assert_eq!(obj.color, vec![ManaColor::White]);
        assert!(events.is_empty());
    }

    /// CR 613.7: flipping grants NO new timestamp. CR 613.7 enumerates every
    /// timestamp-granting event — 613.7d zone entry, 613.7e attachment, 613.7f
    /// turning face up/down, 613.7g transforming/converting — and flipping is
    /// not among them (CR 613.7a only says a static ability's continuous effect
    /// inherits its object's timestamp; it grants nothing on its own).
    ///
    /// Discriminating: re-adding a `state.next_timestamp()` bump to
    /// `flip_permanent` makes `after` differ from `before` and fails here.
    #[test]
    fn flip_does_not_grant_a_new_timestamp() {
        let mut state = GameState::new_two_player(42);
        let id = setup_flip_card(&mut state);
        let before = state.objects[&id].timestamp;
        let mut events = Vec::new();

        flip_permanent(&mut state, id, &mut events).unwrap();

        assert!(state.objects[&id].flipped, "reach guard: it really flipped");
        assert_eq!(
            state.objects[&id].timestamp, before,
            "CR 613.7: flipping is not a timestamp-granting event"
        );
    }

    /// CR 710.1c + CR 701.27a: a flip card that flipped, LEFT the battlefield,
    /// and came back is still a flip card — a later "transform each ..." effect
    /// must not run the double-faced applicator on it.
    ///
    /// Discriminating: `stash_flip_face` re-stamps `LayoutKind::Flip` on the
    /// half it parks in `back_face`. Drop that re-stamp (i.e. use a plain
    /// `snapshot_object_*_face`, whose `layout_kind` is hard-coded `None`) and
    /// the returned permanent has `flipped == false` AND an untagged
    /// `back_face`, so both arms of the transform guard go false: the assertions
    /// below on `transformed`, `name`, `mana_cost`, and `color` all fail
    /// (`apply_back_face_to_object` swaps in Kenzo and blanks the {W} cost).
    #[test]
    fn a_flip_card_that_left_and_returned_still_cannot_transform() {
        let mut state = GameState::new_two_player(42);
        let id = setup_flip_card(&mut state);
        let mut events = Vec::new();

        flip_permanent(&mut state, id, &mut events).unwrap();
        assert!(state.objects[&id].flipped, "reach guard: it really flipped");

        // Die, then get reanimated onto the battlefield.
        crate::game::zones::move_to_zone(&mut state, id, Zone::Graveyard, &mut events);
        crate::game::zones::move_to_zone(&mut state, id, Zone::Battlefield, &mut events);
        assert!(
            !state.objects[&id].flipped,
            "reach guard: CR 110.5b — the returning permanent is unflipped, so \
             the `flipped` arm of the transform guard cannot be what blocks it"
        );
        assert_eq!(
            state.objects[&id].zone,
            Zone::Battlefield,
            "reach guard: transform_permanent only acts on battlefield permanents"
        );

        events.clear();
        crate::game::transform::transform_permanent(&mut state, id, &mut events).unwrap();

        let obj = &state.objects[&id];
        assert!(
            !obj.transformed,
            "CR 701.27a: a flip card is single-faced and cannot transform"
        );
        assert_eq!(obj.name, "Bushi Tenderfoot");
        assert_eq!(
            obj.mana_cost,
            white_mana_cost(),
            "CR 710.1c: the double-faced applicator must never touch a flip card's mana cost"
        );
        assert_eq!(obj.color, vec![ManaColor::White]);
        assert!(events.is_empty());
        assert!(
            is_flip_permanent(obj),
            "the LayoutKind::Flip tag survives the flip → leave → return round trip"
        );
    }

    /// CR 613.1 + CR 710.1b: the half parked in `back_face` must be the PRINTED
    /// one, not the layer-modified live one. Student of Elements' own trigger
    /// condition is "when this creature has flying", so a granted keyword and a
    /// pumped P/T are exactly the state a flip happens in.
    ///
    /// Discriminating: revert `stash_flip_face` to `snapshot_object_face` (live
    /// fields) and the graveyard card is a 2/2 Bushi Tenderfoot WITH flying —
    /// the printed-P/T and no-flying assertions below both fail.
    #[test]
    fn flip_stashes_printed_characteristics_not_layer_modified_ones() {
        let mut state = GameState::new_two_player(42);
        let id = setup_flip_card(&mut state);
        {
            // Stand in for an active continuous effect (an anthem plus a
            // granted keyword): live fields inflated, `base_*` untouched.
            let obj = state.objects.get_mut(&id).unwrap();
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.keywords.push(Keyword::Flying);
        }
        let mut events = Vec::new();

        flip_permanent(&mut state, id, &mut events).unwrap();
        assert_eq!(
            state.objects[&id].name, "Kenzo the Hardhearted",
            "reach guard: it really flipped"
        );

        crate::game::zones::move_to_zone(&mut state, id, Zone::Graveyard, &mut events);

        let obj = &state.objects[&id];
        assert_eq!(obj.name, "Bushi Tenderfoot");
        assert_eq!(
            (obj.power, obj.toughness),
            (Some(1), Some(1)),
            "CR 613.1: the stash must carry the PRINTED 1/1, not the anthem-inflated 2/2"
        );
        assert_eq!((obj.base_power, obj.base_toughness), (Some(1), Some(1)));
        assert!(
            !crate::game::keywords::has_keyword(obj, &Keyword::Flying),
            "CR 613.1: a granted keyword must not be baked into the normal half"
        );
        assert!(
            !obj.base_keywords.contains(&Keyword::Flying),
            "CR 613.1: a granted keyword must not be baked into the printed baseline"
        );
        assert!(
            crate::game::keywords::has_keyword(obj, &Keyword::Bushido(1)),
            "reach guard: the printed keyword set really was restored"
        );
    }

    /// CR 710.2 + CR 708.9: a flipped permanent turned FACE DOWN (Ixidron,
    /// Cyber Conversion — CR 712.16 does not cover flip cards) and then killed
    /// lands in the graveyard as the NORMAL half, not as the nameless
    /// face-down shell and not as the alternative half.
    ///
    /// Discriminating: let `turn_face_down` overwrite `back_face` with a fresh
    /// base snapshot again and the graveyard card is "Kenzo the Hardhearted"
    /// 3/4; run `revert_flip_on_zone_exit` BEFORE the CR 708.9 restore again and
    /// the graveyard card is the nameless 2/2 shell. Both fail the assertions
    /// below.
    #[test]
    fn a_flipped_permanent_turned_face_down_still_dies_as_the_normal_half() {
        use crate::types::ability::{FaceDownProfile, ResolvedAbility, TargetFilter};

        let mut state = GameState::new_two_player(42);
        let id = setup_flip_card(&mut state);
        let mut events = Vec::new();

        flip_permanent(&mut state, id, &mut events).unwrap();
        assert!(state.objects[&id].flipped, "reach guard: it really flipped");

        let turn_down = ResolvedAbility::new(
            Effect::TurnFaceDown {
                target: TargetFilter::SpecificObject { id },
                profile: Some(FaceDownProfile::vanilla_2_2()),
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );
        crate::game::effects::turn_face_down::resolve(&mut state, &turn_down, &mut events).unwrap();
        assert!(
            state.objects[&id].face_down,
            "reach guard: CR 712.16 must NOT block a flip card, so it really is face down"
        );
        assert!(
            state.objects[&id].flipped,
            "CR 710.4: flipping is one-way — turning face down does not unflip it"
        );

        crate::game::zones::move_to_zone(&mut state, id, Zone::Graveyard, &mut events);

        let obj = &state.objects[&id];
        assert!(
            !obj.face_down,
            "CR 708.9: revealed on leaving the battlefield"
        );
        assert!(!obj.flipped, "CR 110.5: the new object is unflipped");
        assert_eq!(
            obj.name, "Bushi Tenderfoot",
            "CR 710.2: off the battlefield a flip card has only its normal characteristics"
        );
        assert_eq!((obj.power, obj.toughness), (Some(1), Some(1)));
        assert!(!obj.card_types.supertypes.contains(&Supertype::Legendary));
        assert_eq!(obj.mana_cost, white_mana_cost());
    }

    /// CR 707.2 + CR 707.3: status is not copied, and CR 707.3's worked example
    /// is a flip card. A Clone copying a flipped Kenzo the Hardhearted must
    /// become an UNFLIPPED Bushi Tenderfoot 1/1.
    ///
    /// Discriminating: remove the flipped branch from
    /// `printed_cards::intrinsic_copiable_values` and the copy reads the
    /// object's `base_*` — which `apply_flipped_face_to_object` overwrote with
    /// the alternative half — producing a legendary 3/4 "Kenzo the Hardhearted"
    /// and failing every assertion below.
    #[test]
    fn a_copy_of_a_flipped_permanent_takes_the_unflipped_normal_half() {
        use crate::types::ability::{Duration, ResolvedAbility, TargetFilter, TargetRef};

        let mut state = GameState::new_two_player(42);
        let flipped = setup_flip_card(&mut state);
        let clone = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Clone".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();

        flip_permanent(&mut state, flipped, &mut events).unwrap();
        assert_eq!(
            state.objects[&flipped].name, "Kenzo the Hardhearted",
            "reach guard: the copy source really is flipped"
        );

        let become_copy = ResolvedAbility::new(
            Effect::BecomeCopy {
                target: TargetFilter::Any,
                recipient: TargetFilter::SelfRef,
                duration: Some(Duration::Permanent),
                mana_value_limit: None,
                additional_modifications: Vec::new(),
            },
            vec![TargetRef::Object(flipped)],
            clone,
            PlayerId(0),
        );
        crate::game::effects::become_copy::resolve(&mut state, &become_copy, &mut events).unwrap();
        crate::game::layers::flush_layers(&mut state);

        let copy = &state.objects[&clone];
        assert!(
            !copy.flipped,
            "CR 707.2: status is not copied — the copy enters unflipped"
        );
        assert_eq!(
            copy.name, "Bushi Tenderfoot",
            "CR 707.2 + CR 707.3: the copiable values are the NORMAL half"
        );
        assert_eq!((copy.power, copy.toughness), (Some(1), Some(1)));
        assert!(
            !copy.card_types.supertypes.contains(&Supertype::Legendary),
            "CR 710.1b: the Legendary supertype lives only on the alternative half"
        );
        assert!(
            !crate::game::keywords::has_keyword(copy, &Keyword::DoubleStrike),
            "CR 707.2: the alternative half's text box is not a copiable value"
        );
        assert_eq!(
            state.objects[&flipped].name, "Kenzo the Hardhearted",
            "the copy source is untouched"
        );
    }
}
