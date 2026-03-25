#!/usr/bin/env python3
"""Train turn-phase-aware AI evaluation weights from 17Lands Premier Draft replay data.

Extracts per-turn board state features from 17Lands replay CSVs, splits into
three game phases (early T1-3, mid T4-7, late T8+), trains a separate logistic
regression for each phase, and outputs phase-bucketed EvalWeights as JSON.

This maximizes leverage of the temporal signal in 17Lands data: what predicts
winning changes dramatically across game phases.

Usage:
    python3 scripts/train_eval_weights.py --data-dir ~/Downloads --output data/learned-weights.json
"""

import argparse
import glob
import json
import os
import sys
from pathlib import Path

import numpy as np
import pandas as pd
from sklearn.linear_model import LogisticRegression
from sklearn.model_selection import train_test_split


# Features extracted from each turn snapshot. Dropped total_permanent_diff
# (linear combination of creature_count + land + non_creature — redundant in
# a linear model and causes collinearity).
FEATURE_NAMES = [
    "life_diff",
    "creature_count_diff",
    "creature_mv_diff",
    "hand_diff",
    "land_diff",
    "non_creature_diff",
    "mana_spent_diff",
]

# Mapping from regression feature names to EvalWeights struct fields.
FEATURE_TO_WEIGHT = {
    "life_diff": "life",
    "creature_count_diff": "board_presence",
    "creature_mv_diff": "board_power",
    "hand_diff": "hand_size",
    "non_creature_diff": "card_advantage",
}

# Hand-tuned defaults for weights 17Lands cannot measure.
HAND_TUNED = {
    "board_toughness": 1.0,
    "aggression": 0.5,
    "zone_quality": 0.3,
    "synergy": 0.5,
}

# Target maximum absolute weight value after scaling.
MAX_WEIGHT_SCALE = 2.5

# Turn boundaries for game phases.
EARLY_MAX = 3   # turns 1-3
MID_MAX = 7     # turns 4-7
# turns 8+ = late


def turn_phase(turn: int) -> str:
    """Classify a turn number into a game phase."""
    if turn <= EARLY_MAX:
        return "early"
    elif turn <= MID_MAX:
        return "mid"
    else:
        return "late"


def count_ids(value) -> int:
    """Count pipe-separated IDs. Returns 0 for NaN/empty."""
    if pd.isna(value):
        return 0
    s = str(value).strip()
    if s == "" or s == "nan":
        return 0
    return len(s.split("|"))


def sum_mv(value, card_mv: dict) -> float:
    """Sum mana values for pipe-separated Arena IDs."""
    if pd.isna(value):
        return 0.0
    s = str(value).strip()
    if s == "" or s == "nan":
        return 0.0
    total = 0.0
    for part in s.split("|"):
        try:
            arena_id = int(float(part))
            total += card_mv.get(arena_id, 0.0)
        except (ValueError, OverflowError):
            continue
    return total


def load_card_data(data_dir: str) -> dict:
    """Load cards.csv and return Arena ID -> mana_value mapping."""
    cards_path = os.path.join(data_dir, "cards.csv")
    if not os.path.exists(cards_path):
        print(f"ERROR: cards.csv not found at {cards_path}", file=sys.stderr)
        sys.exit(1)

    cards = pd.read_csv(cards_path)
    card_mv = {}
    for _, row in cards.iterrows():
        try:
            arena_id = int(row["id"])
            mv = float(row["mana_value"]) if pd.notna(row["mana_value"]) else 0.0
            card_mv[arena_id] = mv
        except (ValueError, KeyError):
            continue

    print(f"Loaded {len(card_mv)} card entries from cards.csv", file=sys.stderr)
    return card_mv


def discover_replay_files(data_dir: str) -> list[str]:
    """Find all 17Lands Premier Draft replay CSVs."""
    pattern = os.path.join(data_dir, "replay_data_public.*.PremierDraft.csv")
    files = sorted(glob.glob(pattern))
    if not files:
        print(f"ERROR: No replay CSVs found matching {pattern}", file=sys.stderr)
        sys.exit(1)

    print(f"\nDiscovered {len(files)} replay file(s):", file=sys.stderr)
    for f in files:
        size_mb = os.path.getsize(f) / (1024 * 1024)
        set_code = Path(f).name.split(".")[1]
        print(f"  {set_code}: {Path(f).name} ({size_mb:.0f} MB)", file=sys.stderr)

    if len(files) < 3:
        print(
            f"\nWARNING: Only {len(files)} set(s) found. "
            "Recommend 3-5 sets for robust weights (per D-03).",
            file=sys.stderr,
        )

    return files


def extract_turn_features(
    row: pd.Series,
    prefix: str,
    card_mv: dict,
) -> list[float] | None:
    """Extract differential features for one turn.

    Returns feature vector or None if turn data is missing.
    """
    life_col = f"{prefix}_eot_user_life"
    if life_col not in row.index or pd.isna(row.get(life_col)):
        return None

    user_life = float(row[f"{prefix}_eot_user_life"])
    oppo_life = float(row[f"{prefix}_eot_oppo_life"])

    user_creatures = row.get(f"{prefix}_eot_user_creatures_in_play")
    oppo_creatures = row.get(f"{prefix}_eot_oppo_creatures_in_play")
    user_hand = row.get(f"{prefix}_eot_user_cards_in_hand")
    oppo_hand = row.get(f"{prefix}_eot_oppo_cards_in_hand")
    user_lands = row.get(f"{prefix}_eot_user_lands_in_play")
    oppo_lands = row.get(f"{prefix}_eot_oppo_lands_in_play")
    user_nc = row.get(f"{prefix}_eot_user_non_creatures_in_play")
    oppo_nc = row.get(f"{prefix}_eot_oppo_non_creatures_in_play")

    # Mana spent columns use a different naming pattern (no _eot_ prefix)
    user_mana = row.get(f"{prefix}_user_mana_spent", 0.0)
    oppo_mana = row.get(f"{prefix}_oppo_mana_spent", 0.0)
    user_mana = float(user_mana) if pd.notna(user_mana) else 0.0
    oppo_mana = float(oppo_mana) if pd.notna(oppo_mana) else 0.0

    life_diff = user_life - oppo_life
    creature_count_diff = count_ids(user_creatures) - count_ids(oppo_creatures)
    creature_mv_diff = sum_mv(user_creatures, card_mv) - sum_mv(oppo_creatures, card_mv)

    # Hand: user hand is pipe-separated IDs, opponent hand is a count
    user_hand_count = count_ids(user_hand)
    oppo_hand_count = float(oppo_hand) if pd.notna(oppo_hand) else 0.0
    hand_diff = user_hand_count - oppo_hand_count

    land_diff = count_ids(user_lands) - count_ids(oppo_lands)
    non_creature_diff = count_ids(user_nc) - count_ids(oppo_nc)
    mana_spent_diff = user_mana - oppo_mana

    return [
        life_diff,
        creature_count_diff,
        creature_mv_diff,
        hand_diff,
        land_diff,
        non_creature_diff,
        mana_spent_diff,
    ]


def process_replay_files(
    files: list[str],
    card_mv: dict,
    min_win_rate: float,
    min_games: int,
) -> tuple[dict[str, list], dict[str, list], list[str]]:
    """Stream replay CSVs and extract training features bucketed by game phase.

    Returns (phase_features, phase_labels, set_codes) where phase_features and
    phase_labels are dicts keyed by "early", "mid", "late".
    """
    phase_features: dict[str, list] = {"early": [], "mid": [], "late": []}
    phase_labels: dict[str, list] = {"early": [], "mid": [], "late": []}
    set_codes = []
    total_games = 0
    total_filtered_games = 0
    total_samples = 0

    for filepath in files:
        set_code = Path(filepath).name.split(".")[1]
        set_codes.append(set_code)
        file_samples = 0
        file_games = 0
        file_filtered = 0

        print(f"\nProcessing {set_code}...", file=sys.stderr)

        for chunk in pd.read_csv(filepath, chunksize=10000, low_memory=False):
            file_games += len(chunk)

            # Skill and experience filters
            if "user_game_win_rate_bucket" in chunk.columns:
                chunk = chunk[chunk["user_game_win_rate_bucket"] >= min_win_rate]
            if "user_n_games_bucket" in chunk.columns:
                chunk = chunk[chunk["user_n_games_bucket"] >= min_games]

            file_filtered += len(chunk)

            for _, row in chunk.iterrows():
                won = bool(row.get("won", False))

                for turn in range(1, 31):
                    phase = turn_phase(turn)

                    # User's turn
                    user_prefix = f"user_turn_{turn}"
                    feats = extract_turn_features(row, user_prefix, card_mv)
                    if feats is not None:
                        phase_features[phase].append(feats)
                        phase_labels[phase].append(1 if won else 0)
                        file_samples += 1

                    # Opponent's turn
                    oppo_prefix = f"oppo_turn_{turn}"
                    feats = extract_turn_features(row, oppo_prefix, card_mv)
                    if feats is not None:
                        phase_features[phase].append(feats)
                        phase_labels[phase].append(1 if won else 0)
                        file_samples += 1

        total_games += file_games
        total_filtered_games += file_filtered
        total_samples += file_samples
        print(
            f"  {set_code}: {file_games} games, {file_filtered} after filter, "
            f"{file_samples} training samples",
            file=sys.stderr,
        )

    print(
        f"\nTotal: {total_games} games, {total_filtered_games} after filter, "
        f"{total_samples} training samples",
        file=sys.stderr,
    )
    for phase in ["early", "mid", "late"]:
        print(
            f"  {phase}: {len(phase_features[phase])} samples",
            file=sys.stderr,
        )

    return phase_features, phase_labels, set_codes


def train_model(
    X: np.ndarray, y: np.ndarray, phase_name: str
) -> tuple[LogisticRegression, float, float]:
    """Train logistic regression for a single phase and return model + accuracy."""
    X_train, X_test, y_train, y_test = train_test_split(
        X, y, test_size=0.2, random_state=42, stratify=y
    )

    model = LogisticRegression(penalty="l2", C=1.0, max_iter=1000, random_state=42)
    model.fit(X_train, y_train)

    train_accuracy = model.score(X_train, y_train)
    test_accuracy = model.score(X_test, y_test)

    print(f"\n  {phase_name} accuracy: train={train_accuracy:.4f} test={test_accuracy:.4f}", file=sys.stderr)

    return model, train_accuracy, test_accuracy


def extract_and_scale_weights(
    model: LogisticRegression,
    phase_name: str,
) -> tuple[dict, dict]:
    """Extract coefficients and scale to EvalWeights range.

    Returns (raw_coefficients, scaled_weights).
    """
    raw_coefs = {}
    for name, coef in zip(FEATURE_NAMES, model.coef_[0]):
        raw_coefs[name] = round(float(coef), 6)

    print(f"  {phase_name} raw coefficients:", file=sys.stderr)
    for name, coef in raw_coefs.items():
        sign = "+" if coef >= 0 else ""
        print(f"    {name}: {sign}{coef}", file=sys.stderr)

    # Sanity checks
    if raw_coefs["life_diff"] <= 0:
        print(
            f"  WARNING: {phase_name} life_diff coefficient is non-positive!",
            file=sys.stderr,
        )
    if raw_coefs["creature_count_diff"] <= 0:
        print(
            f"  WARNING: {phase_name} creature_count_diff coefficient is non-positive!",
            file=sys.stderr,
        )

    # Scale mapped coefficients so max absolute value = MAX_WEIGHT_SCALE.
    mapped_coefs = {
        feat: raw_coefs[feat]
        for feat in FEATURE_TO_WEIGHT
        if feat in raw_coefs
    }

    max_abs = max(abs(v) for v in mapped_coefs.values()) if mapped_coefs else 1.0
    scale_factor = MAX_WEIGHT_SCALE / max_abs if max_abs > 0 else 1.0

    weights = {}
    for feat_name, weight_name in FEATURE_TO_WEIGHT.items():
        scaled = abs(raw_coefs[feat_name]) * scale_factor
        weights[weight_name] = round(scaled, 4)

    # Add hand-tuned defaults for unmeasurable weights
    weights.update(HAND_TUNED)

    print(f"  {phase_name} scaled weights:", file=sys.stderr)
    for name, val in weights.items():
        source = "17Lands" if name not in HAND_TUNED else "hand-tuned"
        print(f"    {name}: {val} ({source})", file=sys.stderr)

    return raw_coefs, weights


def main():
    parser = argparse.ArgumentParser(
        description="Train turn-phase-aware AI evaluation weights from 17Lands replay data."
    )
    parser.add_argument(
        "--data-dir",
        default=os.path.expanduser("~/Downloads"),
        help="Directory containing replay CSVs and cards.csv (default: ~/Downloads)",
    )
    parser.add_argument(
        "--output",
        default="data/learned-weights.json",
        help="Output JSON path (default: data/learned-weights.json)",
    )
    parser.add_argument(
        "--min-win-rate",
        type=float,
        default=0.55,
        help="Minimum user_game_win_rate_bucket filter (default: 0.55)",
    )
    parser.add_argument(
        "--min-games",
        type=int,
        default=50,
        help="Minimum user_n_games_bucket filter (default: 50)",
    )
    args = parser.parse_args()

    print("=== 17Lands Phase-Aware EvalWeights Training ===\n", file=sys.stderr)

    # Load card metadata
    card_mv = load_card_data(args.data_dir)

    # Discover replay files
    files = discover_replay_files(args.data_dir)

    # Extract features bucketed by game phase
    phase_features, phase_labels, set_codes = process_replay_files(
        files, card_mv, args.min_win_rate, args.min_games
    )

    total_samples = sum(len(v) for v in phase_features.values())
    if total_samples < 1000:
        print(
            f"ERROR: Only {total_samples} training samples extracted. "
            "Need at least 1000 for meaningful training.",
            file=sys.stderr,
        )
        sys.exit(1)

    # Train one model per game phase
    print("\n--- Training phase-specific models ---", file=sys.stderr)
    phase_results = {}

    for phase in ["early", "mid", "late"]:
        X = np.array(phase_features[phase], dtype=np.float64)
        y = np.array(phase_labels[phase], dtype=np.float64)

        if len(X) < 100:
            print(f"  WARNING: {phase} has only {len(X)} samples, skipping", file=sys.stderr)
            continue

        model, train_acc, test_acc = train_model(X, y, phase)
        raw_coefs, weights = extract_and_scale_weights(model, phase)

        phase_results[phase] = {
            "sample_count": len(X),
            "train_accuracy": round(train_acc, 4),
            "test_accuracy": round(test_acc, 4),
            "raw_coefficients": raw_coefs,
            "weights": weights,
        }

    # Build output JSON
    output = {
        "source": "17lands_PremierDraft_phase_aware",
        "sets": set_codes,
        "filter": f"win_rate >= {args.min_win_rate}, games >= {args.min_games}",
        "total_sample_count": total_samples,
        "feature_names": FEATURE_NAMES,
        "turn_boundaries": {
            "early": f"turns 1-{EARLY_MAX}",
            "mid": f"turns {EARLY_MAX + 1}-{MID_MAX}",
            "late": f"turns {MID_MAX + 1}+",
        },
        "phases": phase_results,
    }

    # Ensure output directory exists
    os.makedirs(os.path.dirname(args.output) or ".", exist_ok=True)

    with open(args.output, "w") as f:
        json.dump(output, f, indent=2)
        f.write("\n")

    print(f"\nPhase-aware weights written to {args.output}", file=sys.stderr)
    print(f"Total samples: {total_samples}", file=sys.stderr)

    # Summary table
    print("\n=== Phase Weight Comparison ===", file=sys.stderr)
    header = f"{'weight':<18}"
    for phase in ["early", "mid", "late"]:
        header += f"  {phase:>8}"
    print(header, file=sys.stderr)
    print("-" * len(header), file=sys.stderr)

    if phase_results:
        all_weight_names = list(phase_results[next(iter(phase_results))]["weights"].keys())
        for wname in all_weight_names:
            row = f"{wname:<18}"
            for phase in ["early", "mid", "late"]:
                val = phase_results.get(phase, {}).get("weights", {}).get(wname, "-")
                if isinstance(val, float):
                    row += f"  {val:>8.4f}"
                else:
                    row += f"  {str(val):>8}"
            print(row, file=sys.stderr)

    print("\nDone.", file=sys.stderr)


if __name__ == "__main__":
    main()
