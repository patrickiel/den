// Release: Claude reads the changes since the last `v*` tag, picks the semver bump
// by scripts/release-scale.md and writes the release notes. The script then bumps the version in
// Cargo.toml, builds den, packs the per-user NSIS installer (packaging/installer.nsi), signs it
// with the updater key, writes the updater manifest (latest.json), commits, tags, pushes and
// publishes a GitHub release that installed copies update from.
//
//   node scripts/release.ts [--dry-run] [--yes] [--bump major|minor|patch]
//
// Node runs the TypeScript directly (type stripping, Node 22.18+), so there is nothing to install.
// --dry-run stops after showing the proposal; --yes skips the question (needed without a terminal,
// e.g. from Claude Code); --bump overrides Claude's level. The first release (no tag yet) publishes
// the current version.
//
// Signing key: DEN_SIGNING_KEY (path), else ~/.keys/den.key; its password (if any) in
// DEN_SIGNING_KEY_PASSWORD. Make one with
//   pnpm dlx @tauri-apps/cli signer generate -w %USERPROFILE%\.keys\den.key
// Its public half goes into packaging/updater.pub (done here on the first release), which den
// builds in to check downloads. Global TAURI_SIGNING_* variables are ignored: they may belong to
// another app. NSIS comes from Tauri's tool cache (%LOCALAPPDATA%\tauri\NSIS) or PATH.

import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline/promises";
import { parseArgs } from "node:util";

const REPO = "patrickiel/den";
const BRANCH = "main";
const ROOT = join(import.meta.dirname, "..");
/** Cargo.toml has the version; cargo updates the lockfile's copy during the build. */
const RELEASE_FILES = ["Cargo.toml", "Cargo.lock", "packaging/updater.pub"];
const OUT_DIR = join(ROOT, "target", "release");
const PUB_FILE = join(ROOT, "packaging", "updater.pub");
/** Diff text for Claude is cut here; the log and the stat always go in full. */
const DIFF_LIMIT = 150_000;
const BUMPS = ["major", "minor", "patch"] as const;

type Bump = (typeof BUMPS)[number];
type Env = Record<string, string | undefined>;
interface Tools {
  key: string;
  nsis: string;
}
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

/** Run a command with its output on the terminal. `shell` for .cmd shims such as pnpm. */
function run(cmd: string, args: string[], opts: { shell?: boolean; env?: Env } = {}): void {
  console.log(`\n> ${cmd} ${args.join(" ")}`);
  const r = opts.shell
    ? spawnSync([cmd, ...args].map((a) => (/\s/.test(a) ? `"${a}"` : a)).join(" "), { cwd: ROOT, stdio: "inherit", env: opts.env, shell: true })
    : spawnSync(cmd, args, { cwd: ROOT, stdio: "inherit", env: opts.env });
  if (r.status !== 0) throw new Error(`${cmd} ${args[0]} failed${r.status === null ? "" : ` (exit ${r.status})`}`);
}

/**
 * Add the saved user and machine PATH. A shell started before PATH last changed (one an IDE
 * opened, say) misses newer entries, such as the ones for claude and pnpm.
 */
function refreshPath(): void {
  const saved = tryOut("powershell", [
    "-NoProfile", "-Command",
    '[Environment]::GetEnvironmentVariable("PATH", "User") + ";" + [Environment]::GetEnvironmentVariable("PATH", "Machine")',
  ]);
  if (saved) process.env.PATH = `${process.env.PATH};${saved}`;
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

/** The [package] version line, the first `version =` in Cargo.toml. */
function setVersion(version: string): void {
  const path = join(ROOT, "Cargo.toml");
  const text = readFileSync(path, "utf8");
  writeFileSync(path, text.replace(/^version = "[^"]*"/m, `version = "${version}"`));
}

function makensis(): string {
  const cached = join(process.env.LOCALAPPDATA ?? "", "tauri", "NSIS", "Bin", "makensis.exe");
  if (existsSync(cached)) return cached;
  if (tryOut("makensis", ["/VERSION"]) !== null) return "makensis";
  fail("NSIS (makensis) not found: install NSIS, or build any Tauri app once (it caches NSIS in %LOCALAPPDATA%\\tauri).");
}

/** Tauri's signer, through pnpm dlx. */
function signer(): { cmd: string; args: string[] } {
  return { cmd: "pnpm", args: ["dlx", "@tauri-apps/cli@2"] };
}

// ---------- steps ----------

function preflight(dryRun: boolean): Tools {
  const branch = out("git", ["rev-parse", "--abbrev-ref", "HEAD"]);
  if (branch !== BRANCH) fail(`Releases are made from ${BRANCH}; this is ${branch}.`);
  if (!dryRun && out("git", ["status", "--porcelain"])) fail("The working tree has changes: commit or stash them first.");
  out("git", ["fetch", "--quiet", "origin", BRANCH]);
  const behind = Number(out("git", ["rev-list", "--count", `HEAD..origin/${BRANCH}`]));
  if (behind > 0) fail(`${BRANCH} is ${behind} commit(s) behind origin/${BRANCH}: pull first.`);
  if (tryOut("gh", ["auth", "status"]) === null) fail("The GitHub CLI is not logged in: run `gh auth login`.");
  if (tryOut("claude", ["--version"]) === null) fail("The `claude` CLI is not on PATH (it picks the bump and writes the notes).");
  const key = process.env.DEN_SIGNING_KEY ?? join(homedir(), ".keys", "den.key");
  if (!existsSync(key)) {
    fail(`Signing key not found: ${key}. Make one with:\n  pnpm dlx @tauri-apps/cli signer generate -w "${join(homedir(), ".keys", "den.key")}"`);
  }
  // The public half is built into den; the first release puts it there.
  const pub = existsSync(PUB_FILE) ? readFileSync(PUB_FILE, "utf8").trim() : "";
  const keyPub = existsSync(`${key}.pub`) ? readFileSync(`${key}.pub`, "utf8").trim() : "";
  if (!pub) {
    if (!keyPub) fail(`${key}.pub not found: packaging/updater.pub needs the public key.`);
    if (!dryRun) writeFileSync(PUB_FILE, keyPub + "\n");
  } else if (keyPub && keyPub !== pub) {
    fail("packaging/updater.pub is not the public half of the signing key: installed copies would refuse the update.");
  }
  return { key, nsis: makensis() };
}

/** The prompt for Claude: the scale, the version and what changed since `lastTag`. */
function changesPrompt(version: string, lastTag: string | null, log: string): string {
  const scale = readFileSync(join(ROOT, "scripts/release-scale.md"), "utf8");
  const parts = [
    "Decide the version bump for the next release of den (a native Windows desktop app: a project terminal with",
    "split panes, editor, browser, explorer, search and source control) and write its release notes,",
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

/** Build den, pack and sign the installer; on failure put the release files back. Returns its name. */
function build(version: string, { key, nsis }: Tools): string {
  const setup = `den_${version}_x64-setup.exe`;
  try {
    run("cargo", ["build", "--release"]);
    run(nsis, ["-V2", `-DVERSION=${version}`, `-DEXE=${join(OUT_DIR, "den.exe")}`, `-DOUTFILE=${join(OUT_DIR, setup)}`, join(ROOT, "packaging", "installer.nsi")]);
    const { cmd, args } = signer();
    // `--password=` keeps the password one argument: the pnpm shim drops an empty one (no password).
    run(cmd, [...args, "signer", "sign", "-f", key, `--password=${process.env.DEN_SIGNING_KEY_PASSWORD ?? ""}`, join(OUT_DIR, setup)], {
      shell: true,
      env: { ...process.env, TAURI_SIGNING_PRIVATE_KEY: undefined, TAURI_SIGNING_PRIVATE_KEY_PATH: undefined, TAURI_SIGNING_PRIVATE_KEY_PASSWORD: undefined },
    });
  } catch (e) {
    out("git", ["checkout", "--", ...RELEASE_FILES]);
    fail(`${(e as Error).message}. The release files are restored.`);
  }
  return setup;
}

/** The updater manifest next to the installer, in Tauri's format (src/update.rs reads it). */
function writeManifest(version: string, notes: string, setup: string): string {
  const path = join(OUT_DIR, "latest.json");
  const manifest = {
    version,
    notes,
    pub_date: new Date().toISOString(),
    platforms: {
      "windows-x86_64": {
        signature: readFileSync(`${join(OUT_DIR, setup)}.sig`, "utf8").trim(),
        url: `https://github.com/${REPO}/releases/download/v${version}/${setup}`,
      },
    },
  };
  writeFileSync(path, JSON.stringify(manifest, null, 2) + "\n");
  return path;
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
  const tools = preflight(dryRun);

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

  if (version !== current) setVersion(version);
  const setup = build(version, tools);
  const manifest = writeManifest(version, proposal.notes, setup);
  const notesFile = join(mkdtempSync(join(tmpdir(), "den-release-")), "notes.md");
  writeFileSync(notesFile, proposal.notes + "\n");
  const ghArgs = [
    "release", "create", tag, join(OUT_DIR, setup), manifest,
    "--repo", REPO, "--title", `den ${tag}`, "--notes-file", notesFile, "--latest", "--verify-tag",
  ];

  try {
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
  console.log(`\n✓ den ${tag} released: https://github.com/${REPO}/releases/tag/${tag}`);
}

main().catch((e) => fail(e instanceof Error ? e.message : String(e)));
