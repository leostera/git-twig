import { expect, test } from "bun:test";
import { plugin } from "bun";
import { readFile } from "node:fs/promises";
import { z } from "zod";
plugin({
  name: "raw markdown",
  setup(build) {
    build.onLoad({ filter: /SKILL\.md(?:\?raw)?$/ }, async (args) => ({
      contents: `export default ${JSON.stringify(await readFile(args.path.replace(/\?raw$/, ""), "utf8"))}`,
      loader: "js",
    }));
  },
});
const { default: app } = await import("../src/worker");
for (const protocol of ["http", "https"]) {
  test(`${protocol} root serves the website without redirecting or returning agent instructions`, async () => {
    const html = await readFile(new URL("../index.html", import.meta.url), "utf8");
    const url = `${protocol}://twig.leostera.dev/`;
    let assetUrl: string | undefined;
    const response = await app.request(url, {}, {
      ASSETS: {
        fetch: async (request: Request) => {
          assetUrl = request.url;
          return new Response(html, { headers: { "Content-Type": "text/html" } });
        },
      },
    });
    expect(assetUrl).toBe(url);
    expect(response.status).toBe(200);
    expect(response.headers.get("Location")).toBeNull();
    expect(response.headers.get("content-type")).toContain("text/html");
    const body = await response.text();
    expect(body).toContain('<div id="root"></div>');
    expect(body).not.toContain('rel="alternate"');
    expect(body).not.toContain('href="/llms.txt"');
  });
}

test("onboarding is plain text with real install and usage instructions", async () => {
  const response = await app.request("/llms.txt");
  expect(response.status).toBe(200);
  expect(response.headers.get("content-type")).toContain("text/plain");
  const text = await response.text();
  const manifest = z.object({
    workspace: z.object({ package: z.object({ version: z.string() }) }),
  }).parse(Bun.TOML.parse(await readFile(new URL("../../../Cargo.toml", import.meta.url), "utf8")));
  expect(text).toContain(`Version ${manifest.workspace.package.version} is published`);
  expect(text).toContain("brew install leostera/git-twig/git-twig");
  expect(text).toContain("cargo install git-twig --locked");
  expect(text).toContain("scoop bucket add git-twig https://github.com/leostera/git-twig");
  expect(text).toContain("scoop install git-twig/git-twig");
  expect(text).toContain("npm install -g git-twig");
  expect(text).not.toContain("no tagged binary release");
  expect(text).toContain("## CLI reference");
  for (const command of [
    "new", "list", "remove", "move", "lock", "unlock", "prune", "repair",
    "skill", "-h", "--version",
  ]) {
    expect(text).toContain(`git twig ${command}`);
  }
  expect(text).toContain("It does not repair file contents or merge conflicts.");
  expect(text).toContain("It does not lock files");
  expect(text).toContain("git -C main twig new -b feat/x ../feat-x HEAD");
  expect(text).toContain("git twig new --help");
  expect(text).toContain("twigs/staging");
  expect(text).not.toContain("git twig --skill");
  expect(text).not.toContain("add spelling remains");
  expect(text).toContain("Ask before removing");
  expect(text).not.toContain("/Users/");
});
