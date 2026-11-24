export type Platform = "mac" | "linux" | "windows";

interface NavigatorLike {
  userAgent: string;
  userAgentData?: { platform?: string };
}

/** The visitor's desktop OS, so the chart opens on numbers that apply to them. */
export function detectPlatform(navigator: NavigatorLike): Platform {
  const hint = navigator.userAgentData?.platform?.toLowerCase() ?? "";
  if (hint.includes("windows")) return "windows";
  if (hint.includes("linux") || hint.includes("chrome os")) return "linux";
  if (hint.includes("mac")) return "mac";
  const agent = navigator.userAgent;
  // Phones match "Linux" (Android) or "Mac OS X" (iOS) but run no worktrees.
  if (/Android|iPhone|iPad|iPod/.test(agent)) return "mac";
  if (/Windows/.test(agent)) return "windows";
  if (/Linux|X11|CrOS/.test(agent)) return "linux";
  return "mac";
}

/** The chart's largest saving in whole percent, rounded down so it never overstates. */
export function largestSaving(rows: { gitBytes: number; twigBytes: number }[]): number {
  const savings = rows
    .filter((row) => row.gitBytes > 0)
    .map((row) => (row.gitBytes - row.twigBytes) / row.gitBytes);
  return savings.length === 0 ? 0 : Math.floor(Math.max(...savings) * 100);
}
