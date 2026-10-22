//! Prevent ordinary PR tests from changing the host's global Git configuration.
//! This is an accidental-write guard, not a sandbox for hostile shell commands.

use crate::{Config, Result};
use serde_json::Value;
use std::path::Path;

pub fn transform(
    config: &Config,
    working_dir: &str,
    tool: &str,
    input: &Value,
) -> Result<Option<Value>> {
    if !matches!(tool, "bash" | "shell") {
        return Ok(None);
    }
    let root = config.worktree_dir_path()?;
    if !root.exists() {
        return Ok(None);
    }
    let working_dir = Path::new(working_dir).canonicalize()?;
    if !working_dir.starts_with(root.canonicalize()?) {
        return Ok(None);
    }
    let Some(command) = input.get("command").and_then(Value::as_str) else {
        return Ok(None);
    };
    let mut result = input.clone();
    result["command"] = Value::String(wrap(command));
    Ok(Some(result))
}

fn wrap(command: &str) -> String {
    // Each invocation starts fresh. Includes preserve host credentials and settings
    // while `git config --global` writes only to the temporary top-level file.
    format!(
        r#"(
_astrcode_git_config=$(mktemp "${{TMPDIR:-/tmp}}/astrcode-pr-git.XXXXXXXX") || exit 1
trap 'rm -f -- "$_astrcode_git_config"' EXIT
if [ -n "${{GIT_CONFIG_GLOBAL+x}}" ]; then
    [ -z "$GIT_CONFIG_GLOBAL" ] || git config --file "$_astrcode_git_config" --add include.path "$GIT_CONFIG_GLOBAL" || exit 1
else
    for _astrcode_git_source in "${{XDG_CONFIG_HOME:-$HOME/.config}}/git/config" "$HOME/.gitconfig"; do
        if [ -f "$_astrcode_git_source" ]; then
            git config --file "$_astrcode_git_config" --add include.path "$_astrcode_git_source" || exit 1
        fi
    done
fi
export GIT_CONFIG_GLOBAL="$_astrcode_git_config"
(
{command}
)
)"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, process::Command};

    #[test]
    fn scopes_and_isolates_each_command_without_losing_host_settings() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home with spaces");
        let root = temp.path().join("worktrees");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&root).unwrap();
        let original = "[user]\n name = Original\n";
        fs::write(home.join(".gitconfig"), original).unwrap();
        let config = Config {
            worktree_dir: root.display().to_string(),
            ..Config::default()
        };
        let input = serde_json::json!({"command": "git config --global --includes --get user.name; git config --global user.name Changed; git config --global --includes --get user.name", "timeout": 30});
        assert!(transform(&config, home.to_str().unwrap(), "shell", &input)
            .unwrap()
            .is_none());
        assert!(
            transform(&config, root.to_str().unwrap(), "read_file", &input)
                .unwrap()
                .is_none()
        );
        let transformed = transform(&config, root.to_str().unwrap(), "shell", &input)
            .unwrap()
            .unwrap();
        assert_eq!(transformed["timeout"], 30);
        for _ in 0..2 {
            let output = Command::new("bash")
                .args(["-c", transformed["command"].as_str().unwrap()])
                .env("HOME", &home)
                .env_remove("GIT_CONFIG_GLOBAL")
                .env_remove("XDG_CONFIG_HOME")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout),
                "Original\nChanged\n"
            );
            assert_eq!(
                fs::read_to_string(home.join(".gitconfig")).unwrap(),
                original
            );
        }
        let output = Command::new("bash")
            .args(["-c", &wrap("exit 7")])
            .env("HOME", &home)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(7));
    }
}
