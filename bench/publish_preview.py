#!/usr/bin/env python3
"""Derive public v0.5 preview data and its summary table in bench/preview.md from frozen campaign snapshots."""
import argparse
import hashlib
import json
import math
from pathlib import Path
from coverage_snapshot import summaries

ROOT = Path(__file__).resolve().parent.parent
PROFILES = [
    ("cross-revision", "Cross-revision"),
    ("mixed-builtins", "Builtin conversions"),
    ("auto-crlf", "Auto CRLF"),
    ("submodules", "Submodules"),
    ("byte-heavy", "Unchanged bytes"),
    ("file-heavy", "4,096 small files"),
]
BACKENDS = ("git", "baseline", "candidate")


def require(condition, message):
    if not condition:
        raise ValueError(message)


def audit(snapshot):
    manifest = snapshot["manifest"]
    require(snapshot["schemaVersion"] == 1, "unsupported snapshot schema")
    require(not manifest["dirty"] and not manifest["exploratory"], "unfrozen campaign")
    require(manifest["status"] in ("completed", "failed", "stopped"), "campaign still running")
    require(manifest["config"]["rounds"] >= 3, "need at least three paired rounds")
    require(manifest["baseline"]["sourceCommit"] != manifest["candidate"]["sourceCommit"],
            "baseline and candidate must be distinct")
    runs = {run["name"]: run["result"] for run in snapshot["runs"]}
    require(len(runs) == len(snapshot["runs"]), "duplicate result name")
    creates = 0
    for point in manifest["points"]:
        if not point["verified"]:
            continue
        result = runs[point["result"]]
        require(result and result["schemaVersion"] == 4 and not result["dirty"],
                "invalid verified result")
        identity = manifest["baseline" if point["backend"] == "baseline" else "candidate"]
        require(result["commit"] == manifest["commit"] and
                result["candidateSha256"] == identity["sha256"], "source/binary identity differs")
        require(not result["failures"] and len(result["cohorts"]) == 1, "failed verified result")
        cohort = result["cohorts"][0]
        require(cohort["sourceUnchanged"] and cohort["privateWriteTested"],
                "source/private-write checks missing")
        require(cohort["failed"] == 0 and cohort["succeeded"] == point["worktrees"] and
                len(cohort["operations"]) == point["worktrees"], "incomplete verified cohort")
        require(cohort["backend"] == ("git" if point["backend"] == "git" else "twig"),
                "backend identity differs")
        for operation in cohort["operations"]:
            require(operation["verified"] and operation["exitCode"] == 0 and
                    operation["error"] is None, "unverified operation")
            if point["backend"] == "candidate":
                require(operation["nativeCow"], "candidate did not use native CoW")
        creates += cohort["succeeded"]
    return creates


def dataset(entry, snapshot, digest):
    creates = audit(snapshot)
    manifest = snapshot["manifest"]
    rows = list(summaries(snapshot))
    sequential = [row for row in rows if row["concurrency"] == 1]
    require({row["filesystem"] for row in rows} == {manifest["config"]["target"]["filesystem"]},
            "mixed filesystems in one dataset")
    selected = {(row["profile"], row["backend"]): row for row in sequential}
    require(len(selected) == len(sequential), "ambiguous sequential batch sizes")
    profiles = []
    for profile, label in PROFILES:
        measurements = []
        for backend in BACKENDS:
            row = selected.get((profile, backend))
            if not row or not row["pairedGroupComplete"]:
                measurements = None
                break
            require(row["worktrees"] == 2, "public chart expects two-worktree batches")
            values = (row["batchCreateMedianMs"], row["allocationMedianBytes"],
                      row["allocationMinBytes"], row["allocationMaxBytes"])
            require(all(math.isfinite(v) for v in values) and values[0] > 0,
                    "invalid metric")
            measurements.append(dict(backend=backend, timeMs=values[0], bytes=values[1],
                                     minBytes=values[2], maxBytes=values[3]))
        profiles.append(dict(profile=profile, label=label, measurements=measurements))
    points = manifest["points"]
    return dict(label=entry["label"], target=manifest["config"]["target"]["name"],
                snapshot=entry["snapshot"], snapshotSHA256=digest,
                candidate=manifest["candidate"], baseline=manifest["baseline"],
                environment=manifest["config"]["environment"],
                status=manifest["status"], verifiedCreates=creates,
                failedPoints=sum(not p["verified"] for p in points),
                unstablePoints=sum(p["verified"] and not p["allocationStable"] for p in points),
                rounds=manifest["config"]["rounds"], worktrees=2, concurrency=1,
                profiles=profiles)


def build(root=ROOT):
    sources = json.loads((root / "bench/results/preview-sources.json").read_text())
    require(sources["schemaVersion"] == 1, "unsupported source index")
    datasets = []
    for entry in sources["datasets"]:
        path = (root / entry["snapshot"]).resolve()
        require(path.is_relative_to((root / "bench/results").resolve()), "snapshot outside results")
        raw = path.read_bytes()
        datasets.append(dataset(entry, json.loads(raw), hashlib.sha256(raw).hexdigest()))
    require(len({d["target"] for d in datasets}) == len(datasets), "duplicate target")
    require(len({d["candidate"]["sourceCommit"] for d in datasets}) == 1,
            "do not combine different candidate source revisions")
    return dict(schemaVersion=1, kind="twig-checkout-preview", datasets=datasets)


def disk(value):
    for unit, size in (("GiB", 2**30), ("MiB", 2**20), ("KiB", 2**10)):
        if abs(value) >= size:
            return f"{value / size:.2f} {unit}"
    return f"{value:.0f} B"


def duration(value):
    return f"{value / 1000:.2f}s" if value >= 1000 else f"{value:.0f}ms"


def markdown(data):
    default = data["datasets"][0]
    lines = ["| Workload · APFS | Stock Git | Twig v0.4 | v0.5 candidate |",
             "|---|---|---|---|"]
    for profile in default["profiles"]:
        values = profile["measurements"]
        cells = ([f'{duration(v["timeMs"])} / {disk(v["bytes"])}' for v in values]
                 if values else ["Not compared (incomplete or unstable)"] * 3)
        lines.append("| " + " | ".join([profile["label"], *cells]) + " |")
    lines += ["", f'Each cell is **creation time / additional disk allocation**. Medians of {default["rounds"]}',
              "two-worktree batches, one create at a time, on warm-cache filesystem images.",
              "Synthetic workloads, not real-repository or universal speed claims. Allocation",
              "includes Git metadata after first status; it is not exclusive file space reclaimed.",
              "", f'Across all levels: {sum(d["verifiedCreates"] for d in data["datasets"])} certified creates; '
              f'{sum(d["failedPoints"] for d in data["datasets"])} failed points and '
              f'{sum(d["unstablePoints"] for d in data["datasets"])} unstable observation retained.',
              "", "Other filesystems: " + " · ".join(
                  f'[{d["label"]}]({str(Path(d["snapshot"]).with_name("comparison.svg"))})'
                  for d in data["datasets"][1:]) + ".",
              "Speed varies by workload. [Full results and limitations](bench/preview.md)."]
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    data = build()
    output = json.dumps(data, indent=2, allow_nan=False) + "\n"
    path = ROOT / "bench/results/preview.json"
    readme = ROOT / "bench/preview.md"
    text = readme.read_text()
    start, end = "<!-- twig-preview:start -->", "<!-- twig-preview:end -->"
    require(text.count(start) == 1 and text.count(end) == 1, "missing unique preview markers")
    before, rest = text.split(start)
    _, after = rest.split(end)
    # Generated links are repository-relative; this page lives in bench/.
    table = markdown(data).replace("](bench/preview.md)", "](#setup-and-measurements)").replace("](bench/", "](")
    rendered = before + start + "\n\n" + table + "\n" + end + after
    if args.check:
        require(path.read_text() == output, "preview JSON is stale")
        require(text == rendered, "preview table in bench/preview.md is stale")
    else:
        path.write_text(output)
        readme.write_text(rendered)


if __name__ == "__main__":
    main()
