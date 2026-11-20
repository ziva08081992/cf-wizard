"""Run the frozen SourceFacts gate with fresh offline control/candidate processes."""
import argparse
import json
import math
import os
from pathlib import Path
import shutil
import statistics
import subprocess


TEST = "engine::facts_performance::frozen_source_facts_pipeline"


def control_copy(baseline, candidate, destination):
    shutil.copytree(
        baseline, destination, ignore=shutil.ignore_patterns(".git", "target", "__pycache__")
    )
    module = Path("crates/infrastructure/src")
    shutil.copyfile(candidate / module / "facts_performance.rs", destination / module / "facts_performance.rs")
    with (destination / module / "engine.rs").open("a", encoding="utf-8") as engine:
        engine.write('\n#[cfg(test)]\n#[path = "facts_performance.rs"]\nmod facts_performance;\n')
    reader = destination / module / "source_reader.rs"
    before = """                let node_ids: Vec<_> = (0..view.xml.elements.len())
                    .map(|id| format!("{object_key}/{}", view.address(id)))
                    .collect();"""
    after = """                let build_addresses = || (0..view.xml.elements.len())
                    .map(|id| format!("{object_key}/{}", view.address(id)))
                    .collect::<Vec<_>>();
                #[cfg(test)]
                let node_ids = profile::value(
                    "document.addresses", "document.cached", bytes.len(), None, build_addresses,
                );
                #[cfg(not(test))]
                let node_ids = build_addresses();"""
    contents = reader.read_text(encoding="utf-8")
    if contents.count(before) != 1:
        raise ValueError("The baseline address builder no longer matches the audited loop.")
    reader.write_text(contents.replace(before, after), encoding="utf-8")


def execute(binary, fixture, receipt=None):
    environment = os.environ.copy()
    environment["CF_FACTS_BENCH_DIR"] = str(fixture.resolve())
    environment.pop("CF_FACTS_BENCH_GENERATE", None)
    environment.pop("CF_FACTS_BENCH_RECEIPT", None)
    if receipt is None:
        environment["CF_FACTS_BENCH_GENERATE"] = "1"
    else:
        environment["CF_FACTS_BENCH_RECEIPT"] = str(receipt.resolve())
    subprocess.run(
        [str(binary.resolve()), "--exact", "--ignored", "--nocapture", TEST],
        env=environment,
        check=True,
    )
    expected = fixture / "manifest.json" if receipt is None else receipt
    return json.loads(expected.read_text(encoding="utf-8"))


def summary(values):
    ordered = sorted(values)
    return {
        "median": statistics.median(ordered),
        "p95": ordered[math.ceil(len(ordered) * 0.95) - 1],
        "max": ordered[-1],
    }


def compare(args):
    args.out.mkdir(parents=True, exist_ok=False)
    manifest = json.loads((args.fixture / "manifest.json").read_text(encoding="utf-8"))
    samples = {"baseline": [], "candidate": []}
    programs = {"baseline": args.baseline, "candidate": args.candidate}
    all_records = []
    for iteration in range(args.pairs + 1):
        order = list(programs)
        if iteration % 2 == 0:
            order.reverse()
        for label in order:
            path = args.out / f"{iteration:02}-{label}.json"
            current = execute(programs[label], args.fixture, path)
            if current["fixture"] != manifest or current["peak_working_set_bytes"] is None:
                raise ValueError(f"Frozen input or peak working-set evidence is missing: {path}")
            profile = current["profile"]
            if (
                profile["truncated"]
                or profile["counter_overflow"]
                or profile["metrics"]["facts.inflated_bytes"]["items"] < manifest["inflated_bytes"]
            ):
                raise ValueError(f"The pipeline did not record the complete frozen workload: {path}")
            all_records.append(current)
            if iteration:
                samples[label].append(current)

    semantic_parity = len({row["semantic_sha256"] for row in all_records}) == 1
    per_variant = {
        label: {
            key: summary([record[key] for record in rows])
            for key in ["pipeline_ns", "peak_working_set_bytes", "encode_ns"]
        }
        for label, rows in samples.items()
    }
    components = (
        "facts.lookup", "facts.decode", "facts.inflate", "facts.json",
        "facts.validate.codec", "facts.structural.cached", "document.cached", "document.addresses",
    )
    for label, rows in samples.items():
        per_variant[label]["component_inclusive_ns"] = {
            stage: summary([record["profile"]["metrics"][stage]["inclusive_ns"] for record in rows])
            for stage in components
        }
    baseline = per_variant["baseline"]
    candidate = per_variant["candidate"]
    pipeline_ratio = candidate["pipeline_ns"]["median"] / baseline["pipeline_ns"]["median"]
    old_peak = baseline["peak_working_set_bytes"]["max"]
    memory_limit = old_peak + max(16 * 1024 * 1024, old_peak * 0.1)
    memory_pass = candidate["peak_working_set_bytes"]["max"] <= memory_limit
    receipt = {
        "pairs": args.pairs,
        "warmups_per_variant": 1,
        "fixture": manifest,
        "statistics": per_variant,
        "pipeline_improvement_pct": 100 * (1 - pipeline_ratio),
        "pipeline_floor_pct": args.floor,
        "peak_working_set_limit_bytes": memory_limit,
        "semantic_sha256": all_records[0]["semantic_sha256"],
        "semantic_parity": semantic_parity,
        "memory_pass": memory_pass,
        "fixture_gate_pass": semantic_parity and memory_pass and pipeline_ratio <= 1 - args.floor / 100,
    }
    (args.out / "report.json").write_text(json.dumps(receipt, indent=2), encoding="utf-8")
    print(json.dumps(receipt, indent=2))
    return receipt["fixture_gate_pass"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    subcommands = parser.add_subparsers(dest="command", required=True)
    control = subcommands.add_parser("prepare-control")
    for arg in ("baseline", "candidate", "destination"):
        control.add_argument(arg, type=Path)
    freeze = subcommands.add_parser("freeze")
    freeze.add_argument("baseline", type=Path)
    freeze.add_argument("fixture", type=Path)
    run = subcommands.add_parser("compare")
    for arg in ("baseline", "candidate", "fixture", "out"):
        run.add_argument(arg, type=Path)
    run.add_argument("--pairs", type=int, default=7)
    run.add_argument("--floor", type=float, default=10)
    args = parser.parse_args()
    if args.command == "prepare-control":
        control_copy(args.baseline, args.candidate, args.destination)
    elif args.command == "freeze":
        if args.fixture.exists():
            raise FileExistsError(args.fixture)
        execute(args.baseline, args.fixture)
    else:
        if args.pairs < 1 or not 0 <= args.floor <= 100:
            run.error("Specify at least one pair and a percentage floor between 0 and 100.")
        if not compare(args):
            raise SystemExit(2)


if __name__ == "__main__":
    main()
