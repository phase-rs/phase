//! Creature-type named-choice policy.
//!
//! Report (Discord: Radiant Destiny → "Alien"; Patchwork Banner → "Aetherborn"
//! with Villains on the battlefield and in the command zone; Reflections of
//! Littjara → "Advisor" in a Faerie deck): when the engine raises
//! `WaitingFor::NamedChoice { choice_type: ChoiceType::CreatureType { .. } }`,
//! the AI picks the first option alphabetically. Nothing was wrong in the
//! engine — the creature-type list is offered in sorted order, no policy fired
//! on the `GameAction::ChooseOption` candidates, and the tie was broken by
//! candidate string order.
//!
//! This scores each offered type by how many creature-type members the AI
//! actually has across the three zones an anthem / lord / "choose a creature
//! type" payoff can see or grow into: its battlefield, its command zone (a
//! commander's types are a standing tribal commitment even before it is cast),
//! and its hand. The deck's detected dominant tribe is a strict tiebreak worth
//! strictly less than one live member, so a type with a real body on the board
//! always outranks the deck's nominal tribe.
//!
//! CR 205.3m: creature types are the subtypes shared by creature and Kindred
//! cards — subtype presence is the authoritative membership test.
//! CR 702.73a: Changeling is every creature type, so a changeling is a member
//! of whichever type is chosen.
//!
//! Limitation: when the AI has no member of any offered type anywhere and no
//! dominant tribe, every option scores 0.0 and the candidate ordering
//! (alphabetical) still decides. That is the honest answer — with zero signal
//! there is nothing to prefer — and it is why the no-signal case emits its own
//! reason kind rather than a score.

use engine::parser::oracle_util::canonicalize_subtype_name;
use engine::types::ability::ChoiceType;
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use super::tribal_lord_priority::count_subtype_members;
use crate::features::DeckFeatures;

/// Members past the fourth add no further discrimination — by then the type is
/// unambiguously the deck's, and the uncapped sum would run a wide board past
/// the strong band into `critical`, which is reserved for game-deciding terms.
const PRESENCE_CAP: usize = 4;

pub struct CreatureTypeChoicePolicy;

impl TacticalPolicy for CreatureTypeChoicePolicy {
    fn id(&self) -> PolicyId {
        PolicyId::CreatureTypeChoice
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        // `decision_kind::classify` routes every `NamedChoice` prompt into the
        // `ActivateAbility` bucket.
        &[DecisionKind::ActivateAbility]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        if matches!(
            state.waiting_for,
            WaitingFor::NamedChoice {
                choice_type: ChoiceType::CreatureType { .. },
                ..
            }
        ) {
            // The prompt-shape gate above is the entire opt-out: every other
            // `ActivateAbility` candidate skips this policy at zero cost. The
            // deck's tribal commitment must NOT scale the verdict either — the
            // zone census, not the deck profile, is what separates the options.
            // activation-constant: creature-type prompt gate, no deck scaling.
            Some(1.0)
        } else {
            None
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let na = || PolicyVerdict::neutral(PolicyReason::new("creature_type_choice_na"));

        let WaitingFor::NamedChoice {
            player,
            choice_type: ChoiceType::CreatureType { .. },
            ..
        } = &ctx.decision.waiting_for
        else {
            return na();
        };
        if *player != ctx.ai_player {
            return na();
        }
        let GameAction::ChooseOption { choice } = &ctx.candidate.action else {
            return na();
        };

        let canon = canonicalize_subtype_name(choice);

        // CR 205.3m + CR 702.73a: one census, three zone/controller predicates.
        let battlefield = count_subtype_members(
            ctx.state,
            ctx.state.battlefield.iter().copied(),
            |obj| obj.controller == ctx.ai_player,
            &canon,
        );
        // CR 903.6: each player's commander starts in the command zone, so its
        // creature types are a standing part of the deck's tribal commitment
        // even on an empty board.
        let command = count_subtype_members(
            ctx.state,
            ctx.state.command_zone.iter().copied(),
            |obj| obj.owner == ctx.ai_player,
            &canon,
        );
        let hand = ctx
            .state
            .players
            .iter()
            .find(|p| p.id == ctx.ai_player)
            .map_or(0, |p| {
                count_subtype_members(ctx.state, p.hand.iter().copied(), |_| true, &canon)
            });
        let presence = battlefield + command + hand;

        let tribe_match = ctx
            .context
            .session
            .features
            .get(&ctx.ai_player)
            .and_then(|f| f.tribal.dominant_tribe.as_deref())
            .map(canonicalize_subtype_name)
            .is_some_and(|dominant| dominant == canon);

        let penalties = &ctx.config.policy_penalties;
        let total = presence.min(PRESENCE_CAP) as f64 * penalties.creature_type_presence_unit
            + if tribe_match {
                penalties.creature_type_tribe_bonus
            } else {
                0.0
            };

        if total == 0.0 {
            return PolicyVerdict::neutral(PolicyReason::new("creature_type_choice_no_signal"));
        }

        PolicyVerdict::score(
            total,
            PolicyReason::new("creature_type_choice")
                .with_fact("presence", presence as i64)
                .with_fact("tribe_match", tribe_match as i64),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use crate::context::AiContext;
    use crate::features::tribal::TribalFeature;
    use crate::policies::registry::PolicyRegistry;
    use crate::session::AiSession;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::keywords::Keyword;
    use engine::types::mana::ManaColor;
    use engine::types::zones::Zone;
    use std::sync::Arc;

    const AI: PlayerId = PlayerId(0);
    const OPPONENT: PlayerId = PlayerId(1);

    fn creature_type_decision() -> AiDecisionContext {
        decision_for(
            AI,
            ChoiceType::CreatureType {
                options: Vec::new(),
            },
        )
    }

    fn decision_for(player: PlayerId, choice_type: ChoiceType) -> AiDecisionContext {
        AiDecisionContext {
            waiting_for: WaitingFor::NamedChoice {
                player,
                choice_type,
                options: ["Advisor", "Faerie", "Villain"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
                source: None,
                persist_player: None,
                free_entry: None,
            },
            candidates: Vec::new(),
        }
    }

    fn choose_candidate(choice: &str) -> CandidateAction {
        CandidateAction {
            action: GameAction::ChooseOption {
                choice: choice.to_string(),
            },
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Selection),
        }
    }

    fn context_with_dominant_tribe(dominant: Option<&str>) -> (AiContext, AiConfig) {
        let config = AiConfig::default();
        let mut session = AiSession::empty();
        session.features.insert(
            AI,
            DeckFeatures {
                tribal: TribalFeature {
                    dominant_tribe: dominant.map(|s| s.to_string()),
                    commitment: 0.8,
                    tribes: Vec::new(),
                },
                ..Default::default()
            },
        );
        let mut context = AiContext::empty(&config.weights);
        context.session = Arc::new(session);
        context.player = AI;
        (context, config)
    }

    fn put_creature(
        state: &mut GameState,
        card_id: CardId,
        owner: PlayerId,
        name: &str,
        zone: Zone,
        subtypes: &[&str],
    ) -> ObjectId {
        let oid = create_object(state, card_id, owner, name.to_string(), zone);
        state.objects.get_mut(&oid).unwrap().card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Creature],
            subtypes: subtypes.iter().map(|s| s.to_string()).collect(),
        };
        oid
    }

    /// Score one offered option against a prepared state.
    fn score(
        state: &GameState,
        decision: &AiDecisionContext,
        context: &AiContext,
        config: &AiConfig,
        choice: &str,
    ) -> f64 {
        let candidate = choose_candidate(choice);
        let ctx = PolicyContext {
            state,
            decision,
            candidate: &candidate,
            ai_player: AI,
            config,
            context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        match CreatureTypeChoicePolicy.verdict(&ctx) {
            PolicyVerdict::Score { delta, .. } => delta,
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }

    #[test]
    fn board_presence_outranks_alphabetical_first() {
        let mut state = GameState::new_two_player(42);
        put_creature(
            &mut state,
            CardId(1),
            AI,
            "Spellstutter Sprite",
            Zone::Battlefield,
            &["Faerie"],
        );
        put_creature(
            &mut state,
            CardId(2),
            AI,
            "Faerie Miscreant",
            Zone::Battlefield,
            &["Faerie"],
        );

        let decision = creature_type_decision();
        let (context, config) = context_with_dominant_tribe(None);
        let faerie = score(&state, &decision, &context, &config, "Faerie");
        let advisor = score(&state, &decision, &context, &config, "Advisor");

        assert_eq!(advisor, 0.0, "no Advisor anywhere ⇒ no signal");
        assert!(
            faerie > advisor,
            "two board Faeries must outrank the alphabetically-first option ({faerie} vs {advisor})"
        );
    }

    #[test]
    fn command_zone_commander_subtype_counts() {
        let mut state = GameState::new_two_player(42);
        put_creature(
            &mut state,
            CardId(1),
            AI,
            "Tivit, Seller of Secrets",
            Zone::Command,
            &["Villain"],
        );

        let decision = creature_type_decision();
        let (context, config) = context_with_dominant_tribe(None);
        let villain = score(&state, &decision, &context, &config, "Villain");

        assert!(villain > 0.0, "an AI-owned command-zone Villain must score");
        assert_eq!(score(&state, &decision, &context, &config, "Faerie"), 0.0);
    }

    #[test]
    fn hand_subtypes_count() {
        let mut state = GameState::new_two_player(42);
        put_creature(
            &mut state,
            CardId(1),
            AI,
            "Faerie Mastermind",
            Zone::Hand,
            &["Faerie"],
        );

        let decision = creature_type_decision();
        let (context, config) = context_with_dominant_tribe(None);
        assert!(score(&state, &decision, &context, &config, "Faerie") > 0.0);
        assert_eq!(score(&state, &decision, &context, &config, "Advisor"), 0.0);
    }

    #[test]
    fn dominant_tribe_is_a_tiebreak_only() {
        let empty = GameState::new_two_player(42);
        let decision = creature_type_decision();
        let (context, config) = context_with_dominant_tribe(Some("Faerie"));

        let faerie = score(&empty, &decision, &context, &config, "Faerie");
        assert!(
            faerie > 0.0,
            "with no board the dominant tribe still breaks the tie"
        );
        assert_eq!(score(&empty, &decision, &context, &config, "Advisor"), 0.0);

        // One live Villain must beat the deck's nominal Faerie tribe.
        let mut state = GameState::new_two_player(42);
        put_creature(
            &mut state,
            CardId(1),
            AI,
            "Tivit, Seller of Secrets",
            Zone::Battlefield,
            &["Villain"],
        );
        let villain = score(&state, &decision, &context, &config, "Villain");
        let faerie_with_board = score(&state, &decision, &context, &config, "Faerie");
        assert!(
            villain > faerie_with_board,
            "one real member ({villain}) must outrank the dominant tribe alone ({faerie_with_board})"
        );
    }

    #[test]
    fn changeling_in_hand_counts_for_every_option() {
        let mut state = GameState::new_two_player(42);
        let changeling = put_creature(
            &mut state,
            CardId(1),
            AI,
            "Universal Automaton",
            Zone::Hand,
            &[],
        );
        state
            .objects
            .get_mut(&changeling)
            .unwrap()
            .keywords
            .push(Keyword::Changeling);

        let decision = creature_type_decision();
        let (context, config) = context_with_dominant_tribe(None);
        let unit = config.policy_penalties.creature_type_presence_unit;

        // CR 702.73a: every offered type gets exactly one member, never two.
        for option in ["Advisor", "Faerie", "Villain"] {
            assert_eq!(
                score(&state, &decision, &context, &config, option),
                unit,
                "{option} must count the changeling exactly once"
            );
        }
    }

    #[test]
    fn opponent_creatures_do_not_count() {
        let mut state = GameState::new_two_player(42);
        put_creature(
            &mut state,
            CardId(1),
            OPPONENT,
            "Spellstutter Sprite",
            Zone::Battlefield,
            &["Faerie"],
        );
        put_creature(
            &mut state,
            CardId(2),
            OPPONENT,
            "Tivit, Seller of Secrets",
            Zone::Command,
            &["Villain"],
        );

        let decision = creature_type_decision();
        let (context, config) = context_with_dominant_tribe(None);
        assert_eq!(score(&state, &decision, &context, &config, "Faerie"), 0.0);
        assert_eq!(score(&state, &decision, &context, &config, "Villain"), 0.0);
    }

    #[test]
    fn non_creature_type_choice_is_na() {
        let mut state = GameState::new_two_player(42);
        put_creature(
            &mut state,
            CardId(1),
            AI,
            "Spellstutter Sprite",
            Zone::Battlefield,
            &["Faerie"],
        );

        let decision = decision_for(
            AI,
            ChoiceType::Color {
                excluded: Vec::<ManaColor>::new(),
            },
        );
        let (context, config) = context_with_dominant_tribe(Some("Faerie"));
        assert_eq!(score(&state, &decision, &context, &config, "Faerie"), 0.0);
    }

    #[test]
    fn other_players_choice_is_na() {
        let mut state = GameState::new_two_player(42);
        put_creature(
            &mut state,
            CardId(1),
            AI,
            "Spellstutter Sprite",
            Zone::Battlefield,
            &["Faerie"],
        );

        let decision = decision_for(
            OPPONENT,
            ChoiceType::CreatureType {
                options: Vec::new(),
            },
        );
        let (context, config) = context_with_dominant_tribe(Some("Faerie"));
        assert_eq!(score(&state, &decision, &context, &config, "Faerie"), 0.0);
    }

    #[test]
    fn activation_opts_out_when_the_prompt_is_not_a_creature_type_choice() {
        let features = DeckFeatures::default();
        let mut state = GameState::new_two_player(42);
        assert!(
            CreatureTypeChoicePolicy
                .activation(&features, &state, AI)
                .is_none(),
            "a Priority prompt must not pay this policy's verdict cost"
        );

        state.waiting_for = WaitingFor::NamedChoice {
            player: AI,
            choice_type: ChoiceType::CreatureType {
                options: Vec::new(),
            },
            options: Vec::new(),
            source: None,
            persist_player: None,
            free_entry: None,
        };
        assert_eq!(
            CreatureTypeChoicePolicy.activation(&features, &state, AI),
            Some(1.0)
        );
    }

    #[test]
    fn registry_registers_creature_type_choice() {
        assert!(PolicyRegistry::default().has_policy(PolicyId::CreatureTypeChoice));
    }
}
