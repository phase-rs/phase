//! Hand-authored combo-line registry. The first real line is Heliod,
//! Sun-Crowned + Walking Ballista — both cards parse cleanly today and form
//! a self-contained two-card lethal-damage loop. Additional cEDH lines
//! (Thoracle/Consult, Isochron/Reversal, Underworld Breach, Food Chain,
//! Dockside Extortionist storm lines, ...) land in follow-up phases.
//!
//! ## Heliod + Walking Ballista (CR 727 unbounded loop closing on lethal)
//!
//! Setup: AI controls Heliod, Sun-Crowned and Walking Ballista with at least
//! one +1/+1 counter on it. AI has access to `{1}{W}` once.
//!
//! Loop:
//!   1. Activate Heliod {1}{W} → target Walking Ballista. Ballista has
//!      lifelink until end of turn.
//!   2. Activate Walking Ballista's `Remove a +1/+1 counter: deal 1 damage`
//!      ability targeting an opponent.
//!   3. Lifelink trigger gains the AI 1 life (CR 702.15).
//!   4. Heliod's life-gain trigger puts a +1/+1 counter on Ballista
//!      (CR 603 triggered ability).
//!   5. Counter cap is restored. Go to step 2 until each opponent is at 0
//!      (CR 104.3b).
//!
//! Parser coverage verified via
//! `jq '."heliod, sun-crowned"' client/public/card-data.json` and
//! `jq '."walking ballista"' client/public/card-data.json`. Heliod's
//! activated ability sits at `abilities[0]` (the only activated ability —
//! the indestructible keyword and the devotion-based "isn't a creature"
//! static live elsewhere). Walking Ballista's damage ability sits at
//! `abilities[1]` (`abilities[0]` is the unrelated `{4}: put a +1/+1`
//! growth ability).

use engine::types::game_state::GameState;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::player::PlayerId;

use crate::combo::detection::{piece_present, ComboDetector, StructuralComboDetector};
use crate::combo::line::{
    CardPredicate, ComboLine, ComboLineId, ComboPiece, ComboReachability, ComboStep, WinKind,
};
use engine::types::identifiers::ObjectId;

pub struct ComboRegistry {
    lines: Vec<ComboLine>,
    detector: Box<dyn ComboDetector>,
}

impl Default for ComboRegistry {
    fn default() -> Self {
        Self {
            lines: vec![
                heliod_ballista_line(),
                thoracle_consultation_line(),
                kiki_felidar_line(),
            ],
            detector: Box::new(StructuralComboDetector),
        }
    }
}

impl ComboRegistry {
    /// Returns all combo lines that are reachable (this turn or next turn) for
    /// the given AI player. Lines that are `NotReachable` are filtered out.
    pub fn reachable_lines(
        &self,
        state: &GameState,
        ai: PlayerId,
    ) -> Vec<(ComboLineId, ComboReachability)> {
        self.lines
            .iter()
            .map(|line| (line.id, self.detector.assess(state, line, ai)))
            .filter(|(_, r)| !matches!(r, ComboReachability::NotReachable))
            .collect()
    }

    pub fn lines(&self) -> &[ComboLine] {
        &self.lines
    }

    /// Returns the canonical names of cards that, if added to the AI's hand
    /// or battlefield (depending on the piece's required zone), would complete
    /// a registered combo line. A piece is "missing" when its zone is empty of
    /// any object matching the predicate; the line is "near-reachable" when
    /// all OTHER pieces are already in their required zones.
    ///
    /// Used by the tutor target scorer: cards in this set should receive a
    /// dominant boost so the AI fetches the exact piece that closes a combo,
    /// rather than picking the highest-EV generic creature.
    pub fn missing_pieces_for_near_reachable_lines(
        &self,
        state: &GameState,
        ai: PlayerId,
    ) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = Vec::new();
        for line in &self.lines {
            let (present, missing): (Vec<_>, Vec<_>) = line
                .pieces
                .iter()
                .partition(|piece| piece_present(piece, state, ai));
            // Near-reachable: exactly one piece missing, all others assembled.
            // Lines with zero missing pieces are *already* reachable — the
            // tutor isn't needed. Lines with two-or-more missing pieces are
            // too far away to be worth a tutor cycle.
            if missing.len() != 1 {
                continue;
            }
            // Single-missing pieces with `InLibrary` predicate represent the
            // expected tutor target — capture them. Other zones (InHand,
            // OnBattlefield) also count: e.g., a tutor that grabs into hand
            // closes the gap for an InHand piece too.
            let _ = present;
            if let Some(name) = match &missing[0] {
                ComboPiece::InHand(CardPredicate::NameEquals(n))
                | ComboPiece::OnBattlefield(CardPredicate::NameEquals(n))
                | ComboPiece::InGraveyard(CardPredicate::NameEquals(n))
                | ComboPiece::InLibrary(CardPredicate::NameEquals(n)) => Some(*n),
            } {
                if !out.contains(&name) {
                    out.push(name);
                }
            }
        }
        out
    }

    /// Returns every registered line whose `ComboPiece::InHand` components
    /// are *all* present in the provided hand. Lines with no `InHand` pieces
    /// (i.e., combos that activate entirely on the battlefield) are excluded
    /// because there is nothing to verify against the hand for them.
    ///
    /// This is the right primitive for the mulligan layer: pre-game, the
    /// battlefield is empty and mana hasn't been spent, so the full
    /// `reachable_lines` check would always fail for in-hand combos that
    /// require multiple mana sources. The hand-only check answers the
    /// mulligan-relevant question — "do I have a winning combo in my
    /// opening hand?" — without conflating it with mid-game mana availability.
    pub fn lines_with_pieces_in_hand(
        &self,
        hand: &[ObjectId],
        state: &GameState,
    ) -> Vec<ComboLineId> {
        self.lines
            .iter()
            .filter(|line| {
                let in_hand_predicates: Vec<&CardPredicate> = line
                    .pieces
                    .iter()
                    .filter_map(|p| match p {
                        ComboPiece::InHand(pred) => Some(pred),
                        _ => None,
                    })
                    .collect();
                !in_hand_predicates.is_empty()
                    && in_hand_predicates.iter().all(|pred| {
                        hand.iter().any(|&id| {
                            state.objects.get(&id).is_some_and(|obj| match pred {
                                CardPredicate::NameEquals(name) => obj.name == *name,
                            })
                        })
                    })
            })
            .map(|line| line.id)
            .collect()
    }
}

/// Heliod, Sun-Crowned + Walking Ballista. See module docs for the loop
/// rationale and CR references. Card name strings must match the canonical
/// printed names attached to game objects (set from MTGJSON `face_name`/
/// `name` at card load) — verified via card-data lookup keys above.
fn heliod_ballista_line() -> ComboLine {
    ComboLine {
        id: ComboLineId(0),
        name: "Heliod, Sun-Crowned + Walking Ballista",
        pieces: vec![
            ComboPiece::OnBattlefield(CardPredicate::NameEquals("Heliod, Sun-Crowned")),
            ComboPiece::OnBattlefield(CardPredicate::NameEquals("Walking Ballista")),
        ],
        // Cost to start the loop: activate Heliod's {1}{W} once. Ballista's
        // damage ability pays via counter removal, not mana, so the per-loop
        // marginal mana cost is zero.
        mana_cost: ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 1,
        },
        action_sequence: vec![
            ComboStep::Activate {
                predicate: CardPredicate::NameEquals("Heliod, Sun-Crowned"),
                ability_index: 0,
            },
            ComboStep::Activate {
                predicate: CardPredicate::NameEquals("Walking Ballista"),
                ability_index: 1,
            },
        ],
        win_kind: WinKind::InfiniteLoop,
    }
}

/// Thassa's Oracle + Demonic Consultation (CR 104.2a explicit "wins the
/// game" via Thoracle's ETB-conditional `WinTheGame` effect).
///
/// Setup: AI has both cards in hand and `{1}{U}{U}{B}` available.
///
/// Play sequence (the engine handles stack ordering; the policy only needs
/// to recognize the two casts):
///   1. Cast Thassa's Oracle. Spell goes on the stack but hasn't resolved
///      yet, so its ETB trigger has not fired.
///   2. In response, cast Demonic Consultation naming a card the library
///      doesn't contain. CR 701.17 / oracle-parsed effects: `ExileTop` 6
///      then `RevealUntil(HasChosenName)` with `kept_destination: Hand` and
///      `rest_destination: Exile`. With no matching card, the entire
///      library is exiled to zero.
///   3. Consultation resolves first (top of stack) — library is now empty.
///   4. Thoracle resolves: ETB trigger fires (CR 603), `Dig X` with
///      `X = devotion to blue`, then the conditional `WinTheGame` arm fires
///      because `devotion >= ZoneCardCount(library) = 0`.
///
/// Parser coverage verified via
/// `jq '."thassa'\''s oracle"' client/public/card-data.json` and
/// `jq '."demonic consultation"' client/public/card-data.json`. The
/// win-by-ETB chain (`Dig` → `PutAtLibraryPosition` → conditional
/// `WinTheGame`) is fully typed in the parsed JSON.
fn thoracle_consultation_line() -> ComboLine {
    ComboLine {
        id: ComboLineId(1),
        name: "Thassa's Oracle + Demonic Consultation",
        pieces: vec![
            ComboPiece::InHand(CardPredicate::NameEquals("Thassa's Oracle")),
            ComboPiece::InHand(CardPredicate::NameEquals("Demonic Consultation")),
        ],
        // {1}{U}{U}{B} — both spells must be castable in the same turn so
        // Consultation can resolve before Thoracle's ETB.
        mana_cost: ManaCost::Cost {
            shards: vec![
                ManaCostShard::Blue,
                ManaCostShard::Blue,
                ManaCostShard::Black,
            ],
            generic: 1,
        },
        action_sequence: vec![
            ComboStep::Cast {
                predicate: CardPredicate::NameEquals("Thassa's Oracle"),
            },
            ComboStep::Cast {
                predicate: CardPredicate::NameEquals("Demonic Consultation"),
            },
        ],
        win_kind: WinKind::ImmediateLoss,
    }
}

/// Kiki-Jiki, Mirror Breaker + Felidar Guardian (CR 727 unbounded creature
/// generation closing on lethal combat damage).
///
/// Setup: AI controls both. Kiki must have summoning sickness cleared (the
/// engine's legal-actions layer handles that; the combo line records only
/// the structural pieces).
///
/// Loop:
///   1. Activate Kiki-Jiki `{T}: Create a token that's a copy of another
///      target nonlegendary creature you control, except it has haste...`
///      Target Felidar Guardian. A hasty Felidar token enters.
///   2. The token Felidar's ETB trigger fires (CR 603, parsed as
///      `ChangeZone` to Exile → `ChangeZone` to Battlefield): exile Kiki and
///      return it. Kiki re-enters fresh, untapped.
///   3. Repeat. Each cycle adds another hasty Felidar token to the
///      battlefield, eventually swinging for lethal combat damage
///      (CR 510.1 + CR 104.3b).
///
/// Parser coverage verified via `jq` lookups: Kiki's `CopyTokenOf` ability
/// sits at `abilities[0]` (the Haste keyword lives in `.keywords`, not in
/// the activated-ability array). Felidar's ETB is in `.triggers`, not
/// `.abilities`, so it fires automatically — the combo action sequence has
/// a single explicit player action.
fn kiki_felidar_line() -> ComboLine {
    ComboLine {
        id: ComboLineId(2),
        name: "Kiki-Jiki, Mirror Breaker + Felidar Guardian",
        pieces: vec![
            ComboPiece::OnBattlefield(CardPredicate::NameEquals("Kiki-Jiki, Mirror Breaker")),
            ComboPiece::OnBattlefield(CardPredicate::NameEquals("Felidar Guardian")),
        ],
        // The activation cost is `{T}` only — Kiki's mana cost is irrelevant
        // because it is already on the battlefield. No mana shortfall is
        // possible for the loop itself; the engine's legal-actions layer
        // enforces summoning-sickness / tapped-state constraints.
        mana_cost: ManaCost::NoCost,
        action_sequence: vec![ComboStep::Activate {
            predicate: CardPredicate::NameEquals("Kiki-Jiki, Mirror Breaker"),
            ability_index: 0,
        }],
        win_kind: WinKind::InfiniteLoop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state_returns_no_reachable_lines() {
        let state = GameState::new_two_player(0);
        let reg = ComboRegistry::default();
        assert_eq!(reg.reachable_lines(&state, PlayerId(0)).len(), 0);
    }

    #[test]
    fn registry_exposes_expected_lines() {
        let reg = ComboRegistry::default();
        assert_eq!(reg.lines().len(), 3);
        assert_eq!(reg.lines()[0].id, ComboLineId(0));
        assert_eq!(
            reg.lines()[0].name,
            "Heliod, Sun-Crowned + Walking Ballista"
        );
        assert_eq!(reg.lines()[1].id, ComboLineId(1));
        assert_eq!(
            reg.lines()[1].name,
            "Thassa's Oracle + Demonic Consultation"
        );
        assert_eq!(reg.lines()[2].id, ComboLineId(2));
        assert_eq!(
            reg.lines()[2].name,
            "Kiki-Jiki, Mirror Breaker + Felidar Guardian"
        );
    }

    #[test]
    fn thoracle_combo_reachable_with_both_cards_in_hand_and_mana() {
        use engine::game::zones::create_object;
        use engine::types::card_type::CoreType;
        use engine::types::identifiers::CardId;
        use engine::types::zones::Zone;

        let mut state = GameState::new_two_player(0);
        // Four lands producing UUU+B → pays {1}{U}{U}{B} (two U pips + B pip +
        // the third U covers the generic {1}).
        let subtypes = ["Island", "Island", "Island", "Swamp"];
        for (i, subtype) in subtypes.iter().enumerate() {
            let land_id = create_object(
                &mut state,
                CardId(10 + i as u64),
                PlayerId(0),
                subtype.to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&land_id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push(subtype.to_string());
        }
        create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Thassa's Oracle".to_string(),
            Zone::Hand,
        );
        create_object(
            &mut state,
            CardId(201),
            PlayerId(0),
            "Demonic Consultation".to_string(),
            Zone::Hand,
        );

        let reg = ComboRegistry::default();
        let reachable = reg.reachable_lines(&state, PlayerId(0));
        let thoracle = reachable
            .iter()
            .find(|(id, _)| *id == ComboLineId(1))
            .expect("Thoracle/Consult line must be reachable");
        match &thoracle.1 {
            ComboReachability::ReachableThisTurn {
                missing_mana,
                required_actions,
            } => {
                assert_eq!(*missing_mana, 0);
                assert_eq!(required_actions.len(), 2);
            }
            other => panic!("expected ReachableThisTurn, got {other:?}"),
        }
    }

    /// Discriminating regression: both Thoracle pieces are in hand and the AI
    /// controls four untapped lands — but they produce only W and G, never the
    /// U/U/B that {1}{U}{U}{B} requires. With the color-accurate affordability
    /// primitive the line collapses to NotReachable and is filtered out.
    ///
    /// This MUST fail on pre-fix code: the old count-based check saw 4 mana
    /// sources >= 4 pips and reported `missing_mana: 0` → ReachableThisTurn.
    /// It passes only after delegating to `can_pay_cost_after_auto_tap`.
    #[test]
    fn thoracle_line_not_reachable_with_wrong_color_mana() {
        use engine::game::zones::create_object;
        use engine::types::card_type::CoreType;
        use engine::types::identifiers::CardId;
        use engine::types::zones::Zone;

        let mut state = GameState::new_two_player(0);
        // Four untapped lands, WRONG colors only: W, W, G, G — no U, no B.
        let subtypes = ["Plains", "Plains", "Forest", "Forest"];
        for (i, subtype) in subtypes.iter().enumerate() {
            let land_id = create_object(
                &mut state,
                CardId(10 + i as u64),
                PlayerId(0),
                subtype.to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&land_id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push(subtype.to_string());
        }
        create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Thassa's Oracle".to_string(),
            Zone::Hand,
        );
        create_object(
            &mut state,
            CardId(201),
            PlayerId(0),
            "Demonic Consultation".to_string(),
            Zone::Hand,
        );

        let reg = ComboRegistry::default();
        let reachable = reg.reachable_lines(&state, PlayerId(0));
        // The Thoracle line (ComboLineId(1)) must not appear as reachable this
        // turn — under the collapse semantics it is NotReachable and filtered.
        let thoracle = reachable.iter().find(|(id, _)| *id == ComboLineId(1));
        assert!(
            !matches!(
                thoracle,
                Some((
                    _,
                    ComboReachability::ReachableThisTurn {
                        missing_mana: 0,
                        ..
                    }
                ))
            ),
            "Thoracle line must not be reachable-this-turn with wrong-color mana, got {thoracle:?}"
        );
    }
}
