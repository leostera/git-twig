import { expect, test } from "bun:test";
import { detectPlatform, largestSaving } from "../src/platform";

const ua = {
  mac: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0 Safari/537.36",
  windows: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0 Safari/537.36",
  linux: "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0 Safari/537.36",
  android: "Mozilla/5.0 (Linux; Android 15; Pixel 9) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0 Mobile Safari/537.36",
  iphone: "Mozilla/5.0 (iPhone; CPU iPhone OS 19_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/19.0 Mobile/15E148 Safari/604.1",
};

test("picks the visitor's operating system, Mac when unknown", () => {
  expect(detectPlatform({ userAgent: ua.mac })).toBe("mac");
  expect(detectPlatform({ userAgent: ua.windows })).toBe("windows");
  expect(detectPlatform({ userAgent: ua.linux })).toBe("linux");
  // Phones run neither Twig nor Git worktrees; show the default.
  expect(detectPlatform({ userAgent: ua.android })).toBe("mac");
  expect(detectPlatform({ userAgent: ua.iphone })).toBe("mac");
  expect(detectPlatform({ userAgent: "" })).toBe("mac");
});

test("prefers client hints over the user agent string", () => {
  expect(detectPlatform({ userAgent: ua.mac, userAgentData: { platform: "Windows" } })).toBe("windows");
  expect(detectPlatform({ userAgent: ua.windows, userAgentData: { platform: "Linux" } })).toBe("linux");
  expect(detectPlatform({ userAgent: ua.windows, userAgentData: { platform: "Chrome OS" } })).toBe("linux");
});

test("the headline saving is the chart's largest, rounded down", () => {
  const rows = [
    { gitBytes: 1000, twigBytes: 6 },   // 99.4%
    { gitBytes: 1000, twigBytes: 500 }, // 50%
  ];
  expect(largestSaving(rows)).toBe(99);
  expect(largestSaving([{ gitBytes: 1000, twigBytes: 42 }])).toBe(95); // 95.8% never reads as 96%
  expect(largestSaving([])).toBe(0);
});
