#!/usr/bin/env python3
"""Aggregate dropped Oracle clauses by SHARED PREFIX, not full snippet.

For each engine swallow warning, locate the trigger phrase ("if", "unless",
"you may", etc.) and capture only the first N words AFTER it. Group by that
prefix to find shared lead-ins that span many distinct tails.

Goal: surface "if you control a [...]"-style prefixes that cover dozens of
cards via different tails, even though no full-tail normalization matches.
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

TRIGGERS = {
    "Condition_If":           re.compile(r"(?<![a-z])if [a-z]"),
    "Condition_Unless":       re.compile(r"\bunless\b"),
    "Condition_AsLongAs":     re.compile(r"\bas long as\b"),
    "Optional_YouMay":        re.compile(r"\byou may\b"),
    "Duration_ThisTurn":      re.compile(r"\bthis turn\b"),
    "Duration_UntilEndOfTurn":re.compile(r"\buntil end of turn\b"),
    "Replacement_Instead":    re.compile(r"\binstead\b"),
    "DynamicQty":             re.compile(r"\b(?:equal to|for each|\btwice\b|where x is|the number of|half (?:your|their|its|the) (?:life|library))\b"),
}

def sentence_around(text: str, start: int, end: int) -> str:
    s = max(0, text.rfind(".", 0, start) + 1)
    e = text.find(".", end)
    if e == -1:
        e = len(text)
    return text[s:e].strip()

target_class = sys.argv[1] if len(sys.argv) > 1 else "Condition_If"
prefix_words = int(sys.argv[2]) if len(sys.argv) > 2 else 4
top_n        = int(sys.argv[3]) if len(sys.argv) > 3 else 30

freq    = Counter()
samples = defaultdict(list)
cls_re  = re.compile(r"^Swallow:([A-Za-z_]+)\s+—")

for cname, card in CARDS.items():
    warnings = card.get("parse_warnings") or []
    if not warnings:
        continue
    raw = card.get("oracle_text") or ""
    cleaned = PARENS.sub("", raw).lower()
    seen = set()
    for w in warnings:
        m = cls_re.match(w)
        if not m:
            continue
        cls = m.group(1)
        if cls != target_class or cls in seen:
            continue
        seen.add(cls)
        regex = TRIGGERS.get(cls)
        if not regex:
            continue
        rm = regex.search(cleaned)
        if not rm:
            continue
        # Capture text starting at the trigger phrase and take next N words
        tail = cleaned[rm.start():]
        norm = normalize(tail, card["name"])
        words = norm.split()
        if len(words) < prefix_words:
            continue
        prefix = " ".join(words[:prefix_words])
        freq[prefix] += 1
        if len(samples[prefix]) < 4:
            samples[prefix].append(card["name"])

print(f"\n## {target_class} prefixes (first {prefix_words} words)  —  {sum(freq.values())} cards, {len(freq)} distinct prefixes\n")
for prefix, count in freq.most_common(top_n):
    ex = ", ".join(samples[prefix][:4])
    print(f"  [{count:>4}]  {prefix}")
    print(f"          ex: {ex}")
