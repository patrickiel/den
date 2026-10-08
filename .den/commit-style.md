<!-- Commit message style for the Generate Commit Message button. {{diff}} marks where the changes go. -->
Write ONE commit message for the diff below.

First line: <type>: <summary>
- type: feat | chore | fix | refactor | perf | docs | test
- summary: one sentence covering the whole change, ≤ 70 chars, imperative, lowercase, no period

When the diff contains several distinct features or parts, add a blank line and then one short, precise "- " line per part (no period). A small single-purpose change gets only the first line.

Example for a change with several parts:
feat: add dark mode and a settings search

- add a dark theme that follows the system setting
- filter the settings page as you type
- remember the last opened settings section

Rules:
- Describe what the change does, not how the code looks
- Don't invent context not in the diff
- Write in the same language as this repository's recent commit messages
- Output only the message, nothing else

DIFF:
{{diff}}
