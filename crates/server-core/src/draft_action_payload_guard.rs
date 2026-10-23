//! Payload bounds for `DraftAction` bodies on the native WebSocket path.
//!
//! `draft_wire_guard::guard_draft_action` only validates `draft_code`. Oversized
//! pick IDs, submit-deck lists, and match IDs still reach clone-heavy draft
//! reducers unless bounded here.

use std::collections::HashSet;

use draft_core::types::{
    DraftAction, MAX_CARDS_PER_PICK, MAX_COMMANDER_DESIGNATIONS, MAX_SHARED_STACK_PILES,
};
use lobby_broker::inbound_guard::{validate_deck_list, MAX_MAIN_DECK_ENTRIES};
use lobby_broker::validation::{
    validate_required_label, validate_token, MAX_DISPLAY_NAME_LEN, MAX_TOKEN_LEN,
};

/// Validate client-supplied `DraftAction` payload fields before session dispatch.
pub fn guard_draft_action_payload(action: &DraftAction) -> Result<(), String> {
    match action {
        DraftAction::Select {
            prompt_id,
            picked_card_instance_ids,
            burned_card_instance_ids,
            effect_card_instance_ids,
            ..
        } => {
            use draft_core::types::{
                select_pick_count_is_admitted, valid_draft_id, MAX_DRAFT_JSON_INTEGER,
            };
            // An action alone can state only a range of pick counts. The exact
            // count for the session's kind and head pack is
            // `draft_core::pick_pass::selection_requirements`'s.
            if !select_pick_count_is_admitted(
                picked_card_instance_ids.len(),
                effect_card_instance_ids.len(),
            ) {
                return Err("Select has invalid pick count".to_string());
            }
            if burned_card_instance_ids.len() > usize::from(u8::MAX) {
                return Err("Select has oversized burn list".to_string());
            }
            if !valid_draft_id(&prompt_id.session_id.0)
                || prompt_id.generation == 0
                || prompt_id.generation > MAX_DRAFT_JSON_INTEGER
            {
                return Err("Select has invalid prompt identity".to_string());
            }
            let mut ids = HashSet::new();
            for id in picked_card_instance_ids
                .iter()
                .chain(burned_card_instance_ids)
                .chain(effect_card_instance_ids)
            {
                if !valid_draft_id(id) {
                    return Err("Select has invalid card ID".to_string());
                }
                if !ids.insert(id) {
                    return Err("Select requires distinct disjoint IDs".to_string());
                }
            }
        }
        DraftAction::Control { .. } | DraftAction::AdvanceClock => {}
        // CR 903.13b: a pick step carries `cards_per_pick` ids. This function
        // receives only the action and can never consult the session, so the
        // EXACT count is checked in `draft_core::pick_pass::apply_pick_inner`;
        // what is bounded here is what an action alone can state.
        DraftAction::Pick {
            card_instance_ids, ..
        } => {
            if card_instance_ids.is_empty() {
                return Err("Pick requires at least one card ID".to_string());
            }
            if card_instance_ids.len() > MAX_CARDS_PER_PICK {
                return Err(format!(
                    "Pick accepts at most {MAX_CARDS_PER_PICK} card IDs"
                ));
            }
            // Pairwise-distinct over the WHOLE Vec. NOT the index form used by
            // `PickWithDraftEffect` below: that is sound only under its
            // `len() != 2` early return, and a `Pick` is legitimately length 1.
            let mut seen = HashSet::with_capacity(card_instance_ids.len());
            for card_instance_id in card_instance_ids {
                if !seen.insert(card_instance_id) {
                    return Err("Pick requires distinct card IDs".to_string());
                }
                validate_token("Pick.card_instance_id", card_instance_id, MAX_TOKEN_LEN)?;
            }
        }
        DraftAction::PickWithDraftEffect {
            effect_card_instance_id,
            card_instance_ids,
            ..
        } => {
            validate_token(
                "PickWithDraftEffect.effect_card_instance_id",
                effect_card_instance_id,
                MAX_TOKEN_LEN,
            )?;
            if card_instance_ids.len() != 2 {
                return Err("PickWithDraftEffect requires exactly two card IDs".to_string());
            }
            if card_instance_ids[0] == card_instance_ids[1] {
                return Err("PickWithDraftEffect requires distinct card IDs".to_string());
            }
            for card_instance_id in card_instance_ids {
                validate_token(
                    "PickWithDraftEffect.card_instance_id",
                    card_instance_id,
                    MAX_TOKEN_LEN,
                )?;
            }
        }
        DraftAction::SubmitDeck {
            main_deck,
            commanders,
            ..
        } => {
            validate_deck_list("SubmitDeck.main_deck", main_deck, MAX_MAIN_DECK_ENTRIES)?;
            // CR 702.124g: a client-controlled, otherwise unbounded
            // `Vec<String>` reaching the clone-heavy reducer is exactly the
            // hole this module exists to close. Bounded by draft-core's
            // CR-derived constant, imported rather than duplicated -- the same
            // shape `MAX_CARDS_PER_PICK` already uses above.
            //
            // NOT `lobby_broker::inbound_guard::MAX_COMMANDER_ENTRIES`: that is
            // a larger TRANSPORT bound on the lobby's own `DeckData.commander`
            // list, and reaching for it here would silently admit a third
            // commander.
            validate_deck_list(
                "SubmitDeck.commanders",
                commanders,
                MAX_COMMANDER_DESIGNATIONS,
            )?;
        }
        DraftAction::ReportMatchResult { match_id, .. } => {
            validate_token("ReportMatchResult.match_id", match_id, MAX_TOKEN_LEN)?;
        }
        DraftAction::ReplaceSeatWithBot { name, .. } => {
            if let Some(n) = name {
                if !n.trim().is_empty() {
                    validate_required_label("ReplaceSeatWithBot.name", n, MAX_DISPLAY_NAME_LEN)?;
                }
            }
        }
        // A shared-stack decision names a pile index. This function receives
        // only the action and can never consult the session, so the EXACT check
        // -- is this the ACTIVE pile for this seat? -- is
        // `draft_core::shared_stack::refusal_for`'s; what is bounded here is
        // what an action alone can state. Named rather than left to the
        // compiler because falling into the no-op group below compiles and
        // leaves the index unbounded at the wire.
        DraftAction::SharedStackDecision { pile, .. } => {
            if usize::from(*pile) >= MAX_SHARED_STACK_PILES {
                return Err(format!(
                    "SharedStackDecision.pile must be below {MAX_SHARED_STACK_PILES}"
                ));
            }
        }
        DraftAction::StartDraft
        | DraftAction::AdvanceRound
        | DraftAction::GeneratePairings
        | DraftAction::SetSeatConnected { .. } => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lobby_broker::inbound_guard::MAX_MAIN_DECK_ENTRIES;

    /// V21. The bound is `MAX_SHARED_STACK_PILES`, which draft-core DERIVES
    /// from the procedure table -- not a literal repeated here.
    #[test]
    fn shared_stack_decision_rejects_out_of_range_pile() {
        use draft_core::types::SharedStackPileDecision;

        let decision = |pile: u8| DraftAction::SharedStackDecision {
            seat: 0,
            pile,
            decision: SharedStackPileDecision::Take,
        };
        let error = guard_draft_action_payload(&decision(MAX_SHARED_STACK_PILES as u8))
            .expect_err("a pile at the bound is out of range");
        assert!(error.contains("SharedStackDecision.pile"), "{error}");
        assert!(guard_draft_action_payload(&decision(u8::MAX)).is_err());

        // Paired positive: the last legal index is ACCEPTED, so the rejection
        // is not a blanket refusal of the variant.
        guard_draft_action_payload(&decision(MAX_SHARED_STACK_PILES as u8 - 1))
            .expect("the last in-range pile is accepted");
        guard_draft_action_payload(&decision(0)).expect("pile 0 is accepted");
        // Both decisions pass the payload bound; legality is not this layer's.
        for value in SharedStackPileDecision::ALL {
            guard_draft_action_payload(&DraftAction::SharedStackDecision {
                seat: 0,
                pile: 0,
                decision: value,
            })
            .expect("the payload guard bounds shape, not legality");
        }
    }

    #[test]
    fn dormant_select_payload_bounds_preserve_the_core_identity_contract() {
        use draft_core::types::{
            DraftPromptId, DraftSessionId, MAX_DRAFT_ID_BYTES, MAX_DRAFT_JSON_INTEGER,
        };
        assert_eq!(MAX_DRAFT_ID_BYTES, MAX_TOKEN_LEN);
        let valid = DraftAction::Select {
            seat: 9,
            prompt_id: DraftPromptId {
                session_id: DraftSessionId("s".repeat(MAX_DRAFT_ID_BYTES)),
                seat: 2,
                generation: MAX_DRAFT_JSON_INTEGER,
            },
            picked_card_instance_ids: vec!["pick".into()],
            burned_card_instance_ids: (0..255).map(|n| format!("burn-{n}")).collect(),
            effect_card_instance_ids: Vec::new(),
        };
        assert_eq!(guard_draft_action_payload(&valid), Ok(()));
        let mut cases = Vec::new();
        for id in [
            String::new(),
            "x".repeat(MAX_DRAFT_ID_BYTES + 1),
            "bad\nidentity".into(),
        ] {
            let mut bad = valid.clone();
            if let DraftAction::Select {
                picked_card_instance_ids,
                ..
            } = &mut bad
            {
                picked_card_instance_ids[0] = id;
            }
            cases.push((bad, "Select has invalid card ID"));
        }
        for generation in [0, MAX_DRAFT_JSON_INTEGER + 1] {
            let mut bad = valid.clone();
            if let DraftAction::Select { prompt_id, .. } = &mut bad {
                prompt_id.generation = generation;
            }
            cases.push((bad, "Select has invalid prompt identity"));
        }
        let mut bad = valid.clone();
        if let DraftAction::Select {
            burned_card_instance_ids,
            ..
        } = &mut bad
        {
            burned_card_instance_ids.push("extra".into());
        }
        cases.push((bad, "Select has oversized burn list"));
        let mut bad = valid.clone();
        if let DraftAction::Select {
            burned_card_instance_ids,
            ..
        } = &mut bad
        {
            burned_card_instance_ids[0] = "pick".into();
        }
        cases.push((bad, "Select requires distinct disjoint IDs"));
        let mut bad = valid.clone();
        if let DraftAction::Select {
            burned_card_instance_ids,
            ..
        } = &mut bad
        {
            burned_card_instance_ids[1] = "burn-0".into();
        }
        cases.push((bad, "Select requires distinct disjoint IDs"));
        for count in [0, 3] {
            let mut bad = valid.clone();
            if let DraftAction::Select {
                picked_card_instance_ids,
                ..
            } = &mut bad
            {
                *picked_card_instance_ids = (0..count).map(|n| format!("pick-{n}")).collect();
            }
            cases.push((bad, "Select has invalid pick count"));
        }
        let mut effect = valid.clone();
        if let DraftAction::Select {
            picked_card_instance_ids,
            effect_card_instance_ids,
            ..
        } = &mut effect
        {
            picked_card_instance_ids.push("pick-two".into());
            *effect_card_instance_ids = vec!["effect".into()];
        }
        assert_eq!(guard_draft_action_payload(&effect), Ok(()));
        // A selection that names draft-effect cards takes a pick step plus one
        // card for each of them, whatever their number and the kind's step.
        let with_effects = |effects: usize, picks: usize| {
            let mut action = valid.clone();
            if let DraftAction::Select {
                picked_card_instance_ids,
                effect_card_instance_ids,
                ..
            } = &mut action
            {
                *picked_card_instance_ids = (0..picks).map(|n| format!("pick-{n}")).collect();
                *effect_card_instance_ids = (0..effects).map(|n| format!("effect-{n}")).collect();
            }
            action
        };
        for effects in [1, 2, 9] {
            for pick_step in [1, MAX_CARDS_PER_PICK] {
                let picks = pick_step + effects;
                assert_eq!(
                    guard_draft_action_payload(&with_effects(effects, picks)),
                    Ok(()),
                    "{effects} draft-effect ids with {picks} picks"
                );
            }
            for picks in [effects, effects + MAX_CARDS_PER_PICK + 1] {
                cases.push((
                    with_effects(effects, picks),
                    "Select has invalid pick count",
                ));
            }
        }
        // The largest pack a session can hold bounds the picks.
        let largest_pack = usize::from(u8::MAX);
        assert_eq!(
            guard_draft_action_payload(&with_effects(largest_pack - 1, largest_pack)),
            Ok(())
        );
        cases.push((
            with_effects(largest_pack - 1, largest_pack + 1),
            "Select has invalid pick count",
        ));
        // Draft-effect ids are distinct, and disjoint from the picks and burns.
        for repeated in ["effect-0", "pick-0", "burn-0"] {
            let mut bad = with_effects(2, 3);
            if let DraftAction::Select {
                effect_card_instance_ids,
                ..
            } = &mut bad
            {
                effect_card_instance_ids[1] = repeated.into();
            }
            cases.push((bad, "Select requires distinct disjoint IDs"));
        }
        // A draft-effect selection takes the kind's pick step plus one card,
        // so Commander Draft's takes three.
        let mut three = effect.clone();
        if let DraftAction::Select {
            picked_card_instance_ids,
            ..
        } = &mut three
        {
            picked_card_instance_ids.push("pick-three".into());
        }
        assert_eq!(guard_draft_action_payload(&three), Ok(()));
        let with_picks = |count: usize| {
            let mut action = effect.clone();
            if let DraftAction::Select {
                picked_card_instance_ids,
                ..
            } = &mut action
            {
                *picked_card_instance_ids =
                    (0..count).map(|n| format!("effect-pick-{n}")).collect();
            }
            action
        };
        // One draft-effect card: a pick step of one or two cards plus one.
        for count in [2, 3] {
            assert_eq!(
                guard_draft_action_payload(&with_picks(count)),
                Ok(()),
                "{count} picks with a draft effect"
            );
        }
        for count in [1, 4] {
            cases.push((with_picks(count), "Select has invalid pick count"));
        }
        let mut plain_two = effect.clone();
        if let DraftAction::Select {
            effect_card_instance_ids,
            ..
        } = &mut plain_two
        {
            effect_card_instance_ids.clear();
        }
        assert_eq!(guard_draft_action_payload(&plain_two), Ok(()));
        let mut bad = effect.clone();
        if let DraftAction::Select {
            picked_card_instance_ids,
            ..
        } = &mut bad
        {
            picked_card_instance_ids.pop();
        }
        cases.push((bad, "Select has invalid pick count"));
        let mut bad = effect;
        if let DraftAction::Select {
            effect_card_instance_ids,
            ..
        } = &mut bad
        {
            *effect_card_instance_ids = vec!["pick".into()];
        }
        cases.push((bad, "Select requires distinct disjoint IDs"));
        for (action, reason) in cases {
            assert_eq!(guard_draft_action_payload(&action), Err(reason.into()));
        }
    }

    #[test]
    fn pick_accepts_valid_instance_id() {
        let action = DraftAction::Pick {
            seat: 0,
            card_instance_ids: vec!["card-0".to_string()],
        };
        assert!(guard_draft_action_payload(&action).is_ok());
    }

    #[test]
    fn pick_rejects_oversized_instance_id() {
        let action = DraftAction::Pick {
            seat: 0,
            card_instance_ids: vec!["x".repeat(MAX_TOKEN_LEN + 1)],
        };
        let err = guard_draft_action_payload(&action).unwrap_err();
        assert!(err.contains("card_instance_id"));
    }

    /// CR 903.13b: a Commander Draft pick carries two ids, and the guard must
    /// admit them.
    #[test]
    fn pick_accepts_two_distinct_card_ids() {
        let action = DraftAction::Pick {
            seat: 0,
            card_instance_ids: vec!["card-0".to_string(), "card-1".to_string()],
        };
        assert!(guard_draft_action_payload(&action).is_ok());
    }

    #[test]
    fn pick_rejects_empty_card_ids() {
        let action = DraftAction::Pick {
            seat: 0,
            card_instance_ids: Vec::new(),
        };
        let err = guard_draft_action_payload(&action).unwrap_err();
        assert!(err.contains("at least one"));
    }

    #[test]
    fn pick_rejects_more_than_max_cards_per_pick() {
        let action = DraftAction::Pick {
            seat: 0,
            card_instance_ids: (0..=MAX_CARDS_PER_PICK)
                .map(|i| format!("card-{i}"))
                .collect(),
        };
        let err = guard_draft_action_payload(&action).unwrap_err();
        assert!(err.contains("at most"));
    }

    /// The distinctness check must be pairwise over the whole `Vec`, not the
    /// index form `PickWithDraftEffect` uses — that form is sound only under
    /// its own `len() != 2` precondition and would panic on the length-1
    /// `Pick` that `pick_accepts_valid_instance_id` covers.
    #[test]
    fn pick_rejects_duplicate_card_ids() {
        let action = DraftAction::Pick {
            seat: 0,
            card_instance_ids: vec!["card-0".to_string(), "card-0".to_string()],
        };
        let err = guard_draft_action_payload(&action).unwrap_err();
        assert!(err.contains("distinct"));
    }

    #[test]
    fn draft_effect_pick_accepts_two_distinct_card_ids() {
        let action = DraftAction::PickWithDraftEffect {
            seat: 0,
            effect_card_instance_id: "cogwork-1".to_string(),
            card_instance_ids: vec!["card-1".to_string(), "card-2".to_string()],
        };
        assert!(guard_draft_action_payload(&action).is_ok());
    }

    #[test]
    fn draft_effect_pick_rejects_wrong_card_count() {
        let action = DraftAction::PickWithDraftEffect {
            seat: 0,
            effect_card_instance_id: "cogwork-1".to_string(),
            card_instance_ids: vec!["card-1".to_string()],
        };
        let err = guard_draft_action_payload(&action).unwrap_err();
        assert!(err.contains("exactly two"));
    }

    #[test]
    fn draft_effect_pick_rejects_duplicate_card_ids() {
        let action = DraftAction::PickWithDraftEffect {
            seat: 0,
            effect_card_instance_id: "cogwork-1".to_string(),
            card_instance_ids: vec!["card-1".to_string(), "card-1".to_string()],
        };
        let err = guard_draft_action_payload(&action).unwrap_err();
        assert!(err.contains("distinct"));
    }

    #[test]
    fn draft_effect_pick_rejects_oversized_card_id() {
        let action = DraftAction::PickWithDraftEffect {
            seat: 0,
            effect_card_instance_id: "cogwork-1".to_string(),
            card_instance_ids: vec!["card-1".to_string(), "x".repeat(MAX_TOKEN_LEN + 1)],
        };
        let err = guard_draft_action_payload(&action).unwrap_err();
        assert!(err.contains("card_instance_id"));
    }

    #[test]
    fn submit_deck_rejects_oversized_main() {
        let action = DraftAction::SubmitDeck {
            seat: 0,
            main_deck: vec!["Forest".to_string(); MAX_MAIN_DECK_ENTRIES + 1],
            commanders: Vec::new(),
        };
        let err = guard_draft_action_payload(&action).unwrap_err();
        assert!(err.contains("main_deck"));
    }

    #[test]
    fn submit_deck_rejects_invalid_card_name() {
        let action = DraftAction::SubmitDeck {
            seat: 0,
            main_deck: vec!["Forest\nIsland".to_string()],
            commanders: Vec::new(),
        };
        let err = guard_draft_action_payload(&action).unwrap_err();
        assert!(err.contains("control characters"));
    }

    /// CR 702.124g: the WIRE bound on the designation list.
    ///
    /// Written off `MAX_COMMANDER_DESIGNATIONS`, never off the literal 3, so
    /// the assertion tracks the constant rather than a number that happens to
    /// exceed it today. Keys on the FIELD NAME rather than the full sentence,
    /// so rewording `validate_deck_list`'s `format!` does not break it.
    ///
    /// The bound that must NOT be reached for here is
    /// `lobby_broker::inbound_guard::MAX_COMMANDER_ENTRIES` — a larger
    /// TRANSPORT bound on the lobby's own commander list, already imported into
    /// this file's sibling module. Under it a three-commander payload is
    /// ACCEPTED and this test passes vacuously; the two constants having
    /// different values is exactly what makes this discriminating.
    #[test]
    fn submit_deck_rejects_more_than_max_commander_designations() {
        let action = DraftAction::SubmitDeck {
            seat: 0,
            main_deck: vec!["Forest".to_string()],
            commanders: vec!["Forest".to_string(); MAX_COMMANDER_DESIGNATIONS + 1],
        };
        let err = guard_draft_action_payload(&action).unwrap_err();
        assert!(
            err.contains("SubmitDeck.commanders"),
            "expected the commanders field to be named, got {err}"
        );
    }

    /// Paired positive reach-guard for the bound above: exactly
    /// `MAX_COMMANDER_DESIGNATIONS` names is accepted, so the rejection cannot
    /// pass by `SubmitDeck` being rejected for some unrelated reason.
    #[test]
    fn submit_deck_accepts_max_commander_designations() {
        let action = DraftAction::SubmitDeck {
            seat: 0,
            main_deck: vec!["Forest".to_string()],
            commanders: vec!["Forest".to_string(); MAX_COMMANDER_DESIGNATIONS],
        };
        assert!(guard_draft_action_payload(&action).is_ok());
    }

    #[test]
    fn replace_seat_with_bot_rejects_oversized_name() {
        let action = DraftAction::ReplaceSeatWithBot {
            seat: 0,
            name: Some("x".repeat(MAX_DISPLAY_NAME_LEN + 1)),
        };
        let err = guard_draft_action_payload(&action).unwrap_err();
        assert!(err.contains("ReplaceSeatWithBot.name"));
    }
}
