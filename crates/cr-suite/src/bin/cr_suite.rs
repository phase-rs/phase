//! CLI for the Comprehensive Rules scenario suite.
//!
//! ```text
//! cargo cr-suite --generate [--update]
//! cargo cr-suite --summary
//! cargo cr-suite --run [--section N] [--rule R] [--fail-fast]
//! ```

use std::path::PathBuf;
use std::process;

use cr_suite::catalog::{is_included_section, section_title, INCLUDED_SECTIONS};
use cr_suite::generate::{generate_skeletons, GenerateOptions};
use cr_suite::loader::load_scenarios;
use cr_suite::runner::{run_suite, RunFilter, RunOptions};
use cr_suite::schema::ScenarioStatus;

struct Config {
    generate: bool,
    update: bool,
    summary: bool,
    run: bool,
    fail_fast: bool,
    comp_rules: PathBuf,
    scenarios_dir: PathBuf,
    sections: Option<Vec<u32>>,
    rules: Option<Vec<String>>,
}

fn parse_args() -> Config {
    let args: Vec<String> = std::env::args().collect();
    let mut generate = false;
    let mut update = false;
    let mut summary = false;
    let mut run = false;
    let mut fail_fast = false;
    let mut comp_rules = PathBuf::from("docs/MagicCompRules.txt");
    let mut scenarios_dir = PathBuf::from("crates/cr-suite/scenarios");
    let mut sections: Option<Vec<u32>> = None;
    let mut rules: Option<Vec<String>> = None;

    let mut iter = args.iter().skip(1).peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--generate" => generate = true,
            "--update" => update = true,
            "--summary" => summary = true,
            "--run" => run = true,
            "--fail-fast" => fail_fast = true,
            "--comp-rules" => {
                if let Some(v) = iter.next() {
                    comp_rules = PathBuf::from(v);
                }
            }
            "--scenarios-dir" => {
                if let Some(v) = iter.next() {
                    scenarios_dir = PathBuf::from(v);
                }
            }
            "--section" => {
                if let Some(v) = iter.next() {
                    let n: u32 = v.parse().unwrap_or(0);
                    sections.get_or_insert_with(Vec::new).push(n);
                }
            }
            "--rule" => {
                if let Some(v) = iter.next() {
                    rules.get_or_insert_with(Vec::new).push(v.clone());
                }
            }
            "--help" | "-h" => {
                print_help();
                process::exit(0);
            }
            other => {
                eprintln!("unknown argument: {other}");
                print_help();
                process::exit(2);
            }
        }
    }

    // Default mode: summary when no flags given.
    if !generate && !summary && !run {
        summary = true;
    }

    Config {
        generate,
        update,
        summary,
        run,
        fail_fast,
        comp_rules,
        scenarios_dir,
        sections,
        rules,
    }
}

fn print_help() {
    eprintln!(
        "\
cr-suite — executable Comprehensive Rules scenario suite

Usage:
  cargo cr-suite --generate [--update] [--section N]
  cargo cr-suite --summary [--scenarios-dir PATH]
  cargo cr-suite --run [--section N] [--rule R] [--fail-fast]

Options:
  --comp-rules PATH       CompRules.txt (default: docs/MagicCompRules.txt)
  --scenarios-dir PATH    Fixture root (default: crates/cr-suite/scenarios)
  --section N             Filter to section number (repeatable)
  --rule R                Filter to rule number (repeatable)
  --update                Preserve authored (non-skeleton) fixtures on generate
  --fail-fast             Stop suite run on first failure
"
    );
}

fn main() {
    let config = parse_args();

    if config.generate {
        if let Err(e) = do_generate(&config) {
            eprintln!("generate failed: {e}");
            process::exit(1);
        }
    }

    if config.summary {
        if let Err(e) = do_summary(&config) {
            eprintln!("summary failed: {e}");
            process::exit(1);
        }
    }

    if config.run {
        match do_run(&config) {
            Ok(true) => {}
            Ok(false) => process::exit(1),
            Err(e) => {
                eprintln!("run failed: {e}");
                process::exit(1);
            }
        }
    }
}

fn do_generate(config: &Config) -> Result<(), String> {
    let stats = generate_skeletons(&GenerateOptions {
        comp_rules: config.comp_rules.clone(),
        out_dir: config.scenarios_dir.clone(),
        // Never clobber authored (executable / not-applicable / deferred) fixtures.
        preserve_authored: true,
        sections: config.sections.clone(),
    })?;
    let _ = config.update; // reserved: future "refresh skeleton text" mode
    println!(
        "cr-suite generate: saw {} rules, wrote {}, preserved {}, filter-skipped {}",
        stats.rules_seen, stats.written, stats.preserved, stats.skipped_filter
    );
    Ok(())
}

fn do_summary(config: &Config) -> Result<(), String> {
    let loaded = load_scenarios(&config.scenarios_dir).map_err(|e| e.to_string())?;
    let mut skeleton = 0usize;
    let mut executable = 0usize;
    let mut not_applicable = 0usize;
    let mut deferred = 0usize;
    let mut by_section: std::collections::BTreeMap<u32, [usize; 4]> =
        std::collections::BTreeMap::new();

    for (_, scenario) in &loaded {
        if let Some(sections) = &config.sections {
            if !sections.contains(&scenario.section) {
                continue;
            }
        }
        let idx = match scenario.status {
            ScenarioStatus::Skeleton => {
                skeleton += 1;
                0
            }
            ScenarioStatus::Executable => {
                executable += 1;
                1
            }
            ScenarioStatus::NotApplicable => {
                not_applicable += 1;
                2
            }
            ScenarioStatus::Deferred => {
                deferred += 1;
                3
            }
        };
        by_section.entry(scenario.section).or_default()[idx] += 1;
    }

    println!("cr-suite catalog summary");
    println!("  fixtures: {}", loaded.len());
    println!("  skeleton:        {skeleton}");
    println!("  executable:      {executable}");
    println!("  not-applicable:  {not_applicable}");
    println!("  deferred:        {deferred}");
    println!();
    println!("  included sections ({}):", INCLUDED_SECTIONS.len());
    for section in INCLUDED_SECTIONS {
        if !is_included_section(*section) {
            continue;
        }
        let counts = by_section.get(section).copied().unwrap_or([0; 4]);
        let total = counts.iter().sum::<usize>();
        if total == 0 {
            continue;
        }
        println!(
            "    {section:03} {:40}  total={total:4}  exec={}  skel={}  n/a={}  def={}",
            section_title(*section),
            counts[1],
            counts[0],
            counts[2],
            counts[3]
        );
    }
    Ok(())
}

fn do_run(config: &Config) -> Result<bool, String> {
    let report = run_suite(&RunOptions {
        scenarios_dir: config.scenarios_dir.clone(),
        filter: RunFilter {
            sections: config.sections.clone(),
            rules: config.rules.clone(),
            include_non_executable: false,
        },
        fail_fast: config.fail_fast,
    })?;
    print!("{}", report.render_summary());
    Ok(report.success())
}
