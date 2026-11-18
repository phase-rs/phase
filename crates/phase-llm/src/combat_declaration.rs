//! Combat declarations a chat model composes one attack or block at a time.
//!
//! CR 508.1a + CR 508.1b: the attacking player chooses which creatures attack
//! and what each one attacks. CR 509.1a: a defending player chooses which
//! creatures block and which attacker each one blocks. A declaration is one
//! choice of a whole set of pairs, so the number of legal declarations grows
//! combinatorially with the board. The engine's decision contract therefore
//! admits any combat declaration its combat validators accept, not only the
//! sample of declarations it lists as candidates
//! (`AiDecisionContract::contains_action`).
//!
//! A chat model is offered what a human seat is offered: each pair the pending
//! declaration lists, read from the prompt the engine raised
//! (`valid_attack_targets_by_attacker`, `valid_block_targets`), as one numbered
//! option, and it answers with every pair it declares. Each line names its
//! creatures as the board does, numbering any that would read alike, and
//! states the prompt's records of what it requires of them: a requirement to
//! attack or block, a requirement to be blocked that the block obeys, and a
//! minimum number of blockers. Whether those pairs make a legal
//! declaration is the engine's call, made by its completion authority
//! ([`complete_attacker_proposal`], [`complete_blocker_proposal`]) under the
//! posture the engine's own candidates are completed under, which refuses any
//! cost to attack or block: a declaration it would change is refused, never
//! replaced by one the model did not choose. A pair that carries such a cost is
//! therefore not offered.
//!
//! A System One provider (Jev) answers by ranking options and cannot name a
//! set of them, so it keeps choosing one of the engine's candidates
//! ([`crate::game_decision`]).

use std::collections::{HashMap, HashSet};

use engine::ai_support::CandidateAction;
use engine::game::combat::{
    complete_attacker_proposal, complete_attacker_proposals, complete_blocker_proposal,
    complete_blocker_proposals, compute_attack_tax, compute_block_tax, validate_attack_declaration,
    validate_blockers_for_player, AttackTarget, BlockRequirement, CombatRequirement,
    CombatTaxPosture,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

use crate::error::{LlmError, LlmResult};
use crate::prompt::option_value;
use crate::render::action::DeclarationNames;

/// Why the engine refused a declaration its validator accepts. Under
/// [`CombatTaxPosture::Refuse`] the completion changes only a declaration that
/// carries a cost to attack or block, and no pair that carries one is offered
/// ([`DeclarationPairs::pending`]), so this is a fallback, not an expected
/// outcome.
const CHANGED_BY_COMPLETION: &str = "the engine's completion changes it";

/// A combat declaration a chat model composes, with the options it is shown.
#[derive(Debug)]
pub(crate) struct ComposedDeclaration {
    pairs: DeclarationPairs,
    /// The empty declaration, offered as option 0 when the engine accepts it.
    /// CR 508.1d + CR 509.1c: a requirement to attack or block can make
    /// declaring nothing illegal.
    nothing: Option<GameAction>,
    /// Each option as the prompt numbers it: the empty declaration first when
    /// it is offered, then one line per pair, in the engine's order.
    lines: Vec<String>,
}

impl ComposedDeclaration {
    /// The declaration the engine is waiting on `owner` to make, or `None`
    /// when it is waiting on anything else.
    ///
    /// `state` is the authoritative state the engine decides legality on;
    /// `visible` is the same state filtered for `owner`, the board the options
    /// are named from.
    pub(crate) fn pending(state: &GameState, visible: &GameState, owner: PlayerId) -> Option<Self> {
        let pairs = DeclarationPairs::pending(state, owner)?;
        let nothing = pairs.declare(state, &[]).ok();
        let mut lines = Vec::with_capacity(pairs.len() + 1);
        if nothing.is_some() {
            lines.push(pairs.nothing_label().to_string());
        }
        lines.extend(
            pairs
                .labels(visible, owner)
                .into_iter()
                .map(|label| option_value(&label)),
        );
        Some(Self {
            pairs,
            nothing,
            lines,
        })
    }

    /// The numbered options, as the prompt shows them.
    pub(crate) fn lines(&self) -> &[String] {
        &self.lines
    }

    /// Whether the options leave the model one answer, so the declaration is
    /// a forced move rather than a decision.
    ///
    /// They do when there is one line. Otherwise the engine's candidates
    /// decide, with one addition. They hold the empty declaration and each
    /// single pair, every one completed to a legal declaration, so more than
    /// one candidate is a choice. One candidate is a forced move unless
    /// changing one creature's assignment in it, by a line it does not hold,
    /// makes another declaration the engine accepts as chosen: a creature
    /// joining a block a requirement forces, or one of two creatures that must
    /// block moving to the other attacker, is never a candidate of its own.
    /// A declaration two or more such changes away is not looked for, as the
    /// candidates never sample it.
    pub(crate) fn is_forced(&self, state: &GameState, candidates: &[CandidateAction]) -> bool {
        if self.lines.len() < 2 {
            return true;
        }
        let [only] = candidates else {
            return false;
        };
        !self.pairs.any_one_creature_change(state, &only.action)
    }

    /// The engine-authored instruction stated outside the data block: the
    /// model answers with a list, which [`crate::prompt::GAME_RESPONSE_CONTRACT`]
    /// describes.
    pub(crate) fn instruction(&self) -> &'static str {
        match (&self.pairs, &self.nothing) {
            (DeclarationPairs::Attacks { .. }, Some(_)) => {
                "Choose a list of options: every attack you list is declared together, and \
                 each creature attacks only once. To declare no attackers, choose 0 alone."
            }
            (DeclarationPairs::Attacks { .. }, None) => {
                "Choose a list of options: every attack you list is declared together, and \
                 each creature attacks only once."
            }
            (DeclarationPairs::Blocks { .. }, Some(_)) => {
                "Choose a list of options: every block you list is declared together, and \
                 several creatures can block the same attacker. To declare no blockers, \
                 choose 0 alone."
            }
            (DeclarationPairs::Blocks { .. }, None) => {
                "Choose a list of options: every block you list is declared together, and \
                 several creatures can block the same attacker."
            }
        }
    }

    /// The declaration the options at `indices` make, when the engine accepts
    /// it exactly as chosen.
    ///
    /// `indices` are distinct and in range, as [`crate::prompt::decode_choice`]
    /// returns them. A choice the engine would change is refused rather than
    /// repaired: a repaired declaration is a legal one the model did not make.
    pub(crate) fn bind(&self, state: &GameState, indices: &[usize]) -> LlmResult<GameAction> {
        let chosen: Vec<usize> = match (&self.nothing, indices) {
            (Some(nothing), [0]) => return Ok(nothing.clone()),
            (Some(_), _) if indices.contains(&0) => {
                return Err(LlmError::UndecodableChoice {
                    detail: "option 0 declares nothing and was chosen with other options"
                        .to_string(),
                })
            }
            // Option 0 is the empty declaration, so the pairs start at 1.
            (Some(_), _) => indices.iter().map(|index| index - 1).collect(),
            (None, _) => indices.to_vec(),
        };
        self.pairs
            .declare(state, &chosen)
            .map_err(|reason| LlmError::UndecodableChoice {
                detail: format!("the engine does not accept the declaration as chosen: {reason}"),
            })
    }
}

/// The declarations a change to one creature's assignment makes of
/// `declared`: for each pair of `pairs` it does not hold, that pair added, and,
/// when the pair's creature is already declared, that pair in place of the
/// creature's own.
fn one_creature_changes<Target: Copy + PartialEq>(
    pairs: &[(ObjectId, Target)],
    declared: &[(ObjectId, Target)],
) -> Vec<Vec<(ObjectId, Target)>> {
    let mut changes = Vec::new();
    for &pair in pairs.iter().filter(|pair| !declared.contains(pair)) {
        let (creature, _) = pair;
        changes.push(declared.iter().copied().chain([pair]).collect());
        if declared
            .iter()
            .any(|(declared_creature, _)| *declared_creature == creature)
        {
            changes.push(
                declared
                    .iter()
                    .copied()
                    .filter(|(declared_creature, _)| *declared_creature != creature)
                    .chain([pair])
                    .collect(),
            );
        }
    }
    changes
}

/// The pairs one pending combat declaration is composed from, in the order
/// the engine lists them, with the prompt's records of what it requires.
#[derive(Debug)]
enum DeclarationPairs {
    Attacks {
        /// CR 508.1a + CR 508.1b: each creature that can attack, with each
        /// player, planeswalker or battle it can attack.
        attacks: Vec<(ObjectId, AttackTarget)>,
        /// CR 508.1d: what the prompt requires of each attacker.
        requirements: HashMap<ObjectId, CombatRequirement>,
    },
    Blocks {
        defender: PlayerId,
        /// CR 509.1a: each creature the defending player controls that can
        /// block, with each attacker it can block.
        blocks: Vec<(ObjectId, ObjectId)>,
        /// CR 509.1c: what the prompt requires of each blocker.
        requirements: HashMap<ObjectId, CombatRequirement>,
        /// CR 509.1c: per blocker, the attackers whose requirement to be
        /// blocked its block would obey.
        must_be_blocked: HashMap<ObjectId, Vec<ObjectId>>,
        /// CR 509.1b + CR 702.111b: the attackers that can't be blocked
        /// except by two or more creatures, and how many.
        minimums: HashMap<ObjectId, BlockRequirement>,
    },
}

impl DeclarationPairs {
    /// The pairs of the declaration the engine is waiting on `owner` to make,
    /// less any that carry a cost to attack or block.
    ///
    /// Read in the prompt's own order (its `Vec`s, never a map's iteration
    /// order), because the option lines, and the fingerprint taken over them,
    /// must come out the same when the prompt is built and when the reply is
    /// admitted.
    fn pending(state: &GameState, owner: PlayerId) -> Option<Self> {
        match &state.waiting_for {
            WaitingFor::DeclareAttackers {
                player,
                valid_attacker_ids,
                valid_attack_targets,
                valid_attack_targets_by_attacker,
                attacker_constraints,
                ..
            } if *player == owner => {
                let attacks = valid_attacker_ids
                    .iter()
                    .flat_map(move |&attacker| {
                        // The per-attacker map is the engine's authority; only
                        // a state serialized before it existed lacks it, and
                        // then every attacker may attack every listed target.
                        let targets = match valid_attack_targets_by_attacker {
                            Some(by_attacker) => {
                                by_attacker.get(&attacker).map_or(&[][..], Vec::as_slice)
                            }
                            None => valid_attack_targets.as_slice(),
                        };
                        targets.iter().map(move |&target| (attacker, target))
                    })
                    // CR 508.1d + CR 508.1h: a pair that carries a cost to
                    // attack is never accepted under the refusing posture, so
                    // it is not offered. The engine's quote is per creature,
                    // so a pair without one adds none to a declaration.
                    .filter(|&pair| compute_attack_tax(state, &[pair]).is_none())
                    .collect();
                Some(Self::Attacks {
                    attacks,
                    requirements: attacker_constraints.clone(),
                })
            }
            WaitingFor::DeclareBlockers {
                player,
                valid_blocker_ids,
                valid_block_targets,
                block_requirements,
                blocker_constraints,
                must_be_blocked_targets,
                ..
            } if *player == owner => {
                let blocks = valid_blocker_ids
                    .iter()
                    .flat_map(move |&blocker| {
                        valid_block_targets
                            .get(&blocker)
                            .into_iter()
                            .flatten()
                            .map(move |&attacker| (blocker, attacker))
                    })
                    // CR 509.1c + CR 509.1d: likewise a pair that carries a
                    // cost to block.
                    .filter(|&pair| compute_block_tax(state, &[pair]).is_none())
                    .collect();
                Some(Self::Blocks {
                    defender: *player,
                    blocks,
                    requirements: blocker_constraints.clone(),
                    must_be_blocked: must_be_blocked_targets.clone(),
                    minimums: block_requirements.clone(),
                })
            }
            _ => None,
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Attacks { attacks, .. } => attacks.len(),
            Self::Blocks { blocks, .. } => blocks.len(),
        }
    }

    /// Whether changing one creature's assignment in `declared`, by a line it
    /// does not hold, makes a declaration the engine accepts as chosen. Every
    /// such declaration is completed in one batch, as the engine completes
    /// its own candidates.
    fn any_one_creature_change(&self, state: &GameState, declared: &GameAction) -> bool {
        match (self, declared) {
            (Self::Attacks { attacks, .. }, GameAction::DeclareAttackers { attacks: base, .. }) => {
                let proposals = one_creature_changes(attacks, base);
                complete_attacker_proposals(state, &proposals, CombatTaxPosture::Refuse)
                    .iter()
                    .zip(&proposals)
                    .any(|(completed, proposal)| {
                        matches!(
                            completed,
                            GameAction::DeclareAttackers { attacks: kept, .. } if kept == proposal
                        )
                    })
            }
            (
                Self::Blocks {
                    defender, blocks, ..
                },
                GameAction::DeclareBlockers { assignments: base },
            ) => {
                let proposals = one_creature_changes(blocks, base);
                complete_blocker_proposals(state, *defender, &proposals, CombatTaxPosture::Refuse)
                    .iter()
                    .zip(&proposals)
                    .any(|(completed, proposal)| {
                        matches!(
                            completed,
                            GameAction::DeclareBlockers { assignments: kept } if kept == proposal
                        )
                    })
            }
            // A candidate of another kind is no declaration of these pairs.
            _ => false,
        }
    }

    /// The label of the empty declaration's option.
    fn nothing_label(&self) -> &'static str {
        match self {
            Self::Attacks { .. } => "Declare no attackers",
            Self::Blocks { .. } => "Declare no blockers",
        }
    }

    /// One label per pair, named as `viewer` sees the board in `visible`,
    /// with what the prompt requires of its creatures.
    fn labels(&self, visible: &GameState, viewer: PlayerId) -> Vec<String> {
        let names = DeclarationNames::new(visible, viewer, &self.objects());
        match self {
            Self::Attacks {
                attacks,
                requirements,
            } => attacks
                .iter()
                .map(|&pair| names.attack(pair, requirements.get(&pair.0)))
                .collect(),
            Self::Blocks {
                blocks,
                requirements,
                must_be_blocked,
                minimums,
                ..
            } => blocks
                .iter()
                .map(|&(blocker, attacker)| {
                    names.block(
                        (blocker, attacker),
                        requirements.get(&blocker),
                        must_be_blocked.get(&blocker).map(Vec::as_slice),
                        minimums.get(&attacker),
                    )
                })
                .collect(),
        }
    }

    /// Every object the pairs name, once each, in the order they first
    /// appear: the order objects that read alike are numbered in.
    fn objects(&self) -> Vec<ObjectId> {
        let mut objects = Vec::new();
        let mut seen = HashSet::new();
        let mut name = |id: ObjectId| {
            if seen.insert(id) {
                objects.push(id);
            }
        };
        match self {
            Self::Attacks { attacks, .. } => {
                for &(attacker, target) in attacks {
                    name(attacker);
                    match target {
                        AttackTarget::Player(_) => {}
                        AttackTarget::Planeswalker(id) | AttackTarget::Battle(id) => name(id),
                    }
                }
            }
            Self::Blocks { blocks, .. } => {
                for &(blocker, attacker) in blocks {
                    name(blocker);
                    name(attacker);
                }
            }
        }
        objects
    }

    /// The declaration the pairs at `chosen` make, or why the engine refuses
    /// it.
    ///
    /// CR 508.1c + CR 508.1d + CR 509.1b + CR 509.1c: the completion
    /// authority returns a declaration unchanged only when it breaks no
    /// restriction and obeys as many requirements as any declaration could.
    /// Under [`CombatTaxPosture::Refuse`] it also changes any declaration that
    /// carries a cost to attack or block (CR 508.1h + CR 509.1d), as the
    /// engine's own candidates are completed; [`Self::pending`] offers no pair
    /// that carries one.
    fn declare(&self, state: &GameState, chosen: &[usize]) -> Result<GameAction, String> {
        match self {
            Self::Attacks { attacks: pairs, .. } => {
                let attacks: Vec<(ObjectId, AttackTarget)> =
                    chosen.iter().map(|&index| pairs[index]).collect();
                let declared = GameAction::DeclareAttackers {
                    attacks: attacks.clone(),
                    bands: Vec::new(),
                };
                let completed =
                    complete_attacker_proposal(state, &attacks, &[], CombatTaxPosture::Refuse);
                if completed == declared {
                    return Ok(declared);
                }
                Err(validate_attack_declaration(state, &attacks, &[])
                    .err()
                    .unwrap_or_else(|| CHANGED_BY_COMPLETION.to_string()))
            }
            Self::Blocks {
                defender,
                blocks: pairs,
                ..
            } => {
                let assignments: Vec<(ObjectId, ObjectId)> =
                    chosen.iter().map(|&index| pairs[index]).collect();
                let declared = GameAction::DeclareBlockers {
                    assignments: assignments.clone(),
                };
                let completed = complete_blocker_proposal(
                    state,
                    *defender,
                    &assignments,
                    CombatTaxPosture::Refuse,
                );
                if completed == declared {
                    return Ok(declared);
                }
                Err(validate_blockers_for_player(state, *defender, &assignments)
                    .err()
                    .unwrap_or_else(|| CHANGED_BY_COMPLETION.to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Each test lays out printed cards, lets the engine raise the combat
    //! declaration, and takes the seat's decision through the production path:
    //! the prompt from `build_game_decision_prompt`, the reply admitted by
    //! `select_action_from_response`, and the declaration applied through
    //! `apply_ai_action_proposal`, the boundary an AI seat's actions cross.

    use engine::ai_support::{apply_ai_action_proposal, AiDecisionContract, AiProposalApplication};
    use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
    use engine::types::mana::ManaCost;
    use engine::types::phase::Phase;
    use phase_ai::config::AiDifficulty;
    use serde_json::json;

    use super::*;
    use crate::game_decision::{
        build_game_decision_prompt, select_action_from_response, GameDecisionRequest,
    };
    use crate::prompt::{GAME_RESPONSE_CONTRACT, UNTRUSTED_DATA_END};
    use crate::provider::{DecisionContext, LlmEndpointConfig, LlmProvider};
    use crate::wire::{build_chat_request, system_one_option_keys, LlmReply};

    const P2: PlayerId = PlayerId(2);

    const SERRA_ANGEL: &str = "Flying\nVigilance (Attacking doesn't cause this creature to tap.)";
    const BOGGART_BRUTE: &str =
        "Menace (This creature can't be blocked except by two or more creatures.)";
    const BLOODROCK_CYCLOPS: &str = "This creature attacks each combat if able.";
    const RAZORGRASS_SCREEN: &str =
        "Defender (This creature can't attack.)\nThis creature blocks each combat if able.";
    /// What a block line on Boggart Brute adds, read from the prompt's
    /// minimum-blocker record.
    const NEEDS_TWO_BLOCKERS: &str = "the attacker can't be blocked except by 2 or more creatures";
    const GHOSTLY_PRISON: &str = "Creatures can't attack you unless their controller pays {2} \
         for each creature they control that's attacking you.";
    const JUGGERNAUT: &str =
        "This creature attacks each combat if able.\nThis creature can't be blocked by Walls.";
    const ARCHANGEL_OF_TITHES: &str = "Flying\nAs long as this creature is untapped, creatures \
         can't attack you or planeswalkers you control unless their controller pays {1} for each \
         of those creatures.\nAs long as this creature is attacking, creatures can't block unless \
         their controller pays {1} for each of those creatures.";
    const GIANT_SPIDER: &str = "Reach (This creature can block creatures with flying.)";
    const OCHRAN_ASSASSIN: &str = "Deathtouch\nAll creatures able to block this creature do so.";
    const MOGG_FLUNKIES: &str = "This creature can't attack or block alone.";
    const IRRESISTIBLE_PREY: &str =
        "Target creature must be blocked this turn if able.\nDraw a card.";
    const TANGLE_ANGLER: &str = "Infect (This creature deals damage to creatures in the form of \
         -1/-1 counters and to players in the form of poison counters.)\n{G}: Target creature \
         blocks this creature this turn if able.";
    const JACE_BELEREN: &str = "+2: Each player draws a card.\n\u{2212}1: Target player draws a \
         card.\n\u{2212}10: Target player mills twenty cards.";

    /// Pass priority until the engine waits on a combat declaration.
    fn pass_to_declaration(runner: &mut GameRunner) {
        for _ in 0..32 {
            let waiting_for = &runner.state().waiting_for;
            if matches!(
                waiting_for,
                WaitingFor::DeclareAttackers { .. } | WaitingFor::DeclareBlockers { .. }
            ) {
                return;
            }
            assert!(
                matches!(waiting_for, WaitingFor::Priority { .. }),
                "unexpected prompt {waiting_for:?}"
            );
            runner.act(GameAction::PassPriority).expect("pass priority");
        }
        panic!("the engine never asked for a combat declaration");
    }

    /// Player 0 attacks Player 1 with `attackers`, and the engine waits on
    /// Player 1's blockers.
    fn attack_player_one(runner: &mut GameRunner, attackers: &[ObjectId]) {
        pass_to_declaration(runner);
        let attacks: Vec<(ObjectId, AttackTarget)> = attackers
            .iter()
            .map(|&attacker| (attacker, AttackTarget::Player(P1)))
            .collect();
        runner
            .declare_attackers(&attacks)
            .expect("declare attackers");
        pass_to_declaration(runner);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { player, .. } if player == P1
        ));
    }

    /// `seat`'s decision, as the production prompt builder puts it.
    fn decision(runner: &GameRunner, seat: PlayerId) -> (AiDecisionContract, GameDecisionRequest) {
        let contract = AiDecisionContract::issue(runner.state(), seat);
        let request =
            build_game_decision_prompt(runner.state(), &contract, AiDifficulty::Medium, None, &[])
                .expect("the declaration is put to the model");
        (contract, request)
    }

    /// The number the chat prompt gives the option that reads `label`.
    fn option_number(request: &GameDecisionRequest, label: &str) -> usize {
        let user = &request.prompt.user;
        user.lines()
            .find_map(|line| {
                let (number, text) = line.trim_start().strip_prefix('[')?.split_once("] ")?;
                if text != label {
                    return None;
                }
                number.parse().ok()
            })
            .unwrap_or_else(|| panic!("no option reads {label:?}:\n{user}"))
    }

    /// The seat's chat reply naming `choices`, admitted through the response
    /// path as an Anthropic Messages reply.
    fn answer(
        runner: &GameRunner,
        contract: &AiDecisionContract,
        request: &GameDecisionRequest,
        choices: &[usize],
    ) -> LlmResult<GameAction> {
        let text = json!({ "choice": choices, "reason": "the best combat" }).to_string();
        let body = json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "stop_reason": "end_turn",
            "content": [{ "type": "text", "text": text }],
            "usage": { "input_tokens": 1, "output_tokens": 1 },
        })
        .to_string();
        let reply = LlmReply {
            provider: LlmProvider::Anthropic,
            status: 200,
            body: &body,
        };
        select_action_from_response(runner.state(), contract, &request.fingerprint, reply)
            .map(|selection| selection.action)
    }

    /// `action` with its pairs in a fixed order, so two declarations of the
    /// same pairs compare equal.
    fn normalized(action: &GameAction) -> GameAction {
        let mut action = action.clone();
        match &mut action {
            GameAction::DeclareAttackers { attacks, .. } => attacks.sort_unstable(),
            GameAction::DeclareBlockers { assignments } => assignments.sort_unstable(),
            other => panic!("not a combat declaration: {other:?}"),
        }
        action
    }

    /// Whether the engine's candidates, the options a seat was offered before
    /// it could compose a declaration, already hold `action`.
    fn offered_as_candidate(contract: &AiDecisionContract, action: &GameAction) -> bool {
        let wanted = normalized(action);
        contract
            .candidates
            .iter()
            .any(|candidate| normalized(&candidate.action) == wanted)
    }

    /// Apply `action` as the contract's actor, through the AI proposal boundary.
    fn apply(runner: &mut GameRunner, contract: &AiDecisionContract, action: GameAction) {
        let actor = contract.authorized_actor;
        match apply_ai_action_proposal(runner.state_mut(), contract, actor, action) {
            AiProposalApplication::AppliedAction { .. } => {}
            other => panic!("the engine did not apply the declaration: {other:?}"),
        }
    }

    /// Each attacker with what it attacks, in id order.
    fn attacks(runner: &GameRunner) -> Vec<(ObjectId, AttackTarget)> {
        let combat = runner.state().combat.as_ref().expect("a combat");
        let mut attacks: Vec<(ObjectId, AttackTarget)> = combat
            .attackers
            .iter()
            .map(|attacker| (attacker.object_id, attacker.attack_target))
            .collect();
        attacks.sort_unstable();
        attacks
    }

    /// The creatures blocking `attacker`, in id order.
    fn blockers_of(runner: &GameRunner, attacker: ObjectId) -> Vec<ObjectId> {
        let combat = runner.state().combat.as_ref().expect("a combat");
        let mut blockers = combat
            .blocker_assignments
            .get(&attacker)
            .cloned()
            .unwrap_or_default();
        blockers.sort_unstable();
        blockers
    }

    fn refusal(result: LlmResult<GameAction>) -> String {
        match result {
            Err(LlmError::UndecodableChoice { detail }) => detail,
            other => panic!("expected the declaration to be refused, got {other:?}"),
        }
    }

    /// Whether `seat`'s decision is taken as a forced move, not put to a model.
    fn is_forced_move(runner: &GameRunner, seat: PlayerId) -> bool {
        let contract = AiDecisionContract::issue(runner.state(), seat);
        matches!(
            build_game_decision_prompt(runner.state(), &contract, AiDifficulty::Medium, None, &[]),
            Err(LlmError::UnsupportedDecision { .. })
        )
    }

    /// The attackers each blocker on the pending prompt can block.
    fn valid_block_targets(runner: &GameRunner) -> HashMap<ObjectId, Vec<ObjectId>> {
        let WaitingFor::DeclareBlockers {
            valid_block_targets,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("the engine asks for blockers");
        };
        valid_block_targets.clone()
    }

    /// Issue #10992: a seat could attack with one creature or with all of
    /// them. Two of three, keeping one home, is now a declaration it makes.
    #[test]
    fn a_seat_attacks_with_two_of_its_three_creatures() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let angel = scenario
            .add_creature(P0, "Serra Angel", 4, 4)
            .from_oracle_text_with_keywords(&["Flying", "Vigilance"], SERRA_ANGEL)
            .id();
        let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let mut runner = scenario.build();
        pass_to_declaration(&mut runner);

        let (contract, request) = decision(&runner, P0);
        let declared = answer(
            &runner,
            &contract,
            &request,
            &[
                option_number(&request, "Serra Angel (controlled by You) attacks Player 1"),
                option_number(&request, "Hill Giant (controlled by You) attacks Player 1"),
            ],
        )
        .expect("two attackers of three is a legal declaration");
        assert!(!offered_as_candidate(&contract, &declared));

        apply(&mut runner, &contract, declared);
        assert_eq!(
            attacks(&runner),
            vec![
                (angel, AttackTarget::Player(P1)),
                (giant, AttackTarget::Player(P1)),
            ]
        );
        assert!(
            !runner.state().objects[&bears].tapped,
            "the Bears stay home"
        );
    }

    /// CR 508.1b: in a game that lets a player attack several others, each
    /// attacker is aimed separately. A seat could only send every attacker at
    /// one player.
    #[test]
    fn a_seat_splits_its_attackers_between_two_opponents() {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
        let mut runner = scenario.build();
        pass_to_declaration(&mut runner);

        let (contract, request) = decision(&runner, P0);
        let declared = answer(
            &runner,
            &contract,
            &request,
            &[
                option_number(
                    &request,
                    "Grizzly Bears (controlled by You) attacks Player 1",
                ),
                option_number(&request, "Hill Giant (controlled by You) attacks Player 2"),
            ],
        )
        .expect("attacking two players is a legal declaration");
        assert!(!offered_as_candidate(&contract, &declared));

        apply(&mut runner, &contract, declared);
        assert_eq!(
            attacks(&runner),
            vec![
                (bears, AttackTarget::Player(P1)),
                (giant, AttackTarget::Player(P2)),
            ]
        );
    }

    /// Issue #10992: a seat could block at most one attacker. Each of its two
    /// creatures now blocks a different attacker in one declaration.
    #[test]
    fn a_seat_blocks_two_attackers_with_two_creatures() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let armodon = scenario.add_creature(P1, "Trained Armodon", 3, 3).id();
        let ogre = scenario.add_creature(P1, "Gray Ogre", 2, 2).id();
        let mut runner = scenario.build();
        attack_player_one(&mut runner, &[giant, bears]);

        let (contract, request) = decision(&runner, P1);
        let declared = answer(
            &runner,
            &contract,
            &request,
            &[
                option_number(
                    &request,
                    "Trained Armodon (controlled by You) blocks Hill Giant (controlled by Player 0)",
                ),
                option_number(
                    &request,
                    "Gray Ogre (controlled by You) blocks Grizzly Bears (controlled by Player 0)",
                ),
            ],
        )
        .expect("one blocker on each attacker is a legal declaration");
        assert!(!offered_as_candidate(&contract, &declared));

        apply(&mut runner, &contract, declared);
        assert_eq!(blockers_of(&runner, giant), vec![armodon]);
        assert_eq!(blockers_of(&runner, bears), vec![ogre]);
    }

    /// CR 509.1a: the defending player chooses one attacker for each blocking
    /// creature, so several can be assigned the same attacker. Neither of the
    /// seat's creatures kills the Craw Wurm alone; together they deal 5 to its
    /// 4 toughness.
    #[test]
    fn a_seat_double_blocks_an_attacker_without_evasion() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let wurm = scenario.add_creature(P0, "Craw Wurm", 6, 4).id();
        let armodon = scenario.add_creature(P1, "Trained Armodon", 3, 3).id();
        let ogre = scenario.add_creature(P1, "Gray Ogre", 2, 2).id();
        let mut runner = scenario.build();
        attack_player_one(&mut runner, &[wurm]);

        let (contract, request) = decision(&runner, P1);
        let declared = answer(
            &runner,
            &contract,
            &request,
            &[
                option_number(
                    &request,
                    "Trained Armodon (controlled by You) blocks Craw Wurm (controlled by Player 0)",
                ),
                option_number(
                    &request,
                    "Gray Ogre (controlled by You) blocks Craw Wurm (controlled by Player 0)",
                ),
            ],
        )
        .expect("a double block is a legal declaration");
        assert!(!offered_as_candidate(&contract, &declared));

        apply(&mut runner, &contract, declared);
        let mut both = vec![armodon, ogre];
        both.sort_unstable();
        assert_eq!(blockers_of(&runner, wurm), both);
    }

    /// CR 702.111b: a creature with menace can't be blocked except by two or
    /// more creatures. A seat was offered exactly two; three is a gang block
    /// it can now declare.
    #[test]
    fn a_seat_blocks_a_menace_attacker_with_three_creatures() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let brute = scenario
            .add_creature(P0, "Boggart Brute", 3, 2)
            .from_oracle_text_with_keywords(&["Menace"], BOGGART_BRUTE)
            .id();
        let mut blockers = vec![
            scenario.add_creature(P1, "Grizzly Bears", 2, 2).id(),
            scenario.add_creature(P1, "Gray Ogre", 2, 2).id(),
            scenario.add_creature(P1, "Balduvian Bears", 2, 2).id(),
        ];
        let mut runner = scenario.build();
        attack_player_one(&mut runner, &[brute]);

        let (contract, request) = decision(&runner, P1);
        let declared = answer(
            &runner,
            &contract,
            &request,
            &["Grizzly Bears", "Gray Ogre", "Balduvian Bears"].map(|name| {
                option_number(
                    &request,
                    &format!(
                        "{name} (controlled by You) blocks Boggart Brute (controlled by Player \
                         0); {NEEDS_TWO_BLOCKERS}"
                    ),
                )
            }),
        )
        .expect("three blockers on a menace attacker is a legal declaration");
        assert!(!offered_as_candidate(&contract, &declared));

        apply(&mut runner, &contract, declared);
        blockers.sort_unstable();
        assert_eq!(blockers_of(&runner, brute), blockers);
    }

    /// CR 509.1b + CR 702.111b: one creature blocking a menace attacker is an
    /// illegal declaration. The engine's completion would replace it with a
    /// declaration of its own; the seat's reply is refused instead, so the
    /// heuristic declares rather than a block nobody chose.
    #[test]
    fn a_menace_attacker_blocked_by_one_creature_is_refused() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let brute = scenario
            .add_creature(P0, "Boggart Brute", 3, 2)
            .from_oracle_text_with_keywords(&["Menace"], BOGGART_BRUTE)
            .id();
        scenario.add_creature(P1, "Grizzly Bears", 2, 2);
        scenario.add_creature(P1, "Gray Ogre", 2, 2);
        let mut runner = scenario.build();
        attack_player_one(&mut runner, &[brute]);

        let (contract, request) = decision(&runner, P1);
        let detail = refusal(answer(
            &runner,
            &contract,
            &request,
            &[option_number(
                &request,
                &format!(
                    "Grizzly Bears (controlled by You) blocks Boggart Brute (controlled by Player \
                     0); {NEEDS_TWO_BLOCKERS}"
                ),
            )],
        ));
        assert!(
            detail.starts_with("the engine does not accept the declaration as chosen"),
            "{detail}"
        );
    }

    /// Option 0 declares no blockers, so naming it beside a block contradicts
    /// itself and is refused rather than read either way.
    #[test]
    fn declaring_no_blockers_beside_a_block_is_refused() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
        scenario.add_creature(P1, "Trained Armodon", 3, 3);
        let mut runner = scenario.build();
        attack_player_one(&mut runner, &[giant]);

        let (contract, request) = decision(&runner, P1);
        assert_eq!(option_number(&request, "Declare no blockers"), 0);
        let detail = refusal(answer(
            &runner,
            &contract,
            &request,
            &[
                0,
                option_number(
                    &request,
                    "Trained Armodon (controlled by You) blocks Hill Giant (controlled by Player 0)",
                ),
            ],
        ));
        assert!(detail.starts_with("option 0 declares nothing"), "{detail}");
    }

    /// CR 508.1a + CR 508.1b: a creature is declared as an attacker once,
    /// attacking one player, planeswalker or battle, so naming it at two
    /// players is refused.
    #[test]
    fn one_creature_attacking_two_players_is_refused() {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_creature(P0, "Grizzly Bears", 2, 2);
        scenario.add_creature(P0, "Hill Giant", 3, 3);
        let mut runner = scenario.build();
        pass_to_declaration(&mut runner);

        let (contract, request) = decision(&runner, P0);
        let detail = refusal(answer(
            &runner,
            &contract,
            &request,
            &[
                option_number(
                    &request,
                    "Grizzly Bears (controlled by You) attacks Player 1",
                ),
                option_number(
                    &request,
                    "Grizzly Bears (controlled by You) attacks Player 2",
                ),
            ],
        ));
        assert!(
            detail.starts_with("the engine does not accept the declaration as chosen"),
            "{detail}"
        );
    }

    /// CR 508.1d + CR 508.1h: attacking past Ghostly Prison costs mana. A
    /// seat's declaration is completed as the engine's own candidates are,
    /// refusing any cost to attack, so the taxed attack is not offered, while
    /// the free one beside it is declared.
    #[test]
    fn an_attack_that_carries_a_cost_is_not_offered() {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        scenario.add_enchantment_from_oracle(P1, "Ghostly Prison", GHOSTLY_PRISON);
        let mut runner = scenario.build();
        pass_to_declaration(&mut runner);

        // Reach guard: the engine's prompt lists the taxed attack among the
        // attacks the Bears can make.
        let WaitingFor::DeclareAttackers {
            valid_attack_targets_by_attacker: Some(by_attacker),
            ..
        } = &runner.state().waiting_for
        else {
            panic!("the engine asks for attackers with its per-attacker map");
        };
        assert!(by_attacker[&bears].contains(&AttackTarget::Player(P1)));

        let (contract, request) = decision(&runner, P0);
        assert!(
            !request
                .prompt
                .user
                .contains("Grizzly Bears (controlled by You) attacks Player 1"),
            "{}",
            request.prompt.user
        );
        let free = option_number(
            &request,
            "Grizzly Bears (controlled by You) attacks Player 2",
        );
        assert_eq!(
            answer(&runner, &contract, &request, &[free]).expect("an untaxed attack"),
            GameAction::DeclareAttackers {
                attacks: vec![(bears, AttackTarget::Player(P2))],
                bands: vec![],
            }
        );
    }

    /// CR 508.1d: a creature that attacks each combat if able makes declaring
    /// no attackers illegal, so that option is not offered. Its lines say it
    /// must attack, and a declaration that leaves it home is refused.
    #[test]
    fn a_creature_that_must_attack_takes_declaring_nothing_off_the_list() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let cyclops = scenario
            .add_creature_from_oracle(P0, "Bloodrock Cyclops", 3, 3, BLOODROCK_CYCLOPS)
            .id();
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let mut runner = scenario.build();
        pass_to_declaration(&mut runner);

        let (contract, request) = decision(&runner, P0);
        assert!(
            !request.prompt.user.contains("Declare no attackers"),
            "{}",
            request.prompt.user
        );
        assert!(!request.prompt.user.contains("choose 0 alone"));
        let cyclops_attacks = option_number(
            &request,
            "Bloodrock Cyclops (controlled by You) attacks Player 1; it must attack if able",
        );
        let bears_attack = option_number(
            &request,
            "Grizzly Bears (controlled by You) attacks Player 1",
        );
        refusal(answer(&runner, &contract, &request, &[bears_attack]));
        let declared = answer(
            &runner,
            &contract,
            &request,
            &[cyclops_attacks, bears_attack],
        )
        .expect("both attacking obeys the requirement");
        assert_eq!(
            normalized(&declared),
            normalized(&GameAction::DeclareAttackers {
                attacks: vec![
                    (cyclops, AttackTarget::Player(P1)),
                    (bears, AttackTarget::Player(P1)),
                ],
                bands: vec![],
            })
        );
    }

    /// CR 509.1c: a creature that blocks each combat if able makes declaring
    /// no blockers illegal, so that option is not offered. Its lines say it
    /// must block, and a declaration that leaves it out is refused.
    #[test]
    fn a_creature_that_must_block_takes_declaring_nothing_off_the_list() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let screen = scenario
            .add_creature(P1, "Razorgrass Screen", 2, 1)
            .from_oracle_text_with_keywords(&["Defender"], RAZORGRASS_SCREEN)
            .id();
        let ogre = scenario.add_creature(P1, "Gray Ogre", 2, 2).id();
        let mut runner = scenario.build();
        attack_player_one(&mut runner, &[giant, bears]);

        let (contract, request) = decision(&runner, P1);
        assert!(
            !request.prompt.user.contains("Declare no blockers"),
            "{}",
            request.prompt.user
        );
        let screen_blocks = option_number(
            &request,
            "Razorgrass Screen (controlled by You) blocks Grizzly Bears (controlled by Player 0); \
             it must block if able",
        );
        let ogre_blocks = option_number(
            &request,
            "Gray Ogre (controlled by You) blocks Hill Giant (controlled by Player 0)",
        );
        refusal(answer(&runner, &contract, &request, &[ogre_blocks]));
        let declared = answer(&runner, &contract, &request, &[screen_blocks, ogre_blocks])
            .expect("the Screen blocking obeys the requirement");
        assert_eq!(
            normalized(&declared),
            normalized(&GameAction::DeclareBlockers {
                assignments: vec![(screen, bears), (ogre, giant)],
            })
        );
    }

    /// A block beside one a requirement forces: the engine's candidates hold
    /// only the Razorgrass Screen's block, but the Gray Ogre may join it and
    /// the two kill the Hill Giant.
    #[test]
    fn a_block_beside_a_required_block_is_offered() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
        let screen = scenario
            .add_creature(P1, "Razorgrass Screen", 2, 1)
            .from_oracle_text_with_keywords(&["Defender"], RAZORGRASS_SCREEN)
            .id();
        let ogre = scenario.add_creature(P1, "Gray Ogre", 2, 2).id();
        let mut runner = scenario.build();
        attack_player_one(&mut runner, &[giant]);

        let (contract, request) = decision(&runner, P1);
        assert_eq!(contract.candidates.len(), 1, "{:?}", contract.candidates);
        let declared = answer(
            &runner,
            &contract,
            &request,
            &[
                option_number(
                    &request,
                    "Razorgrass Screen (controlled by You) blocks Hill Giant (controlled by \
                     Player 0); it must block if able",
                ),
                option_number(
                    &request,
                    "Gray Ogre (controlled by You) blocks Hill Giant (controlled by Player 0)",
                ),
            ],
        )
        .expect("both blocking obeys the requirement");
        assert!(!offered_as_candidate(&contract, &declared));

        apply(&mut runner, &contract, declared);
        let mut both = vec![screen, ogre];
        both.sort_unstable();
        assert_eq!(blockers_of(&runner, giant), both);
    }

    /// CR 509.1c: two Razorgrass Screens must each block if able. The engine's
    /// one candidate puts both on the Hill Giant, but each attacker blocked by
    /// one Screen obeys both requirements too, so the declaration is offered,
    /// and the split block, which is no candidate, is declared.
    #[test]
    fn two_creatures_that_must_block_may_split_between_attackers() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let screens = [
            scenario
                .add_creature(P1, "Razorgrass Screen", 2, 1)
                .from_oracle_text_with_keywords(&["Defender"], RAZORGRASS_SCREEN)
                .id(),
            scenario
                .add_creature(P1, "Razorgrass Screen", 2, 1)
                .from_oracle_text_with_keywords(&["Defender"], RAZORGRASS_SCREEN)
                .id(),
        ];
        let mut runner = scenario.build();
        attack_player_one(&mut runner, &[giant, bears]);

        let (contract, request) = decision(&runner, P1);
        assert_eq!(contract.candidates.len(), 1, "{:?}", contract.candidates);
        let declared = answer(
            &runner,
            &contract,
            &request,
            &[
                option_number(
                    &request,
                    "Razorgrass Screen (controlled by You) #1 blocks Hill Giant (controlled by \
                     Player 0); it must block if able",
                ),
                option_number(
                    &request,
                    "Razorgrass Screen (controlled by You) #2 blocks Grizzly Bears (controlled \
                     by Player 0); it must block if able",
                ),
            ],
        )
        .expect("one Screen on each attacker obeys both requirements");
        assert!(!offered_as_candidate(&contract, &declared));

        apply(&mut runner, &contract, declared);
        let mut blocking = blockers_of(&runner, giant);
        assert_eq!(blocking.len(), 1, "one Screen blocks the Giant");
        blocking.extend(blockers_of(&runner, bears));
        blocking.sort_unstable();
        let mut both = screens.to_vec();
        both.sort_unstable();
        assert_eq!(blocking, both, "and the other blocks the Bears");
    }

    /// Two attackers and two blockers that read alike on the board are
    /// numbered in the declaration, so the seat can block each attacker with
    /// a different creature instead of naming one of them twice.
    #[test]
    fn creatures_that_read_alike_are_numbered_so_each_can_be_named() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let attackers = [
            scenario.add_creature(P0, "Grizzly Bears", 2, 2).id(),
            scenario.add_creature(P0, "Grizzly Bears", 2, 2).id(),
        ];
        let blockers = [
            scenario.add_creature(P1, "Gray Ogre", 2, 2).id(),
            scenario.add_creature(P1, "Gray Ogre", 2, 2).id(),
        ];
        let mut runner = scenario.build();
        attack_player_one(&mut runner, &attackers);

        let (contract, request) = decision(&runner, P1);
        let declared = answer(
            &runner,
            &contract,
            &request,
            &[
                option_number(
                    &request,
                    "Gray Ogre (controlled by You) #1 blocks Grizzly Bears (controlled by \
                     Player 0) #1",
                ),
                option_number(
                    &request,
                    "Gray Ogre (controlled by You) #2 blocks Grizzly Bears (controlled by \
                     Player 0) #2",
                ),
            ],
        )
        .expect("one blocker on each attacker is a legal declaration");
        assert!(!offered_as_candidate(&contract, &declared));

        apply(&mut runner, &contract, declared);
        let mut blocking = Vec::new();
        for attacker in attackers {
            let on_this_one = blockers_of(&runner, attacker);
            assert_eq!(on_this_one.len(), 1, "each Grizzly Bears is blocked once");
            blocking.extend(on_this_one);
        }
        blocking.sort_unstable();
        let mut both = blockers.to_vec();
        both.sort_unstable();
        assert_eq!(blocking, both, "by different Gray Ogres");
    }

    /// CR 508.1d: two creatures that each attack each combat if able leave
    /// one legal declaration, both attacking, which the lines are exactly. It
    /// is taken as a forced move, as it was when only the engine's candidates
    /// were offered.
    #[test]
    fn two_creatures_that_must_attack_are_a_forced_move() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_creature_from_oracle(P0, "Bloodrock Cyclops", 3, 3, BLOODROCK_CYCLOPS);
        scenario
            .add_creature(P0, "Juggernaut", 5, 3)
            .as_artifact_creature()
            .from_oracle_text(JUGGERNAUT);
        let mut runner = scenario.build();
        pass_to_declaration(&mut runner);

        // Reach guard: the two attacks are two pairs, so a count of lines
        // alone would put the declaration to the model.
        let contract = AiDecisionContract::issue(runner.state(), P0);
        assert_eq!(contract.candidates.len(), 1, "{:?}", contract.candidates);
        let declaration = ComposedDeclaration::pending(runner.state(), runner.state(), P0)
            .expect("a declaration");
        assert_eq!(declaration.lines().len(), 2, "{:?}", declaration.lines());

        assert!(is_forced_move(&runner, P0));
    }

    /// CR 509.1b + CR 702.111b: a menace attacker with only one creature able
    /// to block it can't be blocked. The engine's one candidate declares no
    /// blockers and the lone block can't join it, so the seat takes it as a
    /// forced move.
    #[test]
    fn a_menace_attacker_only_one_creature_can_block_is_a_forced_move() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let brute = scenario
            .add_creature(P0, "Boggart Brute", 3, 2)
            .from_oracle_text_with_keywords(&["Menace"], BOGGART_BRUTE)
            .id();
        let bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
        let mut runner = scenario.build();
        attack_player_one(&mut runner, &[brute]);

        // Reach guard: the engine's prompt lists the block, so the lines are
        // the empty declaration and that block.
        assert_eq!(valid_block_targets(&runner)[&bears], vec![brute]);
        let declaration = ComposedDeclaration::pending(runner.state(), runner.state(), P1)
            .expect("a declaration");
        assert_eq!(declaration.lines().len(), 2, "{:?}", declaration.lines());
        assert!(is_forced_move(&runner, P1));
    }

    /// CR 506.5 + CR 509.1b: a lone Mogg Flunkies can't block alone. As with
    /// the menace attacker, its block can't join the engine's one candidate,
    /// so declaring no blockers is a forced move.
    #[test]
    fn a_creature_that_cant_block_alone_is_a_forced_move_alone() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
        let flunkies = scenario
            .add_creature_from_oracle(P1, "Mogg Flunkies", 3, 3, MOGG_FLUNKIES)
            .id();
        let mut runner = scenario.build();
        attack_player_one(&mut runner, &[giant]);

        assert_eq!(valid_block_targets(&runner)[&flunkies], vec![giant]);
        let declaration = ComposedDeclaration::pending(runner.state(), runner.state(), P1)
            .expect("a declaration");
        assert_eq!(declaration.lines().len(), 2, "{:?}", declaration.lines());
        assert!(is_forced_move(&runner, P1));
    }

    /// CR 509.1c + CR 509.1d: while Archangel of Tithes attacks, blocking
    /// costs mana. The block is completed refusing that cost, so no line
    /// offers it, and with nothing else to declare the seat takes no blockers
    /// as a forced move.
    #[test]
    fn a_block_that_carries_a_cost_is_not_offered() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let archangel = scenario
            .add_creature(P0, "Archangel of Tithes", 3, 5)
            .from_oracle_text_with_keywords(&["Flying"], ARCHANGEL_OF_TITHES)
            .id();
        let spider = scenario
            .add_creature(P1, "Giant Spider", 2, 4)
            .from_oracle_text_with_keywords(&["Reach"], GIANT_SPIDER)
            .id();
        let mut runner = scenario.build();
        attack_player_one(&mut runner, &[archangel]);

        // Reach guard: the engine's prompt lists the block, and it is taxed.
        assert_eq!(valid_block_targets(&runner)[&spider], vec![archangel]);
        assert!(compute_block_tax(runner.state(), &[(spider, archangel)]).is_some());
        let declaration = ComposedDeclaration::pending(runner.state(), runner.state(), P1)
            .expect("a declaration");
        assert_eq!(declaration.lines(), ["Declare no blockers"]);
        assert!(is_forced_move(&runner, P1));
    }

    /// CR 509.1c: Ochran Assassin must be blocked by every creature able to
    /// block it, so both of the seat's creatures blocking it is the one legal
    /// declaration. The lines also offer blocks of the other attacker, which
    /// can't join it, so the seat takes it as a forced move.
    #[test]
    fn a_requirement_every_creature_obeys_is_a_forced_move() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let assassin = scenario
            .add_creature_from_oracle(P0, "Ochran Assassin", 1, 1, OCHRAN_ASSASSIN)
            .id();
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        scenario.add_creature(P1, "Trained Armodon", 3, 3);
        scenario.add_creature(P1, "Gray Ogre", 2, 2);
        let mut runner = scenario.build();
        attack_player_one(&mut runner, &[assassin, bears]);

        // Reach guard: four lines, so a count of lines alone would put the
        // declaration to the model.
        let declaration = ComposedDeclaration::pending(runner.state(), runner.state(), P1)
            .expect("a declaration");
        assert_eq!(declaration.lines().len(), 4, "{:?}", declaration.lines());
        assert!(is_forced_move(&runner, P1));
    }

    /// CR 509.1c: Irresistible Prey makes Grizzly Bears a creature that must be
    /// blocked if able, and one blocker is enough. Each line that blocks the
    /// Bears says the block obeys that requirement, and a line blocking the
    /// other attacker does not.
    #[test]
    fn a_block_that_obeys_a_requirement_to_be_blocked_says_so() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
        // Cast for {0}, as the engine's own Irresistible Prey test casts it;
        // its draw needs a card in the library.
        let prey = scenario
            .add_spell_to_hand_from_oracle(P0, "Irresistible Prey", false, IRRESISTIBLE_PREY)
            .with_mana_cost(ManaCost::zero())
            .id();
        scenario.with_library_top(P0, &["Forest"]);
        let armodon = scenario.add_creature(P1, "Trained Armodon", 3, 3).id();
        let ogre = scenario.add_creature(P1, "Gray Ogre", 2, 2).id();
        let mut runner = scenario.build();
        runner.cast(prey).target_object(bears).resolve();
        attack_player_one(&mut runner, &[bears, giant]);

        let (contract, request) = decision(&runner, P1);
        let obeys = "this block obeys a requirement that the attacker be blocked";
        option_number(
            &request,
            "Gray Ogre (controlled by You) blocks Hill Giant (controlled by Player 0)",
        );
        let declared = answer(
            &runner,
            &contract,
            &request,
            &[
                option_number(
                    &request,
                    "Trained Armodon (controlled by You) blocks Hill Giant (controlled by Player 0)",
                ),
                option_number(
                    &request,
                    &format!(
                        "Gray Ogre (controlled by You) blocks Grizzly Bears (controlled by \
                         Player 0); {obeys}"
                    ),
                ),
            ],
        )
        .expect("one block on the Bears obeys its requirement");
        assert!(!offered_as_candidate(&contract, &declared));

        apply(&mut runner, &contract, declared);
        assert_eq!(blockers_of(&runner, bears), vec![ogre]);
        assert_eq!(blockers_of(&runner, giant), vec![armodon]);
    }

    /// CR 509.1c: Tangle Angler's ability makes the seat's only creature block
    /// the Angler if able. Blocking it is the one legal declaration, and the
    /// lone creature's block of the other attacker can't join it, so the seat
    /// takes it as a forced move.
    #[test]
    fn a_lone_creature_that_must_block_one_attacker_is_a_forced_move() {
        use engine::types::mana::{ManaType, ManaUnit};

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let angler = scenario
            .add_creature(P0, "Tangle Angler", 1, 5)
            .from_oracle_text_with_keywords(&["Infect"], TANGLE_ANGLER)
            .id();
        let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
        let bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
        let mut runner = scenario.build();
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::Green,
            ObjectId(0),
            false,
            vec![],
        ));
        runner.activate(angler, 0).target_object(bears).resolve();
        attack_player_one(&mut runner, &[angler, giant]);

        // Reach guard: the engine's prompt lets the Bears block either one.
        let targets = &valid_block_targets(&runner)[&bears];
        assert!(
            targets.contains(&angler) && targets.contains(&giant),
            "{targets:?}"
        );
        assert!(is_forced_move(&runner, P1));
    }

    /// CR 509.1c: Tangle Angler's ability makes a creature block it if able.
    /// That creature's lines name the attacker it must block, and a free
    /// creature beside it blocks the other attacker.
    #[test]
    fn a_requirement_to_block_a_named_attacker_names_it() {
        use engine::types::mana::{ManaType, ManaUnit};

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let angler = scenario
            .add_creature(P0, "Tangle Angler", 1, 5)
            .from_oracle_text_with_keywords(&["Infect"], TANGLE_ANGLER)
            .id();
        let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
        let bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
        let ogre = scenario.add_creature(P1, "Gray Ogre", 2, 2).id();
        let mut runner = scenario.build();
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::Green,
            ObjectId(0),
            false,
            vec![],
        ));
        runner.activate(angler, 0).target_object(bears).resolve();
        attack_player_one(&mut runner, &[angler, giant]);

        let (contract, request) = decision(&runner, P1);
        let must = "it must block Tangle Angler (controlled by Player 0) if able";
        option_number(
            &request,
            &format!(
                "Grizzly Bears (controlled by You) blocks Hill Giant (controlled by Player 0); \
                 {must}"
            ),
        );
        let declared = answer(
            &runner,
            &contract,
            &request,
            &[
                option_number(
                    &request,
                    &format!(
                        "Grizzly Bears (controlled by You) blocks Tangle Angler (controlled by \
                         Player 0); {must}"
                    ),
                ),
                option_number(
                    &request,
                    "Gray Ogre (controlled by You) blocks Hill Giant (controlled by Player 0)",
                ),
            ],
        )
        .expect("the required block beside a free one is a legal declaration");
        assert!(!offered_as_candidate(&contract, &declared));

        apply(&mut runner, &contract, declared);
        assert_eq!(blockers_of(&runner, angler), vec![bears]);
        assert_eq!(blockers_of(&runner, giant), vec![ogre]);
    }

    /// CR 508.1b: a creature can attack a planeswalker, which its line names
    /// as the board does.
    #[test]
    fn a_seat_attacks_a_planeswalker() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let jace = scenario
            .add_planeswalker_from_oracle(P1, "Jace Beleren", "Jace", 3, JACE_BELEREN)
            .id();
        let mut runner = scenario.build();
        pass_to_declaration(&mut runner);

        let (contract, request) = decision(&runner, P0);
        option_number(
            &request,
            "Grizzly Bears (controlled by You) attacks Player 1",
        );
        let declared = answer(
            &runner,
            &contract,
            &request,
            &[option_number(
                &request,
                "Grizzly Bears (controlled by You) attacks Jace Beleren (controlled by Player 1)",
            )],
        )
        .expect("attacking a planeswalker is a legal declaration");

        apply(&mut runner, &contract, declared);
        assert_eq!(
            attacks(&runner),
            vec![(bears, AttackTarget::Planeswalker(jace))]
        );
    }

    /// Jev ranks the options it is asked about and cannot name a set, so it is
    /// asked about the engine's candidates, as before, and its answer binds to
    /// the candidate it chose.
    #[test]
    fn jev_still_chooses_among_the_engines_candidates() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_creature(P0, "Hill Giant", 3, 3);
        scenario.add_creature(P0, "Grizzly Bears", 2, 2);
        let mut runner = scenario.build();
        pass_to_declaration(&mut runner);

        let (contract, request) = decision(&runner, P0);
        let candidates = &request.prompt.frame.options;
        assert_eq!(candidates.len(), contract.candidates.len());
        let endpoint = LlmEndpointConfig {
            provider: LlmProvider::Jev,
            base_url: Some("https://relay.test".to_string()),
            api_key: "k".to_string(),
            model: "jev-latest".to_string(),
            max_output_tokens: None,
            temperature: None,
            decision_context: DecisionContext::SelfContained,
        };
        let spec = build_chat_request(&endpoint, &request.prompt).expect("a Jev request");
        let body: serde_json::Value = serde_json::from_str(&spec.body).unwrap();
        let mut criteria: Vec<String> = body["request"]["questions"]["pick"]["criteria"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        criteria.sort();
        let keys = system_one_option_keys(candidates);
        assert_eq!(criteria, keys);

        // The candidate that sends both creatures in.
        let both = contract
            .candidates
            .iter()
            .position(|candidate| {
                matches!(
                    &candidate.action,
                    GameAction::DeclareAttackers { attacks, .. } if attacks.len() == 2
                )
            })
            .expect("the engine offers the attack with everything");
        let key = &keys[both];
        let answer = json!({
            "answers": { "pick": {
                "type": "choice",
                "choice": key,
                "probabilities": { key: 1.0 },
                "confidence": 0.9,
            }},
        })
        .to_string();
        let reply = LlmReply {
            provider: LlmProvider::Jev,
            status: 200,
            body: &answer,
        };
        let selection =
            select_action_from_response(runner.state(), &contract, &request.fingerprint, reply)
                .expect("Jev's choice binds");
        assert_eq!(selection.action, contract.candidates[both].action);
    }

    /// A declaration states its own instruction outside the data block, in the
    /// full prompt and in a message-thread continue alike, under the system
    /// prompt every other decision of the seat's is asked under.
    #[test]
    fn a_declaration_keeps_the_seats_system_prompt_and_states_its_instruction() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_creature(P0, "Hill Giant", 3, 3);
        scenario.add_land_to_hand(P0, "Forest");
        let mut runner = scenario.build();
        let (_, main_phase) = decision(&runner, P0);
        pass_to_declaration(&mut runner);
        let (_, declaration) = decision(&runner, P0);

        assert_eq!(declaration.prompt.system, main_phase.prompt.system);
        assert!(declaration.prompt.system.contains(GAME_RESPONSE_CONTRACT));

        let instruction = "Choose a list of options: every attack you list is declared together";
        let update = declaration
            .thread_update(&[], None)
            .expect("a thread that has seen nothing can continue");
        for message in [&declaration.prompt.user, &update] {
            let (data, outside) = message.split_once(UNTRUSTED_DATA_END).unwrap();
            assert!(
                data.contains("[1] Hill Giant (controlled by You) attacks Player 1"),
                "{message}"
            );
            assert!(outside.contains(instruction), "{message}");
            assert!(!data.contains(instruction), "{message}");
        }
    }
}
