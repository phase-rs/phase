//! CR 608.2c — the "exiled this way" anaphor keeps its tracked-set pool.
//!
//! A `ChooseFromZone` clause that NAMES its own zone reads that zone
//! (`ZoneChoiceCandidateSource::Direct`). But some clauses name a zone *and*
//! refer back to the set an earlier instruction of the SAME ability put there —
//! "Choose a nonland card **exiled this way**". CR 608.2c: a spell's
//! instructions are followed in the order written, and that second instruction
//! refers to the first one's result, so its printed pool is the chain's tracked
//! set.
//!
//! Deliberately NOT CR 607.2a. That rule links two SEPARATE abilities printed
//! on one object (CR 607.1); Author of Shadows and Plargg and Nassari each
//! carry a single triggered ability whose second sentence refers to its own
//! first sentence.
//!
//! The discriminator is structural, not phrasal: the anaphor lowers to
//! `TargetFilter::ExiledBySource`, so the lowering keeps `Legacy` whenever the
//! filter mentions it and emits `Direct` otherwise. `ExiledBySource` would
//! independently reject cards this source never exiled, so what this preserves
//! is pool PROVENANCE — a pool defined by the instruction rather than
//! re-derived from the zone.
//!
//! This guard exists because the candidate-source change initially flipped
//! these two cards to `Direct` — caught by reading the card-data parse delta,
//! not by any test. Verbatim Oracle text confirmed via Scryfall `cards/named`.

use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, ZoneChoiceCandidateSource};

const AUTHOR_OF_SHADOWS: &str = "When this creature enters, exile all opponents' graveyards. Choose a nonland card exiled this way. You may cast that card for as long as it remains exiled, and you may spend mana as though it were mana of any color to cast that spell.";

const PLARGG_AND_NASSARI: &str = "At the beginning of your upkeep, each player exiles cards from the top of their library until they exile a nonland card. An opponent chooses a nonland card exiled this way. You may cast up to two spells from among the other cards exiled this way without paying their mana costs.";

const DAUTHI_VOIDWALKER: &str = "Shadow (This creature can block or be blocked by only creatures with shadow.)\nIf a card would be put into an opponent's graveyard from anywhere, instead exile it with a void counter on it.\n{T}, Sacrifice this creature: Choose an exiled card an opponent owns with a void counter on it. You may play it this turn without paying its mana cost.";

const REJOIN_THE_FIGHT: &str = "Mill three cards. Then starting with the next opponent in turn order, each opponent chooses a creature card in your graveyard that hasn't been chosen. Return each card chosen this way to the battlefield under your control.";

/// Every `candidate_source` on every `ChooseFromZone` this card parses to.
fn candidate_sources(oracle: &str, name: &str, types: &[&str]) -> Vec<ZoneChoiceCandidateSource> {
    let types: Vec<String> = types.iter().map(|t| (*t).to_string()).collect();
    let parsed = parse_oracle_text(oracle, name, &[], &types, &[]);

    fn walk(effect: &Effect, out: &mut Vec<ZoneChoiceCandidateSource>) {
        if let Effect::ChooseFromZone {
            candidate_source, ..
        } = effect
        {
            out.push(*candidate_source);
        }
    }

    let mut found = Vec::new();
    for ability in &parsed.abilities {
        let mut cursor = Some(ability);
        while let Some(node) = cursor {
            walk(node.effect.as_ref(), &mut found);
            cursor = node.sub_ability.as_deref();
        }
    }
    for trigger in &parsed.triggers {
        let mut cursor = trigger.execute.as_deref();
        while let Some(node) = cursor {
            walk(node.effect.as_ref(), &mut found);
            cursor = node.sub_ability.as_deref();
        }
    }
    found
}

/// CR 608.2c: "exiled this way" refers to an earlier instruction of the same
/// ability — pool stays the tracked set.
#[test]
fn exiled_this_way_anaphor_keeps_the_tracked_set_pool() {
    for (name, oracle, types) in [
        ("Author of Shadows", AUTHOR_OF_SHADOWS, &["Creature"][..]),
        ("Plargg and Nassari", PLARGG_AND_NASSARI, &["Creature"][..]),
    ] {
        let sources = candidate_sources(oracle, name, types);
        // Positive reach guard: the clause must actually have produced a
        // ChooseFromZone, or the assertion below is vacuous.
        assert!(
            !sources.is_empty(),
            "{name} must lower to at least one ChooseFromZone"
        );
        assert!(
            sources
                .iter()
                .all(|s| matches!(s, ZoneChoiceCandidateSource::Legacy)),
            "{name} chooses from cards exiled THIS WAY — the result of an \
             earlier instruction of this same ability (CR 608.2c), so the pool \
             is that instruction's set, not one re-derived from the zone; got {sources:?}"
        );
    }
}

/// The sibling that genuinely names a zone keeps the direct scan, so the guard
/// above is a discriminator and not a blanket "always Legacy".
#[test]
fn a_clause_that_names_its_zone_without_the_anaphor_reads_that_zone() {
    let sources = candidate_sources(REJOIN_THE_FIGHT, "Rejoin the Fight", &["Sorcery"]);
    assert!(
        !sources.is_empty(),
        "Rejoin the Fight must lower to a ChooseFromZone"
    );
    assert!(
        sources
            .iter()
            .all(|s| matches!(s, ZoneChoiceCandidateSource::Direct)),
        "\"a creature card in your graveyard\" names its zone and carries no \
         exiled-this-way anaphor, so it must read that zone; got {sources:?}"
    );

    // Dauthi Voidwalker names the zone via its own filter ("an exiled card an
    // opponent owns with a void counter on it") with no linked-exile anaphor.
    let dauthi = candidate_sources(DAUTHI_VOIDWALKER, "Dauthi Voidwalker", &["Creature"]);
    assert!(
        !dauthi.is_empty(),
        "Dauthi Voidwalker must lower to a ChooseFromZone"
    );
    assert!(
        dauthi
            .iter()
            .all(|s| matches!(s, ZoneChoiceCandidateSource::Direct)),
        "Dauthi Voidwalker's void-counter clause is a real zone scan, not an \
         exiled-this-way anaphor; got {dauthi:?}"
    );
}
