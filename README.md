# den

A project terminal for Windows: split panes of terminals, agents, files and browsers, with an explorer, search and source control. A native Rust app on [GPUI](https://gpui.rs), Zed's UI framework, through [GPUI Kit](https://gpui-kit.com) 0.7 (`gpui-kit` + `gpui-component`): no webview for the UI, no Svelte, no Monaco. (den was a Tauri + Svelte app up to v0.11.0.)

```sh
cargo run --release -- <folder>     # no folder: the current directory
```

The first build compiles GPUI and the tree-sitter grammars and takes a few minutes. Dev builds optimise dependencies (`opt-level = 2`), because unoptimised GPUI is too slow to use.

## What is in it

| Feature | |
| --- | --- |
| Editor | GPUI Kit's code editor (`EditorState`): rope buffer, tree-sitter highlighting (Rust, JS/TS/TSX, JSON, Markdown, CSS, HTML, TOML, YAML, Python, Bash, Svelte), line numbers, indent guides, folding, find. Ctrl+S saves, `●` marks unsaved changes. **Preview tabs** (italic) as in VS Code. A file that changes on disk while its tab has no unsaved changes is reloaded in place, keeping the cursor. A **status bar** under each file, each item a menu: Ln/Col (Go to Line), indentation (detected; spaces 2/4/8 or tabs), encoding (reopen or save in another encoding; files are read by BOM, else UTF-8, else Windows-1252), LF / CRLF, language mode. **Quick diff** marks in the gutter against the git index (green added, blue changed, red deleted; off with soft wrap). |
| Previews | Markdown and SVG files have **Source / Preview** buttons; the preview includes unsaved edits, Markdown loads images relative to the file, and the last choice is remembered per type. Images (PNG, JPEG, GIF, WebP, BMP, ICO, AVIF) open as pictures. |
| Formatting | **Format Document** (Shift+Alt+F, ☰ ▸ Edit) runs the formatter on this machine: the project's own Prettier (`node_modules/.bin`, else on PATH), rustfmt (edition from `Cargo.toml`), Ruff or Black, gofmt, shfmt, clang-format, PSScriptAnalyzer for PowerShell. One undo step; the file keeps its line endings. **Format on save** in Settings. |
| Groups | den's layout tree (`src/layout.rs`): groups of tabs in splits. Drag a tab to another group's strip, into its middle to join it, or onto a side to start a group there. Groups stay when their last tab closes; an empty group gets its own ✕. Each strip ends in the group's buttons: default, shell, pinned presets, **split** (right, or down while Alt is held) and a ⋮ menu. Double-clicking the empty strip opens another tab of the kind the group shows. **Grab a group** by its strip anywhere but on a tab and drop it beside another group, on a container's header, or in the band along the outer edge. Ctrl+W or a middle-click closes a tab, Ctrl+Shift+Q the group, Ctrl+Tab cycles tabs, Alt+arrows move between groups. |
| Containers | Every split is a container. Nested ones are drawn as a tray with a header: group count and layout, the **default** menu, split, flip, and a ⋮ menu with **Close Container**. Grab a container by its header to move it. The outermost container's buttons are in the title bar. |
| Default groups | A default button and menu on every strip and container (Files, Terminals, Agents, Browsers, None). New tabs of a kind go to a default of that kind. |
| Layout | Layout presets in the title bar (load, **Save Layout…**, **Reset Layout**, delete). Per folder the layout is saved on every change; the arrangement also goes to **`.den/layout.json`** in the folder (paths relative, no machine state) and is restored from there first, so it travels with the folder. |
| Sessions | The session switcher in the title bar: recent folders (each by its name, with parent folders added until it is unique; the full path in the tooltip), **Open Folder…** (Ctrl+Shift+O, saves first) and **Open Folder in New Window…**. A hovered entry's ✕ takes it off the list. The taskbar's jump list has the same recent folders (each opens in a new window). Dropping a folder on the window opens it; dropped files open as tabs. Closing with unsaved files asks first. |
| Terminals | A shell (`pwsh`, else Windows PowerShell; `TERM_SHELL` or Settings override) on a ConPTY (portable-pty) with alacritty_terminal parsing the screen: colours, cursor, alternate screen, scrollback (wheel, Shift+PgUp/PgDn), bracketed paste, mouse reporting, block and box-drawing glyphs drawn as shapes. Selection and copy (Ctrl+C with a selection, Ctrl+Shift+C), paste (Ctrl+Shift+V, right-click), Ctrl+click links (URLs, and `path:line:col` resolved against the shell's folder, which it reports through an OSC 7 prompt hook). Scrollback comes back with the session; Claude Code comes back in the conversation it had (its hooks report the session id; `claude --resume <id>`, else `--continue`), a typed `claude` too. In Claude Code the wheel pages (PgUp/PgDn) instead of walking the prompt history. |
| Presets | Terminal, agent and browser presets (taken over once from the Tauri den's settings, with its font and home page): pinned ones get a button on every strip; drag one onto another to reorder. Settings edits, reorders and pins them; which built-in strip buttons show (shell, browser, split) is in Settings and in each group's ⋮ menu, with the presets' pins; a preset's mark opens a picker: automatic icon, letter, vendor logos (LobeHub), levels, glyphs (Phosphor), colour swatches and a hue slider. |
| Browser | Browser tabs are a real Chromium (WebView2 through wry) inside a group: the globe button on a strip, ☰ ▸ New Browser or Ctrl+Shift+B opens one at the home page. Back / forward / reload and a URL bar (a host opens as https, `localhost` as http, anything else is a search); links that open a new window open as a browser tab. Tabs come back at their last URL; logins persist in den's own profile (`%APPDATA%\den\webview`). The page is a native window over the app: it follows its pane, and hides while its tab is not showing, during a drag and under dialogs. While the page has the keyboard, keys go to it; click the toolbar to get them back. |
| Notifications | When a terminal's program wants you while you look elsewhere: Claude Code finishing a turn or asking (hooks reporting to a localhost listener, also for a typed `claude`), Codex's OSC 9, OSC 777 and the bell. A toast (click: go to the tab), a dot on the tab, synthesized sounds (Chime, Ping, Pop, Bell, Alert, Rise; one for a finished turn, one for a question, with a volume), a taskbar flash; each can be switched off. |
| Explorer | VS Code's Seti file icons in the tree, tabs, Source Control and Search. File tree with git colours and letters, change dots on folders, ignored files dimmed, following the disk (notify watcher). Right-click: New File / Folder, Open Terminal Here, Reveal, Copy Path / Relative Path, Rename (F2), Delete to the Recycle Bin. |
| Search | Regex / case / whole word, include / exclude globs, `.gitignore` aware, streamed results, replace per file or all (unsaved files are skipped). |
| Source Control | git through its CLI: branch menu (switch, create), fetch / pull / push with ahead / behind, Merge / Staged / Changes groups with stage, unstage and discard, commit (Ctrl+Enter; with nothing staged it asks Yes / Always / Never, as VS Code does, kept in Settings), Amend and Commit & Push. The message being written is kept with the session. A change opens side by side. Commits expand to their files; a file shows that commit's change. |
| AI commit messages | The ✨ button by the commit box writes the message with a local model: on first use it downloads a pinned llama.cpp build and the chosen model (Settings ▸ AI; default Qwen2.5-Coder 1.5B) into `%APPDATA%\den\ai`, runs `llama-server` on localhost (GPU, else CPU; stopped when idle and on quit) and streams the message in. The style is the repository's `.den/commit-style.md`, else derived from its history and saved there; big change sets are summarized in parts first. |
| Themes | Dark and Light (VS Code's Dark Modern / Light Modern, Dark+ / Light+ syntax colours), and **Import…** for any VS Code color theme (comments, trailing commas and `include` chains handled): window, syntax colours and terminal palette. Imported themes live in `%APPDATA%\den\themes`. |
| Settings | A tab (gear or Ctrl+,): theme, sidebar side, font, editor size, line numbers, soft wrap, format on save, tab close buttons, notifications, shell, scrollback, terminal and agent presets, AI. |

State lives in `%APPDATA%\den\` (`settings.json`, `state.json`, `themes\`, `hooks\`, `ai\`, `webview\`).

## How it is built

- `src/workspace.rs`: the window. Title bar, sidebar, routing of new tabs, sessions, presets, notifications.
- `src/layout.rs`, `src/defaults.rs`: the layout tree and the default groups as plain data, with every edit on them. Node and pane ids are stable and saved. Unit tested.
- `src/layout_view.rs`: draws the tree and handles every drag and drop.
- `src/layout_file.rs`: the folder's `.den/layout.json`.
- `src/pane.rs`: the `Pane` trait every tab kind implements, and the builder registry that makes panes again from a saved layout.
- `src/panels.rs`: file and Settings panes; `src/diff.rs` diff tabs; `src/dirty_diff.rs` the gutter marks.
- `src/terminal/`: the terminal pane, its GPUI element, glyphs, links, OSC scanners, colours and keys.
- `src/browser.rs`: browser tabs; `src/preset_icon.rs` preset icons and their picker; `src/assets.rs` the embedded icons in `assets/presets`.
- `src/explorer.rs`, `src/search.rs`, `src/scm.rs`, `src/repo.rs`: the sidebar views.
- `src/theme.rs`: VS Code themes onto the component theme, syntax colours and terminal palette.
- `src/backend/`: work off the UI thread: `search`, `git`, `watch`, `format`, `agent` (Claude Code hooks), `ai` (llama.cpp runtime) and `commit_ai` (the prompting).

## Releasing

`node scripts/release.ts [--dry-run] [--yes] [--bump major|minor|patch]`: Claude picks the bump (by `scripts/release-scale.md`) and writes the notes; the script bumps `Cargo.toml`, builds, packs the per-user NSIS installer (`packaging/installer.nsi`, NSIS from Tauri's cache), signs it with `~/.tauri/den.key` (Tauri's signer; password in `DEN_SIGNING_KEY_PASSWORD`), writes `latest.json`, tags and publishes a GitHub release on `patrickiel/den`.

Installed copies check `latest.json` a little after start and from ☰ ▸ Check for Updates, verify the installer against `packaging/updater.pub` (the key's public half, put there by the first release) and install it silently, then restart. Development builds and builds without a key do not update. The exe carries den's icon (its amber dev icon in debug builds).

## Known gaps

- Menus and toasts are drawn under a browser page (it is a native window); dialogs and drags hide it.
- Formatting needs the formatters installed; the Tauri den bundled Prettier, Ruff, shfmt and gofmt.
