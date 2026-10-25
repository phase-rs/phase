use super::*;
use crate::types::DraftStatus::*;
use crate::view::{filter_for_player, filter_for_spectator};
use engine::types::card::DraftEffect;

const SUFFIXES: [&str; 6] = ["z", "a", "y", "b", "x", "c"];
struct Source {
    size: u8,
    effect_seat: Option<u8>,
}
impl PackSource for Source {
    fn generate_pack(&self, _: &mut dyn rand::RngCore, seat: u8, round: u8) -> DraftPack {
        DraftPack(
            SUFFIXES
                .iter()
                .take(usize::from(self.size))
                .enumerate()
                .map(|(i, suffix)| DraftCardInstance {
                    instance_id: format!("s{seat}-r{round}-{suffix}"),
                    name: format!("Card {suffix}"),
                    set_code: "TST".into(),
                    collector_number: i.to_string(),
                    rarity: if i == 1 { "rare" } else { "common" }.into(),
                    colors: vec![],
                    cmc: 0,
                    type_line: "Artifact".into(),
                    draft_effect: (self.effect_seat == Some(seat) && i == 0)
                        .then_some(DraftEffect::AdditionalPick),
                })
                .collect(),
        )
    }
}

const MIXED_SIZES: [u8; 3] = [15, 14, 17];
const MIXED_CODES: [&str; 3] = ["AAA", "BBB", "CCC"];

struct MixedRoundSource;

impl PackSource for MixedRoundSource {
    fn generate_pack(&self, _: &mut dyn rand::RngCore, seat: u8, round: u8) -> DraftPack {
        let round_index = usize::from(round);
        DraftPack(
            (0..MIXED_SIZES[round_index])
                .map(|card| DraftCardInstance {
                    instance_id: format!("mix-{seat}-{round}-{card}"),
                    name: format!("Mixed {seat}-{round}-{card}"),
                    set_code: MIXED_CODES[round_index].into(),
                    collector_number: card.to_string(),
                    rarity: "common".into(),
                    colors: vec![],
                    cmc: 0,
                    type_line: "Artifact".into(),
                    draft_effect: None,
                })
                .collect(),
        )
    }
}

const CHAOS_SIZES: [[u8; 3]; 4] = [[16, 14, 17], [13, 15, 16], [14, 16, 13], [17, 13, 15]];

struct UnequalChaosSource;

impl PackSource for UnequalChaosSource {
    fn generate_pack(&self, _: &mut dyn rand::RngCore, seat: u8, round: u8) -> DraftPack {
        let round_index = usize::from(round);
        DraftPack(
            (0..CHAOS_SIZES[usize::from(seat)][round_index])
                .map(|card| DraftCardInstance {
                    instance_id: format!("chaos-{seat}-{round}-{card}"),
                    name: format!("Chaos {seat}-{round}-{card}"),
                    set_code: MIXED_CODES[(usize::from(seat) + round_index) % MIXED_CODES.len()]
                        .into(),
                    collector_number: card.to_string(),
                    rarity: "common".into(),
                    colors: vec![],
                    cmc: 0,
                    type_line: "Artifact".into(),
                    draft_effect: None,
                })
                .collect(),
        )
    }
}

fn mixed_config(kind: DraftKind, review: u32) -> DraftConfig {
    let mut cfg = config(kind, 15, 0, 0, review);
    cfg.pack_count = 3;
    cfg.source = DraftSource::Set {
        layout: SetLayout::UniformByRound {
            codes: MIXED_CODES.into_iter().map(str::to_string).collect(),
        },
    };
    cfg.set_code = cfg.source.set_code();
    cfg
}

fn mixed_fixture(kind: DraftKind, review: u32) -> DraftSession {
    let mut session = lobby(mixed_config(kind, review));
    let events = apply_at(
        &mut session,
        DraftAction::StartDraft,
        Some(&MixedRoundSource),
        1000,
    )
    .unwrap();
    assert_eq!(events, vec![DraftDelta::DraftStarted]);
    session.validate_persisted_snapshot().unwrap();
    session
}

fn assert_mixed_spectator_shape(
    session: &DraftSession,
    expected_round: u8,
    expected_size: u8,
    expected_steps: u8,
    expected_step_sequence: [u8; 3],
) {
    for visibility in [SpectatorVisibility::Public, SpectatorVisibility::Omniscient] {
        let before = session.clone();
        let view = filter_for_spectator(session, visibility);
        assert_eq!(view.current_pack_number, expected_round);
        assert_eq!(view.cards_per_pack, expected_size);
        assert_eq!(view.pick_steps_per_pack, expected_steps);
        assert_eq!(view.pack_sizes, vec![15, 14, 17]);
        assert_eq!(view.pack_pick_steps, expected_step_sequence);
        assert_eq!(
            view.pack_set_codes,
            vec!["AAA".to_string(), "BBB".to_string(), "CCC".to_string()]
        );
        assert_eq!(*session, before);
    }
}

fn legacy_mixed_fixture(kind: DraftKind) -> DraftSession {
    let mut cfg = mixed_config(kind, 0);
    cfg.flow = DraftFlow::LegacyBarrier;
    cfg.clock_policy = DraftClockPolicy::default();
    let seats = (0..cfg.pod_size)
        .map(|seat| DraftSeat::Human {
            player_id: PlayerId(seat),
            display_name: format!("Player {seat}"),
        })
        .collect();
    let mut session = DraftSession::new(cfg, seats, "legacy-mixed".into());
    let events = apply(
        &mut session,
        DraftAction::StartDraft,
        Some(&MixedRoundSource),
    )
    .unwrap();
    assert_eq!(events, vec![DraftDelta::DraftStarted]);
    session.validate_persisted_snapshot().unwrap();
    session
}

fn choose_legacy(session: &mut DraftSession, seat: u8) {
    let picks = session
        .head_pack(seat)
        .unwrap()
        .0
        .iter()
        .take(usize::from(session.kind.procedure().cards_per_pick))
        .map(|card| card.instance_id.clone())
        .collect();
    apply(
        session,
        DraftAction::Pick {
            seat,
            card_instance_ids: picks,
        },
        None,
    )
    .unwrap();
}

fn drain_legacy_round(session: &mut DraftSession) {
    let round = session.current_pack_number;
    for _ in 0..200 {
        if session.status != Drafting || session.current_pack_number != round {
            return;
        }
        for seat in 0..session.config.pod_size {
            if session.status != Drafting || session.current_pack_number != round {
                return;
            }
            if session
                .head_pack(seat)
                .is_some_and(|pack| !pack.0.is_empty())
                && !session.seats_picked_this_round.get(seat)
            {
                choose_legacy(session, seat);
            }
        }
    }
    panic!("legacy round did not drain");
}

fn config(kind: DraftKind, size: u8, burns: u8, threshold: u8, review: u32) -> DraftConfig {
    DraftConfig {
        flow: DraftFlow::FifoAsync,
        selection_policy: DraftSelectionPolicy {
            burns_per_pick: burns,
            discard_threshold: threshold,
        },
        clock_policy: DraftClockPolicy {
            pick: PickClockPolicy::Fixed { duration_ms: 1000 },
            review_duration_ms: review,
        },
        source: DraftSource::single_set("TST"),
        set_code: "TST".into(),
        kind,
        pod_size: 4,
        cards_per_pack: size,
        pack_count: 2,
        min_deck_size: kind.procedure().min_deck_size,
        addable_cards: DeckAddableCards::standard_basics(),
        rng_seed: 42,
        tournament_format: TournamentFormat::Swiss,
        pod_policy: PodPolicy::Competitive,
        spectator_visibility: SpectatorVisibility::Public,
    }
}
fn lobby(config: DraftConfig) -> DraftSession {
    let seats = (0..config.pod_size)
        .map(|seat| DraftSeat::Human {
            player_id: PlayerId(seat),
            display_name: format!("Player {seat}"),
        })
        .collect();
    DraftSession::new_fifo(
        config,
        seats,
        "same-code".into(),
        DraftSessionId("incarnation-one".into()),
    )
    .unwrap()
}
fn fixture(
    kind: DraftKind,
    size: u8,
    burns: u8,
    threshold: u8,
    review: u32,
    effect_seat: Option<u8>,
) -> DraftSession {
    let mut session = lobby(config(kind, size, burns, threshold, review));
    let events = apply_at(
        &mut session,
        DraftAction::StartDraft,
        Some(&Source { size, effect_seat }),
        1000,
    )
    .unwrap();
    assert!(accepted(&events).is_empty());
    session.validate_persisted_snapshot().unwrap();
    session
}
fn ids(seat: u8, suffixes: &[&str]) -> Vec<String> {
    suffixes
        .iter()
        .map(|suffix| format!("s{seat}-r0-{suffix}"))
        .collect()
}
fn card_ids(cards: &[DraftCardInstance]) -> Vec<String> {
    cards.iter().map(|card| card.instance_id.clone()).collect()
}
fn prompt(session: &DraftSession, seat: u8) -> DraftPromptId {
    session.fifo.as_ref().unwrap().seats[usize::from(seat)]
        .prompt
        .as_ref()
        .unwrap()
        .id
        .clone()
}
fn select(
    session: &DraftSession,
    seat: u8,
    picked: Vec<String>,
    burned: Vec<String>,
    effects: Vec<String>,
) -> DraftAction {
    DraftAction::Select {
        seat,
        prompt_id: prompt(session, seat),
        picked_card_instance_ids: picked,
        burned_card_instance_ids: burned,
        effect_card_instance_ids: effects,
    }
}
fn choose(session: &mut DraftSession, seat: u8, now: u64) -> Vec<DraftDelta> {
    let action = pick_pass::legal_auto_selection(session, seat).unwrap();
    apply_at(session, action, None, now).unwrap()
}
fn accepted(events: &[DraftDelta]) -> Vec<DraftDelta> {
    events
        .iter()
        .filter(|event| matches!(event, DraftDelta::SelectionAccepted { .. }))
        .cloned()
        .collect()
}
fn expected(
    prompt_id: DraftPromptId,
    picks: Vec<String>,
    burns: Vec<String>,
    returned: Vec<String>,
    discarded: Vec<String>,
) -> DraftDelta {
    DraftDelta::SelectionAccepted {
        seat: prompt_id.seat,
        prompt_id,
        picked_card_instance_ids: picks,
        burned_card_instance_ids: burns,
        returned_card_instance_ids: returned,
        discarded_card_instance_ids: discarded,
    }
}
fn assert_refusal(session: &DraftSession, action: DraftAction, now: u64, error: DraftError) {
    let mut clone = session.clone();
    assert_eq!(apply_at(&mut clone, action, None, now), Err(error));
    assert_eq!(clone, *session);
}
fn drain_round(session: &mut DraftSession, now: u64) -> Vec<DraftDelta> {
    let round = session.current_pack_number;
    let mut last = Vec::new();
    for _ in 0..200 {
        if session.status != Drafting || session.current_pack_number != round {
            return last;
        }
        for seat in 0..session.config.pod_size {
            if session.status != Drafting || session.current_pack_number != round {
                return last;
            }
            if session.head_pack(seat).is_some() {
                last = choose(session, seat, now);
            }
        }
    }
    panic!("round did not drain");
}

#[test]
fn fifo_independent_picks_preserve_tail_order() {
    let mut session = fixture(DraftKind::Premier, 6, 0, 0, 0, None);
    let old_b = session.fifo.as_ref().unwrap().seats[1].clone();
    choose(&mut session, 0, 1100);
    assert_eq!(session.fifo.as_ref().unwrap().seats[1].queue.len(), 2);
    assert_eq!(
        session.fifo.as_ref().unwrap().seats[1].queue.front(),
        old_b.queue.front()
    );
    assert_eq!(session.fifo.as_ref().unwrap().seats[1].prompt, old_b.prompt);
    choose(&mut session, 2, 1100);
    choose(&mut session, 3, 1100);
    choose(&mut session, 0, 1100);
    assert_eq!(session.fifo.as_ref().unwrap().seats[1].queue.len(), 3);
    assert_eq!(session.pools[0].len(), 2);
    choose(&mut session, 1, 1100);
    assert_eq!(session.head_metadata(1).unwrap().opening_seat, 0);
    assert_eq!(
        card_ids(&session.head_pack(1).unwrap().0),
        ids(0, &["a", "y", "b", "x", "c"])
    );
}

#[test]
fn fifo_circulated_pack_rejects_old_prompt() {
    let first = fixture(DraftKind::Premier, 6, 0, 0, 0, None);
    let mut second = DraftSession::new_fifo(
        first.config.clone(),
        first.seats.clone(),
        first.draft_code.clone(),
        DraftSessionId("incarnation-two".into()),
    )
    .unwrap();
    apply_at(
        &mut second,
        DraftAction::StartDraft,
        Some(&Source {
            size: 6,
            effect_seat: None,
        }),
        1000,
    )
    .unwrap();
    assert_eq!(first.head_pack(0), second.head_pack(0));
    assert_eq!(prompt(&first, 0).generation, prompt(&second, 0).generation);
    let foreign = select(&second, 0, ids(0, &["z"]), vec![], vec![]);
    assert_refusal(&first, foreign, 1100, DraftError::StalePrompt);
    choose(&mut second, 0, 1100);
    let mut session = fixture(DraftKind::Premier, 6, 0, 0, 0, None);
    let old = select(&session, 0, ids(0, &["c"]), vec![], vec![]);
    for _ in 0..4 {
        for seat in 0..4 {
            choose(&mut session, seat, 1100);
        }
    }
    assert!(session
        .head_pack(0)
        .unwrap()
        .0
        .iter()
        .any(|card| card.instance_id == "s0-r0-c"));
    assert_refusal(&session, old, 1100, DraftError::StalePrompt);
    let good = select(&session, 0, ids(0, &["c"]), vec![], vec![]);
    let mut foreign = good.clone();
    if let DraftAction::Select { prompt_id, .. } = &mut foreign {
        prompt_id.session_id = DraftSessionId("incarnation-two".into());
    }
    assert_refusal(&session, foreign, 1100, DraftError::StalePrompt);
    assert_eq!(
        accepted(&apply_at(&mut session, good, None, 1100).unwrap()).len(),
        1
    );
}

#[test]
fn fifo_selection_accepted_reports_consumed_authority() {
    let mut session = fixture(DraftKind::CommanderDraft, 6, 2, 0, 0, None);
    choose(&mut session, 0, 1100);
    let old = prompt(&session, 1);
    assert_eq!(session.fifo.as_ref().unwrap().seats[1].queue.len(), 2);
    let action = select(
        &session,
        1,
        ids(1, &["x", "a"]),
        ids(1, &["b", "z"]),
        vec![],
    );
    let mut bad = action.clone();
    if let DraftAction::Select { prompt_id, .. } = &mut bad {
        prompt_id.generation += 1;
    }
    assert_refusal(&session, bad, 1100, DraftError::StalePrompt);
    let mut bad = action.clone();
    if let DraftAction::Select {
        picked_card_instance_ids,
        ..
    } = &mut bad
    {
        picked_card_instance_ids.pop();
    }
    assert_refusal(
        &session,
        bad,
        1100,
        DraftError::InvalidSelectionCount {
            role: DraftSelectionRole::Pick,
            expected: 2,
            actual: 1,
        },
    );
    let mut bad = action.clone();
    if let DraftAction::Select {
        burned_card_instance_ids,
        ..
    } = &mut bad
    {
        burned_card_instance_ids[0] = "s1-r0-x".into();
    }
    assert_refusal(
        &session,
        bad,
        1100,
        DraftError::InvalidSelection {
            role: DraftSelectionRole::Burn,
            reason: "duplicate or overlapping physical id".into(),
        },
    );
    assert_refusal(&session, action.clone(), 2000, DraftError::ExpiredPrompt);
    let events = apply_at(&mut session, action, None, 1100).unwrap();
    assert_eq!(
        accepted(&events),
        vec![expected(
            old.clone(),
            ids(1, &["x", "a"]),
            ids(1, &["b", "z"]),
            vec![],
            vec![]
        )]
    );
    assert_ne!(prompt(&session, 1), old);
    assert_eq!(card_ids(&session.pools[1]), ids(1, &["x", "a"]));
    let picked: Vec<_> = events
        .iter()
        .filter_map(|event| {
            if let DraftDelta::CardPicked {
                seat: 1,
                card_instance_id,
            } = event
            {
                Some(card_instance_id.clone())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(picked, ids(1, &["x", "a"]));
    assert!(!events
        .iter()
        .any(|event| matches!(event, DraftDelta::TransitionedTo { .. })));
    let passed = session.fifo.as_ref().unwrap().seats[2]
        .queue
        .back()
        .unwrap();
    assert_eq!(card_ids(&passed.pack.0), ids(1, &["y", "c"]));
}

#[test]
fn fifo_selection_accepted_reports_returned_and_discarded_ids() {
    let mut session = fixture(DraftKind::Premier, 5, 0, 3, 0, Some(1));
    choose(&mut session, 1, 1100);
    choose(&mut session, 0, 1100);
    let old = prompt(&session, 1);
    assert_eq!(
        filter_for_player(&session, 1)
            .fifo
            .unwrap()
            .effects
            .map(|options| options.card_instance_ids),
        Some(vec!["s1-r0-z".to_string()])
    );
    let action = select(
        &session,
        1,
        ids(0, &["b", "a"]),
        vec![],
        vec!["s1-r0-z".into()],
    );
    let mut bad = action.clone();
    if let DraftAction::Select {
        effect_card_instance_ids,
        ..
    } = &mut bad
    {
        *effect_card_instance_ids = vec!["s0-r0-z".into()];
    }
    assert_refusal(
        &session,
        bad,
        1100,
        DraftError::DraftEffectCardNotInPool {
            card_instance_id: "s0-r0-z".into(),
        },
    );
    let mut bad = action.clone();
    if let DraftAction::Select {
        picked_card_instance_ids,
        ..
    } = &mut bad
    {
        picked_card_instance_ids[1] = picked_card_instance_ids[0].clone();
    }
    assert_refusal(
        &session,
        bad,
        1100,
        DraftError::InvalidSelection {
            role: DraftSelectionRole::Pick,
            reason: "duplicate or overlapping physical id".into(),
        },
    );
    let events = apply_at(&mut session, action, None, 1100).unwrap();
    let mut discarded = ids(0, &["y", "x"]);
    discarded.push("s1-r0-z".into());
    assert_eq!(
        accepted(&events),
        vec![expected(
            old,
            ids(0, &["b", "a"]),
            vec![],
            vec!["s1-r0-z".into()],
            discarded.clone()
        )]
    );
    assert_eq!(card_ids(&session.pools[1]), ids(0, &["b", "a"]));
    assert_eq!(
        session
            .fifo
            .as_ref()
            .unwrap()
            .removed
            .iter()
            .map(|removed| removed.card_instance_id.clone())
            .collect::<Vec<_>>(),
        discarded
    );
    assert!(session
        .fifo
        .as_ref()
        .unwrap()
        .removed
        .iter()
        .all(|removed| removed.reason == DraftRemovalReason::Threshold
            && removed.pack_id == DraftPackId(0)));
    assert!(session.fifo.as_ref().unwrap().returned_effects.is_empty());
    let encoded = serde_json::to_string(&events).unwrap();
    assert_eq!(
        serde_json::from_str::<Vec<DraftDelta>>(&encoded).unwrap(),
        events
    );
}

#[test]
fn fifo_selection_is_atomic_and_exact() {
    for kind in [DraftKind::Premier, DraftKind::CommanderDraft] {
        let session = fixture(kind, 6, 2, 0, 0, None);
        let good = pick_pass::legal_auto_selection(&session, 0).unwrap();
        let mut positive = session.clone();
        assert_eq!(
            accepted(&apply_at(&mut positive, good.clone(), None, 1100).unwrap()).len(),
            1
        );
        let DraftAction::Select {
            picked_card_instance_ids,
            ..
        } = &good
        else {
            unreachable!()
        };
        let count = picked_card_instance_ids.len();
        let mut bad = good.clone();
        if let DraftAction::Select {
            picked_card_instance_ids,
            ..
        } = &mut bad
        {
            picked_card_instance_ids.clear();
        }
        assert_refusal(
            &session,
            bad,
            1100,
            DraftError::InvalidSelectionCount {
                role: DraftSelectionRole::Pick,
                expected: count,
                actual: 0,
            },
        );
        let mut bad = good.clone();
        if let DraftAction::Select {
            burned_card_instance_ids,
            ..
        } = &mut bad
        {
            burned_card_instance_ids.pop();
        }
        assert_refusal(
            &session,
            bad,
            1100,
            DraftError::InvalidSelectionCount {
                role: DraftSelectionRole::Burn,
                expected: 2,
                actual: 1,
            },
        );
        let mut bad = good.clone();
        if let DraftAction::Select { prompt_id, .. } = &mut bad {
            prompt_id.seat = 1;
        }
        assert_refusal(&session, bad, 1100, DraftError::StalePrompt);
        assert_refusal(&session, good.clone(), 2000, DraftError::ExpiredPrompt);
        let mut bad = good.clone();
        if let DraftAction::Select {
            picked_card_instance_ids,
            ..
        } = &mut bad
        {
            picked_card_instance_ids[0] = "s1-r0-z".into();
        }
        assert_refusal(
            &session,
            bad,
            1100,
            DraftError::CardNotInPack {
                card_instance_id: "s1-r0-z".into(),
            },
        );
        let mut bad = good.clone();
        if let DraftAction::Select {
            burned_card_instance_ids,
            picked_card_instance_ids,
            ..
        } = &mut bad
        {
            burned_card_instance_ids[0] = picked_card_instance_ids[0].clone();
        }
        assert_refusal(
            &session,
            bad,
            1100,
            DraftError::InvalidSelection {
                role: DraftSelectionRole::Burn,
                reason: "duplicate or overlapping physical id".into(),
            },
        );
        let mut bad = good.clone();
        if let DraftAction::Select {
            burned_card_instance_ids,
            ..
        } = &mut bad
        {
            burned_card_instance_ids[1] = burned_card_instance_ids[0].clone();
        }
        assert_refusal(
            &session,
            bad,
            1100,
            DraftError::InvalidSelection {
                role: DraftSelectionRole::Burn,
                reason: "duplicate or overlapping physical id".into(),
            },
        );
        if count == 2 {
            let mut bad = good.clone();
            if let DraftAction::Select {
                picked_card_instance_ids,
                ..
            } = &mut bad
            {
                picked_card_instance_ids[1] = picked_card_instance_ids[0].clone();
            }
            assert_refusal(
                &session,
                bad,
                1100,
                DraftError::InvalidSelection {
                    role: DraftSelectionRole::Pick,
                    reason: "duplicate or overlapping physical id".into(),
                },
            );
        }
    }
    let mut siblings = fixture(DraftKind::Premier, 6, 0, 0, 1000, None);
    let old = select(&siblings, 0, ids(0, &["z"]), vec![], vec![]);
    choose(&mut siblings, 0, 1100);
    assert_refusal(
        &siblings,
        old.clone(),
        1100,
        DraftError::NoPendingPack { seat: 0 },
    );
    let tail = select(&siblings, 1, ids(0, &["a"]), vec![], vec![]);
    assert_refusal(
        &siblings,
        tail,
        1100,
        DraftError::CardNotInPack {
            card_instance_id: "s0-r0-a".into(),
        },
    );
    let mut wrong = select(&siblings, 1, ids(1, &["z"]), vec![], vec![]);
    if let DraftAction::Select { seat, .. } = &mut wrong {
        *seat = 9;
    }
    assert_refusal(
        &siblings,
        wrong,
        1100,
        DraftError::SeatOutOfRange {
            seat: 9,
            pod_size: 4,
        },
    );
    choose(&mut siblings, 1, 1100);
    choose(&mut siblings, 3, 1100);
    let own_pool = select(&siblings, 0, ids(0, &["z"]), vec![], vec![]);
    assert_refusal(
        &siblings,
        own_pool,
        1100,
        DraftError::CardNotInPack {
            card_instance_id: "s0-r0-z".into(),
        },
    );
    choose(&mut siblings, 0, 1100);
    drain_round(&mut siblings, 1100);
    assert_eq!(siblings.status, Review);
    assert_refusal(
        &siblings,
        old,
        1100,
        DraftError::InvalidTransition {
            from: Review,
            action: "Select".into(),
        },
    );
    let mut session = fixture(DraftKind::Premier, 6, 1, 0, 0, Some(1));
    choose(&mut session, 1, 1100);
    choose(&mut session, 0, 1100);
    let good = select(
        &session,
        1,
        ids(0, &["y", "b"]),
        ids(0, &["x"]),
        vec!["s1-r0-z".into()],
    );
    let mut bad = good.clone();
    if let DraftAction::Select {
        effect_card_instance_ids,
        ..
    } = &mut bad
    {
        *effect_card_instance_ids = vec!["foreign-effect".into()];
    }
    assert_refusal(
        &session,
        bad,
        1100,
        DraftError::DraftEffectCardNotInPool {
            card_instance_id: "foreign-effect".into(),
        },
    );
    let mut bad = good.clone();
    if let DraftAction::Select {
        burned_card_instance_ids,
        ..
    } = &mut bad
    {
        burned_card_instance_ids[0] = "s1-r0-z".into();
    }
    assert_refusal(
        &session,
        bad,
        1100,
        DraftError::CardNotInPack {
            card_instance_id: "s1-r0-z".into(),
        },
    );
    assert_eq!(
        accepted(&apply_at(&mut session, good, None, 1100).unwrap()).len(),
        1
    );
}

#[test]
fn fifo_expiry_selection_is_exact() {
    for (kind, size, burns, picks, burned, survivors) in [
        (
            DraftKind::Premier,
            6,
            0,
            vec!["z"],
            vec![],
            vec!["a", "y", "b", "x", "c"],
        ),
        (
            DraftKind::CommanderDraft,
            6,
            0,
            vec!["z", "a"],
            vec![],
            vec!["y", "b", "x", "c"],
        ),
        (
            DraftKind::Premier,
            6,
            2,
            vec!["z"],
            vec!["a", "y"],
            vec!["b", "x", "c"],
        ),
        (
            DraftKind::CommanderDraft,
            6,
            2,
            vec!["z", "a"],
            vec!["y", "b"],
            vec!["x", "c"],
        ),
        (DraftKind::Premier, 2, 5, vec!["z"], vec!["a"], vec![]),
        (
            DraftKind::CommanderDraft,
            3,
            5,
            vec!["z", "a"],
            vec!["y"],
            vec![],
        ),
    ] {
        let mut session = fixture(kind, size, burns, 0, 0, None);
        assert_eq!(
            card_ids(&session.head_pack(0).unwrap().0),
            ids(0, &SUFFIXES[..usize::from(size)])
        );
        assert_ne!(
            card_ids(&filter_for_player(&session, 0).current_pack.unwrap()),
            card_ids(&session.head_pack(0).unwrap().0)
        );
        let mut sorted = card_ids(&session.head_pack(0).unwrap().0);
        sorted.sort();
        assert_ne!(sorted, card_ids(&session.head_pack(0).unwrap().0));
        let prompts: Vec<_> = (0..4).map(|seat| prompt(&session, seat)).collect();
        let events = apply_at(&mut session, DraftAction::AdvanceClock, None, 2000).unwrap();
        assert_eq!(
            accepted(&events),
            (0..4)
                .map(|seat| expected(
                    prompts[usize::from(seat)].clone(),
                    ids(seat, &picks),
                    ids(seat, &burned),
                    vec![],
                    vec![]
                ))
                .collect::<Vec<_>>()
        );
        let picked_events: Vec<_> = events
            .iter()
            .filter(|event| matches!(event, DraftDelta::CardPicked { .. }))
            .cloned()
            .collect();
        let expected_events: Vec<_> = (0..4)
            .flat_map(|seat| {
                ids(seat, &picks)
                    .into_iter()
                    .map(move |card_instance_id| DraftDelta::CardPicked {
                        seat,
                        card_instance_id,
                    })
            })
            .collect();
        assert_eq!(picked_events, expected_events);
        for seat in 0..4 {
            assert_eq!(
                card_ids(&session.pools[usize::from(seat)]),
                ids(seat, &picks)
            );
            let removed: Vec<_> = session
                .fifo
                .as_ref()
                .unwrap()
                .removed
                .iter()
                .filter(|removed| removed.pack_id == DraftPackId(u32::from(seat)))
                .collect();
            assert_eq!(
                removed
                    .iter()
                    .map(|removed| removed.card_instance_id.clone())
                    .collect::<Vec<_>>(),
                ids(seat, &burned)
            );
            assert!(removed
                .iter()
                .all(|removed| removed.reason == DraftRemovalReason::Burn));
            let recipient = (seat + 1) % 4;
            let remainder = session.fifo.as_ref().unwrap().seats[usize::from(recipient)]
                .queue
                .iter()
                .find(|pack| pack.pack_id == DraftPackId(u32::from(seat)));
            if survivors.is_empty() {
                assert!(remainder.is_none());
            } else {
                assert_eq!(card_ids(&remainder.unwrap().pack.0), ids(seat, &survivors));
            }
        }
        session.validate_persisted_snapshot().unwrap();
    }
}

#[test]
fn fifo_expiry_commander_final_singleton() {
    let mut session = fixture(DraftKind::CommanderDraft, 3, 0, 0, 0, None);
    for seat in 0..4 {
        choose(&mut session, seat, 1100);
    }
    let expected_results: Vec<_> = (0..4)
        .map(|seat| {
            expected(
                prompt(&session, seat),
                ids((seat + 3) % 4, &["y"]),
                vec![],
                vec![],
                vec![],
            )
        })
        .collect();
    let events = apply_at(&mut session, DraftAction::AdvanceClock, None, 2100).unwrap();
    assert_eq!(accepted(&events), expected_results);
    assert_eq!(session.current_pack_number, 1);
    assert!(session.pools.iter().all(|pool| pool.len() == 3));
}

#[test]
fn fifo_expiry_leaves_eligible_effect_unspent() {
    let mut session = fixture(DraftKind::Premier, 6, 1, 0, 0, Some(0));
    choose(&mut session, 0, 1100);
    choose(&mut session, 3, 1100);
    assert_eq!(
        filter_for_player(&session, 0)
            .fifo
            .unwrap()
            .effects
            .unwrap()
            .card_instance_ids,
        vec!["s0-r0-z".to_string()]
    );
    let old = prompt(&session, 0);
    let events = apply_at(&mut session, DraftAction::AdvanceClock, None, 2100).unwrap();
    assert_eq!(
        accepted(&events)[0],
        expected(old, ids(3, &["y"]), ids(3, &["b"]), vec![], vec![])
    );
    assert_eq!(card_ids(&session.pools[0]), vec!["s0-r0-z", "s3-r0-y"]);
    assert_eq!(
        session.pools[0][0].draft_effect,
        Some(DraftEffect::AdditionalPick)
    );
    assert!(!session
        .fifo
        .as_ref()
        .unwrap()
        .removed
        .iter()
        .any(|removed| removed.card_instance_id == "s0-r0-z"));
    assert!(session.fifo.as_ref().unwrap().returned_effects.is_empty());
    let remainder = session.fifo.as_ref().unwrap().seats[1]
        .queue
        .iter()
        .find(|pack| pack.pack_id == DraftPackId(3))
        .unwrap();
    assert_eq!(card_ids(&remainder.pack.0), ids(3, &["x", "c"]));
}

#[test]
fn fifo_expiry_skips_empty_and_nonactionable_heads() {
    let mut session = fixture(DraftKind::Premier, 6, 0, 0, 0, None);
    choose(&mut session, 0, 1100);
    assert!(pick_pass::legal_auto_selection(&session, 0).is_none());
    let events = apply_at(&mut session, DraftAction::AdvanceClock, None, 2000).unwrap();
    assert_eq!(
        accepted(&events)
            .iter()
            .map(
                |event| if let DraftDelta::SelectionAccepted { seat, .. } = event {
                    *seat
                } else {
                    unreachable!()
                }
            )
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert_eq!(session.pools[0].len(), 1);
    assert!(pick_pass::legal_auto_selection(&session, 0).is_some());
    assert_eq!(
        accepted(&apply_at(&mut session, DraftAction::AdvanceClock, None, 3000).unwrap()).len(),
        4
    );
    let mut session = fixture(DraftKind::Premier, 6, 0, 0, 0, None);
    apply_at(
        &mut session,
        DraftAction::Control {
            control: DraftControl::Pause {
                reason: DraftPauseReason::PausedByHost,
            },
        },
        None,
        1500,
    )
    .unwrap();
    let frozen = session.clone();
    assert!(pick_pass::legal_auto_selection(&session, 0).is_none());
    assert!(
        apply_at(&mut session, DraftAction::AdvanceClock, None, 5000)
            .unwrap()
            .is_empty()
    );
    assert_eq!(session.pools, frozen.pools);
    assert_eq!(
        session.fifo.as_ref().unwrap().seats,
        frozen.fifo.as_ref().unwrap().seats
    );
    assert_eq!(
        session.fifo.as_ref().unwrap().revision,
        frozen.fifo.as_ref().unwrap().revision + 1
    );
    apply_at(
        &mut session,
        DraftAction::Control {
            control: DraftControl::Resume,
        },
        None,
        5000,
    )
    .unwrap();
    assert_eq!(
        accepted(&apply_at(&mut session, DraftAction::AdvanceClock, None, 5500).unwrap()).len(),
        4
    );
}

#[test]
fn fifo_paused_review_round_trip() {
    let mut session = fixture(DraftKind::Premier, 2, 0, 0, 1000, None);
    let events = drain_round(&mut session, 1100);
    assert_eq!(session.status, Review);
    assert_eq!(accepted(&events).len(), 1);
    assert!(events.contains(&DraftDelta::TransitionedTo { status: Review }));
    apply_at(
        &mut session,
        DraftAction::Control {
            control: DraftControl::Pause {
                reason: DraftPauseReason::PausedByHost,
            },
        },
        None,
        1600,
    )
    .unwrap();
    let encoded = serde_json::to_string(&session).unwrap();
    let mut restored: DraftSession = serde_json::from_str(&encoded).unwrap();
    restored.validate_persisted_snapshot().unwrap();
    assert!(
        apply_at(&mut restored, DraftAction::AdvanceClock, None, 9000)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        restored
            .fifo
            .as_ref()
            .unwrap()
            .review
            .as_ref()
            .unwrap()
            .clock
            .state,
        DraftClockState::Frozen { remaining_ms: 500 }
    );
    apply_at(
        &mut restored,
        DraftAction::Control {
            control: DraftControl::Resume,
        },
        None,
        9000,
    )
    .unwrap();
    assert!(
        apply_at(&mut restored, DraftAction::AdvanceClock, None, 9499)
            .unwrap()
            .is_empty()
    );
    let events = apply_at(&mut restored, DraftAction::AdvanceClock, None, 9500).unwrap();
    assert!(accepted(&events).is_empty());
    assert_eq!(restored.current_pack_number, 1);
    assert_eq!(restored.pass_direction, PassDirection::Right);
    assert_eq!(
        accepted(&apply_at(&mut restored, DraftAction::AdvanceClock, None, 10500).unwrap()).len(),
        4
    );
    let events = drain_round(&mut restored, 10500);
    assert_eq!(restored.status, Deckbuilding);
    assert!(events.contains(&DraftDelta::TransitionedTo {
        status: Deckbuilding
    }));
    assert_eq!(accepted(&events).len(), 1);
}

#[test]
fn fifo_round_opens_only_after_all_queues_drain() {
    let mut session = fixture(DraftKind::Premier, 2, 0, 0, 0, None);
    let future = session.packs_by_seat.clone();
    choose(&mut session, 0, 1100);
    choose(&mut session, 2, 1100);
    choose(&mut session, 3, 1100);
    assert_eq!(session.packs_by_seat, future);
    assert_eq!(session.current_pack_number, 0);
    assert_eq!(session.fifo.as_ref().unwrap().seats[1].queue.len(), 2);
    drain_round(&mut session, 1100);
    assert_eq!(session.current_pack_number, 1);
    assert!(session
        .fifo
        .as_ref()
        .unwrap()
        .seats
        .iter()
        .all(|seat| seat.queue.len() == 1));
    drain_round(&mut session, 1100);
    assert_eq!(session.status, Deckbuilding);
}

#[test]
fn fifo_selection_threshold_is_after_effect_return() {
    for (threshold, remaining) in [(2, 3), (3, 0), (4, 0)] {
        let mut session = fixture(DraftKind::Premier, 4, 0, threshold, 0, None);
        let events = choose(&mut session, 0, 1100);
        assert_eq!(
            session.fifo.as_ref().unwrap().seats[1]
                .queue
                .iter()
                .find(|pack| pack.pack_id == DraftPackId(0))
                .map_or(0, |pack| pack.pack.0.len()),
            remaining
        );
        assert_eq!(accepted(&events).len(), 1);
    }
    let mut session = fixture(DraftKind::Premier, 5, 0, 2, 0, Some(1));
    choose(&mut session, 1, 1100);
    choose(&mut session, 0, 1100);
    let action = select(
        &session,
        1,
        ids(0, &["b", "a"]),
        vec![],
        vec!["s1-r0-z".into()],
    );
    let events = apply_at(&mut session, action, None, 1100).unwrap();
    assert!(
        matches!(&accepted(&events)[0], DraftDelta::SelectionAccepted { returned_card_instance_ids, discarded_card_instance_ids, .. } if *returned_card_instance_ids == ["s1-r0-z"] && discarded_card_instance_ids.is_empty())
    );
    let remainder = session.fifo.as_ref().unwrap().seats[2]
        .queue
        .back()
        .unwrap();
    assert_eq!(
        card_ids(&remainder.pack.0),
        vec!["s0-r0-y", "s0-r0-x", "s1-r0-z"]
    );
}

#[test]
fn fifo_deadlines_are_prompt_scoped() {
    let mut session = fixture(DraftKind::Premier, 6, 0, 0, 0, None);
    let b = session.fifo.as_ref().unwrap().seats[1].prompt.clone();
    choose(&mut session, 0, 1999);
    assert_eq!(session.fifo.as_ref().unwrap().seats[1].prompt, b);
    let action = pick_pass::legal_auto_selection(&session, 1).unwrap();
    assert_refusal(&session, action.clone(), 2000, DraftError::ExpiredPrompt);
    let events = apply_at(&mut session, DraftAction::AdvanceClock, None, 2000).unwrap();
    assert_eq!(accepted(&events).len(), 3);
    assert_refusal(&session, action, 2000, DraftError::StalePrompt);
    assert_eq!(session.pools[1].len(), 1);
    assert_eq!(
        session.fifo.as_ref().unwrap().seats[1]
            .prompt
            .as_ref()
            .unwrap()
            .clock
            .as_ref()
            .unwrap()
            .state,
        DraftClockState::Running { deadline_ms: 3000 }
    );
    let before = session.clone();
    assert!(
        apply_at(&mut session, DraftAction::AdvanceClock, None, 1900)
            .unwrap()
            .is_empty()
    );
    assert_eq!(session, before);
}

#[test]
fn fifo_clock_policy_boundaries() {
    for (seconds, expected_duration) in [
        (None, Some(75_000)),
        (Some(0), None),
        (Some(1), Some(1000)),
        (Some(86_400), Some(86_400_000)),
    ] {
        let mut cfg = config(DraftKind::Premier, 6, 0, 0, 0);
        cfg.clock_policy = DraftClockPolicy::native(seconds, 0).unwrap();
        let mut session = lobby(cfg);
        apply_at(
            &mut session,
            DraftAction::StartDraft,
            Some(&Source {
                size: 6,
                effect_seat: None,
            }),
            1000,
        )
        .unwrap();
        assert_eq!(
            session.fifo.as_ref().unwrap().seats[0]
                .prompt
                .as_ref()
                .unwrap()
                .clock
                .as_ref()
                .map(|clock| clock.state),
            expected_duration.map(|duration| DraftClockState::Running {
                deadline_ms: 1000 + duration
            })
        );
    }
    assert_eq!(
        DraftClockPolicy::native(Some(86_401), 0),
        Err(DraftError::InvalidClockBounds)
    );
    assert_eq!(
        DraftClockPolicy::native(Some(u32::MAX), 0),
        Err(DraftError::InvalidClockBounds)
    );
    assert_eq!(
        DraftClockPolicy::p2p(PodPolicy::Casual, 0).unwrap(),
        DraftClockPolicy::quick_default()
    );
    let mut cfg = config(DraftKind::Quick, 30, 0, 0, 0);
    cfg.pod_size = 1;
    cfg.source = DraftSource::Cube {
        id: "cube".into(),
        name: "cube".into(),
    };
    cfg.set_code = "cube".into();
    cfg.clock_policy = DraftClockPolicy::p2p(PodPolicy::Competitive, 0).unwrap();
    let mut session = lobby(cfg);
    apply_at(
        &mut session,
        DraftAction::StartDraft,
        Some(&crate::pack_source::FixturePackSource {
            set_code: "cube".into(),
            cards_per_pack: 30,
        }),
        1000,
    )
    .unwrap();
    for duration in [
        75_000, 70_000, 65_000, 58_000, 52_000, 46_000, 40_000, 34_000, 28_000, 23_000, 20_000,
        18_000, 16_000, 15_000, 15_000,
    ] {
        assert_eq!(
            session.fifo.as_ref().unwrap().seats[0]
                .prompt
                .as_ref()
                .unwrap()
                .clock
                .as_ref()
                .unwrap()
                .state,
            DraftClockState::Running {
                deadline_ms: 1000 + duration
            }
        );
        choose(&mut session, 0, 1000);
    }
    let mut cfg = config(DraftKind::Premier, 6, 0, 0, 0);
    cfg.clock_policy.pick = PickClockPolicy::Fixed { duration_ms: 0 };
    let seats = lobby(config(DraftKind::Premier, 6, 0, 0, 0)).seats;
    assert_eq!(
        DraftSession::new_fifo(cfg, seats, "code".into(), DraftSessionId("session".into())),
        Err(DraftError::InvalidClockBounds)
    );
    let mut session = lobby(config(DraftKind::Premier, 6, 0, 0, 0));
    let before = session.clone();
    assert_eq!(
        apply_at(
            &mut session,
            DraftAction::StartDraft,
            Some(&Source {
                size: 6,
                effect_seat: None
            }),
            MAX_DRAFT_JSON_INTEGER
        ),
        Err(DraftError::InvalidClockBounds)
    );
    assert_eq!(session, before);
    let mut session = fixture(DraftKind::Premier, 2, 0, 0, 0, None);
    let mut boundary = lobby(config(DraftKind::Premier, 6, 0, 0, 0));
    apply_at(
        &mut boundary,
        DraftAction::StartDraft,
        Some(&Source {
            size: 6,
            effect_seat: None,
        }),
        MAX_DRAFT_JSON_INTEGER - 1000,
    )
    .unwrap();
    assert_eq!(
        boundary.fifo.as_ref().unwrap().seats[0]
            .prompt
            .as_ref()
            .unwrap()
            .clock
            .as_ref()
            .unwrap()
            .state,
        DraftClockState::Running {
            deadline_ms: MAX_DRAFT_JSON_INTEGER
        }
    );
    let before_boundary = boundary.clone();
    assert_eq!(
        apply_at(
            &mut boundary,
            DraftAction::AdvanceClock,
            None,
            MAX_DRAFT_JSON_INTEGER
        ),
        Err(DraftError::InvalidClockBounds)
    );
    assert_eq!(boundary, before_boundary);
    let mut cfg = config(DraftKind::Premier, 6, 0, 0, 0);
    cfg.clock_policy = DraftClockPolicy::p2p(PodPolicy::Competitive, 0).unwrap();
    let mut independent = lobby(cfg);
    apply_at(
        &mut independent,
        DraftAction::StartDraft,
        Some(&Source {
            size: 6,
            effect_seat: None,
        }),
        1000,
    )
    .unwrap();
    let b = independent.fifo.as_ref().unwrap().seats[1].prompt.clone();
    choose(&mut independent, 0, 1000);
    choose(&mut independent, 3, 1000);
    assert_eq!(independent.fifo.as_ref().unwrap().seats[1].prompt, b);
    assert_eq!(
        independent.fifo.as_ref().unwrap().seats[0].round_completed_steps,
        1
    );
    assert_eq!(
        independent.fifo.as_ref().unwrap().seats[1].round_completed_steps,
        0
    );
    assert_eq!(
        independent.fifo.as_ref().unwrap().seats[0]
            .prompt
            .as_ref()
            .unwrap()
            .clock
            .as_ref()
            .unwrap()
            .state,
        DraftClockState::Running { deadline_ms: 71000 }
    );
    apply_at(
        &mut session,
        DraftAction::Control {
            control: DraftControl::Pause {
                reason: DraftPauseReason::PausedByHost,
            },
        },
        None,
        2000,
    )
    .unwrap();
    apply_at(
        &mut session,
        DraftAction::Control {
            control: DraftControl::Resume,
        },
        None,
        3000,
    )
    .unwrap();
    assert_eq!(
        accepted(&apply_at(&mut session, DraftAction::AdvanceClock, None, 3000).unwrap()).len(),
        4
    );
    let mut cfg = config(DraftKind::Sealed, 2, 0, 0, 0);
    cfg.pack_count = 6;
    cfg.clock_policy = DraftClockPolicy::native(None, 0).unwrap();
    let mut sealed = lobby(cfg);
    apply_at(
        &mut sealed,
        DraftAction::StartDraft,
        Some(&Source {
            size: 2,
            effect_seat: None,
        }),
        1000,
    )
    .unwrap();
    assert_eq!(sealed.status, Deckbuilding);
    assert!(sealed.pools.iter().all(|pool| pool.len() == 12));
    assert!(sealed
        .fifo
        .as_ref()
        .unwrap()
        .seats
        .iter()
        .all(|seat| seat.prompt.is_none()));
    sealed.validate_persisted_snapshot().unwrap();
    let mut wrong_owner = sealed.clone();
    let moved = wrong_owner.pools[0].pop().unwrap();
    wrong_owner.pools[1].push(moved);
    assert_eq!(
        wrong_owner.validate_persisted_snapshot(),
        Err(snapshot_error("AllAtOnce pool origin mismatch"))
    );
    let mut cfg = config(DraftKind::Quick, 6, 0, 0, 0);
    cfg.clock_policy = DraftClockPolicy::quick_default();
    let mut off = lobby(cfg);
    apply_at(
        &mut off,
        DraftAction::StartDraft,
        Some(&Source {
            size: 6,
            effect_seat: None,
        }),
        1000,
    )
    .unwrap();
    let action = select(&off, 0, ids(0, &["z"]), vec![], vec![]);
    assert_refusal(
        &off,
        DraftAction::Control {
            control: DraftControl::Resume,
        },
        1000,
        DraftError::InvalidTransition {
            from: Drafting,
            action: "Resume".into(),
        },
    );
    apply_at(
        &mut off,
        DraftAction::Control {
            control: DraftControl::Pause {
                reason: DraftPauseReason::PausedByHost,
            },
        },
        None,
        1100,
    )
    .unwrap();
    assert_refusal(
        &off,
        action.clone(),
        1200,
        DraftError::InvalidTransition {
            from: Paused,
            action: "Select".into(),
        },
    );
    assert_refusal(
        &off,
        DraftAction::Control {
            control: DraftControl::Pause {
                reason: DraftPauseReason::PausedByHost,
            },
        },
        1200,
        DraftError::InvalidTransition {
            from: Paused,
            action: "Pause".into(),
        },
    );
    assert!(off.fifo.as_ref().unwrap().seats.iter().all(|seat| seat
        .prompt
        .as_ref()
        .unwrap()
        .clock
        .is_none()));
    apply_at(
        &mut off,
        DraftAction::Control {
            control: DraftControl::Resume,
        },
        None,
        5000,
    )
    .unwrap();
    apply_at(&mut off, action, None, 9000).unwrap();
}

#[test]
fn fifo_generated_dimensions_are_atomic() {
    struct Matrix(Vec<Vec<DraftPack>>);
    impl PackSource for Matrix {
        fn generate_pack(&self, _: &mut dyn rand::RngCore, _: u8, _: u8) -> DraftPack {
            panic!("matrix source entry")
        }
        fn generate_packs(
            &self,
            _: &mut dyn rand::RngCore,
            _: &DraftConfig,
            _: u8,
        ) -> Result<Vec<Vec<DraftPack>>, DraftError> {
            Ok(self.0.clone())
        }
    }
    let base = lobby(config(DraftKind::Premier, 2, 0, 0, 0));
    let mut rng = ChaCha20Rng::seed_from_u64(1);
    let valid = Source {
        size: 2,
        effect_seat: None,
    }
    .generate_packs(&mut rng, &base.config, 4)
    .unwrap();
    let mut good = base.clone();
    apply_at(
        &mut good,
        DraftAction::StartDraft,
        Some(&Matrix(valid.clone())),
        1000,
    )
    .unwrap();
    let mut variants = Vec::new();
    let mut bad = valid.clone();
    bad.pop();
    variants.push(bad);
    let mut bad = valid.clone();
    bad[0].pop();
    variants.push(bad);
    let mut bad = valid.clone();
    bad[0][0].0.clear();
    variants.push(bad);
    let mut bad = valid.clone();
    bad[0][0].0 = vec![bad[0][0].0[0].clone(); 256];
    variants.push(bad);
    let mut bad = valid.clone();
    bad[1][1].0[0] = bad[0][0].0[0].clone();
    variants.push(bad);
    for matrix in variants {
        let mut candidate = base.clone();
        assert!(apply_at(
            &mut candidate,
            DraftAction::StartDraft,
            Some(&Matrix(matrix)),
            1000
        )
        .is_err());
        assert_eq!(candidate, base);
    }
    let mut candidate = base.clone();
    assert!(apply_at(&mut candidate, DraftAction::StartDraft, None, 1000).is_err());
    assert_eq!(candidate, base);
    let mut candidate = base.clone();
    candidate.config.pack_count = 0;
    let bad = candidate.clone();
    assert!(apply_at(
        &mut candidate,
        DraftAction::StartDraft,
        Some(&Matrix(valid)),
        1000
    )
    .is_err());
    assert_eq!(candidate, bad);
    struct Counting(std::cell::Cell<usize>);
    impl PackSource for Counting {
        fn generate_pack(&self, rng: &mut dyn rand::RngCore, seat: u8, round: u8) -> DraftPack {
            self.0.set(self.0.get() + 1);
            Source {
                size: 2,
                effect_seat: None,
            }
            .generate_pack(rng, seat, round)
        }
    }
    let counter = Counting(std::cell::Cell::new(0));
    let mut good = base.clone();
    apply_at(&mut good, DraftAction::StartDraft, Some(&counter), 1000).unwrap();
    assert_eq!(counter.0.get(), 8);
    for count in [0, 256] {
        let mut bad = base.clone();
        bad.seats = vec![base.seats[0].clone(); count];
        let before = bad.clone();
        let expected = if count == 0 {
            "pod size mismatch"
        } else {
            "seat count exceeds u8"
        };
        assert_eq!(
            apply_at(&mut bad, DraftAction::StartDraft, Some(&counter), 1000),
            Err(DraftError::InvalidConfiguration {
                reason: expected.into()
            })
        );
        assert_eq!(bad, before);
        assert_eq!(counter.0.get(), 8);
    }
    let mut bad = base.clone();
    bad.config.clock_policy.pick = PickClockPolicy::Fixed { duration_ms: 0 };
    let before = bad.clone();
    assert_eq!(
        apply_at(&mut bad, DraftAction::StartDraft, Some(&counter), 1000),
        Err(DraftError::InvalidClockBounds)
    );
    assert_eq!(bad, before);
    assert_eq!(counter.0.get(), 8);
}

#[test]
fn fifo_max_cube_seating_passes_both_directions() {
    for count in [1, 255] {
        let mut cfg = config(DraftKind::Quick, 2, 0, 0, 0);
        cfg.pod_size = count;
        cfg.clock_policy = DraftClockPolicy::quick_default();
        cfg.source = DraftSource::Cube {
            id: "cube".into(),
            name: "cube".into(),
        };
        cfg.set_code = "cube".into();
        let mut session = lobby(cfg);
        let template = Source {
            size: 2,
            effect_seat: None,
        }
        .generate_pack(&mut ChaCha20Rng::seed_from_u64(1), 0, 0)
        .0[0]
            .clone();
        let source = crate::cube::CubePackSource::new(vec![template; usize::from(count) * 4]);
        apply_at(&mut session, DraftAction::StartDraft, Some(&source), 1000).unwrap();
        for _ in 0..2 {
            for seat in 0..count {
                choose(&mut session, seat, 1100);
            }
        }
        assert_eq!(session.current_pack_number, 1);
        assert_eq!(session.pass_direction, PassDirection::Right);
        choose(&mut session, 0, 1100);
        let recipient = count - 1;
        assert!(session.fifo.as_ref().unwrap().seats[usize::from(recipient)]
            .queue
            .iter()
            .any(|pack| pack.opening_seat == 0 && pack.round == 1));
        session.validate_persisted_snapshot().unwrap();
    }
}

fn assert_forbidden_removal_rejected(kind: DraftKind, reason: DraftRemovalReason) {
    // A configured removal must still survive persistence and a later
    // production operation through the same ownership-validation loop.
    let (burns, threshold, removed_ids) = match reason {
        DraftRemovalReason::Burn => (1, 0, ids(0, &["a"])),
        DraftRemovalReason::Threshold => (0, 2, ids(0, &["a", "y"])),
    };
    let mut enabled = fixture(DraftKind::Quick, 3, burns, threshold, 0, None);
    choose(&mut enabled, 0, 1100);
    assert_eq!(
        enabled.fifo.as_ref().unwrap().removed,
        removed_ids
            .into_iter()
            .map(|card_instance_id| RemovedDraftCard {
                card_instance_id,
                pack_id: DraftPackId(0),
                reason,
            })
            .collect::<Vec<_>>()
    );
    let mut enabled: DraftSession =
        serde_json::from_str(&serde_json::to_string(&enabled).unwrap()).unwrap();
    enabled.validate_persisted_snapshot().unwrap();
    choose(&mut enabled, 1, 1200);

    let mut cfg = config(kind, 3, 0, 0, 0);
    if kind.procedure().distribution == PackDistribution::AllAtOnce {
        cfg.pack_count = kind.procedure().packs_per_player;
    }
    let mut valid = lobby(cfg);
    apply_at(
        &mut valid,
        DraftAction::StartDraft,
        Some(&Source {
            size: 3,
            effect_seat: None,
        }),
        1000,
    )
    .unwrap();
    valid.validate_persisted_snapshot().unwrap();
    let mut continuing = valid.clone();
    apply_at(&mut continuing, DraftAction::AdvanceClock, None, 1001).unwrap();
    match kind.procedure().distribution {
        PackDistribution::PickAndPass => {
            assert_eq!(valid.head_pack(0).unwrap().0.len(), 3);
            choose(&mut continuing, 0, 1100);
            assert_eq!(continuing.pools[0].len(), 1);
        }
        PackDistribution::AllAtOnce => {
            assert_eq!(valid.status, Deckbuilding);
            assert_eq!(continuing.pools[0].len(), 18);
        }
        PackDistribution::SharedStackPiles { .. } => {
            unreachable!("{}", SHARED_STACK_HAS_NO_FIFO)
        }
    }
    let (card, pack_id, expected_error) = match kind.procedure().distribution {
        PackDistribution::PickAndPass => {
            let head = valid.fifo.as_mut().unwrap().seats[0]
                .queue
                .front_mut()
                .unwrap();
            (
                head.pack.0.pop().unwrap(),
                head.pack_id,
                "removal reason is disabled by policy",
            )
        }
        PackDistribution::AllAtOnce => {
            let card = valid.pools[0].pop().unwrap();
            let pack_id = valid.fifo.as_ref().unwrap().cards[&card.instance_id].pack_id;
            (card, pack_id, "AllAtOnce cannot remove cards")
        }
        PackDistribution::SharedStackPiles { .. } => {
            unreachable!("{}", SHARED_STACK_HAS_NO_FIFO)
        }
    };
    valid.fifo.as_mut().unwrap().removed.push(RemovedDraftCard {
        card_instance_id: card.instance_id,
        pack_id,
        reason,
    });
    assert_eq!(valid.fifo.as_ref().unwrap().seats[0].completed_steps, 0);
    let mut restored: DraftSession =
        serde_json::from_str(&serde_json::to_string(&valid).unwrap()).unwrap();
    let before = restored.clone();
    assert_eq!(
        restored.validate_persisted_snapshot(),
        Err(snapshot_error(expected_error))
    );
    assert_eq!(
        apply_at(&mut restored, DraftAction::AdvanceClock, None, 1001),
        Err(snapshot_error(expected_error))
    );
    assert_eq!(restored, before);
}

#[test]
fn fifo_snapshot_rejects_forbidden_quick_burn() {
    assert_forbidden_removal_rejected(DraftKind::Quick, DraftRemovalReason::Burn);
}

#[test]
fn fifo_snapshot_rejects_forbidden_quick_threshold() {
    assert_forbidden_removal_rejected(DraftKind::Quick, DraftRemovalReason::Threshold);
}

#[test]
fn fifo_snapshot_rejects_forbidden_sealed_burn() {
    assert_forbidden_removal_rejected(DraftKind::Sealed, DraftRemovalReason::Burn);
}

#[test]
fn fifo_snapshot_rejects_forbidden_sealed_threshold() {
    assert_forbidden_removal_rejected(DraftKind::Sealed, DraftRemovalReason::Threshold);
}

#[test]
fn fifo_snapshot_rejects_invalid_ownership() {
    let valid = fixture(DraftKind::Premier, 6, 1, 1, 1000, None);
    let mut mutations = Vec::new();
    let mut bad = valid.clone();
    let duplicate = bad.head_pack(0).unwrap().0[0].clone();
    bad.pools[0].push(duplicate);
    mutations.push(bad);
    let mut bad = valid.clone();
    bad.fifo.as_mut().unwrap().seats[0]
        .queue
        .front_mut()
        .unwrap()
        .pack
        .0
        .pop();
    mutations.push(bad);
    let mut bad = valid.clone();
    bad.fifo.as_mut().unwrap().seats[0]
        .queue
        .front_mut()
        .unwrap()
        .opening_seat = 1;
    mutations.push(bad);
    let mut bad = valid.clone();
    bad.fifo.as_mut().unwrap().seats[0]
        .queue
        .front_mut()
        .unwrap()
        .round = 1;
    mutations.push(bad);
    let mut bad = valid.clone();
    bad.packs_by_seat[0][0].0.swap(0, 1);
    mutations.push(bad);
    let mut bad = valid.clone();
    bad.fifo.as_mut().unwrap().seats[0]
        .queue
        .front_mut()
        .unwrap()
        .pack
        .0
        .clear();
    mutations.push(bad);
    let mut bad = valid.clone();
    bad.fifo.as_mut().unwrap().seats.pop();
    mutations.push(bad);
    let mut bad = valid.clone();
    bad.fifo.as_mut().unwrap().seats[0]
        .prompt
        .as_mut()
        .unwrap()
        .pack_id = DraftPackId(1);
    mutations.push(bad);
    let mut bad = valid.clone();
    bad.status = Review;
    mutations.push(bad);
    let mut bad = valid.clone();
    bad.fifo.as_mut().unwrap().revision = MAX_DRAFT_JSON_INTEGER + 1;
    mutations.push(bad);
    let mut bad = valid.clone();
    bad.fifo
        .as_mut()
        .unwrap()
        .returned_effects
        .insert("s0-r0-z".into(), DraftPackId(0));
    mutations.push(bad);
    for (bad, reason) in mutations.into_iter().zip([
        "duplicate/mismatched physical ownership",
        "physical card conservation",
        "live pack origin/round/identity",
        "live pack origin/round/identity",
        "future pack order/origin",
        "empty/oversized/discardable live pack",
        "per-seat dimensions",
        "prompt/head binding",
        "queue/status mismatch",
        "invalid FIFO identity/counter/time",
        "counterfeit return membership",
    ]) {
        assert_eq!(
            bad.validate_persisted_snapshot(),
            Err(snapshot_error(reason))
        );
    }
    let mut bad = valid.clone();
    bad.fifo
        .as_mut()
        .unwrap()
        .cards
        .get_mut("s0-r0-z")
        .unwrap()
        .pack_id = DraftPackId(1);
    assert_eq!(
        bad.validate_persisted_snapshot(),
        Err(snapshot_error("manifest physical identity/origin"))
    );
    let mut bad = valid.clone();
    bad.fifo.as_mut().unwrap().seats[0]
        .queue
        .front_mut()
        .unwrap()
        .original_size = 5;
    assert_eq!(
        bad.validate_persisted_snapshot(),
        Err(snapshot_error("live pack origin/round/identity"))
    );
    let mut bad = valid.clone();
    bad.fifo.as_mut().unwrap().seats[0]
        .prompt
        .as_mut()
        .unwrap()
        .clock = None;
    assert_eq!(
        bad.validate_persisted_snapshot(),
        Err(snapshot_error("pick clock policy mismatch"))
    );
    let mut bad = valid.clone();
    bad.fifo.as_mut().unwrap().seats[0]
        .prompt
        .as_mut()
        .unwrap()
        .clock
        .as_mut()
        .unwrap()
        .state = DraftClockState::Frozen { remaining_ms: 1 };
    assert_eq!(
        bad.validate_persisted_snapshot(),
        Err(snapshot_error("clock status/bounds mismatch"))
    );
    let mut bad = valid.clone();
    bad.fifo.as_mut().unwrap().seats[0]
        .prompt
        .as_mut()
        .unwrap()
        .clock
        .as_mut()
        .unwrap()
        .target = DraftClockTarget::Review { next_round: 1 };
    assert_eq!(
        bad.validate_persisted_snapshot(),
        Err(snapshot_error("clock scope mismatch"))
    );
    let mut reduced = valid.clone();
    choose(&mut reduced, 0, 1100);
    reduced.validate_persisted_snapshot().unwrap();
    let mut bad = reduced.clone();
    bad.fifo.as_mut().unwrap().seats[1]
        .prompt
        .as_mut()
        .unwrap()
        .pack_id = DraftPackId(0);
    assert_eq!(
        bad.validate_persisted_snapshot(),
        Err(snapshot_error("prompt/head binding"))
    );
    let mut bad = reduced.clone();
    bad.config.selection_policy.discard_threshold = 4;
    assert_eq!(
        bad.validate_persisted_snapshot(),
        Err(snapshot_error("empty/oversized/discardable live pack"))
    );
    let mut bad = reduced.clone();
    bad.fifo.as_mut().unwrap().seats[0].completed_steps = 0;
    bad.fifo.as_mut().unwrap().seats[0].round_completed_steps = 0;
    assert_eq!(
        bad.validate_persisted_snapshot(),
        Err(snapshot_error("pool/progress contradiction"))
    );
    let mut bad = reduced.clone();
    bad.fifo.as_mut().unwrap().removed.push(RemovedDraftCard {
        card_instance_id: "s0-r0-z".into(),
        pack_id: DraftPackId(0),
        reason: DraftRemovalReason::Threshold,
    });
    assert_eq!(
        bad.validate_persisted_snapshot(),
        Err(snapshot_error("invalid removed ownership/source"))
    );
    valid.validate_persisted_snapshot().unwrap();
    let mut json = serde_json::to_value(&valid).unwrap();
    json["fifo"].as_object_mut().unwrap().remove("review");
    assert!(serde_json::from_value::<DraftSession>(json).is_err());
    let json = serde_json::to_string(&valid).unwrap();
    let mut restored: DraftSession = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, valid);
    choose(&mut restored, 0, 1100);
}

#[test]
fn fifo_snapshots_continue_through_review_and_tournament() {
    fn round_trip(session: &DraftSession) -> DraftSession {
        session.validate_persisted_snapshot().unwrap();
        let restored: DraftSession =
            serde_json::from_str(&serde_json::to_string(session).unwrap()).unwrap();
        assert_eq!(&restored, session);
        restored.validate_persisted_snapshot().unwrap();
        restored
    }
    let mut session = round_trip(&lobby(config(DraftKind::Premier, 2, 0, 0, 1000)));
    apply_at(
        &mut session,
        DraftAction::StartDraft,
        Some(&Source {
            size: 2,
            effect_seat: None,
        }),
        1000,
    )
    .unwrap();
    session = round_trip(&session);
    drain_round(&mut session, 1100);
    assert_eq!(session.status, Review);
    session = round_trip(&session);
    let mut bad = session.clone();
    bad.fifo
        .as_mut()
        .unwrap()
        .review
        .as_mut()
        .unwrap()
        .next_round = 0;
    assert_eq!(
        bad.validate_persisted_snapshot(),
        Err(snapshot_error("Review round mismatch"))
    );
    apply_at(
        &mut session,
        DraftAction::Control {
            control: DraftControl::Pause {
                reason: DraftPauseReason::PausedByHost,
            },
        },
        None,
        1500,
    )
    .unwrap();
    session = round_trip(&session);
    apply_at(
        &mut session,
        DraftAction::Control {
            control: DraftControl::Resume,
        },
        None,
        2000,
    )
    .unwrap();
    apply_at(&mut session, DraftAction::AdvanceClock, None, 2600).unwrap();
    drain_round(&mut session, 2700);
    assert_eq!(session.status, Deckbuilding);
    session = round_trip(&session);
    for seat in 0..4 {
        apply_at(
            &mut session,
            DraftAction::SubmitDeck {
                seat,
                main_deck: vec!["Forest".into(); 40],
                commanders: Vec::new(),
            },
            None,
            2700,
        )
        .unwrap();
    }
    assert_eq!(session.status, Pairing);
    session = round_trip(&session);
    apply_at(&mut session, DraftAction::GeneratePairings, None, 2700).unwrap();
    assert_eq!(session.status, MatchInProgress);
    session = round_trip(&session);
    for pairing in session.pairings.clone() {
        apply_at(
            &mut session,
            DraftAction::ReportMatchResult {
                match_id: pairing.match_id,
                winner_seat: Some(pairing.players[0].0),
            },
            None,
            2700,
        )
        .unwrap();
    }
    assert_eq!(session.status, RoundComplete);
    round_trip(&session);
}

fn legacy_fixture(mut cfg: DraftConfig, source: &dyn PackSource) -> DraftSession {
    cfg.flow = DraftFlow::LegacyBarrier;
    cfg.clock_policy = DraftClockPolicy::default();
    let seats = (0..cfg.pod_size)
        .map(|seat| DraftSeat::Bot {
            name: format!("Seat {seat}"),
        })
        .collect();
    let mut session = DraftSession::new(cfg, seats, "legacy-evidence".into());
    apply(&mut session, DraftAction::StartDraft, Some(source)).unwrap();
    session.validate_persisted_snapshot().unwrap();
    session
}

fn migration_context() -> LegacyUpgradeContext {
    LegacyUpgradeContext {
        session_id: DraftSessionId("retained-evidence".into()),
        now_ms: 1000,
        clock_policy: DraftClockPolicy::default(),
        paused: None,
        pending_clock: None,
        provenance: None,
        progress: None,
    }
}

fn legacy_pick(session: &mut DraftSession, seat: u8, id: &str) {
    apply(
        session,
        DraftAction::Pick {
            seat,
            card_instance_ids: vec![id.into()],
        },
        None,
    )
    .unwrap();
}

fn migration_inventory(session: &DraftSession) -> BTreeMap<String, DraftCardInstance> {
    let mut cards = BTreeMap::new();
    for card in session
        .pools
        .iter()
        .flatten()
        .chain(
            session
                .packs_by_seat
                .iter()
                .flatten()
                .flat_map(|pack| &pack.0),
        )
        .chain(
            session
                .current_pack
                .iter()
                .flatten()
                .flat_map(|pack| &pack.0),
        )
        .chain(
            session
                .fifo
                .iter()
                .flat_map(|fifo| &fifo.seats)
                .flat_map(|seat| &seat.queue)
                .flat_map(|pack| &pack.pack.0),
        )
    {
        assert!(cards
            .insert(card.instance_id.clone(), card.clone())
            .is_none());
    }
    if let Some(fifo) = &session.fifo {
        assert!(fifo.removed.is_empty());
        assert_eq!(
            cards,
            fifo.cards
                .iter()
                .map(|(id, original)| (id.clone(), original.card.clone()))
                .collect()
        );
    }
    cards
}

fn retain_initial_provenance(session: &DraftSession) -> Vec<DraftPackProvenance> {
    assert_eq!(session.pick_number, 0);
    assert!(session.pools.iter().all(Vec::is_empty));
    let mut packs = Vec::new();
    for (seat, current) in session.current_pack.iter().enumerate() {
        for (round, pack) in current
            .iter()
            .chain(&session.packs_by_seat[seat])
            .enumerate()
        {
            packs.push(DraftPackProvenance {
                pack_id: DraftPackId(u32::try_from(round * session.seats.len() + seat).unwrap()),
                opening_seat: u8::try_from(seat).unwrap(),
                round: u8::try_from(round).unwrap(),
                original_size: u8::try_from(pack.0.len()).unwrap(),
                original_card_ids: card_ids(&pack.0),
            });
        }
    }
    packs.sort_by_key(|pack| pack.pack_id);
    packs
}

fn assert_migration_state(
    legacy: &DraftSession,
    upgraded: &DraftSession,
    provenance: &[DraftPackProvenance],
    steps: &[u64],
    origins: &[Vec<u8>],
) {
    upgraded.validate_persisted_snapshot().unwrap();
    assert_eq!(migration_inventory(upgraded), migration_inventory(legacy));
    assert_eq!(upgraded.pools, legacy.pools);
    assert_eq!(upgraded.packs_by_seat, legacy.packs_by_seat);
    let fifo = upgraded.fifo.as_ref().unwrap();
    assert_eq!(fifo.packs.values().cloned().collect::<Vec<_>>(), provenance);
    assert_eq!(
        fifo.seats
            .iter()
            .map(|seat| seat.completed_steps)
            .collect::<Vec<_>>(),
        steps
    );
    assert_eq!(
        fifo.seats
            .iter()
            .map(|seat| seat.round_completed_steps)
            .collect::<Vec<_>>(),
        steps
    );
    assert_eq!(
        fifo.seats
            .iter()
            .map(|seat| {
                seat.queue
                    .iter()
                    .map(|pack| pack.opening_seat)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>(),
        origins
    );
    let restored: DraftSession =
        serde_json::from_str(&serde_json::to_string(upgraded).unwrap()).unwrap();
    assert_eq!(restored, *upgraded);
    restored.validate_persisted_snapshot().unwrap();
}

#[test]
fn legacy_upgrade_custom_ids_requires_matching_provenance() {
    let mut legacy = legacy_fixture(
        config(DraftKind::Premier, 6, 0, 0, 0),
        &Source {
            size: 6,
            effect_seat: None,
        },
    );
    let provenance = retain_initial_provenance(&legacy);
    let inventory = migration_inventory(&legacy);
    assert_eq!(inventory.len(), 48);
    legacy_pick(&mut legacy, 0, "s0-r0-z");
    legacy_pick(&mut legacy, 2, "s2-r0-z");
    assert_eq!(migration_inventory(&legacy), inventory);
    let before = legacy.clone();
    let mut context = migration_context();
    assert_eq!(
        upgrade_legacy_snapshot(&legacy, context.clone()).unwrap_err(),
        snapshot_error("custom legacy ids require provenance")
    );
    assert_eq!(legacy, before);
    context.provenance = Some(provenance.clone());
    let mut upgraded = upgrade_legacy_snapshot(&legacy, context.clone()).unwrap();
    assert_eq!(legacy, before);
    assert_migration_state(
        &legacy,
        &upgraded,
        &provenance,
        &[1, 0, 1, 0],
        &[vec![], vec![1, 0], vec![], vec![3, 2]],
    );
    assert_eq!(
        card_ids(&upgraded.head_pack(1).unwrap().0),
        ids(1, &SUFFIXES)
    );
    assert_eq!(
        card_ids(&upgraded.fifo.as_ref().unwrap().seats[1].queue[1].pack.0),
        ids(0, &["a", "y", "b", "x", "c"])
    );

    let mut contradictory = context;
    let evidence = contradictory.provenance.as_mut().unwrap();
    evidence[0].original_card_ids[1] = "s1-r0-a".into();
    evidence[1].original_card_ids[1] = "s0-r0-a".into();
    assert_eq!(
        upgrade_legacy_snapshot(&legacy, contradictory).unwrap_err(),
        snapshot_error("legacy current origin contradicts cards")
    );
    assert_eq!(legacy, before);

    let action = select(&upgraded, 1, ids(1, &["z"]), vec![], vec![]);
    let consumed = prompt(&upgraded, 1);
    let deltas = apply_at(&mut upgraded, action, None, 1100).unwrap();
    assert_eq!(
        accepted(&deltas),
        vec![expected(consumed, ids(1, &["z"]), vec![], vec![], vec![])]
    );
    assert_eq!(upgraded.head_metadata(1).unwrap().opening_seat, 0);
    assert_eq!(card_ids(&upgraded.pools[1]), ids(1, &["z"]));
    assert_eq!(upgraded.fifo.as_ref().unwrap().seats[1].completed_steps, 1);
    assert_eq!(migration_inventory(&upgraded), inventory);
}

#[test]
fn legacy_upgrade_unequal_chaos_requires_retained_sparse_progress() {
    struct Unequal;
    impl PackSource for Unequal {
        fn generate_pack(&self, rng: &mut dyn rand::RngCore, seat: u8, round: u8) -> DraftPack {
            let code = if (seat + round).is_multiple_of(2) {
                "AAA"
            } else {
                "BBB"
            };
            crate::pack_source::FixturePackSource {
                set_code: code.into(),
                cards_per_pack: [1, 3, 4, 5][usize::from(seat)],
            }
            .generate_pack(rng, seat, round)
        }
    }
    let mut cfg = config(DraftKind::Premier, 5, 0, 0, 0);
    cfg.source = DraftSource::Set {
        layout: SetLayout::Chaos {
            candidate_codes: vec!["AAA".into(), "BBB".into()],
            assignments: vec![
                vec!["AAA".into(), "BBB".into()],
                vec!["BBB".into(), "AAA".into()],
                vec!["AAA".into(), "BBB".into()],
                vec!["BBB".into(), "AAA".into()],
            ],
        },
    };
    cfg.set_code = cfg.source.set_code();
    let mut legacy = legacy_fixture(cfg, &Unequal);
    let provenance = retain_initial_provenance(&legacy);
    let inventory = migration_inventory(&legacy);
    assert_eq!(inventory.len(), 26);
    assert_eq!(
        provenance
            .iter()
            .take(4)
            .map(|pack| pack.original_size)
            .collect::<Vec<_>>(),
        vec![1, 3, 4, 5]
    );
    for (seat, id) in [
        (0, "AAA-0-0-0"),
        (1, "BBB-1-0-0"),
        (2, "AAA-2-0-0"),
        (3, "BBB-3-0-0"),
    ] {
        legacy_pick(&mut legacy, seat, id);
    }
    assert_eq!(legacy.pick_number, 1);
    assert!(legacy.head_pack(1).unwrap().0.is_empty());
    // Seat 1 genuinely skips the second barrier window: its pack is empty.
    for (seat, id) in [
        (0, "BBB-3-0-1"),
        (2, "BBB-1-0-1"),
        (3, "AAA-2-0-1"),
        (0, "AAA-2-0-2"),
        (3, "BBB-1-0-2"),
    ] {
        legacy_pick(&mut legacy, seat, id);
    }
    assert_eq!(legacy.pick_number, 2);
    assert_eq!(
        legacy.pools.iter().map(Vec::len).collect::<Vec<_>>(),
        vec![3, 1, 2, 3]
    );
    assert_eq!(migration_inventory(&legacy), inventory);
    let before = legacy.clone();
    let mut context = migration_context();
    assert_eq!(
        upgrade_legacy_snapshot(&legacy, context.clone()).unwrap_err(),
        snapshot_error("legacy sparse/effect progress requires retained evidence")
    );
    assert_eq!(legacy, before);
    context.progress = Some(
        [3, 1, 2, 3]
            .map(|steps| LegacySeatProgress {
                total: steps,
                round: steps,
            })
            .to_vec(),
    );
    let mut upgraded = upgrade_legacy_snapshot(&legacy, context.clone()).unwrap();
    assert_eq!(legacy, before);
    assert_migration_state(
        &legacy,
        &upgraded,
        &provenance,
        &[3, 1, 2, 3],
        &[vec![], vec![3, 2], vec![], vec![]],
    );
    assert_eq!(
        card_ids(&upgraded.head_pack(1).unwrap().0),
        vec!["BBB-3-0-2", "BBB-3-0-3", "BBB-3-0-4"]
    );
    assert_eq!(
        card_ids(&upgraded.fifo.as_ref().unwrap().seats[1].queue[1].pack.0),
        vec!["AAA-2-0-3"]
    );
    let mut contradictory = context;
    contradictory.progress.as_mut().unwrap()[1] = LegacySeatProgress { total: 2, round: 2 };
    assert_eq!(
        upgrade_legacy_snapshot(&legacy, contradictory).unwrap_err(),
        snapshot_error("pool/progress contradiction")
    );
    assert_eq!(legacy, before);

    let action = select(&upgraded, 1, vec!["BBB-3-0-2".into()], vec![], vec![]);
    let consumed = prompt(&upgraded, 1);
    assert_eq!(
        accepted(&apply_at(&mut upgraded, action, None, 1100).unwrap()),
        vec![expected(
            consumed,
            vec!["BBB-3-0-2".into()],
            vec![],
            vec![],
            vec![]
        )]
    );
    assert_eq!(upgraded.head_metadata(1).unwrap().opening_seat, 2);
    assert_eq!(upgraded.head_metadata(1).unwrap().original_size, 4);
    assert_eq!(
        card_ids(&upgraded.head_pack(1).unwrap().0),
        vec!["AAA-2-0-3"]
    );
    assert_eq!(
        card_ids(&upgraded.head_pack(2).unwrap().0),
        vec!["BBB-3-0-3", "BBB-3-0-4"]
    );
    assert_eq!(upgraded.fifo.as_ref().unwrap().seats[1].completed_steps, 2);
    assert_eq!(migration_inventory(&upgraded), inventory);
}

#[test]
fn legacy_upgrade_returned_effect_requires_retained_progress() {
    struct EffectSource;
    impl PackSource for EffectSource {
        fn generate_pack(&self, rng: &mut dyn rand::RngCore, seat: u8, round: u8) -> DraftPack {
            let mut pack = crate::pack_source::FixturePackSource {
                set_code: "TST".into(),
                cards_per_pack: 6,
            }
            .generate_pack(rng, seat, round);
            if seat == 0 && round == 0 {
                pack.0[0].draft_effect = Some(DraftEffect::AdditionalPick);
            }
            pack
        }
    }
    let mut legacy = legacy_fixture(config(DraftKind::Premier, 6, 0, 0, 0), &EffectSource);
    let provenance = retain_initial_provenance(&legacy);
    let inventory = migration_inventory(&legacy);
    assert_eq!(inventory.len(), 48);
    for (seat, id) in [
        (0, "TST-0-0-0"),
        (1, "TST-1-0-0"),
        (2, "TST-2-0-0"),
        (3, "TST-3-0-0"),
    ] {
        legacy_pick(&mut legacy, seat, id);
    }
    assert_eq!(
        legacy.pools[0][0].draft_effect,
        Some(DraftEffect::AdditionalPick)
    );
    apply(
        &mut legacy,
        DraftAction::PickWithDraftEffect {
            seat: 0,
            effect_card_instance_id: "TST-0-0-0".into(),
            card_instance_ids: vec!["TST-3-0-2".into(), "TST-3-0-1".into()],
        },
        None,
    )
    .unwrap();
    assert_eq!(card_ids(&legacy.pools[0]), vec!["TST-3-0-2", "TST-3-0-1"]);
    assert_eq!(
        card_ids(&legacy.head_pack(0).unwrap().0),
        vec!["TST-3-0-3", "TST-3-0-4", "TST-3-0-5", "TST-0-0-0"]
    );
    assert_eq!(migration_inventory(&legacy), inventory);
    let before = legacy.clone();
    let mut context = migration_context();
    assert_eq!(
        upgrade_legacy_snapshot(&legacy, context.clone()).unwrap_err(),
        snapshot_error("legacy sparse/effect progress requires retained evidence")
    );
    assert_eq!(legacy, before);
    context.progress = Some(
        [2, 1, 1, 1]
            .map(|steps| LegacySeatProgress {
                total: steps,
                round: steps,
            })
            .to_vec(),
    );
    let mut upgraded = upgrade_legacy_snapshot(&legacy, context.clone()).unwrap();
    assert_eq!(legacy, before);
    assert_migration_state(
        &legacy,
        &upgraded,
        &provenance,
        &[2, 1, 1, 1],
        &[vec![], vec![0, 3], vec![1], vec![2]],
    );
    assert_eq!(
        upgraded.fifo.as_ref().unwrap().returned_effects,
        BTreeMap::from([("TST-0-0-0".into(), DraftPackId(3))])
    );
    assert_eq!(
        upgraded.fifo.as_ref().unwrap().cards["TST-0-0-0"].pack_id,
        DraftPackId(0)
    );
    let mut contradictory = context;
    contradictory.progress.as_mut().unwrap()[0] = LegacySeatProgress { total: 1, round: 1 };
    assert_eq!(
        upgrade_legacy_snapshot(&legacy, contradictory).unwrap_err(),
        snapshot_error("pool/progress contradiction")
    );
    assert_eq!(legacy, before);

    let action = select(&upgraded, 1, vec!["TST-0-0-1".into()], vec![], vec![]);
    apply_at(&mut upgraded, action, None, 1100).unwrap();
    assert_eq!(upgraded.head_metadata(1).unwrap().opening_seat, 3);
    assert_eq!(
        card_ids(&upgraded.head_pack(1).unwrap().0),
        vec!["TST-3-0-3", "TST-3-0-4", "TST-3-0-5", "TST-0-0-0"]
    );
    let action = select(&upgraded, 1, vec!["TST-0-0-0".into()], vec![], vec![]);
    let consumed = prompt(&upgraded, 1);
    assert_eq!(
        accepted(&apply_at(&mut upgraded, action, None, 1200).unwrap()),
        vec![expected(
            consumed,
            vec!["TST-0-0-0".into()],
            vec![],
            vec![],
            vec![]
        )]
    );
    assert!(upgraded.fifo.as_ref().unwrap().returned_effects.is_empty());
    assert_eq!(
        upgraded.fifo.as_ref().unwrap().cards["TST-0-0-0"].pack_id,
        DraftPackId(0)
    );
    assert_eq!(
        card_ids(&upgraded.pools[1]),
        vec!["TST-1-0-0", "TST-0-0-1", "TST-0-0-0"]
    );
    assert_eq!(upgraded.fifo.as_ref().unwrap().seats[1].completed_steps, 3);
    assert_eq!(migration_inventory(&upgraded), inventory);
    upgraded.validate_persisted_snapshot().unwrap();
}

#[test]
fn legacy_partial_barrier_upgrades_without_repick() {
    let mut cfg = config(DraftKind::Premier, 6, 0, 0, 0);
    cfg.flow = DraftFlow::LegacyBarrier;
    cfg.clock_policy = DraftClockPolicy::default();
    let mut legacy = DraftSession::new(
        cfg,
        (0..4)
            .map(|seat| DraftSeat::Human {
                player_id: PlayerId(seat),
                display_name: format!("Player {seat}"),
            })
            .collect(),
        "legacy".into(),
    );
    let source = crate::pack_source::FixturePackSource {
        set_code: "TST".into(),
        cards_per_pack: 6,
    };
    apply(&mut legacy, DraftAction::StartDraft, Some(&source)).unwrap();
    for seat in [0, 2] {
        let id = legacy.head_pack(seat).unwrap().0[0].instance_id.clone();
        let events = apply(
            &mut legacy,
            DraftAction::Pick {
                seat,
                card_instance_ids: vec![id],
            },
            None,
        )
        .unwrap();
        assert!(accepted(&events).is_empty());
    }
    let before = legacy.clone();
    let context = LegacyUpgradeContext {
        session_id: DraftSessionId("upgraded".into()),
        now_ms: 1000,
        clock_policy: DraftClockPolicy::default(),
        paused: None,
        pending_clock: None,
        provenance: None,
        progress: None,
    };
    let mut upgraded = upgrade_legacy_snapshot(&legacy, context.clone()).unwrap();
    assert_eq!(legacy, before);
    assert_eq!(upgraded.pools, legacy.pools);
    assert_eq!(upgraded.packs_by_seat, legacy.packs_by_seat);
    assert_eq!(upgraded.head_metadata(1).unwrap().opening_seat, 1);
    assert_eq!(
        upgraded.fifo.as_ref().unwrap().seats[1]
            .queue
            .back()
            .unwrap()
            .opening_seat,
        0
    );
    assert_eq!(upgraded.head_metadata(3).unwrap().opening_seat, 3);
    assert_eq!(
        upgraded.fifo.as_ref().unwrap().seats[3]
            .queue
            .back()
            .unwrap()
            .opening_seat,
        2
    );
    choose(&mut upgraded, 1, 1100);
    assert_eq!(upgraded.head_metadata(1).unwrap().opening_seat, 0);
    let mut malformed = legacy.clone();
    malformed.current_pack[1].as_mut().unwrap().0[0].instance_id = "custom".into();
    let before = malformed.clone();
    assert!(upgrade_legacy_snapshot(&malformed, context.clone()).is_err());
    assert_eq!(malformed, before);
    let mut unknown_time = context;
    unknown_time.clock_policy = DraftClockPolicy::native(None, 0).unwrap();
    assert!(upgrade_legacy_snapshot(&legacy, unknown_time).is_err());
}

#[test]
fn legacy_upgrade_preserves_retained_clocks_and_known_missing_fields() {
    let mut cfg = config(DraftKind::Premier, 2, 0, 0, 0);
    cfg.flow = DraftFlow::LegacyBarrier;
    cfg.clock_policy = DraftClockPolicy::default();
    let seats = lobby(config(DraftKind::Premier, 2, 0, 0, 0)).seats;
    let legacy_lobby = DraftSession::new(cfg, seats, "legacy".into());
    let context = LegacyUpgradeContext {
        session_id: DraftSessionId("fresh-identity".into()),
        now_ms: 1000,
        clock_policy: DraftClockPolicy::default(),
        paused: None,
        pending_clock: None,
        provenance: None,
        progress: None,
    };
    let upgraded_lobby = upgrade_legacy_snapshot(&legacy_lobby, context.clone()).unwrap();
    assert_eq!(upgraded_lobby.status, Lobby);
    upgraded_lobby.validate_persisted_snapshot().unwrap();
    let source = crate::pack_source::FixturePackSource {
        set_code: "TST".into(),
        cards_per_pack: 2,
    };
    let mut legacy = legacy_lobby.clone();
    apply(&mut legacy, DraftAction::StartDraft, Some(&source)).unwrap();
    let mut old_json = serde_json::to_value(&legacy).unwrap();
    for field in [
        "current_pack_origins",
        "connected_seats",
        "seats_picked_this_round",
        "pack_sizes",
    ] {
        old_json.as_object_mut().unwrap().remove(field);
    }
    let missing: DraftSession = serde_json::from_value(old_json).unwrap();
    let mut restored = upgrade_legacy_snapshot(&missing, context.clone()).unwrap();
    assert_eq!(restored.head_metadata(0).unwrap().opening_seat, 0);
    choose(&mut restored, 0, 1100);
    let first = legacy.current_pack[0].as_ref().unwrap().0[0]
        .instance_id
        .clone();
    apply(
        &mut legacy,
        DraftAction::Pick {
            seat: 0,
            card_instance_ids: vec![first],
        },
        None,
    )
    .unwrap();
    let mut missing = legacy.clone();
    missing.current_pack_origins.clear();
    let restored = upgrade_legacy_snapshot(&missing, context.clone()).unwrap();
    assert_eq!(restored.head_metadata(1).unwrap().opening_seat, 1);
    assert_eq!(
        restored.fifo.as_ref().unwrap().seats[1]
            .queue
            .back()
            .unwrap()
            .opening_seat,
        0
    );
    missing.seats_picked_this_round = Default::default();
    let before = missing.clone();
    assert_eq!(
        upgrade_legacy_snapshot(&missing, context.clone()).unwrap_err(),
        snapshot_error("missing legacy bitmap is ambiguous")
    );
    assert_eq!(missing, before);
    let mut timed = context.clone();
    timed.clock_policy = DraftClockPolicy::native(Some(1), 0).unwrap();
    timed.pending_clock = Some(DraftClockState::Running { deadline_ms: 1400 });
    let mut restored = upgrade_legacy_snapshot(&legacy, timed.clone()).unwrap();
    assert_eq!(
        restored.fifo.as_ref().unwrap().seats[1]
            .prompt
            .as_ref()
            .unwrap()
            .clock
            .as_ref()
            .unwrap()
            .state,
        DraftClockState::Running { deadline_ms: 1400 }
    );
    choose(&mut restored, 1, 1300);
    assert_eq!(restored.pools[1].len(), 1);
    let mut retained = timed.clone();
    retained.progress = Some(vec![
        LegacySeatProgress { total: 1, round: 1 },
        LegacySeatProgress { total: 0, round: 0 },
        LegacySeatProgress { total: 0, round: 0 },
        LegacySeatProgress { total: 0, round: 0 },
    ]);
    let upgraded = upgrade_legacy_snapshot(&legacy, retained.clone()).unwrap();
    assert_eq!(upgraded.fifo.as_ref().unwrap().seats[0].completed_steps, 1);
    retained.progress.as_mut().unwrap()[0] = LegacySeatProgress { total: 0, round: 0 };
    let before = legacy.clone();
    assert_eq!(
        upgrade_legacy_snapshot(&legacy, retained).unwrap_err(),
        snapshot_error("pool/progress contradiction")
    );
    assert_eq!(legacy, before);
    let mut ambiguous = timed.clone();
    ambiguous.pending_clock = None;
    assert_eq!(
        upgrade_legacy_snapshot(&legacy, ambiguous).unwrap_err(),
        snapshot_error("legacy remaining clock is unknown")
    );
    let mut paused = legacy.clone();
    paused.status = Paused;
    timed.paused = Some(DraftPausedState {
        phase: DraftActivePhase::Drafting,
        reason: DraftPauseReason::PausedByHost,
    });
    timed.pending_clock = Some(DraftClockState::Frozen { remaining_ms: 0 });
    let mut restored = upgrade_legacy_snapshot(&paused, timed.clone()).unwrap();
    restored.validate_persisted_snapshot().unwrap();
    assert_eq!(restored.status, Paused);
    apply_at(
        &mut restored,
        DraftAction::Control {
            control: DraftControl::Resume,
        },
        None,
        2000,
    )
    .unwrap();
    assert_eq!(
        accepted(&apply_at(&mut restored, DraftAction::AdvanceClock, None, 2000).unwrap()).len(),
        3
    );
    let mut bad = timed;
    bad.pending_clock = Some(DraftClockState::Running { deadline_ms: 1400 });
    assert_eq!(
        upgrade_legacy_snapshot(&paused, bad).unwrap_err(),
        snapshot_error("legacy clock evidence/status mismatch")
    );
    let mut complete = legacy_lobby;
    apply(&mut complete, DraftAction::StartDraft, Some(&source)).unwrap();
    while complete.status == Drafting {
        for seat in 0..4 {
            let id = complete.current_pack[usize::from(seat)].as_ref().unwrap().0[0]
                .instance_id
                .clone();
            apply(
                &mut complete,
                DraftAction::Pick {
                    seat,
                    card_instance_ids: vec![id],
                },
                None,
            )
            .unwrap();
        }
    }
    assert_eq!(complete.status, Deckbuilding);
    let upgraded = upgrade_legacy_snapshot(&complete, context.clone()).unwrap();
    assert_eq!(upgraded.pools, complete.pools);
    upgraded.validate_persisted_snapshot().unwrap();
    assert_eq!(
        upgrade_legacy_snapshot(&upgraded, context.clone()).unwrap(),
        upgraded
    );
    for seat in 0..4 {
        apply(
            &mut complete,
            DraftAction::SubmitDeck {
                seat,
                main_deck: vec!["Forest".into(); 40],
                commanders: Vec::new(),
            },
            None,
        )
        .unwrap();
    }
    apply(&mut complete, DraftAction::GeneratePairings, None).unwrap();
    let upgraded = upgrade_legacy_snapshot(&complete, context.clone()).unwrap();
    assert_eq!(upgraded.status, MatchInProgress);
    assert_eq!(upgraded.pairings, complete.pairings);
    assert_eq!(upgraded.submitted_decks, complete.submitted_decks);
    let mut cfg = complete.config.clone();
    cfg.kind = DraftKind::Sealed;
    cfg.pack_count = 6;
    let mut sealed = DraftSession::new(cfg, complete.seats.clone(), "sealed".into());
    apply(&mut sealed, DraftAction::StartDraft, Some(&source)).unwrap();
    assert_eq!(upgrade_legacy_snapshot(&sealed, context).unwrap(), sealed);
}

#[test]
fn fifo_spectator_uses_generated_current_round_size() {
    for kind in [DraftKind::Quick, DraftKind::Premier, DraftKind::Traditional] {
        let mut session = mixed_fixture(kind, 0);
        assert_mixed_spectator_shape(&session, 0, 15, 15, [15, 14, 17]);

        drain_round(&mut session, 1100);
        assert_eq!(session.status, Drafting);
        assert_mixed_spectator_shape(&session, 1, 14, 14, [15, 14, 17]);

        drain_round(&mut session, 1100);
        assert_eq!(session.status, Drafting);
        assert_mixed_spectator_shape(&session, 2, 17, 17, [15, 14, 17]);

        drain_round(&mut session, 1100);
        assert_eq!(session.status, Deckbuilding);
        assert_mixed_spectator_shape(&session, 3, 17, 17, [15, 14, 17]);
        session.validate_persisted_snapshot().unwrap();
    }

    let mut commander = mixed_fixture(DraftKind::CommanderDraft, 0);
    assert_mixed_spectator_shape(&commander, 0, 15, 8, [8, 7, 9]);

    drain_round(&mut commander, 1100);
    assert_eq!(commander.status, Drafting);
    assert_mixed_spectator_shape(&commander, 1, 14, 7, [8, 7, 9]);

    drain_round(&mut commander, 1100);
    assert_eq!(commander.status, Drafting);
    assert_mixed_spectator_shape(&commander, 2, 17, 9, [8, 7, 9]);

    drain_round(&mut commander, 1100);
    assert_eq!(commander.status, Deckbuilding);
    assert_mixed_spectator_shape(&commander, 3, 17, 9, [8, 7, 9]);
    commander.validate_persisted_snapshot().unwrap();
}

#[test]
fn fifo_spectator_size_survives_waiting_and_partial_heads() {
    for (kind, expected_steps, expected_step_sequence) in [
        (DraftKind::Premier, 14, [15, 14, 17]),
        (DraftKind::CommanderDraft, 7, [8, 7, 9]),
    ] {
        let mut session = mixed_fixture(kind, 0);
        drain_round(&mut session, 1100);
        assert_eq!(session.current_pack_number, 1);

        let passed_card = format!("mix-0-1-{}", kind.procedure().cards_per_pick);
        let waiting_seat = session.pass_direction.next_seat(0, session.config.pod_size);
        let waiting_before =
            session.fifo.as_ref().unwrap().seats[usize::from(waiting_seat)].clone();
        choose(&mut session, 0, 1100);
        let waiting = &session.fifo.as_ref().unwrap().seats[usize::from(waiting_seat)];
        assert!(session.head_pack(0).is_none());
        assert_eq!(waiting.queue.len(), 2);
        assert_eq!(waiting.queue.front().unwrap().opening_seat, waiting_seat);
        assert_eq!(waiting.queue.back().unwrap().opening_seat, 0);
        assert_eq!(waiting.prompt, waiting_before.prompt);
        assert_eq!(
            waiting.queue.front().unwrap().pack.0[0].instance_id,
            format!("mix-{waiting_seat}-1-0")
        );
        assert_eq!(
            waiting.queue.back().unwrap().pack.0[0].instance_id,
            passed_card
        );

        assert_mixed_spectator_shape(&session, 1, 14, expected_steps, expected_step_sequence);
        let before_player_reads = session.clone();
        let a_view = filter_for_player(&session, 0);
        let waiting_view = filter_for_player(&session, waiting_seat);
        assert_eq!(a_view.cards_per_pack, 0);
        assert_eq!(waiting_view.cards_per_pack, 14);
        assert_eq!(session, before_player_reads);

        let public = filter_for_spectator(&session, SpectatorVisibility::Public);
        assert!(public.pools.is_none());
        assert!(public.current_packs.is_none());
        let omniscient = filter_for_spectator(&session, SpectatorVisibility::Omniscient);
        assert!(omniscient.current_packs.as_ref().unwrap()[0].is_none());
        assert_eq!(
            omniscient.current_packs.as_ref().unwrap()[usize::from(waiting_seat)]
                .as_ref()
                .unwrap()[0]
                .instance_id,
            format!("mix-{waiting_seat}-1-0")
        );
        assert!(!serde_json::to_string(&omniscient)
            .unwrap()
            .contains(&passed_card));

        choose(&mut session, waiting_seat, 1100);
        assert!(session.head_pack(waiting_seat).unwrap().0.len() < 14);
        assert_eq!(
            session.head_metadata(waiting_seat).unwrap().original_size,
            14
        );
        let before_player_read = session.clone();
        assert_eq!(filter_for_player(&session, waiting_seat).cards_per_pack, 14);
        assert_eq!(session, before_player_read);
        assert_mixed_spectator_shape(&session, 1, 14, expected_steps, expected_step_sequence);
    }
}

#[test]
fn fifo_spectator_round_size_survives_review_pause_and_restore() {
    for (kind, review_steps, final_steps, step_sequence) in [
        (DraftKind::Premier, 14, 17, [15, 14, 17]),
        (DraftKind::CommanderDraft, 7, 9, [8, 7, 9]),
    ] {
        let mut session = mixed_fixture(kind, 1000);
        drain_round(&mut session, 1100);
        assert_eq!(session.status, Review);
        assert_mixed_spectator_shape(&session, 0, 15, step_sequence[0], step_sequence);

        apply_at(&mut session, DraftAction::AdvanceClock, None, 2100).unwrap();
        assert_eq!(session.status, Drafting);
        assert_eq!(session.current_pack_number, 1);
        drain_round(&mut session, 2200);
        assert_eq!(session.status, Review);
        assert!(session.packs_by_seat.iter().all(|packs| packs.len() == 1));
        assert_mixed_spectator_shape(&session, 1, 14, review_steps, step_sequence);

        apply_at(
            &mut session,
            DraftAction::Control {
                control: DraftControl::Pause {
                    reason: DraftPauseReason::PausedByHost,
                },
            },
            None,
            2700,
        )
        .unwrap();
        assert_eq!(session.status, Paused);
        assert_mixed_spectator_shape(&session, 1, 14, review_steps, step_sequence);
        let encoded = serde_json::to_string(&session).unwrap();
        let mut restored: DraftSession = serde_json::from_str(&encoded).unwrap();
        assert_eq!(restored, session);
        restored.validate_persisted_snapshot().unwrap();
        assert_eq!(
            restored
                .fifo
                .as_ref()
                .unwrap()
                .review
                .as_ref()
                .unwrap()
                .clock
                .state,
            DraftClockState::Frozen { remaining_ms: 500 }
        );
        assert_mixed_spectator_shape(&restored, 1, 14, review_steps, step_sequence);

        assert!(
            apply_at(&mut restored, DraftAction::AdvanceClock, None, 9000)
                .unwrap()
                .is_empty()
        );
        assert_eq!(restored.status, Paused);
        apply_at(
            &mut restored,
            DraftAction::Control {
                control: DraftControl::Resume,
            },
            None,
            9000,
        )
        .unwrap();
        assert!(
            apply_at(&mut restored, DraftAction::AdvanceClock, None, 9499)
                .unwrap()
                .is_empty()
        );
        assert_eq!(restored.status, Review);
        assert_eq!(restored.current_pack_number, 1);
        assert!(restored.packs_by_seat.iter().all(|packs| packs.len() == 1));
        apply_at(&mut restored, DraftAction::AdvanceClock, None, 9500).unwrap();
        assert_eq!(restored.status, Drafting);
        assert_mixed_spectator_shape(&restored, 2, 17, final_steps, step_sequence);

        drain_round(&mut restored, 9501);
        assert_eq!(restored.status, Deckbuilding);
        assert!(restored.fifo.as_ref().unwrap().review.is_none());
        assert!((0..restored.config.pod_size).all(|seat| restored.head_pack(seat).is_none()));
        assert_mixed_spectator_shape(&restored, 3, 17, final_steps, step_sequence);
        for seat in 0..restored.config.pod_size {
            let before_player_read = restored.clone();
            let view = filter_for_player(&restored, seat);
            assert_eq!(view.cards_per_pack, 0);
            assert!(view.current_pack.is_none());
            assert_eq!(restored, before_player_read);
        }

        let mut paused_drafting = mixed_fixture(kind, 0);
        apply_at(
            &mut paused_drafting,
            DraftAction::Control {
                control: DraftControl::Pause {
                    reason: DraftPauseReason::PausedByHost,
                },
            },
            None,
            1500,
        )
        .unwrap();
        assert_eq!(paused_drafting.status, Paused);
        assert_eq!(
            paused_drafting.fifo.as_ref().unwrap().paused.unwrap().phase,
            DraftActivePhase::Drafting
        );
        assert_mixed_spectator_shape(&paused_drafting, 0, 15, step_sequence[0], step_sequence);
    }

    let mut lobby_session = lobby(mixed_config(DraftKind::Premier, 0));
    let before_spectator_read = lobby_session.clone();
    let lobby_view = filter_for_spectator(&lobby_session, SpectatorVisibility::Public);
    assert_eq!(lobby_view.cards_per_pack, 15);
    assert_eq!(lobby_view.pick_steps_per_pack, 15);
    assert_eq!(lobby_view.pack_sizes, vec![15, 15, 15]);
    assert_eq!(lobby_session, before_spectator_read);
    let before_player_read = lobby_session.clone();
    assert_eq!(filter_for_player(&lobby_session, 0).cards_per_pack, 0);
    assert_eq!(lobby_session, before_player_read);
    let events = apply_at(
        &mut lobby_session,
        DraftAction::StartDraft,
        Some(&MixedRoundSource),
        1000,
    )
    .unwrap();
    assert_eq!(events, vec![DraftDelta::DraftStarted]);
    lobby_session.validate_persisted_snapshot().unwrap();
    assert_mixed_spectator_shape(&lobby_session, 0, 15, 15, [15, 14, 17]);
}

#[test]
fn fifo_chaos_spectator_size_stays_redacted() {
    let mut cfg = mixed_config(DraftKind::Premier, 0);
    cfg.source = DraftSource::Set {
        layout: SetLayout::Chaos {
            candidate_codes: MIXED_CODES.into_iter().map(str::to_string).collect(),
            assignments: vec![
                vec!["AAA".into(), "BBB".into(), "CCC".into()],
                vec!["BBB".into(), "CCC".into(), "AAA".into()],
                vec!["CCC".into(), "AAA".into(), "BBB".into()],
                vec!["AAA".into(), "BBB".into(), "CCC".into()],
            ],
        },
    };
    cfg.set_code = cfg.source.set_code();
    let mut session = lobby(cfg);
    apply_at(
        &mut session,
        DraftAction::StartDraft,
        Some(&UnequalChaosSource),
        1000,
    )
    .unwrap();
    session.validate_persisted_snapshot().unwrap();

    for visibility in [SpectatorVisibility::Public, SpectatorVisibility::Omniscient] {
        let before = session.clone();
        let view = filter_for_spectator(&session, visibility);
        assert_eq!(view.cards_per_pack, 0);
        assert_eq!(view.pick_steps_per_pack, 0);
        assert!(view.pack_sizes.is_empty());
        assert!(view.pack_pick_steps.is_empty());
        assert!(view.pack_set_codes.is_empty());
        assert!(view.pools.is_none());
        assert!(view.current_packs.is_none());
        let json = serde_json::to_string(&view).unwrap();
        assert!(!json.contains("\"assignments\""));
        assert!(!json.contains("chaos-"));
        assert_eq!(session, before);
    }

    let before_player_read = session.clone();
    let player = filter_for_player(&session, 0);
    assert_eq!(player.cards_per_pack, 16);
    assert_eq!(
        player.current_pack.as_ref().unwrap()[0].instance_id,
        "chaos-0-0-0"
    );
    assert_eq!(session, before_player_read);

    choose(&mut session, 1, 1100);
    choose(&mut session, 0, 1100);
    let before_player_read = session.clone();
    let player = filter_for_player(&session, 1);
    assert_eq!(player.cards_per_pack, 16);
    assert_eq!(
        player
            .fifo
            .as_ref()
            .unwrap()
            .head
            .as_ref()
            .unwrap()
            .opening_seat,
        0
    );
    assert_eq!(session, before_player_read);
    let omniscient = filter_for_spectator(&session, SpectatorVisibility::Omniscient);
    assert_eq!(omniscient.cards_per_pack, 0);
    assert!(omniscient.current_packs.is_none());
    assert!(!serde_json::to_string(&omniscient)
        .unwrap()
        .contains("chaos-0-0-1"));
}

#[test]
fn legacy_spectator_generated_sizes_and_fallback_remain_compatible() {
    for (kind, step_sequence) in [
        (DraftKind::Premier, [15, 14, 17]),
        (DraftKind::CommanderDraft, [8, 7, 9]),
    ] {
        let mut session = legacy_mixed_fixture(kind);
        assert_mixed_spectator_shape(&session, 0, 15, step_sequence[0], step_sequence);
        drain_legacy_round(&mut session);
        assert_mixed_spectator_shape(&session, 1, 14, step_sequence[1], step_sequence);
        drain_legacy_round(&mut session);
        assert_mixed_spectator_shape(&session, 2, 17, step_sequence[2], step_sequence);
        drain_legacy_round(&mut session);
        assert_eq!(session.status, Deckbuilding);
        assert_mixed_spectator_shape(&session, 3, 17, step_sequence[2], step_sequence);
        session.validate_persisted_snapshot().unwrap();

        let mut cfg = mixed_config(kind, 0);
        cfg.flow = DraftFlow::LegacyBarrier;
        cfg.clock_policy = DraftClockPolicy::default();
        let old = DraftSession::new(
            cfg,
            (0..4)
                .map(|seat| DraftSeat::Human {
                    player_id: PlayerId(seat),
                    display_name: format!("Player {seat}"),
                })
                .collect(),
            "old-legacy".into(),
        );
        let mut old_json = serde_json::to_value(&old).unwrap();
        for field in ["flow", "selection_policy", "clock_policy", "fifo"] {
            assert!(old_json.get(field).is_none());
        }
        assert_eq!(old_json["pack_sizes"], serde_json::Value::Array(Vec::new()));
        old_json.as_object_mut().unwrap().remove("pack_sizes");
        let restored: DraftSession = serde_json::from_value(old_json).unwrap();
        assert!(restored.pack_sizes.is_empty());
        for visibility in [SpectatorVisibility::Public, SpectatorVisibility::Omniscient] {
            let before = restored.clone();
            let view = filter_for_spectator(&restored, visibility);
            assert_eq!(view.cards_per_pack, 15);
            assert_eq!(view.pick_steps_per_pack, step_sequence[0]);
            assert_eq!(view.pack_sizes, vec![15, 15, 15]);
            assert_eq!(view.pack_pick_steps, vec![step_sequence[0]; 3]);
            assert_eq!(restored, before);
        }
    }

    const SEALED_FIXTURE_PACK_SIZE: u8 = 6;
    let mut cfg = config(DraftKind::Sealed, SEALED_FIXTURE_PACK_SIZE, 0, 0, 0);
    cfg.flow = DraftFlow::LegacyBarrier;
    cfg.clock_policy = DraftClockPolicy::default();
    cfg.pack_count = SEALED_PACK_COUNT;
    let seats = (0..cfg.pod_size)
        .map(|seat| DraftSeat::Human {
            player_id: PlayerId(seat),
            display_name: format!("Player {seat}"),
        })
        .collect();
    let mut sealed = DraftSession::new(cfg, seats, "legacy-sealed".into());
    apply(
        &mut sealed,
        DraftAction::StartDraft,
        Some(&Source {
            size: SEALED_FIXTURE_PACK_SIZE,
            effect_seat: None,
        }),
    )
    .unwrap();
    assert_eq!(sealed.status, Deckbuilding);
    assert!(sealed.current_pack.iter().all(Option::is_none));
    assert!(sealed.packs_by_seat.iter().all(Vec::is_empty));
    assert!(sealed.fifo.is_none());
    assert!(sealed
        .pools
        .iter()
        .all(|pool| pool.len() == usize::from(SEALED_FIXTURE_PACK_SIZE * SEALED_PACK_COUNT)));
    assert!(filter_for_spectator(&sealed, SpectatorVisibility::Public)
        .pools
        .is_none());
    sealed.validate_persisted_snapshot().unwrap();
}

#[test]
fn fifo_projections_hide_tail_and_future_authority() {
    let mut session = fixture(DraftKind::Premier, 6, 0, 0, 0, Some(0));
    choose(&mut session, 0, 1100);
    let view = filter_for_player(&session, 1);
    assert_eq!(view.seats[1].active_pack_count, 2);
    assert_eq!(view.current_pack.as_ref().unwrap().len(), 6);
    let json = serde_json::to_string(&view).unwrap();
    assert!(json.contains("s1-r0-z"));
    assert!(!json.contains("s0-r0-a"));
    assert!(!json.contains("s1-r1-z"));
    assert!(!json.contains("original_card_ids"));
    let public = filter_for_spectator(&session, SpectatorVisibility::Public);
    assert!(public.pools.is_none());
    assert!(public.current_packs.is_none());
    assert_eq!(
        public.seats[0].face_up_draft_cards[0].instance_id,
        "s0-r0-z"
    );
    let omni = filter_for_spectator(&session, SpectatorVisibility::Omniscient);
    assert!(omni.pools.is_some());
    assert!(omni.current_packs.as_ref().unwrap()[0].is_none());
    assert!(!serde_json::to_string(&omni).unwrap().contains("s0-r0-a"));
}

#[test]
fn fifo_chaos_head_provenance_is_latched() {
    struct Unequal;
    impl PackSource for Unequal {
        fn generate_pack(&self, rng: &mut dyn rand::RngCore, seat: u8, round: u8) -> DraftPack {
            Source {
                size: if seat == 0 { 6 } else { 5 },
                effect_seat: Some(1),
            }
            .generate_pack(rng, seat, round)
        }
    }
    let mut cfg = config(DraftKind::Premier, 6, 0, 0, 1000);
    cfg.source = DraftSource::Set {
        layout: SetLayout::Chaos {
            candidate_codes: vec!["AAA".into(), "BBB".into()],
            assignments: vec![
                vec!["AAA".into(), "BBB".into()],
                vec!["BBB".into(), "AAA".into()],
                vec!["AAA".into(), "BBB".into()],
                vec!["BBB".into(), "AAA".into()],
            ],
        },
    };
    cfg.set_code = cfg.source.set_code();
    let mut session = lobby(cfg);
    apply_at(&mut session, DraftAction::StartDraft, Some(&Unequal), 1000).unwrap();
    choose(&mut session, 1, 1100);
    choose(&mut session, 0, 1100);
    let view = filter_for_player(&session, 1);
    assert_eq!(view.cards_per_pack, 6);
    assert_eq!(
        view.fifo
            .as_ref()
            .unwrap()
            .head
            .as_ref()
            .unwrap()
            .opening_seat,
        0
    );
    assert!(view.pack_sizes.is_empty());
    assert!(view.pack_pick_steps.is_empty());
    assert!(serde_json::to_string(&view.source)
        .unwrap()
        .contains("\"current_pack_code\":\"AAA\""));
    let action = select(
        &session,
        1,
        ids(0, &["a", "y"]),
        vec![],
        vec!["s1-r0-z".into()],
    );
    apply_at(&mut session, action, None, 1100).unwrap();
    choose(&mut session, 2, 1100);
    assert_eq!(session.head_metadata(2).unwrap().opening_seat, 1);
    choose(&mut session, 2, 1100);
    let view = filter_for_player(&session, 2);
    assert_eq!(view.cards_per_pack, 6);
    assert!(serde_json::to_string(&view.source)
        .unwrap()
        .contains("\"current_pack_code\":\"AAA\""));
    let omni = filter_for_spectator(&session, SpectatorVisibility::Omniscient);
    assert!(omni.pools.is_none());
    assert!(omni.current_packs.is_none());
    assert_eq!(omni.cards_per_pack, 0);
    assert!(omni.pack_sizes.is_empty());
    drain_round(&mut session, 1100);
    assert_eq!(session.status, Review);
    let view = filter_for_player(&session, 0);
    assert_eq!(view.cards_per_pack, 0);
    assert!(view.current_pack.is_none());
}

#[test]
fn legacy_draft_wire_shapes_remain_unchanged() {
    let mut cfg = config(DraftKind::Premier, 6, 0, 0, 0);
    cfg.flow = DraftFlow::LegacyBarrier;
    cfg.clock_policy = DraftClockPolicy::default();
    let json = serde_json::to_value(&cfg).unwrap();
    for key in ["flow", "selection_policy", "clock_policy"] {
        assert!(json.get(key).is_none());
    }
    assert_eq!(serde_json::from_value::<DraftConfig>(json).unwrap(), cfg);
    let session = DraftSession::new(
        cfg,
        (0..4)
            .map(|seat| DraftSeat::Human {
                player_id: PlayerId(seat),
                display_name: format!("Player {seat}"),
            })
            .collect(),
        "legacy".into(),
    );
    let json = serde_json::to_value(&session).unwrap();
    assert!(json.get("fifo").is_none());
    for json in [
        serde_json::to_value(filter_for_player(&session, 0)).unwrap(),
        serde_json::to_value(filter_for_spectator(&session, SpectatorVisibility::Public)).unwrap(),
    ] {
        assert!(json.get("fifo").is_none());
        assert!(json.get("flow").is_none());
        assert!(json["seats"][0].get("fifo_progress").is_none());
    }
    assert_eq!(
        serde_json::to_string(&DraftStatus::Paused).unwrap(),
        "\"Paused\""
    );
    assert_eq!(
        serde_json::to_string(&DraftAction::Pick {
            seat: 1,
            card_instance_ids: vec!["id".into()]
        })
        .unwrap(),
        "{\"type\":\"Pick\",\"data\":{\"seat\":1,\"card_instance_ids\":[\"id\"]}}"
    );
    assert_eq!(
        serde_json::to_string(&DraftAction::PickWithDraftEffect {
            seat: 1,
            effect_card_instance_id: "effect".into(),
            card_instance_ids: vec!["b".into(), "a".into()]
        })
        .unwrap(),
        r#"{"type":"PickWithDraftEffect","data":{"seat":1,"effect_card_instance_id":"effect","card_instance_ids":["b","a"]}}"#
    );
    assert_eq!(
        serde_json::to_string(&DraftAction::SubmitDeck {
            seat: 1,
            main_deck: vec!["Forest".into()],
            commanders: Vec::new()
        })
        .unwrap(),
        r#"{"type":"SubmitDeck","data":{"seat":1,"main_deck":["Forest"],"commanders":[]}}"#
    );
}

/// Counts every pack the wrapped source generates, so a refusal can be
/// shown to precede generation.
struct CountingSource<'a> {
    inner: &'a dyn PackSource,
    calls: std::cell::Cell<usize>,
}

impl<'a> CountingSource<'a> {
    fn new(inner: &'a dyn PackSource) -> Self {
        Self {
            inner,
            calls: std::cell::Cell::new(0),
        }
    }
}

impl PackSource for CountingSource<'_> {
    fn booster_pack_pool(&self) -> Option<Vec<String>> {
        self.inner.booster_pack_pool()
    }

    fn generate_pack(&self, rng: &mut dyn rand::RngCore, seat: u8, round: u8) -> DraftPack {
        self.calls.set(self.calls.get() + 1);
        self.inner.generate_pack(rng, seat, round)
    }
}

/// The policy matrix the shared-stack tests run: the all-default policy first,
/// then each FIFO-only setting on its own (a burn, a discard threshold, a
/// Review duration and both pick-clock policies).
fn d3_policies() -> [(DraftSelectionPolicy, DraftClockPolicy); 6] {
    let selection = |burns_per_pick, discard_threshold| DraftSelectionPolicy {
        burns_per_pick,
        discard_threshold,
    };
    let clock = |pick, review_duration_ms| DraftClockPolicy {
        pick,
        review_duration_ms,
    };
    [
        (selection(0, 0), clock(PickClockPolicy::Off, 0)),
        (selection(1, 0), clock(PickClockPolicy::Off, 0)),
        (selection(0, 1), clock(PickClockPolicy::Off, 0)),
        (selection(0, 0), clock(PickClockPolicy::Off, 1000)),
        (
            selection(0, 0),
            clock(PickClockPolicy::Fixed { duration_ms: 1000 }, 0),
        ),
        (selection(0, 0), clock(PickClockPolicy::SeatStep, 0)),
    ]
}

fn fifo_config_with(
    kind: DraftKind,
    selection: DraftSelectionPolicy,
    clock: DraftClockPolicy,
) -> DraftConfig {
    let mut cfg = config(kind, 6, 0, 0, 0);
    cfg.selection_policy = selection;
    cfg.clock_policy = clock;
    cfg
}

fn shared_stack_refusal() -> DraftError {
    DraftError::InvalidConfiguration {
        reason: SHARED_STACK_HAS_NO_FIFO.to_string(),
    }
}

/// A legacy Winston pod mirroring upstream's own shared-stack fixture
/// (2 seats, 3 TST packs of 15 cards, Swiss, Competitive).
fn legacy_winston(selection: DraftSelectionPolicy, clock: DraftClockPolicy) -> DraftSession {
    let config = DraftConfig {
        flow: DraftFlow::LegacyBarrier,
        selection_policy: selection,
        clock_policy: clock,
        source: DraftSource::single_set("TST"),
        set_code: "TST".into(),
        kind: DraftKind::Winston,
        pod_size: 2,
        cards_per_pack: 15,
        pack_count: 3,
        min_deck_size: 40,
        addable_cards: DeckAddableCards::standard_basics(),
        rng_seed: 42,
        tournament_format: TournamentFormat::Swiss,
        pod_policy: PodPolicy::Competitive,
        spectator_visibility: SpectatorVisibility::default(),
    };
    let seats = (0..2)
        .map(|seat| DraftSeat::Human {
            player_id: PlayerId(seat),
            display_name: format!("Player {seat}"),
        })
        .collect();
    DraftSession::new(config, seats, "WIN-001".into())
}

fn winston_fixture_source() -> crate::pack_source::FixturePackSource {
    crate::pack_source::FixturePackSource {
        set_code: "TST".into(),
        cards_per_pack: 15,
    }
}

#[test]
fn shared_stack_never_starts_fifo_before_generation() {
    let source = Source {
        size: 6,
        effect_seat: None,
    };
    for (selection, clock) in d3_policies() {
        // (a) PickAndPass positive: the same policy starts a FIFO pod.
        let premier = lobby(fifo_config_with(DraftKind::Premier, selection, clock));
        let mut started = premier.clone();
        let counting = CountingSource::new(&source);
        apply_at(&mut started, DraftAction::StartDraft, Some(&counting), 1000).unwrap();
        assert_eq!(counting.calls.get(), 8, "{selection:?} {clock:?}");
        for seat in 0..4 {
            assert!(started.head_pack(seat).is_some());
            assert!(started.fifo.as_ref().unwrap().seats[usize::from(seat)]
                .prompt
                .is_some());
        }

        // (b) Winston FIFO is refused at creation, whatever its policy.
        assert_eq!(
            DraftSession::new_fifo(
                fifo_config_with(DraftKind::Winston, selection, clock),
                premier.seats.clone(),
                "same-code".into(),
                DraftSessionId("incarnation-one".into()),
            ),
            Err(shared_stack_refusal())
        );

        // (c) Hostile relabel of the valid sibling: refused before any pack
        // is generated, and on import.
        let mut relabeled = premier.clone();
        relabeled.kind = DraftKind::Winston;
        relabeled.config.kind = DraftKind::Winston;
        let mut candidate = relabeled.clone();
        let counting = CountingSource::new(&source);
        assert_eq!(
            apply_at(
                &mut candidate,
                DraftAction::StartDraft,
                Some(&counting),
                1000
            ),
            Err(shared_stack_refusal())
        );
        assert_eq!(counting.calls.get(), 0);
        assert_eq!(candidate, relabeled);
        let imported: DraftSession =
            serde_json::from_str(&serde_json::to_string(&relabeled).unwrap()).unwrap();
        assert_eq!(
            imported.validate_persisted_snapshot(),
            Err(shared_stack_refusal())
        );

        // (e) Positive projection reach on the PickAndPass session, through
        // both seat builders.
        let projected = |session: &DraftSession| {
            [
                filter_for_player(session, 0).seats,
                filter_for_spectator(session, SpectatorVisibility::Public).seats,
            ]
        };
        for seats in projected(&started) {
            for seat in &seats {
                assert_eq!(seat.pick_status, PickStatus::Pending);
                assert_eq!(seat.active_pack_count, 1);
            }
        }
        choose(&mut started, 0, 1100);
        for seats in projected(&started) {
            assert_eq!(seats[0].pick_status, PickStatus::Picked);
            assert_eq!(seats[0].active_pack_count, 0);
            assert_eq!(seats[1].pick_status, PickStatus::Pending);
            assert_eq!(seats[1].active_pack_count, 2);
        }
    }

    // (d) AllAtOnce is the sibling the new arm must not absorb: an
    // all-default Sealed FIFO pod starts, while the same all-default policy
    // on Winston was refused in (b), and a Sealed burn keeps AllAtOnce's own
    // refusal.
    let mut sealed_config = fifo_config_with(
        DraftKind::Sealed,
        DraftSelectionPolicy::default(),
        DraftClockPolicy::default(),
    );
    sealed_config.pack_count = DraftKind::Sealed.procedure().packs_per_player;
    let mut sealed = lobby(sealed_config.clone());
    let counting = CountingSource::new(&source);
    apply_at(&mut sealed, DraftAction::StartDraft, Some(&counting), 1000).unwrap();
    assert_eq!(counting.calls.get(), 24);
    assert_eq!(sealed.status, Deckbuilding);
    let mut burning = sealed_config;
    burning.selection_policy.burns_per_pick = 1;
    assert_eq!(
        DraftSession::new_fifo(
            burning,
            sealed.seats.clone(),
            "same-code".into(),
            DraftSessionId("incarnation-one".into()),
        ),
        Err(DraftError::InvalidConfiguration {
            reason: "AllAtOnce policy".to_string(),
        })
    );
}

#[test]
fn shared_stack_legacy_refuses_fifo_only_settings_before_generation() {
    let source = winston_fixture_source();
    let [(default_selection, default_clock), nonzero @ ..] = d3_policies();

    // (a) Positive: the all-default legacy Winston pod starts with
    // upstream's semantics.
    let mut started = legacy_winston(default_selection, default_clock);
    let counting = CountingSource::new(&source);
    apply(&mut started, DraftAction::StartDraft, Some(&counting)).unwrap();
    assert_eq!(counting.calls.get(), 6);
    let active = started.shared_stack.as_ref().unwrap().active_seat;
    let idle = 1 - active;
    for seats in [
        filter_for_player(&started, 0).seats,
        filter_for_spectator(&started, SpectatorVisibility::Public).seats,
    ] {
        assert_eq!(seats[usize::from(active)].pick_status, PickStatus::Pending);
        assert_eq!(seats[usize::from(idle)].pick_status, PickStatus::Waiting);
    }

    // (b) Each nonzero FIFO-only setting is refused before generation and
    // on import. Its paired PickAndPass acceptance is
    // `shared_stack_never_starts_fifo_before_generation` (a).
    for (selection, clock) in nonzero {
        let pod = legacy_winston(selection, clock);
        let mut candidate = pod.clone();
        let counting = CountingSource::new(&source);
        assert_eq!(
            apply(&mut candidate, DraftAction::StartDraft, Some(&counting)),
            Err(DraftError::IncompatibleFlow),
            "{selection:?} {clock:?}"
        );
        assert_eq!(counting.calls.get(), 0);
        assert_eq!(candidate, pod);
        assert_eq!(
            pod.validate_persisted_snapshot(),
            Err(DraftError::IncompatibleFlow)
        );
    }
}

#[test]
fn shared_stack_legacy_snapshot_never_upgrades_to_fifo() {
    let lobby_source = legacy_winston(DraftSelectionPolicy::default(), DraftClockPolicy::default());
    let mut started = lobby_source.clone();
    apply(
        &mut started,
        DraftAction::StartDraft,
        Some(&winston_fixture_source()),
    )
    .unwrap();
    assert!(started.shared_stack.is_some());
    for source in [lobby_source, started] {
        // Reach: a legacy Winston snapshot is admissible, so the refusal
        // below comes from the FIFO validation authority.
        source.validate_persisted_snapshot().unwrap();
        let before = serde_json::to_value(&source).unwrap();
        assert_eq!(
            upgrade_legacy_snapshot(&source, migration_context()),
            Err(shared_stack_refusal())
        );
        assert_eq!(serde_json::to_value(&source).unwrap(), before);
    }

    // Paired positive: a legacy Premier Lobby upgrades to a validated FIFO
    // session.
    let mut premier_config = config(DraftKind::Premier, 6, 0, 0, 0);
    premier_config.flow = DraftFlow::LegacyBarrier;
    premier_config.clock_policy = DraftClockPolicy::default();
    let seats = (0..4)
        .map(|seat| DraftSeat::Human {
            player_id: PlayerId(seat),
            display_name: format!("Player {seat}"),
        })
        .collect();
    let premier = DraftSession::new(premier_config, seats, "legacy".into());
    let upgraded = upgrade_legacy_snapshot(&premier, migration_context()).unwrap();
    assert_eq!(upgraded.config.flow, DraftFlow::FifoAsync);
    assert!(upgraded.fifo.is_some());
    upgraded.validate_persisted_snapshot().unwrap();
}

/// The refusal a FIFO snapshot gets for carrying state that only the legacy
/// flow keeps live.
fn legacy_live_authority_refusal() -> DraftError {
    DraftError::InvalidSnapshot {
        reason: "FIFO contains legacy live authority".to_string(),
    }
}

/// A healthy legacy Winston pod, started through the legacy reducer, so its
/// shared stack is the one `StartDraft` deals.
fn started_legacy_winston() -> DraftSession {
    let mut started = legacy_winston(DraftSelectionPolicy::default(), DraftClockPolicy::default());
    apply(
        &mut started,
        DraftAction::StartDraft,
        Some(&winston_fixture_source()),
    )
    .unwrap();
    started
}

/// A session after a serde round trip, the form an imported or restored
/// snapshot arrives in.
fn reimported(session: &DraftSession) -> DraftSession {
    serde_json::from_str(&serde_json::to_string(session).unwrap()).unwrap()
}

/// `session` carrying `stack` as its shared stack, after a serde round trip.
/// No other field is edited.
fn reimported_with_stack(session: &DraftSession, stack: &SharedStackState) -> DraftSession {
    let mut attached = session.clone();
    attached.shared_stack = Some(stack.clone());
    reimported(&attached)
}

#[test]
fn fifo_lobby_snapshot_carrying_a_shared_stack_is_refused_before_generation() {
    let fifo_lobby = lobby(with_pod_size(config(DraftKind::Premier, 6, 0, 0, 0), 2));
    let winston = started_legacy_winston();
    // Reach: the Winston pod is an admissible snapshot that carries a stack.
    winston.validate_persisted_snapshot().unwrap();
    let stack = winston.shared_stack.as_ref().unwrap();

    // The imported snapshot is the FIFO Lobby carrying that stack after a
    // serde round trip; its twin is the same Lobby round-tripped without it.
    let imported = reimported_with_stack(&fifo_lobby, stack);
    let twin = reimported(&fifo_lobby);
    assert_eq!(imported.shared_stack.as_ref(), Some(stack));
    assert_eq!(imported.config.flow, DraftFlow::FifoAsync);
    assert_eq!(imported.status, Lobby);
    let mut stripped = imported.clone();
    stripped.shared_stack = None;
    assert_eq!(stripped, twin);
    twin.validate_persisted_snapshot().unwrap();

    assert_eq!(
        imported.validate_persisted_snapshot(),
        Err(legacy_live_authority_refusal())
    );
    let before = serde_json::to_string(&imported).unwrap();
    let mut candidate = imported.clone();
    let source = Source {
        size: 6,
        effect_seat: None,
    };
    let counting = CountingSource::new(&source);
    assert_eq!(
        apply_at(
            &mut candidate,
            DraftAction::StartDraft,
            Some(&counting),
            1000
        ),
        Err(legacy_live_authority_refusal())
    );
    assert_eq!(counting.calls.get(), 0);
    assert_eq!(candidate, imported);
    assert_eq!(serde_json::to_string(&candidate).unwrap(), before);
}

#[test]
fn started_fifo_snapshots_carrying_a_shared_stack_are_refused() {
    let winston = started_legacy_winston();
    let stack = winston.shared_stack.as_ref().unwrap();

    // Drafting: a legal selection on the stackless twin is accepted, and the
    // same selection on the snapshot carrying the stack is refused with the
    // session unchanged.
    let drafting = fixture(DraftKind::Premier, 6, 0, 0, 0, None);
    let mut twin = reimported(&drafting);
    twin.validate_persisted_snapshot().unwrap();
    let action = pick_pass::legal_auto_selection(&twin, 0).unwrap();
    assert_eq!(
        accepted(&apply_at(&mut twin, action.clone(), None, 1100).unwrap()).len(),
        1
    );
    let imported = reimported_with_stack(&drafting, stack);
    assert_eq!(imported.shared_stack.as_ref(), Some(stack));
    assert_eq!(
        imported.validate_persisted_snapshot(),
        Err(legacy_live_authority_refusal())
    );
    assert_refusal(&imported, action, 1100, legacy_live_authority_refusal());

    // Review, Paused and Deckbuilding.
    let mut review = fixture(DraftKind::Premier, 2, 0, 0, 1000, None);
    drain_round(&mut review, 1100);
    let mut paused = drafting.clone();
    apply_at(
        &mut paused,
        DraftAction::Control {
            control: DraftControl::Pause {
                reason: DraftPauseReason::PausedByHost,
            },
        },
        None,
        1500,
    )
    .unwrap();
    let mut deckbuilding = fixture(DraftKind::Premier, 2, 0, 0, 0, None);
    drain_round(&mut deckbuilding, 1100);
    drain_round(&mut deckbuilding, 1100);
    for (status, session) in [
        (Review, &review),
        (Paused, &paused),
        (Deckbuilding, &deckbuilding),
    ] {
        assert_eq!(session.status, status);
        reimported(session).validate_persisted_snapshot().unwrap();
        let imported = reimported_with_stack(session, stack);
        assert_eq!(imported.shared_stack.as_ref(), Some(stack));
        assert_eq!(
            imported.validate_persisted_snapshot(),
            Err(legacy_live_authority_refusal()),
            "{status:?}"
        );
    }
}

#[test]
fn healthy_fifo_start_publishes_no_shared_stack() {
    // The stackless twin of the refused Lobby snapshot starts as a healthy
    // FIFO draft.
    let mut started = reimported(&lobby(with_pod_size(
        config(DraftKind::Premier, 6, 0, 0, 0),
        2,
    )));
    started.validate_persisted_snapshot().unwrap();
    let source = Source {
        size: 6,
        effect_seat: None,
    };
    let counting = CountingSource::new(&source);
    apply_at(&mut started, DraftAction::StartDraft, Some(&counting), 1000).unwrap();
    assert_eq!(counting.calls.get(), 4);
    started.validate_persisted_snapshot().unwrap();
    assert_eq!(started.status, Drafting);
    assert!(started.shared_stack.is_none());
    for seat in 0..2 {
        assert!(started.head_pack(seat).is_some());
        assert!(started.fifo.as_ref().unwrap().seats[usize::from(seat)]
            .prompt
            .is_some());
        let view = filter_for_player(&started, seat);
        assert!(view.fifo.is_some());
        assert!(view.shared_stack.is_none());
        assert!(view.play_first_chooser.is_none());
    }
    assert!(filter_for_spectator(&started, SpectatorVisibility::Public)
        .shared_stack
        .is_none());
}

#[test]
fn legacy_winston_snapshot_is_admitted_and_takes_a_pile() {
    let winston = started_legacy_winston();
    winston.validate_persisted_snapshot().unwrap();
    let mut imported = reimported(&winston);
    imported.validate_persisted_snapshot().unwrap();
    let state = imported.shared_stack.as_ref().unwrap();
    let (active, cursor) = (state.active_seat, state.cursor);
    let turn = filter_for_player(&imported, active)
        .shared_stack
        .expect("the active seat sees its pile turn");
    assert_eq!(turn.active_seat, active);
    let pool_before = imported.pools[usize::from(active)].len();

    apply(
        &mut imported,
        DraftAction::SharedStackDecision {
            seat: active,
            pile: cursor,
            decision: SharedStackPileDecision::Take,
        },
        None,
    )
    .unwrap();
    imported.validate_persisted_snapshot().unwrap();
    assert!(imported.pools[usize::from(active)].len() > pool_before);
    let next = imported.shared_stack.as_ref().unwrap().active_seat;
    assert_ne!(next, active);
    let turn = filter_for_player(&imported, next)
        .shared_stack
        .expect("the next seat sees its pile turn");
    assert_eq!(turn.active_seat, next);
}

#[test]
fn omniscient_spectator_packs_follow_the_seat_list() {
    let pack_source = crate::pack_source::FixturePackSource {
        set_code: "TST".into(),
        cards_per_pack: 6,
    };
    let healthy = legacy_fixture(config(DraftKind::Premier, 6, 0, 0, 0), &pack_source);
    // A snapshot that omits `pod_size` reads serde's default of 8 while the
    // session keeps its four seats.
    let mut json = serde_json::to_value(&healthy).unwrap();
    json["config"]
        .as_object_mut()
        .unwrap()
        .remove("pod_size")
        .unwrap();
    let larger: DraftSession = serde_json::from_value(json).unwrap();
    assert_eq!(larger.config.pod_size, 8);
    let mut smaller = healthy.clone();
    smaller.config.pod_size = 2;

    // The larger mismatch first, then the smaller, then the equal count.
    for session in [&larger, &smaller, &healthy] {
        // Reach: legacy pick-and-pass validation never reads `pod_size`, so
        // a host can restore each of these snapshots.
        session.validate_persisted_snapshot().unwrap();
        let before = session.clone();
        let view = filter_for_spectator(session, SpectatorVisibility::Omniscient);
        let current_packs = view
            .current_packs
            .expect("an omniscient spectator sees every seat's pack");
        assert!(current_packs.iter().any(Option::is_some));
        assert_eq!(current_packs.len(), session.seats.len());
        for (seat, pack) in current_packs.iter().enumerate() {
            let seat = u8::try_from(seat).unwrap();
            assert_eq!(pack, &session.head_pack(seat).map(|pack| pack.0.clone()));
        }
        let seat_packs: Vec<_> = session
            .current_pack
            .iter()
            .map(|pack| pack.as_ref().map(|pack| pack.0.clone()))
            .collect();
        assert_eq!(current_packs, seat_packs);
        assert!(view.pools.is_some());
        assert_eq!(session, &before);

        let public = filter_for_spectator(session, SpectatorVisibility::Public);
        assert_eq!(public.seats.len(), 4);
        assert!(public.current_packs.is_none());
        assert!(public.pools.is_none());
    }

    // A Chaos source stays redacted whatever its pod size says.
    let mut chaos_config = config(DraftKind::Premier, 6, 0, 0, 0);
    chaos_config.source = DraftSource::Set {
        layout: SetLayout::Chaos {
            candidate_codes: vec!["TST".into()],
            assignments: vec![vec!["TST".into(); 2]; 4],
        },
    };
    chaos_config.set_code = chaos_config.source.set_code();
    let mut chaos = legacy_fixture(chaos_config, &pack_source);
    chaos.config.pod_size = 8;
    let view = filter_for_spectator(&chaos, SpectatorVisibility::Omniscient);
    assert_eq!(view.seats.len(), 4);
    assert!(view.current_packs.is_none());
    assert!(view.pools.is_none());

    // A FIFO session cannot carry the mismatch, and still projects its
    // queue heads one per seat.
    let fifo = fixture(DraftKind::Premier, 6, 0, 0, 0, None);
    let current_packs = filter_for_spectator(&fifo, SpectatorVisibility::Omniscient)
        .current_packs
        .unwrap();
    assert!(current_packs.iter().any(Option::is_some));
    assert_eq!(current_packs.len(), fifo.seats.len());
    assert_eq!(current_packs.len(), usize::from(fifo.config.pod_size));
    for (seat, pack) in current_packs.iter().enumerate() {
        let seat = u8::try_from(seat).unwrap();
        assert_eq!(pack, &fifo.head_pack(seat).map(|pack| pack.0.clone()));
    }
    let matched = lobby(config(DraftKind::Premier, 6, 0, 0, 0));
    let mut mismatched = config(DraftKind::Premier, 6, 0, 0, 0);
    mismatched.pod_size = 6;
    assert_eq!(
        DraftSession::new_fifo(
            mismatched,
            matched.seats.clone(),
            "same-code".into(),
            DraftSessionId("incarnation-one".into()),
        ),
        Err(DraftError::InvalidConfiguration {
            reason: "pod size mismatch".into()
        })
    );
}

#[test]
fn legacy_upgrade_refuses_an_invalid_session_id_as_a_snapshot_error() {
    let legacy = legacy_fixture(
        config(DraftKind::Premier, 6, 0, 0, 0),
        &crate::pack_source::FixturePackSource {
            set_code: "TST".into(),
            cards_per_pack: 6,
        },
    );
    let context = |id: String, now_ms: u64| LegacyUpgradeContext {
        session_id: DraftSessionId(id),
        now_ms,
        ..migration_context()
    };

    // Reach: with a valid id and time the same source upgrades, including
    // with the longest id the session accepts.
    let upgraded = upgrade_legacy_snapshot(&legacy, migration_context()).unwrap();
    assert_eq!(upgraded.config.flow, DraftFlow::FifoAsync);
    upgraded.validate_persisted_snapshot().unwrap();
    let longest =
        upgrade_legacy_snapshot(&legacy, context("x".repeat(MAX_DRAFT_ID_BYTES), 1000)).unwrap();
    longest.validate_persisted_snapshot().unwrap();

    let mut fifo_config = legacy.config.clone();
    fifo_config.flow = DraftFlow::FifoAsync;
    for id in [
        String::new(),
        "x".repeat(MAX_DRAFT_ID_BYTES + 1),
        "bad\u{7}id".to_string(),
    ] {
        let before = legacy.clone();
        let refusal = upgrade_legacy_snapshot(&legacy, context(id.clone(), 1000));
        assert_eq!(refusal, Err(snapshot_error("invalid session id")));
        // The same refusal a fresh FIFO session gets for that id.
        assert_eq!(
            refusal.unwrap_err(),
            DraftSession::new_fifo(
                fifo_config.clone(),
                legacy.seats.clone(),
                legacy.draft_code.clone(),
                DraftSessionId(id),
            )
            .unwrap_err()
        );
        assert_eq!(legacy, before);
    }

    // An out-of-range time is still a clock refusal, and it is checked
    // before the id.
    assert_eq!(
        upgrade_legacy_snapshot(
            &legacy,
            context("retained-evidence".into(), MAX_DRAFT_JSON_INTEGER + 1)
        ),
        Err(DraftError::InvalidClockBounds)
    );
    assert_eq!(
        upgrade_legacy_snapshot(&legacy, context(String::new(), MAX_DRAFT_JSON_INTEGER + 1)),
        Err(DraftError::InvalidClockBounds)
    );

    // A legacy Lobby takes the same refusal.
    let mut lobby_config = config(DraftKind::Premier, 6, 0, 0, 0);
    lobby_config.flow = DraftFlow::LegacyBarrier;
    lobby_config.clock_policy = DraftClockPolicy::default();
    let seats = (0..4)
        .map(|seat| DraftSeat::Human {
            player_id: PlayerId(seat),
            display_name: format!("Player {seat}"),
        })
        .collect();
    let legacy_lobby = DraftSession::new(lobby_config, seats, "legacy".into());
    let upgraded = upgrade_legacy_snapshot(&legacy_lobby, migration_context()).unwrap();
    assert_eq!(upgraded.config.flow, DraftFlow::FifoAsync);
    upgraded.validate_persisted_snapshot().unwrap();
    let before = legacy_lobby.clone();
    assert_eq!(
        upgrade_legacy_snapshot(&legacy_lobby, context(String::new(), 1000)),
        Err(snapshot_error("invalid session id"))
    );
    assert_eq!(legacy_lobby, before);
}

/// A cube list of twenty entries in list order: nineteen distinct names
/// and a second copy of the first. A four-seat, two-pack, two-card draft
/// deals sixteen of them, so the list also holds names no seat ever sees.
fn booster_cube() -> (crate::cube::CubePackSource, Vec<String>) {
    let template = Source {
        size: 2,
        effect_seat: None,
    }
    .generate_pack(&mut ChaCha20Rng::seed_from_u64(1), 0, 0)
    .0[0]
        .clone();
    let names: Vec<String> = (0..19)
        .map(|entry| format!("Cube Entry {entry:02}"))
        .chain(std::iter::once("Cube Entry 00".to_string()))
        .collect();
    let cards = names
        .iter()
        .map(|name| DraftCardInstance {
            name: name.clone(),
            ..template.clone()
        })
        .collect();
    (crate::cube::CubePackSource::new(cards), names)
}

fn cube_config(kind: DraftKind) -> DraftConfig {
    let mut cfg = config(kind, 2, 0, 0, 0);
    cfg.source = DraftSource::Cube {
        id: "cube".into(),
        name: "cube".into(),
    };
    cfg.set_code = "cube".into();
    cfg
}

fn dealt_names(session: &DraftSession) -> HashSet<String> {
    session
        .fifo
        .as_ref()
        .unwrap()
        .cards
        .values()
        .map(|provenance| provenance.card.name.clone())
        .collect()
}

fn human_seats(pod_size: u8) -> Vec<DraftSeat> {
    (0..pod_size)
        .map(|seat| DraftSeat::Human {
            player_id: PlayerId(seat),
            display_name: format!("Player {seat}"),
        })
        .collect()
}

#[test]
fn fifo_cube_start_latches_the_original_booster_pool() {
    let (cube, expected) = booster_cube();
    let mut session = lobby(cube_config(DraftKind::Quick));
    assert_eq!(session.booster_pack_pool, None);
    apply_at(&mut session, DraftAction::StartDraft, Some(&cube), 1000).unwrap();

    // Reach: the cube dealt sixteen cards and every seat holds a prompted
    // head pack.
    assert_eq!(session.status, Drafting);
    assert_eq!(session.fifo.as_ref().unwrap().cards.len(), 16);
    let dealt = dealt_names(&session);
    assert!(dealt.iter().all(|name| expected.contains(name)));
    for seat in 0..4 {
        assert!(session.head_pack(seat).is_some());
        assert!(session.fifo.as_ref().unwrap().seats[usize::from(seat)]
            .prompt
            .is_some());
    }

    // The latched pool is the source's whole list in order, so it keeps
    // copies and entries no seat was dealt, which the dealt cards alone
    // cannot reproduce.
    assert!(expected.iter().any(|name| !dealt.contains(name)));
    assert!(expected.iter().collect::<HashSet<_>>().len() < expected.len());
    assert_eq!(session.booster_pack_pool.as_ref(), Some(&expected));
    assert_eq!(
        session.booster_pack_pool_for_game(),
        Some(expected.as_slice())
    );
    assert_ne!(session.booster_pack_pool_for_game(), Some(&[][..]));
    session.validate_persisted_snapshot().unwrap();

    // The legacy reducer latches the same value from the same source and
    // seed.
    let legacy = legacy_fixture(cube_config(DraftKind::Quick), &cube);
    assert_eq!(legacy.status, Drafting);
    assert_eq!(legacy.booster_pack_pool, session.booster_pack_pool);

    // A source without a pool reaches the same start and latches nothing.
    let set = fixture(DraftKind::Quick, 2, 0, 0, 0, None);
    assert_eq!(set.status, Drafting);
    assert!((0..4).all(|seat| set.head_pack(seat).is_some()));
    assert_eq!(set.booster_pack_pool, None);
    assert_eq!(set.booster_pack_pool_for_game(), None);

    // AllAtOnce never runs over a cube: the constructor refuses it before
    // anything is generated, while the same Sealed draft over a set deals
    // its pools with no booster pool.
    let sealed = |mut cfg: DraftConfig| {
        cfg.pack_count = DraftKind::Sealed.procedure().packs_per_player;
        cfg.clock_policy = DraftClockPolicy::native(None, 0).unwrap();
        cfg
    };
    let sealed_cube = sealed(cube_config(DraftKind::Sealed));
    let seats = human_seats(sealed_cube.pod_size);
    assert_eq!(
        DraftSession::new_fifo(
            sealed_cube,
            seats,
            "same-code".into(),
            DraftSessionId("incarnation-one".into()),
        ),
        Err(DraftError::SealedRequiresSetSource)
    );
    let mut sealed_set = lobby(sealed(config(DraftKind::Sealed, 2, 0, 0, 0)));
    apply_at(
        &mut sealed_set,
        DraftAction::StartDraft,
        Some(&Source {
            size: 2,
            effect_seat: None,
        }),
        1000,
    )
    .unwrap();
    assert_eq!(sealed_set.status, Deckbuilding);
    assert!(sealed_set.pools.iter().all(|pool| !pool.is_empty()));
    assert_eq!(sealed_set.booster_pack_pool, None);
}

#[test]
fn legacy_cube_upgrade_carries_the_latched_booster_pool() {
    let (cube, expected) = booster_cube();

    // (a) A legacy cube draft started through the legacy reducer latches
    // the pool.
    let legacy = legacy_fixture(cube_config(DraftKind::Premier), &cube);
    assert_eq!(legacy.status, Drafting);
    assert_eq!(legacy.booster_pack_pool.as_ref(), Some(&expected));

    // (b) Its upgrade recovers provenance from the cube's physical ids and
    // keeps exactly that pool.
    let upgraded = upgrade_legacy_snapshot(&legacy, migration_context()).unwrap();
    assert_eq!(upgraded.config.flow, DraftFlow::FifoAsync);
    assert_eq!(upgraded.status, Drafting);
    upgraded.validate_persisted_snapshot().unwrap();
    assert_eq!(upgraded.booster_pack_pool, legacy.booster_pack_pool);
    assert_eq!(
        upgraded.booster_pack_pool_for_game(),
        Some(expected.as_slice())
    );

    // (c) A snapshot saved before sessions recorded a pool upgrades
    // without one and keeps the bounded empty source.
    let mut json = serde_json::to_value(&legacy).unwrap();
    assert!(json
        .as_object_mut()
        .unwrap()
        .remove("booster_pack_pool")
        .is_some());
    let unlatched: DraftSession = serde_json::from_value(json).unwrap();
    assert_eq!(unlatched.booster_pack_pool, None);
    unlatched.validate_persisted_snapshot().unwrap();
    let upgraded = upgrade_legacy_snapshot(&unlatched, migration_context()).unwrap();
    upgraded.validate_persisted_snapshot().unwrap();
    assert_eq!(upgraded.booster_pack_pool, None);
    assert_eq!(upgraded.booster_pack_pool_for_game(), Some(&[][..]));

    // (d) A legacy cube Lobby upgrades without a pool, and its later FIFO
    // start latches the cube's list.
    let mut lobby_config = cube_config(DraftKind::Premier);
    lobby_config.flow = DraftFlow::LegacyBarrier;
    lobby_config.clock_policy = DraftClockPolicy::default();
    let seats = human_seats(lobby_config.pod_size);
    let legacy_lobby = DraftSession::new(lobby_config, seats, "legacy-lobby".into());
    let mut upgraded = upgrade_legacy_snapshot(&legacy_lobby, migration_context()).unwrap();
    assert_eq!(upgraded.status, Lobby);
    upgraded.validate_persisted_snapshot().unwrap();
    assert_eq!(upgraded.booster_pack_pool, None);
    apply_at(&mut upgraded, DraftAction::StartDraft, Some(&cube), 1000).unwrap();
    assert_eq!(upgraded.status, Drafting);
    assert_eq!(upgraded.booster_pack_pool.as_ref(), Some(&expected));

    // (e) A set draft has no pool before or after its upgrade.
    let set = legacy_fixture(
        config(DraftKind::Premier, 2, 0, 0, 0),
        &crate::pack_source::FixturePackSource {
            set_code: "TST".into(),
            cards_per_pack: 2,
        },
    );
    assert_eq!(set.booster_pack_pool, None);
    let upgraded = upgrade_legacy_snapshot(&set, migration_context()).unwrap();
    upgraded.validate_persisted_snapshot().unwrap();
    assert_eq!(upgraded.booster_pack_pool, None);
    assert_eq!(upgraded.booster_pack_pool_for_game(), None);
}

#[test]
fn fifo_booster_pool_stays_private_and_persists() {
    let (cube, expected) = booster_cube();
    let mut session = lobby(cube_config(DraftKind::Quick));
    apply_at(&mut session, DraftAction::StartDraft, Some(&cube), 1000).unwrap();
    assert_eq!(session.booster_pack_pool.as_ref(), Some(&expected));
    assert!(!expected.is_empty());
    let dealt = dealt_names(&session);
    let undealt: Vec<&String> = expected
        .iter()
        .filter(|name| !dealt.contains(*name))
        .collect();
    assert!(!undealt.is_empty());

    // Every projection keeps its ordinary content and carries neither the
    // pool nor any name only the pool holds.
    let assert_private = |session: &DraftSession| {
        let player = filter_for_player(session, 0);
        let mut projections = vec![serde_json::to_string(&player).unwrap()];
        for visibility in [SpectatorVisibility::Public, SpectatorVisibility::Omniscient] {
            let spectator = filter_for_spectator(session, visibility);
            assert_eq!(spectator.seats.len(), 4);
            projections.push(serde_json::to_string(&spectator).unwrap());
        }
        for json in projections {
            assert!(!json.contains("booster_pack_pool"));
            for name in &undealt {
                assert!(!json.contains(&format!("\"{name}\"")), "{name}");
            }
        }
        player
    };
    let player = assert_private(&session);
    assert_eq!(player.status, Drafting);
    assert_eq!(
        card_ids(&player.current_pack.unwrap()),
        card_ids(&session.head_pack(0).unwrap().0)
    );

    session.validate_persisted_snapshot().unwrap();
    let restored: DraftSession =
        serde_json::from_str(&serde_json::to_string(&session).unwrap()).unwrap();
    assert_eq!(restored, session);
    assert_eq!(
        restored.booster_pack_pool_for_game(),
        Some(expected.as_slice())
    );

    // The pool is unchanged through both rounds into deckbuilding.
    drain_round(&mut session, 1100);
    assert_eq!(session.current_pack_number, 1);
    drain_round(&mut session, 1100);
    assert_eq!(session.status, Deckbuilding);
    assert_eq!(session.booster_pack_pool.as_ref(), Some(&expected));
    session.validate_persisted_snapshot().unwrap();
    assert_eq!(assert_private(&session).status, Deckbuilding);
}

#[test]
fn fifo_only_actions_are_classified_once() {
    let prompt_id = DraftPromptId {
        session_id: DraftSessionId("incarnation-one".into()),
        seat: 0,
        generation: 1,
    };
    let actions = [
        (DraftAction::StartDraft, false),
        (
            DraftAction::Select {
                seat: 0,
                prompt_id,
                picked_card_instance_ids: vec!["s0-r0-z".into()],
                burned_card_instance_ids: vec![],
                effect_card_instance_ids: vec![],
            },
            true,
        ),
        (
            DraftAction::Control {
                control: DraftControl::Resume,
            },
            true,
        ),
        (DraftAction::AdvanceClock, true),
        (
            DraftAction::Pick {
                seat: 0,
                card_instance_ids: vec!["s0-r0-z".into()],
            },
            false,
        ),
        (
            DraftAction::PickWithDraftEffect {
                seat: 0,
                effect_card_instance_id: "s0-r0-z".into(),
                card_instance_ids: vec!["s0-r0-a".into()],
            },
            false,
        ),
        (
            DraftAction::SubmitDeck {
                seat: 0,
                main_deck: vec![],
                commanders: vec![],
            },
            false,
        ),
        (DraftAction::GeneratePairings, false),
        (
            DraftAction::ReportMatchResult {
                match_id: "match".into(),
                winner_seat: None,
            },
            false,
        ),
        (DraftAction::AdvanceRound, false),
        (
            DraftAction::ReplaceSeatWithBot {
                seat: 0,
                name: None,
            },
            false,
        ),
        (
            DraftAction::SetSeatConnected {
                seat: 0,
                connected: true,
            },
            false,
        ),
        (
            DraftAction::SharedStackDecision {
                seat: 0,
                pile: 0,
                decision: SharedStackPileDecision::Take,
            },
            false,
        ),
    ];
    assert_eq!(actions.len(), 13);

    let mut legacy = legacy_fixture(
        config(DraftKind::Premier, 6, 0, 0, 0),
        &Source {
            size: 6,
            effect_seat: None,
        },
    );
    assert_eq!(legacy.status, Drafting);
    for (action, fifo_only) in actions {
        assert_eq!(action.is_fifo_only(), fifo_only, "{action:?}");
        if fifo_only {
            let before = legacy.clone();
            assert_eq!(
                apply(&mut legacy, action, None),
                Err(DraftError::IncompatibleFlow)
            );
            assert_eq!(legacy, before);
        }
    }

    // Reach: the same legacy session still takes an ordinary pick.
    legacy_pick(&mut legacy, 0, "s0-r0-z");
    assert_eq!(legacy.pools[0].len(), 1);
}

#[test]
fn clock_scope_validation_follows_the_pause_state() {
    let now = 1000;
    let duration = 500;
    let pick = DraftClockTarget::Pick {
        prompt_id: DraftPromptId {
            session_id: DraftSessionId("incarnation-one".into()),
            seat: 0,
            generation: 1,
        },
    };
    let review = DraftClockTarget::Review { next_round: 1 };
    let mismatch = Err(snapshot_error("clock status/bounds mismatch"));
    for (target, phase) in [
        (&pick, DraftActivePhase::Drafting),
        (&review, DraftActivePhase::Review),
    ] {
        let paused = Some(DraftPausedState {
            phase,
            reason: DraftPauseReason::PausedByHost,
        });
        let clock = |state| DraftClock {
            target: target.clone(),
            state,
        };
        let running = clock(DraftClockState::Running {
            deadline_ms: now + 500,
        });
        let frozen = clock(DraftClockState::Frozen { remaining_ms: 500 });

        // A running clock belongs to a live session and a frozen one to a
        // paused session.
        assert_eq!(
            validate_scope(&running, target, None, duration, now),
            Ok(())
        );
        assert_eq!(
            validate_scope(&frozen, target, paused, duration, now),
            Ok(())
        );
        assert_eq!(
            validate_scope(&running, target, paused, duration, now),
            mismatch
        );
        assert_eq!(
            validate_scope(&frozen, target, None, duration, now),
            mismatch
        );

        // Bounds still apply in both states.
        let late = clock(DraftClockState::Running {
            deadline_ms: now + 501,
        });
        assert_eq!(validate_scope(&late, target, None, duration, now), mismatch);
        let long = clock(DraftClockState::Frozen { remaining_ms: 501 });
        assert_eq!(
            validate_scope(&long, target, paused, duration, now),
            mismatch
        );
    }

    // A clock for another scope is refused whatever its state.
    let running = DraftClock {
        target: pick,
        state: DraftClockState::Running {
            deadline_ms: now + 500,
        },
    };
    assert_eq!(
        validate_scope(&running, &review, None, duration, now),
        Err(snapshot_error("clock scope mismatch"))
    );
}

/// Seat 1 has drafted the additional-pick card from its own pack and now
/// holds seat 0's remainder as its head, with a fresh prompt.
fn effect_ready(kind: DraftKind, size: u8, burns: u8) -> DraftSession {
    let mut session = fixture(kind, size, burns, 0, 0, Some(1));
    choose(&mut session, 1, 1100);
    choose(&mut session, 0, 1100);
    session
}

fn effect_selection(session: &DraftSession, picks: Vec<String>, burns: Vec<String>) -> DraftAction {
    select(session, 1, picks, burns, vec!["s1-r0-z".into()])
}

/// The additional-pick cards in `seat`'s pool, in pool order.
fn effect_cards(session: &DraftSession, seat: u8) -> Vec<String> {
    session.pools[usize::from(seat)]
        .iter()
        .filter(|card| card.draft_effect == Some(DraftEffect::AdditionalPick))
        .map(|card| card.instance_id.clone())
        .collect()
}

/// Every physical card the session holds: unopened packs, queued heads,
/// pools and removed cards.
fn physical_cards(session: &DraftSession) -> usize {
    let fifo = session.fifo.as_ref().unwrap();
    let unopened: usize = session
        .packs_by_seat
        .iter()
        .flatten()
        .map(|pack| pack.0.len())
        .sum();
    let queued: usize = fifo
        .seats
        .iter()
        .flat_map(|seat| &seat.queue)
        .map(|head| head.pack.0.len())
        .sum();
    let pooled: usize = session.pools.iter().map(Vec::len).sum();
    unopened + queued + pooled + fifo.removed.len()
}

/// `Source`, with the effect seat's second card in every pack also an
/// additional-pick card.
struct SecondEffectSource(Source);

impl PackSource for SecondEffectSource {
    fn generate_pack(&self, rng: &mut dyn rand::RngCore, seat: u8, round: u8) -> DraftPack {
        let mut pack = self.0.generate_pack(rng, seat, round);
        if self.0.effect_seat == Some(seat) {
            pack.0[1].draft_effect = Some(DraftEffect::AdditionalPick);
        }
        pack
    }
}

/// Seat 1 drafts the first cards of its own pack in each of the two rounds
/// through ordinary accepted selections, and then holds seat 2's second-round
/// remainder as its head, with a fresh prompt.
fn second_round_ready(cfg: DraftConfig, source: &dyn PackSource) -> DraftSession {
    let mut session = lobby(cfg);
    apply_at(&mut session, DraftAction::StartDraft, Some(source), 1000).unwrap();
    for round in 0..2 {
        assert_eq!(session.current_pack_number, round);
        let drafted = effect_cards(&session, 1).len();
        let events = choose(&mut session, 1, 1100);
        let accepted_events = accepted(&events);
        let [DraftDelta::SelectionAccepted {
            picked_card_instance_ids,
            ..
        }] = accepted_events.as_slice()
        else {
            panic!("one accepted selection in round {round}");
        };
        // The step's picks include the additional-pick card it drafted.
        let pool_effects = effect_cards(&session, 1);
        let new_effects = &pool_effects[drafted..];
        assert!(
            !new_effects.is_empty()
                && new_effects
                    .iter()
                    .all(|id| picked_card_instance_ids.contains(id)),
            "round {round}: {new_effects:?} from {picked_card_instance_ids:?}"
        );
        if round == 0 {
            drain_round(&mut session, 1100);
        }
    }
    choose(&mut session, 2, 1100);
    session
}

/// `second_round_ready` with one additional-pick card at the front of each
/// pack seat 1 opens: seat 1 drafts two of them.
fn two_effects_ready(kind: DraftKind, size: u8, burns: u8) -> DraftSession {
    second_round_ready(
        config(kind, size, burns, 0, 0),
        &Source {
            size,
            effect_seat: Some(1),
        },
    )
}

#[test]
fn fifo_commander_draft_effect_takes_the_step_plus_one_card() {
    // Commander Draft drafts two cards per step, so a step that uses the
    // additional-pick card drafts three and returns the card to the pack.
    let mut session = effect_ready(DraftKind::CommanderDraft, 6, 0);
    // Reach, read without the effect count: the head holds four cards and
    // the view offers the effect card.
    assert_eq!(
        card_ids(&session.head_pack(1).unwrap().0),
        ids(0, &["y", "b", "x", "c"])
    );
    let effects = filter_for_player(&session, 1)
        .fifo
        .unwrap()
        .effects
        .expect("the view offers the effect card");
    assert_eq!(effects.card_instance_ids, vec!["s1-r0-z".to_string()]);
    assert_eq!(effects.requirements[0].picks, 3);
    assert_eq!(effects.requirements[0].burns, 0);
    assert_eq!(card_ids(&session.pools[1]), ids(1, &["z", "a"]));
    let head_pack_id = session.head_metadata(1).unwrap().pack_id;
    let old = prompt(&session, 1);

    // Two picks with the effect are one card short.
    assert_refusal(
        &session,
        effect_selection(&session, ids(0, &["y", "b"]), vec![]),
        1100,
        DraftError::InvalidSelectionCount {
            role: DraftSelectionRole::Pick,
            expected: 3,
            actual: 2,
        },
    );
    // Without the effect the step is still two cards.
    assert_eq!(
        pick_pass::selection_requirements(&session, 1, &[]),
        Ok(DraftSelectionRequirements { picks: 2, burns: 0 })
    );
    let mut plain = session.clone();
    let action = select(&session, 1, ids(0, &["y", "b"]), vec![], vec![]);
    assert_eq!(
        accepted(&apply_at(&mut plain, action, None, 1100).unwrap()).len(),
        1
    );

    let action = effect_selection(&session, ids(0, &["y", "b", "x"]), vec![]);
    let events = apply_at(&mut session, action, None, 1100).unwrap();
    assert_eq!(
        accepted(&events),
        vec![expected(
            old,
            ids(0, &["y", "b", "x"]),
            vec![],
            vec!["s1-r0-z".into()],
            vec![],
        )]
    );
    let mut pool = ids(1, &["a"]);
    pool.extend(ids(0, &["y", "b", "x"]));
    assert_eq!(card_ids(&session.pools[1]), pool);
    let fifo = session.fifo.as_ref().unwrap();
    let passed = fifo.seats[2].queue.back().unwrap();
    assert_eq!(passed.pack_id, head_pack_id);
    assert_eq!(
        card_ids(&passed.pack.0),
        vec!["s0-r0-c".to_string(), "s1-r0-z".to_string()]
    );
    assert_eq!(fifo.returned_effects.get("s1-r0-z"), Some(&head_pack_id));

    // A burn policy burns from what the three picks leave: none of a
    // three-card head.
    let mut session = effect_ready(DraftKind::CommanderDraft, 6, 1);
    assert_eq!(
        card_ids(&session.head_pack(1).unwrap().0),
        ids(0, &["b", "x", "c"])
    );
    assert_eq!(
        pick_pass::selection_requirements(&session, 1, &["s1-r0-z".to_string()]),
        Ok(DraftSelectionRequirements { picks: 3, burns: 0 })
    );
    let action = effect_selection(&session, ids(0, &["b", "x", "c"]), vec![]);
    assert_eq!(
        accepted(&apply_at(&mut session, action, None, 1100).unwrap()).len(),
        1
    );

    // ... and one card of a five-card head.
    struct EightCardSource;
    impl PackSource for EightCardSource {
        fn generate_pack(&self, _: &mut dyn rand::RngCore, seat: u8, round: u8) -> DraftPack {
            DraftPack(
                (0..8u8)
                    .map(|index| DraftCardInstance {
                        instance_id: format!("s{seat}-r{round}-{index}"),
                        name: format!("Card {index}"),
                        set_code: "TST".into(),
                        collector_number: index.to_string(),
                        rarity: "common".into(),
                        colors: vec![],
                        cmc: 0,
                        type_line: "Artifact".into(),
                        draft_effect: (seat == 1 && index == 0)
                            .then_some(DraftEffect::AdditionalPick),
                    })
                    .collect(),
            )
        }
    }
    let mut session = lobby(config(DraftKind::CommanderDraft, 8, 1, 0, 0));
    let events = apply_at(
        &mut session,
        DraftAction::StartDraft,
        Some(&EightCardSource),
        1000,
    )
    .unwrap();
    assert!(accepted(&events).is_empty());
    choose(&mut session, 1, 1100);
    choose(&mut session, 0, 1100);
    let head: Vec<String> = (3..8).map(|index| format!("s0-r0-{index}")).collect();
    assert_eq!(card_ids(&session.head_pack(1).unwrap().0), head);
    assert_eq!(
        pick_pass::selection_requirements(&session, 1, &["s1-r0-0".to_string()]),
        Ok(DraftSelectionRequirements { picks: 3, burns: 1 })
    );
    let old = prompt(&session, 1);
    let action = select(
        &session,
        1,
        head[..3].to_vec(),
        head[3..4].to_vec(),
        vec!["s1-r0-0".into()],
    );
    let events = apply_at(&mut session, action, None, 1100).unwrap();
    assert_eq!(
        accepted(&events),
        vec![expected(
            old,
            head[..3].to_vec(),
            head[3..4].to_vec(),
            vec!["s1-r0-0".into()],
            vec![],
        )]
    );

    // A one-card kind's effect step takes exactly two cards.
    for kind in [DraftKind::Quick, DraftKind::Premier, DraftKind::Traditional] {
        let session = effect_ready(kind, 6, 0);
        assert_eq!(
            card_ids(&session.head_pack(1).unwrap().0),
            ids(0, &["a", "y", "b", "x", "c"]),
            "{kind:?}"
        );
        let mut two = session.clone();
        let action = effect_selection(&session, ids(0, &["a", "y"]), vec![]);
        assert_eq!(
            accepted(&apply_at(&mut two, action, None, 1100).unwrap()).len(),
            1,
            "{kind:?}"
        );
        assert_refusal(
            &session,
            effect_selection(&session, ids(0, &["a", "y", "b"]), vec![]),
            1100,
            DraftError::InvalidSelectionCount {
                role: DraftSelectionRole::Pick,
                expected: 2,
                actual: 3,
            },
        );
    }
}

#[test]
fn fifo_effect_requirement_is_the_pick_step_plus_one_for_every_kind() {
    let mut one_card_kinds = 0usize;
    let mut multi_card_kinds = 0usize;
    for kind in DraftKind::ALL {
        match kind.procedure().distribution {
            PackDistribution::PickAndPass => {}
            PackDistribution::AllAtOnce | PackDistribution::SharedStackPiles { .. } => continue,
        }
        let cards_per_pick = usize::from(kind.procedure().cards_per_pick);
        let session = effect_ready(kind, 6, 0);
        // Reach: the head is large enough for the effect's step.
        assert!(
            session.head_pack(1).unwrap().0.len() > cards_per_pick,
            "{kind:?}"
        );
        let expected = DraftSelectionRequirements {
            picks: cards_per_pick + 1,
            burns: 0,
        };
        assert_eq!(
            pick_pass::selection_requirements(&session, 1, &["s1-r0-z".to_string()]),
            Ok(expected),
            "{kind:?}"
        );
        let effects = filter_for_player(&session, 1)
            .fifo
            .unwrap()
            .effects
            .expect("the view offers the effect card");
        assert_eq!(
            effects.card_instance_ids,
            vec!["s1-r0-z".to_string()],
            "{kind:?}"
        );
        assert_eq!(effects.requirements, vec![expected], "{kind:?}");
        if cards_per_pick == 1 {
            one_card_kinds += 1;
        } else {
            multi_card_kinds += 1;
        }
    }
    assert!(one_card_kinds >= 1, "the loop must reach a one-card kind");
    assert!(
        multi_card_kinds >= 1,
        "the loop must reach a multi-card kind"
    );

    // Commander Draft offers the effect only when the head holds three.
    let session = effect_ready(DraftKind::CommanderDraft, 4, 0);
    assert_eq!(
        card_ids(&session.head_pack(1).unwrap().0),
        ids(0, &["y", "b"])
    );
    assert!(filter_for_player(&session, 1)
        .fifo
        .unwrap()
        .effects
        .is_none());
    let session = effect_ready(DraftKind::CommanderDraft, 5, 0);
    assert_eq!(
        card_ids(&session.head_pack(1).unwrap().0),
        ids(0, &["y", "b", "x"])
    );
    let effects = filter_for_player(&session, 1)
        .fifo
        .unwrap()
        .effects
        .expect("the view offers the effect card");
    assert_eq!(effects.card_instance_ids, vec!["s1-r0-z".to_string()]);
    assert_eq!(
        effects.requirements,
        vec![DraftSelectionRequirements { picks: 3, burns: 0 }]
    );
}

#[test]
fn fifo_effect_on_a_too_small_head_is_an_invalid_effect_selection() {
    // Reach: the same seat and card on a large head are available.
    let session = effect_ready(DraftKind::Premier, 6, 0);
    assert_eq!(
        pick_pass::selection_requirements(&session, 1, &["s1-r0-z".to_string()]),
        Ok(DraftSelectionRequirements { picks: 2, burns: 0 })
    );

    // A one-card Premier head cannot supply the additional card.
    let premier = effect_ready(DraftKind::Premier, 2, 0);
    assert_eq!(card_ids(&premier.head_pack(1).unwrap().0), ids(0, &["a"]));
    assert_eq!(card_ids(&premier.pools[1]), ids(1, &["z"]));
    let too_small = DraftError::InvalidDraftEffectSelection {
        expected_cards: 2,
        actual_cards: 1,
    };
    assert_eq!(
        pick_pass::selection_requirements(&premier, 1, &["s1-r0-z".to_string()]),
        Err(too_small.clone())
    );
    assert_refusal(
        &premier,
        effect_selection(&premier, ids(0, &["a"]), vec![]),
        1100,
        too_small,
    );
    assert!(filter_for_player(&premier, 1)
        .fifo
        .unwrap()
        .effects
        .is_none());

    // Commander Draft heads of one and two cards are short of three.
    for (size, head) in [(3, &["y"][..]), (4, &["y", "b"][..])] {
        let session = effect_ready(DraftKind::CommanderDraft, size, 0);
        assert_eq!(card_ids(&session.head_pack(1).unwrap().0), ids(0, head));
        let too_small = DraftError::InvalidDraftEffectSelection {
            expected_cards: 3,
            actual_cards: head.len(),
        };
        assert_eq!(
            pick_pass::selection_requirements(&session, 1, &["s1-r0-z".to_string()]),
            Err(too_small.clone()),
            "{size}"
        );
        assert_refusal(
            &session,
            effect_selection(&session, ids(0, head), vec![]),
            1100,
            too_small,
        );
        assert!(filter_for_player(&session, 1)
            .fifo
            .unwrap()
            .effects
            .is_none());
    }

    // Membership is checked before the head's size: a card that is not an
    // additional-pick card in this seat's pool is refused as such.
    let commander = effect_ready(DraftKind::CommanderDraft, 4, 0);
    for (session, id) in [(&premier, "s0-r0-z"), (&commander, "s1-r0-a")] {
        let not_in_pool = DraftError::DraftEffectCardNotInPool {
            card_instance_id: id.into(),
        };
        assert_eq!(
            pick_pass::selection_requirements(session, 1, &[id.to_string()]),
            Err(not_in_pool.clone()),
            "{id}"
        );
        let head = card_ids(&session.head_pack(1).unwrap().0);
        assert_refusal(
            session,
            select(session, 1, head, vec![], vec![id.into()]),
            1100,
            not_in_pool,
        );
    }

    // A seat with no head has no pending pack.
    assert!(premier.head_pack(0).is_none());
    assert_eq!(
        pick_pass::selection_requirements(&premier, 0, &["s1-r0-z".to_string()]),
        Err(DraftError::NoPendingPack { seat: 0 })
    );
}

#[test]
fn fifo_two_effect_cards_drafted_through_picks_return_on_one_head() {
    // Seat 1 drafts two additional-pick cards through ordinary selections and
    // uses both on one head, so a one-card kind's step takes three cards.
    let mut session = two_effects_ready(DraftKind::Premier, 6, 0);
    let effects = effect_cards(&session, 1);
    let head = card_ids(&session.head_pack(1).unwrap().0);
    // Reach, read without the effect count: the pool holds both cards, the
    // head holds at least three, and the view lists both.
    assert_eq!(effects, vec!["s1-r0-z".to_string(), "s1-r1-z".to_string()]);
    assert!(head.len() >= 3, "{head:?}");
    let options = filter_for_player(&session, 1)
        .fifo
        .unwrap()
        .effects
        .expect("the view offers the effect cards");
    assert_eq!(options.card_instance_ids, effects);
    assert_eq!(options.requirements[1].picks, 3);

    let head_pack_id = session.head_metadata(1).unwrap().pack_id;
    let old = prompt(&session, 1);
    let pool = card_ids(&session.pools[1]);
    let cards = physical_cards(&session);
    // Without an effect card the step is one card; with one it is two, and
    // that card returns to the pack.
    assert_eq!(
        pick_pass::selection_requirements(&session, 1, &[]),
        Ok(DraftSelectionRequirements { picks: 1, burns: 0 })
    );
    let mut plain = session.clone();
    let action = select(&session, 1, head[..1].to_vec(), vec![], vec![]);
    assert_eq!(
        accepted(&apply_at(&mut plain, action, None, 1100).unwrap()),
        vec![expected(
            old.clone(),
            head[..1].to_vec(),
            vec![],
            vec![],
            vec![]
        )]
    );
    let mut one = session.clone();
    let action = select(
        &session,
        1,
        head[..2].to_vec(),
        vec![],
        effects[..1].to_vec(),
    );
    assert_eq!(
        accepted(&apply_at(&mut one, action, None, 1100).unwrap()),
        vec![expected(
            old.clone(),
            head[..2].to_vec(),
            vec![],
            effects[..1].to_vec(),
            vec![]
        )]
    );
    // Two effect cards with two picks are one card short.
    assert_refusal(
        &session,
        select(&session, 1, head[..2].to_vec(), vec![], effects.clone()),
        1100,
        DraftError::InvalidSelectionCount {
            role: DraftSelectionRole::Pick,
            expected: 3,
            actual: 2,
        },
    );

    let action = select(&session, 1, head[..3].to_vec(), vec![], effects.clone());
    let events = apply_at(&mut session, action.clone(), None, 1100)
        .expect("two effect cards with three picks are accepted");
    assert_eq!(
        accepted(&events),
        vec![expected(
            old,
            head[..3].to_vec(),
            vec![],
            effects.clone(),
            vec![]
        )]
    );
    // The three picks enter the pool and both effect cards leave it.
    let mut drafted = pool;
    drafted.retain(|id| !effects.contains(id));
    drafted.extend(head[..3].iter().cloned());
    assert_eq!(card_ids(&session.pools[1]), drafted);
    // The remainder reaches seat 0 with both cards at its end, in list order,
    // each recorded against the head's pack.
    let fifo = session.fifo.as_ref().unwrap();
    let passed = fifo.seats[0].queue.back().unwrap();
    assert_eq!(passed.pack_id, head_pack_id);
    let mut remainder = head[3..].to_vec();
    remainder.extend(effects.iter().cloned());
    assert_eq!(card_ids(&passed.pack.0), remainder);
    for id in &effects {
        assert_eq!(fifo.returned_effects.get(id), Some(&head_pack_id), "{id}");
    }
    // The prompt for that head is consumed. Seat 1 now holds no head, so the
    // same action is refused and changes nothing.
    assert!(fifo.seats[1].prompt.is_none());
    assert_refusal(
        &session,
        action,
        1100,
        DraftError::NoPendingPack { seat: 1 },
    );
    // Every physical card is still accounted for.
    assert_eq!(session.validate_persisted_snapshot(), Ok(()));
    assert_eq!(physical_cards(&session), cards);

    // Commander Draft: two effect cards take four picks, and a head of four
    // cannot supply a third.
    let commander = second_round_ready(
        config(DraftKind::CommanderDraft, 6, 0, 0, 0),
        &SecondEffectSource(Source {
            size: 6,
            effect_seat: Some(1),
        }),
    );
    let effects = effect_cards(&commander, 1);
    let head = card_ids(&commander.head_pack(1).unwrap().0);
    assert_eq!(effects.len(), 4);
    assert_eq!(head.len(), 4);
    let mut two = commander.clone();
    let action = select(&commander, 1, head.clone(), vec![], effects[..2].to_vec());
    assert_eq!(
        accepted(&apply_at(&mut two, action, None, 1100).unwrap()),
        vec![expected(
            prompt(&commander, 1),
            head.clone(),
            vec![],
            effects[..2].to_vec(),
            vec![]
        )]
    );
    assert_eq!(two.validate_persisted_snapshot(), Ok(()));
    assert_refusal(
        &commander,
        select(&commander, 1, head, vec![], effects[..3].to_vec()),
        1100,
        DraftError::InvalidDraftEffectSelection {
            expected_cards: 5,
            actual_cards: 4,
        },
    );

    // The discard threshold applies after both returns: a remainder at the
    // threshold, returned cards included, is discarded with them.
    let mut session = second_round_ready(
        config(DraftKind::Premier, 6, 0, 4, 0),
        &Source {
            size: 6,
            effect_seat: Some(1),
        },
    );
    let effects = effect_cards(&session, 1);
    let head = card_ids(&session.head_pack(1).unwrap().0);
    let old = prompt(&session, 1);
    let action = select(&session, 1, head[..3].to_vec(), vec![], effects.clone());
    let events = apply_at(&mut session, action, None, 1100).unwrap();
    let mut discarded = head[3..].to_vec();
    discarded.extend(effects.iter().cloned());
    assert_eq!(
        accepted(&events),
        vec![expected(
            old,
            head[..3].to_vec(),
            vec![],
            effects,
            discarded
        )]
    );
    assert!(session.fifo.as_ref().unwrap().returned_effects.is_empty());
    assert_eq!(session.validate_persisted_snapshot(), Ok(()));
}

#[test]
fn fifo_effect_cards_are_distinct_cards_in_the_seats_pool() {
    let session = second_round_ready(
        config(DraftKind::Premier, 6, 0, 0, 0),
        &SecondEffectSource(Source {
            size: 6,
            effect_seat: Some(1),
        }),
    );
    let effects = effect_cards(&session, 1);
    let head = card_ids(&session.head_pack(1).unwrap().0);
    // Reach: two distinct cards in the seat's pool are a three-pick step.
    assert_eq!(effects.len(), 2);
    let (a, b) = (effects[0].clone(), effects[1].clone());
    assert_eq!(
        pick_pass::selection_requirements(&session, 1, &[a.clone(), b.clone()]),
        Ok(DraftSelectionRequirements { picks: 3, burns: 0 })
    );
    // A card listed twice is one physical card: refused, without a panic, and
    // nothing changes.
    let repeated = DraftError::InvalidSelection {
        role: DraftSelectionRole::Effect,
        reason: "duplicate or overlapping physical id".into(),
    };
    assert_eq!(
        pick_pass::selection_requirements(&session, 1, &[a.clone(), a.clone()]),
        Err(repeated.clone())
    );
    assert_refusal(
        &session,
        select(
            &session,
            1,
            head[..3].to_vec(),
            vec![],
            vec![a.clone(), a.clone()],
        ),
        1100,
        repeated,
    );
    // Another seat's additional-pick card, and the seat's own card without the
    // effect, are not draft-effect cards in its pool, in either list position.
    let foreign = effect_cards(&session, 2);
    assert!(
        !foreign.is_empty(),
        "seat 2 drafted an additional-pick card"
    );
    let plain = session.pools[1]
        .iter()
        .find(|card| card.draft_effect.is_none())
        .unwrap()
        .instance_id
        .clone();
    for id in [foreign[0].clone(), plain] {
        let not_in_pool = DraftError::DraftEffectCardNotInPool {
            card_instance_id: id.clone(),
        };
        for list in [vec![id.clone(), a.clone()], vec![a.clone(), id.clone()]] {
            assert_eq!(
                pick_pass::selection_requirements(&session, 1, &list),
                Err(not_in_pool.clone()),
                "{list:?}"
            );
            assert_refusal(
                &session,
                select(&session, 1, head[..3].to_vec(), vec![], list),
                1100,
                not_in_pool.clone(),
            );
        }
    }
    // An effect card also named as a pick keeps today's refusal.
    let mut picks = head[..2].to_vec();
    picks.insert(0, a.clone());
    assert_refusal(
        &session,
        select(&session, 1, picks, vec![], vec![a.clone(), b.clone()]),
        1100,
        DraftError::CardNotInPack {
            card_instance_id: a.clone(),
        },
    );
    // An empty list is the selection without an effect card.
    assert_eq!(
        pick_pass::selection_requirements(&session, 1, &[]),
        Ok(DraftSelectionRequirements { picks: 1, burns: 0 })
    );
    let mut plain = session.clone();
    let old = prompt(&session, 1);
    let action = select(&session, 1, head[..1].to_vec(), vec![], vec![]);
    assert_eq!(
        accepted(&apply_at(&mut plain, action, None, 1100).unwrap()),
        vec![expected(old, head[..1].to_vec(), vec![], vec![], vec![])]
    );

    // An effect card also named as a burn keeps today's refusal.
    let burning = two_effects_ready(DraftKind::Premier, 6, 1);
    let effects = effect_cards(&burning, 1);
    let head = card_ids(&burning.head_pack(1).unwrap().0);
    assert_eq!(
        pick_pass::selection_requirements(&burning, 1, &effects),
        Ok(DraftSelectionRequirements { picks: 3, burns: 1 })
    );
    assert_refusal(
        &burning,
        select(
            &burning,
            1,
            head[..3].to_vec(),
            vec![effects[0].clone()],
            effects.clone(),
        ),
        1100,
        DraftError::CardNotInPack {
            card_instance_id: effects[0].clone(),
        },
    );
}

#[test]
fn fifo_view_publishes_the_requirement_for_each_usable_effect_count() {
    let mut one_card_kinds = 0usize;
    let mut multi_card_kinds = 0usize;
    for kind in DraftKind::ALL {
        match kind.procedure().distribution {
            PackDistribution::PickAndPass => {}
            PackDistribution::AllAtOnce | PackDistribution::SharedStackPiles { .. } => continue,
        }
        let cards_per_pick = usize::from(kind.procedure().cards_per_pick);
        for burns in [0, 1] {
            let session = two_effects_ready(kind, 6, burns);
            let effects = effect_cards(&session, 1);
            let head = session.head_pack(1).unwrap().0.len();
            // Reach: two eligible cards and a head that can supply at least one
            // additional card.
            assert_eq!(effects.len(), 2, "{kind:?}");
            assert!(head > cards_per_pick, "{kind:?}");
            // The test's own oracle: k cards take the step plus k, and burn
            // from what the picks leave.
            let usable = effects.len().min(head - cards_per_pick);
            let expected: Vec<_> = (1..=usable)
                .map(|used| {
                    let picks = cards_per_pick + used;
                    DraftSelectionRequirements {
                        picks,
                        burns: usize::from(burns).min(head - picks),
                    }
                })
                .collect();
            let options = filter_for_player(&session, 1)
                .fifo
                .unwrap()
                .effects
                .expect("the view offers the effect cards");
            assert_eq!(options.card_instance_ids, effects, "{kind:?}");
            assert_eq!(options.requirements, expected, "{kind:?}, {burns} burns");
            for (index, requirement) in expected.iter().enumerate() {
                assert_eq!(
                    pick_pass::selection_requirements(&session, 1, &effects[..=index]),
                    Ok(*requirement),
                    "{kind:?}, {burns} burns"
                );
            }
            // The view never offers more cards than the head can supply.
            if usable < effects.len() {
                assert_eq!(
                    pick_pass::selection_requirements(&session, 1, &effects[..=usable]),
                    Err(DraftError::InvalidDraftEffectSelection {
                        expected_cards: cards_per_pick + usable + 1,
                        actual_cards: head,
                    }),
                    "{kind:?}, {burns} burns"
                );
            }
        }
        if cards_per_pick == 1 {
            one_card_kinds += 1;
        } else {
            multi_card_kinds += 1;
        }
    }
    assert!(one_card_kinds >= 1, "the loop must reach a one-card kind");
    assert!(
        multi_card_kinds >= 1,
        "the loop must reach a multi-card kind"
    );

    // A Commander Draft head of three with two eligible cards offers one
    // count, three picks.
    let session = two_effects_ready(DraftKind::CommanderDraft, 5, 0);
    assert_eq!(session.head_pack(1).unwrap().0.len(), 3);
    let options = filter_for_player(&session, 1)
        .fifo
        .unwrap()
        .effects
        .expect("the view offers the effect cards");
    assert_eq!(options.card_instance_ids.len(), 2);
    assert_eq!(
        options.requirements,
        vec![DraftSelectionRequirements { picks: 3, burns: 0 }]
    );
    // The spectator projection publishes no requirement.
    assert!(serde_json::to_string(&filter_for_player(&session, 1))
        .unwrap()
        .contains("\"requirements\""));
    for visibility in [SpectatorVisibility::Public, SpectatorVisibility::Omniscient] {
        assert!(
            !serde_json::to_string(&filter_for_spectator(&session, visibility))
                .unwrap()
                .contains("\"requirements\""),
            "{visibility:?}"
        );
    }
}

#[test]
fn fifo_effects_on_a_too_small_head_are_an_invalid_effect_selection() {
    // Reach: the same seat and cards on a large head are available.
    let large = two_effects_ready(DraftKind::Premier, 6, 0);
    let effects = effect_cards(&large, 1);
    assert_eq!(
        pick_pass::selection_requirements(&large, 1, &effects),
        Ok(DraftSelectionRequirements { picks: 3, burns: 0 })
    );
    // Two cards on a two-card Premier head, and on a three-card Commander
    // Draft head, are short of the step plus two.
    for (kind, size, expected_cards) in [
        (DraftKind::Premier, 3, 3),
        (DraftKind::CommanderDraft, 5, 4),
    ] {
        let session = two_effects_ready(kind, size, 0);
        let effects = effect_cards(&session, 1);
        let head = card_ids(&session.head_pack(1).unwrap().0);
        assert_eq!(effects.len(), 2, "{kind:?}");
        assert_eq!(head.len(), expected_cards - 1, "{kind:?}");
        let too_small = DraftError::InvalidDraftEffectSelection {
            expected_cards,
            actual_cards: head.len(),
        };
        assert_eq!(
            pick_pass::selection_requirements(&session, 1, &effects),
            Err(too_small.clone()),
            "{kind:?}"
        );
        assert_refusal(
            &session,
            select(&session, 1, head.clone(), vec![], effects.clone()),
            1100,
            too_small,
        );
        // Distinctness and membership are checked before the head's size.
        assert_eq!(
            pick_pass::selection_requirements(
                &session,
                1,
                &[effects[0].clone(), effects[0].clone()]
            ),
            Err(DraftError::InvalidSelection {
                role: DraftSelectionRole::Effect,
                reason: "duplicate or overlapping physical id".into(),
            }),
            "{kind:?}"
        );
        assert_eq!(
            pick_pass::selection_requirements(&session, 1, &[effects[0].clone(), head[0].clone()]),
            Err(DraftError::DraftEffectCardNotInPool {
                card_instance_id: head[0].clone(),
            }),
            "{kind:?}"
        );
    }
}

fn sealed_fifo_config() -> DraftConfig {
    let mut cfg = fifo_config_with(
        DraftKind::Sealed,
        DraftSelectionPolicy::default(),
        DraftClockPolicy::default(),
    );
    cfg.pack_count = DraftKind::Sealed.procedure().packs_per_player;
    cfg
}

fn create_fifo(cfg: DraftConfig, seats: u8) -> Result<DraftSession, DraftError> {
    DraftSession::new_fifo(
        cfg,
        human_seats(seats),
        "same-code".into(),
        DraftSessionId("incarnation-one".into()),
    )
}

fn with_pod_size(mut cfg: DraftConfig, pod_size: u8) -> DraftConfig {
    cfg.pod_size = pod_size;
    cfg
}

#[test]
fn fifo_creation_pod_refusals_use_start_draft_errors() {
    let commander = config(DraftKind::CommanderDraft, 6, 0, 0, 0);
    assert!(create_fifo(with_pod_size(commander.clone(), 3), 3).is_ok());
    assert_eq!(
        create_fifo(with_pod_size(commander, 2), 2),
        Err(DraftError::PodBelowMinimumSize {
            kind: DraftKind::CommanderDraft,
            required: 3,
            actual: 2,
        })
    );

    let mut elimination = config(DraftKind::Premier, 6, 0, 0, 0);
    elimination.tournament_format = TournamentFormat::SingleElimination;
    assert!(create_fifo(with_pod_size(elimination.clone(), 8), 8).is_ok());
    assert_eq!(
        create_fifo(elimination, 4),
        Err(DraftError::UnsupportedTournamentSize {
            format: TournamentFormat::SingleElimination,
            required: 8,
            actual: 4,
        })
    );

    assert!(create_fifo(with_pod_size(cube_config(DraftKind::Quick), 1), 1).is_ok());
    assert_eq!(
        create_fifo(with_pod_size(cube_config(DraftKind::Premier), 1), 1),
        Err(DraftError::PodBelowMinimumSize {
            kind: DraftKind::Premier,
            required: 2,
            actual: 1,
        })
    );

    // The shared stack keeps its own refusal; a pod the procedure refuses
    // first reports the seat count.
    let winston = config(DraftKind::Winston, 6, 0, 0, 0);
    assert_eq!(
        create_fifo(with_pod_size(winston.clone(), 2), 2),
        Err(shared_stack_refusal())
    );
    assert_eq!(
        create_fifo(with_pod_size(winston, 5), 5),
        Err(DraftError::UnsupportedTournamentSize {
            format: TournamentFormat::Swiss,
            required: 2,
            actual: 5,
        })
    );
}

#[test]
fn fifo_creation_sealed_refusals_use_start_draft_errors() {
    assert!(create_fifo(sealed_fifo_config(), 4).is_ok());
    let dimensions = DraftError::InvalidSealedConfiguration {
        reason: "sealed requires six packs and a 40-card minimum deck".to_string(),
    };
    let mut cfg = sealed_fifo_config();
    cfg.pack_count = 2;
    assert_eq!(create_fifo(cfg, 4), Err(dimensions.clone()));
    let mut cfg = sealed_fifo_config();
    cfg.min_deck_size = 41;
    assert_eq!(create_fifo(cfg, 4), Err(dimensions.clone()));
    let mut cfg = sealed_fifo_config();
    cfg.source = DraftSource::Cube {
        id: "TST".into(),
        name: "TST".into(),
    };
    assert_eq!(
        create_fifo(cfg, 4),
        Err(DraftError::SealedRequiresSetSource)
    );

    // The FIFO-only policy check follows the shared admission.
    let mut cfg = sealed_fifo_config();
    cfg.selection_policy.burns_per_pick = 1;
    assert_eq!(
        create_fifo(cfg.clone(), 4),
        Err(DraftError::InvalidConfiguration {
            reason: "AllAtOnce policy".to_string(),
        })
    );
    cfg.pack_count = 2;
    assert_eq!(create_fifo(cfg, 4), Err(dimensions));
}

#[test]
fn fifo_snapshot_pod_refusals_use_snapshot_errors() {
    let source = Source {
        size: 6,
        effect_seat: None,
    };
    let mut session = lobby(config(DraftKind::Premier, 6, 0, 0, 0));
    assert_eq!(session.validate_persisted_snapshot(), Ok(()));
    let mut started = session.clone();
    apply_at(&mut started, DraftAction::StartDraft, Some(&source), 1000).unwrap();
    assert_eq!(started.status, Drafting);

    session.config.tournament_format = TournamentFormat::SingleElimination;
    // The refused snapshot passes FIFO's own seat-count check; only the
    // procedure's pod admission refuses it.
    assert_eq!(session.seats.len(), usize::from(session.config.pod_size));
    let pod = DraftError::InvalidSnapshot {
        reason: "pod size does not satisfy the draft procedure".to_string(),
    };
    assert_eq!(session.validate_persisted_snapshot(), Err(pod.clone()));
    // StartDraft revalidates the stored session first, so it reports the
    // snapshot error and changes nothing.
    let mut refused = session.clone();
    assert_eq!(
        apply_at(&mut refused, DraftAction::StartDraft, Some(&source), 1000),
        Err(pod)
    );
    assert_eq!(refused, session);
}

#[test]
fn fifo_snapshot_sealed_refusals_use_snapshot_errors() {
    let source = Source {
        size: 6,
        effect_seat: None,
    };
    let session = lobby(sealed_fifo_config());
    assert_eq!(session.validate_persisted_snapshot(), Ok(()));
    let mut started = session.clone();
    apply_at(&mut started, DraftAction::StartDraft, Some(&source), 1000).unwrap();
    assert_eq!(started.status, Deckbuilding);

    let dimensions = DraftError::InvalidSnapshot {
        reason: "sealed requires six packs and a 40-card minimum deck".to_string(),
    };
    let mut deck_size = session.clone();
    deck_size.config.min_deck_size = 41;
    assert_eq!(
        deck_size.validate_persisted_snapshot(),
        Err(dimensions.clone())
    );
    let mut packs = session.clone();
    packs.config.pack_count = 2;
    assert_eq!(packs.validate_persisted_snapshot(), Err(dimensions.clone()));
    let mut cube = session.clone();
    cube.config.source = DraftSource::Cube {
        id: "TST".into(),
        name: "TST".into(),
    };
    assert_eq!(
        cube.validate_persisted_snapshot(),
        Err(DraftError::SealedRequiresSetSource)
    );
    // StartDraft revalidates the stored session first, so it reports the
    // snapshot error and changes nothing.
    for (stored, error) in [
        (deck_size, dimensions.clone()),
        (packs, dimensions),
        (cube, DraftError::SealedRequiresSetSource),
    ] {
        let mut refused = stored.clone();
        assert_eq!(
            apply_at(&mut refused, DraftAction::StartDraft, Some(&source), 1000),
            Err(error)
        );
        assert_eq!(refused, stored);
    }
}

#[test]
fn fifo_sealed_snapshot_source_label_keeps_its_configuration_error() {
    let source = Source {
        size: 6,
        effect_seat: None,
    };
    let session = lobby(sealed_fifo_config());
    // Reach: the same snapshot with matching labels validates.
    assert_eq!(session.validate_persisted_snapshot(), Ok(()));
    // FIFO's own source-label check runs before the Sealed admission in the
    // snapshot context too, so a source naming another set keeps its error.
    let mut relabelled = session.clone();
    relabelled.config.source = DraftSource::single_set("OTHER");
    let mismatch = DraftError::InvalidConfiguration {
        reason: "source code mismatch".to_string(),
    };
    assert_eq!(
        relabelled.validate_persisted_snapshot(),
        Err(mismatch.clone())
    );
    let mut refused = relabelled.clone();
    assert_eq!(
        apply_at(&mut refused, DraftAction::StartDraft, Some(&source), 1000),
        Err(mismatch)
    );
    assert_eq!(refused, relabelled);
}

#[test]
fn fifo_own_admission_checks_keep_their_order() {
    assert!(create_fifo(sealed_fifo_config(), 4).is_ok());
    let invalid = |reason: &str| DraftError::InvalidConfiguration {
        reason: reason.to_string(),
    };

    let mismatched = with_pod_size(config(DraftKind::CommanderDraft, 6, 0, 0, 0), 3);
    assert_eq!(
        create_fifo(mismatched, 2),
        Err(invalid("pod size mismatch")),
        "the seat count must equal pod_size before the seat-count admission"
    );

    let mut cfg = sealed_fifo_config();
    cfg.pack_count = 0;
    assert_eq!(
        create_fifo(cfg, 4),
        Err(invalid("invalid pack dimensions")),
        "pack dimensions before the Sealed admission"
    );

    let mut cfg = sealed_fifo_config();
    cfg.source = DraftSource::single_set("OTHER");
    assert_eq!(
        create_fifo(cfg, 4),
        Err(invalid("source code mismatch")),
        "the source label before the Sealed admission"
    );

    let mut cfg = sealed_fifo_config();
    cfg.pack_count = 2;
    cfg.source = DraftSource::Set {
        layout: SetLayout::Chaos {
            candidate_codes: vec!["TST".into()],
            assignments: vec![vec!["TST".into(); 2]; 3],
        },
    };
    assert_eq!(
        create_fifo(cfg, 4),
        Err(DraftError::InvalidPackSequence {
            reason: "Chaos assignments must contain 4 seats, got 3".to_string(),
        }),
        "the source layout before the Sealed admission"
    );

    let mut cfg = sealed_fifo_config();
    cfg.pack_count = 2;
    cfg.clock_policy.pick = PickClockPolicy::Fixed { duration_ms: 0 };
    assert_eq!(
        create_fifo(cfg, 4),
        Err(DraftError::InvalidClockBounds),
        "the clock policy before the Sealed admission"
    );

    let mut relabelled = lobby(sealed_fifo_config());
    relabelled.set_code = "OTHER".into();
    assert_eq!(
        relabelled.validate_persisted_snapshot(),
        Err(snapshot_error("session/config mismatch")),
        "the session label is compared after the configuration checks"
    );

    // A legacy session is validated as a legacy snapshot before any FIFO
    // check, and a legacy Sealed session is returned as it is.
    let fixture_source = crate::pack_source::FixturePackSource {
        set_code: "TST".into(),
        cards_per_pack: 6,
    };
    let upgraded = upgrade_legacy_snapshot(
        &legacy_fixture(config(DraftKind::Premier, 6, 0, 0, 0), &fixture_source),
        migration_context(),
    )
    .unwrap();
    assert_eq!(upgraded.config.flow, DraftFlow::FifoAsync);
    let legacy_lobby = |mut cfg: DraftConfig| {
        cfg.flow = DraftFlow::LegacyBarrier;
        cfg.clock_policy = DraftClockPolicy::default();
        let seats = human_seats(cfg.pod_size);
        DraftSession::new(cfg, seats, "legacy-admission".into())
    };
    let mut elimination = config(DraftKind::Premier, 6, 0, 0, 0);
    elimination.tournament_format = TournamentFormat::SingleElimination;
    assert_eq!(
        upgrade_legacy_snapshot(&legacy_lobby(elimination), migration_context()),
        Err(DraftError::InvalidSealedSnapshot {
            reason: "pod size does not satisfy the draft procedure".to_string(),
        }),
        "legacy validation before the upgrade's FIFO check"
    );
    let mut packs = sealed_fifo_config();
    packs.pack_count = 3;
    assert_eq!(
        upgrade_legacy_snapshot(&legacy_lobby(packs), migration_context()),
        Err(DraftError::InvalidSealedSnapshot {
            reason: "sealed requires six packs and a 40-card minimum deck".to_string(),
        }),
        "legacy validation before the upgrade's FIFO check"
    );
    let mut cube = sealed_fifo_config();
    cube.source = DraftSource::Cube {
        id: "TST".into(),
        name: "TST".into(),
    };
    assert_eq!(
        upgrade_legacy_snapshot(&legacy_lobby(cube), migration_context()),
        Err(DraftError::SealedRequiresSetSource),
        "legacy validation before the upgrade's FIFO check"
    );
    let sealed = legacy_lobby(sealed_fifo_config());
    assert_eq!(
        upgrade_legacy_snapshot(&sealed, migration_context()),
        Ok(sealed.clone()),
        "a legacy Sealed session is returned before the upgrade's FIFO check"
    );
}
