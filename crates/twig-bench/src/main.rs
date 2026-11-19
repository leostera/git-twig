use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{
    env,
    ffi::OsString,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Output,
    sync::Arc,
    thread,
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use twig_bench::{canonical, command};

#[derive(Clone, Serialize)]
struct Config {
    files: usize,
    bytes_per_file: usize,
    samples: usize,
    concurrency: Vec<usize>,
    seed: u64,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Run {
    schema_version: u32,
    generated_at_unix_ms: u128,
    commit: String,
    dirty: bool,
    git_version: String,
    rust_version: String,
    platform: String,
    cpu: String,
    twig_version: String,
    candidate_sha256: String,
    fixture_tree: String,
    config: Config,
    notes: Vec<String>,
    points: Vec<Point>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Point {
    backend: String,
    concurrency: usize,
    sample: usize,
    wall_ms: f64,
    operations_per_second: f64,
    bytes_per_second: f64,
    operations: Vec<Operation>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Operation {
    create_ms: f64,
    first_status_ms: f64,
    warm_status_ms: f64,
    modified_status_ms: f64,
    verified_files: usize,
    source_unchanged: bool,
    correct: bool,
    native_cow: bool,
    error: Option<String>,
}
#[derive(Clone)]
struct Fixture {
    source: PathBuf,
    root: PathBuf,
    twig: PathBuf,
    config: Config,
}

fn main() -> Result<()> {
    let repository = env::current_dir()?;
    let mut config = Config {
        files: 64,
        bytes_per_file: 262_144,
        samples: 3,
        concurrency: vec![1, 2, 4],
        seed: 42,
    };
    let mut output = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--help" {
            println!(
                "cargo run --release -p twig-bench -- --files 64 --bytes 262144 --samples 3 --concurrency 1,2,4 --output bench/results/run.json\nFixtures are retained under target/bench-fixtures. Run from the repository root."
            );
            return Ok(());
        }
        let value = args.next().context("option requires a value")?;
        match arg.as_str() {
            "--files" => config.files = value.parse()?,
            "--bytes" => config.bytes_per_file = value.parse()?,
            "--samples" => config.samples = value.parse()?,
            "--concurrency" => {
                config.concurrency = value
                    .split(',')
                    .map(str::parse)
                    .collect::<std::result::Result<_, _>>()?
            }
            "--output" => output = Some(PathBuf::from(value)),
            _ => anyhow::bail!("unknown option: {arg}"),
        }
    }
    ensure!(
        config.files > 0
            && config.bytes_per_file >= 16
            && config.samples > 0
            && !config.concurrency.is_empty(),
        "positive sizes/samples required (bytes >= 16)"
    );
    ensure!(
        config.concurrency.iter().all(|n| *n > 0 && *n <= 128),
        "concurrency must be in 1..=128"
    );
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let output = output.unwrap_or_else(|| PathBuf::from(format!("bench/results/{timestamp}.json")));
    ensure!(!output.exists(), "refusing to overwrite an existing run");
    let root = repository.join(format!(
        "target/bench-fixtures/{timestamp}-{}",
        std::process::id()
    ));
    fs::create_dir_all(&root)?;
    let root = canonical(root)?;
    let source = root.join("source");
    fs::create_dir(&source)?;
    let twig = canonical(repository.join("target/release/git-twig"))
        .context("build cargo build --release --workspace first")?;
    let fixture = Arc::new(Fixture {
        root,
        source,
        twig,
        config: config.clone(),
    });
    let mut run = Run {
        schema_version: 2, generated_at_unix_ms: timestamp,
        commit: capture(&repository, "git", &["rev-parse", "HEAD"]).unwrap_or_else(|_| "uncommitted".into()),
        dirty: !capture(&repository, "git", &["status", "--porcelain"])?.is_empty(),
        git_version: capture(&repository, "git", &["--version"])?,
        rust_version: capture(&repository, "rustc", &["--version"] )?,
        platform: format!("{} {} {}", env::consts::OS, env::consts::ARCH, capture(&repository, "uname", &["-r"]) ?),
        cpu: capture(&repository, "sysctl", &["-n", "machdep.cpu.brand_string"]).unwrap_or_else(|_| "unknown".into()),
        twig_version: capture(&repository, fixture.twig.to_str().context("binary path is not UTF-8")?, &["--version"] )?,
        candidate_sha256: capture(&repository, "shasum", &["-a", "256", fixture.twig.to_str().context("binary path")?])?.split_whitespace().next().context("missing binary digest")?.into(),
        fixture_tree: String::new(),
        config: config.clone(), points: vec![], notes: vec![
            "Local real CLI benchmark, warm caches; no cold-cache claim. Baseline: installed git worktree add --detach; candidate: release git-twig new --detach.".into(),
            "One unmeasured warmup per backend. Backend order alternates per sample. Monotonic Instant measures CLI completion and first/warm/modified git status separately.".into(),
            "Full byte/mode-independent content verification after first/warm status. In-place write must leave source unchanged. Synthetic deterministic pseudo-random bytes; no compression/dedupe assumptions.".into(),
            "Every operation is retained on disk. Report exclusive physical storage as unmeasured. Logical fixture bytes are not APFS allocation.".into(),
            "All create/first-status/warm-status workers finish before any full-content verification or private-write check begins. Batch throughput excludes generator verification work, includes status calls.".into(),
            "Tail percentiles with few samples are descriptive only; errors or incorrect content block conclusions. No generator saturation claim at high concurrency.".into(),
        ],
    };
    save(&output, &run)?;
    seed(&fixture)?;
    run.fixture_tree = capture(&fixture.source, "git", &["rev-parse", "HEAD^{tree}"])?;
    save(&output, &run)?;
    eprintln!("fixtures retained: {}", fixture.root.display());
    for backend in ["git", "twig"] {
        let name = format!("warmup-{backend}");
        let mut result = operation(&fixture, backend, &name);
        verify_operation(&fixture, &name, &mut result);
        ensure!(result.correct, "warmup failed: {:?}", result.error);
    }
    for &concurrency in &config.concurrency {
        for sample in 0..config.samples {
            let backends = if sample % 2 == 0 {
                ["git", "twig"]
            } else {
                ["twig", "git"]
            };
            for backend in backends {
                eprintln!(
                    "{backend}: concurrency={concurrency} sample={}/{}",
                    sample + 1,
                    config.samples
                );
                let start = Instant::now();
                let handles: Vec<_> = (0..concurrency)
                    .map(|worker| {
                        let fixture = Arc::clone(&fixture);
                        thread::spawn(move || {
                            operation(
                                &fixture,
                                backend,
                                &format!("{backend}-c{concurrency}-s{sample}-w{worker}"),
                            )
                        })
                    })
                    .collect();
                let mut operations = handles
                    .into_iter()
                    .map(|h| h.join().expect("benchmark worker panicked"))
                    .collect::<Vec<_>>();
                let wall_ms = ms(start);
                for (worker, result) in operations.iter_mut().enumerate() {
                    verify_operation(
                        &fixture,
                        &format!("{backend}-c{concurrency}-s{sample}-w{worker}"),
                        result,
                    );
                }
                let failed = operations.iter().any(|op| !op.correct);
                let point = Point {
                    backend: backend.into(),
                    concurrency,
                    sample,
                    wall_ms,
                    operations_per_second: concurrency as f64 * 1000.0 / wall_ms,
                    bytes_per_second: concurrency as f64
                        * config.files as f64
                        * config.bytes_per_file as f64
                        * 1000.0
                        / wall_ms,
                    operations,
                };
                eprintln!(
                    "  {:.1} ms batch, {} errors",
                    wall_ms,
                    point
                        .operations
                        .iter()
                        .filter(|op| op.error.is_some())
                        .count()
                );
                run.points.push(point);
                save(&output, &run)?;
                ensure!(
                    !failed,
                    "correctness/error gate failed; raw result saved to {}",
                    output.display()
                );
            }
        }
    }
    println!("{}", output.display());
    Ok(())
}

fn seed(fixture: &Fixture) -> Result<()> {
    git(&fixture.source, &["init", "-b", "main"])?;
    git(&fixture.source, &["config", "user.name", "Benchmark"])?;
    git(
        &fixture.source,
        &["config", "user.email", "bench@example.invalid"],
    )?;
    for index in 0..fixture.config.files {
        let path = fixture.source.join(filename(index));
        fs::create_dir_all(path.parent().context("no fixture parent")?)?;
        fs::write(
            path,
            contents(fixture.config.seed, index, fixture.config.bytes_per_file),
        )?;
    }
    git(&fixture.source, &["add", "."])?;
    git(
        &fixture.source,
        &["commit", "-m", "deterministic benchmark fixture"],
    )?;
    Ok(())
}
fn filename(index: usize) -> String {
    format!("group-{}/file-{index}.bin", index / 100)
}
fn contents(seed: u64, index: usize, size: usize) -> Vec<u8> {
    let mut state = seed.wrapping_add(index as u64 + 1);
    (0..size)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}
fn operation(fixture: &Fixture, backend: &str, name: &str) -> Operation {
    let mut result = Operation {
        create_ms: 0.0,
        first_status_ms: 0.0,
        warm_status_ms: 0.0,
        modified_status_ms: 0.0,
        verified_files: 0,
        source_unchanged: false,
        correct: false,
        native_cow: false,
        error: None,
    };
    if let Err(error) = measure(fixture, backend, name, &mut result) {
        result.error = Some(format!("{error:#}"));
    }
    result
}
fn measure(fixture: &Fixture, backend: &str, name: &str, result: &mut Operation) -> Result<()> {
    let destination = fixture.root.join(name);
    let mut command = if backend == "git" {
        command("git", &fixture.source)
    } else {
        command(&fixture.twig, &fixture.source)
    };
    if backend == "git" {
        command.arg("worktree");
    }
    command.args([
        OsString::from(if backend == "git" { "add" } else { "new" }),
        OsString::from("--detach"),
        destination.as_os_str().into(),
        OsString::from("HEAD"),
    ]);
    let start = Instant::now();
    let output = command.output()?;
    result.create_ms = ms(start);
    let output = checked(output)?;
    result.native_cow =
        String::from_utf8_lossy(&output.stderr).contains("CoW: verified tracked files");
    ensure!(
        backend != "twig"
            || !cfg!(any(target_os = "macos", target_os = "linux"))
            || result.native_cow,
        "candidate silently fell back to Git: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    for warm in [false, true] {
        let start = Instant::now();
        let status = git(
            &destination,
            &["status", "--porcelain", "--untracked-files=no"],
        )?;
        let elapsed = ms(start);
        if warm {
            result.warm_status_ms = elapsed;
        } else {
            result.first_status_ms = elapsed;
        }
        ensure!(status.stdout.is_empty(), "new checkout is dirty");
    }
    Ok(())
}

fn verify_operation(fixture: &Fixture, name: &str, result: &mut Operation) {
    if result.error.is_some() {
        return;
    }
    if let Err(error) = verify_files(fixture, name, result) {
        result.error = Some(format!("{error:#}"));
    }
}

fn verify_files(fixture: &Fixture, name: &str, result: &mut Operation) -> Result<()> {
    let destination = fixture.root.join(name);
    for index in 0..fixture.config.files {
        let actual = fs::read(destination.join(filename(index)))?;
        ensure!(
            actual == contents(fixture.config.seed, index, fixture.config.bytes_per_file),
            "file {index} content differs"
        );
        result.verified_files += 1;
    }
    let first = filename(0);
    let mut file = OpenOptions::new()
        .write(true)
        .open(destination.join(&first))?;
    file.write_all(b"private!")?;
    drop(file);
    let start = Instant::now();
    let modified = git(
        &destination,
        &["status", "--porcelain", "--untracked-files=no"],
    )?;
    result.modified_status_ms = ms(start);
    ensure!(
        !modified.stdout.is_empty(),
        "Git failed to detect private write"
    );
    result.source_unchanged = fs::read(fixture.source.join(first))?
        == contents(fixture.config.seed, 0, fixture.config.bytes_per_file);
    ensure!(
        result.source_unchanged,
        "source changed after private write"
    );
    result.correct = true;
    Ok(())
}
fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}
fn checked(output: Output) -> Result<Output> {
    ensure!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output)
}
fn git(cwd: &Path, args: &[&str]) -> Result<Output> {
    checked(command("git", cwd).args(args).output()?)
}
fn capture(cwd: &Path, program: &str, args: &[&str]) -> Result<String> {
    Ok(
        String::from_utf8(checked(command(program, cwd).args(args).output()?)?.stdout)?
            .trim()
            .to_owned(),
    )
}
fn save(path: &Path, run: &Run) -> Result<()> {
    fs::create_dir_all(path.parent().context("output needs a parent directory")?)?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(run)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}
