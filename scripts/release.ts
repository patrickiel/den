// Release: Claude reads the changes since the last `v*` tag, picks the semver bump by
// scripts/release-scale.md and writes the release notes. The script bumps the version in
// Cargo.toml (and the lockfile), commits, tags, pushes and makes a draft GitHub release with the
// notes. The tag starts .github/workflows/release.yml, which builds and signs the Windows
// installer and the Mac app, adds them and latest.json (the updater's manifest) to the release
// and publishes it.
//
//   node scripts/release.ts [--dry-run] [--yes] [--bump major|minor|patch]
//
// Node runs the TypeScript directly (type stripping, Node 22.18+), so there is nothing to install.
// --dry-run stops after showing the proposal; --yes skips the question (needed without a terminal,
// e.g. from Claude Code); --bump overrides Claude's level. The first release (no tag yet) publishes
// the current version.
//
// The updater's signing key lives in the repository's secrets (DEN_SIGNING_KEY, and
// DEN_SIGNING_KEY_PASSWORD if it has one); its public half is packaging/updater.pub, which den
// builds in to check downloads. Make a pair with
//   pnpm dlx @tauri-apps/cli signer generate -w ~/.keys/den.key

import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline/promises";
import { parseArgs } from "node:util";

const REPO = "patrickiel/den";
const BRANCH = "main";
const ROOT = join(import.meta.dirname, "..");
/** Cargo.toml has the version; `cargo update --workspace` carries it into the lockfile. */
const RELEASE_FILES = ["Cargo.toml", "Cargo.lock"];
const PUB_FILE = join(ROOT, "packaging", "updater.pub");
/** Diff text for Claude is cut here; the log and the stat always go in full. */
const DIFF_LIMIT = 150_000;
const BUMPS = ["major", "minor", "patch"] as const;

type Bump = (typeof BUMPS)[number];
interface Proposal {
  bump: Bump;
  reason: string;
  notes: string;
}

const SCHEMA = {
  type: "object",
  properties: {
    bump: { type: "string", enum: BUMPS },
    reason: { type: "string" },
    notes: { type: "string" },
  },
  required: ["bump", "reason", "notes"],
  additionalProperties: false,
};

// ---------- helpers ----------

function fail(text: string): never {
  console.error(`\n✗ ${text}`);
  process.exit(1);
}

function isBump(value: unknown): value is Bump {
  return (BUMPS as readonly unknown[]).includes(value);
}

/** Run a program in the repo and return its trimmed stdout; throws with its stderr on failure. */
function out(cmd: string, args: string[], input?: string): string {
  return execFileSync(cmd, args, {
    cwd: ROOT,
    encoding: "utf8",
    input,
    maxBuffer: 256 * 1024 * 1024,
    stdio: [input === undefined ? "ignore" : "pipe", "pipe", "pipe"],
  }).trim();
}

/** `out`, or null when the program fails (a missing tag, a missing tool). */
function tryOut(cmd: string, args: string[]): string | null {
  try {
    return out(cmd, args);
  } catch {
    return null;
  }
}

/** Run a command with its output on the terminal. */
function run(cmd: string, args: string[]): void {
  console.log(`\n> ${cmd} ${args.join(" ")}`);
  const r = spawnSync(cmd, args, { cwd: ROOT, stdio: "inherit" });
  if (r.status !== 0) throw new Error(`${cmd} ${args[0]} failed${r.status === null ? "" : ` (exit ${r.status})`}`);
}

/**
 * On Windows, add the saved user and machine PATH. A shell started before PATH last changed (one
 * an IDE opened, say) misses newer entries, such as the ones for claude and gh.
 */
function refreshPath(): void {
  if (process.platform !== "win32") return;
  const saved = tryOut("powershell", [
    "-NoProfile", "-Command",
    '[Environment]::GetEnvironmentVariable("PATH", "User") + ";" + [Environment]::GetEnvironmentVariable("PATH", "Machine")',
  ]);
  if (!saved) return;
  // Skip entries already present: cmd stops resolving commands once PATH passes 8191 characters.
  const seen = new Set<string>();
  process.env.PATH = `${process.env.PATH};${saved}`
    .split(";")
    .filter((p) => {
      const k = p.trim().replace(/\\+$/, "").toLowerCase();
      if (!k || seen.has(k)) return false;
      seen.add(k);
      return true;
    })
    .join(";");
}

function currentVersion(): string {
  const m = /^version = "([^"]*)"/m.exec(readFileSync(join(ROOT, "Cargo.toml"), "utf8"));
  if (!m) fail("No version line in Cargo.toml.");
  return m[1];
}

function bumpVersion(version: string, bump: Bump): string {
  const [major, minor, patch] = version.split(".").map(Number);
  if ([major, minor, patch].some((n) => !Number.isInteger(n))) fail(`Cannot bump version "${version}".`);
  return bump === "major" ? `${major + 1}.0.0` : bump === "minor" ? `${major}.${minor + 1}.0` : `${major}.${minor}.${patch + 1}`;
}

/** The [package] version line, the first `version =` in Cargo.toml, and the lockfile's copy. */
function setVersion(version: string): void {
  const path = join(ROOT, "Cargo.toml");
  const text = readFileSync(path, "utf8");
  writeFileSync(path, text.replace(/^version = "[^"]*"/m, `version = "${version}"`));
  run("cargo", ["update", "--workspace", "--offline"]);
}

// ---------- steps ----------

function preflight(dryRun: boolean): void {
  const branch = out("git", ["rev-parse", "--abbrev-ref", "HEAD"]);
  if (branch !== BRANCH) fail(`Releases are made from ${BRANCH}; this is ${branch}.`);
  if (!dryRun && out("git", ["status", "--porcelain"])) fail("The working tree has changes: commit or stash them first.");
  out("git", ["fetch", "--quiet", "origin", BRANCH]);
  const behind = Number(out("git", ["rev-list", "--count", `HEAD..origin/${BRANCH}`]));
  if (behind > 0) fail(`${BRANCH} is ${behind} commit(s) behind origin/${BRANCH}: pull first.`);
  if (tryOut("gh", ["auth", "status"]) === null) fail("The GitHub CLI is not logged in: run `gh auth login`.");
  if (tryOut("claude", ["--version"]) === null) fail("The `claude` CLI is not on PATH (it picks the bump and writes the notes).");
  // den builds the public key in; the workflow signs with the private half from the secrets.
  if (!readFileSync(PUB_FILE, "utf8").trim()) fail("packaging/updater.pub is empty: put the public half of the signing key there.");
}

/** The prompt for Claude: the scale, the version and what changed since `lastTag`. */
function changesPrompt(version: string, lastTag: string | null, log: string): string {
  const scale = readFileSync(join(ROOT, "scripts/release-scale.md"), "utf8");
  const parts = [
    "Decide the version bump for the next release of den (a native desktop app for Windows and macOS: a project",
    "terminal with split panes, editor, browser, explorer, search and source control) and write its release notes,",
    "following the scale below. Answer with the structured output only.",
    `\n<scale>\n${scale}\n</scale>`,
    `\nCurrent version: ${version}. ${lastTag ? `Last release: ${lastTag}.` : "There is no earlier release."}`,
    `\n<commits>\n${log}\n</commits>`,
  ];
  if (lastTag) {
    const stat = out("git", ["diff", "--stat", lastTag, "HEAD"]);
    let diff = out("git", ["diff", lastTag, "HEAD", "--", ".", ":(exclude)Cargo.lock"]);
    if (diff.length > DIFF_LIMIT) diff = `${diff.slice(0, DIFF_LIMIT)}\n[… diff cut at ${DIFF_LIMIT} characters]`;
    parts.push(`\n<diffstat>\n${stat}\n</diffstat>`, `\n<diff>\n${diff}\n</diff>`);
  } else {
    parts.push(
      "\nThis is the first release: the bump is ignored, and the notes should introduce den's main features.",
      `\n<readme>\n${readFileSync(join(ROOT, "README.md"), "utf8")}\n</readme>`,
    );
  }
  return parts.join("\n");
}

function askClaude(prompt: string): Proposal {
  console.log("Asking Claude for the bump and the release notes…");
  const raw = out("claude", [
    "-p", "--output-format", "json", "--json-schema", JSON.stringify(SCHEMA),
    "--tools", "", "--no-session-persistence",
    "--system-prompt", "You decide semantic version bumps and write user-facing release notes for a desktop app.",
  ], prompt);
  const res = JSON.parse(raw);
  if (res.is_error) fail(`Claude failed: ${res.result ?? raw}`);
  const p = res.structured_output ?? JSON.parse(res.result);
  if (!isBump(p?.bump) || typeof p.notes !== "string") fail(`Unexpected answer from Claude: ${raw}`);
  return p;
}

/** Enter keeps the level, a level name changes it, anything else cancels. */
async function confirm(bump: Bump): Promise<Bump | null> {
  const rl = createInterface({ input: process.stdin, output: process.stdout });
  const answer = (await rl.question("\nRelease? [Y/n, or major/minor/patch to change the level] ")).trim().toLowerCase();
  rl.close();
  if (answer === "" || answer === "y" || answer === "yes") return bump;
  return isBump(answer) ? answer : null;
}

// ---------- main ----------

async function main(): Promise<void> {
  const { values: opts } = parseArgs({
    options: { "dry-run": { type: "boolean" }, yes: { type: "boolean" }, bump: { type: "string" } },
  });
  const dryRun = opts["dry-run"] ?? false;
  const forced = opts.bump;
  if (forced !== undefined && !isBump(forced)) fail(`--bump takes major, minor or patch, not "${forced}".`);

  refreshPath();
  preflight(dryRun);

  const current = currentVersion();
  const lastTag = tryOut("git", ["describe", "--tags", "--abbrev=0", "--match", "v*"]);
  const log = out("git", ["log", "--no-merges", "--format=- %s%n%b", lastTag ? `${lastTag}..HEAD` : "HEAD"]);
  if (!log) fail(`Nothing to release: no commits since ${lastTag}.`);

  const proposal = askClaude(changesPrompt(current, lastTag, log));
  let bump: Bump = forced ?? proposal.bump;
  // The first release publishes the version the app already has, unless told otherwise.
  const nextVersion = (b: Bump) => (lastTag || forced ? bumpVersion(current, b) : current);

  console.log(`\n${current} → ${nextVersion(bump)}${lastTag ? ` (${bump})` : " (first release)"}`);
  if (lastTag) console.log(`Claude: ${proposal.bump}: ${proposal.reason}${forced ? ` (overridden by --bump ${forced})` : ""}`);
  console.log(`\n${proposal.notes}`);
  if (dryRun) return console.log("\nDry run: nothing changed.");

  if (!opts.yes) {
    if (!process.stdin.isTTY) fail("No terminal to confirm on: pass --yes to release without asking.");
    const answer = await confirm(bump);
    if (!answer) return console.log("Cancelled.");
    bump = answer;
  }
  const version = nextVersion(bump);
  const tag = `v${version}`;
  if (tryOut("git", ["rev-parse", "--quiet", "--verify", `refs/tags/${tag}`]) !== null) fail(`Tag ${tag} already exists.`);

  const notesFile = join(mkdtempSync(join(tmpdir(), "den-release-")), "notes.md");
  writeFileSync(notesFile, proposal.notes + "\n");
  // A draft: the workflow adds the builds and publishes it, so the previous release stays the
  // latest until this one is whole.
  const ghArgs = ["release", "create", tag, "--repo", REPO, "--title", `den ${tag}`, "--notes-file", notesFile, "--draft", "--verify-tag"];

  try {
    if (version !== current) setVersion(version);
    run("git", ["add", "--", ...RELEASE_FILES]);
    if (tryOut("git", ["diff", "--cached", "--quiet"]) === null) run("git", ["commit", "--quiet", "-m", `chore: release ${tag}`]);
    run("git", ["tag", "-a", tag, "-m", `den ${tag}`]);
    run("git", ["push", "origin", BRANCH]);
    run("git", ["push", "origin", tag]);
    run("gh", ghArgs);
  } catch (e) {
    fail(
      `${(e as Error).message}. Steps before it are done; finish with:\n`
      + `  git push origin ${BRANCH} ${tag}\n`
      + `  gh ${ghArgs.map((a) => (/[\s\\]/.test(a) ? `"${a}"` : a)).join(" ")}`,
    );
  }
  console.log(`\n✓ den ${tag} tagged; the release workflow builds and publishes it:`);
  console.log(`  https://github.com/${REPO}/actions/workflows/release.yml`);
  console.log(`  https://github.com/${REPO}/releases/tag/${tag}`);
}

main().catch((e) => fail(e instanceof Error ? e.message : String(e)));
