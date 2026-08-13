use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const OVERENCUMBERED_ORACLE: &str = "Enchant opponent\n\
When this Aura enters, enchanted opponent creates a Clue token, a Food token, and a Junk token.\n\
At the beginning of combat on enchanted opponent's turn, that player may pay {1} for each artifact they control. If they don't, creatures can't attack this combat.";

/// CR 111.2 + CR 303.4b + CR 608.2c: a player Aura's ETB resolves its
/// shared-verb token list for the enchanted opponent, not the Aura's controller.
#[test]
fn issue_5271_overencumbered_etb_gives_every_token_to_enchanted_opponent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let aura = {
        let mut builder = scenario.add_spell_to_hand_from_oracle(
            P0,
            "Overencumbered",
            false,
            OVERENCUMBERED_ORACLE,
        );
        builder
            .as_enchantment()
            .with_subtypes(vec!["Aura", "Curse"])
            .with_mana_cost(ManaCost::zero());
        builder.id()
    };

    let mut runner = scenario.build();
    runner.cast(aura).target_player(P1).resolve();

    for token_name in ["Clue", "Food", "Junk"] {
        let token = runner
            .state()
            .objects
            .values()
            .find(|object| object.name == token_name && object.is_token)
            .unwrap_or_else(|| panic!("Overencumbered must create a {token_name} token"));
        assert_eq!(
            token.owner, P1,
            "{token_name} must be owned by the enchanted opponent"
        );
        assert_eq!(
            token.controller, P1,
            "{token_name} must be controlled by the enchanted opponent"
        );
    }
}
