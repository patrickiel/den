---
name: release
description: Release den — build, sign and publish a new version to GitHub Releases via scripts/release.ts.
argument-hint: "[major|minor|patch] [dry-run]"
disable-model-invocation: true
allowed-tools: Bash(node scripts/release.ts:*)
---

Run the release script from the repo root. It does everything (bump, notes, build, sign, tag, push, publish); don't do any of those steps by hand.

Arguments: `$ARGUMENTS`

1. Build the command: `node scripts/release.ts --yes`, plus `--bump <level>` if the arguments name major, minor or patch, plus `--dry-run` if they say dry run. Run exactly that, alone, with the Bash tool and `run_in_background: true` (the release build takes several minutes), then wait for it to finish.
2. Report the result in a few lines: the version, the release link, and the notes it printed. If it failed, quote the `✗` line. The script restores its files on failure and prints any remaining steps; don't retry or work around it, tell the user.
