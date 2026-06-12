use serde::{Deserialize, Serialize};

use super::ability::{LibraryPosition, TargetRef};
use super::counter::CounterType;
use super::game_state::{
    AutoMayChoice, AutoPassRequest, CastPaymentMode, CombatDamageAssignmentMode, CounterCostChoice,
    CounterMoveChoice, ShardChoice,
};
use super::identifiers::{CardId, ObjectId};
use super::keywords::Keyword;
use super::mana::ManaType;
use super::match_config::DeckCardCount;
use super::phase::Phase;
use super::player::{PlayerCounterKind, PlayerId};
use super::zones::Zone;
use crate::game::combat::AttackTarget;
use crate::game::game_object::AttachTarget;

/// CR 701.57a + CR 702.85a: Player decision for any "you may cast that card
/// without paying its mana cost" mid-resolution choice (Discover, Cascade).
/// Bool flags are not composable — this enum can grow new branches (e.g.,
/// "Cast face-down", "Put into hand" already exists for Discover) without
/// changing call sites that already exhaustively match.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CastChoice {
    /// CR 701.57a + CR 702.85a: Cast the offered card without paying its mana
    /// cost. The cast pipeline still enforces target legality, the
    /// cast-during-resolution resulting-MV constraint (`ManaValue` carried on
    /// the `ExileWithAltCost` permission with `resolution_cleanup`), and other
    /// CR 601.2 checks.
    Cast,
    /// CR 701.57a + CR 702.85a: Decline the offer. For Discover the card goes
    /// to hand; for Cascade the card joins the misses on the bottom of the
    /// library in a random order.
    Decline,
}

/// CR 103.5 + Serum Powder Oracle text: Player decision at a `MulliganDecision`
/// prompt. The three branches correspond to the three actions a player can take
/// while still pending in the mulligan-decision phase:
/// - `Keep` — lock in the current opening hand (CR 103.5).
/// - `Mulligan` — shuffle the hand back, redraw the starting hand size, and
///   remain pending (CR 103.5).
/// - `UseSerumPowder` — exile every card in hand and redraw the same number,
///   without taking a mulligan and without incrementing the mulligan counter.
///   Only available when `object_id` references a card named "Serum Powder" in
///   the actor's hand (CR 103.5b and Serum Powder Oracle text). The player
///   remains pending and may keep, mulligan, or use another Serum Powder next.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum MulliganChoice {
    Keep,
    Mulligan,
    /// CR 103.5b: A "you could mulligan" action. `object_id` is the Serum
    /// Powder being used; it goes to exile with the rest of the hand.
    UseSerumPowder {
        object_id: ObjectId,
    },
}

/// CR 118.9: Player decision at a `WaitingFor::AlternativeCastChoice` prompt —
/// pay the spell's printed mana cost or the keyword-granted alternative cost.
/// Typed enum (not `bool`, per the no-bool-flags rule) so the action serializes
/// self-describingly and survives future expansion (e.g., a third "Decline"
/// path) without breaking exhaustive matches. The specific keyword whose
/// alternative cost is in play lives on the `WaitingFor::AlternativeCastChoice`
/// state, not on this action — the decision is structurally identical across
/// keywords; only post-payment semantics diverge (per CR 702.74a Evoke,
/// CR 702.96a Overload, CR 702.103a Bestow, and the custom Warp keyword).
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AlternativeCastDecision {
    /// Pay the spell's printed mana cost. Resolution proceeds normally.
    Normal,
    /// Pay the keyword-granted alternative cost. Resolution applies the
    /// keyword's post-payment effects (Overload's target→each text change per
    /// CR 702.96b-c, Evoke's ETB-sacrifice trigger per CR 702.74b, Bestow's
    /// Aura transformation per CR 702.103b, Warp's exile-at-end-step rider).
    Alternative,
}

/// CR 118.12a: Player decision at an `UnlessPaymentChooseCost` prompt — the
/// disjunctive ("unless they X or Y") unless-cost choice. `Decline` falls
/// through to the effect happening (mirrors `PayUnlessCost { pay: false }`);
/// `Pay { index }` selects the sub-cost by its position in
/// `WaitingFor::UnlessPaymentChooseCost::costs` and routes back into the
/// standard single-cost `handle_unless_payment` path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum UnlessCostBranch {
    Decline,
    Pay { index: usize },
}

/// CR 400.11 + CR 406.3: One discriminated selection committed for an
/// outside-game choice. The two source pools (sideboard and face-up exile) are
/// expressed as parallel variants so the action wire format is uniform.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum OutsideGameSelection {
    /// CR 400.11a: A copy from the player's sideboard, identified by its slot.
    Sideboard { sideboard_index: usize },
    /// CR 406.3: A face-up exile object the player owns.
    FaceUpExile { object_id: ObjectId },
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
        #[serde(default)]
        payment_mode: CastPaymentMode,
    },
    /// CR 702.143a-b: Foretell special action — during your turn while you
    /// have priority, pay {2} and exile this card from your hand. The card
    /// becomes foretold in exile and may be cast on a later turn for its
    /// foretell cost.
    Foretell {
        object_id: ObjectId,
        card_id: CardId,
    },
    ActivateAbility {
        source_id: ObjectId,
        ability_index: usize,
    },
    DeclareAttackers {
        attacks: Vec<(ObjectId, AttackTarget)>,
        /// CR 702.22c: As a player declares attackers, they may declare that one
        /// or more attacking creatures with banding (or one with banding and any
        /// number of others) form an attacking band. Each inner `Vec` is one band
        /// of attacker `ObjectId`s. Empty (the default) means no bands declared.
        #[serde(default)]
        bands: Vec<Vec<ObjectId>>,
    },
    DeclareBlockers {
        assignments: Vec<(ObjectId, ObjectId)>,
    },
    /// CR 502.3: Choose whether a permanent with "You may choose not to untap"
    /// untaps during the active player's untap step.
    ChooseUntap {
        object_id: ObjectId,
        untap: bool,
    },
    /// CR 508.1g + CR 701.43d: The active player's decision whether to pay the
    /// optional "exert this creature as it attacks" cost for the attacker named
    /// in the pending `WaitingFor::ExertChoice`. `exert: false` declines.
    ChooseExert {
        exert: bool,
    },
    /// CR 701.30b: The clashing player's choice of which opponent to clash with,
    /// answering a pending `WaitingFor::ClashChooseOpponent`. `opponent` must be
    /// one of that prompt's `candidates`.
    ChooseClashOpponent {
        opponent: PlayerId,
    },
    /// CR 702.132a: Assist — the caster's answer to `WaitingFor::AssistChoosePlayer`.
    /// `Some(p)` chooses player `p` (one of the prompt's `candidates`) to help pay
    /// the generic mana; `None` declines and proceeds to normal payment.
    ChooseAssistPlayer {
        player: Option<PlayerId>,
    },
    /// CR 702.132a: Assist — the chosen player's answer to `WaitingFor::AssistPayment`.
    /// `generic` is how much of the spell's generic mana they pay (0 = nothing),
    /// capped at the prompt's `max_generic`.
    CommitAssistPayment {
        generic: u32,
    },
    /// CR 103.5 + 103.5b: A player's decision at a `WaitingFor::MulliganDecision`
    /// prompt. See [`MulliganChoice`] for the three branches.
    MulliganDecision {
        choice: MulliganChoice,
    },
    /// CR 402.3: A player may arrange their hand in any convenient fashion at any time.
    /// Hand order has no game-rules significance for mainline gameplay, so
    /// this action is purely a display-preference update on the actor's own
    /// hand. `order` MUST be a permutation of the actor's current hand —
    /// same multiset of ObjectIds, no additions or removals. Like
    /// `SetPhaseStops` and `CancelAutoPass`, it bypasses the WaitingFor
    /// dispatch and the priority/turn checks: a player can rearrange their
    /// hand whenever they want, including while the opponent holds priority
    /// or while another interactive choice is open.
    ReorderHand {
        order: Vec<ObjectId>,
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
    /// CR 118.3 + CR 122.1: Choose exactly how many counters each selected
    /// object contributes to a remove-counter cost that says "from among".
    ChooseRemoveCounterCostDistribution {
        distribution: Vec<CounterCostChoice>,
    },
    /// CR 705.1: Krark's Thumb keep-choice — indices into `results` the player
    /// keeps (ignoring the rest, CR 614.1a). Length must equal `keep_count`.
    SelectCoinFlips {
        keep_indices: Vec<usize>,
    },
    /// CR 400.11 + CR 406.3: Player commits one or more selections from the
    /// offered outside-game pool. Each selection is a discriminated source —
    /// a sideboard slot (wishboard) or a face-up exile object (Karn / Coax).
    ChooseOutsideGameCards {
        selections: Vec<OutsideGameSelection>,
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
    /// CR 603.3b: Player submits the chosen order for their pending triggers.
    /// `order` is a permutation of indices into the `OrderTriggers.triggers`
    /// vec the player was prompted with; index 0 = first placed (bottom of
    /// that controller's group on the stack — resolves last, CR 405.3 LIFO).
    OrderTriggers {
        order: Vec<usize>,
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
    /// Alchemy spellbook draft: the player's chosen card name in response to
    /// `WaitingFor::SpellbookDraft`. The named card is conjured into the
    /// pending destination.
    SubmitSpellbookDraft {
        card: String,
    },
    /// CR 700.3 + CR 700.3a: Submit one pile (pile A) of a
    /// `SeparateIntoPiles` partition. Pile B is derived by the engine as
    /// `eligible \ pile_a` — CR 700.3a requires the partition to be
    /// exhaustive and disjoint, and CR 700.3d permits either pile to be
    /// empty. Plain `Vec` payload (transport-only); the engine ledger uses
    /// `im::Vector` per the persistent-container convention.
    SubmitPilePartition {
        pile_a: Vec<ObjectId>,
    },
    /// CR 700.3: Chooser selects one of the two piles produced by a
    /// `SeparateIntoPiles` partition. Typed [`PileSide`] rather than `bool`
    /// so the action shape is self-documenting and the parser/AI cannot
    /// accidentally swap pile semantics.
    ChoosePile {
        pile: crate::types::game_state::PileSide,
    },
    /// CR 701.55a: Choose one branch of a resolution-time "A or B" instruction.
    ChooseBranch {
        index: usize,
    },
    /// CR 609.7a: Choose a source of damage for a prevention or replacement effect.
    ChooseDamageSource {
        source: ObjectId,
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
    /// CR 118.9: Resolve a `WaitingFor::AlternativeCastChoice` by selecting
    /// the printed cost or the keyword-granted alternative cost. The specific
    /// keyword (Warp, Evoke per CR 702.74a, Overload per CR 702.96a, Bestow
    /// per CR 702.103a) lives on the waiting state — the action is uniform
    /// because the player's *decision* (which cost) is uniform; only the
    /// keyword's post-payment semantics diverge and are dispatched in the
    /// engine handler.
    ChooseAlternativeCast {
        choice: AlternativeCastDecision,
    },
    /// CR 601.2b: Resolve a `WaitingFor::CastingVariantChoice` by selecting
    /// one of the engine-authored options by index.
    ChooseCastingVariant {
        index: usize,
    },
    /// CR 707.10c: Resolve a `WaitingFor::CopyRetarget` by leaving every
    /// remaining slot's target unchanged. Single action so the UI can offer
    /// "Keep Current Targets" without N round-trips through `ChooseTarget`.
    KeepAllCopyTargets,
    /// CR 110.4: Choose which permanent type slot to consume for a multi-type
    /// graveyard cast/play via OncePerTurnPerPermanentType (Muldrotha).
    ChoosePermanentTypeSlot {
        slot: super::card_type::CoreType,
    },
    /// CR 702.49: Activate a Ninjutsu-family keyword from hand or command zone during combat.
    ActivateNinjutsu {
        /// The card object with Ninjutsu in hand or command zone.
        ninjutsu_object_id: ObjectId,
        /// The unblocked attacker to return.
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
        #[serde(default)]
        payment_mode: CastPaymentMode,
    },
    /// CR 702.188a: Cast a spell from HAND via the Web-slinging alternative cost.
    /// The returned creature must be a tapped creature controlled by the caster.
    CastSpellAsWebSlinging {
        hand_object: ObjectId,
        card_id: CardId,
        creature_to_return: ObjectId,
        #[serde(default)]
        payment_mode: CastPaymentMode,
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
        #[serde(default)]
        payment_mode: CastPaymentMode,
    },
    /// CR 702.94a + CR 603.11: Accept a pending `WaitingFor::MiracleReveal`
    /// and cast `object_id` from hand for the card's miracle mana cost. Mirror
    /// of `CastSpellAsSneak` / `CastSpellForFree` — dedicated variant because
    /// the cast is opted into from a specialized prompt, not from Priority.
    /// Decline is via the shared `DecideOptionalEffect { accept: false }`.
    CastSpellAsMiracle {
        object_id: ObjectId,
        card_id: CardId,
        #[serde(default)]
        payment_mode: CastPaymentMode,
    },
    /// CR 702.35a: Accept a pending `WaitingFor::CastOffer` (Madness) and cast
    /// `object_id` from exile for its madness cost. Decline is via the shared
    /// `DecideOptionalEffect { accept: false }`.
    CastSpellAsMadness {
        object_id: ObjectId,
        card_id: CardId,
        #[serde(default)]
        payment_mode: CastPaymentMode,
    },
    /// CR 609.3: Accept or decline an optional effect ("You may X").
    DecideOptionalEffect {
        accept: bool,
    },
    /// CR 702.47a–e: Respond to a `WaitingFor::SpliceOffer`. `Some(card)` splices
    /// that card from hand onto the spell being cast (re-presenting the offer for
    /// any remaining eligible cards, CR 702.47e); `None` declines/finishes
    /// splicing and proceeds to target selection.
    RespondToSpliceOffer {
        card: Option<ObjectId>,
    },
    DecideOptionalEffectAndRemember {
        choice: AutoMayChoice,
    },
    /// CR 118.12: Pay or decline an "unless pays" cost (e.g., Mana Leak, No More Lies).
    PayUnlessCost {
        pay: bool,
    },
    /// CR 118.12a: Choose **which** sub-cost branch to pay from a disjunctive
    /// unless-cost ("unless they X or Y"). The `UnlessCostBranch` discriminant
    /// is `Decline` (fall through to the effect) or `Pay { index }` (re-enter
    /// the standard single-cost payment path with the chosen sub-cost).
    /// Drives Tergrid's Lantern's "sacrifice ... or discard ..." disjunction.
    ChooseUnlessCostBranch {
        choice: UnlessCostBranch,
    },
    /// CR 118.12a: Choose which branch of a disjunctive activation cost to pay.
    ChooseActivationCostBranch {
        index: usize,
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
    /// CR 702.95a + CR 608.2d: Choose a Soulbond partner while the PairWith
    /// effect is resolving. This is not targeting.
    ChoosePair {
        partner: Option<ObjectId>,
    },
    /// CR 701.49a: Choose which dungeon to venture into.
    ChooseDungeon {
        dungeon: crate::game::dungeon::DungeonId,
    },
    /// CR 309.5a: Choose which room to advance to at a branch point.
    ChooseDungeonRoom {
        room_index: u8,
    },
    /// CR 709.5e: Special action to pay a locked Room door's unlock cost.
    UnlockRoomDoor {
        object_id: ObjectId,
        door: crate::game::game_object::RoomDoor,
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
    /// CR 702.60a: Choose to cast a revealed same-named ripple card for free.
    RippleChoice {
        choice: CastChoice,
    },
    /// CR 608.2g + CR 601.2: Pick one candidate to cast for free from an open
    /// `WaitingFor::CastOffer { FreeCastWindow }` (Invoke Calamity), or `None`
    /// to finish the window without casting (further) spells. Distinct from the
    /// binary `CastChoice` used by Cascade/Discover/Ripple because the player
    /// chooses *which* of several offered cards to cast, not merely whether to
    /// cast a single pre-selected one.
    FreeCastWindowChoice {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selection: Option<crate::types::identifiers::ObjectId>,
    },
    /// CR 401.4: Choose top or bottom of library.
    ChooseTopOrBottom {
        top: bool,
    },
    /// CR 702.140c + CR 730.2a: As a mutating creature spell resolves with a
    /// legal target, the spell's controller chooses whether the spell is placed
    /// on top of or under the target creature. Resolved by
    /// `merge::handle_mutate_merge_choice`.
    ChooseMutateMergeSide {
        side: crate::game::merge::MergeSide,
    },
    /// CR 702.99a: As a Cipher spell resolves, the controller chooses a creature
    /// to encode the card on (`Some`) or declines (`None`, card → graveyard).
    /// Resolved by `cipher::handle_encode_choice`.
    CipherEncode {
        #[serde(default)]
        creature: Option<ObjectId>,
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
    /// CR 510.1d + CR 702.22k: Assign a blocking creature's combat damage,
    /// divided as the active player chooses, among the creatures it is blocking.
    /// Answers a `WaitingFor::AssignBlockerDamage` prompt. Each `(ObjectId, u32)`
    /// is `(attacker_being_blocked, damage)`; the amounts must sum to the
    /// blocker's combat power. Unlike `AssignCombatDamage`, there is no lethal,
    /// trample, or planeswalker dimension — a blocker only ever assigns to the
    /// attackers it blocks.
    AssignBlockerDamage {
        assignments: Vec<(ObjectId, u32)>,
    },
    /// CR 601.2d: Distribute N among targets at casting time.
    DistributeAmong {
        distribution: Vec<(TargetRef, u32)>,
    },
    /// CR 122.5 + CR 608.2d: Submit resolution-time counter-move distribution.
    ChooseCounterMoveDistribution {
        selections: Vec<CounterMoveChoice>,
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
    /// `AnyCombination` prompts submit a `Combination` vector with one entry
    /// per produced mana unit.
    ///
    /// CR 605.3a: `count` (default 1) bulk-activates `count - 1` additional
    /// identical, choice-free mana sources (e.g. a player's other Treasures)
    /// with the same color in one round-trip — each is an independent mana
    /// ability that resolves before the next (CR 605.3c). Only honored for a
    /// `SingleColor` prompt answering a `ManaAbility` context; capped by the
    /// engine-computed `PendingManaAbility::batch_siblings`.
    ChooseManaColor {
        choice: super::game_state::ManaChoice,
        #[serde(default = "default_one")]
        count: u32,
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
    /// Digital-only Specialize: pick the color specialization to apply.
    ChooseSpecializeColor {
        color: super::mana::ManaColor,
    },
    /// CR 702.xxx: Paradigm (Strixhaven) — accept the turn-based offer during
    /// `WaitingFor::CastOffer` (Paradigm), casting a token copy of the exiled
    /// source spell without paying its mana cost. The exiled source stays in
    /// exile. Assign when WotC publishes SOS CR update.
    CastParadigmCopy {
        source: ObjectId,
    },
    /// CR 702.xxx: Paradigm (Strixhaven) — decline the turn-based offer during
    /// `WaitingFor::CastOffer` (Paradigm). The exiled source stays in exile and
    /// may be offered again next turn. Assign when WotC publishes SOS CR
    /// update.
    PassParadigmOffer,
    /// Debug/remediation action — bypasses WaitingFor validation (like Concede).
    /// Gated on `GameState::debug_mode`. Rejected in multiplayer at both the
    /// WASM and server-core layers.
    Debug(DebugAction),
    /// Sandbox-only host action: grant a player permission to submit
    /// `GameAction::Debug(_)`. The host's seat (PlayerId(0)) cannot grant
    /// in a non-sandbox game (gated server-side on
    /// `format_config.allow_debug_actions`). Only the host can submit this
    /// (server-side check). Bypasses `WaitingFor` like Concede.
    GrantDebugPermission {
        player_id: PlayerId,
    },
    /// Sandbox-only host action: revoke a player's debug permission. The
    /// host cannot revoke their own permission (server-side check). Only
    /// the host can submit this.
    RevokeDebugPermission {
        player_id: PlayerId,
    },
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

/// Serde default for debug spawn `run_etb` flags: omitting the field means
/// "run the ETB pipeline", preserving the historical always-ETB behavior for
/// any payload that predates the toggle.
fn default_true() -> bool {
    true
}

/// Direct game-state manipulation actions for debugging, testing, and remediation.
/// Bypasses `WaitingFor` validation — fires from any game state without disrupting
/// the current prompt. Gated on `GameState::debug_mode`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum DebugAction {
    // ── Object Zone Manipulation ──────────────────────────────────────────
    /// Move an existing object to a different zone.
    /// When `simulate` is true, runs the full pipeline (triggers placed on stack, SBAs).
    /// When false, raw placement with no triggers or SBAs.
    MoveToZone {
        object_id: ObjectId,
        to_zone: Zone,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        library_position: Option<LibraryPosition>,
        #[serde(default)]
        simulate: bool,
    },
    /// Create a new card object by name. Resolved against CardDatabase at the
    /// WASM layer; the engine returns InvalidAction if this reaches apply().
    ///
    /// `attach_to` is consulted only when `zone == Battlefield` and the card is
    /// an Aura/Equipment-style attachment. When set, the object's `attached_to`
    /// is populated before the ETB pipeline runs, so the SBA pass (CR 704.5n)
    /// sees a legal host instead of an orphan. Ignored for non-Battlefield zones.
    CreateCard {
        card_name: String,
        owner: PlayerId,
        zone: Zone,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attach_to: Option<AttachTarget>,
        /// When `true`, route a `Battlefield` spawn through the real ETB pipeline
        /// (replacements → ETB triggers → SBAs). When `false`, place the card raw
        /// with no entry effects — mirrors `MoveToZone { simulate: false }`. Only
        /// consulted for `zone == Battlefield`; ignored for other destinations.
        #[serde(default = "default_true")]
        run_etb: bool,
    },
    /// Remove an object from the game entirely.
    RemoveObject { object_id: ObjectId },
    /// CR 701.21: Sacrifice a permanent — route through the single sacrifice
    /// authority so the replacement pipeline and dies/leaves-the-battlefield
    /// triggers fire. Distinct from `RemoveObject`, which deletes the object
    /// outright with no triggers. The sacrificing player is the permanent's
    /// controller.
    Sacrifice { object_id: ObjectId },
    /// Draw N cards using the real draw pipeline (CR 121.1).
    /// Routes through replacement effects and emits CardDrawn events.
    DrawCards { player_id: PlayerId, count: u32 },
    /// Mill N cards from library to graveyard.
    Mill { player_id: PlayerId, count: u32 },
    /// CR 701.20a: Reveal the top N card(s) of a player's library using the real
    /// `Effect::RevealTop` resolver — marks them revealed and emits
    /// `CardsRevealed` without moving the cards (CR 701.20b).
    Reveal { player_id: PlayerId, count: u32 },
    /// Shuffle a player's library.
    ShuffleLibrary { player_id: PlayerId },
    /// Start a proliferate choice for a player using the real proliferate
    /// resolver (CR 701.34a).
    Proliferate { player_id: PlayerId },

    // ── Object Property Manipulation ──────────────────────────────────────
    /// Overwrite base power/toughness (layer 7a input). Marks layers dirty.
    SetBasePowerToughness {
        object_id: ObjectId,
        power: Option<i32>,
        toughness: Option<i32>,
    },
    /// Modify counters: positive delta adds, negative removes (clamped at 0).
    /// Bypasses replacement effects.
    ModifyCounters {
        object_id: ObjectId,
        counter_type: CounterType,
        delta: i32,
    },
    /// Tap or untap an object.
    SetTapped { object_id: ObjectId, tapped: bool },
    /// CR 722.3a: Give or remove the "prepared" designation on an object so a
    /// preparation card's prepare spell can be cast for testing. Routes through
    /// the `game::effects::prepare` single authority, so setting `prepared`
    /// no-ops on objects without a prepare-spell face and emits the
    /// `BecamePrepared` / `BecameUnprepared` events.
    SetPrepared { object_id: ObjectId, prepared: bool },
    /// Change an object's controller. Marks layers dirty.
    SetController {
        object_id: ObjectId,
        controller: PlayerId,
    },
    /// Set summoning sickness flag directly.
    SetSummoningSickness { object_id: ObjectId, sick: bool },
    /// Transform a DFC, flip a flip-card, or turn face-down/up.
    SetFaceState {
        object_id: ObjectId,
        face_down: Option<bool>,
        transformed: Option<bool>,
        flipped: Option<bool>,
    },
    /// Attach an object (equipment/aura) to a target permanent or player.
    /// CR 301.5 / CR 303.4f: Equipment hosts must be `Object`; player-attachable
    /// Auras (Curse cycle, Faith's Fetters-class) use `Player`. The handler
    /// dispatches to `attach_to` vs `attach_to_player` accordingly.
    Attach {
        object_id: ObjectId,
        target: AttachTarget,
    },
    /// Detach an object from whatever it's attached to.
    Detach { object_id: ObjectId },
    /// Grant a keyword to an object (added to runtime keywords list).
    GrantKeyword {
        object_id: ObjectId,
        keyword: Keyword,
    },
    /// Remove a keyword from an object's runtime keywords list.
    RemoveKeyword {
        object_id: ObjectId,
        keyword: Keyword,
    },

    // ── Player State Manipulation ─────────────────────────────────────────
    /// Set a player's life total directly.
    SetLife { player_id: PlayerId, life: i32 },
    /// Modify a non-energy player counter. Positive delta adds, negative removes.
    ModifyPlayerCounters {
        player_id: PlayerId,
        counter_kind: PlayerCounterKind,
        delta: i32,
    },
    /// Modify energy counters, which are stored separately from `PlayerCounterKind`.
    ModifyEnergy { player_id: PlayerId, delta: i32 },
    /// Add mana to a player's pool (mixed types in one action).
    AddMana {
        player_id: PlayerId,
        mana: Vec<ManaType>,
    },
    /// Toggle "infinite mana" for a player (debug-only). While `enabled`, the
    /// engine keeps the player's mana pool topped up after every action and
    /// suppresses the end-of-step empty (CR 500.5) for that player, so any cost
    /// is payable. Setting `enabled = false` clears the toggle; the pool then
    /// empties normally on the next step transition. Off by default.
    SetInfiniteMana { player_id: PlayerId, enabled: bool },

    // ── Game Flow ─────────────────────────────────────────────────────────
    /// Advance or rewind to a specific phase/step.
    SetPhase {
        phase: Phase,
        active_player: PlayerId,
    },
    /// Explicitly run state-based actions. Use after a batch of raw mutations.
    RunStateBasedActions,
    /// Create a token on the battlefield, either from a catalog preset or
    /// explicit custom characteristics.
    ///
    /// `enter_with_counters` is plumbed straight through to
    /// `TokenSpec::enter_with_counters` and travels the same replacement
    /// pipeline as engine-driven token creation, so debug spawns of bodies
    /// that need counters to survive (0/0 creature tokens, Hangarback /
    /// Hydra shapes) can produce viable objects without the FE inferring
    /// rules state. See `ProposedEvent::CreateToken` and
    /// `TokenSpec::enter_with_counters` — same semantics, real pipeline.
    /// CR 122.6a (counters placed at ETB), CR 614.1 (replacement window),
    /// CR 704.5f (0-toughness SBA — why this field exists).
    ///
    /// When `run_etb` is `true`, the created token's ETB triggers are placed on
    /// the stack and SBAs run; when `false`, the token is still created (with its
    /// replacement-window counters) but its "when ~ enters" triggers and the SBA
    /// pass are skipped — mirrors `MoveToZone { simulate: false }`.
    CreateToken {
        request: DebugTokenRequest,
        #[serde(default = "default_true")]
        run_etb: bool,
    },
    /// Create a token copy of an existing object using the real copy-token
    /// resolver (CR 707.2).
    CreateTokenCopy {
        source_id: ObjectId,
        owner: PlayerId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum DebugTokenRequest {
    Preset {
        preset_id: String,
        owner: PlayerId,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        enter_with_counters: Vec<(CounterType, u32)>,
    },
    Custom {
        owner: PlayerId,
        characteristics: super::proposed_event::TokenCharacteristics,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        enter_with_counters: Vec<(CounterType, u32)>,
    },
}

impl DebugTokenRequest {
    pub fn owner(&self) -> PlayerId {
        match self {
            Self::Preset { owner, .. } | Self::Custom { owner, .. } => *owner,
        }
    }

    pub fn enter_with_counters(&self) -> &[(CounterType, u32)] {
        match self {
            Self::Preset {
                enter_with_counters,
                ..
            }
            | Self::Custom {
                enter_with_counters,
                ..
            } => enter_with_counters,
        }
    }
}

impl DebugAction {
    /// Human-readable description of this debug action, used by the sandbox
    /// audit log so all players see what an authorized debugger did. Engine
    /// owns the wording so the FE remains a pure display layer.
    pub fn describe(&self, state: &super::game_state::GameState) -> String {
        let obj = |id: ObjectId| -> String {
            state
                .objects
                .get(&id)
                .map(|o| o.name.clone())
                .or_else(|| state.lki_cache.get(&id).map(|l| l.name.clone()))
                .unwrap_or_else(|| format!("#{}", id.0))
        };
        let player_label = |id: PlayerId| -> String {
            state
                .log_player_names
                .get(id.0 as usize)
                .filter(|n| !n.is_empty())
                .cloned()
                .unwrap_or_else(|| format!("Player {}", id.0 + 1))
        };
        match self {
            DebugAction::MoveToZone {
                object_id,
                to_zone,
                library_position,
                ..
            } => {
                let position = match (to_zone, library_position) {
                    (Zone::Library, Some(LibraryPosition::Top)) => " top".to_string(),
                    (Zone::Library, Some(LibraryPosition::Bottom)) => " bottom".to_string(),
                    (Zone::Library, Some(LibraryPosition::NthFromTop { n })) => {
                        format!(" {} from top", n)
                    }
                    _ => String::new(),
                };
                format!(
                    "MoveToZone ({} → {:?}{})",
                    obj(*object_id),
                    to_zone,
                    position
                )
            }
            DebugAction::CreateCard {
                card_name,
                owner,
                zone,
                attach_to,
                run_etb,
            } => {
                let attach_suffix = match attach_to {
                    Some(AttachTarget::Object(id)) => format!(" attached to {}", obj(*id)),
                    Some(AttachTarget::Player(pid)) => {
                        format!(" attached to {}", player_label(*pid))
                    }
                    None => String::new(),
                };
                let etb_suffix = if *run_etb { "" } else { " (no ETB)" };
                format!(
                    "CreateCard ({} for {} in {:?}{}{})",
                    card_name,
                    player_label(*owner),
                    zone,
                    attach_suffix,
                    etb_suffix,
                )
            }
            DebugAction::RemoveObject { object_id } => {
                format!("RemoveObject ({})", obj(*object_id))
            }
            DebugAction::Sacrifice { object_id } => {
                format!("Sacrifice ({})", obj(*object_id))
            }
            DebugAction::Reveal { player_id, count } => {
                format!("Reveal (top {} of {})", count, player_label(*player_id))
            }
            DebugAction::DrawCards { player_id, count } => {
                format!("DrawCards ({} draws {})", player_label(*player_id), count)
            }
            DebugAction::Mill { player_id, count } => {
                format!("Mill ({} mills {})", player_label(*player_id), count)
            }
            DebugAction::ShuffleLibrary { player_id } => {
                format!("ShuffleLibrary ({})", player_label(*player_id))
            }
            DebugAction::Proliferate { player_id } => {
                format!("Proliferate ({})", player_label(*player_id))
            }
            DebugAction::SetBasePowerToughness {
                object_id,
                power,
                toughness,
            } => format!(
                "SetBasePowerToughness ({} → {:?}/{:?})",
                obj(*object_id),
                power,
                toughness
            ),
            DebugAction::ModifyCounters {
                object_id,
                counter_type,
                delta,
            } => format!(
                "ModifyCounters ({:+} {:?} counters on {})",
                delta,
                counter_type,
                obj(*object_id)
            ),
            DebugAction::ModifyPlayerCounters {
                player_id,
                counter_kind,
                delta,
            } => format!(
                "ModifyPlayerCounters ({:+} {} counters on {})",
                delta,
                counter_kind,
                player_label(*player_id)
            ),
            DebugAction::ModifyEnergy { player_id, delta } => format!(
                "ModifyEnergy ({:+} energy on {})",
                delta,
                player_label(*player_id)
            ),
            DebugAction::SetTapped { object_id, tapped } => format!(
                "SetTapped ({} → {})",
                obj(*object_id),
                if *tapped { "tapped" } else { "untapped" }
            ),
            DebugAction::SetPrepared {
                object_id,
                prepared,
            } => format!(
                "SetPrepared ({} → {})",
                obj(*object_id),
                if *prepared { "prepared" } else { "unprepared" }
            ),
            DebugAction::SetController {
                object_id,
                controller,
            } => format!(
                "SetController ({} → {})",
                obj(*object_id),
                player_label(*controller)
            ),
            DebugAction::SetSummoningSickness { object_id, sick } => format!(
                "SetSummoningSickness ({} → {})",
                obj(*object_id),
                if *sick { "sick" } else { "not sick" }
            ),
            DebugAction::SetFaceState {
                object_id,
                face_down,
                transformed,
                flipped,
            } => format!(
                "SetFaceState ({}, face_down={:?}, transformed={:?}, flipped={:?})",
                obj(*object_id),
                face_down,
                transformed,
                flipped
            ),
            DebugAction::Attach { object_id, target } => {
                let target_label = match target {
                    AttachTarget::Object(id) => obj(*id),
                    AttachTarget::Player(pid) => player_label(*pid),
                };
                format!("Attach ({} → {})", obj(*object_id), target_label)
            }
            DebugAction::Detach { object_id } => format!("Detach ({})", obj(*object_id)),
            DebugAction::GrantKeyword { object_id, keyword } => {
                format!("GrantKeyword ({} gains {:?})", obj(*object_id), keyword)
            }
            DebugAction::RemoveKeyword { object_id, keyword } => {
                format!("RemoveKeyword ({} loses {:?})", obj(*object_id), keyword)
            }
            DebugAction::SetLife { player_id, life } => {
                format!("SetLife ({} → {})", player_label(*player_id), life)
            }
            DebugAction::AddMana { player_id, mana } => {
                format!("AddMana ({} gains {:?})", player_label(*player_id), mana)
            }
            DebugAction::SetInfiniteMana { player_id, enabled } => format!(
                "SetInfiniteMana ({} {})",
                player_label(*player_id),
                if *enabled { "on" } else { "off" }
            ),
            DebugAction::SetPhase {
                phase,
                active_player,
            } => format!(
                "SetPhase ({:?} for {})",
                phase,
                player_label(*active_player)
            ),
            DebugAction::RunStateBasedActions => "RunStateBasedActions".to_string(),
            DebugAction::CreateToken { request, run_etb } => {
                let counters = if request.enter_with_counters().is_empty() {
                    String::new()
                } else {
                    let parts: Vec<String> = request
                        .enter_with_counters()
                        .iter()
                        .map(|(ct, n)| format!("{n} {}", ct.as_str()))
                        .collect();
                    format!(" with {}", parts.join(", "))
                };
                let token_label = match request {
                    DebugTokenRequest::Preset { preset_id, .. } => preset_id.as_str(),
                    DebugTokenRequest::Custom {
                        characteristics, ..
                    } => characteristics.display_name.as_str(),
                };
                let etb_suffix = if *run_etb { "" } else { " (no ETB)" };
                format!(
                    "CreateToken ({} for {}{}{})",
                    token_label,
                    player_label(request.owner()),
                    counters,
                    etb_suffix
                )
            }
            DebugAction::CreateTokenCopy { source_id, owner } => format!(
                "CreateTokenCopy ({} for {})",
                obj(*source_id),
                player_label(*owner)
            ),
        }
    }
}

/// Serde default for `GameAction::ChooseManaColor::count` — a single activation
/// when the field is absent (every pre-batch client/serialized action).
fn default_one() -> u32 {
    1
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
            GameAction::Foretell { object_id, .. } => Some(*object_id),
            GameAction::CastSpellAsSneak { hand_object, .. } => Some(*hand_object),
            GameAction::CastSpellAsWebSlinging { hand_object, .. } => Some(*hand_object),
            GameAction::ActivateNinjutsu {
                ninjutsu_object_id, ..
            } => Some(*ninjutsu_object_id),
            GameAction::CastSpellForFree { object_id, .. }
            | GameAction::CastSpellAsMiracle { object_id, .. }
            | GameAction::CastSpellAsMadness { object_id, .. } => Some(*object_id),
            GameAction::ActivateAbility { source_id, .. } => Some(*source_id),
            GameAction::TapLandForMana { object_id } => Some(*object_id),
            GameAction::UntapLandForMana { object_id } => Some(*object_id),
            GameAction::Equip { equipment_id, .. } => Some(*equipment_id),
            GameAction::CrewVehicle { vehicle_id, .. } => Some(*vehicle_id),
            GameAction::ActivateStation { spacecraft_id, .. } => Some(*spacecraft_id),
            GameAction::SaddleMount { mount_id, .. } => Some(*mount_id),
            GameAction::Transform { object_id } => Some(*object_id),
            GameAction::UnlockRoomDoor { object_id, .. } => Some(*object_id),
            GameAction::PlayFaceDown { object_id, .. } => Some(*object_id),
            GameAction::TurnFaceUp { object_id } => Some(*object_id),
            GameAction::ChooseRingBearer { target } => Some(*target),
            GameAction::ChoosePair { partner } => *partner,
            GameAction::ChooseDamageSource { source } => Some(*source),
            GameAction::ChooseUntap { object_id, .. } => Some(*object_id),
            GameAction::TapForConvoke { object_id, .. } => Some(*object_id),
            GameAction::ChooseLegend { keep } => Some(*keep),
            GameAction::CastPreparedCopy { source } => Some(*source),
            GameAction::CastParadigmCopy { source } => Some(*source),
            // Actions with no per-permanent anchor.
            GameAction::PassPriority
            | GameAction::ChooseExert { .. }
            | GameAction::DeclareAttackers { .. }
            | GameAction::DeclareBlockers { .. }
            | GameAction::MulliganDecision { .. }
            | GameAction::ReorderHand { .. }
            | GameAction::SelectCards { .. }
            | GameAction::ChooseRemoveCounterCostDistribution { .. }
            | GameAction::SelectCoinFlips { .. }
            | GameAction::ChooseOutsideGameCards { .. }
            | GameAction::SelectTargets { .. }
            | GameAction::ChooseTarget { .. }
            | GameAction::ChooseReplacement { .. }
            | GameAction::OrderTriggers { .. }
            | GameAction::CancelCast
            | GameAction::SubmitSideboard { .. }
            | GameAction::ChoosePlayDraw { .. }
            | GameAction::ChooseOption { .. }
            | GameAction::SubmitSpellbookDraft { .. }
            | GameAction::SubmitPilePartition { .. }
            | GameAction::ChoosePile { .. }
            | GameAction::ChooseBranch { .. }
            | GameAction::SelectModes { .. }
            | GameAction::DecideOptionalCost { .. }
            | GameAction::RespondToSpliceOffer { .. }
            | GameAction::ChooseAdventureFace { .. }
            | GameAction::ChooseModalFace { .. }
            | GameAction::ChooseAlternativeCast { .. }
            | GameAction::ChooseCastingVariant { .. }
            | GameAction::KeepAllCopyTargets
            | GameAction::ChoosePermanentTypeSlot { .. }
            | GameAction::DecideOptionalEffect { .. }
            | GameAction::DecideOptionalEffectAndRemember { .. }
            | GameAction::PayUnlessCost { .. }
            | GameAction::ChooseUnlessCostBranch { .. }
            | GameAction::PayCombatTax { .. }
            | GameAction::ChooseDungeon { .. }
            | GameAction::ChooseDungeonRoom { .. }
            | GameAction::ChooseSpecializeColor { .. }
            | GameAction::HarmonizeTap { .. }
            | GameAction::DeclareCompanion { .. }
            | GameAction::CompanionToHand
            | GameAction::DiscoverChoice { .. }
            | GameAction::CascadeChoice { .. }
            | GameAction::RippleChoice { .. }
            | GameAction::FreeCastWindowChoice { .. }
            | GameAction::ChooseTopOrBottom { .. }
            | GameAction::ChooseMutateMergeSide { .. }
            | GameAction::CipherEncode { .. }
            | GameAction::ChooseClashOpponent { .. }
            | GameAction::ChooseAssistPlayer { .. }
            | GameAction::CommitAssistPayment { .. }
            | GameAction::ChooseBattleProtector { .. }
            | GameAction::SetAutoPass { .. }
            | GameAction::CancelAutoPass
            | GameAction::SetPhaseStops { .. }
            | GameAction::AssignCombatDamage { .. }
            | GameAction::AssignBlockerDamage { .. }
            | GameAction::DistributeAmong { .. }
            | GameAction::ChooseCounterMoveDistribution { .. }
            | GameAction::SubmitPayAmount { .. }
            | GameAction::RetargetSpell { .. }
            | GameAction::LearnDecision { .. }
            | GameAction::SelectCategoryPermanents { .. }
            | GameAction::ChooseX { .. }
            | GameAction::SubmitPhyrexianChoices { .. }
            | GameAction::ChooseManaColor { .. }
            | GameAction::PayManaAbilityMana { .. }
            | GameAction::PassParadigmOffer
            | GameAction::Concede { .. }
            | GameAction::Debug(_)
            | GameAction::GrantDebugPermission { .. }
            | GameAction::RevokeDebugPermission { .. }
            | GameAction::ChooseActivationCostBranch { .. } => None,
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

            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        };
        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(json["type"], "CastSpell");
        assert_eq!(json["data"]["object_id"], 5);
        assert_eq!(json["data"]["targets"], serde_json::json!([10, 20]));
    }

    #[test]
    fn mulligan_decision_roundtrips() {
        let action = GameAction::MulliganDecision {
            choice: MulliganChoice::Keep,
        };
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
            bands: vec![],
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
            bands: vec![],
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

                    payment_mode: crate::types::game_state::CastPaymentMode::Auto,
                },
                Some(oid),
            ),
            (
                GameAction::Foretell {
                    object_id: oid,
                    card_id: cid,
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
            (
                GameAction::ActivateNinjutsu {
                    ninjutsu_object_id: oid,
                    creature_to_return: ObjectId(99),
                },
                Some(oid),
            ),
            (
                GameAction::CastSpellAsWebSlinging {
                    hand_object: oid,
                    card_id: cid,
                    creature_to_return: ObjectId(99),

                    payment_mode: crate::types::game_state::CastPaymentMode::Auto,
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
            (
                GameAction::MulliganDecision {
                    choice: MulliganChoice::Keep,
                },
                None,
            ),
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
