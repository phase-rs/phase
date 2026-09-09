//! End-to-end pin for the activation-time own-board veto.
//!
//! Report (Discord #ai-suggestions): "In a game my AI opponent sacrificed their
//! own Expendable Troops to destroy their own attacking Serra Advocate."
//!
//! Both cards are real and their Oracle text is verified against
//! `data/card-data.json`; this fixture parses that text through the shipped
//! Oracle parser rather than hand-building an AST, so a parser shape change
//! fails these tests instead of leaving them green on a shape no card produces.
//!
//! ```text
//! Expendable Troops  {1}{W}  2/1
//!   {T}, Sacrifice this creature: It deals 2 damage to target attacking or
//!   blocking creature.
//! Serra Advocate     {3}{W}  2/2  Flying
//!   {T}: Target attacking or blocking creature gets +2/+2 until end of turn.
//! ```
//!
//! # Why the pre-existing veto could not catch this
//!
//! `anti_self_harm::own_permanent_with_opponent_alternative` is slot-local: it
//! fires only while the SAME target slot still offers an opponent-controlled
//! object to prefer instead. On the reported board the AI was the only player
//! attacking, so "target attacking or blocking creature" admitted exactly one
//! legal target — the AI's own Serra Advocate. With no alternative to prefer,
//! that veto stands down by construction.
//!
//! And it would have been too late regardless. CR 601.2c announces targets,
//! CR 601.2h pays costs, and CR 602.2b applies that whole sequence to
//! activating an ability — so the Troops is already sacrificed by the time any
//! target-step policy runs. The veto has to live on the ACTIVATION decision,
//! which is what these tests pin.
//!
//! # The two arms
//!
//! The negative arm is the reported board. The positive control is the same
//! board plus an opposing blocker: the veto must NOT fire there, or it would
//! pass this file while quietly deleting the card's entire purpose. A blanket
//! "never activate Expendable Troops" would satisfy the negative arm alone.

use std::sync::Arc;

use engine::game::combat::{AttackTarget, AttackerInfo, CombatState};
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::AbilityDefinition;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};

use crate::config::{create_config, AiConfig, AiDifficulty, Platform};
use crate::context::AiContext;
use crate::policies::anti_self_harm::AntiSelfHarmPolicy;
use crate::policies::context::{PolicyContext, SearchDepth};
use crate::policies::registry::{PolicyVerdict, TacticalPolicy};
use crate::score_candidates;
use crate::session::AiSession;

const AI: PlayerId = PlayerId(0);
const OPP: PlayerId = PlayerId(1);

/// Per-run card-id source, deliberately not a process-global atomic so both
/// arms build byte-identical boards regardless of test-runner ordering.
struct Ids(u64);

impl Ids {
    fn new() -> Self {
        Self(9100)
    }
    fn next(&mut self) -> CardId {
        self.0 += 1;
        CardId(self.0)
    }
}

/// The printed card a fixture creature is built from.
struct Printed<'a> {
    name: &'a str,
    oracle_text: &'a str,
    keywords: &'a [&'a str],
    power: i32,
    toughness: i32,
}

/// A creature carrying the shipped parse of its Oracle text.
fn printed_creature(
    state: &mut GameState,
    ids: &mut Ids,
    owner: PlayerId,
    card: &Printed<'_>,
) -> ObjectId {
    let Printed {
        name,
        oracle_text,
        keywords,
        power,
        toughness,
    } = *card;
    let id = create_object(
        state,
        ids.next(),
        owner,
        name.to_string(),
        Zone::Battlefield,
    );
    let keyword_strings: Vec<String> = keywords.iter().map(|k| (*k).to_string()).collect();
    let parsed = parse_oracle_text(
        oracle_text,
        name,
        &keyword_strings,
        &["Creature".to_string()],
        &[],
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.power = Some(power);
    obj.toughness = Some(toughness);
    obj.summoning_sick = false;
    if keywords.contains(&"Flying") {
        obj.keywords.push(Keyword::Flying);
    }
    *Arc::make_mut(&mut obj.abilities) = parsed.abilities;
    id
}

fn vanilla_creature(
    state: &mut GameState,
    ids: &mut Ids,
    owner: PlayerId,
    name: &str,
    power: i32,
    toughness: i32,
) -> ObjectId {
    let id = create_object(
        state,
        ids.next(),
        owner,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.power = Some(power);
    obj.toughness = Some(toughness);
    obj.summoning_sick = false;
    id
}

struct Board {
    state: GameState,
    troops: ObjectId,
    advocate: ObjectId,
    blocker: Option<ObjectId>,
}

/// The reported board: AI attacking with Serra Advocate, Expendable Troops
/// untapped and able to activate. `with_blocker` adds an opposing creature
/// blocking the Advocate, which is the only difference between the two arms.
fn build_board(with_blocker: bool) -> Board {
    build_board_at_life(with_blocker, None)
}

fn build_board_at_life(with_blocker: bool, opponent_life: Option<i32>) -> Board {
    let mut ids = Ids::new();
    let mut state = GameState::new_two_player(4242);
    // CR 509: blockers are declared, so both "attacking" and "blocking" legs of
    // the Or filter are live and the ability is activatable.
    state.phase = Phase::DeclareBlockers;
    state.active_player = AI;
    state.priority_player = AI;

    let troops = printed_creature(
        &mut state,
        &mut ids,
        AI,
        &Printed {
            name: "Expendable Troops",
            oracle_text: "{T}, Sacrifice this creature: It deals 2 damage to target attacking \
                          or blocking creature.",
            keywords: &[],
            power: 2,
            toughness: 1,
        },
    );
    let advocate = printed_creature(
        &mut state,
        &mut ids,
        AI,
        &Printed {
            name: "Serra Advocate",
            oracle_text: "Flying\n{T}: Target attacking or blocking creature gets +2/+2 until \
                          end of turn.",
            keywords: &["Flying"],
            power: 2,
            toughness: 2,
        },
    );

    let mut combat = CombatState::default();
    combat.attackers.push(AttackerInfo {
        object_id: advocate,
        defending_player: OPP,
        attack_target: AttackTarget::Player(OPP),
        blocked: with_blocker,
        band_id: None,
    });
    state.objects.get_mut(&advocate).unwrap().tapped = true;

    let blocker = with_blocker.then(|| {
        // A 4/2: dies to the Troops' 2 damage, and is worth enough
        // (1.5*4 + 2 = 8.0) that trading a 2/1 for it is plainly correct.
        let id = vanilla_creature(&mut state, &mut ids, OPP, "Opposing Blocker", 4, 2);
        // CR 509.1a: the two halves of the blocking relationship, plus the
        // declaration record the engine's `Blocking` filter property reads.
        combat.blocker_assignments.insert(advocate, vec![id]);
        combat.blocker_to_attacker.insert(id, vec![advocate]);
        combat.blockers_declared_by.push(OPP);
        id
    });
    state.combat = Some(combat);

    if let Some(life) = opponent_life {
        state.players[OPP.0 as usize].life = life;
    }

    state.waiting_for = WaitingFor::Priority { player: AI };
    Board {
        state,
        troops,
        advocate,
        blocker,
    }
}

/// Score of the Expendable Troops activation through the production pipeline,
/// plus the engine's own count of how many times that activation was offered.
///
/// The offered count is the NON-VACUITY probe: without it, "the AI did not
/// activate" is indistinguishable from "the fixture never presented the choice".
fn measure(board: &Board, search_enabled: bool) -> (usize, Option<f64>) {
    let offered = engine::ai_support::legal_actions(&board.state)
        .iter()
        .filter(|a| {
            matches!(a, GameAction::ActivateAbility { source_id, .. } if *source_id == board.troops)
        })
        .count();

    let mut config = create_config(AiDifficulty::Medium, Platform::Native);
    config.search.enabled = search_enabled;
    let score = score_candidates(&board.state, AI, &config)
        .into_iter()
        .find(|(a, _)| {
            matches!(a, GameAction::ActivateAbility { source_id, .. } if *source_id == board.troops)
        })
        .map(|(_, s)| s);

    (offered, score)
}

/// The reported board. With no opposing attacker or blocker, the ONLY legal
/// target is the AI's own Serra Advocate, so activating spends a 2/1 to kill a
/// 2/2 the AI controls. The activation must not survive to selection.
///
/// Like the low-life arm below, this is an OUTCOME guard: on a board this
/// simple the engine's `targeted_exchange` gate declines it too, so removing
/// the policy veto alone leaves this green. Keep it anyway — it is the arm that
/// states the required behaviour — and read
/// [`the_own_board_veto_is_the_mechanism`] for the discriminating claim.
#[test]
fn own_lone_attacker_is_never_shot_with_or_without_search() {
    for search_enabled in [false, true] {
        let board = build_board(false);

        let (offered, score) = measure(&board, search_enabled);
        assert!(
            offered > 0,
            "search={search_enabled}: fixture must present the Expendable Troops activation, \
             or this test proves nothing"
        );
        // A rejected candidate is dropped from the scored set entirely; if it
        // does survive, it must at least carry the `-inf` a `Reject` produces.
        if let Some(score) = score {
            assert!(
                score.is_infinite() && score.is_sign_negative(),
                "search={search_enabled}: shooting the AI's own lone attacker must be \
                 REJECTED, got {score}. CR 601.2c + CR 601.2h: targets and costs are \
                 announced in one process, so the Troops is already sacrificed by the time \
                 any target-step veto could run — the activation decision is the last \
                 window to decline"
            );
        }
        // The Advocate is untouched, and the Troops is still around to be spent
        // on a real target later.
        assert_eq!(
            board
                .state
                .objects
                .get(&board.advocate)
                .unwrap()
                .damage_marked,
            0
        );
    }
}

/// Positive control, and the reason this file is not vacuous: add ONE opposing
/// blocker and the very same activation becomes correct. A blanket "never
/// activate Expendable Troops" would satisfy the negative arm above while
/// deleting the card's entire purpose; this arm fails on such a fix.
#[test]
fn an_opposing_blocker_makes_the_same_activation_available_again() {
    for search_enabled in [false, true] {
        let board = build_board(true);
        assert!(board.blocker.is_some(), "positive arm must have a blocker");

        let (offered, score) = measure(&board, search_enabled);
        assert!(
            offered > 0,
            "search={search_enabled}: activation must be offered"
        );
        let score = score.expect("a legal activation is always scored");
        assert!(
            score.is_finite(),
            "search={search_enabled}: with a 4/2 blocker legal for the SAME 'attacking or \
             blocking creature' filter, the own-board veto must stand down — got {score}"
        );
    }
}

/// Build the `AntiSelfHarmPolicy` verdict for the Expendable Troops activation
/// on `board`.
fn anti_self_harm_verdict(board: &Board) -> PolicyVerdict {
    let config = AiConfig::default();
    let mut session = AiSession::empty();
    session.features.insert(AI, Default::default());
    let mut context = AiContext::empty(&config.weights);
    context.session = Arc::new(session);
    context.player = AI;

    let candidate = CandidateAction {
        action: GameAction::ActivateAbility {
            source_id: board.troops,
            ability_index: 0,
        },
        metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Ability),
    };
    let decision = AiDecisionContext {
        waiting_for: WaitingFor::Priority { player: AI },
        candidates: Vec::new(),
    };
    let ctx = PolicyContext {
        state: &board.state,
        decision: &decision,
        candidate: &candidate,
        ai_player: AI,
        config: &config,
        context: &context,
        cast_facts: None,
        search_depth: SearchDepth::Root,
    };
    AntiSelfHarmPolicy.verdict(&ctx)
}

fn reject_kind(verdict: &PolicyVerdict) -> Option<&'static str> {
    match verdict {
        PolicyVerdict::Reject { reason } => Some(reason.kind),
        PolicyVerdict::Score { .. } => None,
    }
}

/// Pins WHICH mechanism declines the activation.
///
/// The pipeline arm above only proves the misplay does not happen; several
/// policies could produce that outcome on this board (`SelfCostValuePolicy`
/// would also price a 2/1 sacrifice against a payoff it reads as trivial). If
/// the own-board veto silently stopped firing, this file would keep passing on
/// the wrong mechanism — and would stop protecting every board where the other
/// policies do not happen to agree. So assert the reason by name.
#[test]
fn the_own_board_veto_is_the_mechanism() {
    let board = build_board(false);
    assert_eq!(
        reject_kind(&anti_self_harm_verdict(&board)),
        Some("anti_self_harm_harmful_activation_own_board_only"),
        "the activation-time own-board veto must be what declines this"
    );
}

/// The same assertion on the positive-control board: the veto must be SILENT
/// once an opposing blocker is legal for the very same filter.
#[test]
fn the_own_board_veto_is_silent_when_an_opponent_is_reachable() {
    let board = build_board(true);
    assert_eq!(
        reject_kind(&anti_self_harm_verdict(&board)),
        None,
        "with an opposing blocker legal for the same 'attacking or blocking creature' \
         filter, the own-board veto must not fire"
    );
}

/// An opponent's life total must not license a creature-only ability.
///
/// CR 115.4: "target attacking or blocking creature" can never be pointed at a
/// player, so how low an opponent is has no bearing on whether this activation
/// is worth making. Pre-fix, `self_cost::filter_can_target_player` fell open
/// (`_ => true`) on the `Or` filter and let `damage_lethal_to_opponent` be
/// consulted anyway — a creature-only slot reading a life total it could never
/// reach. `filter_domain_tests::attacking_or_blocking_creature_is_creature_only`
/// pins that unit answer directly; this is the pipeline-level guard on it.
///
/// MEASURED, and worth recording: on a board this simple the engine's own
/// `ai_support::targeted_exchange` gate also declines this activation
/// independently (`gate_candidates` keeps 0 of 1), so this arm is an OUTCOME
/// guard, not a discriminating one — reverting both fixes leaves it green. The
/// discriminating assertion for the veto is
/// [`the_own_board_veto_is_the_mechanism`], which names the mechanism and does
/// fail when the veto is removed.
#[test]
fn a_low_opponent_life_total_cannot_justify_a_creature_only_ability() {
    for search_enabled in [false, true] {
        let board = build_board_at_life(false, Some(2));
        assert_eq!(board.state.players[OPP.0 as usize].life, 2);

        let (offered, score) = measure(&board, search_enabled);
        assert!(
            offered > 0,
            "search={search_enabled}: fixture must present the activation"
        );
        if let Some(score) = score {
            assert!(
                score.is_infinite() && score.is_sign_negative(),
                "search={search_enabled}: 'target attacking or blocking creature' (CR 115.4) \
                 can never be pointed at a player, so an opponent at 2 life must not make \
                 shooting the AI's OWN attacker look worthwhile — got {score}"
            );
        }
    }
}

/// Review finding (PR #8696): the veto's `CastSpell` arm read activated
/// abilities the cast never commits to, and hard-`Reject`ed the spell.
///
/// `PolicyContext::effects()`'s `CastSpell` arm walks EVERY printed
/// `AbilityDefinition` on the source with no `kind` filter, activated abilities
/// included — unlike this file's own `action_ability_definitions`, whose
/// `CastSpell` arm carries `.filter(|ability| ability.kind == AbilityKind::Spell)`.
///
/// Royal Assassin ("{T}: Destroy target tapped creature.", verified against
/// `data/card-data.json`) is the sharpest case. Cast it while the AI controls a
/// tapped creature and the opponent controls none, and the veto read the `{T}`
/// ability's `Effect::Destroy` — an ability that is not even activatable yet,
/// the creature having summoning sickness — found a non-empty own-board-only
/// legal-target pool, and rejected the CAST. A `Reject` is `-inf`, so the
/// candidate is deleted rather than mispriced: the AI refused to deploy Royal
/// Assassin precisely when it was ahead on board.
///
/// CR 601.2c / CR 601.2h / CR 602.2b motivate the ACTIVATION case only — the
/// activation is the last window in which the AI can decline. Nothing in that
/// argument reaches the cast, and `score_pre_cast` already prices a genuine
/// no-opponent-target cast with `wasted_cast_penalty` (a soft penalty, by
/// deliberate design). So the `CastSpell` arm is gone.
mod cast_arm {
    use super::*;

    use engine::types::game_state::CastPaymentMode;
    use engine::types::identifiers::CardId as EngineCardId;

    /// Royal Assassin in hand, one tapped creature the AI controls, and no
    /// opposing creature at all.
    fn board_with_assassin_in_hand() -> (GameState, ObjectId, ObjectId) {
        let mut ids = Ids::new();
        let mut state = GameState::new_two_player(4242);
        state.phase = Phase::PreCombatMain;
        state.active_player = AI;
        state.priority_player = AI;

        // The AI's own tapped creature: the only thing "target tapped creature"
        // can legally see on this board.
        let own_tapped = vanilla_creature(&mut state, &mut ids, AI, "Own Body", 2, 2);
        state.objects.get_mut(&own_tapped).unwrap().tapped = true;

        let card_id = ids.next();
        let assassin = create_object(
            &mut state,
            card_id,
            AI,
            "Royal Assassin".to_string(),
            Zone::Hand,
        );
        let parsed = parse_oracle_text(
            "{T}: Destroy target tapped creature.",
            "Royal Assassin",
            &[],
            &["Creature".to_string()],
            &[],
        );
        {
            let obj = state.objects.get_mut(&assassin).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(1);
            obj.toughness = Some(1);
            *Arc::make_mut(&mut obj.abilities) = parsed.abilities;
        }

        state.waiting_for = WaitingFor::Priority { player: AI };
        (state, assassin, own_tapped)
    }

    fn verdict_for_action(state: &GameState, action: GameAction) -> PolicyVerdict {
        let config = AiConfig::default();
        let mut session = AiSession::empty();
        session.features.insert(AI, Default::default());
        let mut context = AiContext::empty(&config.weights);
        context.session = Arc::new(session);
        context.player = AI;

        let candidate = CandidateAction {
            action,
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Spell),
        };
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        };
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: SearchDepth::Root,
        };
        AntiSelfHarmPolicy.verdict(&ctx)
    }

    /// The regression. Casting a creature must never be vetoed because of an
    /// activated ability the cast does not commit to.
    #[test]
    fn casting_a_creature_is_not_vetoed_by_its_own_activated_ability() {
        let (state, assassin, _own_tapped) = board_with_assassin_in_hand();
        let card_id: EngineCardId = state.objects.get(&assassin).unwrap().card_id;
        let verdict = verdict_for_action(
            &state,
            GameAction::CastSpell {
                object_id: assassin,
                card_id,
                targets: Vec::new(),
                payment_mode: CastPaymentMode::default(),
            },
        );
        assert_eq!(
            reject_kind(&verdict),
            None,
            "casting Royal Assassin must not be rejected: its {{T}} ability is not something \
             the CAST commits to (CR 601.2c binds targets for the SPELL), and the creature has \
             summoning sickness so the ability is not even activatable yet"
        );
    }

    /// The other half of the same card, and the reason dropping the `CastSpell`
    /// arm costs nothing: ACTIVATING Royal Assassin to destroy the AI's own
    /// tapped creature is exactly the misplay this veto exists for, and it is
    /// still caught.
    #[test]
    fn activating_the_same_ability_at_the_same_board_is_still_vetoed() {
        let (mut state, assassin, _own_tapped) = board_with_assassin_in_hand();
        // Move it to the battlefield and let it be activatable.
        {
            let obj = state.objects.get_mut(&assassin).unwrap();
            obj.zone = Zone::Battlefield;
            obj.summoning_sick = false;
        }
        state.players[AI.0 as usize]
            .hand
            .retain(|&id| id != assassin);
        state.battlefield.push_back(assassin);

        let verdict = verdict_for_action(
            &state,
            GameAction::ActivateAbility {
                source_id: assassin,
                ability_index: 0,
            },
        );
        assert_eq!(
            reject_kind(&verdict),
            Some("anti_self_harm_harmful_activation_own_board_only"),
            "activating Royal Assassin when the only legal 'tapped creature' is the AI's own \
             must still be vetoed"
        );
    }
}

/// Review finding (PR #8696), then a follow-up finding on the fix itself:
/// the veto's per-effect loop reads `ctx.effects()` through
/// `extract_target_filter`, and several REAL, unconditionally beneficial
/// effects — `Effect::Draw` chief among them — are absent from that
/// function's match arms and fall to `_ => None`. Such a leg is silently
/// `continue`d past by `anti_self_harm.rs`'s per-effect loop and never
/// reaches the `EffectPolarity::Beneficial => return None` arm, because that
/// arm only fires for legs the loop actually visits.
///
/// Garruk, Cursed Huntsman's [−3] ("Destroy target creature. Draw a card.",
/// verified against `data/card-data.json`) is the sharpest case: a chained
/// `sub_ability` shape the engine's own parser produces byte-identically from
/// the bare ability line (pinned in `destroy_then_draw_ability` below), so this
/// fixture needs no planeswalker/loyalty machinery to reproduce it faithfully.
/// `Effect::Draw` DOES carry a `target` field (defaulting to `Controller`) —
/// so "it has no target, it can't be missed" is the wrong refutation. The
/// field exists; the VARIANT is simply absent from `extract_target_filter`'s
/// match arms, and only those arms decide what the loop can see.
///
/// The FIRST fix here stopped there — any effect the loop can't see AND that
/// `effect_polarity` calls `Beneficial` stood the veto down. That is not
/// evidence the AI receives a usable payoff: `effect_polarity(Effect::Draw)`
/// is `Beneficial` regardless of WHO draws, and nothing checked deliverability
/// either. So the SAME escape that correctly rescues Garruk's [−3] would
/// ALSO have rescued a synthetic "Destroy target creature. Target opponent
/// draws a card" (a pure gift, not a payoff) and an AI-directed draw off an
/// empty library (no payoff, and risks the CR 104.3b loss an empty-library
/// draw itself creates). `untargeted_effect_confirms_ai_payoff` closes that:
/// it resolves the actual recipient and, for Draw, the actual deliverability,
/// rather than trusting `effect_polarity`'s global answer.
///
/// Four boards, from one shared fixture parameterized by library size and
/// draw recipient:
///
/// 1. AI-recipient, drawable (Garruk's real [−3], library non-empty) — the
///    positive control: this activation must NOT be vetoed.
/// 2. AI-recipient, NOT drawable (same ability, empty library) — still
///    vetoed: no card is actually coming.
/// 3. Opponent-recipient (the Draw leg's target swapped to `Opponent`,
///    synthetic — no printed card does this shape, so it is a hand-modified
///    variant of the real ability rather than a second real card) — still
///    vetoed: the AI gets nothing.
/// 4. No Draw leg at all (bare "Destroy target creature.") — still vetoed:
///    proves the fix isn't a blanket stand-down.
mod split_ability_leg {
    use super::*;
    use engine::types::ability::{Effect, TargetFilter as SplitTargetFilter};

    /// The shipped parser's own output for Garruk's [−3], pinned so a parser
    /// shape change fails this test rather than leaving it green on a shape no
    /// card produces. Matches `data/card-data.json`'s parse of the printed
    /// card: `Effect::Destroy` on the root, `Effect::Draw { target: Controller }`
    /// chained as a `SequentialSibling` `sub_ability`.
    fn destroy_then_draw_ability() -> AbilityDefinition {
        parse_oracle_text(
            "[−3]: Destroy target creature. Draw a card.",
            "Garruk, Cursed Huntsman",
            &[],
            &["Planeswalker".to_string()],
            &[],
        )
        .abilities
        .into_iter()
        .next()
        .expect("Garruk's [-3] parses one activated ability")
    }

    /// `board_with_only_own_creature`'s ability, with its Draw leg's recipient
    /// hand-swapped to `Opponent`. Not a printed card — labeled synthetic in
    /// every test that uses it — built specifically to isolate the RECIPIENT
    /// half of `untargeted_effect_confirms_ai_payoff` from its DELIVERABILITY
    /// half, the same way `without_the_draw_leg_the_same_board_is_still_vetoed`
    /// already isolates "no Draw leg at all" by hand-editing the same root.
    fn destroy_then_gift_opponent_a_draw_ability() -> AbilityDefinition {
        let mut ability = destroy_then_draw_ability();
        let sub = ability
            .sub_ability
            .as_mut()
            .expect("premise: Garruk's [-3] has a sub_ability");
        let Effect::Draw { target, .. } = &mut *sub.effect else {
            panic!("premise: Garruk's [-3] sub_ability is Effect::Draw");
        };
        *target = SplitTargetFilter::Opponent;
        ability
    }

    /// `library_size` cards in the AI's library — 0 makes the Draw leg
    /// undeliverable (CR 121.3), any non-zero count makes it deliverable.
    fn board_with_only_own_creature(
        ability: AbilityDefinition,
        library_size: u32,
    ) -> (GameState, ObjectId, ObjectId) {
        let mut ids = Ids::new();
        let mut state = GameState::new_two_player(4242);
        state.phase = Phase::PreCombatMain;
        state.active_player = AI;
        state.priority_player = AI;

        let own_creature = vanilla_creature(&mut state, &mut ids, AI, "Own Body", 2, 2);

        let planeswalker = create_object(
            &mut state,
            ids.next(),
            AI,
            "Garruk, Cursed Huntsman".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&planeswalker).unwrap();
            obj.card_types.core_types.push(CoreType::Planeswalker);
            *Arc::make_mut(&mut obj.abilities) = vec![ability];
        }

        for i in 0..library_size {
            create_object(
                &mut state,
                ids.next(),
                AI,
                format!("Library Card {i}"),
                Zone::Library,
            );
        }

        state.waiting_for = WaitingFor::Priority { player: AI };
        (state, planeswalker, own_creature)
    }

    fn verdict_for(state: &GameState, source_id: ObjectId) -> PolicyVerdict {
        let config = AiConfig::default();
        let mut session = AiSession::empty();
        session.features.insert(AI, Default::default());
        let mut context = AiContext::empty(&config.weights);
        context.session = Arc::new(session);
        context.player = AI;

        let candidate = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id,
                ability_index: 0,
            },
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Ability),
        };
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        };
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: SearchDepth::Root,
        };
        AntiSelfHarmPolicy.verdict(&ctx)
    }

    /// Case 3 from the module doc, and the reason the other three exist: a
    /// chained "Destroy target creature. Draw a card." whose Destroy leg can
    /// only reach the AI's own creature, with the AI actually able to draw,
    /// must NOT be vetoed — the Draw leg is a real, deliverable payoff the
    /// harmful leg's own-board confinement does not erase.
    #[test]
    fn own_board_destroy_with_a_deliverable_ai_draw_is_not_vetoed() {
        let (state, planeswalker, _own_creature) =
            board_with_only_own_creature(destroy_then_draw_ability(), 10);
        assert_eq!(
            reject_kind(&verdict_for(&state, planeswalker)),
            None,
            "'Destroy target creature. Draw a card.' must not be hard-rejected when the \
             Destroy leg's only legal target is the AI's own creature and the AI can actually \
             draw — the Draw leg is a real, deliverable payoff the veto's per-effect loop \
             must not go blind to"
        );
    }

    /// Case 1: the SAME real ability, but the AI's library is empty. CR
    /// 121.3: a draw attempt from an empty library still occurs (it just puts
    /// nothing in hand) and is the state-based-loss condition, so this is not
    /// merely "no payoff" — it can set up a loss. `can_draw_at_least_one` must
    /// see through the abstract `effect_polarity(Draw) == Beneficial` reading
    /// and keep the veto engaged.
    #[test]
    fn own_board_destroy_with_an_undeliverable_ai_draw_is_still_vetoed() {
        let (state, planeswalker, _own_creature) =
            board_with_only_own_creature(destroy_then_draw_ability(), 0);
        assert_eq!(
            reject_kind(&verdict_for(&state, planeswalker)),
            Some("anti_self_harm_harmful_activation_own_board_only"),
            "an AI-directed draw off an EMPTY library is not a payoff — 'Beneficial in the \
             abstract' must not be enough to rescue an own-board-only Destroy when the draw \
             itself cannot be delivered"
        );
    }

    /// Case 2: the Draw leg's recipient hand-swapped to `Opponent` (synthetic
    /// — see the module doc). The AI gains nothing from this activation at
    /// all: it gives up its own creature AND hands the opponent a card. Global
    /// `effect_polarity` alone cannot see the difference between this and the
    /// real card, because it never reads WHO draws — only recipient-aware
    /// resolution can.
    #[test]
    fn own_board_destroy_with_an_opponent_recipient_draw_is_still_vetoed() {
        let (state, planeswalker, _own_creature) =
            board_with_only_own_creature(destroy_then_gift_opponent_a_draw_ability(), 10);
        assert_eq!(
            reject_kind(&verdict_for(&state, planeswalker)),
            Some("anti_self_harm_harmful_activation_own_board_only"),
            "a Draw leg that resolves to the OPPONENT is not an AI payoff — it must not \
             rescue an own-board-only Destroy no matter how much library the AI has"
        );
    }

    /// Case 4, non-vacuity: remove the Draw leg entirely (a bare "Destroy
    /// target creature.") and the SAME board is still vetoed. Proves the fix
    /// isn't a blanket stand-down — it responds specifically to a confirmed
    /// AI payoff, not to the mere presence of a second effect.
    #[test]
    fn without_the_draw_leg_the_same_board_is_still_vetoed() {
        let mut bare = destroy_then_draw_ability();
        bare.sub_ability = None;
        let (state, planeswalker, _own_creature) = board_with_only_own_creature(bare, 10);
        assert_eq!(
            reject_kind(&verdict_for(&state, planeswalker)),
            Some("anti_self_harm_harmful_activation_own_board_only"),
            "with no Draw leg to rescue it, a bare own-board-only Destroy must still be vetoed"
        );
    }
}
