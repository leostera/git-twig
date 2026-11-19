//! Real-repository cohorts: isolated images for allocation, host APFS for representative timing.
//! Only image-owned paths are removed. Original repositories are read-only inputs.
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{
    env,
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use twig_bench::command;

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Filesystem {
    IsolatedImage,
    CaseSensitiveImage,
    Host,
    LinuxXfsImage,
    LinuxBtrfsImage,
}

impl Filesystem {
    fn snapshot(self, path: &Path) -> Result<Option<Space>> {
        match self {
            Self::IsolatedImage
            | Self::CaseSensitiveImage
            | Self::LinuxXfsImage
            | Self::LinuxBtrfsImage => Ok(Some(settled_space(path)?)),
            Self::Host => Ok(None),
        }
    }
}

#[derive(Clone, Serialize)]
struct Repository {
    label: String,
    #[serde(skip)]
    path: PathBuf,
    commit: String,
    donor_commit: Option<String>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Suite {
    schema_version: u32,
    filesystem: Filesystem,
    image_size_gib: Option<u64>,
    allocation_sampling: Option<AllocationSampling>,
    warmup: bool,
    require_native: bool,
    selected_backend: Option<String>,
    suite: String,
    generated_at_unix_ms: u128,
    commit: String,
    dirty: bool,
    candidate_sha256: String,
    git_version: String,
    platform: String,
    cpu: String,
    concurrency: usize,
    worktrees_per_cohort: usize,
    repeats: usize,
    repositories: Vec<Repository>,
    notes: Vec<String>,
    cohorts: Vec<Cohort>,
    failures: Vec<String>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Cohort {
    repository: String,
    source_commit: String,
    source_tree: String,
    target_commit: String,
    target_tree: String,
    backend: String,
    repeat: usize,
    tracked_files: usize,
    logical_bytes_per_worktree: u64,
    create_wall_ms: f64,
    harness_cpu_ms_during_create: f64,
    observer_wall_ms: f64,
    creates_per_second: f64,
    status_and_verification_wall_ms: f64,
    succeeded: usize,
    failed: usize,
    verified_files: usize,
    source_unchanged: bool,
    private_write_tested: bool,
    warmup_cleanup_recovered: bool,
    private_write_file_bytes: Option<usize>,
    private_write_offset: Option<usize>,
    before: Option<Space>,
    after_create: Option<Space>,
    after_status: Option<Space>,
    after_private_writes: Option<Space>,
    growth_after_create_bytes: Option<i128>,
    growth_after_status_bytes: Option<i128>,
    private_write_growth_bytes: Option<i128>,
    operations: Vec<Operation>,
}
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Space {
    allocated_bytes: u64,
    total_bytes: u64,
    stable: bool,
    observed_allocated_bytes: Vec<u64>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Operation {
    worker: usize,
    create_ms: f64,
    create_started_at_ms: f64,
    create_finished_at_ms: f64,
    first_status_ms: Option<f64>,
    exit_code: Option<i32>,
    native_cow: bool,
    verified_clones: Option<usize>,
    git_written_paths: Option<usize>,
    verified: bool,
    error: Option<String>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreationProgress<'a> {
    schema_version: u32,
    phase: &'static str,
    repository: &'a str,
    source_commit: &'a str,
    target_commit: &'a str,
    backend: &'a str,
    repeat: usize,
    expected_operations: usize,
    elapsed_ms: f64,
    operations: &'a [Operation],
}

struct Expected {
    path: PathBuf,
    contents: Contents,
}
enum Contents {
    File { data: Vec<u8>, executable: bool },
    Symlink(Vec<u8>),
    GitlinkDirectory,
}
impl Expected {
    fn regular_bytes(&self) -> Option<&[u8]> {
        match &self.contents {
            Contents::File { data, .. } => Some(data),
            _ => None,
        }
    }
    fn logical_bytes(&self) -> u64 {
        match &self.contents {
            Contents::File { data, .. } | Contents::Symlink(data) => data.len() as u64,
            Contents::GitlinkDirectory => 0,
        }
    }
}
#[derive(Clone, Copy)]
struct CohortOptions {
    filesystem: Filesystem,
    warmup: bool,
    require_native: bool,
    worktrees: usize,
}

struct Image {
    path: PathBuf,
    mount: PathBuf,
    attached: bool,
    filesystem: Filesystem,
    size_gib: u64,
}

fn main() -> Result<()> {
    if env::args().any(|arg| arg == "--help") {
        println!(
            "twig-real-bench --repo label=/absolute/repo [--repo ...] --concurrency 100 --repeats 3 --output target/real-results/run.json\nSequential example: --worktrees 10 --concurrency 1 --repeats 1\nOptional: --filesystem host (retains fixtures, does not measure allocation), --revision label=COMMIT (target, after --repo), --donor-revision label=COMMIT (different source HEAD), --require-native false (explicit fallback baseline only).\nmacOS: --filesystem isolated-image or case-sensitive-image. Linux: --filesystem linux-xfs-image or linux-btrfs-image (mkfs + noninteractive sudo mount/umount required). --image-gib sets capacity (default 8). Linux images/fixtures are retained after unmount; APFS images are disposable.\nOriginal repositories are read-only inputs. Results default to target/ (not published)."
        );
        return Ok(());
    }
    ensure!(
        cfg!(any(target_os = "macos", target_os = "linux")),
        "image benchmarks require macOS or Linux"
    );
    let cwd = env::current_dir()?;
    let mut repositories = Vec::new();
    let mut concurrency = 100;
    let mut worktrees = None;
    let mut repeats = 3;
    let mut output = None;
    let mut candidate_path = cwd.join("target/release/git-twig");
    let mut filesystem = Filesystem::IsolatedImage;
    let mut image_size_gib: u64 = 8;
    let mut warmup = true;
    let mut require_native = true;
    let mut selected_backend = None;
    let mut run_notes = Vec::new();
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next().context("option requires a value")?;
        match arg.as_str() {
            "--repo" => {
                let (label, path) = value.split_once('=').context("repo must be label=path")?;
                ensure!(
                    !label.is_empty()
                        && label
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
                    "label must use letters, digits or hyphens"
                );
                ensure!(
                    !repositories.iter().any(|r: &Repository| r.label == label),
                    "duplicate repository label"
                );
                let path = fs::canonicalize(path)?;
                let commit = git_text(&path, &["rev-parse", "HEAD"])?;
                repositories.push(Repository {
                    label: label.into(),
                    path,
                    commit,
                    donor_commit: None,
                });
            }
            "--revision" => {
                let (label, commit) = value
                    .split_once('=')
                    .context("revision must be label=COMMIT")?;
                ensure!(
                    (commit.len() == 40 || commit.len() == 64)
                        && commit.bytes().all(|b| b.is_ascii_hexdigit()),
                    "revision must be a full commit OID"
                );
                let repository = repositories
                    .iter_mut()
                    .find(|r| r.label == label)
                    .context("declare --repo before --revision")?;
                repository.commit = git_text(
                    &repository.path,
                    &["rev-parse", "--verify", &format!("{commit}^{{commit}}")],
                )?;
            }
            "--donor-revision" => {
                let (label, commit) = value
                    .split_once('=')
                    .context("donor revision must be label=COMMIT")?;
                ensure!(
                    (commit.len() == 40 || commit.len() == 64)
                        && commit.bytes().all(|b| b.is_ascii_hexdigit()),
                    "donor revision must be a full commit OID"
                );
                let repository = repositories
                    .iter_mut()
                    .find(|r| r.label == label)
                    .context("declare --repo before --donor-revision")?;
                repository.donor_commit = Some(git_text(
                    &repository.path,
                    &["rev-parse", "--verify", &format!("{commit}^{{commit}}")],
                )?);
            }
            "--require-native" => require_native = value.parse()?,
            "--filesystem" => {
                filesystem = match value.as_str() {
                    "host" => Filesystem::Host,
                    "isolated-image" => Filesystem::IsolatedImage,
                    "case-sensitive-image" => Filesystem::CaseSensitiveImage,
                    "linux-xfs-image" => Filesystem::LinuxXfsImage,
                    "linux-btrfs-image" => Filesystem::LinuxBtrfsImage,
                    _ => anyhow::bail!(
                        "filesystem must be host, isolated-image, case-sensitive-image, linux-xfs-image or linux-btrfs-image"
                    ),
                }
            }
            "--warmup" => warmup = value.parse()?,
            "--note" => run_notes.push(value),
            "--backend" => {
                ensure!(
                    value == "git" || value == "twig",
                    "backend must be git or twig"
                );
                selected_backend = Some(value);
            }
            "--image-gib" => image_size_gib = value.parse()?,
            "--concurrency" => concurrency = value.parse()?,
            "--worktrees" => worktrees = Some(value.parse::<usize>()?),
            "--repeats" => repeats = value.parse()?,
            "--output" => output = Some(PathBuf::from(value)),
            "--candidate" => candidate_path = PathBuf::from(value),
            _ => anyhow::bail!("unknown option: {arg}"),
        }
    }
    ensure!(
        match filesystem {
            Filesystem::LinuxXfsImage | Filesystem::LinuxBtrfsImage => cfg!(target_os = "linux"),
            Filesystem::IsolatedImage | Filesystem::CaseSensitiveImage => cfg!(target_os = "macos"),
            Filesystem::Host => true,
        },
        "selected image format is not supported on this platform"
    );
    let worktrees = worktrees.unwrap_or(concurrency);
    ensure!(
        (1..=1000).contains(&worktrees) && concurrency <= worktrees,
        "worktrees must be 1..1000 and at least the concurrency"
    );
    ensure!(
        !repositories.is_empty()
            && (1..=128).contains(&concurrency)
            && repeats > 0
            && (4..=256).contains(&image_size_gib),
        "provide repositories, concurrency 1..128, positive repeats, image capacity 4..256 GiB"
    );
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let output =
        output.unwrap_or_else(|| cwd.join(format!("target/real-results/{timestamp}.json")));
    ensure!(!output.exists(), "refusing to overwrite an existing run");
    let root = cwd.join(format!(
        "target/real-images/{timestamp}-{}",
        std::process::id()
    ));
    fs::create_dir_all(&root)?;
    let root = fs::canonicalize(root)?;
    let candidate = fs::canonicalize(candidate_path)?;
    let digest = if cfg!(target_os = "linux") {
        capture(&cwd, "sha256sum", &[candidate.as_os_str().into()])?
    } else {
        capture(
            &cwd,
            "shasum",
            &["-a".into(), "256".into(), candidate.as_os_str().into()],
        )?
    };
    let cpu = if cfg!(target_os = "linux") {
        fs::read_to_string("/proc/cpuinfo")?
            .lines()
            .find_map(|line| {
                line.strip_prefix("model name")
                    .and_then(|s| s.split_once(':'))
                    .map(|(_, model)| model.trim().to_owned())
            })
            .unwrap_or_else(|| env::consts::ARCH.into())
    } else {
        capture(
            &cwd,
            "sysctl",
            &["-n".into(), "machdep.cpu.brand_string".into()],
        )?
    };
    let mut suite = Suite {
        schema_version: 4, filesystem, allocation_sampling: if filesystem == Filesystem::Host { None } else { Some(ALLOCATION_SAMPLING) }, warmup, require_native, selected_backend: selected_backend.clone(), image_size_gib: if filesystem == Filesystem::Host { None } else { Some(image_size_gib) }, suite: "real-image-cohort".into(), generated_at_unix_ms: timestamp,
        commit: git_text(&cwd, &["rev-parse", "HEAD"])?, dirty: !git_text(&cwd, &["status", "--porcelain"])?.is_empty(),
        candidate_sha256: digest.split_whitespace().next().context("missing candidate hash")?.into(),
        git_version: capture(&cwd, "git", &["--version".into()])?,
        platform: format!("{} {} {}", env::consts::OS, env::consts::ARCH, capture(&cwd, "uname", &["-r".into()])?),
        cpu, concurrency, worktrees_per_cohort: worktrees, repeats, repositories: repositories.clone(),
        notes: vec![
            "Each image-backed backend/repeat gets a fresh, isolated sparse filesystem image (capacity/format recorded in imageSizeGiB/filesystem). Seed uses git clone --shared --no-checkout plus explicit pinned-commit checkout. Original repositories and object stores are read-only inputs; seed history uses alternates outside image.".into(),
            if warmup { "One operation warmup per backend in its own seed/registry so it cannot become a measured donor. Removed before APFS snapshots; retained on Linux/host and excluded by the before snapshot. Global/system config and fixture hooks disabled. Backend order alternates by repeat.".into() } else { "Operation warmup disabled; seed checkout still warms input objects/files. Global/system Git config and fixture hooks disabled.".into() },
            "A barrier releases the bounded worker pool simultaneously; each worker starts its next queued create only after its previous CLI returns. worktreesPerCohort is the total count; concurrency is the maximum in flight. Create wall time includes thread startup/barrier and ends at the last CLI completion, excluding final coordinator checkpoint/join work. Per-operation latency measures only that CLI. harnessCpuMsDuringCreate records self CPU (coordinator and worker threads), excluding CLI child CPU. No verification/generation competes with creates. No operation retries inside a cohort. Operator-authorized complete paired repository attempts are retained separately; select the first stable complete attempt, never favorable timings or individual cohorts.".into(),
            "First git status is measured sequentially after all creates and the initial space snapshot. Full byte/symlink/executable verification follows each first status; its CPU is outside creation timing. A first-status sum is not a concurrent-status wall time.".into(),
            "Allocated space is total minus free blocks from statvfs on the isolated filesystem, sampled after sync until stable (or marked unstable). The recorded allocationSampling policy requires at least 20 samples 500 ms apart and an identical final 11-sample window, with a maximum of 240 samples (120 seconds) on either platform. This includes working files, indexes, retained staging and filesystem metadata, not logical size/du/st_blocks. Host sparse-image allocation is not the metric.".into(),
            "After initial verification/space measurement, one data-bearing byte in the largest regular target file matching the original donor (offset and file length recorded) is changed in place; if no such data exists, the probe is not applicable. Additional filesystem allocation is sampled; seed file must remain unchanged. APFS images are detached and deleted after saving each cohort; Linux images and fixtures are retained after unmount; no benchmark input paths or source file contents are serialized.".into(),
            "Local warm-cache results. A sparse loopback/disk image is a controlled filesystem, not a claim about native host-volume maximum throughput. Report failures alongside latency and allocation; do not compare unequal successful counts as identical capacity.".into(),
        ], cohorts: vec![], failures: vec![],
    };
    if matches!(
        filesystem,
        Filesystem::LinuxXfsImage | Filesystem::LinuxBtrfsImage
    ) {
        suite.notes.push("Linux images use the host kernel through loopback devices. XFS is formatted with reflink=1; Btrfs uses default features (no compression). Filesystem allocation includes metadata/logs and shared data counted once. Native Twig reflinks are mandatory. Each backend/repeat gets a separate filesystem, seed, warmup and registry. Images and fixtures are retained; mount/mkfs require root or noninteractive sudo inside a dedicated test environment. These are not bare-device performance numbers.".into());
        suite.notes.push(format!(
            "Rust: {}; UID: {}; image tool: {}",
            env::var("TWIG_BENCH_RUST_VERSION")
                .or_else(|_| capture(&cwd, "rustc", &["--version".into()]))
                .unwrap_or_else(|_| {
                    "compiler outside measurement container; candidate SHA-256 recorded".into()
                }),
            capture(&cwd, "id", &["-u".into()])?,
            capture(
                &cwd,
                if filesystem == Filesystem::LinuxXfsImage {
                    "mkfs.xfs"
                } else {
                    "mkfs.btrfs"
                },
                &["-V".into()]
            )
            .unwrap_or_else(|_| "see environment".into())
        ));
    }
    if filesystem == Filesystem::Host {
        suite.notes.push("Filesystem=host overrides image setup/cleanup notes: fresh cohort directories and warmup worktrees are retained, no allocation snapshots/deltas are reported, no sync is forced. Use isolated-image runs for space comparisons. Input revisions can be pinned to match earlier image runs.".into());
    }
    suite.notes.push("Schema 4 records source/donor and target commits/trees separately. Different-revision checkout bytes come from an independent Git oracle outside the measured filesystem and worktree registry. Later measured creates may legitimately reuse earlier completed targets. requireNative=false is only for explicitly named fallback baselines; native flags and clone counts remain recorded, never relabeled.".into());
    suite.notes.extend(run_notes);
    save(&output, &suite)?;
    for repository in &repositories {
        for repeat in 0..repeats {
            let order = if repeat % 2 == 0 {
                ["git", "twig"]
            } else {
                ["twig", "git"]
            };
            for backend in order {
                if selected_backend
                    .as_deref()
                    .is_some_and(|chosen| chosen != backend)
                {
                    continue;
                }
                eprintln!(
                    "{}: {backend}, repeat {}/{repeats}, {worktrees} creates with concurrency {concurrency}",
                    repository.label,
                    repeat + 1
                );
                let stem = root.join(format!("{}-{backend}-{repeat}", repository.label));
                let mut image = Image::new(&stem, filesystem, image_size_gib);
                let prepare = match filesystem {
                    Filesystem::IsolatedImage
                    | Filesystem::CaseSensitiveImage
                    | Filesystem::LinuxXfsImage
                    | Filesystem::LinuxBtrfsImage => image.attach(),
                    Filesystem::Host => fs::create_dir(&image.mount).map_err(Into::into),
                };
                let result = prepare.and_then(|()| {
                    cohort(
                        repository,
                        backend,
                        repeat,
                        concurrency,
                        &candidate,
                        &image.mount,
                        CohortOptions {
                            filesystem,
                            warmup,
                            require_native,
                            worktrees,
                        },
                    )
                });
                match result {
                    Ok(point) => {
                        eprintln!(
                            "  {:.2}s creates; {} ok / {} failed; allocation after status: {}",
                            point.create_wall_ms / 1000.0,
                            point.succeeded,
                            point.failed,
                            point
                                .growth_after_status_bytes
                                .map(|bytes| format!("{:.2} MiB", bytes as f64 / 1048576.0))
                                .unwrap_or_else(|| "unmeasured on shared host filesystem".into())
                        );
                        if point.failed > 0 {
                            suite.failures.push(format!(
                                "{} / {backend} / repeat {repeat}: {} failed or incorrect checkouts",
                                repository.label, point.failed
                            ));
                        }
                        suite.cohorts.push(point);
                    }
                    Err(error) => suite.failures.push(format!(
                        "{} / {backend} / repeat {repeat}: {error:#}",
                        repository.label
                    )),
                }
                persist_and_cleanup(
                    || save(&output, &suite),
                    || {
                        if filesystem != Filesystem::Host {
                            image.cleanup()
                        } else {
                            eprintln!("  host fixtures retained at {}", image.mount.display());
                            Ok(())
                        }
                    },
                )?;
                ensure!(
                    suite.failures.is_empty(),
                    "setup/measurement failure saved to {}",
                    output.display()
                );
            }
        }
    }
    println!("{}", output.display());
    Ok(())
}

impl Image {
    fn new(stem: &Path, filesystem: Filesystem, size_gib: u64) -> Self {
        Self {
            path: stem.with_extension(
                if matches!(
                    filesystem,
                    Filesystem::LinuxXfsImage | Filesystem::LinuxBtrfsImage
                ) {
                    "img"
                } else {
                    "sparseimage"
                },
            ),
            mount: stem.with_extension("mount"),
            attached: false,
            filesystem,
            size_gib,
        }
    }
    fn attach(&mut self) -> Result<()> {
        if matches!(
            self.filesystem,
            Filesystem::LinuxXfsImage | Filesystem::LinuxBtrfsImage
        ) {
            return self.attach_linux();
        }
        checked(
            Command::new("hdiutil")
                .args(["create", "-quiet", "-size"])
                .arg(format!("{}g", self.size_gib))
                .args(["-type", "SPARSE", "-fs"])
                .arg(if self.filesystem == Filesystem::CaseSensitiveImage {
                    "Case-sensitive APFS"
                } else {
                    "APFS"
                })
                .args(["-volname", "TwigBench", "-nospotlight"])
                .arg(&self.path)
                .output()?,
        )?;
        fs::create_dir(&self.mount)?;
        checked(
            Command::new("hdiutil")
                .args([
                    "attach",
                    "-quiet",
                    "-nobrowse",
                    "-owners",
                    "on",
                    "-mountpoint",
                ])
                .arg(&self.mount)
                .arg(&self.path)
                .output()?,
        )?;
        self.attached = true;
        fs::write(self.mount.join(".metadata_never_index"), [])?;
        Ok(())
    }
    fn attach_linux(&mut self) -> Result<()> {
        // Format only a newly created, benchmark-owned regular file, never a device.
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.path)?
            .set_len(self.size_gib * 1024 * 1024 * 1024)?;
        let mut format = Command::new(if self.filesystem == Filesystem::LinuxXfsImage {
            "mkfs.xfs"
        } else {
            "mkfs.btrfs"
        });
        format.arg("-q");
        if self.filesystem == Filesystem::LinuxXfsImage {
            format.args(["-m", "reflink=1"]);
        }
        checked(format.arg(&self.path).output()?)?;
        fs::create_dir(&self.mount)?;
        checked(
            privileged("mount")
                .args(["-o", "loop"])
                .arg(&self.path)
                .arg(&self.mount)
                .output()?,
        )?;
        self.attached = true;
        let owner = format!(
            "{}:{}",
            capture(&self.mount, "id", &["-u".into()])?,
            capture(&self.mount, "id", &["-g".into()])?
        );
        checked(privileged("chown").arg(owner).arg(&self.mount).output()?)?;
        Ok(())
    }
    fn cleanup(&mut self) -> Result<()> {
        if matches!(
            self.filesystem,
            Filesystem::LinuxXfsImage | Filesystem::LinuxBtrfsImage
        ) {
            if self.attached {
                checked(privileged("umount").arg(&self.mount).output()?)?;
                self.attached = false;
            }
            eprintln!("  Linux image retained at {}", self.path.display());
            return Ok(());
        }
        if self.attached {
            checked(
                Command::new("hdiutil")
                    .arg("detach")
                    .arg(&self.mount)
                    .output()?,
            )
            .with_context(|| {
                format!(
                    "image still mounted at {}; retained {}",
                    self.mount.display(),
                    self.path.display()
                )
            })?;
            self.attached = false;
        }
        if self.path.exists() {
            fs::remove_file(&self.path)?;
        }
        eprintln!("  disposable image detached and removed");
        Ok(())
    }
}

fn seed_checkout(repository: &Repository, destination: &Path, commit: &str) -> Result<()> {
    let parent = destination.parent().context("seed has no parent")?;
    checked(
        command("git", parent)
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "clone",
                "--shared",
                "--no-checkout",
                "--",
            ])
            .arg(&repository.path)
            .arg(destination)
            .output()?,
    )?;
    checked(
        command("git", destination)
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "submodule.recurse=false",
                "checkout",
                "--detach",
                commit,
            ])
            .output()?,
    )?;
    Ok(())
}

fn cohort(
    repository: &Repository,
    backend: &str,
    repeat: usize,
    concurrency: usize,
    candidate: &Path,
    mount: &Path,
    options: CohortOptions,
) -> Result<Cohort> {
    let filesystem = options.filesystem;
    let seed = mount.join("seed");
    let source_commit = repository
        .donor_commit
        .as_deref()
        .unwrap_or(&repository.commit);
    seed_checkout(repository, &seed, source_commit)?;
    let tree = git_text(&seed, &["rev-parse", "HEAD^{tree}"])?;
    let source_expected = expected_files(&seed)?;
    let oracle = if source_commit == repository.commit {
        seed.clone()
    } else {
        let oracle = mount.with_extension("oracle");
        seed_checkout(repository, &oracle, &repository.commit)?;
        oracle
    };
    let target_tree = git_text(&oracle, &["rev-parse", "HEAD^{tree}"])?;
    let expected = expected_files(&oracle)?;
    let logical_bytes = expected.iter().map(Expected::logical_bytes).sum();
    let mut warmup_cleanup_recovered = false;
    if options.warmup {
        let warmup_seed = mount.join("warmup-seed");
        seed_checkout(repository, &warmup_seed, source_commit)?;
        let warmup = create(
            &warmup_seed,
            &mount.join("warmup"),
            candidate,
            backend,
            &repository.commit,
            0,
            Instant::now(),
        );
        ensure!(
            warmup.error.is_none()
                && (backend != "twig" || !options.require_native || warmup.native_cow),
            "warmup did not use expected backend: {:?}",
            warmup.error
        );
        if matches!(
            filesystem,
            Filesystem::IsolatedImage | Filesystem::CaseSensitiveImage
        ) {
            warmup_cleanup_recovered = remove_owned_warmup(&warmup_seed, mount)?;
            // Only benchmark-owned warmup staging paths on this disposable image are removed.
            for entry in fs::read_dir(mount)? {
                let entry = entry?;
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".git-twig-stage-")
                {
                    fs::remove_dir_all(entry.path())?;
                }
            }
        }
    }
    let before = filesystem.snapshot(mount)?;
    let barrier = Arc::new(Barrier::new(concurrency));
    let next = Arc::new(AtomicUsize::new(0));
    let (sender, receiver) = mpsc::channel();
    let checkpoint = mount.with_extension("creation.jsonl");
    let mut progress_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(checkpoint)?;
    let cpu_before = self_cpu_ms()?;
    let start = Instant::now();
    let handles = (0..concurrency)
        .map(|_| {
            let seed = seed.clone();
            let mount = mount.to_path_buf();
            let candidate = candidate.to_path_buf();
            let backend = backend.to_owned();
            let target_commit = repository.commit.clone();
            let barrier = Arc::clone(&barrier);
            let next = Arc::clone(&next);
            let sender = sender.clone();
            thread::spawn(move || {
                barrier.wait();
                loop {
                    let worker = next.fetch_add(1, Ordering::Relaxed);
                    if worker >= options.worktrees {
                        break;
                    }
                    let destination = mount.join(format!("w-{worker}"));
                    let outcome = create(
                        &seed,
                        &destination,
                        &candidate,
                        &backend,
                        &target_commit,
                        worker,
                        start,
                    );
                    if sender.send(outcome).is_err() {
                        break;
                    }
                }
            })
        })
        .collect::<Vec<_>>();
    eprintln!(
        "cohort workers launched: {concurrency}; queued creates: {}",
        options.worktrees
    );
    drop(sender);
    let mut operations = Vec::with_capacity(options.worktrees);
    let mut observer_error = None;
    for mut operation in receiver {
        if let Some(error) = &mut operation.error {
            *error = error
                .replace(&mount.display().to_string(), "<image>")
                .replace(&repository.path.display().to_string(), "<source>");
        }
        operations.push(operation);
        eprintln!(
            "{backend}: CLI completed {}/{}; content verification pending",
            operations.len(),
            options.worktrees
        );
        let progress = CreationProgress {
            schema_version: 3,
            phase: "creation; verification pending",
            repository: &repository.label,
            source_commit,
            target_commit: &repository.commit,
            backend,
            repeat,
            expected_operations: options.worktrees,
            elapsed_ms: ms(start),
            operations: &operations[operations.len() - 1..],
        };
        if observer_error.is_none() {
            observer_error = save_creation_progress(&mut progress_file, &progress).err();
        }
    }
    for handle in handles {
        if handle.join().is_err() && observer_error.is_none() {
            observer_error = Some(anyhow::anyhow!("CLI worker panicked"));
        }
    }
    // Never leave CLI workers running while the caller unmounts their filesystem.
    if let Some(error) = observer_error {
        return Err(error.context("creation observer failed after all workers joined"));
    }
    ensure!(
        operations.len() == options.worktrees,
        "a CLI worker did not return an outcome"
    );
    operations.sort_by_key(|operation| operation.worker);
    if concurrency == 1 {
        ensure!(
            operations
                .windows(2)
                .all(|pair| pair[0].create_finished_at_ms <= pair[1].create_started_at_ms),
            "sequential creates overlapped"
        );
    }
    let observer_wall_ms = ms(start);
    let harness_cpu_ms_during_create = self_cpu_ms()? - cpu_before;
    // End at the last CLI completion, not after coordinator checkpoint/join work.
    let create_wall_ms = creation_makespan(&operations);
    let after_create = filesystem.snapshot(mount)?;
    let status_start = Instant::now();
    let mut verified_files = 0;
    for operation in &mut operations {
        if operation.error.is_some() {
            continue;
        }
        let destination = mount.join(format!("w-{}", operation.worker));
        let verification = (|| -> Result<()> {
            let start = Instant::now();
            let status = checked(
                command("git", &destination)
                    .args(["status", "--porcelain", "--untracked-files=no"])
                    .output()?,
            )?;
            operation.first_status_ms = Some(ms(start));
            ensure!(status.stdout.is_empty(), "checkout is dirty");
            for entry in &expected {
                verify_file(&destination, entry)?;
                verified_files += 1;
            }
            ensure!(
                checked(
                    command("git", &destination)
                        .args(["ls-files", "--others", "-z"])
                        .output()?
                )?
                .stdout
                .is_empty(),
                "unexpected untracked or ignored checkout contents"
            );
            ensure!(
                backend != "twig"
                    || !options.require_native
                    || (operation.native_cow
                        && operation.verified_clones.is_some_and(|count| count > 0)),
                "Twig delegated to Git rather than using CoW"
            );
            operation.verified = true;
            Ok(())
        })();
        if let Err(error) = verification {
            operation.error = Some(format!("{error:#}"));
        }
    }
    let status_and_verification_wall_ms = ms(status_start);
    let after_status = filesystem.snapshot(mount)?;
    // Prefer bytes actually present in the original donor, not a target-only
    // attributes file: this must exercise isolation of data that could be shared.
    let source_files: std::collections::BTreeMap<_, _> = source_expected
        .iter()
        .map(|entry| (&entry.path, entry))
        .collect();
    let probe = private_write_probe(expected.iter().filter(|entry| {
        source_files.get(&entry.path).is_some_and(|source| {
            source
                .regular_bytes()
                .zip(entry.regular_bytes())
                .is_some_and(|(source, target)| source == target)
        })
    }));
    if let Some(PrivateWriteProbe {
        entry: first,
        data,
        offset,
    }) = probe
    {
        for operation in &mut operations {
            if !operation.verified {
                continue;
            }
            let destination = mount
                .join(format!("w-{}", operation.worker))
                .join(&first.path);
            let write = (|| -> Result<()> {
                let mut file = OpenOptions::new().write(true).open(destination)?;
                file.seek(SeekFrom::Start(offset as u64))?;
                file.write_all(&[data[offset] ^ 0xff])?;
                let status = command("git", &mount.join(format!("w-{}", operation.worker)))
                    .args([
                        "-c",
                        "core.fsmonitor=false",
                        "diff-files",
                        "--quiet",
                        "--no-ext-diff",
                    ])
                    .output()?;
                ensure!(
                    status.status.code() == Some(1),
                    "Git did not detect the private write"
                );
                Ok(())
            })();
            if let Err(error) = write {
                operation.error = Some(format!("private write: {error:#}"));
                operation.verified = false;
            }
        }
    }
    let after_private_writes = if probe.is_some() {
        filesystem.snapshot(mount)?
    } else {
        None
    };
    let source_unchanged = source_expected
        .iter()
        .all(|entry| verify_file(&seed, entry).is_ok());
    ensure!(source_unchanged, "seed changed after private writes");
    for operation in &mut operations {
        if let Some(error) = &mut operation.error {
            *error = error
                .replace(&mount.display().to_string(), "<image>")
                .replace(&repository.path.display().to_string(), "<source>");
        }
    }
    let succeeded = operations
        .iter()
        .filter(|op| op.verified && op.error.is_none())
        .count();
    Ok(Cohort {
        repository: repository.label.clone(),
        source_commit: source_commit.into(),
        source_tree: tree,
        target_commit: repository.commit.clone(),
        target_tree,
        backend: backend.into(),
        repeat,
        tracked_files: expected.len(),
        logical_bytes_per_worktree: logical_bytes,
        create_wall_ms,
        harness_cpu_ms_during_create,
        observer_wall_ms,
        creates_per_second: succeeded as f64 * 1000.0 / create_wall_ms,
        status_and_verification_wall_ms,
        succeeded,
        failed: options.worktrees - succeeded,
        verified_files,
        source_unchanged,
        private_write_tested: probe.is_some(),
        warmup_cleanup_recovered,
        private_write_file_bytes: probe.map(|probe| probe.data.len()),
        private_write_offset: probe.map(|probe| probe.offset),
        growth_after_create_bytes: allocation_delta(&after_create, &before),
        growth_after_status_bytes: allocation_delta(&after_status, &before),
        private_write_growth_bytes: allocation_delta(&after_private_writes, &after_status),
        before,
        after_create,
        after_status,
        after_private_writes,
        operations,
    })
}

#[derive(Clone, Copy)]
struct PrivateWriteProbe<'a> {
    entry: &'a Expected,
    data: &'a [u8],
    offset: usize,
}
fn private_write_probe<'a>(
    expected: impl IntoIterator<Item = &'a Expected>,
) -> Option<PrivateWriteProbe<'a>> {
    expected
        .into_iter()
        .filter_map(|entry| {
            // A nonzero byte is data-bearing even when the file has sparse zero holes.
            let data = entry.regular_bytes()?;
            data.iter()
                .position(|byte| *byte != 0)
                .map(|offset| PrivateWriteProbe {
                    entry,
                    data,
                    offset,
                })
        })
        .max_by_key(|probe| probe.data.len())
}
fn creation_makespan(operations: &[Operation]) -> f64 {
    operations
        .iter()
        .map(|op| op.create_finished_at_ms)
        .fold(0.0, f64::max)
}
fn save_creation_progress(file: &mut fs::File, progress: &CreationProgress<'_>) -> Result<()> {
    serde_json::to_writer(&mut *file, progress)?;
    file.write_all(b"\n")?;
    Ok(())
}

fn persist_and_cleanup(
    save: impl FnOnce() -> Result<()>,
    cleanup: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let saved = save();
    let cleaned = cleanup();
    match (saved, cleaned) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(save), Err(cleanup)) => {
            anyhow::bail!("save failed: {save:#}; cleanup also failed: {cleanup:#}")
        }
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
}

fn allocation_delta(after: &Option<Space>, before: &Option<Space>) -> Option<i128> {
    after
        .as_ref()
        .zip(before.as_ref())
        .map(|(after, before)| after.allocated_bytes as i128 - before.allocated_bytes as i128)
}

#[derive(Debug, PartialEq, Eq)]
struct CheckoutCounts {
    verified_clones: usize,
    git_written_paths: usize,
}

fn checkout_counts(stderr: &[u8]) -> Option<CheckoutCounts> {
    String::from_utf8_lossy(stderr).lines().find_map(|line| {
        let line = line.strip_prefix("git-twig: checkout: ")?;
        let (clones, rest) = line.split_once(" verified file clones, ")?;
        let written = rest.strip_suffix(" Git-written paths")?;
        Some(CheckoutCounts {
            verified_clones: clones.parse().ok()?,
            git_written_paths: written.parse().ok()?,
        })
    })
}

fn create(
    seed: &Path,
    destination: &Path,
    candidate: &Path,
    backend: &str,
    revision: &str,
    worker: usize,
    cohort_start: Instant,
) -> Operation {
    let mut command = if backend == "git" {
        command("git", seed)
    } else {
        command(candidate, seed)
    };
    if backend == "git" {
        command.arg("worktree");
    }
    command
        .args([if backend == "git" { "add" } else { "new" }, "--detach"])
        .arg(destination)
        .arg(revision);
    let start = Instant::now();
    let create_started_at_ms = start.duration_since(cohort_start).as_secs_f64() * 1000.0;
    let output = command.output();
    let elapsed = ms(start);
    let create_finished_at_ms = ms(cohort_start);
    match output {
        Ok(output) => Operation {
            worker,
            create_ms: elapsed,
            create_started_at_ms,
            create_finished_at_ms,
            first_status_ms: None,
            exit_code: output.status.code(),
            native_cow: String::from_utf8_lossy(&output.stderr)
                .contains("CoW: verified tracked files"),
            verified_clones: checkout_counts(&output.stderr).map(|counts| counts.verified_clones),
            git_written_paths: checkout_counts(&output.stderr)
                .map(|counts| counts.git_written_paths),
            verified: false,
            error: if output.status.success() {
                None
            } else {
                Some(String::from_utf8_lossy(&output.stderr).into_owned())
            },
        },
        Err(error) => Operation {
            worker,
            create_ms: elapsed,
            create_started_at_ms,
            create_finished_at_ms,
            first_status_ms: None,
            exit_code: None,
            native_cow: false,
            verified_clones: None,
            git_written_paths: None,
            verified: false,
            error: Some(error.to_string()),
        },
    }
}

/// Platform primitives for measurement. Windows has no executable bit, and Git
/// for Windows writes paths as UTF-8 and may write symlinks as plain files.
mod platform {
    use anyhow::{Result, ensure};
    use std::{ffi::OsString, fs::Metadata, path::Path};

    #[cfg(unix)]
    pub fn os_string(bytes: Vec<u8>) -> Result<OsString> {
        use std::os::unix::ffi::OsStringExt;
        Ok(OsString::from_vec(bytes))
    }
    #[cfg(windows)]
    pub fn os_string(bytes: Vec<u8>) -> Result<OsString> {
        Ok(String::from_utf8(bytes)?.into())
    }

    #[cfg(unix)]
    pub fn bytes(path: &Path) -> Result<Vec<u8>> {
        use std::os::unix::ffi::OsStrExt;
        Ok(path.as_os_str().as_bytes().to_vec())
    }
    #[cfg(windows)]
    pub fn bytes(path: &Path) -> Result<Vec<u8>> {
        Ok(path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("path is not Unicode: {path:?}"))?
            .replace('\\', "/")
            .into_bytes())
    }

    /// The executable bit, where the platform has one.
    pub fn executable(metadata: &Metadata) -> Option<bool> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            Some(metadata.permissions().mode() & 0o100 != 0)
        }
        #[cfg(windows)]
        {
            let _ = metadata;
            None
        }
    }

    /// A tracked symlink's target as checked out: a link, or a plain file
    /// holding the target where Git does not create symlinks.
    pub fn symlink_target(path: &Path, metadata: &Metadata) -> Result<Vec<u8>> {
        if metadata.file_type().is_symlink() {
            return bytes(&std::fs::read_link(path)?);
        }
        ensure!(cfg!(windows) && metadata.is_file(), "symlink changed type");
        Ok(std::fs::read(path)?)
    }

    #[cfg(unix)]
    pub fn self_cpu_ms() -> Result<f64> {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        // SAFETY: usage points to writable rusage storage; RUSAGE_SELF excludes CLI children.
        ensure!(
            unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } == 0,
            "getrusage failed: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: successful getrusage initialized usage.
        let usage = unsafe { usage.assume_init() };
        Ok(
            (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as f64 * 1000.0
                + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as f64 / 1000.0,
        )
    }
    #[cfg(windows)]
    pub fn self_cpu_ms() -> Result<f64> {
        use windows_sys::Win32::{
            Foundation::FILETIME,
            System::Threading::{GetCurrentProcess, GetProcessTimes},
        };
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
        // SAFETY: the pseudo-handle is always valid; all outputs are writable.
        let result = unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            )
        };
        ensure!(
            result != 0,
            "GetProcessTimes failed: {}",
            std::io::Error::last_os_error()
        );
        let ticks = |time: FILETIME| {
            (u64::from(time.dwHighDateTime) << 32 | u64::from(time.dwLowDateTime)) as f64
        };
        // FILETIME counts 100 ns intervals.
        Ok((ticks(kernel) + ticks(user)) / 10_000.0)
    }

    /// Allocated and total bytes of the filesystem holding `mount`.
    // libc block counters are u32 on Darwin and u64 on Linux.
    #[cfg(unix)]
    #[allow(clippy::unnecessary_cast)]
    pub fn space(mount: &Path) -> Result<(u64, u64)> {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(mount.as_os_str().as_bytes())?;
        let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: path is a live C string; stat points to writable storage of the required size.
        ensure!(
            unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } == 0,
            "statvfs failed: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: successful statvfs initialized the complete result.
        let stat = unsafe { stat.assume_init() };
        let block_size = stat.f_frsize;
        Ok((
            (stat.f_blocks as u64 - stat.f_bfree as u64) * block_size,
            stat.f_blocks as u64 * block_size,
        ))
    }
    #[cfg(windows)]
    pub fn space(mount: &Path) -> Result<(u64, u64)> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let path: Vec<u16> = mount.as_os_str().encode_wide().chain([0]).collect();
        let (mut available, mut total, mut free) = (0, 0, 0);
        // SAFETY: path is NUL-terminated UTF-16; all outputs are writable.
        let result =
            unsafe { GetDiskFreeSpaceExW(path.as_ptr(), &mut available, &mut total, &mut free) };
        ensure!(
            result != 0,
            "GetDiskFreeSpaceExW failed: {}",
            std::io::Error::last_os_error()
        );
        Ok((total - free, total))
    }

    /// Flush the measured filesystem before sampling its allocation.
    pub fn sync(mount: &Path) -> Result<()> {
        #[cfg(unix)]
        {
            let mut sync = std::process::Command::new("sync");
            #[cfg(target_os = "linux")]
            sync.arg("-f").arg(mount); // syncfs: never flush unrelated host filesystems.
            #[cfg(not(target_os = "linux"))]
            let _ = mount;
            ensure!(sync.status()?.success(), "sync failed");
        }
        // Flushing a whole volume needs administrator rights on Windows. ReFS
        // reports allocation as it happens; the settle loop still waits.
        #[cfg(windows)]
        let _ = mount;
        Ok(())
    }
}

fn expected_files(seed: &Path) -> Result<Vec<Expected>> {
    let listing = checked(
        command("git", seed)
            .args(["ls-files", "--stage", "-z"])
            .output()?,
    )?
    .stdout;
    listing.split(|byte| *byte == 0).filter(|record| !record.is_empty()).map(|record| {
        let tab = record.iter().position(|byte| *byte == b'\t').context("missing index path")?;
        let mut header = std::str::from_utf8(&record[..tab])?.split(' ');
        let mode = header.next().context("missing index mode")?;
        let object = header.next().context("missing index object")?;
        ensure!((object.len() == 40 || object.len() == 64) && object.bytes().all(|b| b.is_ascii_hexdigit()), "invalid index object");
        ensure!(header.next() == Some("0") && header.next().is_none(), "unmerged or malformed index");
        let path = PathBuf::from(platform::os_string(record[tab + 1..].to_vec())?);
        ensure!(!path.as_os_str().is_empty() && path.components().all(|part| matches!(part, std::path::Component::Normal(name) if !name.eq_ignore_ascii_case(".git"))), "unsafe benchmark path");
        let absolute = seed.join(&path);
        let metadata = fs::symlink_metadata(&absolute)?;
        let contents = match mode {
            "100644" | "100755" => {
                ensure!(metadata.is_file(), "expected regular file");
                Contents::File { data: fs::read(&absolute)?, executable: mode == "100755" }
            }
            "120000" => Contents::Symlink(platform::symlink_target(&absolute, &metadata)?),
            "160000" => {
                ensure!(metadata.is_dir() && fs::read_dir(&absolute)?.next().is_none(), "expected uninitialized gitlink directory");
                Contents::GitlinkDirectory
            }
            _ => anyhow::bail!("unsupported benchmark index mode"),
        };
        Ok(Expected { path, contents })
    }).collect()
}
fn verify_file(root: &Path, entry: &Expected) -> Result<()> {
    let absolute = root.join(&entry.path);
    let metadata = fs::symlink_metadata(&absolute)?;
    match &entry.contents {
        Contents::File { data, executable } => {
            ensure!(metadata.is_file(), "regular file changed type");
            ensure!(fs::read(absolute)? == *data, "file bytes differ");
            ensure!(
                platform::executable(&metadata).is_none_or(|actual| actual == *executable),
                "executable bit differs"
            );
        }
        Contents::Symlink(target) => {
            ensure!(
                platform::symlink_target(&absolute, &metadata)? == *target,
                "symlink differs"
            );
        }
        Contents::GitlinkDirectory => {
            ensure!(
                metadata.is_dir() && fs::read_dir(absolute)?.next().is_none(),
                "gitlink directory is missing, changed type or populated"
            );
        }
    }
    Ok(())
}
fn self_cpu_ms() -> Result<f64> {
    platform::self_cpu_ms()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AllocationSampling {
    interval_ms: u64,
    minimum_samples: usize,
    stable_samples: usize,
    maximum_samples: usize,
}
const ALLOCATION_SAMPLING: AllocationSampling = AllocationSampling {
    interval_ms: 500,
    minimum_samples: 20,
    stable_samples: 11,
    maximum_samples: 240,
};

fn allocation_has_settled(observations: &[u64]) -> bool {
    observations.len() >= ALLOCATION_SAMPLING.minimum_samples
        && observations[observations.len() - ALLOCATION_SAMPLING.stable_samples..]
            .iter()
            .all(|value| Some(value) == observations.last())
}

fn settled_space(mount: &Path) -> Result<Space> {
    platform::sync(mount)?;
    let mut observations = vec![];
    let mut snapshot = space(mount)?;
    // Allocation accounting may continue after sync; require a long quiet window.
    for _ in 0..ALLOCATION_SAMPLING.maximum_samples {
        thread::sleep(Duration::from_millis(ALLOCATION_SAMPLING.interval_ms));
        snapshot = space(mount)?;
        observations.push(snapshot.allocated_bytes);
        if allocation_has_settled(&observations) {
            snapshot.stable = true;
            break;
        }
    }
    snapshot.observed_allocated_bytes = observations;
    Ok(snapshot)
}
fn space(mount: &Path) -> Result<Space> {
    let (allocated_bytes, total_bytes) = platform::space(mount)?;
    Ok(Space {
        allocated_bytes,
        total_bytes,
        stable: false,
        observed_allocated_bytes: vec![],
    })
}
/// Called only for this cohort's newly-created warmup on a disposable image.
/// Recover Git's observed partial ENOTEMPTY deletion before the allocation baseline;
/// never retry a measured creation or touch an input repository's registration.
fn remove_owned_warmup(seed: &Path, mount: &Path) -> Result<bool> {
    let warmup = mount.join("warmup");
    let output = command("git", seed)
        .args(["worktree", "remove", "--force"])
        .arg(&warmup)
        .output()?;
    if output.status.success() {
        return Ok(false);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure!(
        stderr.contains("Directory not empty"),
        "warmup removal failed: {stderr}"
    );
    ensure!(
        fs::symlink_metadata(&warmup)?.is_dir(),
        "owned warmup changed type"
    );
    let remaining = fs::read_dir(&warmup)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    eprintln!(
        "warmup cleanup recovery before measurement: {stderr}; remaining top-level entries: {remaining:?}"
    );
    for attempt in 0..3 {
        match fs::remove_dir_all(&warmup) {
            Ok(()) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) if attempt == 2 => return Err(error.into()),
            Err(_) => thread::sleep(Duration::from_millis(100)),
        }
    }
    // This seed and all its linked registrations belong exclusively to the image.
    checked(
        command("git", seed)
            .args(["worktree", "prune", "--expire", "now"])
            .output()?,
    )?;
    let listing = checked(
        command("git", seed)
            .args(["worktree", "list", "--porcelain", "-z"])
            .output()?,
    )?;
    ensure!(
        listing
            .stdout
            .split(|byte| *byte == 0)
            .filter(|field| field.starts_with(b"worktree "))
            .count()
            == 1,
        "warmup registration remains after cleanup"
    );
    ensure!(
        fs::symlink_metadata(&warmup).is_err(),
        "warmup remains before baseline"
    );
    Ok(true)
}

fn privileged(program: &str) -> Command {
    // Root is useful in a disposable container; never request interactive credentials.
    if env::var_os("TWIG_BENCH_ROOT").is_some() {
        Command::new(program)
    } else {
        let mut command = Command::new("sudo");
        command.args(["-n", program]);
        command
    }
}

fn checked(output: Output) -> Result<Output> {
    ensure!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output)
}
fn git_text(cwd: &Path, args: &[&str]) -> Result<String> {
    capture(
        cwd,
        "git",
        &args.iter().map(OsString::from).collect::<Vec<_>>(),
    )
}
fn capture(cwd: &Path, program: &str, args: &[OsString]) -> Result<String> {
    Ok(
        String::from_utf8(checked(command(program, cwd).args(args).output()?)?.stdout)?
            .trim()
            .to_owned(),
    )
}
fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}
fn save(path: &Path, suite: &Suite) -> Result<()> {
    fs::create_dir_all(path.parent().context("output needs a parent")?)?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(suite)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn allocation_requires_a_long_observation_and_quiet_window() {
        assert!(!super::allocation_has_settled(&[4096; 19]));
        assert!(super::allocation_has_settled(&[4096; 20]));
        let mut changing = vec![4096; 20];
        changing.push(8192);
        assert!(!super::allocation_has_settled(&changing));
        changing.extend([8192; 9]);
        assert!(!super::allocation_has_settled(&changing));
        changing.push(8192);
        assert!(super::allocation_has_settled(&changing));
        assert!(!super::allocation_has_settled(
            &(0..240).collect::<Vec<_>>()
        ));
    }
    use super::*;
    #[test]
    fn checkout_counters_are_not_inferred_from_a_backend_banner() {
        assert_eq!(
            checkout_counts(b"git-twig: Linux reflink CoW: verified tracked files\n"),
            None
        );
        assert_eq!(
            checkout_counts(b"git-twig: checkout: 12 verified file clones, 3 Git-written paths\n"),
            Some(CheckoutCounts {
                verified_clones: 12,
                git_written_paths: 3
            })
        );
        assert_eq!(
            checkout_counts(b"git-twig: checkout: x verified file clones, 3 Git-written paths\n"),
            None
        );
    }

    #[test]
    fn cross_revision_oracle_and_warmup_cannot_become_measured_donors() -> Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(format!(
                "bench-contract-{}-{}",
                std::process::id(),
                SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
            ));
        fs::create_dir_all(&root)?;
        let root = fs::canonicalize(root)?;
        let source = root.join("source");
        fs::create_dir(&source)?;
        checked(
            command("git", &source)
                .args(["init", "-b", "main"])
                .output()?,
        )?;
        for (key, value) in [
            ("user.name", "Fixture"),
            ("user.email", "fixture@example.invalid"),
        ] {
            checked(
                command("git", &source)
                    .args(["config", key, value])
                    .output()?,
            )?;
        }
        fs::write(source.join("common"), b"before\n")?;
        fs::write(source.join("stable"), b"shared\n")?;
        checked(command("git", &source).args(["add", "."]).output()?)?;
        checked(
            command("git", &source)
                .args(["commit", "-m", "A"])
                .output()?,
        )?;
        let donor = git_text(&source, &["rev-parse", "HEAD"])?;
        fs::write(source.join("common"), b"after!\n")?;
        fs::write(source.join("new"), b"new target path\n")?;
        checked(command("git", &source).args(["add", "."]).output()?)?;
        checked(
            command("git", &source)
                .args(["commit", "-m", "B"])
                .output()?,
        )?;
        let target = git_text(&source, &["rev-parse", "HEAD"])?;
        let repository = Repository {
            label: "contract".into(),
            path: source,
            commit: target.clone(),
            donor_commit: Some(donor.clone()),
        };
        let mount = root.join("mount");
        fs::create_dir(&mount)?;
        let point = cohort(
            &repository,
            "git",
            0,
            1,
            Path::new("/unused"),
            &mount,
            CohortOptions {
                filesystem: Filesystem::Host,
                warmup: true,
                require_native: false,
                worktrees: 1,
            },
        )?;
        assert_eq!(point.source_commit, donor);
        assert_eq!(point.target_commit, target);
        assert_ne!(point.source_tree, point.target_tree);
        assert!(point.source_unchanged && point.private_write_tested);
        assert_eq!(point.succeeded, 1);
        assert_eq!(point.failed, 0);
        let listing = git_text(&mount.join("seed"), &["worktree", "list", "--porcelain"])?;
        assert_eq!(
            listing
                .lines()
                .filter(|line| line.starts_with("worktree "))
                .count(),
            2
        );
        assert!(!listing.contains("warmup"));
        assert!(!listing.contains("oracle"));
        assert_eq!(fs::read(mount.join("seed/common"))?, b"before\n");
        eprintln!("fixture retained: {}", root.display());
        Ok(())
    }
    #[test]
    fn cleanup_runs_even_if_saving_fails_and_both_errors_survive() {
        let cleaned = std::cell::Cell::new(false);
        let result = persist_and_cleanup(
            || anyhow::bail!("disk full"),
            || {
                cleaned.set(true);
                anyhow::bail!("busy mount")
            },
        );
        assert!(cleaned.get());
        let error = format!("{:#}", result.unwrap_err());
        assert!(error.contains("disk full") && error.contains("busy mount"));
    }
    #[test]
    fn gitlink_directories_must_be_empty_and_are_never_followed() -> Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(format!(
                "gitlink-check-{}-{}",
                std::process::id(),
                SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
            ));
        fs::create_dir_all(root.join("sub"))?;
        let directory = Expected {
            path: "sub".into(),
            contents: Contents::GitlinkDirectory,
        };
        assert_eq!(directory.logical_bytes(), 0);
        assert!(private_write_probe([&directory]).is_none());
        verify_file(&root, &directory)?;
        fs::write(root.join("sub/private"), "must not be copied\n")?;
        assert!(verify_file(&root, &directory).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("sub", root.join("link"))?;
            let link = Expected {
                path: "link".into(),
                contents: Contents::GitlinkDirectory,
            };
            assert!(verify_file(&root, &link).is_err());
        }
        Ok(())
    }

    #[test]
    fn private_writes_skip_empty_and_sparse_holes() {
        let entry = |data| Expected {
            path: "file".into(),
            contents: Contents::File {
                data,
                executable: false,
            },
        };
        let entries = [entry(vec![]), entry(vec![0, 0, 42])];
        let probe = private_write_probe(&entries).unwrap();
        assert_eq!(probe.offset, 2);
        assert_eq!(probe.data.len(), 3);
        assert!(private_write_probe(&[entry(vec![]), entry(vec![0; 32])]).is_none());
    }
    #[test]
    fn creation_makespan_uses_cli_endpoints_not_observer_delay() {
        let operation = |end| Operation {
            worker: 0,
            create_ms: 1.0,
            create_started_at_ms: end - 1.0,
            create_finished_at_ms: end,
            first_status_ms: None,
            exit_code: Some(0),
            native_cow: false,
            verified_clones: None,
            git_written_paths: None,
            verified: false,
            error: None,
        };
        let operations = [operation(7.0), operation(19.0), operation(13.0)];
        std::thread::sleep(Duration::from_millis(5)); // Simulate a delayed observer.
        assert_eq!(creation_makespan(&operations), 19.0);
    }
    #[test]
    fn host_mode_has_no_allocation_measurement() {
        assert!(
            Filesystem::Host
                .snapshot(Path::new("/not-a-benchmark-path"))
                .unwrap()
                .is_none()
        );
        assert_eq!(allocation_delta(&None, &None), None);
    }
    #[test]
    fn preserves_negative_allocator_deltas() {
        let snapshot = |bytes| {
            Some(Space {
                allocated_bytes: bytes,
                total_bytes: 100,
                stable: true,
                observed_allocated_bytes: vec![bytes],
            })
        };
        assert_eq!(allocation_delta(&snapshot(20), &snapshot(24)), Some(-4));
    }
}
