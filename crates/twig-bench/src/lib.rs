//! Shared subprocess boundary for owned benchmark fixtures, never user Git context.
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::Command,
};

/// A canonical path in the form Git and Windows tools accept: without the
/// verbatim `\\?\` prefix `fs::canonicalize` gives on Windows.
pub fn canonical(path: impl AsRef<Path>) -> std::io::Result<PathBuf> {
    let path = std::fs::canonicalize(path)?;
    #[cfg(windows)]
    if let Some(ordinary) = path.to_str().and_then(|text| text.strip_prefix(r"\\?\"))
        && !ordinary.starts_with("UNC\\")
    {
        return Ok(PathBuf::from(ordinary));
    }
    Ok(path)
}

/// Keep repository redirects, injected config and trace destinations out of fixtures.
/// Per-command private index/worktree overrides may be added by a caller afterwards.
pub fn command(program: impl AsRef<OsStr>, cwd: &Path) -> Command {
    let mut command = Command::new(program);
    command.current_dir(cwd);
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_CONFIG",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_COUNT",
        "GIT_ATTR_SOURCE",
        "GIT_REPLACE_REF_BASE",
        "GIT_EXEC_PATH",
        "GIT_SHALLOW_FILE",
        "GIT_OPTIONAL_LOCKS",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
        "GIT_EXTERNAL_DIFF",
        "GIT_AUTHOR_NAME",
        "GIT_AUTHOR_EMAIL",
        "GIT_COMMITTER_NAME",
        "GIT_COMMITTER_EMAIL",
        "GIT_TRACE",
        "GIT_TRACE_SETUP",
        "GIT_TRACE_PERFORMANCE",
        "GIT_TRACE_PACKET",
        "GIT_TRACE2",
        "GIT_TRACE2_EVENT",
        "GIT_TRACE2_PERF",
        "GIT_TRACE_CURL",
    ] {
        command.env_remove(key);
    }
    for (key, _) in std::env::vars_os() {
        if key
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("GIT_TRACE")
        {
            command.env_remove(key);
        }
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        // A run may name one global config file, such as Git for Windows'
        // defaults; the suite records it.
        .env(
            "GIT_CONFIG_GLOBAL",
            std::env::var_os("TWIG_BENCH_GIT_CONFIG_GLOBAL")
                .unwrap_or_else(|| if cfg!(windows) { "NUL" } else { "/dev/null" }.into()),
        )
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_TEMPLATE_DIR", "")
        .env("GIT_DEFAULT_HASH", "sha1")
        .env("XDG_CONFIG_HOME", "/nonexistent/twig-bench")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .env("TWIG_TRACE", "1");
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicitly_removes_redirects_without_mutating_parent_environment() {
        let command = command("git", Path::new("."));
        let variables: std::collections::BTreeMap<_, _> = command.get_envs().collect();
        for key in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_COMMON_DIR",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_PARAMETERS",
            "GIT_TRACE",
        ] {
            assert_eq!(variables.get(OsStr::new(key)), Some(&None), "{key}");
        }
        assert_eq!(
            variables.get(OsStr::new("GIT_TEMPLATE_DIR")),
            Some(&Some(OsStr::new("")))
        );
        assert_eq!(
            variables.get(OsStr::new("GIT_ATTR_NOSYSTEM")),
            Some(&Some(OsStr::new("1")))
        );
        assert_eq!(
            variables.get(OsStr::new("TWIG_TRACE")),
            Some(&Some(OsStr::new("1")))
        );
    }
}
