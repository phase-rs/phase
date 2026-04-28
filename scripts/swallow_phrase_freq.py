#!/usr/bin/env python3
"""
Aggregate dropped Oracle clauses across cards using the *engine's own*
swallow warnings as ground truth, then group by a normalized phrase form
and rank by frequency. Use this to find the highest-leverage phrase
patterns to fix next.

Why use engine warnings (not Python regex heuristics)?
  The engine emits `parse_warnings: ["Swallow:<class> — ..."]` from
  swallow_check.rs at parse time, with full AST visibility. Python regex
  heuristics over a serialized AST produce many false positives because
  they cannot recognize structural shapes like AddTargetReplacement,
  GrantCastingPermission, MayLookAtTopOfLibrary, MayChooseNotToUntap, etc.
  that *imply* the concept without using the literal field name.

Normalization rules (aggressive — we want shape, not specifics):
  * card name (self-reference) → ~
  * mana costs {x}{1}{u} etc.    → {COST}
  * standalone integers           → N
  * tap/untap/quotes/punct        → stripped
  * collapse whitespace
  * trim trailing/leading filler
"""
import json
import re
import sys
from collections import Counter, defaultdict

CARDS = json.load(open("client/public/card-data.json"))

PARENS = re.compile(r"\([^)]*\)")
MANA   = re.compile(r"(?:\{[^}]+\})+")
NUM    = re.compile(r"\b\d+\b")
PUNCT  = re.compile(r"[\"'`,;:!?]")
WS     = re.compile(r"\s+")

def normalize(text: str, card_name: str) -> str:
    t = text.lower()
    n = card_name.lower()
    t = t.replace(n, "~")
    short = n.split(",")[0]
    if short and short != n:
        t = t.replace(short, "~")
    t = PARENS.sub("", t)
    t = MANA.sub("{COST}", t)
    t = NUM.sub("N", t)
    t = PUNCT.sub("", t)
    t = WS.sub(" ", t).strip()
    return t

# Per-class trigger regex used to locate the offending sentence in oracle_text.
TRIGGERS = {
    "DynamicQty":          re.compile(r"\b(?:equal to|for each|\btwice\b|where x is|the number of|half (?:your|their|its|the) (?:life|library))\b"),
    "Condition_If":        re.compile(r"(?<![a-z])if [a-z]"),
    "Condition_Unless":    re.compile(r"\bunless\b"),
    "Condition_AsLongAs":  re.compile(r"\bas long as\b"),
    "Duration_UntilEndOfTurn": re.compile(r"\buntil end of turn\b"),
    "Duration_ThisTurn":   re.compile(r"\bthis turn\b"),
    "Duration_NextTurn":   re.compile(r"\buntil your next turn\b"),
    "Optional_YouMay":     re.compile(r"\byou may\b"),
    "Optional_MayHave":    re.compile(r"\b(?:you may have|may have it)\b"),
    "Replacement_Instead": re.compile(r"\binstead\b"),
    "ActivateOnlyDuring":  re.compile(r"\bactivate (?:this ability )?only during\b"),
    "ActivateLimit":       re.compile(r"\bactivate (?:this ability )?(?:no more than|only) (?:once|twice|three times) each\b"),
    "APNAP":               re.compile(r"\bstarting with (?:the active player|you|that player)\b|\bin turn order\b"),
}

def sentence_around(text: str, start: int, end: int) -> str:
    s = max(0, text.rfind(".", 0, start) + 1)
    e = text.find(".", end)
    if e == -1:
        e = len(text)
    return text[s:e].strip()

target_class = sys.argv[1] if len(sys.argv) > 1 else None
top_n        = int(sys.argv[2]) if len(sys.argv) > 2 else 30

freq    = defaultdict(Counter)
samples = defaultdict(lambda: defaultdict(list))
cls_re  = re.compile(r"^Swallow:([A-Za-z_]+)\s+—")

for cname, card in CARDS.items():
    warnings = card.get("parse_warnings") or []
    if not warnings:
        continue
    raw = card.get("oracle_text") or ""
    cleaned = PARENS.sub("", raw).lower()
    seen_classes = set()
    for w in warnings:
        m = cls_re.match(w)
        if not m:
            continue
        cls = m.group(1)
        if cls in seen_classes:
            continue  # don't double-count the same class for one card
        seen_classes.add(cls)
        if target_class and cls != target_class:
            continue
        regex = TRIGGERS.get(cls)
        if not regex:
            continue
        rm = regex.search(cleaned)
        if not rm:
            continue
        sent = sentence_around(cleaned, rm.start(), rm.end())
        norm = normalize(sent, card["name"])
        freq[cls][norm] += 1
        if len(samples[cls][norm]) < 3:
            samples[cls][norm].append(card["name"])

order = sorted(freq.keys(), key=lambda k: -sum(freq[k].values()))
for cls in order:
    total = sum(freq[cls].values())
    distinct = len(freq[cls])
    print(f"\n## {cls}  —  {total} cards, {distinct} distinct shapes\n")
    for norm, count in freq[cls].most_common(top_n):
        ex = ", ".join(samples[cls][norm][:3])
        print(f"  [{count:>4}]  {norm[:140]}")
        print(f"          ex: {ex}")
