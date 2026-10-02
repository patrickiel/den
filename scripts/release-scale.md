# Release scale

`node scripts/release.mjs` sends this file to Claude, along with the commits and the diff since the last release. Claude uses it to pick the version bump and to write the release notes. Edit it to change how releases are judged.

## Picking the bump

Judge the change as the person using den sees it, not by how big the code change is.

- **patch**: nothing new to learn. Bug fixes, performance, refactors, visual polish, dependency bumps, docs, build or tooling changes. A patch can be large in code and still be a patch.
- **minor**: something new the user can do or notice. A new feature, pane kind, sidebar view, setting, menu item, keybinding or theme ability. A clearly changed behaviour of an existing feature also counts.
- **major**: an existing workflow breaks. Saved sessions or settings no longer load, or load with data lost. A feature is removed, or a default keybinding or behaviour that people rely on changes incompatibly.

Rules:

- While the version is **0.x**, a breaking change is a **minor** bump. Never propose major on 0.x; 1.0.0 is a deliberate choice made with `--bump major`.
- Pick the highest level that any single change deserves.
- When unsure between two levels, pick the lower one.
- If the changes only touch the release tooling, docs or tests, choose patch.

## Writing the notes

- Write for people who use den, not for its developers. Describe what changed for them: "Search can replace across files", not "add replace.ts".
- Use a Markdown bullet list. With more than about five items, group them under `### New`, `### Improved` and `### Fixed`, and leave out any group that has nothing in it.
- Leave out purely internal work such as refactors, renames and code moves, unless it changes behaviour. If nothing user-visible changed, write one bullet like "Internal improvements".
- Don't add a title, the version number, commit hashes or file paths. The release page shows the version already.
- Keep each bullet to one line.

## The reason

Write one sentence that names the change that decided the level.
