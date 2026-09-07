//! CR 717 + CR 701.51 + CR 701.52: Unfinity Attraction deck, open, and visit.
//!
//! Attractions live in a supplementary deck (command zone) tracked per player via
//! `Player::attraction_deck`. Opening moves the top card to the battlefield; rolling
//! to visit is a turn-based action at the beginning of the active player's precombat
//! main phase when they control an Attraction.

use crate::types::ability::{DieRollIgnoreRule, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::resolution::{DieRollContinuation, PendingDieRollInstruction};
use crate::types::zones::Zone;

use super::effects::roll_die;
use super::game_object::GameObject;
use crate::types::ability::EffectError;

/// CR 717.1: Default lit numbers when card data omits variant lights (1 and 6 are always lit).
pub fn default_attraction_lights() -> Vec<u8> {
    vec![1, 6]
}

pub fn is_attraction_card(obj: &GameObject) -> bool {
    obj.in_attraction_deck
        || !obj.attraction_lights.is_empty()
        || obj
            .card_types
            .subtypes
            .iter()
            .any(|s| s.eq_ignore_ascii_case("Attraction"))
}

pub fn is_attraction_permanent(obj: &GameObject) -> bool {
    obj.zone == Zone::Battlefield && is_attraction_card(obj)
}

/// CR 701.51b: Put the top card of the controller's Attraction deck onto the battlefield.
pub fn open_attractions(
    state: &mut GameState,
    player: PlayerId,
    count: u32,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    for opened in 0..count {
        let Some(object_id) = state
            .players
            .iter_mut()
            .find(|p| p.id == player)
            .and_then(|p| p.attraction_deck.pop_front())
        else {
            // CR 609.3: If the player has fewer Attractions than requested, open
            // as many as possible and ignore the impossible remainder.
            break;
        };
        // CR 614.1c: route the Attraction's battlefield entry through the
        // zone-change pipeline so the delivery tail applies enters-with-counters
        // statics (e.g. an artifact-scoped "enters with an additional counter"
        // static) — the raw `move_to_zone` skipped that tail, so an opened
        // Attraction never received them. CR 400.7 attributes the entry to the
        // opened object itself (the pre-pipeline raw move recorded no source).
        //
        // CR 616.1: a battlefield-entry pause IS reachable here — two co-played
        // external `Moved` effects can write the entry event's tap field in
        // *opposite* directions (a "enters tapped" Frozen Aether class effect +
        // a "enters untapped" Spelunking / Archelos class effect), a material
        // same-field collision (last-applied-wins) that surfaces an ordering
        // prompt. (Two same-direction writes are idempotent and commute without
        // a prompt — see replacement.rs `CommuteClass::EnterTapped`/`EnterUntapped`.)
        // On the pause, the paused Attraction's open bookkeeping and
        // the REMAINING opens of this instruction are deferred onto a
        // `BatchCompletion::AttractionOpenRemainder` so the replacement-choice
        // resume runs them — the old bail `break` left `in_attraction_deck`
        // set, never emitted `AttractionOpened`, and dropped the remaining
        // opens.
        match super::zone_pipeline::move_object(
            state,
            super::zone_pipeline::ZoneMoveRequest::effect(object_id, Zone::Battlefield, object_id),
            events,
        ) {
            super::zone_pipeline::ZoneMoveResult::Done => {}
            super::zone_pipeline::ZoneMoveResult::NeedsChoice(_)
            | super::zone_pipeline::ZoneMoveResult::NeedsAuraAttachmentChoice => {
                super::zone_pipeline::defer_completion_on_pause(
                    state,
                    crate::types::game_state::BatchCompletion::AttractionOpenRemainder {
                        player,
                        object_id,
                        remaining: count - opened - 1,
                    },
                );
                return Ok(());
            }
        }
        finish_attraction_open(state, player, object_id, events);
    }
    Ok(())
}

/// CR 701.51b + CR 701.51c: Per-Attraction open bookkeeping, run exactly once
/// after the Attraction's battlefield entry delivers — inline on the
/// synchronous path, or from `BatchCompletion::AttractionOpenRemainder` when
/// the entry parked on a CR 616.1 replacement-ordering choice and resumed.
/// Clears the supplementary-deck membership flag and emits `AttractionOpened`
/// (the "whenever a player opens an Attraction" trigger event, which fires only
/// when the card actually entered the battlefield — CR 701.51c).
pub(crate) fn finish_attraction_open(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    events: &mut Vec<GameEvent>,
) {
    if let Some(obj) = state.objects.get_mut(&object_id) {
        obj.in_attraction_deck = false;
    }
    // CR 701.51c: the "opens an Attraction" trigger fires only when the card
    // actually entered the battlefield — "If an effect prevents that Attraction
    // from entering the battlefield or replaces entering the battlefield with
    // another event, that ability doesn't trigger." `ZoneMoveResult::Done`
    // also covers prevented/redirected deliveries, so gate on arrival.
    if state
        .objects
        .get(&object_id)
        .is_some_and(|obj| obj.zone == Zone::Battlefield)
    {
        events.push(GameEvent::AttractionOpened {
            player_id: player,
            object_id,
        });
    }
}

/// CR 706.6 + CR 703.4g: Which roll (if any) the roll-to-visit turn-based action
/// ignores, decided WITHOUT prompting the roller.
///
/// The roll-to-visit is a turn-based action (CR 703.4g) with no continuation
/// frame to suspend into, so this path cannot open
/// `WaitingFor::DieKeepChoice`. That makes a deterministic pick safe only for
/// the tied-extreme rules, so the rule is matched rather than blindly taking the
/// first candidate index:
///
/// - `Lowest` (and any future tied-extreme rule): `ignorable_indices` returns
///   only indices tied at ONE extreme, so every candidate holds the SAME value.
///   CR 706.6 removes the ROLL and CR 701.52a decides visits per RESULT, so
///   dropping either of two equal rolls yields an identical set of visits — the
///   auto-pick cannot change any observable outcome.
///
/// `DieRollIgnoreRule` has exactly one leaf today, and it is a tied-extreme
/// rule, so the auto-pick above is always safe here. A future FREE-choice rule
/// ("ignore one", where every remaining roll is a legal pick with a DIFFERENT
/// value) would NOT be: any pick the engine made would silently take a decision
/// away from the roller, and a different pick visits different Attractions
/// (CR 701.52a). This path has no way to prompt, so such a rule needs a real
/// continuation frame here rather than a silent pick — it must not be added to
/// the enum without one.
fn unprompted_ignored_indices(ignore_rules: &[DieRollIgnoreRule], naturals: &[u8]) -> Vec<usize> {
    // CR 706.6 applies once per instructing effect, so each applied replacement
    // removes one roll from the pool the earlier ones left behind. Every rule is
    // a tied-extreme rule (`Lowest` is the only leaf), whose candidates are all
    // tied at one extreme, so the deterministic pick cannot change any
    // observable outcome (CR 701.52a decides visits per RESULT, and tied rolls
    // hold equal results).
    let mut remaining: Vec<usize> = (0..naturals.len()).collect();
    let mut ignored = Vec::new();
    for rule in ignore_rules {
        if remaining.is_empty() {
            break;
        }
        let pool: Vec<u8> = remaining.iter().map(|&index| naturals[index]).collect();
        let Some(&local) = rule.ignorable_indices(&pool).first() else {
            continue;
        };
        ignored.push(remaining.remove(local));
    }
    ignored
}

/// CR 701.52a: Roll a d6 and visit each controlled Attraction whose lights include the result.
pub fn roll_to_visit_attractions(
    state: &mut GameState,
    player: PlayerId,
    events: &mut Vec<GameEvent>,
) {
    if !controls_attraction(state, player) {
        return;
    }
    // CR 701.52a + CR 614.1a: the roll-to-visit is a die roll like any other, so
    // "if you would roll one or more dice, instead roll that many dice plus one
    // and ignore the lowest roll" (Barbarian Class, Pixie Guide, Wyll) applies
    // here too. CR 614.1a scopes a replacement by its own text, which does not
    // restrict itself to spell- or ability-sourced rolls.
    //
    // CR 616.1: park the continuation frame BEFORE proposing. Two applicable
    // die-roll replacements (Barbarian Class + Pixie Guide) suspend on a
    // `ReplacementChoice`; the parked frame is what tells
    // `roll_die::resume_roll_dice_after_replacement` that this roll belongs to
    // the turn-based roll-to-visit and must be finished by
    // `complete_roll_to_visit`, not by the resolution path.
    state.pending_die_roll_instruction = Some(Box::new(PendingDieRollInstruction {
        // CR 703.4g: the turn-based action has no source object and no
        // resolution context — only the roller matters downstream.
        source_id: ObjectId(0),
        controller: player,
        roller: player,
        targets: Vec::new(),
        results_table: Vec::new(),
        modifier: None,
        die_result: None,
        continuation: DieRollContinuation::RollToVisitAttractions,
    }));
    let proposal = roll_die::propose_roll(state, player, 1, 6, events);
    if !matches!(proposal, roll_die::RollProposal::Suspended) {
        // Only a suspension needs the parked frame; every other outcome finishes
        // inline and must not leave a stale instruction behind for an unrelated
        // later roll to pick up.
        state.pending_die_roll_instruction = None;
    }
    complete_roll_to_visit(state, player, proposal, events);
}

/// CR 701.52a + CR 706.6: Carry out the roll-to-visit once its replacement
/// proposal is known.
///
/// The single authority shared by the inline path (`roll_to_visit_attractions`)
/// and the CR 616.1 resume path
/// (`roll_die::resume_roll_dice_after_replacement`), so an ordering choice
/// cannot silently take a different route — or no route at all — than an
/// unreplaced roll takes.
pub(crate) fn complete_roll_to_visit(
    state: &mut GameState,
    player: PlayerId,
    proposal: roll_die::RollProposal,
    events: &mut Vec<GameEvent>,
) {
    let (count, ignore_rules) = match proposal {
        roll_die::RollProposal::Execute {
            count,
            ignore_rules,
        } => (count, ignore_rules),
        // CR 614.6: the roll never happened — nothing is rolled and nothing is
        // visited.
        roll_die::RollProposal::Prevented => return,
        // CR 616.1: the affected player is ordering the competing replacements.
        // The parked frame routes the resume back into this function with the
        // fully-applied proposal; nothing happens until then.
        roll_die::RollProposal::Suspended => return,
    };

    // CR 706.2: roll every die first, collecting NATURAL results — nothing is
    // emitted until the ignore set is known (CR 706.6).
    let naturals: Vec<u8> = (0..count)
        .map(|_| roll_die::roll_natural(state, 6))
        .collect();
    // CR 706.6: the ignore set is computed over the naturals. This path applies
    // no die-roll modifier, so each surviving natural is also its actual result.
    let ignored_indices = unprompted_ignored_indices(&ignore_rules, &naturals);

    // CR 706.6: an ignored roll is considered never to have happened — it emits
    // no `DieRolled`, appears in no event payload, and visits nothing.
    let survivors: Vec<u8> = naturals
        .into_iter()
        .enumerate()
        .filter(|(index, _)| !ignored_indices.contains(index))
        .map(|(_, roll)| roll)
        .collect();

    // CR 706.2: this path does not go through `roll_die::resume_after_ignore`
    // (it has no results table, no modifier, and no resolution context), so it
    // emits the authoritative die-roll event itself, for survivors only.
    for &roll in &survivors {
        events.push(GameEvent::DieRolled {
            player_id: player,
            sides: 6,
            result: Some(roll),
        });
    }

    // CR 701.52a: the roll-to-visit is ONE turn-based action, so it is reported
    // ONCE no matter how many dice a count-raising replacement (CR 706.6) left
    // surviving. Emitting per surviving die would tell the log and every
    // events-slice consumer that the action happened twice. The per-RESULT
    // decision lives in the `AttractionVisited` loop below, which is where
    // CR 701.52a actually distinguishes results.
    if survivors.is_empty() {
        // CR 614.6 / CR 706.6: every roll was ignored — the action produced no
        // result to visit by, so nothing is reported and nothing is visited.
        super::triggers::process_triggers(state, events);
        return;
    }
    events.push(GameEvent::AttractionsRolledToVisit {
        player_id: player,
        rolls: survivors.clone(),
    });

    // CR 701.52a: visiting is decided PER RESULT — "if you control one or more
    // Attractions with a number lit up that is equal to that result". Two
    // surviving dice therefore make two independent visit checks.
    for roll in survivors {
        for attraction_id in visited_attraction_ids(state, player, roll) {
            events.push(GameEvent::AttractionVisited {
                player_id: player,
                roll,
                attraction_id,
            });
        }
    }

    // CR 706.4: this path deliberately does not stamp
    // `die_result_this_resolution`. The turn-based roll-to-visit is not a
    // spell/ability resolution, so no "equal to the result" effect can read it;
    // stamping would leave stale resolution-scoped state behind.
    super::triggers::process_triggers(state, events);
}

/// CR 703.4g + CR 717.4: Turn-based action at the beginning of the precombat main phase.
pub fn perform_roll_to_visit_turn_based_action(state: &mut GameState, events: &mut Vec<GameEvent>) {
    let player = state.active_player;
    roll_to_visit_attractions(state, player, events);
}

fn controls_attraction(state: &GameState, player: PlayerId) -> bool {
    state.battlefield.iter().any(|id| {
        state
            .objects
            .get(id)
            // CR 702.26b: a phased-out permanent is treated as though it does not exist.
            .is_some_and(|o| {
                o.controller == player && o.is_phased_in() && is_attraction_permanent(o)
            })
    })
}

fn visited_attraction_ids(state: &GameState, player: PlayerId, roll: u8) -> Vec<ObjectId> {
    state
        .battlefield
        .iter()
        .filter_map(|id| {
            let obj = state.objects.get(id)?;
            // CR 702.26b: a phased-out permanent is treated as though it does not exist.
            if obj.controller != player || !obj.is_phased_in() || !is_attraction_permanent(obj) {
                return None;
            }
            if obj.attraction_lights.contains(&roll) {
                Some(*id)
            } else {
                None
            }
        })
        .collect()
}

pub fn resolve_open(
    state: &mut GameState,
    ability: &ResolvedAbility,
    count: u32,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    open_attractions(state, ability.controller, count, events)?;
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::OpenAttractions,
        source_id: ability.source_id,
        subject: None,
    });
    Ok(())
}

pub fn resolve_roll_to_visit(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    roll_to_visit_attractions(state, ability.controller, events);
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::RollToVisitAttractions,
        source_id: ability.source_id,
        subject: None,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::engine::apply_as_current;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, Effect, ReplacementDefinition, TargetFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::game_state::WaitingFor;
    use crate::types::identifiers::CardId;
    use crate::types::replacements::ReplacementEvent;

    /// CR 706.6 + CR 701.52a: the unprompted roll-to-visit ignore decision is
    /// safe ONLY for the tied-extreme rules. `Lowest` — the only rule today —
    /// offers candidates that all hold the same value, so picking any of them is
    /// observationally identical. A free-choice rule would offer candidates with
    /// DIFFERENT values, so picking one would silently discard a roll the roller
    /// was entitled to keep and (CR 701.52a decides visits per result) visit
    /// different Attractions; the enum carries no such rule, and this test pins
    /// the tied-extreme behavior the auto-pick depends on.
    #[test]
    fn unprompted_ignore_is_restricted_to_the_tied_extreme_rules() {
        // A unique lowest is not a choice at all — drop it.
        assert_eq!(
            unprompted_ignored_indices(&[DieRollIgnoreRule::Lowest], &[2, 5]),
            vec![0]
        );
        // The lowest is not always first.
        assert_eq!(
            unprompted_ignored_indices(&[DieRollIgnoreRule::Lowest], &[5, 2]),
            vec![1]
        );
        // A tie among lowest rolls: every candidate holds the same value, so
        // auto-picking the first is observationally identical to any other.
        assert_eq!(
            unprompted_ignored_indices(&[DieRollIgnoreRule::Lowest], &[3, 7, 3]),
            vec![0]
        );
        // Two stacked `Lowest` rules take the lowest of what REMAINS each time,
        // so the pair is {0, 2} — never index 0 twice.
        assert_eq!(
            unprompted_ignored_indices(
                &[DieRollIgnoreRule::Lowest, DieRollIgnoreRule::Lowest],
                &[3, 7, 3]
            ),
            vec![0, 2]
        );

        // No ignore replacement in play: CR 706.6 is inert.
        assert_eq!(
            unprompted_ignored_indices(&[], &[4, 5, 9]),
            Vec::<usize>::new()
        );
        // CR 706.6 applies once per instructing effect: two stacked `Lowest`
        // replacements (Barbarian Class + Pixie Guide) roll three dice and
        // ignore TWO of them, each taking the lowest of what remains — never the
        // same roll twice. Over `[1, 3, 6]` that is indices 0 then 1, leaving
        // the single CR-correct survivor.
        assert_eq!(
            unprompted_ignored_indices(
                &[DieRollIgnoreRule::Lowest, DieRollIgnoreRule::Lowest],
                &[1, 3, 6]
            ),
            vec![0, 1],
            "CR 706.6: N applied replacements must ignore N distinct rolls"
        );
        // More rules than dice: a rule that finds an empty pool ignores nothing
        // rather than panicking or double-counting.
        assert_eq!(
            unprompted_ignored_indices(
                &[DieRollIgnoreRule::Lowest, DieRollIgnoreRule::Lowest],
                &[4]
            ),
            vec![0]
        );
        // No rolls at all: nothing to ignore.
        assert_eq!(
            unprompted_ignored_indices(&[DieRollIgnoreRule::Lowest], &[]),
            Vec::<usize>::new()
        );
    }

    /// CR 701.51 + CR 616.1 discriminating test (fail-first): an Attraction
    /// whose battlefield entry parks on a replacement-ordering prompt (two
    /// co-played external enter-tapped `Moved` effects — the Kismet / Frozen
    /// Aether class parses as ChangeZone Moved defs and collides on the entry's
    /// tap field) must, after the prompt is answered, still receive its open
    /// bookkeeping (`in_attraction_deck` cleared, `AttractionOpened` emitted)
    /// AND the remaining opens of the same instruction must still happen. The
    /// old bail `break` skipped the bookkeeping on the paused Attraction and
    /// silently dropped every remaining open.
    #[test]
    fn paused_attraction_open_resumes_bookkeeping_and_remaining_opens() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);

        // Two Attractions in the supplementary deck (command zone).
        let mut attractions = Vec::new();
        for i in 0..2u64 {
            let id = create_object(
                &mut state,
                CardId(100 + i),
                player,
                format!("Attraction {i}"),
                Zone::Command,
            );
            state.objects.get_mut(&id).unwrap().in_attraction_deck = true;
            state.players[0].attraction_deck.push_back(id);
            attractions.push(id);
        }

        // A genuinely *material* enter tap-state collision: one replacement makes
        // the entering permanent enter tapped (Frozen Aether class), the other
        // makes it enter untapped (Spelunking / Archelos class). Opposite
        // directions are last-applied-wins, so CR 616.1e/f requires the
        // controller to order them and the open parks on a ReplacementChoice.
        // (Two *same*-direction writes are idempotent and commute — they would
        // not prompt; see replacement.rs `CommuteClass::EnterTapped`/`EnterUntapped`.)
        for (offset, name, state_change) in [
            (
                0u64,
                "Frozen Aether",
                crate::types::ability::TapStateChange::Tap,
            ),
            (
                1,
                "Spelunking",
                crate::types::ability::TapStateChange::Untap,
            ),
        ] {
            let oid = ObjectId(9000 + offset);
            let mut src = GameObject::new(
                oid,
                CardId(900 + offset),
                PlayerId(1),
                name.to_string(),
                Zone::Battlefield,
            );
            src.replacement_definitions = vec![ReplacementDefinition::new(ReplacementEvent::Moved)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::SetTapState {
                        target: TargetFilter::SelfRef,
                        scope: crate::types::ability::EffectScope::Single,
                        state: state_change,
                    },
                ))
                .destination_zone(Zone::Battlefield)
                .description(name.to_string())]
            .into();
            state.objects.insert(oid, src);
            state.battlefield.push_back(oid);
        }

        let mut events = Vec::new();
        open_attractions(&mut state, player, 2, &mut events).expect("open attractions");

        // CR 616.1: the first open parked on the tap/untap (opposite-direction)
        // collision.
        let WaitingFor::ReplacementChoice {
            player: chooser, ..
        } = state.waiting_for.clone()
        else {
            panic!(
                "expected parked ReplacementChoice for the tap/untap collision, got {:?}",
                state.waiting_for
            );
        };
        state.priority_player = chooser;
        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("resume first open");

        // The first Attraction's open bookkeeping ran on resume.
        let first = &state.objects[&attractions[0]];
        assert_eq!(first.zone, Zone::Battlefield, "first Attraction delivered");
        assert!(
            !first.in_attraction_deck,
            "open bookkeeping must run on the resumed Attraction (old bail left the flag set)"
        );

        // The remaining open ran — and re-parked on its own entry prompt.
        let WaitingFor::ReplacementChoice {
            player: chooser2, ..
        } = state.waiting_for.clone()
        else {
            panic!(
                "remaining open must run after the pause and re-park, got {:?} (old bail dropped it)",
                state.waiting_for
            );
        };
        state.priority_player = chooser2;
        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("resume second open");

        let second = &state.objects[&attractions[1]];
        assert_eq!(
            second.zone,
            Zone::Battlefield,
            "remaining open must deliver after the pause (old bail dropped it)"
        );
        assert!(!second.in_attraction_deck);
    }
}
