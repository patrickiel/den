# Next steps: extensions and the index

What's left to take the extension work live. The code is done and tested locally, but nothing is committed or pushed.

## 1. Commit and push den

The index tool (`den-extensions/tools/den-index`) gets the SDK from `github.com/patrickiel/den`. It won't build in CI until `crates/den-extension` is on GitHub.

- [ ] Review the working tree. It includes the extension SDK and its new host methods, the Extensions view, the extension page, the settings, the icons and the index client.
- [ ] Commit and push to `main`.
- [ ] Check that this builds: `cargo build --manifest-path ../den-extensions/tools/den-index/Cargo.toml`

## 2. Publish the index repository

The draft is in `..\den-extensions`.

- [ ] Create `patrickiel/den-extensions` on GitHub. It must be public, because den reads `index.json` from `raw.githubusercontent.com`.
- [ ] Push the draft:
  ```sh
  cd ../den-extensions; git init; git add .; git commit -m "feat: extension index"; git branch -M main; git remote add origin https://github.com/patrickiel/den-extensions.git; git push -u origin main
  ```
- [ ] In the repo settings, under Actions → General → Workflow permissions, allow **Read and write** so `build.yml` can commit `index.json`.
- [ ] Run the **build** workflow once by hand (Actions → build → Run workflow) and check it passes.
- [ ] Optional: protect `main` so that changes come through pull requests with the **check** workflow passing.

Until this repo exists, den shows an error under **Available**. Installed extensions, the `owner/repo` box and side-loading keep working.

## 3. Seed the index (optional)

So that **Available** isn't empty on day one:

- [ ] Move `examples/task-buttons` into its own repo, `patrickiel/task-buttons`:
  - copy the folder;
  - change the dependency to `den-extension = { git = "https://github.com/patrickiel/den" }`;
  - add `"repository": "patrickiel/task-buttons"` to `extension.json`.
- [ ] Tag `v0.1.0` there. `release.yml` makes the release with the zip, the README and the icon.
- [ ] Do the same for `workspace-stats` if you want it listed too.
- [ ] Open a PR to `den-extensions` adding `{ "repo": "patrickiel/task-buttons" }` to `extensions.json`, and check that the **check** workflow passes.
- [ ] Merge it, then check that `index.json` is rebuilt and the extension shows up under **Available** in den (the refresh button in the Extensions view fetches straight away).
- [ ] Decide whether the copies in `examples/` stay as examples or become links to the new repos.

## 4. Not verified yet

- [ ] The offline case: with no network and a cached index, **Available** should still list it and show the fetch error below it.
- [ ] A real install from the index: the confirmation, the download, then a restart.
- [ ] An update from the index: release `v0.1.1` of a listed extension, wait for the daily build (or run it by hand), and check for the **Update** badge in den.
- [ ] `den-index check` and `build` against the live GitHub API. So far they have only run on fixtures.
- [ ] An extension's keybinding: Ctrl+Alt+H should run Hello's Say Hello. Posted messages can't carry modifier keys, so I only tested the menu.
- [ ] `file_saved`: save a file in a folder Workspace Stats tracks, then run Show Workspace Stats and check the saved-files count went up.

## Later

- Ratings, download counts, or signing beyond the release zip's `sha256`.
- A GitHub issue template for reporting a listed extension.
- Showing an available extension's settings in its preview page, read from the release's `extension.json`.
