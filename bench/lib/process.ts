import { spawn, spawnSync } from "node:child_process";
import { open } from "node:fs/promises";

// A fresh environment per child: no inherited Git context or injected configuration.
export function environment(
  parent: NodeJS.ProcessEnv = process.env,
): NodeJS.ProcessEnv {
  const env = { ...parent };
  for (const key of Object.keys(env)) {
    if (/^GIT_/i.test(key)) delete env[key];
  }
  return {
    ...env,
    GIT_CONFIG_NOSYSTEM: "1",
    GIT_CONFIG_GLOBAL: process.platform === "win32" ? "NUL" : "/dev/null",
    GIT_ATTR_NOSYSTEM: "1",
    GIT_TEMPLATE_DIR: "",
    GIT_DEFAULT_HASH: "sha1",
    XDG_CONFIG_HOME: "/nonexistent/twig-bench",
    GIT_TERMINAL_PROMPT: "0",
    LC_ALL: "C",
    TWIG_TRACE: "1",
  };
}

export interface ProcessOutcome {
  exitCode: number | null;
  signal: NodeJS.Signals | null;
  termination: "exited" | "timeout" | "interrupted";
  timeoutMs: number;
}

export async function execute(
  command: string[],
  options: {
    cwd: string;
    stdout: string;
    stderr: string;
    timeoutMs: number;
    signal?: AbortSignal;
    graceMs?: number;
    deadlineMs?: number;
  },
): Promise<ProcessOutcome> {
  const stdout = await open(options.stdout, "wx");
  let stderr;
  try {
    stderr = await open(options.stderr, "wx");
    const child = spawn(command[0]!, command.slice(1), {
      cwd: options.cwd,
      env: environment(),
      detached: true,
      stdio: ["ignore", stdout.fd, stderr.fd],
    });
    let termination: ProcessOutcome["termination"] = "exited";
    let escalation: Promise<void> | undefined;
    function kill(signal: NodeJS.Signals) {
      if (!child.pid) return;
      // Windows has no process groups; end the whole tree instead.
      if (process.platform === "win32") {
        spawnSync("taskkill", ["/PID", String(child.pid), "/T", "/F"], { stdio: "ignore" });
        return;
      }
      try {
        process.kill(-child.pid, signal);
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error;
      }
    }
    function stop(reason: "timeout" | "interrupted") {
      if (termination !== "exited") return;
      termination = reason;
      kill("SIGTERM");
      // Keep escalation alive even if the leader exits but a descendant ignores TERM.
      escalation = new Promise((resolve) =>
        setTimeout(() => {
          kill("SIGKILL");
          resolve();
        }, options.graceMs ?? 15000),
      );
    }
    const timeoutMs = Math.max(
      1,
      Math.min(
        options.timeoutMs,
        (options.deadlineMs ?? Infinity) - performance.now(),
      ),
    );
    const timer = setTimeout(() => stop("timeout"), timeoutMs);
    const abort = () => stop("interrupted");
    options.signal?.addEventListener("abort", abort, { once: true });
    if (options.signal?.aborted) abort();
    try {
      const result = await new Promise<{
        exitCode: number | null;
        signal: NodeJS.Signals | null;
      }>((resolve, reject) => {
        child.once("error", reject);
        child.once("close", (exitCode, signal) =>
          resolve({ exitCode, signal }),
        );
      });
      if (escalation) await escalation;
      else kill("SIGKILL"); // A finished leader must not leave owned background writers.
      return { ...result, termination, timeoutMs };
    } finally {
      clearTimeout(timer);
      options.signal?.removeEventListener("abort", abort);
    }
  } finally {
    await stdout.close();
    await stderr?.close();
  }
}
