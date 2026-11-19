//! Prepare owned deterministic coverage fixtures; campaign scheduling lives in bench/run.ts.
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{
    env, fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::Command,
};
use twig_bench::{canonical, command};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Fixture {
    label: String,
    path: PathBuf,
    target_commit: String,
    donor_commit: Option<String>,
}

fn checked(command: &mut Command) -> Result<String> {
    let result = command.output()?;
    ensure!(
        result.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(String::from_utf8(result.stdout)?
        .trim_end_matches('\n')
        .into())
}
fn git(root: &Path, args: &[&str]) -> Result<String> {
    checked(
        command("git", root)
            .args(["-c", "core.hooksPath=/dev/null"])
            .args(args),
    )
}
fn commit(root: &Path, message: &str) -> Result<String> {
    git(root, &["add", "."])?;
    checked(
        command("git", root)
            .env("GIT_AUTHOR_DATE", "2026-11-17T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2026-11-17T00:00:00Z")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "user.name=Twig fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-m",
                message,
            ]),
    )?;
    git(root, &["rev-parse", "HEAD"])
}
fn clone_input(source: &Path, destination: &Path) -> Result<()> {
    checked(
        command(
            "git",
            destination.parent().context("fixture has no parent")?,
        )
        .args(["-c", "core.hooksPath=/dev/null", "clone", "--shared", "--"])
        .arg(source)
        .arg(destination),
    )?;
    Ok(())
}
fn fixtures(root: &Path, binaries: &Path) -> Result<Vec<Fixture>> {
    let inputs = root.join("inputs");
    fs::create_dir(&inputs)?;
    let bytes = inputs.join("bytes");
    let files = inputs.join("files");
    checked(
        command(binaries.join("twig-fixture"), root)
            .arg(&bytes)
            .args(["16", "4194304"]),
    )?;
    checked(
        command(binaries.join("twig-fixture"), root)
            .arg(&files)
            .args(["4096", "1024"]),
    )?;
    let byte_commit = git(&bytes, &["rev-parse", "HEAD"])?;
    let file_commit = git(&files, &["rev-parse", "HEAD"])?;
    let cross = inputs.join("cross");
    clone_input(&bytes, &cross)?;
    for i in 0..4 {
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(cross.join(format!("tree-000/file-{i:06}")))?;
        let mut byte = [0];
        file.read_exact(&mut byte)?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&[byte[0] ^ 0xff])?;
    }
    fs::write(cross.join(".gitattributes"), "* text=auto eol=lf\n")?;
    fs::write(cross.join("target-only"), b"new target file\n")?;
    let target = commit(
        &cross,
        "fixture: change four blobs and add LF target policy",
    )?;
    let mixed = inputs.join("mixed");
    clone_input(&bytes, &mixed)?;
    fs::write(
        mixed.join(".gitattributes"),
        "doc.id ident\ndoc.utf16 working-tree-encoding=UTF-16LE\n",
    )?;
    fs::write(mixed.join("doc.id"), "$Id$\n")?;
    let utf16: Vec<u8> = "hello\n"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    fs::write(mixed.join("doc.utf16"), utf16)?;
    let mixed_commit = commit(&mixed, "fixture: add builtin ident and UTF-16 exceptions")?;
    let auto = inputs.join("auto-crlf");
    clone_input(&bytes, &auto)?;
    fs::write(auto.join(".gitattributes"), "* text=auto eol=crlf\n")?;
    fs::write(auto.join("doc.txt"), "text must become CRLF\n")?;
    let auto_commit = commit(&auto, "fixture: binary sharing with auto CRLF text")?;
    let module = inputs.join("module-template");
    fs::create_dir(&module)?;
    git(&module, &["init", "-b", "main"])?;
    fs::write(
        module.join("library"),
        "nested module data is not checked out\n",
    )?;
    commit(&module, "fixture: nested module")?;
    let submodules = inputs.join("submodules");
    clone_input(&bytes, &submodules)?;
    checked(
        command("git", &submodules)
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "--",
            ])
            .arg(&module)
            .arg("sub"),
    )?;
    git(
        &submodules,
        &[
            "config",
            "--file",
            ".gitmodules",
            "submodule.sub.url",
            "../module-template",
        ],
    )?;
    let submodule_commit = commit(
        &submodules,
        "fixture: gitlink with shared application files",
    )?;
    // New capability scenarios go first; expensive many-file ramps come last.
    Ok(vec![
        Fixture {
            label: "cross-revision".into(),
            path: cross,
            target_commit: target,
            donor_commit: Some(byte_commit.clone()),
        },
        Fixture {
            label: "mixed-builtins".into(),
            path: mixed,
            target_commit: mixed_commit,
            donor_commit: None,
        },
        Fixture {
            label: "auto-crlf".into(),
            path: auto,
            target_commit: auto_commit,
            donor_commit: None,
        },
        Fixture {
            label: "submodules".into(),
            path: submodules,
            target_commit: submodule_commit,
            donor_commit: None,
        },
        Fixture {
            label: "byte-heavy".into(),
            path: bytes,
            target_commit: byte_commit,
            donor_commit: None,
        },
        Fixture {
            label: "file-heavy".into(),
            path: files,
            target_commit: file_commit,
            donor_commit: None,
        },
    ])
}

fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let root = PathBuf::from(
        args.next()
            .context("usage: twig-coverage-fixture NEW_ROOT")?,
    );
    ensure!(args.next().is_none(), "unexpected extra arguments");
    fs::create_dir(&root).context("fixture root must be new")?;
    let root = canonical(root)?;
    let binaries = env::current_exe()?
        .parent()
        .context("binary directory")?
        .to_path_buf();
    let values = fixtures(&root, &binaries)?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join("fixtures.json"))?;
    output.write_all(&serde_json::to_vec_pretty(&values)?)?;
    Ok(())
}
