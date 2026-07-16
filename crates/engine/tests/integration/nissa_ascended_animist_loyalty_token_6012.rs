//! Issue #6012 — Nissa, Ascended Animist's +1 must create the Phyrexian Horror
//! token whose power and toughness equal Nissa's loyalty.
//!
//! Oracle (+1 loyalty ability):
//!   "Create an X/X green Phyrexian Horror creature token, where X is Nissa's
//!    loyalty."
//!
//! After card-name normalization the parser sees "where X is ~'s loyalty".
//! Before the fix, the self-possessive loyalty quantity was unrecognized, so
//! `parse_cda_quantity` returned `None`, the whole token clause failed to lower
//! (`try_parse_token` returns `None` when the P/T expression is unrepresentable),
//! and the ability degraded to `Effect::Unimplemented` — activating +1 created
//! no token at all.
//!
//! The fix adds `parse_self_loyalty_ref`, mapping "~'s loyalty" to
//! `QuantityRef::CountersOn { scope: Source, counter_type: Some(Loyalty) }`
//! (CR 306.5c: the loyalty of a planeswalker on the battlefield is the number of
//! loyalty counters on it). This runtime regression resolves the parsed ability
//! against a Nissa with five loyalty counters and asserts a 5/5 Phyrexian Horror
//! token is created; if the quantity ever silently drops again the token becomes
//! 0/0 (or absent) and the assertions flip.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::AbilityKind;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::format::FormatConfig;
use engine::types::game_state::GameState;
use engine::types::identifiers::CardId;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

// Verbatim +1 body with the card name normalized to `~` exactly as the pipeline
// presents it to the effect parser (CR 201.5 name normalization).
const PLUS_ONE_BODY: &str =
    "Create an X/X green Phyrexian Horror creature token, where X is ~'s loyalty.";

const LOYALTY: u32 = 5;

#[test]
fn issue_6012_nissa_plus_one_creates_horror_token_sized_to_loyalty() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 7);

    // Nissa on the battlefield as a planeswalker holding five loyalty counters.
    // CR 306.5c: loyalty on the battlefield IS the loyalty-counter count, which
    // is the quantity the token's "where X is ~'s loyalty" clause reads.
    let nissa = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Nissa, Ascended Animist".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&nissa).expect("nissa exists");
        obj.card_types.core_types.push(CoreType::Planeswalker);
        obj.base_card_types = obj.card_types.clone();
        obj.counters.insert(CounterType::Loyalty, LOYALTY);
    }

    let def = parse_effect_chain(PLUS_ONE_BODY, AbilityKind::Activated);
    // Reach-guard: the +1 body must lower to a real token effect, not the
    // `Effect::Unimplemented` degrade that the pre-fix parser produced. Without
    // this positive assertion the "token exists" check below could pass
    // vacuously for the wrong reason (foot-gun #6: vacuous negative).
    assert!(
        !format!("{:?}", def.effect).contains("Unimplemented"),
        "+1 body must parse to a concrete effect, got {:?}",
        def.effect
    );

    let ability = build_resolved_from_def(&def, nissa, PlayerId(0));
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).expect("token creation resolves");

    // Observable state: a brand-new token creature (distinct from Nissa) with
    // the Horror subtype, on the controller's battlefield, sized to loyalty.
    let token = state
        .objects
        .values()
        .find(|obj| {
            obj.id != nissa
                && obj.is_token
                && obj.zone == Zone::Battlefield
                && obj.card_types.core_types.contains(&CoreType::Creature)
        })
        .expect("Nissa's +1 must create a token creature");

    assert!(
        token
            .card_types
            .subtypes
            .iter()
            .any(|s| s.eq_ignore_ascii_case("Horror")),
        "token must be a Phyrexian Horror, got subtypes {:?}",
        token.card_types.subtypes
    );
    assert_eq!(
        token.power,
        Some(LOYALTY as i32),
        "token power must equal Nissa's loyalty ({LOYALTY})"
    );
    assert_eq!(
        token.toughness,
        Some(LOYALTY as i32),
        "token toughness must equal Nissa's loyalty ({LOYALTY})"
    );
}
