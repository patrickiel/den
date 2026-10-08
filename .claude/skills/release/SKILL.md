---
name: release
description: Release den — bump, notes, tag and a draft release via scripts/release.ts; GitHub Actions builds both platforms and publishes it.
argument-hint: "[major|minor|patch] [dry-run]"
disable-model-invocation: true
allowed-tools: Bash(node scripts/release.ts:*)
---

Run the release script from the repo root. It does the local part (bump, notes, commit, tag, push, draft release); the `release` workflow (`.github/workflows/release.yml`) then builds and signs the Windows installer and the Mac app, adds them with `latest.json` to the release and publishes it. Don't do any of those steps by hand.

Arguments: `$ARGUMENTS`

1. Build the command: `node scripts/release.ts --yes`, plus `--bump <level>` if the arguments name major, minor or patch, plus `--dry-run` if they say dry run. Run exactly that, alone, with the Bash tool and `run_in_background: true`, then wait for it to finish.
2. Report the result in a few lines: the version, the notes it printed, and the two links it ends with (the workflow run, which takes some ten minutes, and the release page, a draft until the workflow publishes it). If it failed, quote the `✗` line and the remaining steps it printed; don't retry or work around it, tell the user.
3. If asked how the build went, check the run with `gh run list --workflow release.yml --limit 1` and `gh run view <id>`; a failed run can be repeated from the Actions tab (Run workflow, with the tag).
