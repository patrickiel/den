//! Shell integration for zsh and bash on macOS and Linux, as PowerShell gets
//! it on Windows: rc files under den's data folder that load the user's own
//! files first, then report the folder at each prompt (OSC 7), wrap `claude`
//! so every run reports to its tab, and run the tab's first command.
//!
//! zsh is pointed at den's folder with `ZDOTDIR`; each file there loads the
//! user's equivalent from `DEN_USER_ZDOTDIR` (their `ZDOTDIR`, else home), and
//! `.zshrc` puts `ZDOTDIR` back before anything runs, so nested shells see
//! nothing of it (zsh reads the user's own `.zlogin` next). bash takes den's
//! file with `--rcfile`; it loads the profile files a login shell would, as
//! macOS's Terminal starts one.

use std::path::{Path, PathBuf};

use portable_pty::CommandBuilder;

use crate::backend::agent;

const ZSHENV: &str = r#"# den's shell integration. Your own files load from $DEN_USER_ZDOTDIR.
if [[ -f "$DEN_USER_ZDOTDIR/.zshenv" ]]; then
  DEN_ZDOTDIR="$ZDOTDIR"
  ZDOTDIR="$DEN_USER_ZDOTDIR"
  . "$DEN_USER_ZDOTDIR/.zshenv"
  if [[ "$ZDOTDIR" != "$DEN_USER_ZDOTDIR" ]]; then
    export DEN_USER_ZDOTDIR="$ZDOTDIR"
  fi
  ZDOTDIR="$DEN_ZDOTDIR"
fi
"#;

const ZPROFILE: &str = r#"# den's shell integration. Your own .zprofile loads here.
if [[ -f "$DEN_USER_ZDOTDIR/.zprofile" ]]; then
  DEN_ZDOTDIR="$ZDOTDIR"
  ZDOTDIR="$DEN_USER_ZDOTDIR"
  . "$DEN_USER_ZDOTDIR/.zprofile"
  ZDOTDIR="$DEN_ZDOTDIR"
fi
"#;

const ZSHRC: &str = r#"# den's shell integration. Your own .zshrc loads first.
if [[ -f "$DEN_USER_ZDOTDIR/.zshrc" ]]; then
  DEN_ZDOTDIR="$ZDOTDIR"
  ZDOTDIR="$DEN_USER_ZDOTDIR"
  . "$DEN_USER_ZDOTDIR/.zshrc"
  ZDOTDIR="$DEN_ZDOTDIR"
fi

# The folder at each prompt, for the tab and for new tabs next to it.
__den_precmd() { printf '\e]7;file://%s%s\a' "$HOST" "$PWD" }
autoload -Uz add-zsh-hook
add-zsh-hook precmd __den_precmd

# Every `claude` in this tab reports to it (finished, needs input). An alias
# of yours would turn the definition into a parse error, so it goes first,
# on a line of its own: a block is parsed whole before it runs.
unalias claude 2>/dev/null
if [[ -n "$DEN_CLAUDE_HOOKS" ]]; then
  claude() { command claude --settings "$DEN_CLAUDE_HOOKS" "$@" }
fi

# Nested shells get your own files (zsh reads your .zlogin from there next).
ZDOTDIR="$DEN_USER_ZDOTDIR"
unset DEN_USER_ZDOTDIR DEN_ZDOTDIR

# The tab's first command: a preset, or a session coming back.
if [[ -n "$DEN_INIT_COMMAND" ]]; then
  __den_init="$DEN_INIT_COMMAND"
  unset DEN_INIT_COMMAND
  print -s -- "$__den_init"
  eval "$__den_init"
  unset __den_init
fi
"#;

const BASHRC: &str = r#"# den's shell integration. The files a login shell reads load first.
if [ -f /etc/profile ]; then . /etc/profile; fi
for __den_file in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
  if [ -f "$__den_file" ]; then . "$__den_file"; break; fi
done
unset __den_file

# The folder at each prompt, for the tab and for new tabs next to it.
__den_prompt() { printf '\e]7;file://%s%s\a' "${HOSTNAME:-$HOST}" "$PWD"; }
PROMPT_COMMAND="__den_prompt${PROMPT_COMMAND:+; $PROMPT_COMMAND}"

# Every `claude` in this tab reports to it (finished, needs input). An alias
# of yours would turn the definition into a parse error, so it goes first,
# on a line of its own: a block is parsed whole before it runs.
unalias claude 2>/dev/null
if [ -n "$DEN_CLAUDE_HOOKS" ]; then
  claude() { command claude --settings "$DEN_CLAUDE_HOOKS" "$@"; }
fi

# The tab's first command: a preset, or a session coming back.
if [ -n "$DEN_INIT_COMMAND" ]; then
  __den_init="$DEN_INIT_COMMAND"
  unset DEN_INIT_COMMAND
  history -s -- "$__den_init"
  eval "$__den_init"
  unset __den_init
fi
"#;

/// Set `cmd` (the shell `shell`) up with den's rc files for terminal `pane`,
/// `command` run first; `false` for a shell den has no files for (or when
/// they cannot be written), which the caller starts plainly.
pub fn configure(cmd: &mut CommandBuilder, shell: &str, pane: u64, command: Option<&str>) -> bool {
    if !cfg!(unix) {
        return false;
    }
    let Some(name) = Path::new(shell).file_name().and_then(|n| n.to_str()) else { return false };
    // The files first: `cmd` is left alone unless they are there.
    let dir = crate::settings::data_dir().join("shell").join(name);
    match name {
        "zsh" => {
            if !write(&dir, &[(".zshenv", ZSHENV), (".zprofile", ZPROFILE), (".zshrc", ZSHRC)]) {
                return false;
            }
            let user = std::env::var_os("ZDOTDIR").or_else(|| std::env::var_os("HOME")).unwrap_or_default();
            cmd.env("ZDOTDIR", &dir);
            cmd.env("DEN_USER_ZDOTDIR", user);
            cmd.arg("-l");
        }
        "bash" => {
            if !write(&dir, &[("bashrc", BASHRC)]) {
                return false;
            }
            cmd.arg("--rcfile");
            cmd.arg(dir.join("bashrc"));
        }
        _ => return false,
    }
    if let Some(hooks) = agent::claude_hooks(pane) {
        cmd.env("DEN_CLAUDE_HOOKS", hooks);
    }
    if let Some(command) = command {
        cmd.env("DEN_INIT_COMMAND", agent::inject(pane, command, true));
    }
    true
}

/// Write `files` into `dir`, each only when it differs.
fn write(dir: &PathBuf, files: &[(&str, &str)]) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    files.iter().all(|(name, text)| {
        let path = dir.join(name);
        std::fs::read_to_string(&path).ok().as_deref() == Some(*text) || std::fs::write(&path, text).is_ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_zsh_and_bash_only() {
        let mut cmd = CommandBuilder::new("fish");
        assert!(!configure(&mut cmd, "/opt/homebrew/bin/fish", 1, None));
    }

    #[test]
    fn rc_files_reset_the_tab_variables() {
        for rc in [ZSHRC, BASHRC] {
            assert!(rc.contains("unset DEN_INIT_COMMAND"), "the first command must not reach child shells");
            assert!(rc.contains("--settings \"$DEN_CLAUDE_HOOKS\""));
        }
        // ZDOTDIR goes back before the first command runs, so its children see the user's files.
        let restore = ZSHRC.find("ZDOTDIR=\"$DEN_USER_ZDOTDIR\"").expect("zshrc restores ZDOTDIR");
        assert!(restore < ZSHRC.find("eval \"$__den_init\"").unwrap());
        assert!(ZSHRC.contains("unalias claude") && BASHRC.contains("unalias claude"));
    }
}
