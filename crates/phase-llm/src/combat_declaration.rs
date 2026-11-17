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
//! option, and it answers with every pair it declares. Whether those pairs make
//! a legal declaration is the engine's call, made by its completion authority
//! ([`complete_attacker_proposal`], [`complete_blocker_proposal`]): a
//! declaration it would change is refused, never replaced by one the model did
//! not choose.
//!
//! A System One provider (Jev) answers by ranking options and cannot name a
//! set of them, so it keeps choosing one of the engine's candidates
//! ([`crate::game_decision`]).

use engine::game::combat::{
    complete_attacker_proposal, complete_blocker_proposal, validate_attack_declaration,
    validate_blockers_for_player, AttackTarget, CombatTaxPosture,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

use crate::error::{LlmError, LlmResult};
use crate::prompt::option_value;
use crate::render::action::{describe_attack, describe_block};

/// Why the engine changed a declaration its own validator accepts: under
/// [`CombatTaxPosture::Refuse`] the completion drops any declaration that
/// carries a cost to attack or block (CR 508.1h + CR 509.1d).
const TAXED_DECLARATION: &str = "it carries a cost to attack or block";

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

    /// The engine-authored instruction stated outside the data block: the
    /// model answers with a list, which [`crate::prompt::GAME_RESPONSE_CONTRACT`]
    /// describes.
    pub(crate) fn instruction(&self) -> &'static str {
        match (&self.pairs, &self.nothing) {
            (DeclarationPairs::Attacks(_), Some(_)) => {
                "Choose a list of options: every attack you list is declared together, and \
                 each creature attacks only once. To declare no attackers, choose 0 alone."
            }
            (DeclarationPairs::Attacks(_), None) => {
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

/// The pairs one pending combat declaration is composed from, in the order
/// the engine lists them.
#[derive(Debug)]
enum DeclarationPairs {
    /// CR 508.1a + CR 508.1b: each creature that can attack, with each
    /// player, planeswalker or battle it can attack.
    Attacks(Vec<(ObjectId, AttackTarget)>),
    /// CR 509.1a: each creature the defending player controls that can block,
    /// with each attacker it can block.
    Blocks {
        defender: PlayerId,
        blocks: Vec<(ObjectId, ObjectId)>,
    },
}

impl DeclarationPairs {
    /// The pairs of the declaration the engine is waiting on `owner` to make.
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
                    .collect();
                Some(Self::Attacks(attacks))
            }
            WaitingFor::DeclareBlockers {
                player,
                valid_blocker_ids,
                valid_block_targets,
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
                    .collect();
                Some(Self::Blocks {
                    defender: *player,
                    blocks,
                })
            }
            _ => None,
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Attacks(attacks) => attacks.len(),
            Self::Blocks { blocks, .. } => blocks.len(),
        }
    }

    /// The label of the empty declaration's option.
    fn nothing_label(&self) -> &'static str {
        match self {
            Self::Attacks(_) => "Declare no attackers",
            Self::Blocks { .. } => "Declare no blockers",
        }
    }

    /// One label per pair, named as `viewer` sees the board in `visible`.
    fn labels(&self, visible: &GameState, viewer: PlayerId) -> Vec<String> {
        match self {
            Self::Attacks(attacks) => attacks
                .iter()
                .map(|&(attacker, target)| describe_attack(visible, viewer, attacker, target))
                .collect(),
            Self::Blocks { blocks, .. } => blocks
                .iter()
                .map(|&(blocker, attacker)| describe_block(visible, viewer, blocker, attacker))
                .collect(),
        }
    }

    /// The declaration the pairs at `chosen` make, or why the engine refuses
    /// it.
    ///
    /// CR 508.1c + CR 508.1d + CR 509.1b + CR 509.1c: the completion
    /// authority returns a declaration unchanged only when it breaks no
    /// restriction and obeys as many requirements as any declaration could.
    /// Under [`CombatTaxPosture::Refuse`] it also changes any declaration that
    /// carries a cost to attack or block (CR 508.1h + CR 509.1d), as the
    /// engine's own candidates are completed.
    fn declare(&self, state: &GameState, chosen: &[usize]) -> Result<GameAction, String> {
        match self {
            Self::Attacks(pairs) => {
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
                    .unwrap_or_else(|| TAXED_DECLARATION.to_string()))
            }
            Self::Blocks {
                defender,
                blocks: pairs,
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
                    .unwrap_or_else(|| TAXED_DECLARATION.to_string()))
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
    const GHOSTLY_PRISON: &str = "Creatures can't attack you unless their controller pays {2} \
         for each creature they control that's attacking you.";

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

    /// CR 509.1a: several creatures can block one attacker. Neither of the
    /// seat's creatures kills the Craw Wurm alone; together they deal 5 to
    /// its 4 toughness.
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
                        "{name} (controlled by You) blocks Boggart Brute (controlled by Player 0)"
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
                "Grizzly Bears (controlled by You) blocks Boggart Brute (controlled by Player 0)",
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

    /// CR 508.1a: a creature is declared as an attacker once, so naming it at
    /// two players is refused.
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

    /// CR 508.1d + CR 508.1h: attacking past Ghostly Prison costs mana. The
    /// seat's declarations are completed as the engine's own candidates are,
    /// refusing any cost to attack, so the taxed attack is refused while the
    /// free one beside it is declared.
    #[test]
    fn an_attack_that_carries_a_cost_is_refused() {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        scenario.add_enchantment_from_oracle(P1, "Ghostly Prison", GHOSTLY_PRISON);
        let mut runner = scenario.build();
        pass_to_declaration(&mut runner);

        let (contract, request) = decision(&runner, P0);
        let taxed = option_number(
            &request,
            "Grizzly Bears (controlled by You) attacks Player 1",
        );
        let free = option_number(
            &request,
            "Grizzly Bears (controlled by You) attacks Player 2",
        );
        let detail = refusal(answer(&runner, &contract, &request, &[taxed]));
        assert!(detail.ends_with(TAXED_DECLARATION), "{detail}");
        assert_eq!(
            answer(&runner, &contract, &request, &[free]).expect("an untaxed attack"),
            GameAction::DeclareAttackers {
                attacks: vec![(bears, AttackTarget::Player(P2))],
                bands: vec![],
            }
        );
    }

    /// CR 508.1d: a creature that attacks each combat if able makes declaring
    /// no attackers illegal, so that option is not offered, and a declaration
    /// that leaves the creature home is refused.
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
            "Bloodrock Cyclops (controlled by You) attacks Player 1",
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
                matches!(&candidate.action, GameAction::DeclareAttackers { attacks, .. } if attacks.len() == 2)
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
