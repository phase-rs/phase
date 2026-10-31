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
//! Holding an activation, and lending its line to passing (`search::lend_deferral_witness`),
//! rest on one premise: activating in the main phase reproduces it, because nothing between
//! this priority and the main phase can depend on it. Every stand-down and every draw-setup
//! escape hatch below is an instance of that rule. From the upkeep, these lie in between:
//!
//! - The draw step's beginning triggers (CR 117.3a). An intervening-if is checked as the step
//!   begins (CR 603.4), so the activation may be what makes it true: Voltaic Key untapping a
//!   tapped Howling Mine. Any trigger that reaches the AI's draw step (its own "your"
//!   triggers, anyone's "each player's", an opponent's "each opponent's", and delayed
//!   triggers set for it, CR 603.7) stands the whole upkeep down (`patient_window`).
//! - The draw step's draw (CR 504.1 + CR 121.1). An activation that replaces it (CR 614.4) or
//!   changes what it takes from a library that can be the AI's own stands itself down
//!   (`DrawSetup`, escape hatch 2).
//! - The main phase's opening events (CR 505.3-505.5, CR 117.3a), which stand the upkeep and
//!   the draw step down (`main_phase_opens_with_events`).
//!
//! From the draw step only the last of these remains: its draw and its triggers are behind its
//! priority (CR 504.2). The upkeep's own triggers resolve before its stack is next empty, so
//! they only end its claim to being the low-information window (`PatientWindow::BeforeMain`).
//!
//! Four cases of the rule are not yet classified. A search and its shuffle stay held and lent,
//! though nothing in the rule exempts them: with the order hidden (CR 401.2) and randomized
//! (CR 701.24a) the draw takes a random card on either side of a fetch, and choosing the land
//! after the draw is what the hold is for. It fails when the AI knows its top card, which a
//! shuffle then replaces, and the lent value is not literally Pass's: with no determinization
//! the fetch line's draw is one concrete post-shuffle card, not the top card Pass would draw.
//! And the trigger scans read only permanents already on the
//! battlefield, so a trigger whose source the held activation itself puts onto the battlefield
//! is not seen: Quicksilver Amulet putting Avaricious Dragon onto the battlefield in the upkeep,
//! and likewise for the main-phase stand-down. And the draw step's draw itself triggers
//! "whenever you draw a card" abilities (CR 603.2), whose intervening-if (CR 603.4) an upkeep
//! activation may make true: Starving Revenant's descend 8 after an upkeep sacrifice. Only
//! beginning-of-step triggers are scanned. And the classification does not read inside an
//! inline branch carrier (a die roll's results) or a node the parser left `Unimplemented`.
//!
//! The classification errs toward standing down, so it also releases activations that could
//! have waited: a card put on the bottom changes this draw only on an empty library
//! (CR 704.5b), a "target player" slot, a card slot, a tracked set, an anaphoric player or a
//! node with no slot is read as able to name
//! the AI even when the line names an opponent, and a blind mill or exile is equal in
//! distribution on either side of the draw while the order is hidden. Those lose the hold, and
//! the donor's line still sees a draw that Pass's does not, so the search may now prefer
//! activating. For a blind mill that preference reads the real library order (no
//! determinization ships).
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
//! divided by that weight.
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
//! own arm beside `Scry`. In the draw step that draw is behind (CR 504.2), and any such change is
//! held like any other activation. The report names the other two times to activate early: when the
//! damage is part of a lethal line, and (implicitly) when the ability's own condition is met *now* and might
//! not be later. Those are lethal damage and `ability.condition`, in that order
//! below. Its third exception, a source that "is going to die or leave play
//! before the AI draws a card", has no reading in these windows: both are the
//! AI's own turn with an empty stack, so nothing is targeting the source and no
//! opponent attacks before the main phase.

use engine::game::{attractions, mana_abilities};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityTag, ActivationRestriction, DelayedTriggerCondition,
    Effect, TargetFilter,
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
        // CR 602.5: an ability restricted to "your upkeep" can't be activated in the main
        // phase, so waiting would give it up.
        && !ability
            .activation_restrictions
            .contains(&ActivationRestriction::DuringYourUpkeep)
}

/// What an upkeep activation does to the draw step's coming draw (CR 504.1 + CR 121.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrawSetup {
    /// CR 614.4: a replacement for that draw must exist before it.
    Replacement,
    /// CR 121.1: a change to a library that can be the AI's own, after which that draw takes a
    /// different card or a chosen one: an arrangement of its top (CR 701.22a, CR 701.25a,
    /// CR 701.20e), a card put on it, or cards taken from it (CR 701.17a).
    LibraryTop,
}

/// What `node`, of an ability `ai_player` activates from `source_id`, does to the draw step's
/// coming draw. `chain` is every node of that ability.
///
/// The question is whether the node commutes with that draw: does activating before it leave
/// the game activating after it would? Fail-closed: anything the engine's wildcard-free library
/// authorities call a move to or from a library is a setup, so a new effect that makes such a
/// move is one
/// until it is named below. What is named commutes with the draw, except a search, which stays
/// held for the reason given there:
///
/// - A draw (CR 121.1): this draw and that one take the same two cards, in either order. So do
///   connive and learn (CR 701.50a, CR 701.48a), which draw. A chain that also puts a card
///   back is a setup through that other node (Sensei's Divining Top, Brainstone).
/// - A search and its own instructions (CR 701.23a): the order is hidden (CR 401.2) and the
///   shuffle randomizes it (CR 701.24a), so the draw takes a random card either way, which is
///   no peek at the real top. It does not commute (the fetched card has left the pool); it
///   stays held because its payoff is a land or card better chosen after the draw. See "What a
///   hold rests on". A tutored card put back on the library is a setup. Seek has no CR entry;
///   it takes a random match and leaves the order alone.
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
    let setup_if_reaches = || {
        node_can_reach_ai_library(state, ai_player, source_id, node)
            .then_some(DrawSetup::LibraryTop)
    };
    match &*node.effect {
        Effect::CreateDrawReplacement { .. } => Some(DrawSetup::Replacement),
        Effect::Draw { .. } | Effect::Connive { .. } | Effect::Learn => None,
        Effect::SearchLibrary { .. } | Effect::Seek { .. } => None,
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
            setup_if_reaches()
        }
        effect if effect.moves_card_to_or_from_library() => setup_if_reaches(),
        // Moves no card to or from a library; a reorder must be named above it.
        _ => None,
    }
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

/// Can `node` act on a library that is `ai_player`'s own?
///
/// `player_scope` comes first: "each opponent mills" is `Mill { target: Controller }` under
/// `player_scope: Opponent`, and each scoped player becomes the acting player, so its slot
/// names nobody yet. Otherwise the node's slot answers (`slot_can_name`), and a node with no
/// slot counts as able to reach the AI: most act on their controller's library, and
/// `ChangeZoneAll`, which surfaces none, fails closed (Jester's Mask). A card slot names no
/// player, so it counts too: that card's owner may be the AI (Academy Ruins, Reito Lantern).
fn node_can_reach_ai_library(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
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
    slot.is_none_or(|slot| slot_can_name(state, ai_player, source_id, slot))
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
/// A hold, and the line it lends to passing, rest on activating in the main phase reproducing
/// it. A trigger at the beginning of the AI's draw step lies between the upkeep and that main
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
        // the search takes through a mode is the one passing does not reproduce.
        if ctx.state.phase == Phase::Upkeep {
            let chain = printed_ability(ctx.state, *source_id, *ability_index)
                .map(|ability| collect_scoped_nodes(ability, AbilityScope::Potential))
                .unwrap_or_default();
            let sets_up = |setup: DrawSetup| {
                chain.iter().any(|node| {
                    draw_setup(ctx.state, ctx.ai_player, *source_id, &chain, node) == Some(setup)
                })
            };
            if sets_up(DrawSetup::Replacement) {
                return PolicyVerdict::neutral(PolicyReason::new(
                    "activation_patience_draw_replacement",
                ));
            }
            if sets_up(DrawSetup::LibraryTop) {
                return PolicyVerdict::neutral(PolicyReason::new(
                    "activation_patience_library_top",
                ));
            }
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
            PolicyReason::new(match window {
                PatientWindow::Upkeep => "activation_patience_hold",
                PatientWindow::BeforeMain => "activation_patience_hold_for_main",
            })
            .with_fact("phase", ctx.state.phase as i64),
        )
    }
}
