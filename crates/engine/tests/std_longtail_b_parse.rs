//! Standard long-tail batch B — per-card 0-`Unimplemented` parse gate for the
//! shipped cards. Each shipped card's full Oracle text must parse with zero
//! residual `Effect::Unimplemented` nodes; reverting the corresponding parser
//! arm reintroduces an `Unimplemented` node and flips the assertion.
//!
//! Deferred cards (Heirloom Epic, Steamcore Scholar, Combustion Man)
//! intentionally retain an honest `Effect::Unimplemented` residual and are NOT
//! asserted 0-unimpl here. (Stolen Uniform now parses 0-unimpl — front half B3
//! plus the last-sentence lose-control delayed trigger both shipped.)

use engine::parser::oracle::parse_oracle_text;

fn assert_zero_unimplemented(
    oracle: &str,
    name: &str,
    keywords: &[&str],
    types: &[&str],
    subtypes: &[&str],
) {
    let kw: Vec<String> = keywords.iter().map(|s| s.to_string()).collect();
    let t: Vec<String> = types.iter().map(|s| s.to_string()).collect();
    let s: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
    let parsed = parse_oracle_text(oracle, name, &kw, &t, &s);
    let dbg = format!("{parsed:#?}");
    assert!(
        !dbg.contains("Unimplemented"),
        "{name}: expected zero Unimplemented nodes, parse was:\n{dbg}"
    );
}

#[test]
fn all_out_assault_zero_unimplemented() {
    assert_zero_unimplemented(
        "Creatures you control get +1/+1 and have deathtouch.\nWhen this enchantment \
         enters, if it's your main phase, there is an additional combat phase after this \
         phase followed by an additional main phase. When you next attack this turn, untap \
         each creature you control.",
        "All-Out Assault",
        &[],
        &["Enchantment"],
        &[],
    );
}

#[test]
fn lightstall_inquisitor_zero_unimplemented() {
    // CR 611.2a + CR 601.2f + CR 614.1c: the compound "each opponent exiles a
    // card from their hand and may play that card for as long as it remains
    // exiled" splits into a player-scoped exile + an `ObjectOwner`
    // `PlayFromExile` grant; the two rider sentences fold into the grant's
    // `cast_cost_raise` / `land_enter_tapped`, leaving no `Unimplemented`.
    assert_zero_unimplemented(
        "Vigilance\nWhen this creature enters, each opponent exiles a card from their hand \
         and may play that card for as long as it remains exiled. Each spell cast this way \
         costs {1} more to cast. Each land played this way enters tapped.",
        "Lightstall Inquisitor",
        &["Vigilance"],
        &["Creature"],
        &["Bird", "Cleric"],
    );
}

#[test]
fn fear_of_burning_alive_zero_unimplemented() {
    assert_zero_unimplemented(
        "When this creature enters, it deals 4 damage to each opponent.\nDelirium — \
         Whenever a source you control deals noncombat damage to an opponent, if there are \
         four or more card types among cards in your graveyard, this creature deals that \
         amount of damage to target creature that player controls.",
        "Fear of Burning Alive",
        &[],
        &["Creature"],
        &[],
    );
}

#[test]
fn fortune_loyal_steed_zero_unimplemented() {
    assert_zero_unimplemented(
        "When Fortune enters, scry 2.\nWhenever Fortune attacks while saddled, at end of \
         combat, exile it and up to one creature that saddled it this turn, then return \
         those cards to the battlefield under their owner's control.\nSaddle 1",
        "Fortune, Loyal Steed",
        &[],
        &["Creature"],
        &["Horse"],
    );
}

#[test]
fn mutable_explorer_zero_unimplemented() {
    // Changeling is supplied as an MTGJSON keyword in production; with it, the
    // whole card (including "create a tapped Mutavault token") parses clean.
    assert_zero_unimplemented(
        "Changeling (This card is every creature type.)\nWhen this creature enters, create \
         a tapped Mutavault token. (It's a land with \"{T}: Add {C}\" and \"{1}: This token \
         becomes a 2/2 creature with all creature types until end of turn. It's still a \
         land.\")",
        "Mutable Explorer",
        &["Changeling"],
        &["Creature"],
        &[],
    );
}

#[test]
fn yshtola_rhul_zero_unimplemented() {
    assert_zero_unimplemented(
        "At the beginning of your end step, exile target creature you control, then return \
         it to the battlefield under its owner's control. Then if it's the first end step of \
         the turn, there is an additional end step after this step.",
        "Y'shtola Rhul",
        &[],
        &["Creature"],
        &[],
    );
}
