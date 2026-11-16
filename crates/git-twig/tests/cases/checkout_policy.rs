//! Differential checkout tests: stock Git's raw working tree is the oracle.
use super::*;
#[cfg_attr(not(unix), allow(unused_imports))]
use std::io::{Read, Write};

const LF: &[u8] = b"hello\n";
const CRLF: &[u8] = b"hello\r\n";

struct Case {
    name: &'static str,
    attributes: &'static str,
    blob: &'static [u8],
    donor: &'static [u8],
    config: &'static [(&'static str, &'static str)],
}

fn commit_blob(f: &Fixture, path: &str, bytes: &[u8], mode: &str) -> Result<()> {
    let mut child = command("git", &f.source)
        .args(["hash-object", "-w", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    child.stdin.take().unwrap().write_all(bytes)?;
    let oid = String::from_utf8(checked(child.wait_with_output()?)?.stdout)?;
    git(
        &f.source,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            mode,
            oid.trim(),
            path,
        ],
    )?;
    git(
        &f.source,
        &["commit", "--allow-empty", "-m", "raw target blob"],
    )?;
    Ok(())
}

fn compare_stock(f: &Fixture, paths: &[&str], require_native: bool) -> Result<Output> {
    git(
        &f.source,
        &["worktree", "add", "--detach", "../stock", "HEAD"],
    )?;
    let output = checked(f.add()?)?;
    if require_native {
        native(&output);
    }
    for name in paths {
        assert_eq!(
            fs::read(f.destination().join(name))?,
            fs::read(f.root.join("stock").join(name))?,
            "{name}"
        );
    }
    assert_eq!(
        git(&f.destination(), &["status", "--porcelain"])?.stdout,
        git(&f.root.join("stock"), &["status", "--porcelain"])?.stdout
    );
    Ok(output)
}

#[test]
fn checkout_bytes_match_git_across_line_ending_policies() -> Result<()> {
    let cases = [
        Case {
            name: "auto LF",
            attributes: "* text=auto eol=lf\n",
            blob: LF,
            donor: LF,
            config: &[],
        },
        Case {
            name: "normalized clean CRLF donor",
            attributes: "* text=auto eol=lf\n",
            blob: LF,
            donor: CRLF,
            config: &[],
        },
        Case {
            name: "normalized clean mixed donor",
            attributes: "* text=auto eol=lf\n",
            blob: b"a\nb\n",
            donor: b"a\r\nb\n",
            config: &[],
        },
        Case {
            name: "legacy CRLF blob",
            attributes: "* text=auto eol=lf\n",
            blob: CRLF,
            donor: CRLF,
            config: &[],
        },
        Case {
            name: "forced text legacy CRLF",
            attributes: "* text eol=lf\n",
            blob: CRLF,
            donor: CRLF,
            config: &[],
        },
        Case {
            name: "binary",
            attributes: "* text=auto eol=lf\n",
            blob: b"\0binary\r\n",
            donor: b"\0binary\r\n",
            config: &[],
        },
        Case {
            name: "bare CR",
            attributes: "* text=auto eol=lf\n",
            blob: b"a\rb\n",
            donor: b"a\rb\n",
            config: &[],
        },
        Case {
            name: "empty",
            attributes: "* text=auto eol=lf\n",
            blob: b"",
            donor: b"",
            config: &[],
        },
        Case {
            name: "forced LF",
            attributes: "* text eol=lf\n",
            blob: LF,
            donor: LF,
            config: &[],
        },
        Case {
            name: "CRLF needs checkout",
            attributes: "hello.txt text eol=crlf\n",
            blob: LF,
            donor: LF,
            config: &[],
        },
        Case {
            name: "CRLF already checked out",
            attributes: "hello.txt text eol=crlf\n",
            blob: LF,
            donor: CRLF,
            config: &[],
        },
        Case {
            name: "automatic CRLF mixed blob",
            attributes: "* text=auto eol=crlf\n",
            blob: b"a\r\nb\n",
            donor: b"a\r\nb\n",
            config: &[],
        },
        Case {
            name: "binary disables CRLF",
            attributes: "hello.txt -text eol=crlf\n",
            blob: LF,
            donor: LF,
            config: &[],
        },
        Case {
            name: "literal unset is not -text",
            attributes: "hello.txt text=unset eol=crlf\n",
            blob: LF,
            donor: LF,
            config: &[],
        },
        Case {
            name: "input",
            attributes: "",
            blob: LF,
            donor: CRLF,
            config: &[("core.autocrlf", "input")],
        },
        Case {
            name: "true",
            attributes: "",
            blob: LF,
            donor: LF,
            config: &[("core.autocrlf", "true")],
        },
        Case {
            name: "LF overrides true",
            attributes: "* text=auto eol=lf\n",
            blob: LF,
            donor: LF,
            config: &[("core.autocrlf", "true")],
        },
        Case {
            name: "config CRLF",
            attributes: "* text\n",
            blob: LF,
            donor: LF,
            config: &[("core.eol", "crlf")],
        },
        Case {
            name: "boolean synonym",
            attributes: "* text\n",
            blob: LF,
            donor: LF,
            config: &[("core.autocrlf", "off")],
        },
        Case {
            name: "macro",
            attributes: "[attr]lftext text=auto eol=lf\n* lftext\n",
            blob: LF,
            donor: LF,
            config: &[],
        },
    ];
    for case in cases {
        eprintln!("checkout case: {}", case.name);
        let f = Fixture::new()?;
        for (key, value) in case.config {
            git(&f.source, &["config", key, value])?;
        }
        if !case.attributes.is_empty() {
            fs::write(f.source.join(".gitattributes"), case.attributes)?;
            git(&f.source, &["add", ".gitattributes"])?;
        }
        commit_blob(&f, "hello.txt", case.blob, "100644")?;
        fs::write(f.source.join("hello.txt"), case.donor)?;
        compare_stock(&f, &["hello.txt", ".gitignore"], true)?;
        assert_eq!(fs::read(f.source.join("hello.txt"))?, case.donor);
        fs::write(f.destination().join("hello.txt"), "private")?;
        assert_eq!(fs::read(f.source.join("hello.txt"))?, case.donor);
        assert!(f.stages()?.is_empty());
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn oversized_dirty_sparse_donor_is_repaired_without_reading_its_apparent_size() -> Result<()> {
    let f = Fixture::new()?;
    // The 64-GiB verifier regression matters on native clone filesystems.
    // Some fallback filesystems (notably ntfs3) cannot represent that sparse
    // shape on the bounded test volume. Keep testing dirty-byte preservation
    // there with a bounded length mismatch, without weakening native coverage.
    #[cfg(target_os = "linux")]
    let native_donor = linux_reflinks_available(&f)?;
    #[cfg(not(target_os = "linux"))]
    let native_donor = cfg!(target_os = "macos");
    let donor_length = if native_donor {
        64 * 1024 * 1024 * 1024
    } else {
        8 * 1024 * 1024
    };
    let file = fs::OpenOptions::new()
        .write(true)
        .open(f.source.join("hello.txt"))?;
    file.set_len(donor_length)?;
    let mut child = command(env!("CARGO_BIN_EXE_git-twig"), &f.source);
    child.args(["new", "--detach", "../feature", "HEAD"]);
    let output = checked(bounded_output(child)?)?;
    native(&output);
    assert_eq!(fs::read(f.destination().join("hello.txt"))?, LF);
    assert_eq!(
        fs::metadata(f.source.join("hello.txt"))?.len(),
        donor_length
    );
    let mut prefix = [0; 6];
    fs::File::open(f.source.join("hello.txt"))?.read_exact(&mut prefix)?;
    assert_eq!(&prefix, LF);
    clean(&f.destination())
}

#[test]
fn parallel_raw_hashing_preserves_order_formats_and_dirty_file_repairs() -> Result<()> {
    for format in ["sha1", "sha256"] {
        let f = Fixture::with_object_format(format)?;
        fs::create_dir(f.source.join("files"))?;
        let content = |index: usize| {
            let mut bytes = vec![b'x'; 128 * 1024];
            bytes[..8].copy_from_slice(&(index as u64).to_le_bytes());
            bytes
        };
        // Keep >512 eligible files and >64 MiB even after rejecting a dirty
        // length mismatch. Distinct object IDs make reordered results visible.
        for index in 0..520 {
            fs::write(f.source.join(format!("files/{index:04}")), content(index))?;
        }
        git(&f.source, &["add", "."])?;
        git(&f.source, &["commit", "-m", "parallel raw verification"])?;
        fs::write(f.source.join("files/0001"), vec![b'y'; 128 * 1024])?;
        fs::OpenOptions::new()
            .append(true)
            .open(f.source.join("files/0002"))?
            .write_all(b"extra")?;
        let output = checked(f.add()?)?;
        native(&output);
        let trace = String::from_utf8_lossy(&output.stderr);
        if trace.contains("raw verification:") {
            let workers = std::thread::available_parallelism()
                .map_or(1, usize::from)
                .min(4);
            assert!(trace.contains(&format!("raw verification: {workers} Git hash workers")));
        }
        for index in 0..520 {
            assert_eq!(
                fs::read(f.destination().join(format!("files/{index:04}")))?,
                content(index)
            );
        }
        assert_eq!(
            fs::read(f.source.join("files/0001"))?,
            vec![b'y'; 128 * 1024]
        );
        fs::write(f.destination().join("files/0500"), "private")?;
        assert_eq!(fs::read(f.source.join("files/0500"))?, content(500));
    }
    Ok(())
}

#[test]
fn target_attributes_ignore_dirty_staged_and_untracked_source_policies() -> Result<()> {
    for kind in ["dirty", "staged", "untracked", "nested-untracked"] {
        let f = Fixture::new()?;
        fs::create_dir_all(f.source.join("tree"))?;
        fs::write(f.source.join("tree/file"), LF)?;
        if matches!(kind, "dirty" | "staged") {
            fs::write(f.source.join(".gitattributes"), "* text=auto eol=lf\n")?;
        }
        git(&f.source, &["add", "."])?;
        git(&f.source, &["commit", "-m", "target policy"])?;
        let policy = if kind == "nested-untracked" {
            "tree/.gitattributes"
        } else {
            ".gitattributes"
        };
        fs::write(f.source.join(policy), "* text eol=crlf\n")?;
        if kind == "staged" {
            git(&f.source, &["add", ".gitattributes"])?;
        }
        fs::write(f.source.join("hello.txt"), CRLF)?;
        fs::write(f.source.join("tree/file"), CRLF)?;
        let before = git(&f.source, &["status", "--porcelain"])?.stdout;
        compare_stock(&f, &["hello.txt", "tree/file"], true)?;
        assert_eq!(git(&f.source, &["status", "--porcelain"])?.stdout, before);
        if matches!(kind, "untracked" | "nested-untracked") {
            assert!(!f.destination().join(policy).exists());
        } else {
            assert_eq!(
                fs::read(f.destination().join(policy))?,
                b"* text=auto eol=lf\n"
            );
        }
    }
    Ok(())
}

#[test]
fn nested_info_global_and_worktree_configuration_match_git() -> Result<()> {
    for kind in ["nested", "info", "global", "worktree"] {
        let f = Fixture::new()?;
        fs::create_dir_all(f.source.join("tree"))?;
        fs::write(f.source.join("tree/file"), LF)?;
        fs::write(f.source.join(".gitattributes"), "* text=auto eol=lf\n")?;
        if kind == "nested" {
            fs::write(f.source.join("tree/.gitattributes"), "file text eol=crlf\n")?;
        }
        git(&f.source, &["add", "."])?;
        git(&f.source, &["commit", "-m", "attributes"])?;
        match kind {
            "info" => fs::write(
                f.source.join(".git/info/attributes"),
                "tree/file text eol=crlf\n",
            )?,
            "global" => {
                // Override root policy's eol with unspecified so global can supply text
                // for an otherwise unclassified path in this dedicated fixture.
                fs::write(
                    f.source.join(".gitattributes"),
                    "hello.txt text=auto eol=lf\n",
                )?;
                git(&f.source, &["add", "."])?;
                git(&f.source, &["commit", "-m", "narrow root policy"])?;
                let global = f.root.join("global-attributes");
                fs::write(&global, "tree/file text eol=crlf\n")?;
                git(
                    &f.source,
                    &["config", "core.attributesFile", global.to_str().unwrap()],
                )?;
            }
            "worktree" => {
                fs::write(f.source.join(".gitattributes"), "* text\n")?;
                git(&f.source, &["add", "."])?;
                git(&f.source, &["commit", "-m", "config selects endings"])?;
                git(&f.source, &["config", "extensions.worktreeConfig", "true"])?;
                git(&f.source, &["config", "core.eol", "lf"])?;
                git(&f.source, &["config", "--worktree", "core.eol", "crlf"])?;
            }
            _ => {}
        }
        compare_stock(&f, &["hello.txt", "tree/file"], true)?;
    }
    Ok(())
}

#[test]
fn copied_worktree_config_keeps_absolute_includes_but_delegates_relative_ones() -> Result<()> {
    for relative in [false, true] {
        let f = Fixture::new()?;
        fs::write(f.source.join(".gitattributes"), "* text\n")?;
        git(&f.source, &["add", "."])?;
        git(&f.source, &["commit", "-m", "worktree include policy"])?;
        git(&f.source, &["config", "extensions.worktreeConfig", "true"])?;
        git(&f.source, &["config", "core.eol", "lf"])?;
        let included = f.source.join(".git/source-only.inc");
        fs::write(&included, "[core]\n eol = crlf\n")?;
        let include_path = if relative {
            "source-only.inc"
        } else {
            included.to_str().unwrap()
        };
        git(
            &f.source,
            &["config", "--worktree", "include.path", include_path],
        )?;
        assert_eq!(
            git(&f.source, &["config", "--get", "core.eol"])?.stdout,
            b"crlf\n"
        );
        // Source is a canonical CRLF checkout under its own effective config.
        fs::write(f.source.join("hello.txt"), CRLF)?;
        let output = compare_stock(&f, &["hello.txt", ".gitattributes"], !relative)?;
        if relative {
            let trace = String::from_utf8_lossy(&output.stderr);
            assert!(
                trace.contains("relative config.worktree includes require stock Git"),
                "{trace}"
            );
            assert!(!trace.contains("CoW: verified"), "{trace}");
            assert!(!f.staging_root().exists());
            assert_eq!(fs::read(f.destination().join("hello.txt"))?, LF);
        } else {
            assert_eq!(fs::read(f.destination().join("hello.txt"))?, CRLF);
        }
        assert_eq!(fs::read(f.source.join("hello.txt"))?, CRLF);
    }
    Ok(())
}

#[test]
fn auto_crlf_keeps_verified_nul_blobs_but_still_converts_text() -> Result<()> {
    for configured in [false, true] {
        for dirty in [false, true] {
            let f = Fixture::new()?;
            if configured {
                git(&f.source, &["config", "core.autocrlf", "true"])?;
            } else {
                fs::write(f.source.join(".gitattributes"), "* text=auto eol=crlf\n")?;
            }
            let mut blob = b"binary-looking-late\n".repeat(8000);
            blob.push(0); // Past multiple comparison buffers; inspect the target, not a prefix.
            fs::write(f.source.join("binary.bin"), &blob)?;
            git(&f.source, &["add", "."])?;
            git(&f.source, &["commit", "-m", "auto binary and text"])?;
            if dirty {
                *blob.last_mut().unwrap() = b'x';
                fs::write(f.source.join("binary.bin"), &blob)?;
            }
            let output = compare_stock(&f, &["binary.bin", "hello.txt"], true)?;
            assert_eq!(fs::read(f.destination().join("hello.txt"))?, CRLF);
            let plain = if configured { 2 } else { 3 };
            let trace = String::from_utf8_lossy(&output.stderr);
            if trace.contains("CoW: verified") {
                let clones = usize::from(!dirty);
                let written = plain + usize::from(dirty);
                assert!(
                    trace.contains(&format!(
                        "checkout: {clones} verified file clones, {written} Git-written paths"
                    )),
                    "{trace}"
                );
            }
            assert_eq!(fs::read(f.source.join("binary.bin"))?, blob);
        }
    }
    Ok(())
}

/// Git for Windows converts every text file by default. Donor files checked out
/// under the same conversion hold exactly the bytes Git writes; those are
/// shared, while a stale donor file and paths an archive cannot vouch for are
/// written by Git.
#[test]
fn converted_donor_bytes_are_shared_when_they_equal_git_checkout() -> Result<()> {
    type Config = &'static [(&'static str, &'static str)];
    let cases: [(&str, Config, &str); 5] = [
        ("autocrlf", &[("core.autocrlf", "true")], ""),
        ("text eol=crlf", &[], "*.txt text eol=crlf\n"),
        ("legacy crlf", &[("core.eol", "crlf")], "*.txt crlf\n"),
        ("ident", &[], "*.txt ident\n"),
        ("encoding", &[], "*.txt working-tree-encoding=UTF-16LE\n"),
    ];
    for (name, config, attributes) in cases {
        eprintln!("converted case: {name}");
        let f = Fixture::new()?;
        for (key, value) in config {
            git(&f.source, &["config", key, value])?;
        }
        let attributes = format!("{attributes}subst.txt export-subst\n");
        fs::write(f.source.join(".gitattributes"), &attributes)?;
        fs::create_dir(f.source.join("tree"))?;
        for path in ["a.txt", "tree/b.txt", "tree/stale.txt"] {
            fs::write(f.source.join(path), "$Id$\nfirst\nsecond\n")?;
        }
        fs::write(f.source.join("subst.txt"), "$Format:%H$\n")?;
        git(&f.source, &["add", "."])?;
        git(&f.source, &["commit", "-m", "converted"])?;
        // Check the donor out through the same conversion, as a clone would be.
        let tracked = [
            ".gitattributes",
            ".gitignore",
            "a.txt",
            "tree/b.txt",
            "tree/stale.txt",
            "subst.txt",
            "hello.txt",
        ];
        for path in tracked {
            fs::remove_file(f.source.join(path))?;
        }
        git(&f.source, &["checkout", "--", "."])?;
        let converted = fs::read(f.source.join("a.txt"))?;
        fs::write(f.source.join("tree/stale.txt"), "$Id$\nfirst\nsecond\n")?;
        let output = compare_stock(&f, &tracked, true)?;
        let trace = String::from_utf8_lossy(&output.stderr);
        if cow_available(&f)? {
            // Archived: every converted file but subst.txt, which archives
            // expand. autocrlf converts all six others; the attributes, the
            // four *.txt files. stale.txt is unconverted, so it differs from
            // Git's bytes unless the conversion round-trips it unchanged.
            let candidates = if name == "autocrlf" { 6 } else { 4 };
            let stale = usize::from(
                fs::read(f.root.join("stock/tree/stale.txt"))? != b"$Id$\nfirst\nsecond\n",
            );
            assert!(
                trace.contains(&format!(
                    "converted verification: {} of {candidates} files equal Git's checkout bytes",
                    candidates - stale
                )),
                "{trace}"
            );
            assert!(
                trace.contains(&format!(
                    "checkout: {} verified file clones, {} Git-written paths",
                    tracked.len() - 1 - stale,
                    1 + stale
                )),
                "{trace}"
            );
        }
        fs::write(f.destination().join("a.txt"), "private\n")?;
        assert_eq!(fs::read(f.source.join("a.txt"))?, converted);
    }
    Ok(())
}

#[test]
fn forced_text_does_not_get_the_auto_binary_exemption() -> Result<()> {
    let f = Fixture::new()?;
    fs::write(
        f.source.join(".gitattributes"),
        "binary.bin text eol=crlf\n",
    )?;
    fs::write(f.source.join("binary.bin"), b"a\0\nb\n")?;
    git(&f.source, &["add", "."])?;
    git(&f.source, &["commit", "-m", "forced binary text"])?;
    let output = compare_stock(&f, &["binary.bin"], true)?;
    assert_eq!(
        fs::read(f.destination().join("binary.bin"))?,
        b"a\0\r\nb\r\n"
    );
    let trace = String::from_utf8_lossy(&output.stderr);
    if trace.contains("CoW: verified") {
        assert!(
            trace.contains("checkout: 3 verified file clones, 1 Git-written paths"),
            "{trace}"
        );
    }
    Ok(())
}

#[test]
fn builtin_conversions_are_hybrid_but_external_filters_still_delegate() -> Result<()> {
    for attributes in [
        "hello.txt ident\n",
        "hello.txt working-tree-encoding=UTF-16LE\n",
        "hello.txt -filter\n",
    ] {
        let f = Fixture::new()?;
        fs::write(f.source.join(".gitattributes"), attributes)?;
        git(&f.source, &["add", ".gitattributes"])?;
        commit_blob(&f, "hello.txt", b"$Id$\n", "100644")?;
        fs::write(f.source.join("hello.txt"), b"$Id$\n")?;
        let external = attributes.contains("filter");
        let output = compare_stock(&f, &["hello.txt"], !external)?;
        if external {
            assert!(!String::from_utf8_lossy(&output.stderr).contains("CoW: verified"));
        }
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn hybrid_preserves_odd_paths_modes_and_symlink_targets() -> Result<()> {
    use std::os::unix::fs::{PermissionsExt, symlink};
    for format in ["sha1", "sha256"] {
        let f = Fixture::with_object_format(format)?;
        fs::write(f.source.join(".gitattributes"), "* text=auto eol=lf\n")?;
        let odd = " leading\tname\nfile";
        fs::write(f.source.join(odd), LF)?;
        fs::set_permissions(f.source.join(odd), fs::Permissions::from_mode(0o755))?;
        symlink("missing\nlink", f.source.join("link"))?;
        git(&f.source, &["add", "."])?;
        git(&f.source, &["commit", "-m", "odd paths"])?;
        fs::write(f.source.join(odd), CRLF)?;
        fs::rename(f.source.join("link"), f.root.join("saved-link"))?;
        symlink("different", f.source.join("link"))?;
        compare_stock(&f, &[odd], true)?;
        assert_eq!(
            fs::read_link(f.destination().join("link"))?,
            Path::new("missing\nlink")
        );
        assert_eq!(
            fs::read_link(f.source.join("link"))?,
            Path::new("different")
        );
        assert_eq!(
            fs::metadata(f.destination().join(odd))?
                .permissions()
                .mode()
                & 0o777,
            fs::metadata(f.root.join("stock").join(odd))?
                .permissions()
                .mode()
                & 0o777
        );
    }
    Ok(())
}

#[test]
fn failing_filters_run_once_through_stock_git_and_retain_failure() -> Result<()> {
    let f = Fixture::new()?;
    let marker = f.root.join("filter-calls");
    fs::write(
        f.source.join(".gitattributes"),
        "hello.txt filter=unspecified\n",
    )?;
    git(&f.source, &["add", ".gitattributes"])?;
    git(&f.source, &["commit", "-m", "required filter"])?;
    git(
        &f.source,
        &["config", "filter.unspecified.required", "true"],
    )?;
    git(
        &f.source,
        &[
            "config",
            "filter.unspecified.smudge",
            &format!("echo call >> '{}'; exit 1", marker.display()),
        ],
    )?;
    let output = f.add()?;
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("CoW: verified"));
    assert_eq!(fs::read_to_string(marker)?, "call\n");
    assert_eq!(fs::read(f.source.join("hello.txt"))?, LF);
    // Staging exists only where Twig got as far as preparing one.
    if cow_available(&f)? {
        assert!(!f.stages()?.is_empty());
    }
    Ok(())
}

#[test]
fn conditional_includes_and_redirected_attribute_sources_delegate() -> Result<()> {
    let f = Fixture::new()?;
    let include = f.root.join("conditional-config");
    fs::write(&include, "[core]\n eol = crlf\n")?;
    git(
        &f.source,
        &[
            "config",
            "includeIf.onbranch:feature.path",
            include.to_str().unwrap(),
        ],
    )?;
    fs::write(f.source.join(".gitattributes"), "* text\n")?;
    git(&f.source, &["add", "."])?;
    git(&f.source, &["commit", "-m", "branch-sensitive checkout"])?;
    let output = checked(f.add()?)?;
    assert!(!String::from_utf8_lossy(&output.stderr).contains("CoW: verified"));
    assert_eq!(fs::read(f.destination().join("hello.txt"))?, CRLF);
    Ok(())
}

#[test]
fn conditional_includes_of_identity_settings_stay_native() -> Result<()> {
    let f = Fixture::new()?;
    let include = f.root.join("identity-config");
    fs::write(
        &include,
        "[user]\n name = Other\n[url \"git@example.invalid:\"]\n insteadOf = https://example.invalid/\n",
    )?;
    git(
        &f.source,
        &[
            "config",
            "includeIf.onbranch:feature.path",
            include.to_str().unwrap(),
        ],
    )?;
    git(
        &f.source,
        &[
            "config",
            "includeIf.gitdir:/nonexistent/.path",
            f.root.join("missing").to_str().unwrap(),
        ],
    )?;
    let output = checked(f.add()?)?;
    native(&output);
    assert_eq!(fs::read(f.destination().join("hello.txt"))?, LF);
    Ok(())
}

#[test]
fn relative_conditional_includes_delegate() -> Result<()> {
    let f = Fixture::new()?;
    git(
        &f.source,
        &[
            "config",
            "includeIf.onbranch:feature.path",
            "identity-config",
        ],
    )?;
    let output = checked(f.add()?)?;
    let trace = String::from_utf8_lossy(&output.stderr);
    assert!(!trace.contains("CoW: verified"), "{trace}");
    assert!(
        trace.contains("relative conditional config includes"),
        "{trace}"
    );
    Ok(())
}
