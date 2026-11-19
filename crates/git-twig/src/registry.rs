//! Read-only, lossless Git registration inventory. Paths are not ownership proofs.
use anyhow::{Context, Result, bail, ensure};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Checkout {
    Bare,
    Detached { head: String },
    Branch { head: String, reference: OsString },
}
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Registration {
    pub path: PathBuf,
    pub checkout: Checkout,
    pub locked: Option<OsString>,
    pub prunable: Option<OsString>,
    pub unknown: Vec<OsString>,
}

pub(super) fn ensure_environment() -> Result<()> {
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
    ] {
        ensure!(
            std::env::var_os(key).is_none(),
            "environment override {key} is unsupported"
        );
    }
    Ok(())
}
pub(super) fn git(path: &Path) -> Command {
    let mut command = crate::git(path);
    command.args([
        "--no-optional-locks",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.hooksPath=/nonexistent/twig-inspection-hooks",
    ]);
    command
}
pub(super) fn read(path: &Path) -> Result<Vec<Registration>> {
    ensure_environment()?;
    let output = crate::checked(git(path).args(["worktree", "list", "--porcelain", "-z"]))?;
    parse(&output.stdout)
}

#[derive(Default)]
struct Record {
    path: Option<PathBuf>,
    head: Option<String>,
    branch: Option<OsString>,
    bare: bool,
    detached: bool,
    locked: Option<OsString>,
    prunable: Option<OsString>,
    unknown: Vec<OsString>,
}
fn one<T>(slot: &mut Option<T>, value: T) -> Result<()> {
    ensure!(slot.is_none(), "duplicate registration field");
    *slot = Some(value);
    Ok(())
}
impl Record {
    fn field(&mut self, field: &[u8]) -> Result<()> {
        if let Some(value) = field.strip_prefix(b"worktree ") {
            let path = crate::native::git_path(value.to_vec());
            ensure!(path.is_absolute(), "registration path must be absolute");
            one(&mut self.path, path)?;
        } else if let Some(value) = field.strip_prefix(b"HEAD ") {
            ensure!(
                matches!(value.len(), 40 | 64) && value.iter().all(u8::is_ascii_hexdigit),
                "invalid registration HEAD"
            );
            one(&mut self.head, String::from_utf8(value.to_vec())?)?;
        } else if let Some(value) = field.strip_prefix(b"branch ") {
            ensure!(
                value.starts_with(b"refs/") && value.len() > 5,
                "invalid branch reference"
            );
            one(&mut self.branch, crate::native::os_string(value.to_vec()))?;
        } else if field == b"bare" {
            ensure!(!self.bare, "duplicate bare flag");
            self.bare = true;
        } else if field == b"detached" {
            ensure!(!self.detached, "duplicate detached flag");
            self.detached = true;
        } else if field == b"locked" || field.starts_with(b"locked ") {
            one(
                &mut self.locked,
                crate::native::os_string(field.get(7..).unwrap_or_default().to_vec()),
            )?;
        } else if field == b"prunable" || field.starts_with(b"prunable ") {
            one(
                &mut self.prunable,
                crate::native::os_string(field.get(9..).unwrap_or_default().to_vec()),
            )?;
        } else {
            self.unknown.push(crate::native::os_string(field.to_vec()));
        }
        Ok(())
    }
    fn finish(self) -> Result<Registration> {
        let path = self.path.context("registration lacks worktree path")?;
        let checkout = match (self.bare, self.detached, self.branch, self.head) {
            (true, false, None, None) => Checkout::Bare,
            (false, true, None, Some(head)) => Checkout::Detached { head },
            (false, false, Some(reference), Some(head)) => Checkout::Branch { head, reference },
            _ => bail!("inconsistent registration checkout state"),
        };
        Ok(Registration {
            path,
            checkout,
            locked: self.locked,
            prunable: self.prunable,
            unknown: self.unknown,
        })
    }
}
fn parse(bytes: &[u8]) -> Result<Vec<Registration>> {
    ensure!(
        !bytes.is_empty() && bytes.ends_with(b"\0\0"),
        "incomplete registration output"
    );
    let mut records = Vec::new();
    let mut current = None;
    for field in bytes.split(|byte| *byte == 0) {
        if field.is_empty() {
            if let Some(record) = current.take() {
                records.push(Record::finish(record)?);
            }
        } else {
            current.get_or_insert_with(Record::default).field(field)?;
        }
    }
    ensure!(!records.is_empty(), "empty registry");
    let mut paths = std::collections::HashSet::new();
    ensure!(
        records
            .iter()
            .all(|record| paths.insert(record.path.clone())),
        "duplicate registration path"
    );
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inventory_preserves_states_reasons_and_future_fields() -> Result<()> {
        let head = "a".repeat(40);
        let d = if cfg!(windows) { "C:" } else { "" };
        let raw = format!(
            "worktree {d}/root\0HEAD {head}\0branch refs/heads/main\0\0worktree {d}/peer\0HEAD {head}\0detached\0locked reason\nline\0prunable missing\0future opaque\0\0worktree {d}/bare\0bare\0\0"
        );
        let records = parse(raw.as_bytes())?;
        assert_eq!(records.len(), 3);
        assert!(matches!(records[0].checkout, Checkout::Branch { .. }));
        assert!(matches!(records[1].checkout, Checkout::Detached { .. }));
        assert_eq!(
            records[1].locked.as_deref(),
            Some(std::ffi::OsStr::new("reason\nline"))
        );
        assert_eq!(
            records[1].prunable.as_deref(),
            Some(std::ffi::OsStr::new("missing"))
        );
        assert_eq!(records[1].unknown, vec![OsString::from("future opaque")]);
        assert_eq!(records[2].checkout, Checkout::Bare);
        Ok(())
    }
    #[cfg(unix)]
    #[test]
    fn paths_are_not_line_or_unicode_parsed() -> Result<()> {
        use std::os::unix::ffi::OsStrExt;
        let raw = b"worktree /space path\n\xff\0HEAD aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\0detached\0\0";
        let records = parse(raw)?;
        assert_eq!(records[0].path.as_os_str().as_bytes(), b"/space path\n\xff");
        Ok(())
    }
    #[cfg(unix)]
    #[test]
    fn generated_byte_paths_round_trip_without_claiming_ownership() -> Result<()> {
        use std::os::unix::ffi::OsStrExt;
        let mut random = 0x9e3779b97f4a7c15u64;
        for length in 1..128 {
            let mut path = b"/fixture/".to_vec();
            for _ in 0..length {
                random ^= random << 13;
                random ^= random >> 7;
                random ^= random << 17;
                let byte = random as u8;
                path.push(if byte == 0 || byte == b'/' {
                    b'_'
                } else {
                    byte
                });
            }
            let mut record = b"worktree ".to_vec();
            record.extend_from_slice(&path);
            record.extend_from_slice(b"\0HEAD aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\0detached\0\0");
            let parsed = parse(&record)?;
            assert_eq!(parsed[0].path.as_os_str().as_bytes(), path);
            assert!(matches!(&parsed[0].checkout, Checkout::Detached { head } if head.len() == 64));
        }
        Ok(())
    }

    #[test]
    fn malformed_or_ambiguous_inventory_is_rejected() {
        let head = "a".repeat(40);
        let invalid = [
            String::new(),
            "worktree /root\0bare\0".into(),
            "worktree relative\0bare\0\0".into(),
            "HEAD bad\0\0".into(),
            "worktree /x\0bare\0detached\0\0".into(),
            format!("worktree /x\0HEAD {head}\0HEAD {head}\0detached\0\0"),
            format!("worktree /x\0HEAD {head}\0branch refs/heads/x\0detached\0\0"),
            "worktree /x\0bare\0\0worktree /x\0bare\0\0".into(),
            "worktree /x\0bare\0locked\0locked twice\0\0".into(),
        ];
        for raw in invalid {
            assert!(parse(raw.as_bytes()).is_err(), "{raw:?}");
        }
    }
}
