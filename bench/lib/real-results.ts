import { invariant, list, record } from "./results.ts";

export interface RealInput {
  label: string;
  path: string;
  revision: string;
  worktrees: number;
  imageGiB: number;
  hostTiming: boolean;
}
export function parseInputs(value: unknown): RealInput[] {
  const input = record(value);
  invariant(input.schemaVersion === 1, "Unsupported input catalog");
  const rows = list(input.repositories).map((value) => {
    const r = record(value);
    invariant(
      typeof r.label === "string" && /^[a-z][a-z0-9-]{0,63}$/.test(r.label),
      "Invalid repository label",
    );
    invariant(
      typeof r.path === "string" && /^(\/|[A-Za-z]:[\\/])/.test(r.path),
      "Input path must be absolute",
    );
    invariant(
      typeof r.revision === "string" && /^[a-f0-9]{40}$/.test(r.revision),
      "Pin a full input commit",
    );
    invariant(
      Number.isSafeInteger(r.worktrees) &&
        Number(r.worktrees) > 0 &&
        Number(r.worktrees) <= 128,
      "Invalid worktree count",
    );
    invariant(
      Number.isSafeInteger(r.imageGiB) &&
        Number(r.imageGiB) >= 4 &&
        Number(r.imageGiB) <= 256,
      "Invalid image capacity",
    );
    invariant(
      typeof r.hostTiming === "boolean",
      "Declare host timing eligibility",
    );
    return {
      label: r.label,
      path: r.path,
      revision: r.revision,
      worktrees: Number(r.worktrees),
      imageGiB: Number(r.imageGiB),
      hostTiming: r.hostTiming,
    };
  });
  invariant(
    rows.length > 0 && new Set(rows.map((r) => r.label)).size === rows.length,
    "Empty or duplicate catalog",
  );
  return rows;
}
function scalar(r: Record<string, unknown>, key: string): number {
  const n = r[key];
  invariant(
    typeof n === "number" && Number.isFinite(n) && n >= 0,
    `Invalid ${key}`,
  );
  return n;
}
export interface VerifiedCohort {
  backend: "git" | "twig";
  repeat: number;
  createWallMs: number;
  createsPerSecond: number;
  harnessCpuMs: number;
  logicalBytes: number;
  allocationBytes: number | null;
  allocationStable: boolean;
  operations: { createMs: number; firstStatusMs: number }[];
}
export function parseReal(
  value: unknown,
  expected: {
    commit: string;
    sha256: string;
    filesystem: string;
    input: RealInput;
    rounds: number;
    longAllocationWindow?: boolean;
  },
): VerifiedCohort[] {
  const r = record(value),
    { input, rounds } = expected;
  invariant(
    r.schemaVersion === 4 && r.dirty === false && r.commit === expected.commit,
    "Wrong schema or unfrozen harness",
  );
  invariant(r.candidateSha256 === expected.sha256, "Wrong candidate binary");
  invariant(
    r.filesystem === expected.filesystem &&
      r.concurrency === 1 &&
      r.worktreesPerCohort === input.worktrees &&
      r.repeats === rounds &&
      r.warmup === true &&
      r.requireNative === true &&
      r.selectedBackend === null,
    "Wrong real-repository protocol",
  );
  if (expected.longAllocationWindow && expected.filesystem !== "host") {
    const sampling = record(r.allocationSampling);
    invariant(
      sampling.intervalMs === 500 &&
        sampling.minimumSamples === 20 &&
        sampling.stableSamples === 11 &&
        sampling.maximumSamples === 240,
      "Wrong allocation settlement protocol",
    );
  }
  invariant(list(r.failures).length === 0, "Worker reported failures");
  const repositories = list(r.repositories);
  invariant(
    repositories.length === 1 &&
      record(repositories[0]).label === input.label &&
      record(repositories[0]).commit === input.revision &&
      record(repositories[0]).donor_commit === null,
    "Wrong repository input",
  );
  const seen = new Set<string>();
  const cohorts = list(r.cohorts).map((value): VerifiedCohort => {
    const c = record(value);
    invariant(c.backend === "git" || c.backend === "twig", "Wrong backend");
    invariant(
      Number.isSafeInteger(c.repeat) &&
        Number(c.repeat) >= 0 &&
        Number(c.repeat) < rounds,
      "Wrong repeat",
    );
    const key = `${c.backend}:${c.repeat}`;
    invariant(!seen.has(key), "Duplicate paired cohort");
    seen.add(key);
    invariant(
      c.repository === input.label &&
        c.sourceCommit === input.revision &&
        c.targetCommit === input.revision &&
        typeof c.sourceTree === "string" &&
        /^[a-f0-9]{40}$/.test(c.sourceTree) &&
        c.sourceTree === c.targetTree,
      "Source/target pin mismatch",
    );
    invariant(
      c.succeeded === input.worktrees &&
        c.failed === 0 &&
        c.sourceUnchanged === true &&
        c.privateWriteTested === true,
      "Failed, changed or unprobed cohort",
    );
    invariant(
      Number.isSafeInteger(c.trackedFiles) &&
        Number(c.trackedFiles) > 0 &&
        c.verifiedFiles === Number(c.trackedFiles) * input.worktrees,
      "Missing full raw verification",
    );
    const createWallMs = scalar(c, "createWallMs"),
      createsPerSecond = scalar(c, "createsPerSecond");
    invariant(
      createWallMs > 0 && createsPerSecond > 0,
      "Missing creation timing",
    );
    const workers = new Set<number>();
    const operations = list(c.operations).map((value) => {
      const op = record(value);
      invariant(
        op.verified === true && op.exitCode === 0 && op.error === null,
        "Incorrect operation",
      );
      invariant(
        Number.isSafeInteger(op.worker) &&
          Number(op.worker) >= 0 &&
          Number(op.worker) < input.worktrees &&
          !workers.has(Number(op.worker)),
        "Missing or duplicate worker",
      );
      workers.add(Number(op.worker));
      if (c.backend === "twig")
        invariant(
          op.nativeCow === true &&
            Number.isSafeInteger(op.verifiedClones) &&
            Number(op.verifiedClones) > 0,
          "Missing verified native clones",
        );
      return {
        createMs: scalar(op, "createMs"),
        firstStatusMs: scalar(op, "firstStatusMs"),
      };
    });
    invariant(
      operations.length === input.worktrees,
      "Incomplete operation inventory",
    );
    const host = expected.filesystem === "host",
      refs = expected.filesystem === "windows-dev-drive";
    let allocationStable = !host;
    for (const key of [
      "before",
      "afterCreate",
      "afterStatus",
      "afterPrivateWrites",
    ]) {
      if (host) {
        invariant(c[key] === null, "Host allocation must remain unmeasured");
        continue;
      }
      const space = record(c[key]);
      scalar(space, "allocatedBytes");
      invariant(scalar(space, "totalBytes") > 0, "Invalid filesystem capacity");
      invariant(
        typeof space.stable === "boolean" &&
          list(space.observedAllocatedBytes).length > 0 &&
          list(space.observedAllocatedBytes).every(
            (n) => Number.isSafeInteger(n) && Number(n) >= 0,
          ),
        "Missing allocation observations",
      );
      if (expected.longAllocationWindow) {
        const observations = list(space.observedAllocatedBytes);
        invariant(
          observations.length >= 20 &&
            observations.length <= 240 &&
            observations.at(-1) === space.allocatedBytes,
          "Invalid long-window observations",
        );
        if (space.stable)
          invariant(
            observations.slice(-11).every((n) => n === space.allocatedBytes),
            "False allocation stability",
          );
      }
      if (refs)
        invariant(
          Number.isSafeInteger(space.fileDataBytes) && Number(space.fileDataBytes) > 0,
          "Missing ReFS file-data clusters",
        );
      allocationStable &&= space.stable;
    }
    // ReFS volume free space moves on its own even after a flush. The clusters
    // files reference are exact, so they are the comparison; volume samples stay
    // as raw observations.
    if (refs) allocationStable = true;
    const allocation = refs
      ? c.fileDataGrowthAfterStatusBytes
      : c.growthAfterStatusBytes;
    invariant(
      host
        ? allocation === null
        : typeof allocation === "number" && Number.isSafeInteger(allocation),
      "Invalid allocation delta",
    );
    return {
      backend: c.backend,
      repeat: Number(c.repeat),
      createWallMs,
      createsPerSecond,
      harnessCpuMs: scalar(c, "harnessCpuMsDuringCreate"),
      logicalBytes: scalar(c, "logicalBytesPerWorktree"),
      allocationBytes: host ? null : Number(allocation),
      allocationStable,
      operations,
    };
  });
  invariant(cohorts.length === rounds * 2, "Missing paired cohorts");
  return cohorts;
}
function median(values: number[]): number {
  const rows = [...values].sort((a, b) => a - b),
    middle = Math.floor(rows.length / 2);
  return rows.length % 2
    ? rows[middle]!
    : (rows[middle - 1]! + rows[middle]!) / 2;
}
function percentile(values: number[], p: number): number {
  return [...values].sort((a, b) => a - b)[
    Math.max(0, Math.ceil(values.length * p) - 1)
  ]!;
}
export function summarizeReal(cohorts: VerifiedCohort[]) {
  const allocationComparable = cohorts.every((c) => c.allocationStable);
  return (["git", "twig"] as const).map((backend) => {
    const rows = cohorts.filter((c) => c.backend === backend),
      operations = rows.flatMap((c) => c.operations),
      times = operations.map((o) => o.createMs);
    return {
      backend,
      rounds: rows.length,
      operations: operations.length,
      allocationComparable,
      batchCreateMedianMs: median(rows.map((c) => c.createWallMs)),
      createsPerSecondMedian: median(rows.map((c) => c.createsPerSecond)),
      logicalBytesPerSecondMedian: median(
        rows.map(
          (c) => (c.logicalBytes * c.operations.length * 1000) / c.createWallMs,
        ),
      ),
      createP50Ms: percentile(times, 0.5),
      createP95Ms: percentile(times, 0.95),
      createP99Ms: percentile(times, 0.99),
      createMaxMs: Math.max(...times),
      firstStatusP50Ms: median(operations.map((o) => o.firstStatusMs)),
      allocationMedianBytes: rows.every((c) => c.allocationBytes !== null)
        ? median(rows.map((c) => c.allocationBytes!))
        : null,
      allocationMinBytes: rows.every((c) => c.allocationBytes !== null)
        ? Math.min(...rows.map((c) => c.allocationBytes!))
        : null,
      allocationMaxBytes: rows.every((c) => c.allocationBytes !== null)
        ? Math.max(...rows.map((c) => c.allocationBytes!))
        : null,
      maxHarnessCpuWallRatio: Math.max(
        ...rows.map((c) => c.harnessCpuMs / c.createWallMs),
      ),
    };
  });
}
