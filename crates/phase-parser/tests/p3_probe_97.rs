//! SCRATCH (ticket 97, never committed): print the parse of card texts as JSON.
use phase_parser::parser::oracle::parse_oracle_text;

#[test]
fn p3_probe_97_parse() {
    let path = std::env::var("P3PROBE_CASES").expect("P3PROBE_CASES");
    let out = std::env::var("P3PROBE_OUT").expect("P3PROBE_OUT");
    let cases: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let mut lines = String::new();
    for c in cases {
        let name = c["name"].as_str().unwrap();
        let text = c["text"].as_str().unwrap();
        let types: Vec<String> = c["types"]
            .as_array()
            .map(|a| a.iter().map(|v| v.as_str().unwrap().to_string()).collect())
            .unwrap_or_else(|| vec!["Instant".into()]);
        let subtypes: Vec<String> = vec![];
        let parsed = parse_oracle_text(text, name, &[], &types, &subtypes);
        let v = serde_json::json!({"name": name, "parsed": parsed});
        lines.push_str(&serde_json::to_string(&v).unwrap());
        lines.push('\n');
    }
    std::fs::write(out, lines).unwrap();
}
