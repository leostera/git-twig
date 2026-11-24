import { readFileSync } from "node:fs";
import { z } from "zod";

export interface DiskComparison {
  name: string;
  worktrees: number;
  concurrency: number;
  rounds: number;
  gitBytes: number;
  twigBytes: number;
  source: string;
}
/** README and site both read this index of committed raw runs. */
const indexFile = "readme.json";
/** A commit containing every raw run the index names, for stable source links. */
const resultsCommit = "6741c210de7edfbdcbc4c348dde3ccd4cb2c9544";
const platformKeys = {
  "macOS · APFS": "mac", "Linux · XFS": "xfs", "Linux · Btrfs": "btrfs", "Windows · Dev Drive": "windows",
} as const;
type PlatformKey = (typeof platformKeys)[keyof typeof platformKeys];
export const benchmarkFile = (file: string) => new URL(`../../../bench/results/${file}`, import.meta.url);
const read = (file: string) => JSON.parse(readFileSync(benchmarkFile(file), "utf8"));
const index = z.object({
  platforms: z.array(z.object({
    name: z.string(),
    entries: z.array(z.object({ repository: z.string(), allocation: z.string().startsWith("bench/results/") })),
  })),
});
const relative = (path: string) => path.slice("bench/results/".length);
export const sources = [{ file: indexFile }, ...index.parse(read(indexFile)).platforms
  .flatMap(p => p.entries.map(e => ({ file: relative(e.allocation) })))];
const names = [
  ["linux", "Linux kernel"], ["pulumi", "Pulumi docs"], ["openclaw", "OpenClaw"],
  ["axo", "Axo"], ["twig", "git-twig"], ["workers-starter", "Workers starter"],
] as const;
const snapshot = z.object({
  stable: z.boolean(),
  allocatedBytes: z.number().finite(),
  observedAllocatedBytes: z.array(z.number().finite()).min(3),
  fileDataBytes: z.number().finite().nonnegative().optional(),
});
const suite = z.object({
  schemaVersion: z.literal(4), dirty: z.literal(false), failures: z.array(z.unknown()).length(0),
  filesystem: z.string(),
  candidateSha256: z.string(), concurrency: z.literal(1), worktreesPerCohort: z.number().int().positive(),
  allocationSampling: z.object({ minimumSamples: z.number().int().positive(), stableSamples: z.number().int().positive() }).optional(),
  cohorts: z.array(z.object({
    repository: z.string(), sourceCommit: z.string(), targetCommit: z.string(),
    backend: z.enum(["git", "twig"]), repeat: z.number().int(),
    sourceUnchanged: z.literal(true), privateWriteTested: z.literal(true),
    succeeded: z.number().int().positive(), failed: z.literal(0),
    before: snapshot, afterStatus: snapshot, growthAfterStatusBytes: z.number().finite(),
    fileDataGrowthAfterStatusBytes: z.number().finite().nonnegative().optional(),
    operations: z.array(z.object({
      verified: z.literal(true), exitCode: z.literal(0), error: z.null(), nativeCow: z.boolean(),
    })),
  })).length(6),
});
const median = (values: number[]) => [...values].sort((a,b) => a-b)[1]!;
function comparison(file: string, repository: string, name: string): DiskComparison {
  const run = suite.parse(read(file));
  const pairs = new Set(run.cohorts.map(c => c.backend + ":" + c.repeat));
  if (!["git:0","git:1","git:2","twig:0","twig:1","twig:2"].every(p => pairs.has(p))) throw new Error("Missing paired round");
  const count = run.worktreesPerCohort;
  // ReFS volume totals drift, so Windows runs compare the file data files reference.
  const windows = run.filesystem === "windows-dev-drive";
  const space = (c: (typeof run.cohorts)[number]) => {
    if (!windows) return c.growthAfterStatusBytes;
    if (c.fileDataGrowthAfterStatusBytes === undefined || c.before.fileDataBytes === undefined ||
        c.afterStatus.fileDataBytes === undefined) throw new Error("Missing Windows file data");
    if (c.fileDataGrowthAfterStatusBytes !== c.afterStatus.fileDataBytes - c.before.fileDataBytes) throw new Error("Delta differs");
    return c.fileDataGrowthAfterStatusBytes;
  };
  for (const c of run.cohorts) {
    if (c.repository !== repository || c.sourceCommit !== c.targetCommit || c.sourceCommit !== run.cohorts[0]!.sourceCommit ||
        c.succeeded !== count || c.operations.length !== count) throw new Error("Repository/cohort differs");
    if (c.backend === "twig" && c.operations.some(o => !o.nativeCow)) throw new Error("Not native CoW");
    if (windows) continue;
    for (const s of [c.before, c.afterStatus]) {
      const n = run.allocationSampling?.stableSamples ?? 3;
      const values = s.observedAllocatedBytes;
      if (!s.stable || values.length < (run.allocationSampling?.minimumSamples ?? 3) ||
          new Set(values.slice(-n)).size !== 1) throw new Error("Allocation not settled");
    }
    if (c.growthAfterStatusBytes !== c.afterStatus.allocatedBytes-c.before.allocatedBytes) throw new Error("Delta differs");
  }
  return { name, worktrees: count, concurrency: 1, rounds: 3,
    gitBytes: median(run.cohorts.filter(c=>c.backend==="git").map(space)),
    twigBytes: median(run.cohorts.filter(c=>c.backend==="twig").map(space)),
    source: `https://github.com/leostera/git-twig/blob/${resultsCommit}/bench/results/${file}` };
}
export function loadPlatforms(): Record<PlatformKey, DiskComparison[]> {
  const result: Record<PlatformKey, DiskComparison[]> = { mac: [], xfs: [], btrfs: [], windows: [] };
  const platforms = index.parse(read(indexFile)).platforms;
  for (const [title, key] of Object.entries(platformKeys)) {
    const platform = platforms.find(p => p.name === title);
    if (!platform) throw new Error(`readme.json has no ${title} results`);
    for (const [repository, name] of names) {
      const entry = platform.entries.find(e => e.repository === repository);
      if (!entry) continue;
      const row = comparison(relative(entry.allocation), repository, name);
      // Nothing to compare when neither backend allocates (ReFS keeps small files resident).
      if (row.gitBytes > 0 || row.twigBytes > 0) result[key].push(row);
    }
  }
  return result;
}
export function loadBenchmarks(): DiskComparison[] { return loadPlatforms().mac; }
