//! Cross-item document relations.
//!
//! CR 607.2d: "If an object has an ability printed on it that causes a player to
//! 'choose a [value]' and an ability printed on it that refers to 'the chosen
//! [value]' … those abilities are linked." A document relation is a link between
//! two (or more) parsed items that a single item cannot express on its own — one
//! item *produces* a fact (a chosen value, an exiled card, a coerced attacker)
//! that another item *consumes*.
//!
//! These links are **text-level anaphoric facts**, recognizable the moment items
//! are emitted with stable ids — not intrinsically properties of the lowered
//! runtime shapes. So they are recovered at parse time by pairing producer and
//! consumer items **by `OracleItemId`** and stored on the `OracleDocIr`, then
//! applied at the single `lower_oracle_ir` seam by resolving those ids back to
//! their lowered definitions. This replaces the former dual authority — scanning
//! the lowered category vectors by shape to *rediscover* the pairs — which the
//! parser/lower split exists to remove.
//!
//! The enum is **closed** (no wildcard): a new document relation must be a named
//! variant, and a new *linked-choice* shape must be a new `LinkedChoiceKind`
//! value, never a new relation variant (CR 607.2d is one rule section, so the
//! whole choice axis is one parameterized variant).

use super::doc::OracleItemId;
use crate::types::ability::ChosenSubtypeKind;

/// A cross-item relation between parsed document items, recovered at parse time
/// and applied by id during lowering. Closed set.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) enum DocumentRelationIr {
    /// CR 607.1 + CR 607.2a + CR 406.6 + CR 610.3: A two-trigger exile/return
    /// design. An ETB "exile ..." trigger (`etb_exile`) pairs with an LTB "return
    /// the exiled card(s)" trigger (`ltb_return`); because the ETB exile has no
    /// printed duration, the exiled card(s) would never return. Covers both the
    /// single-target class (Journey to Nowhere, Oblivion Ring) and the mass-exile
    /// class (Worldgorger Dragon's "exile all other permanents you control").
    ///
    /// `outcome` records how the pair is applied. An unmodified LTB return is
    /// `DurationStamped`: applying it stamps `Duration::UntilHostLeavesPlay` on
    /// the ETB exile so the existing `ExileLink::UntilSourceLeaves` mechanism
    /// returns the cards. A return carrying an entry rider (Realm Razer's "return
    /// the exiled cards to the battlefield TAPPED") is `ModifierUnsupported`: the
    /// automatic return path performs a plain zone move and can't apply that
    /// rider, so instead of silently dropping it the LTB return is marked
    /// unsupported through `Effect::unimplemented` so coverage reports the gap
    /// rather than the card showing as falsely supported. See
    /// `trigger_is_ltb_return_shape` and `change_zone_return_has_no_entry_modifiers`
    /// in `parser/oracle.rs`.
    EtbExileLtbReturn {
        etb_exile: OracleItemId,
        ltb_return: OracleItemId,
        outcome: LinkedReturnOutcome,
    },
    /// CR 102.1 + CR 603.7c + CR 608.2c: A mass-`MustAttack` coerce clause over
    /// the active player (`coerce`, Siren's Call) pairs with a sibling delayed
    /// punisher (`punisher`) whose "that player controls" anaphor defaulted to
    /// `You`. Applying the relation rebinds the punisher's destroyed-set
    /// controller to the active player (and folds the CR 302.6 / CR 508.1a
    /// continuous-control exemption sibling into the set predicate).
    ActivePlayerPunisher {
        coerce: OracleItemId,
        punisher: OracleItemId,
    },
    /// CR 607.2d: A "choose a [value]" producer linked to an ability that reads
    /// "the chosen [value]" back. One parameterized relation; `LinkedChoiceKind`
    /// distinguishes the value kind and the consumer surface that reads it.
    LinkedChoice(LinkedChoiceKind),
}

/// CR 610.3: How a detected ETB-exile / LTB-return pair is applied during
/// lowering. Parameterizes `EtbExileLtbReturn` rather than adding a sibling
/// relation variant (see the module doc): both outcomes describe the same
/// linked-ability pair, differing only in whether the generic return path can
/// carry the printed return.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) enum LinkedReturnOutcome {
    /// The LTB return is unmodified. Applying the relation stamps
    /// `Duration::UntilHostLeavesPlay` on the ETB exile so the automatic
    /// `ExileLink::UntilSourceLeaves` path returns the exiled card(s).
    DurationStamped,
    /// CR 610.3: The LTB return carries an entry modifier the automatic return
    /// path cannot apply (Realm Razer's "return the exiled cards to the
    /// battlefield tapped"). Applying the relation attaches an
    /// `Effect::unimplemented` marker to the LTB return trigger so the
    /// unsupported semantic is visible to coverage instead of silently dropped.
    /// `fragment` is the verbatim LTB return text, captured at detection time
    /// because the relation applier no longer has access to the item list.
    ModifierUnsupported { fragment: String },
}

/// CR 607.2d: The kind of linked-choice relation — what value is chosen and
/// which consumer surface reads it back. Derived from the three cross-item
/// reconcile call sites; a fourth shape is a new value here, never a new
/// `DocumentRelationIr` variant.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) enum LinkedChoiceKind {
    /// CR 607.2d + CR 614.1c: A persisted "as this enters, choose a creature
    /// type / color" replacement (`chooser`) linked to a self-ETB counter
    /// replacement (`counter`) whose counter count reads the chosen creature
    /// type / color. Applying folds the counter replacement's execute into the
    /// chooser's sub-ability chain, so the single enters-replacement carries both.
    EtbCounterCount {
        chooser: OracleItemId,
        counter: OracleItemId,
    },
    /// CR 607.2d + CR 205.3: A chosen-subtype value linked to the statics /
    /// dig-filter surfaces that read "the chosen type". The resolved subtype
    /// `chosen` is carried as payload because it is determined at detection time
    /// — from a persisted creature/land-type choice, or (for a card whose type
    /// line fixes it) the card's printed types. The consumers are split by the
    /// exact rewrite each needs, so lowering applies them by id with no shape
    /// rescanning:
    ChosenTypeStatic {
        /// The resolved subtype the linked consumers are realigned to.
        chosen: ChosenSubtypeKind,
        /// CR 205.3 + CR 608.2c: items whose `IsChosenCardType` discriminator is
        /// realigned to `IsChosenCreatureType` (a static's `ModifyCost` spell
        /// filter, or an ability's/trigger's `Dig` filter). Non-empty only when
        /// `chosen` is a creature type — a card-type chooser (Umori) keeps
        /// `IsChosenCardType`.
        retarget: Vec<OracleItemId>,
        /// Self-"~ is the chosen type" statics whose `AddChosenSubtype` kind is
        /// set to `chosen`.
        set_subtype: Vec<OracleItemId>,
    },
    /// CR 607.2d + CR 613.1: A "choose a player / an opponent" producer linked to
    /// a durable `SourceChosenPlayer` reader elsewhere on the card. Each producer
    /// `chooser` has its choice persisted so the continuous reader can read it
    /// after the choosing ability finishes resolving. Emitted only when a reader
    /// exists, so a resolution-scoped choice with no durable reader stays
    /// non-persisted.
    PersistedPlayer { choosers: Vec<OracleItemId> },
}
