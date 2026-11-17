//! Activation-patience tactical policy.
//!
//! Report (Discord #ai-suggestions): "The AI, in general, should not make blind
//! decisions during random phases, such as upkeep. If I have a 5-toughness
//! creature in play and my opponent has a Grim Lavamancer, there are very few
//! situations where the AI should activate the Grim Lavamancer to deal 2 damage
//! to my 5-toughness creature **without drawing a card first**."
//!
//! The observation generalises past that one card. The upkeep step, taken with
//! an empty stack, is the AI's LOWEST-INFORMATION window of the turn: it has
//! not drawn yet this turn, no spell has been cast, and combat has not been
//! declared. For an ability that is still going to be there in the main phase,
//! activating during upkeep spends a resource to buy strictly less information
//! than waiting would. Nothing in the corpus modelled that: `TacticalWindow`
//! (`tactical_gate.rs`) has no upkeep variant, and `card_hints` gives every
//! `ActivateAbility` a flat base score with no timing term at all.
//!
//! The draw step is gated for a different reason. CR 117.3a and CR 504.1/504.2
//! (matched by the engine's own comment beside `execute_draw` in
//! `crates/engine/src/game/turns.rs`): the active player receives priority
//! during the draw step only AFTER the turn-based draw, so waiting there buys no
//! information. But it gives nothing up either: the ability can still be
//! activated in the precombat main phase, and the mana and untapped creatures it
//! would spend now are still free for the sorcery-speed plays only the main phase
//! allows (CR 307.1). The same holds in an upkeep whose triggers have already
//! resolved. It fails when the main phase opens with something an earlier
//! activation could still change (CR 505.3-505.5 turn-based actions, CR 117.3a
//! triggers) or, from the upkeep, when a trigger reaches the draw step (see
//! "What a hold rests on"), so those boards are not gated.
//! The untap step is excluded because CR 502.4 means no player ever receives
//! priority in it at all, so `WaitingFor::Priority` cannot occur there to gate on.
//!
//! So this policy asks one question — **does waiting cost anything?** — and
//! penalises the activation when the answer is no.
//!
//! ## What a hold rests on
//!
//! Lending an activation's line to passing (`search::lend_deferral_witness`) rests on one
//! premise: activating in the main phase reproduces it, because nothing between this priority
//! and the main phase can depend on it. Holding rests on the same premise, with two exceptions,
//! a search and a clear of the hand (below). Every stand-down and every draw-setup escape hatch
//! below is an instance of that rule. From the upkeep, these lie in between:
//!
//! - The draw step's beginning triggers (CR 117.3a). An intervening-if is checked as the step
//!   begins (CR 603.4), so the activation may be what makes it true: Voltaic Key untapping a
//!   tapped Howling Mine. Any trigger that reaches the AI's draw step (its own "your"
//!   triggers, anyone's "each player's", an opponent's "each opponent's", and delayed
//!   triggers set for it, CR 603.7) stands the whole upkeep down (`patient_window`).
//! - The draw step's draw (CR 504.1 + CR 121.1). An activation that replaces it (CR 614.4) or
//!   changes what it takes from a library that can be the AI's own stands itself down
//!   (`DrawSetup`, escape hatch 2), except a search and a clear of the hand, which are held and
//!   not lent.
//! - The main phase's opening events (CR 505.3-505.5, CR 117.3a), which stand the upkeep and
//!   the draw step down (`main_phase_opens_with_events`).
//!
//! From the draw step only the last of these remains: its draw and its triggers are behind its
//! priority (CR 504.2). The upkeep's own triggers resolve before its stack is next empty, so
//! they only end its claim to being the low-information window (`PatientWindow::BeforeMain`).
//!
//! A search of a library that can be the AI's own is held but not lent
//! (`HeldLine::Irreproducible`). It doesn't commute with the draw: with an Island on top as the
//! library's only match, Flooded Strand in the upkeep ends with the Island in play and another
//! card drawn, while passing draws the Island and a main-phase fetch finds nothing. The search
//! can't tell a known top from an unknown one (no determinization ships), so every such search
//! stands out of lending. It stays held because, with the order hidden (CR 401.2), choosing the
//! land after the draw is what patience is for. That hold is now the penalty alone. On a board
//! where the line resolves within the quiesce, the fetch's line sees a draw that Pass's static
//! read does not, so the search may prefer cracking in the upkeep. The draw being behind, a
//! search in the draw step is lent like any other activation.
//!
//! A clear of the AI's hand is held but not lent for the same reason. Magus of the Wheel in the
//! upkeep keeps the draw step's card on top of its seven, and in the main phase discards it. So
//! is anything else sized by the hand, and a discard the hand can't pay. So is an activation
//! whose own cost the hand sizes (CR 602.2b + CR 601.2f): Loreseeker's Stone costs {1} more for
//! each card in hand, so the main phase pays more for the same line, or can't pay it. It stays
//! held, because the main phase can play the hand down first.
//!
//! A continuation is a whole line, so such an activation, or one that stands down over the draw
//! (Crystal Ball), ends the lend of every other hold at its root (`root_held_line`): a held
//! Wellwisher's line can go on to crack Flooded Strand.
//!
//! An activation whose own restriction lapses before the main phase stands down, because waiting
//! gives it up (CR 602.5): a cap on the hand, which the draw lifts it out of (Library of
//! Alexandria), and a phase with no main phase in it (Dwarven Armory).
//!
//! These cases of the rule are not yet classified. The trigger scans read only permanents
//! already on the battlefield, so a trigger whose source the held activation itself adds is not
//! seen: Quicksilver Amulet putting Avaricious Dragon onto the battlefield in the upkeep, or a
//! control effect taking a permanent that has one, and likewise for the main-phase stand-down.
//! And the draw step's draw itself triggers "whenever you draw a card" abilities (CR 603.2),
//! whose intervening-if (CR 603.4) an upkeep activation may make true: Starving Revenant's
//! descend 8 after an upkeep sacrifice. Only beginning-of-step triggers are scanned. And the
//! classification does not read inside an inline branch carrier (a die roll's results) or a node
//! the parser left `Unimplemented`. And `root_held_line` reads each action the
//! root offers, by its own definition, so a lent line can still go through what that doesn't
//! show: an activation that line alone puts on offer, a cast beyond its spell's resolution, or
//! an activation that stands down for another reason (one whose own `condition` is met now).
//!
//! The classification errs toward standing down, so it also releases activations that could
//! have waited: a card put on the bottom changes this draw only on an empty library
//! (CR 704.5b), a "target player" slot, a card slot, a tracked set, an anaphoric player whose
//! parent target isn't an opponent's, or a node with no slot is read as able to name
//! the AI even when the line names an opponent, and a blind mill or exile is equal in
//! distribution on either side of the draw while the order is hidden. Those lose the hold, and
//! the donor's line still sees a draw that Pass's does not, so the search may now prefer
//! activating. For a blind mill that preference reads the real library order (no
//! determinization ships). Those that are searches lose the lend and keep the hold. A discard
//! of "all the cards" followed by drawing that many (Soratami Seer) ends at the same hand size
//! either way, and loses the lend too. So does a line main would do better, sized by the hand
//! (Tomoya the Revealer), a "target player discards" on a hand too small to pay (Disrupting
//! Scepter), and Attunement on an empty hand, whose cost pays the discard.
//!
//! ## Why a soft penalty and not a `Reject`
//!
//! "Later is better" is a judgement about information, not a rule-derived
//! impossibility, and `tactical_gate` owns the latter (see its layering note).
//! A `Reject` would also remove the activation from search entirely, so it
//! could neither be chosen when another policy prices a real payoff above the
//! penalty nor lend its line to passing. On the search branch the root applies
//! this verdict at full strength beside the weighted tactical term, and passing
//! inherits the held activation's continuation (`search::lend_deferral_witness`):
//! waiting keeps the activation on offer (CR 117.1b), so the root compares
//! acting now with waiting on their tactical scores, not on a continuation the
//! short beam reaches only on the acting line. The escape hatches below are
//! therefore what keeps an early activation's search value. On the heuristic
//! branch the penalty is sized to lose to a real payoff and win against an idle
//! ping. On the search branch, where other policies' payoffs are scaled by the
//! root `tactical_weight`, a payoff beats the hold only above the penalty
//! divided by that weight. At a root offering an activation passing can't reach, every hold has
//! the penalty only: nothing is lent there (see "What a hold rests on"), so an activation whose
//! line beats Pass's by more than the penalty fires in the upkeep.
//!
//! ## The escape hatches
//!
//! Floating mana the activation may spend comes first: it empties as the step
//! ends (CR 500.5), so for it waiting is not free. A draw replacement made in the
//! upkeep comes next: it must exist before the draw it replaces (CR 614.4). So
//! does an arrangement that can reach the top of the AI's own library — scry,
//! surveil, or looking at the top cards and putting them back in any order, on the
//! AI's library or on a player the AI may name (CR 115.1: Elemental Augury's
//! "target player's library" can be the AI's own): the draw step's
//! draw takes the top card (CR 504.1 + CR 121.1), and the upkeep's priority is the
//! last before it (CR 502.4, CR 503.1), so only an upkeep activation chooses that
//! card. An arrangement made later arranges a later draw, so waiting loses the
//! value of arranging this one, whatever the ability costs. A tap cost or a
//! once-each-turn cap does not make the hold right: it only makes the activation
//! dearer, and whether this draw's choice is worth the tap or the capped
//! activation is a resource question the search's valuation answers. Any other change to that
//! library before the draw gives up the same thing: a card put on top of it (Academy Ruins,
//! whose artifact this draw then takes), cards taken from the top of it, a look whose card is
//! then acted on, a shuffle with no search. `draw_setup` reads the engine's wildcard-free library
//! authorities. A new effect that moves a card to or from a library is a draw setup until it is
//! named otherwise. A new reorder within a library is not seen by the authority, and needs its
//! own arm beside `Scry`. A search is the exception: it stays held and isn't lent. A clear of
//! the hand is treated the same way. In the draw step that draw is behind (CR 504.2), and any
//! such change is held like any other activation. The report names the other two times to activate early: when the
//! damage is part of a lethal line, and (implicitly) when the ability's own condition is met *now* and might
//! not be later. Those are lethal damage and `ability.condition`, in that order
//! below. Its third exception, a source that "is going to die or leave play
//! before the AI draws a card", has no reading in these windows: both are the
//! AI's own turn with an empty stack, so nothing is targeting the source and no
//! opponent attacks before the main phase.

use engine::ai_support::CandidateAction;
use engine::game::{attractions, mana_abilities};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, AbilityTag, ActivationRestriction, Comparator,
    CostReduction, DelayedTriggerCondition, Effect, ParsedCondition, PlayerScope, QuantityExpr,
    QuantityRef, TargetFilter, ZoneRef,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use super::anti_self_harm::extract_damage_amount;
use super::context::PolicyContext;
use super::effect_classify::ability_is_temporary_combat_modifier;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::ability_chain::{collect_scoped_nodes, AbilityScope};
use crate::cast_facts::cast_object_for_action;
use crate::features::lifegain::filter_is_opponent_scoped;
use crate::features::DeckFeatures;
use engine::game::players;

/// Base magnitude of the patience penalty, before the profile's interaction
/// patience scales it: preference band at the default patience (strong band at
/// 0.75 and up), enough, on the heuristic branch, to lose to any real payoff the
/// other policies price and to beat an idle activation's flat base score. On the
/// search branch the verdict is applied unweighted at the root
/// (`planner::RankedCandidate::deferral`), against other policies' payoffs scaled
/// by the root `tactical_weight`, so there a payoff must exceed this penalty
/// divided by that weight.
const PATIENCE_PENALTY_BASE: f64 = 1.2;

pub struct ActivationPatiencePolicy;

/// Whether activating later reproduces a line, so that passing keeps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeldLine {
    /// Nothing before the main phase can depend on the activation. For a hold, this also takes
    /// every other action the root offers ([`root_held_line`]): a continuation is a whole line,
    /// and only then may PassPriority inherit it (`search::lend_deferral_witness`).
    Reproducible,
    /// The line is one passing can't reach, or at a hold's root, may go through one. It keeps
    /// its own continuation.
    Irreproducible,
}

/// Does deferring this activation to a later window cost anything **structurally**?
///
/// Deliberately narrow: it answers only what can be read off the ability
/// itself, never board-state judgement. The escape hatches live in
/// [`ActivationPatiencePolicy::verdict`], on top of this.
///
/// Note this is NOT the same question `search::empty_stack_activation_is_low_value`
/// asks, and the two must not be merged. That predicate feeds the auto-pass
/// fast path and means "not worth searching for" — for which a mana ability and
/// a temporary combat modifier are the two YES answers, because both are
/// pointless in an empty-stack upkeep. Here those same two shapes are the two
/// NO answers, because waiting is not free for them. Same shapes, opposite
/// polarity. Folding this into that fast path would additionally let it skip a
/// lethal line, since the fast path bypasses policy scoring and would inherit
/// none of the escape hatches below.
///
/// Two shapes are NOT deferrable:
///
/// * A mana ability (CR 605.1a). It does not use the stack, produces mana that
///   empties at the end of the step (CR 106.4), and is answered by the payment
///   pipeline rather than by timing.
/// * A temporary combat modifier (CR 611.2 + CR 514.2 — the resolution
///   generates the continuous effect, and the cleanup step ends it). Waiting
///   is not "free" for those; they are window-bound. `EffectTimingPolicy`'s
///   `combat_trick_score` prices those whose whole value is combat
///   (`CombatBinding::Whole`); out of combat, the rest are not priced.
///
/// Cycling is excluded so this does not double-charge alongside
/// `CyclingDisciplinePolicy`, which owns that class's timing.
fn activation_is_deferrable(state: &GameState, source_id: ObjectId, ability_index: usize) -> bool {
    printed_ability(state, source_id, ability_index).is_some_and(ability_is_deferrable)
}

/// The ability an activation candidate names, as `ctx.effects()` reads it: the entry at
/// `ability_index` in the source's `abilities`, printed or granted by a layer.
fn printed_ability(
    state: &GameState,
    source_id: ObjectId,
    ability_index: usize,
) -> Option<&AbilityDefinition> {
    state
        .objects
        .get(&source_id)
        .and_then(|object| object.abilities.get(ability_index))
}

fn ability_is_deferrable(ability: &AbilityDefinition) -> bool {
    !mana_abilities::is_mana_ability(ability)
        && !ability_is_temporary_combat_modifier(ability)
        && ability.ability_tag != Some(AbilityTag::Cycling)
        // CR 602.5: an ability restricted to a phase before the main phase can't be activated
        // there, so waiting would give it up.
        && !ability
            .activation_restrictions
            .iter()
            .any(restriction_excludes_main)
}

/// CR 602.5: does this restriction leave the main phase out? "During your upkeep" and "during
/// any upkeep step" (Dwarven Armory) do.
fn restriction_excludes_main(restriction: &ActivationRestriction) -> bool {
    match restriction {
        ActivationRestriction::DuringYourUpkeep => true,
        ActivationRestriction::RequiresCondition {
            condition: Some(ParsedCondition::CurrentPhaseIs { phases }),
        } => !phases.contains(&Phase::PreCombatMain),
        _ => false,
    }
}

/// CR 602.5 + CR 504.1: does this restriction cap the AI's hand, so that the draw step's draw
/// can make the activation illegal? A floor can't lapse this way (Resonating Lute), and neither
/// can a comparison with another player's hand from the AI's side (Kitsune Bonesetter).
fn restriction_caps_hand(restriction: &ActivationRestriction) -> bool {
    matches!(
        restriction,
        ActivationRestriction::RequiresCondition { condition: Some(condition) }
            if condition_caps_hand(condition)
    )
}

fn condition_caps_hand(condition: &ParsedCondition) -> bool {
    match condition {
        ParsedCondition::QuantityComparison {
            lhs,
            comparator: Comparator::EQ | Comparator::LT | Comparator::LE,
            ..
        } => lhs.any_ref(&mut reads_a_hand),
        ParsedCondition::HandSizeExact { .. } | ParsedCondition::HandSizeOneOf { .. } => true,
        ParsedCondition::And { conditions } | ParsedCondition::Or { conditions } => {
            conditions.iter().any(condition_caps_hand)
        }
        _ => false,
    }
}

/// What an upkeep activation does to the draw step's coming draw (CR 504.1 + CR 121.1). Declared
/// in precedence order (`upkeep_draw_setup` takes the least): everything that stands down comes
/// before everything that stays held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DrawSetup {
    /// CR 602.5 + CR 504.1: a restriction that caps the AI's hand, which the draw lifts it out
    /// of. Waiting gives the activation up, so it stands down, as an upkeep-only ability does.
    HandBound,
    /// CR 614.4: a replacement for that draw must exist before it.
    Replacement,
    /// CR 121.1: a change to a library that can be the AI's own, after which that draw takes a
    /// different card or a chosen one: an arrangement of its top (CR 701.22a, CR 701.25a,
    /// CR 701.20e), a card put on it, or cards taken from it (CR 701.17a).
    LibraryTop,
    /// CR 701.23a + CR 701.24a: a search of a library that can be the AI's own. The found card
    /// leaves the pool, and a shuffle replaces the top, so this does not commute with the draw
    /// (CR 121.1). Unlike the three above it stays held, because its payoff is a card better
    /// chosen after the draw; only its line isn't lent. [`HeldLine::Irreproducible`].
    Search,
    /// CR 121.1 + CR 701.9a: a line sized by the AI's hand, a cost sized by it (CR 602.2b +
    /// CR 601.2f), or a discard the hand can't pay (CR 609.3). An activation before the draw
    /// reads a hand without the card that draw takes; one after it does not. So it does not
    /// commute with the draw, and like a search it is held and not lent: held because main can
    /// play out the hand first.
    HandClear,
}

/// What this activation does to the draw step's coming draw, while that draw is ahead.
/// `Phase::Upkeep`, not `PatientWindow::Upkeep`: an upkeep whose triggers have resolved is a
/// `BeforeMain` window with the draw still ahead (CR 504.1). Every branch is read: the line the
/// search takes through a mode is the one passing does not reproduce.
fn upkeep_draw_setup(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    ability_index: usize,
) -> Option<DrawSetup> {
    if state.phase != Phase::Upkeep {
        return None;
    }
    let ability = printed_ability(state, source_id, ability_index)?;
    if ability
        .activation_restrictions
        .iter()
        .any(restriction_caps_hand)
    {
        return Some(DrawSetup::HandBound);
    }
    ability_draw_setup(state, ai_player, source_id, ability)
}

/// [`upkeep_draw_setup`] for a cast: what each face the cast can put on the stack does to that
/// draw, read from its spell abilities (CR 113.3a + CR 608.2). Nothing but a cast has one.
fn upkeep_spell_draw_setup(
    state: &GameState,
    ai_player: PlayerId,
    action: &GameAction,
) -> Option<DrawSetup> {
    if state.phase != Phase::Upkeep {
        return None;
    }
    // CR 722.3c: a prepared copy casts the source's second face.
    let object = match action {
        GameAction::CastPreparedCopy { source } => state.objects.get(source)?,
        action => cast_object_for_action(state, action, ai_player)?,
    };
    let back = object.back_face.as_ref();
    // CR 720.3d: an Omen shuffles into its owner's library as it resolves.
    let omen = back.is_some_and(|face| face.card_types.subtypes.iter().any(|s| s == "Omen"));
    object
        .abilities
        .iter()
        .chain(back.into_iter().flat_map(|face| face.abilities.iter()))
        .filter(|ability| ability.kind == AbilityKind::Spell)
        .filter_map(|ability| ability_draw_setup(state, ai_player, object.id, ability))
        .chain(omen.then_some(DrawSetup::LibraryTop))
        .min()
}

/// The least [`draw_setup`] of any node of `ability`, whose source is `source_id`.
fn ability_draw_setup(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    ability: &AbilityDefinition,
) -> Option<DrawSetup> {
    let chain = collect_scoped_nodes(ability, AbilityScope::Potential);
    chain
        .iter()
        .filter_map(|node| draw_setup(state, ai_player, source_id, &chain, node))
        .min()
}

/// Whether passing keeps the lines of the activations ActivationPatience holds at this root:
/// [`HeldLine::Reproducible`] only when [`held_line`] says so of every candidate.
///
/// A continuation is a whole line, not its root action. A reproducible donor's line can go on to
/// an action passing can't reach: Wellwisher, then Flooded Strand, on a board where passing
/// draws the Island. Nothing here says which actions a line took, so one irreproducible action
/// on offer ends the lend of every hold at the root, and:
///
/// - It counts held or not. An arrangement stands down and passing can't reach it either
///   (Crystal Ball).
/// - It needs no continuation of its own: none above Pass's, none open, none at all. Its line
///   from the root bounds nothing about the line through it, which starts after the donor and
///   ranks its beams apart. A sibling off the beam has no line, and one that wins takes the root
///   regardless.
/// - `candidates` must be the engine-issued list, before the tactical gate, validation and
///   `max_branching`. An interior node ranks the engine's list on its own, with no gate, so a
///   sibling the root's beam dropped can be in a donor's line.
///
/// The rule: an action on offer bars when its own definition, read against the pool and the hand
/// as they stand, shows the main phase can't repeat it. Three things end before then: the priority
/// it's legal in, the mana in the pool, and the draw being ahead. So [`held_line`] reads every
/// candidate that way:
///
/// - An activation: a restriction that leaves the main phase out (CR 602.5), mana it may spend
///   that empties first (CR 500.5), and its every branch against the draw (`draw_setup`).
/// - A cast: every branch of each face it can put on the stack, against the draw, by the same
///   reading, and a prepared copy's face. This is any spell the engine offers here, so an instant
///   or a flash spell (CR 117.1a, CR 702.8a), and a scry cantrip bars like an arrangement:
///   passing draws what it would have set aside. A spell that leaves the draw alone doesn't
///   (Shock, Giant Growth).
/// - Anything else: no bar. Crewing keeps past the draw, and a special action leaves all three
///   alone, though not what it triggers (below).
///
/// Any other stand-down bars only by showing one of these: passing reaches a mana ability, a
/// combat modifier or a cycling ability, and no lethal line is lent (`open_continuation`).
///
/// The rule reads only that, at the root, so these stay out of it, and a line can go through
/// them:
///
/// - What isn't on offer at the root: an activation only the donor's line puts there (one that
///   untaps the sibling's source).
/// - An activation's own `condition`, unread here: one met now and not after the draw (escape
///   hatch 4).
/// - What a cast does beyond its spell's resolution: its additional cost, the floating mana that
///   pays it, and a flash permanent's enters trigger, which is the permanent's and not the
///   spell's (CR 608.3a + CR 603.6a). So too what turning a permanent face up triggers (Nervous
///   Gardener's search). A spell's own cost static is unread too, though the draw can change it
///   (CR 601.2f): Saiba Syphoner costs {2} less with no instant or sorcery card in hand, so a
///   drawn instant raises it. Only an activation's own cost modification is read (`draw_setup`).
///
/// It also bars what doesn't need barring: at rung 0 a donor's continuation is its quiesced
/// resolution, which takes no sibling, and a deeper line may not take it either. Pass then keeps
/// its own continuation, and the holds their penalties, so a hold whose line beats Pass's by more
/// than the penalty fires in the upkeep: Wellwisher and five Elves beside Fabled Passage.
pub(crate) fn root_held_line(
    state: &GameState,
    ai_player: PlayerId,
    candidates: &[CandidateAction],
) -> HeldLine {
    if candidates
        .iter()
        .any(|candidate| held_line(state, ai_player, &candidate.action) == HeldLine::Irreproducible)
    {
        HeldLine::Irreproducible
    } else {
        HeldLine::Reproducible
    }
}

/// Whether passing keeps the line of this one action, held or not: [`root_held_line`] gives the
/// rule. An activation that stands down over the draw is irreproducible, and so is one the main
/// phase loses, and a spell that changes the draw.
pub(crate) fn held_line(state: &GameState, ai_player: PlayerId, action: &GameAction) -> HeldLine {
    let GameAction::ActivateAbility {
        source_id,
        ability_index,
        ..
    } = action
    else {
        return line_past_draw(upkeep_spell_draw_setup(state, ai_player, action));
    };
    if activation_lapses_before_main(state, ai_player, *source_id, *ability_index) {
        return HeldLine::Irreproducible;
    }
    line_past_draw(upkeep_draw_setup(
        state,
        ai_player,
        *source_id,
        *ability_index,
    ))
}

/// Does waiting for the main phase lose this activation as it stands, whatever it does?
/// CR 602.5: a restriction can leave the main phase out. CR 500.5: mana it may spend empties as
/// the step ends, read as escape hatch 1 reads it: of a deferrable activation.
fn activation_lapses_before_main(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    ability_index: usize,
) -> bool {
    printed_ability(state, source_id, ability_index).is_some_and(|ability| {
        ability
            .activation_restrictions
            .iter()
            .any(restriction_excludes_main)
    }) || (activation_is_deferrable(state, source_id, ability_index)
        && engine::game::casting::pool_has_expiring_mana_for_activation(
            state,
            ai_player,
            source_id,
            ability_index,
        ))
}

/// Whether a line keeps past the draw, given what it does to that draw. Every kind of setup
/// answers here, held or not.
fn line_past_draw(setup: Option<DrawSetup>) -> HeldLine {
    match setup {
        None => HeldLine::Reproducible,
        Some(
            DrawSetup::HandBound
            | DrawSetup::Replacement
            | DrawSetup::LibraryTop
            | DrawSetup::Search
            | DrawSetup::HandClear,
        ) => HeldLine::Irreproducible,
    }
}

/// What `node`, of an ability `ai_player` activates from `source_id` or of the spell it casts as
/// `source_id`, does to the draw step's coming draw. `chain` is every node of that ability.
///
/// The question is whether the node commutes with that draw: does activating before it leave
/// the game activating after it would? Fail-closed: anything the engine's wildcard-free library
/// authorities call a move to or from a library is a setup, so a new effect that makes such a
/// move is one
/// until it is named below. What is named commutes with the draw, except a search and a hand
/// clear:
///
/// - A draw (CR 121.1): this draw and that one take the same two cards, in either order. So do
///   connive and learn (CR 701.50a, CR 701.48a), which draw. A chain that also puts a card
///   back or clears the hand is a setup through that other node (Sensei's Divining Top,
///   Brainstone).
/// - A search of a library that can be the AI's own (CR 701.23a) does not commute: the found
///   card leaves the pool, so a card on top can be fetched away from the draw, and a shuffle
///   replaces that top (CR 701.24a). It is `DrawSetup::Search`: held, because its payoff is a
///   card better chosen after the draw, and not lent. Its own instructions answer through the
///   search node. A tutored card put back on the library is a `LibraryTop` setup through that
///   node. Seek has no CR entry; it takes a random match and leaves the order alone, and a match
///   on top is still taken from the draw.
/// - A line sized by the AI's hand does not commute either (CR 701.9a): a line before the draw
///   reads a hand without the card that draw takes, and a line after it reads that card too,
///   with a draw after a clear (Magus of the Wheel), with none (Bomat Courier), or with a loss
///   counted by the hand (Castle Locthwain). It is `DrawSetup::HandClear`, read from any
///   quantity of the node or its cost that counts a hand, from its own cost modification when
///   that counts or tests a hand (Loreseeker's Stone), or from a mass move out of the hand. A
///   fixed count leaves the choice to the discarder, so a looter commutes, unless the hand can't
///   pay it (CR 609.3: Bazaar of Baghdad on an empty hand).
/// - A look or reveal whose card nothing then acts on (CR 701.20b, CR 701.20e). One whose card
///   is then acted on is a setup, because the draw changes which card that is. A later node may
///   move it or only read it: Iron Lad's draw and a Deceiver's pump are gated on the card. Either
///   way the authority can't see through the node, which names the card by anaphor (`target` is
///   `ParentTarget`, `origin` is `None`) or by a condition, so the look answers for it.
///
/// A scry and a shuffle with no search stay in their library, so the authority says no; both
/// still change what the draw takes.
fn draw_setup(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    chain: &[&AbilityDefinition],
    node: &AbilityDefinition,
) -> Option<DrawSetup> {
    // CR 602.1a: an activation cost is paid by the activator, so it is the AI's own library.
    // CR 118.12: a chain link's cost is paid as the ability resolves.
    if node
        .cost
        .as_ref()
        .is_some_and(AbilityCost::moves_card_to_or_from_library)
    {
        return Some(DrawSetup::LibraryTop);
    }
    let reaches = || node_can_reach_ai(state, ai_player, source_id, chain, node);
    let if_reaches = |setup| reaches().then_some(setup);
    let effect_setup = match &*node.effect {
        Effect::CreateDrawReplacement { .. } => Some(DrawSetup::Replacement),
        Effect::Draw { .. } | Effect::Connive { .. } | Effect::Learn => None,
        Effect::SearchLibrary { .. } | Effect::Seek { .. } => if_reaches(DrawSetup::Search),
        // The search node answers for its own instructions.
        Effect::ChangeZone {
            origin: Some(Zone::Library),
            destination,
            ..
        } if *destination != Zone::Library && chain_searches(chain) => None,
        Effect::Shuffle { .. } if chain_searches(chain) => None,
        Effect::RevealTop { .. }
        | Effect::Dig {
            destination: None,
            keep_count: Some(0),
            rest_destination: None,
            ..
        } if !chain_acts_on_a_looked_at_card(chain) => None,
        Effect::RevealTop { .. } | Effect::Scry { .. } | Effect::Shuffle { .. } => {
            if_reaches(DrawSetup::LibraryTop)
        }
        effect if effect.moves_card_to_or_from_library() => if_reaches(DrawSetup::LibraryTop),
        // After the library arm: a hand put into a library is already a `LibraryTop` setup.
        Effect::ChangeZoneAll {
            origin: Some(Zone::Hand),
            ..
        } => if_reaches(DrawSetup::HandClear),
        Effect::Discard { count, .. }
            if discard_outruns_hand(state, ai_player, source_id, chain, node, count) =>
        {
            if_reaches(DrawSetup::HandClear)
        }
        // Moves no card to or from a library; a reorder must be named above it.
        _ => None,
    };
    // CR 602.1a: an activation cost is paid by the activator, so it is the AI's own hand.
    let hand_cost = node.cost.as_ref().is_some_and(cost_reads_a_hand)
        || node
            .cost_reduction
            .as_ref()
            .is_some_and(cost_rider_reads_a_hand);
    effect_setup.or_else(|| {
        (hand_cost || effect_reads_a_hand(&node.effect) && reaches())
            .then_some(DrawSetup::HandClear)
    })
}

/// Does `qty` count a hand that can be the AI's? The draw step's draw changes that count.
/// An opponent's hand is left out: that draw doesn't touch it.
fn reads_a_hand(qty: &QuantityRef) -> bool {
    match qty {
        QuantityRef::HandSize { player } => !matches!(player, PlayerScope::Opponent { .. }),
        QuantityRef::ZoneCardCount {
            zone: ZoneRef::Hand,
            ..
        } => true,
        _ => false,
    }
}

/// Does a quantity of `effect` count that hand? The discard of "your hand" is `HandSize`, a
/// discard down to a count is a `Difference` from it (Magus of the Balance), and a loss can be
/// sized by it (Castle Locthwain). `for_each_quantity_expr` is exhaustive, so a new effect is
/// read without an arm here.
fn effect_reads_a_hand(effect: &Effect) -> bool {
    let mut reads = false;
    effect.for_each_quantity_expr(&mut |expr| reads |= expr.any_ref(&mut reads_a_hand));
    reads
}

/// [`effect_reads_a_hand`] for a cost. The walker recurses into `Composite` and `OneOf`.
fn cost_reads_a_hand(cost: &AbilityCost) -> bool {
    let mut reads = false;
    cost.for_each_quantity_expr(&mut |expr| reads |= expr.any_ref(&mut reads_a_hand));
    reads
}

/// CR 602.2b + CR 601.2f: does the ability's own cost modification count that hand, or test it?
/// The total cost is determined as it's activated, so the draw step's draw changes it: Loreseeker's
/// Stone costs {1} more for each card in hand, and the main phase pays more for the same line or
/// can't pay it at all.
fn cost_rider_reads_a_hand(rider: &CostReduction) -> bool {
    rider.count.any_ref(&mut reads_a_hand)
        || rider.condition.as_ref().is_some_and(condition_reads_a_hand)
}

/// Does `condition` test a hand that can be the AI's, in either direction? Unlike
/// [`condition_caps_hand`], a floor counts too: Arm-Mounted Anchor's equip costs {2} less with
/// one or fewer cards in hand.
fn condition_reads_a_hand(condition: &ParsedCondition) -> bool {
    match condition {
        ParsedCondition::QuantityComparison { lhs, rhs, .. } => {
            lhs.any_ref(&mut reads_a_hand) || rhs.any_ref(&mut reads_a_hand)
        }
        ParsedCondition::QuantityVsEachOpponent { lhs, rhs, .. } => {
            reads_a_hand(lhs) || reads_a_hand(rhs)
        }
        ParsedCondition::HandSizeExact { .. } | ParsedCondition::HandSizeOneOf { .. } => true,
        ParsedCondition::ZoneCardCountAtLeast { zone, .. }
        | ParsedCondition::ZoneCardTypeCountAtLeast { zone, .. }
        | ParsedCondition::ZoneCoreTypeCardCountAtLeast { zone, .. }
        | ParsedCondition::ZoneSubtypeCardCountAtLeast { zone, .. } => *zone == Zone::Hand,
        ParsedCondition::And { conditions } | ParsedCondition::Or { conditions } => {
            conditions.iter().any(condition_reads_a_hand)
        }
        ParsedCondition::Not { condition } => condition_reads_a_hand(condition),
        ParsedCondition::SourceInZone { .. }
        | ParsedCondition::SourceIsAttacking
        | ParsedCondition::SourceIsAttackingOrBlocking
        | ParsedCondition::SourceIsBlocked
        | ParsedCondition::SourcePowerAtLeast { .. }
        | ParsedCondition::SourceHasCounterAtLeast { .. }
        | ParsedCondition::SourceHasNoCounter { .. }
        | ParsedCondition::SourceEnteredThisTurn
        | ParsedCondition::SourceAttackedThisTurn
        | ParsedCondition::SourceIsCreature
        | ParsedCondition::SourceAttachedTo { .. }
        | ParsedCondition::SourceUntappedAttachedTo { .. }
        | ParsedCondition::SourceLacksKeyword { .. }
        | ParsedCondition::SourceIsColor { .. }
        | ParsedCondition::FirstSpellThisGame
        | ParsedCondition::OpponentSearchedLibraryThisTurn
        | ParsedCondition::BeenAttackedThisStep
        | ParsedCondition::OpponentPoisonAtLeast { .. }
        | ParsedCondition::CreaturesYouControlTotalPowerAtLeast { .. }
        | ParsedCondition::YouControlLandSubtypeAny { .. }
        | ParsedCondition::YouControlSubtypeCountAtLeast { .. }
        | ParsedCondition::YouControlCoreTypeCountAtLeast { .. }
        | ParsedCondition::YouControlColorPermanentCountAtLeast { .. }
        | ParsedCondition::YouControlSubtypeOrGraveyardCardSubtype { .. }
        | ParsedCondition::YouControlLegendaryCreature
        | ParsedCondition::YouControlNamedPlaneswalker { .. }
        | ParsedCondition::ControlsCreatureWithKeyword { .. }
        | ParsedCondition::YouControlCreatureWithPowerAtLeast { .. }
        | ParsedCondition::YouControlCreatureWithPt { .. }
        | ParsedCondition::YouControlAnotherColorlessCreature
        | ParsedCondition::YouControlSnowPermanentCountAtLeast { .. }
        | ParsedCondition::YouControlDifferentPowerCreatureCountAtLeast { .. }
        | ParsedCondition::YouControlLandsWithSameNameAtLeast { .. }
        | ParsedCondition::YouControlNoCreatures
        | ParsedCondition::YouAttackedThisTurn
        | ParsedCondition::YouAttackedSourceControllerThisTurn
        | ParsedCondition::YouAttackedWithAtLeast { .. }
        | ParsedCondition::YouPlayedLandThisTurn
        | ParsedCondition::YouCastSpellThisTurn { .. }
        | ParsedCondition::YouCastNoncreatureSpellThisTurn
        | ParsedCondition::YouCastSpellCountAtLeast { .. }
        | ParsedCondition::YouGainedLifeThisTurn
        | ParsedCondition::YouCreatedTokenThisTurn
        | ParsedCondition::YouDiscardedCardThisTurn
        | ParsedCondition::YouSacrificedArtifactThisTurn
        | ParsedCondition::CreatureDiedThisTurn
        | ParsedCondition::YouHadCreatureEnterThisTurn
        | ParsedCondition::YouHadAngelOrBerserkerEnterThisTurn
        | ParsedCondition::YouHadArtifactEnterThisTurn
        | ParsedCondition::BattlefieldEntriesThisTurn { .. }
        | ParsedCondition::CardsLeftYourGraveyardThisTurnAtLeast { .. }
        | ParsedCondition::PlayerCountAtLeast { .. }
        | ParsedCondition::HasCityBlessing
        | ParsedCondition::HasEnduringStory
        | ParsedCondition::CompletedDungeon { .. }
        | ParsedCondition::HasMaxSpeed
        | ParsedCondition::IsYourTurn
        | ParsedCondition::IsOpponentsTurn
        | ParsedCondition::CurrentPhaseIs { .. }
        | ParsedCondition::SpellTargetsFilter { .. }
        | ParsedCondition::ControlsCommander { .. } => false,
    }
}

/// CR 609.3 + CR 701.9a: can this fixed discard take more than the hand holds when it resolves?
/// It then does only as much as possible, and a line before the draw keeps the card that draw
/// takes (Bazaar of Baghdad on an empty hand). The hand is today's without the source, which has
/// left it by then if it's a spell (CR 601.2a: Faithless Salvaging, the last card in hand), plus
/// the fixed draws before this node that can be the AI's. A dynamic draw counts as 0, which errs
/// toward not lending, as does a source that stays in hand.
fn discard_outruns_hand(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    chain: &[&AbilityDefinition],
    node: &AbilityDefinition,
    count: &QuantityExpr,
) -> bool {
    let QuantityExpr::Fixed { value } = count else {
        return false;
    };
    let hand = state
        .players
        .iter()
        .find(|player| player.id == ai_player)
        .map_or(0, |player| {
            player.hand.iter().filter(|id| **id != source_id).count()
        }) as i32;
    let drawn: i32 = chain
        .iter()
        .take_while(|earlier| !std::ptr::eq(**earlier, node))
        .filter_map(|earlier| match &*earlier.effect {
            Effect::Draw {
                count: QuantityExpr::Fixed { value },
                ..
            } if node_can_reach_ai(state, ai_player, source_id, chain, earlier) => Some(*value),
            _ => None,
        })
        .sum();
    hand + drawn < *value
}

/// Does `chain` search a library (CR 701.23a)?
fn chain_searches(chain: &[&AbilityDefinition]) -> bool {
    chain
        .iter()
        .any(|node| matches!(*node.effect, Effect::SearchLibrary { .. }))
}

/// Does `chain` act on a card, so that a look or reveal at the top of a library can be the first
/// half of moving it or reading it? Any node can, except another look or reveal and a delayed
/// trigger, which acts later (Mishra's Bauble's draw is next turn's).
fn chain_acts_on_a_looked_at_card(chain: &[&AbilityDefinition]) -> bool {
    chain.iter().any(|node| {
        !matches!(
            *node.effect,
            Effect::RevealTop { .. } | Effect::Dig { .. } | Effect::CreateDelayedTrigger { .. }
        )
    })
}

/// Can `node` act on a library or a hand that is `ai_player`'s own?
///
/// `player_scope` comes first: "each opponent mills" is `Mill { target: Controller }` under
/// `player_scope: Opponent`, and each scoped player becomes the acting player, so its slot
/// names nobody yet. Otherwise the node's slot answers (`slot_can_name`), and a node with no
/// slot counts as able to reach the AI: most act on their controller's library, and
/// `ChangeZoneAll`, which surfaces none, fails closed (Jester's Mask). A card slot names no
/// player, so it counts too: that card's owner may be the AI (Academy Ruins, Reito Lantern).
fn node_can_reach_ai(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    chain: &[&AbilityDefinition],
    node: &AbilityDefinition,
) -> bool {
    if let Some(scope) = &node.player_scope {
        return can_name_ai(state, ai_player, |player| {
            engine::game::effects::matches_player_scope(state, player, scope, ai_player, source_id)
        });
    }
    let slot = match &*node.effect {
        // `target_filter` surfaces a reveal's player only for `Player`, so read the field itself.
        Effect::RevealTop { player, .. } => Some(player),
        effect => effect.target_filter(),
    };
    // CR 608.2c: "that player" names the controller of the target an earlier instruction chose.
    // CR 608.2b: a target an opponent must control is illegal under the AI's control, so that
    // player can't be the AI.
    if slot == Some(&TargetFilter::ParentTargetController)
        && filter_is_opponent_scoped(parent_slot(chain, node))
    {
        return false;
    }
    slot.is_none_or(|slot| slot_can_name(state, ai_player, source_id, slot))
}

/// The slot of the nearest node before `node` in `chain` that has one.
fn parent_slot<'a>(
    chain: &[&'a AbilityDefinition],
    node: &AbilityDefinition,
) -> Option<&'a TargetFilter> {
    chain
        .iter()
        .take_while(|earlier| !std::ptr::eq(**earlier, node))
        .filter_map(|earlier| earlier.effect.target_filter())
        .last()
}

/// Can the player slot `filter` of an ability `ai_player` activates from `source_id` name
/// `ai_player`, so that the change can be to the AI's own library?
///
/// The engine's player-filter authority answers the question. CR 109.5 + CR 602.2a: "you" on an
/// activated ability is the player who activated it, here the AI, so it is the source controller
/// the filter is read against. `Controller` names the AI, and `Player` ("target player", CR 115.1)
/// admits it as a legal choice, as does any filter the authority matches the AI with.
///
/// The authority fails closed on a filter it cannot answer without ability context (a parent
/// target's controller, a chosen or triggering player): it then matches no player at all. Such a
/// slot still names some player when the ability resolves, and that player may be the AI, so a
/// filter matching no living player counts as able to name it. Only a filter the authority
/// answers with other players but not the AI, such as "target opponent's library", cannot.
fn slot_can_name(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    filter: &TargetFilter,
) -> bool {
    can_name_ai(state, ai_player, |player| {
        engine::game::filter::player_matches_target_filter_in_state(
            state,
            filter,
            player,
            Some(ai_player),
            Some(source_id),
        )
    })
}

/// Does `matches` admit `ai_player`, or fail closed by matching no living player at all?
fn can_name_ai(state: &GameState, ai_player: PlayerId, matches: impl Fn(PlayerId) -> bool) -> bool {
    matches(ai_player)
        || !state
            .players
            .iter()
            .filter(|player| !player.is_eliminated)
            .any(|player| matches(player.id))
}

/// The AI's own windows before its precombat main phase in which it holds priority with an empty
/// stack, and why waiting for the main phase costs nothing there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PatientWindow {
    /// CR 503.1: the upkeep step has no turn-based actions of its own, so taken with an empty
    /// stack it is the turn's low-information window: nothing has been drawn, cast, or declared.
    Upkeep,
    /// The draw step (CR 504.2: its priority comes after the draw) or an upkeep whose triggers
    /// have resolved: waiting buys no information, but it gives nothing up either, provided
    /// nothing happens as the main phase begins (see [`main_phase_opens_with_events`]) or, from
    /// the upkeep, as the draw step begins (see [`patient_window`]). Every
    /// instant-speed use of the ability is still on offer in the main phase, and the mana and
    /// untapped permanents it would spend now are still there for the sorcery-speed plays
    /// (CR 307.1) that only the main phase allows.
    BeforeMain,
}

/// The [`PatientWindow`] `ai_player` holds priority in, if any.
///
/// The untap step is excluded because CR 502.4 means no player ever receives priority in it, so
/// `WaitingFor::Priority` cannot occur there. The end step is the *patient* window itself, the
/// one a held ability is being saved for (`FetchLandPatiencePolicy` treats it the same way).
///
/// # Why this must be `ai_player`'s OWN turn
///
/// CR 117.3d passes priority to the next player in turn order
/// (`crates/engine/src/game/priority.rs`'s `priority_pass_participants`), not
/// just the active player — so the AI can hold `WaitingFor::Priority` with an
/// empty stack during an OPPONENT's upkeep or draw step too. During an opponent's turn the AI's
/// next draw and main phase are no closer for having waited, and deferring costs something
/// real: this priority window is the AI's chance to act BEFORE the opponent's draw and
/// combat, and passing it up trades away a live interaction window for nothing.
/// `state.active_player == ai_player` is therefore a hard requirement, not a refinement.
///
/// # Why no beginning-of-upkeep trigger may reach `ai_player`'s upkeep
///
/// CR 503.1a: "at the beginning of your upkeep" triggers are put on the stack
/// BEFORE the active player ever receives priority for the step — and once
/// they're on the stack, `state.stack` is non-empty, so THIS predicate does
/// not fire yet (correctly: a live stack is `StackResponse` territory, not a
/// quiet window). But every one of those triggers must still be responded to
/// and resolved before the stack empties again, and priority then returns to
/// exactly the same shape this predicate matches — `Phase::Upkeep`, empty
/// stack, `Priority` — this time AFTER something already happened. Left
/// unguarded, that second window reads as identically "nothing has happened
/// yet" as the pristine first one, even when the resolved trigger drew a
/// card (Phyrexian Arena), scried, or revealed information. Excluding it
/// whenever ANY such trigger fires in `ai_player`'s upkeep (its own "your" triggers and
/// anyone's "each player's" triggers) is deliberately
/// coarse rather than trying to classify which triggers are
/// information-producing: CR 503.1a guarantees such a trigger is ALREADY
/// queued (and, by the time the stack is next empty, already resolved)
/// before the truly first grant, so the pristine window this predicate wants
/// is provably unreachable whenever one exists — there is no non-conservative
/// case being given up. Such an upkeep is still a [`PatientWindow::BeforeMain`], whose case
/// never rested on information.
///
/// # Why, from the upkeep, no beginning-of-draw-step trigger may reach `ai_player`'s draw step
///
/// A hold's lend, and every hold but a search's or a hand clear's, rest on activating in the main
/// phase reproducing it. A trigger at the beginning of the AI's draw step lies between the upkeep and that main
/// phase. An intervening-if checked as the step begins (CR 603.4), or anything else the trigger
/// reads, may depend on an activation made now. So any such trigger stands the whole upkeep
/// down, pristine or not ([`phase_opens_with_triggers`]: the AI's own "your" triggers, anyone's
/// "each player's" triggers, an opponent's "each opponent's" triggers, and delayed triggers set
/// for it). Like the upkeep check, this is
/// deliberately not gated on the trigger functioning or on its condition. The draw step itself
/// is unaffected: by its priority, its triggers and its draw are behind it (CR 504.2,
/// CR 117.3a).
fn patient_window(state: &GameState, ai_player: PlayerId) -> Option<PatientWindow> {
    if state.active_player != ai_player
        || !state.stack.is_empty()
        || !matches!(state.waiting_for, WaitingFor::Priority { .. })
    {
        return None;
    }
    // A main phase that opens with events stands both windows down: even a pristine upkeep can
    // run straight into it when the draw step is skipped (CR 614.10).
    if !matches!(state.phase, Phase::Upkeep | Phase::Draw)
        || main_phase_opens_with_events(state, ai_player)
    {
        return None;
    }
    // CR 504.1 + CR 117.3a + CR 603.4: from the upkeep the draw step is still ahead, and a
    // trigger at its beginning checks its intervening-if as the step begins, so it can read
    // what an activation now changes (Voltaic Key untapping a tapped Howling Mine). Activating
    // in the main phase cannot reach it, so waiting is not free: this upkeep, pristine or not,
    // is not a patient window.
    if state.phase == Phase::Upkeep && phase_opens_with_triggers(state, ai_player, Phase::Draw) {
        return None;
    }
    if state.phase == Phase::Upkeep
        && !beginning_of_phase_trigger_reaches(state, ai_player, Phase::Upkeep)
    {
        Some(PatientWindow::Upkeep)
    } else {
        Some(PatientWindow::BeforeMain)
    }
}

/// Does `player`'s next `phase` open with a trigger, which goes on the stack before they
/// receive priority in it (CR 117.3a)? Either a printed "at the beginning of [phase]" trigger
/// that reaches it ([`beginning_of_phase_trigger_reaches`]) or a delayed trigger set for it
/// (CR 603.7: Mana Drain's "at the beginning of your next main phase"). Coarse in the safe
/// direction, like the printed scan: neither arm reads whether the trigger will do anything.
fn phase_opens_with_triggers(state: &GameState, player: PlayerId, phase: Phase) -> bool {
    beginning_of_phase_trigger_reaches(state, player, phase)
        || state
            .delayed_triggers
            .iter()
            .any(|delayed| match delayed.condition {
                DelayedTriggerCondition::AtNextPhase { phase: at } => at == phase,
                DelayedTriggerCondition::AtNextPhaseForPlayer {
                    phase: at,
                    player: for_player,
                    ..
                } => at == phase && for_player == player,
                DelayedTriggerCondition::AtBeginningOfAddedPhase { .. }
                | DelayedTriggerCondition::WhenLeavesPlay { .. }
                | DelayedTriggerCondition::WhenDies { .. }
                | DelayedTriggerCondition::WhenLeavesPlayFiltered { .. }
                | DelayedTriggerCondition::WhenEntersBattlefield { .. }
                | DelayedTriggerCondition::WhenDiesOrExiled { .. }
                | DelayedTriggerCondition::WheneverEvent { .. }
                | DelayedTriggerCondition::WhenNextEvent { .. } => false,
            })
}

/// Does `player`'s precombat main phase open with something that happens before they receive
/// priority in it, which an activation now could still change? CR 505.3: they are the archenemy
/// and set a scheme in motion. CR 505.4: they put a lore counter on each of their Sagas.
/// CR 505.5: they roll to visit their Attractions. CR 117.3a: "at the beginning of your
/// precombat main phase" triggers go on the stack, and so do CR 603.7 delayed triggers set for
/// it ([`phase_opens_with_triggers`]).
/// Coarse in the safe direction, like
/// [`beginning_of_phase_trigger_reaches`]: any such permanent stands the window down. Triggers
/// worded "at the beginning of each of your main phases" (parsed without a phase) are not
/// counted; today they only add mana, which the main phase does not need earlier.
///
/// Sagas are read the way `turns::add_lore_counters_to_sagas` reads them, and Attractions
/// through the engine's own `is_attraction_permanent`.
fn main_phase_opens_with_events(state: &GameState, player: PlayerId) -> bool {
    state.archenemy == Some(player)
        || phase_opens_with_triggers(state, player, Phase::PreCombatMain)
        || state.battlefield.iter().any(|&id| {
            state.objects.get(&id).is_some_and(|object| {
                object.controller == player
                    && (object.card_types.subtypes.iter().any(|s| s == "Saga")
                        || attractions::is_attraction_permanent(object))
            })
        })
}

/// Does a printed "at the beginning of [phase]" trigger that fires on `player`'s turn sit on any
/// battlefield permanent (CR 503.1a for the upkeep, CR 117.3a in general)? A "your" trigger counts
/// for its controller only; an "each player's" trigger (Blinkmoth Urn) counts for every player,
/// whoever controls it, through the engine's `phase_trigger_scoped_to_turn_of`. Deliberately NOT
/// gated on the trigger currently functioning or on its intervening-if (CR 603.4: an activation
/// now may be what makes it true) — `Definitions::iter_unchecked` is the sanctioned
/// classification-only accessor (engine runtime paths use the CR-gated `functioning_*` helpers
/// instead; this is neither, so a source under a static suppression still counts). That is the
/// safe direction: this predicate exists only to EXCLUDE a window from patience-gating, so
/// over-including a source that could not actually have fired only means the policy declines to
/// nudge a few boards it safely could have — never the reverse.
fn beginning_of_phase_trigger_reaches(state: &GameState, player: PlayerId, phase: Phase) -> bool {
    state.battlefield.iter().any(|&id| {
        state.objects.get(&id).is_some_and(|object| {
            object.trigger_definitions.iter_unchecked().any(|entry| {
                let def = entry.definition();
                def.mode == TriggerMode::Phase
                    && def.phase == Some(phase)
                    && engine::game::triggers::phase_trigger_scoped_to_turn_of(
                        state,
                        def,
                        object.controller,
                        player,
                    )
            })
        })
    })
}

/// Does this activation's own damage end the game now? CR 104.3b — a player at 0
/// or less life loses, so damage that reaches a live opponent's life total is
/// never worth deferring.
///
/// A lethal intent alone does not make an activation part of the line: the line
/// is still there in the precombat main phase, before any combat it depends on.
fn is_lethal_line(ctx: &PolicyContext<'_>) -> bool {
    let Some(damage) = extract_damage_amount(&ctx.effects()) else {
        return false;
    };
    players::opponents(ctx.state, ctx.ai_player)
        .iter()
        .any(|&opponent| ctx.state.players[opponent.0 as usize].life <= damage)
}

impl TacticalPolicy for ActivationPatiencePolicy {
    fn id(&self) -> PolicyId {
        PolicyId::ActivationPatience
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::ActivateAbility]
    }

    fn activation(
        &self,
        features: &DeckFeatures,
        state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        super::activation::turn_only(features, state)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let na = || PolicyVerdict::neutral(PolicyReason::new("activation_patience_na"));

        let GameAction::ActivateAbility {
            source_id,
            ability_index,
            ..
        } = &ctx.candidate.action
        else {
            return na();
        };

        let Some(window) = patient_window(ctx.state, ctx.ai_player) else {
            return na();
        };

        if !activation_is_deferrable(ctx.state, *source_id, *ability_index) {
            return na();
        }

        // Escape hatch 1 — CR 500.5: mana in the pool empties as this step ends, so
        // when this activation may spend it (Thran Turbine's upkeep {C}{C}, which can't
        // be spent on spells) waiting can destroy it rather than save it for the main
        // phase. The engine answers eligibility only; whether this cost would actually
        // use the mana is left to the payment pipeline, and the hatch errs toward acting.
        if engine::game::casting::pool_has_expiring_mana_for_activation(
            ctx.state,
            ctx.ai_player,
            *source_id,
            *ability_index,
        ) {
            return PolicyVerdict::neutral(PolicyReason::new("activation_patience_floating_mana"));
        }

        // Escape hatch 2 — the draw-step instance of the rule in the module doc ("What a hold
        // rests on"): CR 504.1 + CR 121.1: the draw step's turn-based draw takes the top card
        // of the library, and in the upkeep it is still ahead (CR 502.4: no priority in the untap
        // step; CR 503.1: the upkeep's is the last before that draw). An activation that sets up
        // that draw gives its payoff up by waiting for the main phase: a draw replacement must
        // exist before the draw it replaces (CR 614.4), an arrangement of the top of the AI's
        // own library chooses the card that draw takes (CR 701.22a, CR 701.25a, CR 701.20e), and
        // any other change to that library changes the card it takes. So does one whose player
        // slot the AI may fill with itself, such as Elemental Augury's "target player's library"
        // (CR 115.1, `slot_can_name`): the line that names the AI arranges this draw, which
        // passing cannot. A change made later changes a later draw, so waiting loses the value
        // of changing this one whatever the ability costs: one that taps its source or is capped
        // (CR 602.5b) is exempt as much as one paid in mana alone. Whether spending the tap or
        // the capped activation on this draw is worth it is a resource question for the search's
        // valuation, not a timing question this policy answers. Every branch is read: the line
        // the search takes through a mode is the one passing does not reproduce. A search and a
        // hand clear don't commute with that draw either, and are held all the same; see
        // `DrawSetup`. An activation the draw would make illegal stands down first (CR 602.5).
        let setup = upkeep_draw_setup(ctx.state, ctx.ai_player, *source_id, *ability_index);
        match setup {
            Some(DrawSetup::HandBound) => {
                return PolicyVerdict::neutral(PolicyReason::new("activation_patience_hand_bound"));
            }
            Some(DrawSetup::Replacement) => {
                return PolicyVerdict::neutral(PolicyReason::new(
                    "activation_patience_draw_replacement",
                ));
            }
            Some(DrawSetup::LibraryTop) => {
                return PolicyVerdict::neutral(PolicyReason::new(
                    "activation_patience_library_top",
                ));
            }
            Some(DrawSetup::Search | DrawSetup::HandClear) | None => {}
        }

        // Escape hatch 3 — a lethal line beats any information the AI could buy
        // by waiting.
        if is_lethal_line(ctx) {
            return PolicyVerdict::neutral(PolicyReason::new("activation_patience_lethal_line"));
        }

        // Escape hatch 4 — CR 608.2c: `ability.condition` on an ACTIVATED
        // ability (as opposed to a triggered ability's CR 603.4 intervening-if)
        // gates the PAYOFF at resolution, not the activation itself — CR 602.5
        // has no bearing here; per the Shelldock Isle ruling `condition_gated_
        // activation.rs`'s own module doc already cites, activating is legal
        // regardless of whether the condition holds (the effect simply does
        // nothing if it's false at resolution). What makes patience wrong here
        // is narrower: a condition that's TRUE now may not still be true after
        // waiting, so deferring risks losing a real payoff for a purely
        // informational gain. `ConditionGatedActivationPolicy` is this hatch's
        // mirror image — it prices activating while the condition is FALSE.
        if ctx
            .effective_activated_ability()
            .is_some_and(|ability| ability.condition.is_some())
        {
            return PolicyVerdict::neutral(PolicyReason::new("activation_patience_conditional"));
        }

        let patience = ctx.config.profile.interaction_patience;
        PolicyVerdict::score(
            -PATIENCE_PENALTY_BASE * (0.5 + patience),
            PolicyReason::new(match (setup, window) {
                (Some(DrawSetup::Search | DrawSetup::HandClear), _) => {
                    "activation_patience_hold_unlent"
                }
                (_, PatientWindow::Upkeep) => "activation_patience_hold",
                (_, PatientWindow::BeforeMain) => "activation_patience_hold_for_main",
            })
            .with_fact("phase", ctx.state.phase as i64),
        )
    }
}
