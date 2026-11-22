import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { expectedVersion } from "../real.ts";

test("campaigns require the CLI version this checkout's workspace declares", async () => {
  const manifest = readFileSync(
    new URL("../../Cargo.toml", import.meta.url),
    "utf8",
  );
  const version = /^\[workspace\.package\][^[]*?^version = "([^"]+)"/ms.exec(
    manifest,
  )![1];
  expect(await expectedVersion()).toBe(`git-twig ${version}`);
});
