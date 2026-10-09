// Build an extension and side-load it into den, which picks it up at its next start:
//   node scripts/sideload.ts examples/hello-extension
// `--dev` loads it into a debug build of den, which keeps its own folder (`den-dev`). Without a folder it offers the repository's extensions. It goes in as `<id>.pending`, as an
// install from the Extensions view does, so den can stay open (a loaded library stays locked
// until den exits). Node runs the TypeScript directly (Node 22.18+).

import { execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { homedir } from "node:os";
import { basename, dirname, join } from "node:path";
import { createInterface } from "node:readline/promises";

const ROOT = join(import.meta.dirname, "..");
const DEV = process.argv.includes("--dev");
const ARGS = process.argv.slice(2).filter((a) => a !== "--dev");

/** den's data folder, as src/settings.rs picks it (a debug build's with `--dev`). */
function dataDir(): string {
  const name = DEV ? "den-dev" : "den";
  if (process.platform === "win32") return join(process.env.APPDATA ?? "", name);
  if (process.platform === "darwin") return join(homedir(), "Library", "Application Support", name);
  return join(process.env.XDG_CONFIG_HOME ?? join(homedir(), ".config"), name);
}

/** The library's file name, as Cargo names a cdylib crate called `id`. */
function libraryName(id: string): string {
  const name = id.replace(/-/g, "_");
  if (process.platform === "win32") return `${name}.dll`;
  return `lib${name}.${process.platform === "darwin" ? "dylib" : "so"}`;
}

/** The repository's extensions: folders two levels down holding an extension.json. */
function repositoryExtensions(): string[] {
  const found: string[] = [];
  for (const group of readdirSync(ROOT, { withFileTypes: true }).filter((d) => d.isDirectory())) {
    for (const dir of readdirSync(join(ROOT, group.name), { withFileTypes: true }).filter((d) => d.isDirectory())) {
      if (existsSync(join(ROOT, group.name, dir.name, "extension.json"))) found.push(join(group.name, dir.name));
    }
  }
  return found;
}

async function pickFolder(): Promise<string> {
  const found = repositoryExtensions();
  found.forEach((f, i) => console.log(`  ${i + 1}) ${f}`));
  const rl = createInterface({ input: process.stdin, output: process.stdout });
  const answer = (await rl.question("Extension (number, or a folder path): ")).trim().replace(/^"|"$/g, "");
  rl.close();
  const n = Number(answer);
  return Number.isInteger(n) && n >= 1 && n <= found.length ? join(ROOT, found[n - 1]) : answer;
}

async function main(): Promise<void> {
  const folder = ARGS[0] ?? (await pickFolder());
  const manifestPath = join(folder, "extension.json");
  if (!existsSync(manifestPath)) throw new Error(`No extension.json in ${folder}`);
  const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  const cargoToml = join(folder, "Cargo.toml");

  execFileSync("cargo", ["build", "--release", "--manifest-path", cargoToml], { stdio: "inherit" });
  const metadata = JSON.parse(execFileSync("cargo", ["metadata", "--format-version", "1", "--no-deps", "--manifest-path", cargoToml], { encoding: "utf8" }));
  const library = join(metadata.target_directory, "release", libraryName(manifest.id));

  const pending = join(dataDir(), "extensions", `${manifest.id}.pending`);
  rmSync(pending, { recursive: true, force: true });
  mkdirSync(pending, { recursive: true });
  cpSync(manifestPath, join(pending, "extension.json"));
  cpSync(library, join(pending, basename(library)));
  if (existsSync(join(folder, "README.md"))) cpSync(join(folder, "README.md"), join(pending, "README.md"));
  if (manifest.icon) {
    mkdirSync(dirname(join(pending, manifest.icon)), { recursive: true });
    cpSync(join(folder, manifest.icon), join(pending, manifest.icon));
  }
  console.log(`Staged ${manifest.id}. Restart den to load it; see ${join(dataDir(), "extensions.log")}`);
}

main().catch((e) => {
  console.error(`\n✗ ${e instanceof Error ? e.message : e}`);
  process.exit(1);
});
