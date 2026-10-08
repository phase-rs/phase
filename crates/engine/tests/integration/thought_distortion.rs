//! Runtime pipeline coverage — Thought Distortion ({4}{B}{B} sorcery).
//!
//! Verbatim Oracle text (Scryfall oracle_id 5f089ac6-9e92-4ec2-bf46-a0b08d1e2979):
//!   "This spell can't be countered.
//!    Target opponent reveals their hand. Exile all noncreature, nonland cards
//!    from that player's hand and graveyard."
//!
//! This is the discriminating regression for PR #6940's owner-scoped,
//! type-restricted, multi-zone exile: the exile must move ONLY the targeted
//! opponent's noncreature, nonland cards, and only from that player's hand and
//! graveyard. The controls prove all three scopes at once:
//!   - OWNER scope (CR 400.3): the caster's own noncreature/nonland cards, in the
//!     same zones, must NOT move.
//!   - TYPE restriction (CR 205.2a): the target's creature and land cards must
//!     NOT move.
//!   - ZONE union (CR 402.1 + CR 404.1): the target's qualifying cards move from
//!     BOTH hand and graveyard.
//!
//! Before this PR the "and graveyard" leg was an `Unimplemented` no-op, so the
//! graveyard assertions are the revert-failing authority.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const THOUGHT_DISTORTION: &str = "This spell can't be countered.\n\
     Target opponent reveals their hand. Exile all noncreature, nonland cards from that player's hand and graveyard.";

#[test]
fn thought_distortion_exiles_only_the_targeted_opponents_noncreature_nonland_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Caster (P0) casts, targeting opponent P1.
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Thought Distortion", false, THOUGHT_DISTORTION)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 4,
        })
        .id();

    // --- Target opponent (P1): the cards that SHOULD move, plus type controls. ---
    let opp_hand_noncreature = scenario
        .add_spell_to_hand(P1, "Opp Hand Instant", true)
        .id();
    let opp_hand_creature = scenario
        .add_creature_to_hand(P1, "Opp Hand Bear", 2, 2)
        .id();
    let opp_hand_land = scenario.add_land_to_hand(P1, "Opp Hand Forest").id();
    let opp_gy_noncreature = scenario
        .add_spell_to_graveyard(P1, "Opp GY Instant", true)
        .id();
    let opp_gy_creature = scenario
        .add_creature_to_graveyard(P1, "Opp GY Bear", 2, 2)
        .id();
    // A land card in the SAME graveyard: proves the `nonland` restriction is
    // enforced on the graveyard origin too, not just hand (the filter spans both).
    let opp_gy_land = scenario.add_land_to_graveyard(P1, "Opp GY Swamp").id();

    // --- Caster (P0): owner-scope controls — must NOT move. ---
    let my_hand_noncreature = scenario.add_spell_to_hand(P0, "My Hand Instant", true).id();
    let my_gy_noncreature = scenario
        .add_spell_to_graveyard(P0, "My GY Instant", true)
        .id();

    // Fund {4}{B}{B} so the real cost is paid from the battlefield.
    for _ in 0..6 {
        scenario.add_basic_land(P0, ManaColor::Black);
    }

    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_player(P1).resolve();

    // --- The targeted opponent's noncreature, nonland cards, from BOTH zones. ---
    assert_eq!(
        outcome.zone_of(opp_hand_noncreature),
        Zone::Exile,
        "the target opponent's noncreature/nonland HAND card must be exiled"
    );
    assert_eq!(
        outcome.zone_of(opp_gy_noncreature),
        Zone::Exile,
        "the target opponent's noncreature/nonland GRAVEYARD card must be exiled \
         (the revert-failing 'and graveyard' leg)"
    );

    // --- Type controls: the target's creature/land cards stay put. ---
    assert_eq!(
        outcome.zone_of(opp_hand_creature),
        Zone::Hand,
        "a creature card is not noncreature — it must stay in the target's hand"
    );
    assert_eq!(
        outcome.zone_of(opp_hand_land),
        Zone::Hand,
        "a land card is not nonland — it must stay in the target's hand"
    );
    assert_eq!(
        outcome.zone_of(opp_gy_creature),
        Zone::Graveyard,
        "a creature card must stay in the target's graveyard"
    );
    assert_eq!(
        outcome.zone_of(opp_gy_land),
        Zone::Graveyard,
        "a land card is not nonland — it must stay in the target's GRAVEYARD \
         (proves the nonland restriction on the graveyard origin)"
    );

    // --- Owner-scope controls: the CASTER's own qualifying cards never move. ---
    assert_eq!(
        outcome.zone_of(my_hand_noncreature),
        Zone::Hand,
        "the caster's own hand card must not move — the exile is scoped to the target player"
    );
    assert_eq!(
        outcome.zone_of(my_gy_noncreature),
        Zone::Graveyard,
        "the caster's own graveyard card must not move — owner scope (CR 400.3)"
    );

    // The spell resolves to its owner's graveyard (an ordinary sorcery).
    assert_eq!(
        outcome.zone_of(spell),
        Zone::Graveyard,
        "Thought Distortion is an ordinary sorcery — it goes to its owner's graveyard"
    );
}
