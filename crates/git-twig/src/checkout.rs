//! Target-index checkout policy and per-path recovery inside private staging.
use crate::{
    checked, checked_with_input as with_input, git,
    index::Stat,
    native,
    objects::{self, Objects},
    output_path,
    tree::{Entry, Kind},
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::{Command, Output},
};

/// Git copies config.worktree into a new Git directory, but not sibling include
/// files. Relative includes there can therefore change policy after registration.
pub fn ensure_relocatable_worktree_config(root: &Path) -> Result<()> {
    let enabled = git(root)
        .args(["config", "--bool", "--get", "extensions.worktreeConfig"])
        .output()?;
    if enabled.status.code() == Some(1) {
        return Ok(());
    }
    let enabled = checked_output(enabled)?;
    if enabled.stdout != b"true\n" {
        return Ok(());
    }
    let includes = git(root)
        .args([
            "config",
            "--worktree",
            "--no-includes",
            "--path",
            "--null",
            "--get-all",
            "include.path",
        ])
        .output()?;
    if includes.status.code() == Some(1) {
        return Ok(());
    }
    let includes = checked_output(includes)?.stdout;
    ensure!(
        includes.last() == Some(&0),
        "unterminated worktree config includes"
    );
    for path in includes[..includes.len() - 1].split(|b| *b == 0) {
        // --path expands ~/ and %(prefix)/, but leaves ordinary relative paths
        // relative. Absolute includes keep their own nested-include anchoring.
        ensure!(
            PathBuf::from(native::os_string(path.to_vec())).is_absolute(),
            "relative config.worktree includes require stock Git"
        );
    }
    Ok(())
}

pub struct Checkout<'a> {
    git_dir: PathBuf,
    data: &'a Path,
    index: &'a Path,
}

impl<'a> Checkout<'a> {
    pub fn new(root: &Path, data: &'a Path, index: &'a Path) -> Result<Self> {
        let git_dir =
            output_path(checked(git(root).args(["rev-parse", "--absolute-git-dir"]))?.stdout)?;
        Ok(Self {
            git_dir,
            data,
            index,
        })
    }

    fn command(&self) -> Command {
        let mut command = git(self.data);
        command
            .env("GIT_DIR", &self.git_dir)
            .env("GIT_WORK_TREE", self.data)
            .env("GIT_INDEX_FILE", self.index);
        // Native clones carry executable bits on Unix. Windows has none, and Git
        // for Windows records core.filemode=false for that reason.
        #[cfg(unix)]
        command.args(["-c", "core.filemode=true"]);
        command.args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.sparseCheckout=false",
            "-c",
            "submodule.recurse=false",
            // The staged index is installed into the new worktree: it must be
            // one self-contained file with nothing tied to this location.
            "-c",
            "core.splitIndex=false",
            "-c",
            "core.untrackedCache=false",
            "-c",
            "index.sparse=false",
        ]);
        command
    }

    pub fn initialize(&self, commit: &str) -> Result<()> {
        checked(self.command().args(["read-tree", commit]))?;
        Ok(())
    }

    pub fn policy(&self, entries: &[Entry]) -> Result<Policy> {
        let config = checked(self.command().args(["config", "--null", "--list"]))?.stdout;
        let configuration = Configuration::parse(&config)?;
        let input = paths(entries.iter().map(|entry| entry.path.as_path()));
        let attributes = with_input(
            self.command()
                .args(["check-attr", "--cached", "--all", "-z", "--stdin"]),
            input,
        )?
        .stdout;
        let (actions, archivable) = parse_attributes(entries, &attributes, &configuration)?;
        Ok(Policy {
            archivable,
            config: checkout_config(&config),
            attributes,
            actions,
            native_symlinks: configuration.native_symlinks(),
        })
    }

    fn write_paths(&self, paths: &[&Path]) -> Result<()> {
        if !paths.is_empty() {
            with_input(
                self.command()
                    .args(["checkout-index", "--force", "-z", "--stdin"]),
                self::paths(paths.iter().copied()),
            )?;
        }
        Ok(())
    }

    /// Verify every private clone against the target, let Git write the rest, and
    /// return per entry the stat data of each file whose bytes were hashed equal.
    pub fn repair(
        &self,
        root: &Path,
        commit: &str,
        entries: &[Entry],
        policy: &Policy,
        lazy: bool,
    ) -> Result<Vec<Option<Stat>>> {
        let regular: Vec<_> = entries
            .iter()
            .zip(&policy.actions)
            .filter(|(entry, action)| {
                matches!(action, Action::VerifyClone | Action::VerifySettled)
                    && matches!(entry.kind, Kind::File { .. })
            })
            .map(|(entry, _)| entry)
            .collect();
        let mut matches = objects::hash_private_files(root, self.data, &regular)?.into_iter();
        // A partial clone's archive would fetch each absent blob on its own.
        let converted: Vec<_> = entries
            .iter()
            .zip(policy.actions.iter().zip(&policy.archivable))
            .filter(|(entry, (action, archivable))| {
                matches!(action, Action::VerifyConverted | Action::VerifyAutoBinary)
                    && **archivable
                    && matches!(entry.kind, Kind::File { .. })
                    && !lazy
                    && std::fs::symlink_metadata(self.data.join(&entry.path)).is_ok()
            })
            .map(|(entry, _)| entry.path.as_path())
            .collect();
        let mut converted_verdicts: std::collections::HashMap<&Path, bool> = converted
            .iter()
            .copied()
            .zip(self.converted(commit, &converted)?)
            .collect();
        let mut symlink_objects = None;
        let mut attributes = Vec::new();
        let mut files = Vec::new();
        let mut certified = Vec::with_capacity(entries.len());
        let mut verified = 0;
        for (entry, action) in entries.iter().zip(&policy.actions) {
            let mut certificate = None;
            let retain = match (*action, entry.kind) {
                (Action::VerifyClone | Action::VerifySettled, Kind::File { .. }) => {
                    let verdict = matches.next().expect("one hash verdict per regular file");
                    // Where cleaning could convert, equal raw bytes may still be a
                    // modified file to Git. Let Git judge those, as after a checkout.
                    if *action == Action::VerifySettled {
                        certificate = verdict;
                    }
                    verdict.is_some()
                }
                (Action::VerifyConverted, Kind::File { .. }) => converted_verdicts
                    .remove(entry.path.as_path())
                    .unwrap_or(false),
                (Action::VerifyClone, Kind::Symlink)
                | (Action::VerifyAutoBinary, Kind::File { .. }) => {
                    if symlink_objects.is_none() {
                        symlink_objects = Some(Objects::open(root)?);
                    }
                    // Automatic conversion needs the target's full NUL classification;
                    // ordinary regular files retain the batched unfiltered hash path.
                    let comparison = symlink_objects
                        .as_mut()
                        .expect("opened object stream")
                        .compare(self.data, entry)?;
                    let raw = comparison.equal
                        && (*action != Action::VerifyAutoBinary || comparison.contains_nul);
                    // Text under automatic conversion: compare Git's bytes instead.
                    let converted = converted_verdicts
                        .remove(entry.path.as_path())
                        .unwrap_or(false);
                    raw || converted
                }
                _ => false,
            };
            certified.push(certificate);
            if retain {
                if matches!(entry.kind, Kind::File { .. }) {
                    verified += 1;
                }
                continue;
            }
            // checkout-index asks a promisor remote for one blob per path. Only
            // Git's own checkout batches, so leave absent blobs to it.
            ensure!(
                !lazy || entry.kind == Kind::Submodule,
                "partial clone: Git must write {:?}, and stock Git fetches absent blobs in one batch",
                entry.path
            );
            if entry
                .path
                .file_name()
                .is_some_and(|name| name == ".gitattributes")
            {
                attributes.push(entry.path.as_path());
            } else {
                files.push(entry.path.as_path());
            }
        }
        // Git may consult working-tree attributes while writing. Restore target policies
        // first; untracked policies were already removed by the snapshot inventory.
        self.write_paths(&attributes)?;
        self.write_paths(&files)?;
        policy.ensure_same(&self.policy(entries)?)?;
        crate::trace(&format!(
            "checkout: {verified} verified file clones, {} Git-written paths",
            attributes.len() + files.len()
        ));
        Ok(certified)
    }

    /// Whether each private file holds exactly the bytes Git's checkout would
    /// write there, as `git archive` writes them under this configuration.
    fn converted(&self, commit: &str, paths: &[&Path]) -> Result<Vec<bool>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let mut command = self.command();
        command.args(["archive", "--format=tar", commit]);
        // Name few paths literally; otherwise stream the whole tree, which also
        // keeps the command line short.
        let literal: usize = paths.iter().map(|path| path.as_os_str().len() + 12).sum();
        if literal < 8192 {
            command.arg("--");
            for path in paths {
                let mut spec = std::ffi::OsString::from(":(literal)");
                spec.push(path.as_os_str());
                command.arg(spec);
            }
        }
        let verdicts = crate::archive::compare(command, self.data, paths)?;
        crate::trace(&format!(
            "converted verification: {} of {} files equal Git's checkout bytes",
            verdicts.iter().filter(|equal| **equal).count(),
            verdicts.len()
        ));
        Ok(verdicts)
    }

    /// Replace the staged index with one recording the verified files' stat data.
    /// Git must read the draft, agree that it names exactly the target commit, and
    /// rewrite it in its own format. Its refresh trusts the recorded files and
    /// compares the rest by content, exactly as after a checkout: a path that is
    /// modified the moment Git writes it stays visibly modified.
    pub fn record(
        &self,
        commit: &str,
        entries: &[Entry],
        certified: &[Option<Stat>],
    ) -> Result<()> {
        crate::index::write_draft(self.data, self.index, entries, certified)?;
        checked(
            self.command()
                .args(["update-index", "-q", "--refresh", "--force-write-index"]),
        )?;
        checked(
            self.command()
                .args(["diff-index", "--cached", "--quiet", commit, "--"]),
        )
        .context("recorded index does not match the target commit")?;
        Ok(())
    }
}

// Git legitimately adds branch.<name>.remote/merge/rebase during registration.
// Those do not affect checkout bytes; branch-conditional includes were already
// rejected by Configuration::parse. Keep all other keys in the recheck boundary.
fn checkout_config(bytes: &[u8]) -> Vec<u8> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty() && !field.starts_with(b"branch."))
        .flat_map(|field| field.iter().copied().chain(std::iter::once(0)))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    /// Raw bytes equal to the blob settle the matter: Git converts this path in
    /// neither direction, so it also sees the file as unchanged.
    VerifySettled,
    /// Checkout is the identity, so raw bytes certify the clone. Cleaning may
    /// still convert, so whether the file counts as unchanged is Git's call.
    VerifyClone,
    VerifyAutoBinary,
    /// Checkout converts this file. A clone is kept only if its bytes equal
    /// what Git writes, read from `git archive`; Git judges it unchanged.
    VerifyConverted,
    GitWrite,
}

pub struct Policy {
    config: Vec<u8>,
    attributes: Vec<u8>,
    actions: Vec<Action>,
    /// Per entry: `git archive` writes it as checkout does (no `export-subst`
    /// or `export-ignore`), so its archived bytes can certify a clone.
    archivable: Vec<bool>,
    /// Whether Twig recreates symlinks itself. Otherwise Git writes each one:
    /// as a plain file holding the target under core.symlinks=false, and as a
    /// file or directory symlink of Git's choosing on Windows.
    pub native_symlinks: bool,
}

impl Policy {
    pub fn ensure_same(&self, other: &Self) -> Result<()> {
        ensure!(
            self.config == other.config && self.attributes == other.attributes,
            "checkout configuration or attributes changed during creation"
        );
        Ok(())
    }
}

#[derive(Default)]
struct Attributes {
    text: Option<Vec<u8>>,
    eol: Option<Vec<u8>>,
    legacy_crlf: bool,
    builtin_conversion: bool,
    /// `export-subst` or `export-ignore`: archives differ from checkouts.
    archive_differs: bool,
}

fn parse_attributes(
    entries: &[Entry],
    bytes: &[u8],
    configuration: &Configuration,
) -> Result<(Vec<Action>, Vec<bool>)> {
    ensure!(
        bytes.is_empty() || bytes.last() == Some(&0),
        "unterminated Git attributes"
    );
    let fields: Vec<_> = if bytes.is_empty() {
        Vec::new()
    } else {
        bytes[..bytes.len() - 1].split(|b| *b == 0).collect()
    };
    ensure!(fields.len() % 3 == 0, "invalid Git attribute response");
    let inventory: BTreeMap<_, _> = entries
        .iter()
        .enumerate()
        .map(|(i, entry)| (native::path_bytes(&entry.path), i))
        .collect();
    let mut attributes: Vec<_> = entries.iter().map(|_| Attributes::default()).collect();
    let mut seen = std::collections::BTreeSet::new();
    for row in fields.as_chunks::<3>().0 {
        let index = *inventory
            .get(row[0])
            .context("attributes for unknown path")?;
        ensure!(seen.insert((index, row[1])), "duplicate Git attribute");
        // Git does not run blob conversions for symlink text or gitlink
        // directories. These attributes are observable but cannot authorize a
        // filter invocation on such an entry.
        if !matches!(entries[index].kind, Kind::File { .. }) {
            continue;
        }
        let attribute = &mut attributes[index];
        match row[1] {
            // --all reports explicitly present values, including literal sentinel names.
            // External programs must not run in speculative staging or twice on fallback.
            b"filter" => {
                anyhow::bail!(
                    "checkout attribute {}={:?} on {:?} requires stock Git",
                    String::from_utf8_lossy(row[1]),
                    String::from_utf8_lossy(row[2]),
                    native::os_string(row[0].to_vec())
                )
            }
            b"ident" | b"working-tree-encoding" => attribute.builtin_conversion = true,
            b"export-subst" | b"export-ignore" => attribute.archive_differs = true,
            b"text" => attribute.text = Some(row[2].to_vec()),
            b"eol" => attribute.eol = Some(row[2].to_vec()),
            b"crlf" => attribute.legacy_crlf = true,
            _ => {}
        }
    }
    let archivable = attributes
        .iter()
        .map(|attrs| !attrs.archive_differs)
        .collect();
    let actions = entries
        .iter()
        .zip(attributes)
        .map(|(entry, attrs)| {
            if entry.kind == Kind::Submodule {
                Action::GitWrite
            } else if matches!(entry.kind, Kind::File { .. }) && attrs.unconverted(configuration) {
                Action::VerifySettled
            } else if entry.kind == Kind::Symlink {
                if configuration.native_symlinks() {
                    Action::VerifyClone
                } else {
                    Action::GitWrite
                }
            } else if attrs.identity(configuration) {
                Action::VerifyClone
            } else if attrs.auto_binary(configuration) {
                Action::VerifyAutoBinary
            } else if matches!(entry.kind, Kind::File { .. }) && !attrs.archive_differs {
                Action::VerifyConverted
            } else {
                Action::GitWrite
            }
        })
        .collect();
    Ok((actions, archivable))
}

impl Attributes {
    fn auto_binary(&self, config: &Configuration) -> bool {
        if self.legacy_crlf || self.builtin_conversion {
            return false;
        }
        // Git convert.c skips CRLF conversion for NUL-bearing blobs only in AUTO
        // modes. Explicit text/eol without text=auto can force conversion even
        // for binary data. Literal unset/unspecified values remain conservative.
        self.text.as_deref() == Some(b"auto")
            || (self.text.is_none()
                && self.eol.is_none()
                && matches!(config.autocrlf.as_slice(), b"true" | b"yes" | b"on" | b"1"))
    }

    /// No conversion when checking out and none when cleaning.
    fn unconverted(&self, config: &Configuration) -> bool {
        !self.legacy_crlf
            && !self.builtin_conversion
            && self.eol.is_none()
            && matches!(self.text.as_deref(), None | Some(b"unset" | b"unspecified"))
            && matches!(config.autocrlf.as_slice(), b"false" | b"no" | b"off" | b"0")
    }

    fn identity(&self, config: &Configuration) -> bool {
        if self.legacy_crlf || self.builtin_conversion {
            return false;
        }
        let text = self.text.as_deref();
        if !matches!(
            text,
            None | Some(b"set" | b"auto" | b"unset" | b"unspecified")
        ) {
            return false;
        }
        match self.eol.as_deref() {
            Some(b"lf") => return true,
            Some(_) => return false,
            None => {}
        }
        match config.autocrlf.as_slice() {
            b"input" => true,
            b"false" | b"no" | b"off" | b"0" => {
                // "unset" could be -text OR literal text=unset. Both are identity
                // with autocrlf=false and no eol/crlf, regardless of core.eol.
                // The native line ending is CRLF in Git for Windows.
                matches!(text, None | Some(b"unset" | b"unspecified"))
                    || config.eol == b"lf"
                    || (config.eol == b"native" && !cfg!(windows))
            }
            _ => false,
        }
    }
}

struct Configuration {
    autocrlf: Vec<u8>,
    eol: Vec<u8>,
    symlinks: bool,
}

impl Configuration {
    fn native_symlinks(&self) -> bool {
        cfg!(unix) && self.symlinks
    }

    fn parse(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.is_empty() || bytes.last() == Some(&0),
            "unterminated Git config"
        );
        let mut result = Self {
            autocrlf: b"false".to_vec(),
            eol: b"native".to_vec(),
            symlinks: true,
        };
        for field in bytes.split(|b| *b == 0).filter(|field| !field.is_empty()) {
            let split = field.iter().position(|b| *b == b'\n');
            let (key, value) =
                split.map_or((field, b"".as_slice()), |i| (&field[..i], &field[i + 1..]));
            ensure!(
                !key.starts_with(b"includeif."),
                "conditional config includes require stock Git"
            );
            match key {
                b"core.autocrlf" => result.autocrlf = value.to_ascii_lowercase(),
                b"core.eol" => result.eol = value.to_ascii_lowercase(),
                b"core.symlinks" => {
                    result.symlinks = !matches!(
                        value.to_ascii_lowercase().as_slice(),
                        b"false" | b"no" | b"off" | b"0"
                    )
                }
                b"core.attributesfile" => {
                    // ~ expansion and relative paths depend on Git context. Keep the
                    // first implementation to explicit absolute external policy paths.
                    ensure!(
                        PathBuf::from(native::os_string(value.to_vec())).is_absolute(),
                        "relative attribute files require stock Git"
                    );
                }
                _ => {}
            }
        }
        Ok(result)
    }
}

fn paths<'a>(paths: impl Iterator<Item = &'a Path>) -> Vec<u8> {
    paths
        .flat_map(|path| {
            let mut bytes = native::path_bytes(path);
            bytes.push(0);
            bytes
        })
        .collect()
}

fn checked_output(output: Output) -> Result<Output> {
    ensure!(
        output.status.success(),
        "Git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absence_is_not_a_literal_attribute_state() -> Result<()> {
        let entries =
            crate::tree::parse(b"100644 blob 0000000000000000000000000000000000000000 6\tfile\0")?;
        let config = Configuration::parse(b"")?;
        for value in ["unset", "unspecified", "set", "upper"] {
            assert!(
                parse_attributes(
                    &entries,
                    format!("file\0filter\0{value}\0").as_bytes(),
                    &config
                )
                .is_err()
            );
        }
        assert_eq!(
            parse_attributes(&entries, b"", &config)?.0,
            vec![Action::VerifySettled]
        );
        assert_eq!(
            parse_attributes(&entries, b"file\0text\0unset\0", &config)?.0,
            vec![Action::VerifySettled]
        );
        // Cleaning converts CRLF here: raw bytes certify, Git decides "unchanged".
        let input = Configuration::parse(b"core.autocrlf\ninput\0")?;
        assert_eq!(
            parse_attributes(&entries, b"", &input)?.0,
            vec![Action::VerifyClone]
        );
        assert_eq!(
            parse_attributes(&entries, b"file\0text\0auto\0file\0eol\0lf\0", &config)?.0,
            vec![Action::VerifyClone]
        );
        assert_eq!(
            parse_attributes(&entries, b"file\0text\0unset\0file\0eol\0crlf\0", &config)?.0,
            vec![Action::VerifyConverted]
        );
        // Archives expand or drop these paths; they are no oracle for checkout.
        for attribute in ["export-subst", "export-ignore"] {
            assert_eq!(
                parse_attributes(
                    &entries,
                    format!("file\0eol\0crlf\0file\0{attribute}\0set\0").as_bytes(),
                    &config
                )?
                .0,
                vec![Action::GitWrite]
            );
        }
        assert!(parse_attributes(&entries, b"other\0text\0auto\0", &config).is_err());
        assert!(parse_attributes(&entries, b"file\0text\0auto", &config).is_err());
        Ok(())
    }
}
