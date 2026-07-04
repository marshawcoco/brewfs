#!/usr/bin/env python3
"""Summarize stability across multiple BrewFS perf-run artifacts.

This script complements compare_artifacts.py: compare_artifacts.py answers
"which run is faster?", while this script answers "how noisy is this setup?".
It intentionally reuses the existing artifact parser so fio, summary, drain,
and BrewFS stats are interpreted the same way in both reports.
"""

from __future__ import annotations

import argparse
import math
import pathlib
import statistics
import sys
from dataclasses import dataclass
from typing import Iterable

SCRIPT_DIR = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

import compare_artifacts  # noqa: E402


MetricKey = tuple[str, str, str]


@dataclass(frozen=True)
class StabilityRow:
    kind: str
    item: str
    metric: str
    n: int
    mean: float
    stdev: float
    cv_pct: float
    minimum: float
    maximum: float
    unit: str


DEFAULT_KIND_ORDER = {
    "summary": 0,
    "fio": 1,
    "runtime": 2,
    "drain": 3,
    "stats": 4,
    "amplification": 5,
}


def is_number(value: object) -> bool:
    return isinstance(value, (float, int)) and math.isfinite(float(value))


def load_runs(paths: Iterable[pathlib.Path]) -> list[tuple[pathlib.Path, dict[MetricKey, compare_artifacts.Metric]]]:
    runs = []
    for path in paths:
        artifact = path.resolve()
        runs.append((artifact, compare_artifacts.load_artifact(artifact)))
    return runs


def build_rows(
    runs: list[tuple[pathlib.Path, dict[MetricKey, compare_artifacts.Metric]]],
    min_samples: int,
    include_kinds: set[str] | None,
) -> list[StabilityRow]:
    values_by_key: dict[MetricKey, list[float]] = {}
    unit_by_key: dict[MetricKey, str] = {}

    for _, metrics in runs:
        for key, metric in metrics.items():
            if include_kinds is not None and key[0] not in include_kinds:
                continue
            if not is_number(metric.value):
                continue
            values_by_key.setdefault(key, []).append(float(metric.value))
            unit_by_key.setdefault(key, metric.unit)

    rows = []
    for key, values in values_by_key.items():
        if len(values) < min_samples:
            continue
        mean = statistics.fmean(values)
        stdev = statistics.stdev(values) if len(values) > 1 else 0.0
        cv_pct = abs(stdev / mean * 100.0) if mean else 0.0
        rows.append(
            StabilityRow(
                kind=key[0],
                item=key[1],
                metric=key[2],
                n=len(values),
                mean=mean,
                stdev=stdev,
                cv_pct=cv_pct,
                minimum=min(values),
                maximum=max(values),
                unit=unit_by_key.get(key, ""),
            )
        )
    rows.sort(key=lambda row: (DEFAULT_KIND_ORDER.get(row.kind, 99), row.item, row.metric))
    return rows


def format_float(value: float) -> str:
    return f"{value:.3f}"


def emit_tsv(rows: list[StabilityRow]) -> str:
    lines = ["kind\titem\tmetric\tn\tmean\tstdev\tcv_pct\tmin\tmax\tunit"]
    for row in rows:
        lines.append(
            "\t".join(
                [
                    row.kind,
                    row.item,
                    row.metric,
                    str(row.n),
                    format_float(row.mean),
                    format_float(row.stdev),
                    format_float(row.cv_pct),
                    format_float(row.minimum),
                    format_float(row.maximum),
                    row.unit,
                ]
            )
        )
    return "\n".join(lines) + "\n"


def emit_markdown(
    rows: list[StabilityRow],
    artifacts: list[pathlib.Path],
    cv_warn_pct: float,
    top_unstable: int,
) -> str:
    lines = [
        "# BrewFS Perf Stability Report",
        "",
        f"- Samples: {len(artifacts)}",
    ]
    lines.extend(f"- `{artifact}`" for artifact in artifacts)

    unstable = sorted(rows, key=lambda row: row.cv_pct, reverse=True)
    unstable = [row for row in unstable if row.cv_pct >= cv_warn_pct][:top_unstable]
    if unstable:
        lines.extend(
            [
                "",
                f"## Highest Variance Metrics (CV >= {cv_warn_pct:.1f}%)",
                "",
                "| Item | Metric | n | Mean | CV | Min | Max | Unit |",
                "| --- | --- | ---: | ---: | ---: | ---: | ---: | --- |",
            ]
        )
        for row in unstable:
            lines.append(
                f"| {row.item} | {row.metric} | {row.n} | {format_float(row.mean)} | "
                f"{format_float(row.cv_pct)} | {format_float(row.minimum)} | "
                f"{format_float(row.maximum)} | {row.unit} |"
            )

    for kind, heading in (
        ("summary", "Summary"),
        ("fio", "Fio"),
        ("runtime", "Runtime And Tail"),
        ("drain", "Drain And Backpressure"),
        ("stats", "BrewFS Stats"),
        ("amplification", "Object And Upload Amplification"),
    ):
        group = [row for row in rows if row.kind == kind]
        if not group:
            continue
        lines.extend(
            [
                "",
                f"## {heading}",
                "",
                "| Item | Metric | n | Mean | Stdev | CV | Min | Max | Unit |",
                "| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |",
            ]
        )
        for row in group:
            lines.append(
                f"| {row.item} | {row.metric} | {row.n} | {format_float(row.mean)} | "
                f"{format_float(row.stdev)} | {format_float(row.cv_pct)} | "
                f"{format_float(row.minimum)} | {format_float(row.maximum)} | {row.unit} |"
            )
    return "\n".join(lines) + "\n"


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Report mean/stdev/CV across multiple BrewFS perf-run artifacts."
    )
    parser.add_argument("artifacts", nargs="+", type=pathlib.Path, help="perf-run artifact directories")
    parser.add_argument(
        "--format",
        choices=("markdown", "tsv"),
        default="markdown",
        help="Output format (default: markdown)",
    )
    parser.add_argument(
        "--min-samples",
        type=int,
        default=2,
        help="Only report metrics present in at least this many artifacts (default: 2)",
    )
    parser.add_argument(
        "--kinds",
        default="summary,fio,runtime,drain,stats,amplification",
        help="Comma-separated metric kinds to include, or 'all' (default: common perf kinds)",
    )
    parser.add_argument(
        "--cv-warn-pct",
        type=float,
        default=10.0,
        help="Variance section threshold in percent (default: 10)",
    )
    parser.add_argument(
        "--top-unstable",
        type=int,
        default=25,
        help="Maximum rows in the high-variance section (default: 25)",
    )
    parser.add_argument("-o", "--output", type=pathlib.Path, help="Write output to a file")
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    if args.min_samples < 1:
        compare_artifacts.die("--min-samples must be >= 1")
    include_kinds = None
    if args.kinds != "all":
        include_kinds = {kind.strip() for kind in args.kinds.split(",") if kind.strip()}
    runs = load_runs(args.artifacts)
    rows = build_rows(runs, args.min_samples, include_kinds)
    if not rows:
        compare_artifacts.die("no numeric metrics had enough samples")

    artifacts = [artifact for artifact, _ in runs]
    if args.format == "tsv":
        output = emit_tsv(rows)
    else:
        output = emit_markdown(rows, artifacts, args.cv_warn_pct, args.top_unstable)

    if args.output:
        args.output.write_text(output)
    else:
        print(output, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
