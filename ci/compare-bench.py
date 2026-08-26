#!/usr/bin/env python3
"""Gate ci/bench.sh JSON against ci/perf-budgets.json.

Exit 1 if any metric exceeds its *_max budget. An optional --baseline JSON
(from main) prints a delta only; it never fails the job.
"""

from __future__ import annotations

import argparse
import json
import sys
from typing import Any

PAIRS = (
    ("dehydrate_wall_ms", "dehydrate_wall_ms_max"),
    ("rehydrate_wall_ms", "rehydrate_wall_ms_max"),
    ("dehydrate_peak_rss_kb", "dehydrate_peak_rss_kb_max"),
    ("rehydrate_peak_rss_kb", "rehydrate_peak_rss_kb_max"),
    ("ratio_archive_to_jars", "ratio_archive_to_jars_max"),
    ("ratio_unique_to_uncompressed", "ratio_unique_to_uncompressed_max"),
)


def load_json(path: str) -> dict[str, Any]:
    with open(path, encoding="utf-8") as f:
        data = json.load(f)
    if not isinstance(data, dict):
        sys.exit(f"{path}: expected a JSON object")
    return data


def as_number(value: Any, label: str) -> float:
    if isinstance(value, bool) or value is None:
        sys.exit(f"{label}: expected a number, got {value!r}")
    if isinstance(value, (int, float)):
        return float(value)
    if isinstance(value, str):
        try:
            return float(value)
        except ValueError:
            sys.exit(f"{label}: expected a number, got {value!r}")
    sys.exit(f"{label}: expected a number, got {value!r}")


def fmt_num(value: float) -> str:
    if value.is_integer() and abs(value) < 1e15:
        return str(int(value))
    return f"{value:.6g}"


def print_delta(results: dict[str, Any], baseline: dict[str, Any]) -> None:
    print("=== vs baseline (informational; not a gate) ===")
    for metric, _budget_key in PAIRS:
        if metric not in results or metric not in baseline:
            print(f"{metric}: (missing in results or baseline)")
            continue
        cur = as_number(results[metric], f"results.{metric}")
        old = as_number(baseline[metric], f"baseline.{metric}")
        delta = cur - old
        if old == 0:
            pct = "n/a"
        else:
            pct = f"{(delta / old) * 100:+.1f}%"
        sign = "+" if delta >= 0 else ""
        print(
            f"{metric}: {fmt_num(old)} -> {fmt_num(cur)}  ({sign}{fmt_num(delta)}, {pct})"
        )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Fail if bench-results.json exceeds perf-budgets.json maxima."
    )
    parser.add_argument("results", help="bench-results.json from ci/bench.sh")
    parser.add_argument("budgets", help="ci/perf-budgets.json")
    parser.add_argument(
        "--baseline",
        help="optional previous (main) bench-results.json; delta is non-failing",
    )
    args = parser.parse_args(argv)

    results = load_json(args.results)
    budgets = load_json(args.budgets)

    failed: list[str] = []
    print("=== performance budgets ===")
    for metric, budget_key in PAIRS:
        if metric not in results:
            failed.append(f"missing results.{metric}")
            print(f"{metric}: MISSING")
            continue
        if budget_key not in budgets:
            failed.append(f"missing budgets.{budget_key}")
            print(f"{budget_key}: MISSING")
            continue
        value = as_number(results[metric], f"results.{metric}")
        limit = as_number(budgets[budget_key], f"budgets.{budget_key}")
        # Equal to the max is in budget; strictly greater fails.
        if value > limit:
            status = "OVER"
            failed.append(
                f"{metric}={fmt_num(value)} exceeds {budget_key}={fmt_num(limit)}"
            )
        else:
            status = "OK"
        print(f"{metric}: {fmt_num(value)} / {fmt_num(limit)}  {status}")

    if args.baseline:
        print_delta(results, load_json(args.baseline))

    if failed:
        print("budget gate failed:", file=sys.stderr)
        for line in failed:
            print(f"  {line}", file=sys.stderr)
        return 1
    print("budget gate passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
