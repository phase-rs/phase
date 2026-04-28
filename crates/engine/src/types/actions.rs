use serde::{Deserialize, Serialize};

use super::ability::TargetRef;
use super::game_state::{AutoPassRequest, CombatDamageAssignmentMode, ShardChoice};
use super::identifiers::{CardId, ObjectId};
use super::mana::ManaType;
use super::match_config::DeckCardCount;
use super::player::PlayerId;
use crate::game::combat::AttackTarget;

/// CR 701.57a + CR 702.85a: Player decision for any "you may cast that card
/// without paying its mana cost" mid-resolution choice (Discover, Cascade).
/// Bool flags are not composable — this enum can grow new branches (e.g.,
/// "Cast face-down", "Put into hand" already exists for Discover) without
/// changing call sites that already exhaustively match.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CastChoice {
    /// CR 701.57a + CR 702.85a: Cast the offered card without paying its mana
    /// cost. The cast pipeline still enforces target legality, alternative
    /// constraints (e.g., `CascadeResultingMvBelow`), and other CR 601.2
    /// checks.
    Cast,
    /// CR 701.57a + CR 702.85a: Decline the offer. For Discover the card goes
    /// to hand; for Cascade the card joins the misses on the bottom of the
    /// library in a random order.
    Decline,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, strum::IntoStaticStr)]
#[serde(tag = "type", content = "data")]
pub enum GameAction {
    PassPriority,
    PlayLand {
        object_id: ObjectId,
        card_id: CardId,
    },
    CastSpell {
        object_id: ObjectId,
        card_id: CardId,
        targets: Vec<ObjectId>,
    },
    ActivateAbility {
        source_id: ObjectId,
        ability_index: usize,
    },
    DeclareAttackers {
        attacks: Vec<(ObjectId, AttackTarget)>,
    },
    DeclareBlockers {
        assignments: Vec<(ObjectId, ObjectId)>,
    },
    MulliganDecision {
        keep: bool,
    },
    TapLandForMana {
        object_id: ObjectId,
    },
    /// CR 605.3a: Undo a manual mana ability activation — untap source, remove produced mana.
    /// Only valid for lands in `lands_tapped_for_mana` whose mana hasn't been spent.
    UntapLandForMana {
        object_id: ObjectId,
    },
    SelectCards {
        cards: Vec<ObjectId>,
    },
    SelectTargets {
        targets: Vec<TargetRef>,
    },
    ChooseTarget {
        target: Option<TargetRef>,
    },
    ChooseReplacement {
        index: usize,
    },
    CancelCast,
    Equip {
        equipment_id: ObjectId,
        target_id: ObjectId,
    },
    /// CR 702.122a: Crew a Vehicle by tapping creatures with total power >= N.
    /// During Priority: creature_ids is empty (triggers state transition).
    /// During CrewVehicle: creature_ids contains the selected creatures.
    CrewVehicle {
        vehicle_id: ObjectId,
        creature_ids: Vec<ObjectId>,
    },
    /// CR 702.184a: Activate a Spacecraft's station ability.
    /// During Priority: creature_id is None (triggers state transition to
    /// `WaitingFor::StationTarget`). During StationTarget: creature_id is
    /// `Some(id)` — the single creature being tapped to station.
    ActivateStation {
        spacecraft_id: ObjectId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        creature_id: Option<ObjectId>,
    },
    /// CR 702.171a: Saddle a Mount by tapping creatures with total power >= N.
    /// During Priority: creature_ids is empty (triggers state transition to
    /// `WaitingFor::SaddleMount`). During SaddleMount: creature_ids contains
    /// the selected creatures.
    SaddleMount {
        mount_id: ObjectId,
        creature_ids: Vec<ObjectId>,
    },
    Transform {
        object_id: ObjectId,
    },
    PlayFaceDown {
        object_id: ObjectId,
        card_id: CardId,
    },
    TurnFaceUp {
        object_id: ObjectId,
    },
    SubmitSideboard {
        main: Vec<DeckCardCount>,
        sideboard: Vec<DeckCardCount>,
    },
    ChoosePlayDraw {
        play_first: bool,
    },
    ChooseOption {
        choice: String,
    },
    SelectModes {
        indices: Vec<usize>,
    },
    DecideOptionalCost {
        pay: bool,
    },
    /// CR 715.3a: Choose creature face (true) or Adventure half (false).
    ChooseAdventureFace {
        creature: bool,
    },
    /// CR 712.12: Choose front face (false) or back face (true) for MDFC land play.
    ChooseModalFace {
        back_face: bool,
    },
    /// Choose normal cast (false) or Warp cast (true) from hand.
    ChooseWarpCost {
        use_warp: bool,
    },
    /// CR 702.74a: Choose normal cast (false) or Evoke cast (true) from hand.
    /// Evoke cast tags the resolving permanent so the synthesized intervening-if
    /// ETB sacrifice trigger fires.
    ChooseEvokeCost {
        use_evoke: bool,
    },
    /// CR 702.96a: Choose normal cast (false) or Overload cast (true) from hand.
    /// Overload cast substitutes the overload mana cost and transforms every
    /// "target" in the spell's text to "each" (CR 702.96b-c).
    ChooseOverloadCost {
        use_overload: bool,
    },
    /// CR 702.49: Activate a Ninjutsu-family keyword from hand or command zone during combat.
    ActivateNinjutsu {
        ninjutsu_card_id: CardId,
        /// The creature to return — unblocked attacker (Ninjutsu) or tapped creature (WebSlinging).
        creature_to_return: ObjectId,
    },
    /// CR 702.190a: Cast a spell from HAND via the Sneak alternative cost.
    /// Legal only during the declare-blockers step (CR 702.190a). Applies to
    /// any card type (creature, artifact, sorcery, instant, …) — the printed
    /// keyword's cost grants permission regardless of the card's core type.
    ///
    /// `creature_to_return` must be an unblocked attacker controlled by the
    /// casting player; it is returned to its owner's hand as part of paying
    /// the Sneak cost (CR 702.190a).
    ///
    /// CR 702.190b applies only to permanent spells: they enter tapped and
    /// attacking alongside the returned creature. Non-permanent Sneak casts
    /// resolve normally.
    CastSpellAsSneak {
        hand_object: ObjectId,
        card_id: CardId,
        creature_to_return: ObjectId,
    },
    /// CR 601.2b + CR 118.9a: Cast a spell from hand for free via a
    /// `StaticMode::CastFromHandFree` permission source (Zaffai and the
    /// Tempests — "Once during each of your turns, you may cast an instant or
    /// sorcery spell from your hand without paying its mana cost").
    ///
    /// The implicit Omniscience silent-free path uses `GameAction::CastSpell`
    /// with `CastingVariant::Normal` and a `NoCost` short-circuit — this
    /// dedicated action variant is reserved for `OncePerTurn` permissions where
    /// the player's "may cast" choice and the source-slot consumption must be
    /// visible at the action layer.
    CastSpellForFree {
        object_id: ObjectId,
        card_id: CardId,
        source_id: ObjectId,
    },
    /// CR 702.94a + CR 603.11: Accept a pending `WaitingFor::MiracleReveal`
    /// and cast `object_id` from hand for the card's miracle mana cost. Mirror
    /// of `CastSpellAsSneak` / `CastSpellForFree` — dedicated variant because
    /// the cast is opted into from a specialized prompt, not from Priority.
    /// Decline is via the shared `DecideOptionalEffect { accept: false }`.
    CastSpellAsMiracle {
        object_id: ObjectId,
        card_id: CardId,
    },
    /// CR 702.35a: Accept a pending `WaitingFor::MadnessCastOffer` and cast
    /// `object_id` from exile for its madness cost. Decline is via the shared
    /// `DecideOptionalEffect { accept: false }`.
    CastSpellAsMadness {
        object_id: ObjectId,
        card_id: CardId,
    },
    /// CR 609.3: Accept or decline an optional effect ("You may X").
    DecideOptionalEffect {
        accept: bool,
    },
    /// CR 118.12: Pay or decline an "unless pays" cost (e.g., Mana Leak, No More Lies).
    PayUnlessCost {
        pay: bool,
    },
    /// CR 508.1d + CR 508.1h + CR 509.1c + CR 509.1d: Pay or decline the aggregate
    /// combat tax (Ghostly Prison, Propaganda, Sphere of Safety, Windborn Muse).
    /// On accept the engine deducts the locked-in total and completes the paused
    /// attack/block declaration; on decline the engine strips the taxed creatures
    /// from the declaration and completes with the remaining, untaxed subset.
    PayCombatTax {
        accept: bool,
    },
    /// CR 701.54a: Choose a creature to be the ring-bearer.
    ChooseRingBearer {
        target: ObjectId,
    },
    /// CR 701.49a: Choose which dungeon to venture into.
    ChooseDungeon {
        dungeon: crate::game::dungeon::DungeonId,
    },
    /// CR 309.5a: Choose which room to advance to at a branch point.
    ChooseDungeonRoom {
        room_index: u8,
    },
    /// CR 702.51a: Tap creature/artifact for convoke or waterbend mana.
    /// CR 302.6: Summoning sickness does not apply (convoke doesn't use the tap ability mechanism).
    TapForConvoke {
        object_id: ObjectId,
        mana_type: super::mana::ManaType,
    },
    /// CR 702.180a/b: Harmonize — optionally tap a creature to reduce casting cost by its power.
    /// None = skip (decline the cost reduction).
    HarmonizeTap {
        creature_id: Option<ObjectId>,
    },
    /// CR 702.139a: Declare a companion during pre-game reveal (or decline).
    DeclareCompanion {
        /// Index into the eligible_companions list, or None to decline.
        card_index: Option<usize>,
    },
    /// CR 702.139a: Pay {3} to put companion into hand (special action, see rule 116.2g).
    CompanionToHand,
    /// CR 701.57a: Choose to cast discovered card or put it to hand.
    DiscoverChoice {
        choice: CastChoice,
    },
    /// CR 702.85a: Choose to cast the cascaded card without paying its mana cost.
    CascadeChoice {
        choice: CastChoice,
    },
    /// CR 401.4: Choose top or bottom of library.
    ChooseTopOrBottom {
        top: bool,
    },
    /// CR 704.5j: Choose which legendary permanent to keep.
    ChooseLegend {
        keep: ObjectId,
    },
    /// CR 310.10 + CR 704.5w + CR 704.5x: Choose which player becomes the
    /// battle's new protector when the SBA pauses with a `BattleProtectorChoice`.
    ChooseBattleProtector {
        protector: PlayerId,
    },
    /// Set auto-pass mode for the acting player (CR 117.4).
    SetAutoPass {
        mode: AutoPassRequest,
    },
    /// Cancel any active auto-pass for the acting player.
    CancelAutoPass,
    /// Replace the acting player's phase-stop preference list. Phase stops
    /// interrupt an `UntilEndOfTurn` auto-pass session and prevent the engine
    /// from auto-submitting empty blocker declarations during the named phases.
    /// Legal in any WaitingFor state — pure preference propagation.
    SetPhaseStops {
        stops: Vec<super::phase::Phase>,
    },
    /// CR 510.1c/d: Assign damage from an attacker to its blockers (and optionally
    /// the defending player/PW with trample, plus PW controller with trample-over-PW).
    AssignCombatDamage {
        #[serde(default)]
        mode: CombatDamageAssignmentMode,
        assignments: Vec<(ObjectId, u32)>,
        trample_damage: u32,
        /// CR 702.19c: Damage to PW controller when trample-over-PW spills past loyalty.
        #[serde(default)]
        controller_damage: u32,
    },
    /// CR 601.2d: Distribute N among targets at casting time.
    DistributeAmong {
        distribution: Vec<(TargetRef, u32)>,
    },
    /// CR 107.1c + CR 107.14: Submit the chosen amount for a
    /// `WaitingFor::PayAmountChoice` prompt ("pay any amount of {E}" and
    /// similar resource-choice patterns).
    SubmitPayAmount {
        amount: u32,
    },
    /// CR 115.7: Choose new target(s) for a spell or ability on the stack.
    RetargetSpell {
        new_targets: Vec<TargetRef>,
    },
    /// CR 701.48a: Learn — choose to rummage (discard a card, draw a card) or skip.
    LearnDecision {
        choice: LearnOption,
    },
    /// CR 101.4 + CR 701.21a: Select one permanent per type category to keep;
    /// the rest will be sacrificed. Each position corresponds to a category in
    /// `WaitingFor::CategoryChoice::categories`. `None` = no permanent of that type.
    SelectCategoryPermanents {
        choices: Vec<Option<ObjectId>>,
    },
    /// CR 107.1b + CR 601.2f: Choose the value of X for a spell or activated
    /// ability whose cost contains X. Chosen as part of determining total cost,
    /// before mana is paid.
    ChooseX {
        value: u32,
    },
    /// CR 107.4f + CR 601.2f: Caster submits their per-shard payment choice
    /// (mana or 2 life) for each Phyrexian shard in the spell's cost. The length
    /// of `choices` MUST equal `WaitingFor::PhyrexianPayment.shards.len()`.
    SubmitPhyrexianChoices {
        choices: Vec<ShardChoice>,
    },
    /// CR 605.3b: Answer the `WaitingFor::ChooseManaColor` prompt.
    /// Shape mirrors the prompt variant (`SingleColor` or `Combination`).
    ChooseManaColor {
        choice: super::game_state::ManaChoice,
    },
    /// CR 605.3a + CR 601.2h + CR 107.4e: Answer the
    /// `WaitingFor::PayManaAbilityMana` prompt by picking one of the legal
    /// per-hybrid-shard color vectors. `payment.len()` equals the number of
    /// hybrid shards in the ability's `Mana` sub-cost. The engine verifies
    /// the vector is present in the prompt's `options` before debiting.
    PayManaAbilityMana {
        payment: Vec<ManaType>,
    },
    /// CR 702.xxx: Prepare (Strixhaven) — at priority, cast a token copy of a
    /// prepared creature's face-`b` prepare-spell. The source creature must
    /// have `prepared.is_some()` and be controlled by the acting player.
    /// On cast, the source becomes unprepared (single-authority via
    /// `effects::prepare::unprepare_object`). Assign when WotC publishes SOS
    /// CR update.
    CastPreparedCopy {
        source: ObjectId,
    },
    /// CR 702.xxx: Paradigm (Strixhaven) — accept the turn-based offer during
    /// `WaitingFor::ParadigmCastOffer`, casting a token copy of the exiled
    /// source spell without paying its mana cost. The exiled source stays in
    /// exile. Assign when WotC publishes SOS CR update.
    CastParadigmCopy {
        source: ObjectId,
    },
    /// CR 702.xxx: Paradigm (Strixhaven) — decline the turn-based offer during
    /// `WaitingFor::ParadigmCastOffer`. The exiled source stays in exile and
    /// may be offered again next turn. Assign when WotC publishes SOS CR
    /// update.
    PassParadigmOffer,
    /// CR 104.3a: A player may concede the game at any time. That player leaves the game.
    /// CR 800.4a: When a player leaves a multiplayer game, all objects owned by that player
    /// leave the game and all spells/abilities controlled by that player cease to exist.
    ///
    /// Concede is always legal regardless of priority or `WaitingFor` state — the action
    /// handler bypasses the normal `(WaitingFor, GameAction)` match dispatch and delegates
    /// directly to `eliminate_player`. It is intentionally NOT included in
    /// `legal_actions()` enumeration; callers (UI, network layer) surface it directly.
    Concede {
        player_id: PlayerId,
    },
}

/// CR 701.48a: Learn choice — rummage a specific card, or skip entirely.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum LearnOption {
    /// Discard the specified card, then draw one.
    Rummage { card_id: ObjectId },
    /// Decline to learn (skip).
    Skip,
}

impl GameAction {
    /// Returns the enum variant name as a static string (e.g., `"CastSpell"`, `"PassPriority"`).
    /// Useful for structured logging without the full `Debug` representation.
    pub fn variant_name(&self) -> &'static str {
        self.into()
    }

    /// CR 605.3a: Whether this action is a mana ability activation.
    ///
    /// Mana abilities are excluded from the flat `legal_actions()` result
    /// because they do not represent meaningful priority decisions. They are
    /// still exposed through the engine-authored per-object action grouping so
    /// frontends can render mana affordances without inferring them locally.
    pub fn is_mana_ability(&self) -> bool {
        matches!(
            self,
            GameAction::TapLandForMana { .. } | GameAction::UntapLandForMana { .. }
        )
    }

    /// Engine-side authoritative mapping from action → permanent it acts on.
    ///
    /// Used by `legal_actions_with_costs` to group `legal_actions` by source
    /// permanent so the frontend can look up "what can I do with this card?"
    /// via a single map lookup instead of introspecting `GameAction` variants
    /// (which would push engine-owned structural knowledge into the client).
    ///
    /// Returns `Some(id)` for actions that act on a single permanent or
    /// hand-zone card object; `None` for global actions (`PassPriority`,
    /// `MulliganDecision`, etc.) and for multi-target actions whose "source"
    /// is ambiguous (`DeclareAttackers`, `AssignCombatDamage`, etc.).
    ///
    /// EXHAUSTIVE: every variant must be classified. Adding a new variant
    /// without updating this method is a compile-time error.
    pub fn source_object(&self) -> Option<ObjectId> {
        match self {
            GameAction::PlayLand { object_id, .. } => Some(*object_id),
            GameAction::CastSpell { object_id, .. } => Some(*object_id),
            GameAction::CastSpellAsSneak { hand_object, .. } => Some(*hand_object),
            GameAction::CastSpellForFree { object_id, .. } => Some(*object_id),
            GameAction::CastSpellAsMiracle { object_id, .. } => Some(*object_id),
            GameAction::CastSpellAsMadness { object_id, .. } => Some(*object_id),
            GameAction::ActivateAbility { source_id, .. } => Some(*source_id),
            GameAction::TapLandForMana { object_id } => Some(*object_id),
            GameAction::UntapLandForMana { object_id } => Some(*object_id),
            GameAction::Equip { equipment_id, .. } => Some(*equipment_id),
            GameAction::CrewVehicle { vehicle_id, .. } => Some(*vehicle_id),
            GameAction::ActivateStation { spacecraft_id, .. } => Some(*spacecraft_id),
            GameAction::SaddleMount { mount_id, .. } => Some(*mount_id),
            GameAction::Transform { object_id } => Some(*object_id),
            GameAction::PlayFaceDown { object_id, .. } => Some(*object_id),
            GameAction::TurnFaceUp { object_id } => Some(*object_id),
            GameAction::ChooseRingBearer { target } => Some(*target),
            GameAction::TapForConvoke { object_id, .. } => Some(*object_id),
            GameAction::ChooseLegend { keep } => Some(*keep),
            GameAction::CastPreparedCopy { source } => Some(*source),
            GameAction::CastParadigmCopy { source } => Some(*source),
            // Actions with no per-permanent anchor.
            GameAction::PassPriority
            | GameAction::DeclareAttackers { .. }
            | GameAction::DeclareBlockers { .. }
            | GameAction::MulliganDecision { .. }
            | GameAction::SelectCards { .. }
            | GameAction::SelectTargets { .. }
            | GameAction::ChooseTarget { .. }
            | GameAction::ChooseReplacement { .. }
            | GameAction::CancelCast
            | GameAction::SubmitSideboard { .. }
            | GameAction::ChoosePlayDraw { .. }
            | GameAction::ChooseOption { .. }
            | GameAction::SelectModes { .. }
            | GameAction::DecideOptionalCost { .. }
            | GameAction::ChooseAdventureFace { .. }
            | GameAction::ChooseModalFace { .. }
            | GameAction::ChooseWarpCost { .. }
            | GameAction::ChooseEvokeCost { .. }
            | GameAction::ChooseOverloadCost { .. }
            | GameAction::ActivateNinjutsu { .. }
            | GameAction::DecideOptionalEffect { .. }
            | GameAction::PayUnlessCost { .. }
            | GameAction::PayCombatTax { .. }
            | GameAction::ChooseDungeon { .. }
            | GameAction::ChooseDungeonRoom { .. }
            | GameAction::HarmonizeTap { .. }
            | GameAction::DeclareCompanion { .. }
            | GameAction::CompanionToHand
            | GameAction::DiscoverChoice { .. }
            | GameAction::CascadeChoice { .. }
            | GameAction::ChooseTopOrBottom { .. }
            | GameAction::ChooseBattleProtector { .. }
            | GameAction::SetAutoPass { .. }
            | GameAction::CancelAutoPass
            | GameAction::SetPhaseStops { .. }
            | GameAction::AssignCombatDamage { .. }
            | GameAction::DistributeAmong { .. }
            | GameAction::SubmitPayAmount { .. }
            | GameAction::RetargetSpell { .. }
            | GameAction::LearnDecision { .. }
            | GameAction::SelectCategoryPermanents { .. }
            | GameAction::ChooseX { .. }
            | GameAction::SubmitPhyrexianChoices { .. }
            | GameAction::ChooseManaColor { .. }
            | GameAction::PayManaAbilityMana { .. }
            | GameAction::PassParadigmOffer
            | GameAction::Concede { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pass_priority_serializes_as_tagged_union() {
        let action = GameAction::PassPriority;
        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(json["type"], "PassPriority");
        assert!(json.get("data").is_none());
    }

    #[test]
    fn play_land_serializes_with_data() {
        let action = GameAction::PlayLand {
            object_id: ObjectId(99),
            card_id: CardId(42),
        };
        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(json["type"], "PlayLand");
        assert_eq!(json["data"]["card_id"], 42);
        assert_eq!(json["data"]["object_id"], 99);
    }

    #[test]
    fn cast_spell_serializes_with_targets() {
        let action = GameAction::CastSpell {
            object_id: ObjectId(5),
            card_id: CardId(1),
            targets: vec![ObjectId(10), ObjectId(20)],
        };
        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(json["type"], "CastSpell");
        assert_eq!(json["data"]["object_id"], 5);
        assert_eq!(json["data"]["targets"], serde_json::json!([10, 20]));
    }

    #[test]
    fn mulligan_decision_roundtrips() {
        let action = GameAction::MulliganDecision { keep: true };
        let serialized = serde_json::to_string(&action).unwrap();
        let deserialized: GameAction = serde_json::from_str(&serialized).unwrap();
        assert_eq!(action, deserialized);
    }

    #[test]
    fn deserialize_from_tagged_json() {
        let json = r#"{"type":"PassPriority"}"#;
        let action: GameAction = serde_json::from_str(json).unwrap();
        assert_eq!(action, GameAction::PassPriority);
    }

    #[test]
    fn declare_attackers_with_attack_targets_roundtrips() {
        use crate::game::combat::AttackTarget;
        use crate::types::player::PlayerId;

        let action = GameAction::DeclareAttackers {
            attacks: vec![
                (ObjectId(1), AttackTarget::Player(PlayerId(1))),
                (ObjectId(2), AttackTarget::Planeswalker(ObjectId(99))),
            ],
        };
        let serialized = serde_json::to_string(&action).unwrap();
        let deserialized: GameAction = serde_json::from_str(&serialized).unwrap();
        assert_eq!(action, deserialized);
    }

    #[test]
    fn attack_target_serializes_as_tagged_union() {
        use crate::game::combat::AttackTarget;
        use crate::types::player::PlayerId;

        let target = AttackTarget::Player(PlayerId(1));
        let json = serde_json::to_value(target).unwrap();
        assert_eq!(json["type"], "Player");
        assert_eq!(json["data"], 1);

        let target = AttackTarget::Planeswalker(ObjectId(42));
        let json = serde_json::to_value(target).unwrap();
        assert_eq!(json["type"], "Planeswalker");
        assert_eq!(json["data"], 42);
    }

    #[test]
    fn declare_attackers_empty_attacks_roundtrips() {
        let action = GameAction::DeclareAttackers {
            attacks: Vec::new(),
        };
        let serialized = serde_json::to_string(&action).unwrap();
        let deserialized: GameAction = serde_json::from_str(&serialized).unwrap();
        assert_eq!(action, deserialized);
    }

    #[test]
    fn source_object_for_every_permanent_action_variant() {
        let oid = ObjectId(7);
        let cid = CardId(1);
        let cases: &[(GameAction, Option<ObjectId>)] = &[
            (
                GameAction::PlayLand {
                    object_id: oid,
                    card_id: cid,
                },
                Some(oid),
            ),
            (
                GameAction::CastSpell {
                    object_id: oid,
                    card_id: cid,
                    targets: vec![],
                },
                Some(oid),
            ),
            (
                GameAction::ActivateAbility {
                    source_id: oid,
                    ability_index: 0,
                },
                Some(oid),
            ),
            (GameAction::TapLandForMana { object_id: oid }, Some(oid)),
            (GameAction::UntapLandForMana { object_id: oid }, Some(oid)),
            (
                GameAction::Equip {
                    equipment_id: oid,
                    target_id: ObjectId(99),
                },
                Some(oid),
            ),
            (
                GameAction::CrewVehicle {
                    vehicle_id: oid,
                    creature_ids: vec![],
                },
                Some(oid),
            ),
            (
                GameAction::ActivateStation {
                    spacecraft_id: oid,
                    creature_id: None,
                },
                Some(oid),
            ),
            (
                GameAction::SaddleMount {
                    mount_id: oid,
                    creature_ids: vec![],
                },
                Some(oid),
            ),
            (GameAction::Transform { object_id: oid }, Some(oid)),
            (
                GameAction::PlayFaceDown {
                    object_id: oid,
                    card_id: cid,
                },
                Some(oid),
            ),
            (GameAction::TurnFaceUp { object_id: oid }, Some(oid)),
            (
                GameAction::TapForConvoke {
                    object_id: oid,
                    mana_type: super::super::mana::ManaType::White,
                },
                Some(oid),
            ),
            (GameAction::ChooseLegend { keep: oid }, Some(oid)),
            // Non-permanent actions return None.
            (GameAction::PassPriority, None),
            (GameAction::MulliganDecision { keep: true }, None),
            (GameAction::CancelCast, None),
            (GameAction::CompanionToHand, None),
            (GameAction::CancelAutoPass, None),
        ];
        for (action, expected) in cases {
            assert_eq!(
                action.source_object(),
                *expected,
                "source_object mismatch for {}",
                action.variant_name()
            );
        }
    }
}
