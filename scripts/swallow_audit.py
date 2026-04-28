#!/usr/bin/env python3
"""
Comprehensive parser-swallow audit. For each card marked supported, scan
Oracle text for clauses that MUST be represented in the AST per the
"never throw text away" rule. Flag every card where any required clause
is unrepresented.

Detector classes (each looks for clauses present in Oracle text but absent
from the parsed AST):

  A. DynamicQty       — "equal to <X>", "for each", "twice", "where x is",
                         "the number of", "half [poss]"
  B. Condition        — "if <X>" / "unless <X>" / "as long as <X>" not in
                         AST condition / constraint slots
  C. Duration         — "until end of turn", "this turn", "for as long as",
                         "until your next turn" not in duration slot
  D. Optional         — "you may" / "may have it" not in optional flag
  E. Replacement      — " instead" not in replacement slot
  F. ActivationTiming — "activate only during" / "activate only any time you
                         could cast a sorcery" not in restriction slot
  G. ActivationLimit  — "activate this ability only once each" / "no more
                         than X times" not in restriction slot
  H. Restriction      — "can't" / "doesn't untap during" not represented
  I. APNAP            — "starting with you" / "in turn order" not in order
                         metadata

For each card, report all detector classes triggered.
"""
import json
import re
from collections import Counter, defaultdict

CARDS = json.load(open("client/public/card-data.json"))
COVERAGE = json.load(open("client/public/coverage-data.json"))

supported = {c["card_name"].lower() for c in COVERAGE["cards"] if c["supported"]}

PARENS = re.compile(r"\([^)]*\)")

# AST-side searches: what fields/values exist in the AST that represent each
# concept. If the AST contains the marker, the clause is NOT swallowed.
def has_dynamic_qty(ast: str) -> bool:
    return any(m in ast for m in (
        '"type":"Ref"', '"type":"Multiply"', '"type":"HalfRounded"',
        '"type":"Offset"', '"Variable"',
        'EventContext', 'CountersOn', 'NumberOf', 'ForEach',
    ))

def has_condition(ast: str) -> bool:
    return any(m in ast for m in (
        '"condition":', '"constraint":', '"unless_filter"',
        '"if_clause"', '"intervening_if"',
    ))

def has_duration(ast: str) -> bool:
    return any(m in ast for m in (
        '"duration":"', 'UntilEndOfTurn', 'YourNextTurn', 'Permanent',
        'EndOfCombat', 'AsLongAs',
    ))

def has_optional(ast: str) -> bool:
    # Mirrors swallow_check.rs: top-level/sub_ability optional flags, plus
    # effect-internal optionality (Dig{up_to:true} encodes "you may keep up to N"),
    # plus replacement-mode optional ("you may have it enter as ...").
    return ('"optional":true' in ast
            or '"optional_targeting":true' in ast
            or '"may":true' in ast
            or '"up_to":true' in ast
            or '"mode":{"type":"Optional"' in ast)

def has_replacement(ast: str) -> bool:
    # If the card has any replacement definition or replaced-event semantics
    return '"replacements":[{' in ast or 'ReplacementDefinition' in ast or \
           '"is_replacement":true' in ast or '"instead":true' in ast

def has_activation_constraint(ast: str) -> bool:
    return any(m in ast for m in (
        'OnlyDuring', 'SorcerySpeed', 'ActivationLimit', 'ActivateOnly',
        '"sorcery_speed":true', 'OnceEachTurn',
    ))

DETECTORS = [
    ("DynamicQty",        re.compile(r"\b(?:equal to|for each|\btwice\b|where x is|the number of|half (?:your|their|its|the) (?:life|library))\b"), has_dynamic_qty),
    ("Condition_If",      re.compile(r"(?<![a-z])if [a-z]"), has_condition),
    ("Condition_Unless",  re.compile(r"\bunless\b"), has_condition),
    ("Condition_AsLongAs",re.compile(r"\bas long as\b"), has_condition),
    ("Duration_EOT",      re.compile(r"\buntil end of turn\b"), has_duration),
    ("Duration_ThisTurn", re.compile(r"\bthis turn\b"), has_duration),
    ("Duration_NextTurn", re.compile(r"\buntil your next turn\b"), has_duration),
    ("Optional_YouMay",   re.compile(r"\byou may\b"), has_optional),
    ("Optional_MayHave",  re.compile(r"\b(?:you may have|may have it)\b"), has_optional),
    ("Replacement_Instead", re.compile(r"\binstead\b"), has_replacement),
    ("ActivateOnlyDuring",re.compile(r"\bactivate (?:this ability )?only during\b"), has_activation_constraint),
    ("ActivateLimit",     re.compile(r"\bactivate (?:this ability )?(?:no more than|only) (?:once|twice|three times) each\b"), has_activation_constraint),
    ("SorcerySpeed",      re.compile(r"\bany time you could cast a sorcery\b"), has_activation_constraint),
    ("APNAP",             re.compile(r"\bstarting with (?:the active player|you|that player)\b|\bin turn order\b"), lambda ast: 'apnap' in ast.lower() or 'turnorder' in ast.lower() or 'starting_with' in ast.lower()),
]

# Effect verbs to gate the detector on (avoid noise in flavor/keyword reminders)
findings = defaultdict(list)
unique = set()

for cname, card in CARDS.items():
    if cname not in supported:
        continue
    raw = card.get("oracle_text") or ""
    if not raw:
        continue
    cleaned = PARENS.sub("", raw).lower()
    if not cleaned.strip():
        continue
    ast = json.dumps(card, separators=(",", ":"))
    if '"type":"Unimplemented"' in ast:
        continue
    triggered_here = []
    for label, regex, ast_check in DETECTORS:
        if regex.search(cleaned) and not ast_check(ast):
            # extract sentence
            m = regex.search(cleaned)
            start = max(0, cleaned.rfind(".", 0, m.start()) + 1)
            end = cleaned.find(".", m.end())
            if end == -1:
                end = len(cleaned)
            triggered_here.append((label, cleaned[start:end].strip()))
    if triggered_here:
        unique.add(card["name"])
        for label, snippet in triggered_here:
            findings[label].append((card["name"], snippet))

# Report
print(f"# Comprehensive parser-swallow audit\n")
print(f"Total UNIQUE cards with at least one swallowed clause: {len(unique)}\n")
print("## Findings by detector class\n")
print(f"| Class | Cards |")
print(f"|-------|-------|")
for label, items in sorted(findings.items(), key=lambda kv: -len(kv[1])):
    print(f"| {label} | {len(items)} |")

print()
for label, items in sorted(findings.items(), key=lambda kv: -len(kv[1])):
    print(f"\n## {label} — {len(items)} cards (top 5 sample)\n")
    for name, snippet in items[:5]:
        print(f"  - {name}: {snippet[:140]}")
