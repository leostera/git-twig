import { execFileSync } from "node:child_process";
import { readFile, statfs } from "node:fs/promises";
import { delimiter, resolve, join } from "node:path";
import { fileURLToPath } from "node:url";
import { targets } from "./lib/config.ts";
import { environment, execute } from "./lib/process.ts";
import { allocate, digest, invariant, save } from "./lib/results.ts";
import { parseInputs, parseReal, summarizeReal } from "./lib/real-results.ts";

const host = {
  name: "macos-host",
  os: "darwin",
  workerFilesystem: "host",
} as const;
// Fresh expandable VHDX images formatted as ReFS Dev Drives. ReFS volume free
// space moves on its own; space is compared by the clusters files reference.
const windows = {
  name: "windows-dev-drive",
  os: "win32",
  workerFilesystem: "windows-dev-drive",
} as const;
const available = [...targets, host, windows];
const executable = (name: string) =>
  process.platform === "win32" ? name + ".exe" : name;
/// The candidate must be the CLI this checkout's workspace version names.
export async function expectedVersion(): Promise<string> {
  const manifest = await readFile(join(root, "Cargo.toml"), "utf8");
  const version = /^\[workspace\.package\][^[]*?^version = "([^"]+)"/ms.exec(manifest)?.[1];
  invariant(version, "Workspace version not found in Cargo.toml");
  return "git-twig " + version;
}
const root = fileURLToPath(new URL("../", import.meta.url));
function git(args: string[]) {
  return execFileSync("git", args, {
    cwd: root,
    env: environment(),
    encoding: "utf8",
  }).trim();
}
export async function run(args: string[]) {
  const values = new Map<string, string>();
  const allowed = [
    "target",
    "candidate",
    "binary-dir",
    "inputs",
    "environment",
    "candidate-commit",
    "budget-seconds",
    "point-timeout-seconds",
    "minimum-free-gib",
    "max-growth-gib",
  ];
  for (let i = 0; i < args.length; i++) {
    const name = args[i]!;
    invariant(
      name.startsWith("--") &&
        allowed.includes(name.slice(2)) &&
        !values.has(name),
      `Unknown/duplicate ${name}`,
    );
    const value = args[++i];
    invariant(value && !value.startsWith("--"), `Missing ${name}`);
    values.set(name, value);
  }
  const required = (key: string) => {
    const v = values.get("--" + key);
    invariant(v, `Missing --${key}`);
    return v;
  };
  const integer = (key: string, fallback: number, max: number) => {
    const v = values.get("--" + key) ?? String(fallback);
    invariant(
      /^[1-9][0-9]*$/.test(v) &&
        Number.isSafeInteger(Number(v)) &&
        Number(v) <= max,
      `Invalid --${key}`,
    );
    return Number(v);
  };
  const target = available.find((t) => t.name === required("target"));
  invariant(target && target.os === process.platform, "Wrong target OS");
  const bin = resolve(required("binary-dir")),
    candidate = resolve(required("candidate")),
    inputsFile = resolve(required("inputs"));
  const sourceCommit = required("candidate-commit");
  invariant(
    /^[a-f0-9]{40}$/.test(sourceCommit),
    "Invalid candidate source commit",
  );
  const description = required("environment");
  const budgetSeconds = integer("budget-seconds", 7200, 86400),
    pointSeconds = integer("point-timeout-seconds", 1800, 86400),
    minimumFreeGiB = integer("minimum-free-gib", 128, 1048576),
    maxGrowthGiB = integer("max-growth-gib", 160, 1048576);
  const inputs = parseInputs(
    JSON.parse(await readFile(inputsFile, "utf8")),
  ).filter((i) => target !== host || i.hostTiming);
  invariant(inputs.length > 0, "No eligible inputs");
  process.env.PATH = bin + delimiter + process.env.PATH;
  invariant(
    git(["status", "--porcelain"]) === "",
    "Commit the harness before measuring",
  );
  const commit = git(["rev-parse", "HEAD"]),
    worker = join(bin, executable("twig-real-bench"));
  const tools = new Map<string, string>();
  for (const file of [candidate, worker, join(bin, executable("git"))])
    tools.set(file, await digest(file));
  const version = execFileSync(candidate, ["--version"], {
    env: environment(),
    encoding: "utf8",
  }).trim();
  const wanted = await expectedVersion();
  invariant(version === wanted, `This campaign requires ${wanted}, not ${version}`);
  for (const input of inputs) {
    git(["-C", input.path, "cat-file", "-e", input.revision + "^{commit}"]);
  }
  const output = await allocate(
    join(root, "bench/results"),
    tools.get(candidate)!,
    target.name,
  );
  const manifest = {
    schemaVersion: 1,
    kind: "real-repository-campaign",
    generatedAt: new Date().toISOString(),
    commit,
    dirty: false,
    candidate: { version, sourceCommit, sha256: tools.get(candidate) },
    git: {
      version: git(["--version"]),
      sha256: tools.get(join(bin, executable("git"))),
    },
    worker: { sha256: tools.get(worker) },
    config: {
      target: target.name,
      filesystem: target.workerFilesystem,
      rounds: 3,
      concurrency: 1,
      warmup: true,
      budgetSeconds,
      pointSeconds,
      minimumFreeGiB,
      maxGrowthGiB,
      environment: description,
    },
    inputs: inputs.map(({ path: _, ...input }) => input),
    status: "running",
    stopReason: null as string | null,
    resourceSamples: [] as {
      freeGiB: number;
      growthGiB: number;
      elapsedMs: number;
    }[],
    jobs: [] as {
      repository: string;
      file: string;
      status: string;
      verified: boolean;
      allocationComparable: boolean;
      timeoutMs: number;
      error: string | null;
      startedElapsedMs: number;
      finishedElapsedMs: number | null;
      exitCode: number | null;
    }[],
    notes: [
      `${version} built from ${sourceCommit} vs frozen stock Git. Three paired rounds, concurrency one, warmup enabled. Original v0.4 input pins and counts.`,
      "No retries, no automatic exclusion of errors, and no website changes. Unstable allocation observations remain raw data, not comparative allocation claims.",
      "Host timing and filesystem-image allocation are separate environments; never combine them into one cohort. Warm caches on shared hosts; background apps not stopped. No cold-cache, saturation or universal speed claim.",
      "Full independent raw-byte/mode/symlink verification, native-clone checks, source isolation and private writes are mandatory. Verification follows creation timing. Original sources are read-only; applications and dependency scripts are never executed.",
      "Observed nearest-rank percentiles from small finite samples are not population tail estimates. CLI creation and subsequent first-status latency are reported separately.",
    ],
  };
  const start = performance.now(),
    controller = new AbortController();
  const interrupt = () => controller.abort();
  process.on("SIGINT", interrupt);
  process.on("SIGTERM", interrupt);
  const initial = await statfs(root),
    initialFreeGiB = (initial.bavail * initial.bsize) / 2 ** 30;
  async function persist() {
    await save(join(output, "manifest.json"), manifest);
  }
  async function guard() {
    invariant(!controller.signal.aborted, "Campaign interrupted");
    invariant(
      performance.now() - start < budgetSeconds * 1000,
      "Total campaign budget exhausted",
    );
    const disk = await statfs(root),
      freeGiB = (disk.bavail * disk.bsize) / 2 ** 30,
      growthGiB = initialFreeGiB - freeGiB;
    manifest.resourceSamples.push({
      freeGiB,
      growthGiB,
      elapsedMs: performance.now() - start,
    });
    invariant(
      freeGiB >= minimumFreeGiB && growthGiB <= maxGrowthGiB,
      "Disk budget exhausted",
    );
    invariant(
      git(["rev-parse", "HEAD"]) === commit &&
        git(["status", "--porcelain"]) === "",
      "Harness changed",
    );
    for (const [file, hash] of tools)
      invariant(
        (await digest(file)) === hash,
        `Frozen executable changed: ${file}`,
      );
  }
  console.log("results:", output);
  await persist();
  try {
    for (const input of inputs) {
      await guard();
      const file = input.label + ".json",
        startedElapsedMs = performance.now() - start;
      const timeoutMs = Math.min(
        pointSeconds * 1000,
        budgetSeconds * 1000 - startedElapsedMs,
      );
      const job = {
        repository: input.label,
        file,
        status: "running",
        verified: false,
        allocationComparable: false,
        timeoutMs,
        error: null as string | null,
        startedElapsedMs,
        finishedElapsedMs: null as number | null,
        exitCode: null as number | null,
      };
      manifest.jobs.push(job);
      await persist();
      console.log("START", target.name, input.label);
      try {
        const command = [
          worker,
          "--repo",
          input.label + "=" + input.path,
          "--revision",
          input.label + "=" + input.revision,
          "--candidate",
          candidate,
          "--filesystem",
          target.workerFilesystem,
          "--image-gib",
          String(input.imageGiB),
          "--concurrency",
          "1",
          "--worktrees",
          String(input.worktrees),
          "--repeats",
          "3",
          "--warmup",
          "true",
          "--require-native",
          "true",
          "--output",
          join(output, file),
          "--note",
          description,
        ];
        const outcome = await execute(command, {
          cwd: root,
          stdout: join(output, file + ".stdout.log"),
          stderr: join(output, file + ".stderr.log"),
          timeoutMs,
          deadlineMs: start + budgetSeconds * 1000,
          signal: controller.signal,
        });
        job.exitCode = outcome.exitCode;
        job.timeoutMs = outcome.timeoutMs;
        invariant(
          outcome.termination === "exited" && outcome.exitCode === 0,
          `Worker ${outcome.termination}; exit ${outcome.exitCode}; no retry`,
        );
        await guard();
        const cohorts = parseReal(
          JSON.parse(await readFile(join(output, file), "utf8")),
          {
            commit,
            sha256: tools.get(candidate)!,
            filesystem: target.workerFilesystem,
            input,
            rounds: 3,
            longAllocationWindow: true,
          },
        );
        const summary = summarizeReal(cohorts);
        await save(join(output, input.label + ".summary.json"), {
          schemaVersion: 1,
          repository: input.label,
          filesystem: target.workerFilesystem,
          summary,
        });
        job.verified = true;
        job.allocationComparable = cohorts.every((c) => c.allocationStable);
        invariant(
          target.workerFilesystem === "host" || job.allocationComparable,
          "Allocation did not settle; retain this complete attempt and rerun the entire paired repository job, never select individual cohorts",
        );
        job.status = "completed";
      } catch (error) {
        job.error = String(error);
        job.status = "failed";
        throw error;
      } finally {
        job.finishedElapsedMs = performance.now() - start;
        await persist();
        console.log(job.status, target.name, input.label);
      }
    }
    manifest.status = "completed";
  } catch (error) {
    manifest.status = controller.signal.aborted ? "interrupted" : "failed";
    manifest.stopReason = String(error);
  } finally {
    await persist();
    process.off("SIGINT", interrupt);
    process.off("SIGTERM", interrupt);
  }
  console.log(manifest.status, join(output, "manifest.json"));
  return manifest.status === "completed" ? 0 : 1;
}
if (import.meta.main) {
  try {
    process.exitCode = await run(process.argv.slice(2));
  } catch (error) {
    console.error(error);
    process.exitCode = 1;
  }
}
