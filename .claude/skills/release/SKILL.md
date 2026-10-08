---
name: release
description: Release den — build, sign and publish a new version to GitHub Releases via scripts/release.ts.
argument-hint: "[major|minor|patch] [dry-run]"
disable-model-invocation: true
allowed-tools: Bash(node scripts/release.ts:*)
---

Run the release script from the repo root. It does everything (bump, notes, build, sign, tag, push, publish); don't do any of those steps by hand.

Arguments: `$ARGUMENTS`

1. Build the command: `node scripts/release.ts --yes`, plus `--bump <level>` if the arguments name major, minor or patch, plus `--dry-run` if they say dry run. On a Mac, or if the arguments say attach, the command is `node scripts/release.ts --attach` instead: it adds this Mac's build to the release of the version in Cargo.toml, whose tag must be checked out and already released from Windows. Run exactly that, alone, with the Bash tool and `run_in_background: true` (the release build takes several minutes), then wait for it to finish.
2. Report the result in a few lines: the version, the release link, and the notes it printed. If it failed, quote the `✗` line. The script restores its files on failure and prints any remaining steps; don't retry or work around it, tell the user.
3. After a Windows release, remind the user that the Mac build is still missing from it: on a Mac with the signing key, `git fetch --tags; git checkout v<version>` and `/release attach`. Until then Mac users see the release as not yet built for them.
