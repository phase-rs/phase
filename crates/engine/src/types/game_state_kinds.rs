use serde::{Deserialize, Serialize};

use super::ability::{Comparator, QuantityExpr, TargetRef};
use super::identifiers::ObjectId;
use super::keywords::{Keyword, KeywordKind};
use super::player::PlayerId;
use super::zones::Zone;
use crate::types::attack_target::AttackTarget;

/// Tracks whether the game is in day or night state (CR 730).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DayNight {
    Day,
    Night,
}

/// CR 601.2f: The kind of modification to apply to the next qualifying spell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum NextSpellModifier {
    /// "The next spell you cast this turn can't be countered."
    CantBeCountered,
    /// "The next spell you cast this turn has [keyword]."
    HasKeyword { keyword: Keyword },
    /// "The next spell you cast this turn can be cast as though it had flash."
    CastAsThoughFlash,
    /// CR 118.9a: "The next [filter] spell you cast this turn can be cast without
    /// paying its mana cost." Additional costs still apply (CR 118.8).
    WithoutPayingManaCost,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TargetSelectionConstraint {
    /// CR 601.2c + CR 115.3: every chosen player target must be a different
    /// player. Attached by the modal path ("each mode must target a different
    /// player"), where distinctness spans the ability's modes rather than one
    /// instance of the word "target".
    DifferentTargetPlayers,
    /// CR 115.1 + CR 601.2c: Object targets must be controlled by different players.
    DifferentObjectControllers,
    /// CR 115.1 + CR 601.2c + CR 400.1: Object targets must come from the same
    /// player-owned zone of the given kind, e.g. "from a single graveyard".
    SameZoneOwner { zone: Zone },
    /// CR 202.3 + CR 601.2c: the chosen target set's combined mana value must
    /// satisfy `comparator` against `value`. `value` is a `QuantityExpr` (not
    /// `i32` like `SearchSelectionConstraint::TotalManaValue`) because the bound
    /// is the dynamic where-X die result (`EventContextAmount`). NOT unified with
    /// `SearchSelectionConstraint::TotalManaValue` — different CR section
    /// (CR 115.1 / CR 601.2c target declaration vs CR 701.23 search-set) and a
    /// different value type.
    TotalManaValue {
        comparator: Comparator,
        value: QuantityExpr,
    },
}

/// CR 601.2d: What is being distributed (damage, counters, life).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum DistributionUnit {
    Damage,
    /// CR 601.2d: Even split — engine auto-computes `total / num_targets` (rounded down).
    /// No player *choice* in HOW it's split, but whether the flow pauses at
    /// `WaitingFor::DistributeAmong` depends on the casting route:
    /// - Non-deferred-target-selection flow (inside `finalize_mana_payment`):
    ///   the split is applied inline and `WaitingFor::DistributeAmong` is bypassed.
    /// - Deferred-target-selection flow (e.g. Fireball, gated by
    ///   `ability_utils::ability_distribution_pool_needs_chosen_x`): the cast still
    ///   pauses at `WaitingFor::DistributeAmong` via
    ///   `casting_targets::maybe_pause_for_cast_distribution` — the split itself is
    ///   automatic, but the pause is needed so cost (CR 601.2f) and target legality
    ///   can re-resolve once targets are known.
    EvenSplitDamage,
    Counters(String),
    Life,
}

/// CR 115.7: Scope of retargeting — single target, all targets, or forced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum RetargetScope {
    Single,
    All,
    ForcedTo(TargetRef),
}

/// CR 702.190b: Placement data for a Sneak-cast **permanent** spell —
/// captures the `(defender, attack_target)` pair from the returned creature's
/// `AttackerInfo` at cost-payment time, so the permanent can enter the
/// battlefield attacking the same target after resolution (by which point
/// combat no longer remembers the returned creature).
///
/// Absent for instant/sorcery Sneak casts (CR 702.190b applies only to
/// permanent spells).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SneakPlacement {
    pub defender: PlayerId,
    pub attack_target: AttackTarget,
}

/// How a spell was cast — determines zone routing and post-resolution behavior.
/// Replaces individual boolean flags (cast_as_adventure, cast_as_warp) with a
/// single enum that captures the casting context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CastingVariant {
    /// Normal spell cast — no special resolution behavior.
    #[default]
    Normal,
    /// CR 715.4: Cast as the Adventure half. On resolution, exiled with
    /// AdventureCreature permission and creature face restored.
    Adventure,
    /// CR 720.3d / CR 720.4: Cast as the Omen half. On resolution, shuffled
    /// into its owner's library with normal characteristics restored.
    Omen,
    /// CR 702.185a: Cast via Warp alternative cost from hand. On resolution,
    /// creates a delayed trigger to exile at end step with WarpExile permission.
    Warp,
    /// CR 702.138: Cast from graveyard via Escape. On resolution, goes to
    /// appropriate zone normally (unlike Flashback which exiles).
    Escape,
    /// CR 702.81a: Cast from graveyard via Retrace by discarding a land card as
    /// an additional cost. Resolution uses normal spell routing.
    Retrace,
    /// CR 702.180a: Cast from graveyard for harmonize cost. On resolution, exiled
    /// instead of going anywhere else (unlike Escape which returns to graveyard).
    Harmonize,
    /// CR 702.187b: Cast from graveyard for mayhem cost (allowed only while the
    /// card was discarded this turn). Unlike Flashback/Harmonize, the spell is
    /// NOT exiled — it resolves normally (like Escape), so it can be discarded
    /// and recast again on a later turn.
    Mayhem,
    /// CR 702.34a: Cast from graveyard for flashback cost. On resolution (or
    /// whenever leaving the stack for any reason), exiled instead of going anywhere else.
    Flashback,
    /// CR 702.127a: Cast an aftermath half of a split card from a graveyard.
    /// If it was cast from a graveyard, exile it any time it leaves the stack.
    Aftermath,
    /// CR 702.146a-b + CR 712.8c: Cast transformed from graveyard for disturb
    /// cost. The stack spell uses its back-face characteristics and the
    /// permanent enters the battlefield back face up on resolution.
    Disturb,
    /// CR 601.2a: Cast from graveyard via a static permission source (e.g. Lurrus).
    /// Stores the granting permanent's ObjectId for per-turn tracking.
    /// CR 400.7: the source's per-turn entry ends when it leaves the battlefield
    /// (`zones::apply_zone_exit_cleanup`), so an object that returns has its own.
    GraveyardPermission {
        source: ObjectId,
        /// CR 601.2a: When `OncePerTurn`, casting consumes this source's slot in
        /// `graveyard_cast_permissions_used`. `Unlimited` permissions (Conduit)
        /// skip tracking entirely. When `OncePerTurnPerPermanentType` (Muldrotha),
        /// casting consumes the `(source, slot_type)` entry in
        /// `graveyard_cast_permissions_used_per_type` — see `slot_type`.
        frequency: super::statics::CastFrequency,
        /// CR 110.4: Permanent type slot consumed when `frequency` is
        /// `OncePerTurnPerPermanentType`. Always one of the six CR 110.4
        /// permanent types. `None` for `Unlimited` and `OncePerTurn`
        /// frequencies (those track by source only).
        #[serde(default)]
        slot_type: Option<super::card_type::CoreType>,
        /// CR 614.1a: Some graveyard cast permissions add "If a spell cast
        /// this way would be put into your graveyard, exile it instead."
        /// This replaces only stack-to-graveyard destinations.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        graveyard_destination_replacement: Option<Zone>,
    },
    /// CR 601.2b + CR 118.9a: Cast from hand via a `CastFromHandFree` static
    /// permission source (Zaffai). Stores the granting permanent's ObjectId for
    /// per-turn tracking. Omniscience's unconditional silent path does not use
    /// this variant — it short-circuits the mana cost to NoCost while leaving
    /// `casting_variant = Normal`.
    /// CR 400.7: the source's per-turn entry ends when it leaves the battlefield
    /// (`zones::apply_zone_exit_cleanup`), so an object that returns has its own.
    HandPermission {
        source: ObjectId,
        /// CR 601.2b: When `OncePerTurn`, casting consumes this source's slot in
        /// `hand_cast_free_permissions_used`.
        frequency: super::statics::CastFrequency,
    },
    /// CR 601.2a + CR 113.6b + CR 118.9a: Cast from exile via a
    /// `StaticMode::ExileCastPermission` source (Maralen, Fae Ascendant).
    /// Stores the granting permanent's ObjectId for per-turn tracking; the
    /// finalize-cast step zeroes the spell's mana cost when the static carries
    /// `without_paying_mana_cost: true` (the only published shape today). The
    /// resolution-time routing matches a normal cast — no on-resolve exile
    /// behavior — so this is treated as a casting-context tag, not as an
    /// alternative cost.
    /// CR 400.7: the source's per-turn slot ends when it leaves the battlefield
    /// (`zones::apply_zone_exit_cleanup`), so an object that returns has its own.
    ExilePermission {
        source: ObjectId,
        /// CR 601.2a: When `OncePerTurn`, casting consumes this source's slot
        /// in `exile_cast_permissions_used`. `Unlimited` skips tracking.
        frequency: super::statics::CastFrequency,
    },
    /// CR 702.190a: Cast from HAND via the Sneak alternative cost. Legal only
    /// during the declare-blockers step. The returned unblocked attacker you
    /// control is part of the cost, bounced to its owner's hand at
    /// `finalize_cast_to_stack`.
    ///
    /// CR 702.190b applies only to **permanent spells**: on resolution the
    /// permanent enters tapped and attacking the same defender as the
    /// returned creature. Non-permanent spells (instants/sorceries) resolve
    /// normally with no alongside-attacker placement, so `placement` is
    /// `None` for those casts. This `Option` carries real per-card-class data
    /// (not a discriminator) — see `SneakPlacement`.
    Sneak {
        returned_creature: ObjectId,
        /// CR 702.190b data for permanent spells; `None` for instants/sorceries.
        placement: Option<SneakPlacement>,
    },
    /// CR 702.188a: Cast from hand via Web-slinging's alternative cost by
    /// returning a tapped creature you control to its owner's hand rather than
    /// paying the spell's mana cost. Unlike Sneak, Web-slinging grants no
    /// special timing permission and has no enter-attacking placement rule.
    WebSlinging { returned_creature: ObjectId },
    /// CR 702.94a + CR 608.2g: Cast from hand via Miracle's alternative cost after
    /// revealing the card as the first card drawn this turn. This is a UNIT variant;
    /// the concrete miracle cost is NOT re-read from live keywords at cast time.
    /// Instead it is latched at offer-enqueue on `CastOfferKind::Miracle.cost` (a
    /// concrete `ManaCost::Cost` resolved by `draw.rs`) and threaded through
    /// `handle_cast_spell_as_miracle_with_payment_mode` →
    /// `prepare_spell_cast_with_variant_override_inner(latched_alt_cost)`. This
    /// preserves the offered cost even if the granting source (e.g. Aminatou) has
    /// left the battlefield between reveal-accept and trigger resolution
    /// (CR 608.2b last-known-information).
    Miracle,
    /// CR 702.35a: Cast from exile via Madness after the discard replacement
    /// exiled the card and its madness triggered ability resolved.
    Madness,
    /// CR 702.74a: Cast from hand via Evoke's alternative cost. On resolution,
    /// the permanent enters tagged with `CastVariantPaid::Evoke`, which fires
    /// the synthesized intervening-if ETB sacrifice trigger.
    Evoke,
    /// CR 702.119a-c: Cast from hand via Emerge's alternative cost. The printed
    /// mana cost is replaced by `Keyword::Emerge(cost)` at cast preparation;
    /// casting requires sacrificing a creature, then reduces that emerge cost by
    /// the sacrificed creature's mana value. Resolution routing matches a normal
    /// cast; Emerge has no resolution rider.
    Emerge,
    /// CR 702.109a: Cast from hand via Dash's alternative cost. On resolution,
    /// `dash::install_dash_riders` grants the permanent haste and schedules a
    /// next-end-step return to its owner's hand.
    Dash,
    /// CR 702.152a: Cast from hand via Blitz's alternative cost. On resolution,
    /// `blitz::install_blitz_riders` grants the permanent haste and a dies-draw
    /// trigger and schedules a next-end-step sacrifice.
    Blitz,
    /// CR 702.137a: Cast from hand via Spectacle's alternative cost, available
    /// only if an opponent lost life this turn. A pure cost substitution — the
    /// spell resolves normally with no resolution riders.
    Spectacle,
    /// CR 702.62a: Cast from exile via Suspend's "play it without paying its
    /// mana cost" trigger after the last time counter was removed. On resolution
    /// of the resulting permanent, the stack handler tags
    /// `CastVariantPaid::Suspend` and — for creature spells — installs a
    /// transient continuous "has haste" effect that lasts as long as the
    /// resolution-time controller still controls the permanent.
    Suspend,
    /// CR 702.170d: Cast from exile via the Plot "cast without paying its mana
    /// cost" permission during the owner's main phase on a turn after the card
    /// was plotted. Detected at cast preparation when the exile-zone source has
    /// a `CastingPermission::Plotted { turn_plotted }` and the current turn is
    /// strictly greater than `turn_plotted`. Zeroes the mana cost and routes
    /// through the normal cast pipeline; no special resolution-time behavior.
    Plot,
    /// CR 702.143a-c: Cast from exile via a foretold card's foretell cost on a
    /// turn after it was foretold. Detected at cast preparation when the
    /// exile-zone source has a `CastingPermission::Foretold { .. }`. The
    /// permission supplies the alternative mana cost; stack finalization tags
    /// the source with `CastVariantPaid::Foretell` so "if this spell was
    /// foretold" clauses can evaluate while the spell resolves.
    Foretell,
    /// CR 702.96a-c: Cast from hand via Overload's alternative cost. The
    /// printed mana cost is replaced by `Keyword::Overload(cost)` at cast
    /// preparation (mirrors `Evoke`/`Warp`). Per CR 702.96b, every "target"
    /// in the spell's text is replaced by "each" — applied as a cast-time
    /// transformation of the spell's ability tree (`Destroy`→`DestroyAll`,
    /// `Pump`→`PumpAll`, `DealDamage`→`DamageAll`, `Tap`→`TapAll`,
    /// `Bounce`→`ChangeZoneAll`). Per CR 702.96c, the resulting spell has
    /// no targets, so target selection is naturally skipped because the
    /// transformed effects carry no `TargetRef` slots.
    Overload,
    /// CR 702.103a-b: Cast from hand via Bestow's alternative cost. The
    /// printed mana cost is replaced by `Keyword::Bestow(cost)` at cast
    /// preparation; the spell becomes an Aura with `enchant creature` while
    /// on the stack and as the resulting permanent (until it becomes
    /// unattached, per CR 702.103f). The type-changing mutation is applied
    /// directly to the stack object (mirroring `swap_to_alternative_spell_face`
    /// for Adventure/Omen) — Layers cannot be used here because they only
    /// apply to battlefield/hand objects, not stack objects.
    ///
    /// Per CR 702.103e: if the target is illegal at resolution, the
    /// type-changing effect ends and the spell resolves as a creature spell.
    /// Per CR 702.103f: when a bestowed Aura becomes unattached on the
    /// battlefield, the type-changing effect ends — it remains as an
    /// enchantment creature (overrides CR 704.5m for bestow Auras).
    Bestow,
    /// CR 702.113a: Cast from hand via Awaken's alternative cost. The printed
    /// mana cost is replaced by `Keyword::Awaken { cost }` at cast preparation
    /// (mirrors `Overload`). A resolution rider is appended to the tail of the
    /// spell's ability tree (`effects::awaken::append_awaken_rider`): the
    /// printed effect resolves first, then "put N +1/+1 counters on target land
    /// you control; that land becomes a 0/0 Elemental creature with haste; it's
    /// still a land." Per CR 702.113b, the land target only exists on the awaken
    /// variant — a normal cast appends no rider and requests no land target.
    /// CR 702.113a: the spell goes to the graveyard normally, so this variant is
    /// deliberately absent from `exiles_when_leaving_stack_for_any_reason`.
    Awaken,
    /// CR 702.148a-b + CR 612: Cast from hand via Cleave's alternative cost
    /// (CR 118.9). The printed mana cost is replaced by `Keyword::Cleave(cost)`
    /// at cast preparation (mirrors `Evoke`/`Overload`). Per CR 702.148a, paying
    /// the cleave cost is a text-changing effect (CR 612) that removes every
    /// square-bracketed span from the spell's rules text. The bracket-removed
    /// ability set is parsed at build time into `CardFace::cleave_variant` and
    /// swapped onto the stack object before preparation (mirroring the Bestow
    /// object-mutation-before-prepare seam). Resolution routing matches a normal
    /// spell — there is no on-resolve special behavior, so the spell goes to its
    /// owner's graveyard like any instant/sorcery.
    Cleave,
    /// CR 702.162a + CR 712.14a: Cast from any castable zone via the More Than
    /// Meets the Eye alternative cost. The printed mana cost is replaced by the
    /// `Keyword::MoreThanMeetsTheEye(cost)` payload at cast preparation (mirrors
    /// Overload). On resolution the spell is cast CONVERTED — the resulting
    /// permanent enters the battlefield transformed (back face up) via the
    /// existing `enter_transformed` ZoneChange seed. CR 701.28 (Convert).
    MoreThanMeetsTheEye,
    /// CR 702.176a: Cast from hand via Impending's alternative cost. The printed
    /// mana cost is replaced by `Keyword::Impending { cost, .. }` at cast
    /// preparation (mirrors Overload/Evoke). On resolution the permanent enters
    /// with N time counters (from the keyword) and is not a creature while any
    /// remain. At the beginning of your end step one time counter is removed.
    Impending,
    /// CR 702.160a: Cast from hand prototyped. The printed mana cost is replaced
    /// by the prototype cost during cast preparation, and the object is tagged so
    /// stack display plus layer evaluation use the secondary mana cost and P/T
    /// while it is a creature.
    Prototype,
    /// CR 702.140a-c: Cast from hand via Mutate's alternative cost. The printed
    /// mana cost is replaced by `Keyword::Mutate(cost)` at cast preparation
    /// (mirrors Bestow). The spell gains a single target — a non-Human creature
    /// the caster owns (CR 702.140a) — attached Bestow-style before preparation.
    /// On resolution (`stack::resolve_top`): if the target is illegal
    /// (CR 702.140b) the spell reverts to a plain creature spell and enters the
    /// battlefield normally; if legal (CR 702.140c) it does NOT enter — instead
    /// it merges with the target creature (CR 730) and the controller chooses
    /// top/bottom. Unlike Bestow this variant neither exiles on leaving the stack
    /// nor restores a front face, so it is intentionally absent from
    /// `exiles_when_leaving_stack_for_any_reason` and
    /// `restores_front_face_after_stack_exit`.
    Mutate,
    /// CR 702.173a: Cast from hand via Freerunning's alternative cost. Legal
    /// only when a player was dealt combat damage this turn by an Assassin
    /// creature or a commander under the caster's control. The printed mana
    /// cost is replaced by the `Keyword::Freerunning(cost)` payload at cast
    /// preparation (mirrors `Overload` / `Foretell`). Resolution routing
    /// matches a normal cast — no on-resolve special behavior — so this is a
    /// casting-context tag, not a resolution-affecting variant.
    Freerunning,
    /// CR 702.76a: Cast from hand via Prowl's alternative cost. Legal only when a
    /// player was dealt combat damage this turn by a source under the caster's
    /// control that, at damage time, had any of this spell's creature types. The
    /// printed mana cost is replaced by the `Keyword::Prowl(cost)` payload at cast
    /// preparation (mirrors `Freerunning`/`Overload`). Resolution routing matches
    /// a normal cast — no on-resolve special behavior — so this is a
    /// casting-context tag, not a resolution-affecting variant.
    Prowl,
    /// CR 702.133a: Cast from a graveyard via Jump-start. The card is cast for
    /// its normal mana cost plus an additional cost of discarding a card
    /// (CR 601.2b/601.2f–h) — so, like `Retrace`/`Aftermath`, this is an
    /// additional cost, not an alternative cost, and is absent from
    /// `uses_alternative_cost`. Like `Flashback`, a spell cast this way is
    /// exiled instead of going anywhere else any time it would leave the stack
    /// (see `exiles_when_leaving_stack_for_any_reason`).
    JumpStart,
    /// CR 702.102a-d: Both halves of a split card cast from hand as a fused
    /// split spell. The mana cost is the combined cost of both halves
    /// (CR 702.102c). On resolution, the left half's instructions are followed
    /// first, then the right half's (CR 702.102d). Not an alternative cost
    /// (CR 118.9a) — the player pays the full combined printed mana cost.
    Fuse,
    /// CR 702.117a: Cast from hand for the surge alternative cost, legal only if
    /// the caster has cast another spell this turn. Resolution is normal (no
    /// exile/restore), so it appears only in `uses_alternative_cost`.
    Surge,
    /// CR 708.4 + CR 702.37c (Morph) / CR 702.168b (Disguise): Cast a card face
    /// down as a 2/2 face-down creature spell by paying a fixed {3} (an
    /// alternative cost, CR 601.2b) rather than its mana cost. Morph, Megamorph,
    /// and Disguise all cast identically this way — the only differences (ward {2}
    /// for Disguise per CR 702.168a; the turn-face-up cost) are read downstream
    /// from the hidden real card's keyword, so this variant is parameterless.
    ///
    /// Runtime: `casting::continue_cast_face_down` blanks the object to its
    /// face-down 2/2 (stashing the real card in `back_face`) BEFORE it is put on
    /// the stack (CR 708.4), so the whole downstream face-down machinery is
    /// inherited: `visibility` redacts the stack spell to opponents, the object
    /// resolves onto the battlefield still face down (CR 702.37c), and
    /// `GameAction::TurnFaceUp` (CR 702.37e) flips it. The face-down spell is an
    /// alternative cast (appears in `uses_alternative_cost`); it neither exiles
    /// on resolution nor restores a front face (the object simply stays face down),
    /// but CR 708.9 reveal-on-leave-stack is handled by the shared
    /// `apply_zone_exit_cleanup`, so it needs no `restores_front_face_after_stack_exit`.
    FaceDown,
}

impl CastingVariant {
    pub fn is_normal(&self) -> bool {
        *self == CastingVariant::Normal
    }

    /// CR 118.9b: the keyword that names this casting method, matched against
    /// a permission that requires one ("You may cast this card from your
    /// graveyard using its blitz ability."). `None` when no keyword names the
    /// method: the printed cost and the permission routes, and the keyword
    /// methods `KeywordKind` has no discriminant for. A permission that requires
    /// a method admits a cast only when this is `Some` of that keyword.
    pub fn cast_keyword(self) -> Option<crate::types::keywords::KeywordKind> {
        match self {
            CastingVariant::Warp => Some(KeywordKind::Warp),
            CastingVariant::Escape => Some(KeywordKind::Escape),
            CastingVariant::Retrace => Some(KeywordKind::Retrace),
            CastingVariant::Harmonize => Some(KeywordKind::Harmonize),
            CastingVariant::Mayhem => Some(KeywordKind::Mayhem),
            CastingVariant::Flashback => Some(KeywordKind::Flashback),
            CastingVariant::Aftermath => Some(KeywordKind::Aftermath),
            CastingVariant::Disturb => Some(KeywordKind::Disturb),
            CastingVariant::Sneak { .. } => Some(KeywordKind::Sneak),
            CastingVariant::Miracle => Some(KeywordKind::Miracle),
            CastingVariant::Madness => Some(KeywordKind::Madness),
            CastingVariant::Dash => Some(KeywordKind::Dash),
            CastingVariant::Blitz => Some(KeywordKind::Blitz),
            CastingVariant::Suspend => Some(KeywordKind::Suspend),
            CastingVariant::Plot => Some(KeywordKind::Plot),
            CastingVariant::Foretell => Some(KeywordKind::Foretell),
            CastingVariant::Bestow => Some(KeywordKind::Bestow),
            CastingVariant::Awaken => Some(KeywordKind::Awaken),
            CastingVariant::Cleave => Some(KeywordKind::Cleave),
            CastingVariant::MoreThanMeetsTheEye => Some(KeywordKind::MoreThanMeetsTheEye),
            CastingVariant::Mutate => Some(KeywordKind::Mutate),
            CastingVariant::Freerunning => Some(KeywordKind::Freerunning),
            CastingVariant::JumpStart => Some(KeywordKind::JumpStart),
            CastingVariant::Fuse => Some(KeywordKind::Fuse),
            CastingVariant::Normal
            | CastingVariant::Adventure
            | CastingVariant::Omen
            | CastingVariant::GraveyardPermission { .. }
            | CastingVariant::HandPermission { .. }
            | CastingVariant::ExilePermission { .. }
            | CastingVariant::WebSlinging { .. }
            | CastingVariant::Evoke
            | CastingVariant::Emerge
            | CastingVariant::Spectacle
            | CastingVariant::Overload
            | CastingVariant::Impending
            | CastingVariant::Prototype
            | CastingVariant::Prowl
            | CastingVariant::Surge
            | CastingVariant::FaceDown => None,
        }
    }

    /// CR 601.2a: The `ObjectId` of the `StaticMode::ExileCastPermission` source
    /// elected for this cast, when the variant is `ExilePermission`. The cast
    /// pipeline carries the elected source so per-source cost treatment
    /// (extra-cost riders) binds to the permission the player actually cast
    /// through — not whichever functioning source a battlefield scan reaches
    /// first when several permissions offer the same exiled spell.
    pub fn exile_permission_source(self) -> Option<ObjectId> {
        match self {
            CastingVariant::ExilePermission { source, .. } => Some(source),
            _ => None,
        }
    }

    /// CR 118.9a: Only one alternative cost can be applied to a spell.
    pub fn uses_alternative_cost(self) -> bool {
        match self {
            CastingVariant::Warp
            | CastingVariant::Escape
            | CastingVariant::Harmonize
            | CastingVariant::Mayhem
            | CastingVariant::Flashback
            | CastingVariant::HandPermission { .. }
            | CastingVariant::Sneak { .. }
            | CastingVariant::WebSlinging { .. }
            | CastingVariant::Miracle
            | CastingVariant::Madness
            | CastingVariant::Evoke
            | CastingVariant::Emerge
            | CastingVariant::Dash
            | CastingVariant::Blitz
            | CastingVariant::Spectacle
            | CastingVariant::Suspend
            | CastingVariant::Plot
            | CastingVariant::Foretell
            | CastingVariant::Overload
            | CastingVariant::Bestow
            | CastingVariant::Awaken
            | CastingVariant::Cleave
            | CastingVariant::MoreThanMeetsTheEye
            | CastingVariant::Disturb
            | CastingVariant::Impending
            // CR 702.140a: Mutate replaces the spell's mana cost with the mutate
            // cost — an alternative cost, so only one may apply (CR 118.9a).
            | CastingVariant::Mutate
            // CR 702.76a: Prowl substitutes the prowl cost for the printed cost.
            | CastingVariant::Prowl
            // CR 702.117a: Surge substitutes the surge cost for the printed cost.
            | CastingVariant::Surge
            // CR 601.2b + CR 702.37c / CR 702.168b: casting face down pays a fixed
            // {3} rather than the printed mana cost — an alternative cost, so only
            // one alternative casting method may apply (CR 118.9a).
            | CastingVariant::FaceDown
            | CastingVariant::Freerunning => true,
            CastingVariant::Normal
            | CastingVariant::Adventure
            | CastingVariant::Omen
            | CastingVariant::Retrace
            | CastingVariant::Aftermath
            // CR 718.2 + CR 718.3b: Prototype swaps in an alternative SET OF
            // CHARACTERISTICS — the prototyped mana cost IS the spell's mana
            // cost, not an alternative cost (CR 118.9). A prototyped cast can
            // therefore still combine with one alternative cost (a free-cast
            // grant or rider).
            | CastingVariant::Prototype
            // CR 702.133a: Jump-start discards a card as an *additional* cost on
            // top of the normal mana cost — not an alternative cost (CR 118.9a).
            | CastingVariant::JumpStart
            // CR 702.102c + CR 118.9a: Fuse pays the full combined printed mana
            // cost of both halves — not an alternative cost.
            | CastingVariant::Fuse
            | CastingVariant::GraveyardPermission { .. }
            | CastingVariant::ExilePermission { .. } => false,
        }
    }

    /// CR 118.9a + CR 601.2b: does this variant elect an alternative cost of
    /// the CARD's own — a keyword rider (Evoke, Bestow, Overload, …) or the
    /// fixed face-down {3} — INDEPENDENT of whatever zone authority admits
    /// the cast? Such a rider may only ride a normal-cost route: an
    /// alternative-cost grant underneath it would be a second alternative
    /// cost on the same cast.
    ///
    /// Distinct from [`CastingVariant::uses_alternative_cost`]: a variant
    /// whose alternative cost IS its admitting permission's cost (Madness,
    /// Suspend, Plot, Foretell, the permission elections, the graveyard
    /// keyword routes) forms ONE casting method with its route — it applies
    /// one alternative cost total, never a second — so it is `false` here
    /// while `true` there.
    pub fn is_independent_alternative_cost_rider(self) -> bool {
        match self {
            CastingVariant::Miracle
            | CastingVariant::Evoke
            | CastingVariant::Emerge
            | CastingVariant::Dash
            | CastingVariant::Blitz
            | CastingVariant::Spectacle
            | CastingVariant::Overload
            | CastingVariant::Bestow
            | CastingVariant::Awaken
            | CastingVariant::Cleave
            | CastingVariant::MoreThanMeetsTheEye
            | CastingVariant::Impending
            | CastingVariant::Mutate
            | CastingVariant::Freerunning
            | CastingVariant::Prowl
            | CastingVariant::Surge
            // CR 702.185a: the warp cost is the HAND-side alternative cost;
            // the exile return cast is its own `WarpExile` route and never
            // elects this variant.
            | CastingVariant::Warp
            | CastingVariant::Sneak { .. }
            | CastingVariant::WebSlinging { .. }
            // CR 601.2b + CR 702.37c / CR 702.168b: the fixed face-down {3}.
            | CastingVariant::FaceDown => true,
            CastingVariant::Normal
            | CastingVariant::Adventure
            | CastingVariant::Omen
            // CR 718.2 + CR 718.3b: alternative CHARACTERISTICS, not an
            // alternative cost — a free grant stacks with the prototype
            // election (see `uses_alternative_cost`).
            | CastingVariant::Prototype
            // CR 702.81a / CR 702.133a / CR 702.102c: additional or combined
            // printed costs, no alternative cost anywhere.
            | CastingVariant::Retrace
            | CastingVariant::JumpStart
            | CastingVariant::Fuse
            | CastingVariant::Aftermath
            // Route-coupled: the variant's alternative cost IS its admitting
            // permission's or keyword-route's cost — one method (CR 118.9a).
            | CastingVariant::Madness
            | CastingVariant::Suspend
            | CastingVariant::Plot
            | CastingVariant::Foretell
            | CastingVariant::Escape
            | CastingVariant::Flashback
            | CastingVariant::Harmonize
            | CastingVariant::Mayhem
            | CastingVariant::Disturb
            | CastingVariant::GraveyardPermission { .. }
            | CastingVariant::HandPermission { .. }
            | CastingVariant::ExilePermission { .. } => false,
        }
    }

    pub fn exiles_when_leaving_stack_for_any_reason(self) -> bool {
        matches!(
            self,
            CastingVariant::Flashback
                | CastingVariant::Aftermath
                | CastingVariant::Harmonize
                // CR 702.133a: "exile this card instead of putting it anywhere
                // else any time it would leave the stack."
                | CastingVariant::JumpStart
        )
    }

    pub fn stack_to_graveyard_replacement(self) -> Option<Zone> {
        if self.exiles_when_leaving_stack_for_any_reason() {
            return Some(Zone::Exile);
        }
        if let CastingVariant::GraveyardPermission {
            graveyard_destination_replacement,
            ..
        } = self
        {
            return graveyard_destination_replacement;
        }
        None
    }

    pub fn replaces_stack_to_graveyard_with_exile(self) -> bool {
        matches!(self.stack_to_graveyard_replacement(), Some(Zone::Exile))
    }

    /// CR 400.7 + CR 712.11a: these variants put a non-front face on the
    /// stack. If the spell leaves the stack without becoming that face on the
    /// battlefield, restore the object's normal front-face characteristics.
    pub fn restores_front_face_after_stack_exit(self) -> bool {
        matches!(
            self,
            CastingVariant::Adventure
                | CastingVariant::Omen
                | CastingVariant::MoreThanMeetsTheEye
                | CastingVariant::Disturb
        )
    }
}
