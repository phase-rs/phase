//! GitHub issue #3295 — Scrapwork Mutt's Unearth ability can be activated
//! while the card is on the battlefield.
//!
//! Oracle text:
//!   Scrapwork Mutt
//!   When this creature enters, you may discard a card. If you do, draw a card.
//!   Unearth {1}{R} ({1}{R}: Return this card from your graveyard to the
//!     battlefield. It gains haste. Exile it at the beginning of the next end
//!     step or if it would leave the battlefield. Unearth only as a sorcery.)
//!
//! CR 702.84a: "Unearth [cost]" means "[cost]: Return this card from your
//! graveyard to the battlefield. It gains haste. Exile it at the beginning of
//! the next end step or if it would leave the battlefield. **Activate this
//! ability only as a sorcery.**" The activation site is the **graveyard** —
//! the ability "functions only while the card is in a graveyard." Surfacing
//! Unearth as a castable/activatable action on a permanent already on the
//! battlefield is a CR 602.1a / CR 702.84a violation: a player can't pay
//! {1}{R} to "return this card from your graveyard to the battlefield" when
//! the card is not in any graveyard.
//!
//! The `unearth_ability` builder in `crates/engine/src/database/unearth.rs:88`
//! sets `activation_zone = Some(Zone::Graveyard)` for exactly this reason, and
//! the activation gate at `crates/engine/src/game/casting.rs:11290-11293`
//! filters out activations whose source zone disagrees with `activation_zone`.
//! This test is the discriminating runtime guard: with a Scrapwork-Mutt-shape
//! card on the battlefield (zone = Battlefield), no `GameAction::ActivateAbility`
//! targeting its Unearth ability should appear in `legal_actions`.

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::mana::ManaColor;
use engine::types::zones::Zone;

const SCRAPWORK_MUTT_ORACLE: &str = "When this creature enters, you may \
    discard a card. If you do, draw a card.\nUnearth {1}{R}";

#[test]
fn scrapwork_mutt_unearth_is_not_activatable_from_battlefield() {
    let mut scenario = GameScenario::new();

    // Place a Scrapwork-Mutt-shape card on P0's battlefield. The card text
    // is the fully parsed Oracle text including the Unearth keyword line;
    // the synthesizer attaches the Unearth ability with
    // `activation_zone = Some(Zone::Graveyard)` automatically.
    let mutt = scenario
        .add_creature_from_oracle(P0, "Scrapwork Mutt", 3, 1, SCRAPWORK_MUTT_ORACLE)
        .id();

    // Give P0 a mountain so any cost shortage cannot be the reason an
    // Unearth offer is *not* surfaced — without this guard, a buggy
    // implementation that silently failed on the cost check would look
    // like a passing test.
    scenario.add_basic_land(P0, ManaColor::Red);

    let runner = scenario.build();
    let state = runner.state();

    // Sanity: the Mutt is on the battlefield, not the graveyard.
    assert_eq!(
        state.objects.get(&mutt).map(|o| o.zone),
        Some(Zone::Battlefield),
        "precondition: Scrapwork Mutt must be on the battlefield"
    );

    // CR 702.84a + CR 602.1a: enumerate every legal action the active
    // player could take. The discriminator: no `ActivateAbility` whose
    // `source_id == mutt` should appear, because Unearth's activation zone
    // is the graveyard, not the battlefield.
    let actions = legal_actions(state);
    let unearth_from_battlefield: Vec<&GameAction> = actions
        .iter()
        .filter(|a| match a {
            GameAction::ActivateAbility { source_id, .. } => *source_id == mutt,
            _ => false,
        })
        .collect();

    assert!(
        unearth_from_battlefield.is_empty(),
        "issue #3295: Scrapwork Mutt's Unearth ability must NOT be activatable \
         while the card is on the battlefield (CR 702.84a: 'functions only \
         while the card is in a graveyard'). Offered actions: {:#?}",
        unearth_from_battlefield
    );
}
