import { expect, test } from "bun:test";
import { loadBenchmarks, loadPlatforms } from "../scripts/benchmarks";

const repositories = [
  "Linux kernel", "Pulumi docs", "OpenClaw", "Axo", "git-twig", "Workers starter",
];

test("uses verified checked-in allocation measurements", () => {
  const data = loadBenchmarks();
  expect(data.map(d => d.name)).toEqual(repositories);
  expect(data[0]!.gitBytes).toBeGreaterThan(data[0]!.twigBytes);
  expect(data.every(d => d.source.startsWith("https://github.com/leostera/git-twig/blob/"))).toBe(true);
});

test("provides complete paired real-repository comparisons for every platform", () => {
  const platforms = loadPlatforms();
  expect(Object.keys(platforms)).toEqual(["mac", "xfs", "btrfs", "windows"]);
  for (const key of ["mac", "xfs", "btrfs"] as const) {
    const rows = platforms[key];
    expect(rows.map(d => d.name)).toEqual(repositories);
    expect(rows.map(d => d.worktrees)).toEqual([1, 1, 1, 1, 10, 10]);
  }
  for (const rows of Object.values(platforms))
    expect(rows.every(d => d.gitBytes > 0 && d.twigBytes > 0 && d.rounds === 3 && d.concurrency === 1)).toBe(true);
});

test("Windows shows the repositories with file data to compare", () => {
  // No kernel (case-colliding paths); Workers starter's files are all resident in ReFS metadata.
  expect(loadPlatforms().windows.map(d => d.name)).toEqual(["Pulumi docs", "OpenClaw", "Axo", "git-twig"]);
});

test("retains the audited absolute allocation values", () => {
  const data = loadPlatforms();
  expect(data.mac[0]!.gitBytes).toBe(1967841280);
  expect(data.mac[0]!.twigBytes).toBe(45477888);
  expect(data.xfs[0]!.gitBytes).toBe(1982631936);
  expect(data.xfs[0]!.twigBytes).toBe(74010624);
  expect(data.btrfs[0]!.gitBytes).toBe(1979482112);
  expect(data.btrfs[0]!.twigBytes).toBe(178974720);
  expect(data.windows[0]!.gitBytes).toBe(1280544768);
  expect(data.windows[0]!.twigBytes).toBe(53325824);
});
