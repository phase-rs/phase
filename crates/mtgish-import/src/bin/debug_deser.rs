//! Throwaway: classify deserialize failures by error message.

use std::collections::BTreeMap;
use std::fs;

use mtgish_import::schema::types::OracleCard;

fn main() -> anyhow::Result<()> {
    let raw = fs::read_to_string("data/mtgish-cards.json")?;
    let arr: Vec<serde_json::Value> = serde_json::from_str(&raw)?;
    let mut by_error: BTreeMap<String, (usize, Vec<String>)> = BTreeMap::new();
    let mut total_fail = 0;
    for v in arr {
        let name = v
            .get("Name")
            .and_then(|x| x.as_str())
            .unwrap_or("<no name>")
            .to_string();
        if let Err(e) = serde_json::from_value::<OracleCard>(v) {
            total_fail += 1;
            let key = normalize_err(&e.to_string());
            let entry = by_error.entry(key).or_default();
            entry.0 += 1;
            if entry.1.len() < 3 {
                entry.1.push(name);
            }
        }
    }
    println!("total deserialize failures: {total_fail}");
    let mut ranked: Vec<_> = by_error.into_iter().collect();
    ranked.sort_by_key(|b| std::cmp::Reverse(b.1 .0));
    for (err, (count, examples)) in ranked.iter().take(30) {
        println!("\n[{count}] {err}");
        for ex in examples {
            println!("    e.g. {ex}");
        }
    }
    Ok(())
}

fn normalize_err(s: &str) -> String {
    // Strip line/column suffix to cluster similar errors.
    if let Some(idx) = s.find(" at line ") {
        s[..idx].to_string()
    } else {
        s.to_string()
    }
}
