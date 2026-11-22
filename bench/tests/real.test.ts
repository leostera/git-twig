import { expect, test } from "bun:test";
import { parseInputs, parseReal, summarizeReal } from "../lib/real-results.ts";
function fixture(host = false) {
  const input = {
    label: "repo",
    path: "/input",
    revision: "a".repeat(40),
    worktrees: 1,
    imageGiB: 8,
    hostTiming: true,
  };
  const space = {
    stable: true,
    allocatedBytes: 4096,
    totalBytes: 1000000,
    observedAllocatedBytes: [4096],
  };
  const raw = {
    schemaVersion: 4,
    dirty: false,
    commit: "runner",
    candidateSha256: "binary",
    filesystem: host ? "host" : "linux-xfs-image",
    concurrency: 1,
    worktreesPerCohort: 1,
    repeats: 3,
    warmup: true,
    requireNative: true,
    selectedBackend: null,
    failures: [],
    repositories: [
      { label: "repo", commit: input.revision, donor_commit: null },
    ],
    cohorts: [0, 1, 2].flatMap((repeat) =>
      ["git", "twig"].map((backend) => ({
        repository: "repo",
        backend,
        repeat,
        sourceCommit: input.revision,
        targetCommit: input.revision,
        sourceTree: "b".repeat(40),
        targetTree: "b".repeat(40),
        succeeded: 1,
        failed: 0,
        sourceUnchanged: true,
        privateWriteTested: true,
        trackedFiles: 2,
        verifiedFiles: 2,
        createWallMs: 10,
        createsPerSecond: 100,
        harnessCpuMsDuringCreate: 1,
        logicalBytesPerWorktree: 100,
        before: host ? null : structuredClone(space),
        afterCreate: host ? null : structuredClone(space),
        afterStatus: host ? null : structuredClone(space),
        afterPrivateWrites: host ? null : structuredClone(space),
        growthAfterStatusBytes: host ? null : -4096,
        operations: [
          {
            worker: 0,
            verified: true,
            exitCode: 0,
            error: null,
            nativeCow: backend === "twig",
            verifiedClones: backend === "twig" ? 2 : null,
            createMs: 10,
            firstStatusMs: 1,
          },
        ],
      })),
    ),
  };
  return {
    raw,
    expected: {
      commit: "runner",
      sha256: "binary",
      filesystem: raw.filesystem,
      input,
      rounds: 3,
    },
  };
}
test("real inputs require pinned unique repositories and explicit protocol counts", () => {
  const { expected } = fixture();
  expect(
    parseInputs({ schemaVersion: 1, repositories: [expected.input] }),
  ).toEqual([expected.input]);
  const drive = { ...expected.input, path: "C:\\twig-bench\\inputs\\repo" };
  expect(parseInputs({ schemaVersion: 1, repositories: [drive] })).toEqual([
    drive,
  ]);
  for (const input of [
    { ...expected.input, revision: "HEAD" },
    { ...expected.input, worktrees: 0 },
    { ...expected.input, path: "relative" },
    { ...expected.input, path: "C:relative" },
    { ...expected.input, label: "../repo" },
  ])
    expect(() =>
      parseInputs({ schemaVersion: 1, repositories: [input] }),
    ).toThrow();
  expect(() =>
    parseInputs({
      schemaVersion: 1,
      repositories: [expected.input, expected.input],
    }),
  ).toThrow();
});
test("real gate certifies complete pairs, preserves negatives and keeps host allocation null", () => {
  for (const host of [false, true]) {
    const { raw, expected } = fixture(host),
      cohorts = parseReal(raw, expected),
      rows = summarizeReal(cohorts);
    expect(rows.length).toBe(2);
    expect(rows[0]!.allocationMedianBytes).toBe(host ? null : -4096);
    expect(rows[0]!.allocationComparable).toBe(!host);
  }
  const { raw, expected } = fixture();
  raw.cohorts[0]!.afterStatus!.stable = false;
  expect(summarizeReal(parseReal(raw, expected))[0]!.allocationComparable).toBe(
    false,
  );
});
test("real gate rejects wrong versions, missing rounds, corruption, fallback and provenance changes", () => {
  const mutations: ((r: any) => void)[] = [
    (r) => (r.dirty = true),
    (r) => (r.candidateSha256 = "wrong"),
    (r) => (r.commit = "wrong"),
    (r) => (r.concurrency = 2),
    (r) => (r.warmup = false),
    (r) => r.cohorts.pop(),
    (r) => (r.cohorts[1] = r.cohorts[0]),
    (r) => (r.cohorts[0].sourceUnchanged = false),
    (r) => (r.cohorts[0].verifiedFiles = 0),
    (r) => (r.cohorts[0].privateWriteTested = false),
    (r) => (r.cohorts[0].sourceTree = "wrong"),
    (r) => (r.cohorts[0].operations[0].verified = false),
    (r) => (r.cohorts[1].operations[0].nativeCow = false),
    (r) => (r.cohorts[1].operations[0].verifiedClones = 0),
    (r) => (r.cohorts[0].operations[0].error = "bad bytes"),
    (r) => (r.failures = ["failure"]),
    (r) => (r.cohorts[0].afterStatus.observedAllocatedBytes = ["not bytes"]),
  ];
  for (const mutation of mutations) {
    const { raw, expected } = fixture();
    mutation(raw);
    expect(() => parseReal(raw, expected)).toThrow();
  }
});
test("ReFS Dev Drives compare file-data clusters, not volume free space", () => {
  const { raw, expected } = fixture();
  raw.filesystem = "windows-dev-drive";
  expected.filesystem = "windows-dev-drive";
  for (const cohort of raw.cohorts) {
    for (const key of [
      "before",
      "afterCreate",
      "afterStatus",
      "afterPrivateWrites",
    ] as const)
      Object.assign(cohort[key]!, { fileDataBytes: 8192 });
    Object.assign(cohort, {
      fileDataGrowthAfterStatusBytes: cohort.backend === "git" ? 12288 : 4096,
    });
    // ReFS free space keeps moving; a sample that never settled is expected.
    cohort.afterStatus!.stable = false;
  }
  const rows = summarizeReal(parseReal(raw, expected));
  expect(
    rows.map((r) => [
      r.backend,
      r.allocationMedianBytes,
      r.allocationComparable,
    ]),
  ).toEqual([
    ["git", 12288, true],
    ["twig", 4096, true],
  ]);
  delete (raw.cohorts[3]!.before as Record<string, unknown>).fileDataBytes;
  expect(() => parseReal(raw, expected)).toThrow("file-data clusters");
});
test("a Dev Drive whose files are all resident has no file-data clusters", () => {
  const { raw, expected } = fixture();
  raw.filesystem = "windows-dev-drive";
  expected.filesystem = "windows-dev-drive";
  // ReFS keeps small files in its metadata, so a tiny checkout allocates none.
  for (const cohort of raw.cohorts) {
    for (const key of [
      "before",
      "afterCreate",
      "afterStatus",
      "afterPrivateWrites",
    ] as const)
      Object.assign(cohort[key]!, { fileDataBytes: 0 });
    Object.assign(cohort, { fileDataGrowthAfterStatusBytes: 0 });
  }
  const rows = summarizeReal(parseReal(raw, expected));
  expect(rows.map((r) => r.allocationMedianBytes)).toEqual([0, 0]);
  (raw.cohorts[0]!.before as Record<string, unknown>).fileDataBytes = -4096;
  expect(() => parseReal(raw, expected)).toThrow("file-data clusters");
});
