//! Jobs: a closed set of kinds, a fixed argv per kind, a state machine, log
//! capture with progress parsing, a timeout, and cancellation that kills the
//! child.
//!
//! A job never runs a shell. Every step is a program plus a vector of
//! arguments built here from values that were validated on the way in, and the
//! working directory is always inside the project's own tree or the cache.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Semaphore;

use crate::error::{ApiError, Result};

/// At most this many tools run at once; the rest queue.
pub const MAX_CONCURRENT_HEAVY: usize = 2;
/// Lines kept in memory for the log panel. The full log always goes to a file.
pub const LOG_RING: usize = 4000;
/// A job is killed when it runs longer than this.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(1800);

/// The closed enum of things a job can be. Nothing else is reachable from the
/// API, so no argument can name an arbitrary program.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Compile,
    Bake,
    Navmesh,
    Package,
    Deploy,
    Undeploy,
    Reload,
    ImportMap,
    ImportTf2,
    ExportMinimap,
    ExportLoadscreen,
}

impl JobKind {
    pub const ALL: [JobKind; 11] = [
        JobKind::Compile,
        JobKind::Bake,
        JobKind::Navmesh,
        JobKind::Package,
        JobKind::Deploy,
        JobKind::Undeploy,
        JobKind::Reload,
        JobKind::ImportMap,
        JobKind::ImportTf2,
        JobKind::ExportMinimap,
        JobKind::ExportLoadscreen,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Compile => "compile",
            Self::Bake => "bake",
            Self::Navmesh => "navmesh",
            Self::Package => "package",
            Self::Deploy => "deploy",
            Self::Undeploy => "undeploy",
            Self::Reload => "reload",
            Self::ImportMap => "import_map",
            Self::ImportTf2 => "import_tf2",
            Self::ExportMinimap => "export_minimap",
            Self::ExportLoadscreen => "export_loadscreen",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s).ok_or_else(|| {
            ApiError::bad(format!(
                "unknown job kind {s:?}; expected one of {}",
                Self::ALL.map(|k| k.as_str()).join(", ")
            ))
        })
    }

    /// Whether the kind counts against the heavy-job limit. `reload` is a
    /// single agent-link line, so it does not.
    pub fn is_heavy(self) -> bool {
        !matches!(self, Self::Reload)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

/// A navmesh bake walks one hull per invocation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hull {
    pub name: String,
    pub bounds: [f32; 6],
}

/// Everything a job needs, already validated. The API layer builds one of
/// these from the request body and never hands a raw string to the runner.
/// Every field is optional on the wire, because which ones a job needs depends
/// on its kind and a body that carries all of them is not an error.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct JobParams {
    /// Targets for a compile, comma separated, exactly as the CLI spells it.
    pub targets: String,
    /// The map stem a navmesh or import job works on.
    pub stem: String,
    pub hulls: Vec<Hull>,
    /// Deploy: the files to copy, relative to the project build directory.
    pub files: Vec<String>,
    /// Deploy: allow replacing a file the host did not write itself.
    pub replace_shipped: bool,
    /// Deploy/undeploy: which manifest this applies to.
    pub manifest: String,
    /// Import: the source `.bsp` for a TF2 import.
    pub source_bsp: String,
    /// Import: a map's client pak (`*_perm.rpak`; its `_temp` partner is
    /// found beside it). Empty means the install's pak for `stem`.
    pub pak: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobRequest {
    pub kind: JobKind,
    pub project: String,
    #[serde(default)]
    pub params: JobParams,
}

/// One step of a job: either an external tool or an in-process action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// A program with a fully built argument vector, run in `cwd`.
    Tool { program: PathBuf, args: Vec<OsString> },
    /// One `SCRIPT` line for the running dedicated server to execute.
    Live { code: String },
    /// Chunks an imported map's render lumps into the reference layer.
    Reference { stem: String },
    /// The deploy or undeploy, which the host performs itself.
    Deploy { files: Vec<String>, replace_shipped: bool },
    Undeploy { manifest: String },
}

/// A job's whole plan: the steps, the directory they run in, and the cap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub steps: Vec<Step>,
    pub cwd: PathBuf,
    pub timeout: Duration,
    /// Variables every tool step runs with: where the compiler finds the game
    /// and its asset tools.
    pub env: Vec<(String, PathBuf)>,
}

impl Plan {
    /// The exact argument vector of the first tool step, for the tests and for
    /// the log header. `None` for a job with no tool step.
    pub fn first_argv(&self) -> Option<(&Path, &[OsString])> {
        self.steps.iter().find_map(|s| match s {
            Step::Tool { program, args } => Some((program.as_path(), args.as_slice())),
            _ => None,
        })
    }
}

/// A log line with the progress value the tool reported, if any.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogLine {
    pub seq: u64,
    pub text: String,
    /// 0-100, taken from a `PROGRESS <n>` line; the last one seen wins.
    pub progress: Option<u8>,
}

/// The newest `LOG_RING` lines, plus the running total.
#[derive(Default)]
pub struct LogBuffer {
    lines: Mutex<VecDeque<LogLine>>,
    next: AtomicU64,
    progress: AtomicU64,
}

impl LogBuffer {
    pub fn push(&self, text: String) -> LogLine {
        let progress = parse_progress(&text);
        let line = LogLine { seq: self.next.fetch_add(1, Ordering::SeqCst), text, progress };
        if let Some(p) = line.progress {
            self.progress.store(u64::from(p), Ordering::Relaxed);
        }
        let mut g = self.lines.lock().expect("log mutex");
        if g.len() >= LOG_RING {
            g.pop_front();
        }
        g.push_back(line.clone());
        line
    }

    pub fn progress(&self) -> u8 {
        self.progress.load(Ordering::Relaxed).min(100) as u8
    }

    /// The tail, newest last, optionally only what is new since `since`.
    pub fn tail(&self, since: u64, limit: usize) -> Vec<LogLine> {
        let g = self.lines.lock().expect("log mutex");
        g.iter()
            .filter(|l| l.seq >= since)
            .rev()
            .take(limit.clamp(1, LOG_RING))
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }
}

/// The progress convention every tool in the pipeline follows:
/// `PROGRESS <0-100>`, optionally followed by what is happening
/// (`PROGRESS 45 baking lightmaps: 41% ...`).
pub fn parse_progress(line: &str) -> Option<u8> {
    let t = line.trim();
    let rest = t.strip_prefix("PROGRESS")?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let n: u32 = rest.split_whitespace().next()?.parse().ok()?;
    if n > 100 {
        return None;
    }
    Some(n as u8)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobView {
    pub id: String,
    pub kind: JobKind,
    pub state: JobState,
    pub progress: u8,
    pub log: Vec<LogLine>,
    pub artifacts: Vec<String>,
    pub error: Option<String>,
    pub since: u64,
}

/// The shared, observable state of one job.
pub struct Job {
    pub id: String,
    pub kind: JobKind,
    pub project: String,
    pub plan: Plan,
    pub log: Arc<LogBuffer>,
    pub artifacts: Arc<Mutex<Vec<String>>>,
    pub state: Arc<Mutex<JobState>>,
    pub error: Arc<Mutex<Option<String>>>,
    pub started: Instant,
    cancel: Arc<AtomicBool>,
}

impl Job {
    pub fn view(&self, since: u64, limit: usize) -> JobView {
        let state = *self.state.lock().expect("state mutex");
        let progress = if state == JobState::Succeeded { 100 } else { self.log.progress() };
        JobView {
            id: self.id.clone(),
            kind: self.kind,
            state,
            progress,
            log: self.log.tail(since, limit),
            artifacts: self.artifacts.lock().expect("artifacts mutex").clone(),
            error: self.error.lock().expect("error mutex").clone(),
            since,
        }
    }
}

/// The roots every job resolves paths against. A job never sees a raw path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Env {
    pub game_root: PathBuf,
    pub projects_root: PathBuf,
    pub cache_root: PathBuf,
}

impl Env {
    pub fn project_dir(&self, name: &str) -> Result<PathBuf> {
        crate::projects::root_dir(&self.projects_root, name)
    }

    pub fn project(&self, name: &str) -> Result<std::path::PathBuf> {
        Ok(self.project_dir(name)?.join("build"))
    }
}

/// The job registry. Cloning shares it, which is what the router needs.
#[derive(Clone)]
pub struct Manager {
    inner: Arc<Inner>,
}

struct Inner {
    jobs: Mutex<Vec<Arc<Job>>>,
    heavy: Arc<Semaphore>,
    env: Env,
}

impl Manager {
    pub fn new(env: Env) -> Self {
        Self {
            inner: Arc::new(Inner {
                jobs: Mutex::new(Vec::new()),
                heavy: Arc::new(Semaphore::new(MAX_CONCURRENT_HEAVY)),
                env,
            }),
        }
    }

    pub fn env(&self) -> &Env {
        &self.inner.env
    }

    /// The most recent job ids first, so the panel can show them in order.
    pub fn recent(&self) -> Vec<Arc<Job>> {
        let mut v = self.inner.jobs.lock().expect("jobs mutex").clone();
        v.sort_by(|a, b| b.started.cmp(&a.started));
        v
    }

    /// The public view of a job, for the API.
    pub fn view(&self, id: &str, since: u64, limit: usize) -> Result<JobView> {
        Ok(self.get(id)?.view(since, limit))
    }

    pub fn get(&self, id: &str) -> Result<Arc<Job>> {
        self.inner
            .jobs
            .lock()
            .expect("jobs mutex")
            .iter()
            .find(|j| j.id == id)
            .cloned()
            .ok_or_else(|| ApiError::not_found(format!("no job {id}")))
    }

    /// Registers a job and starts running it in the background.
    pub fn submit(&self, plan: Plan, kind: JobKind, project: String) -> Arc<Job> {
        let id = uuid::Uuid::new_v4().to_string();
        let job = Arc::new(Job {
            id: id.clone(),
            kind,
            project,
            plan,
            log: Arc::new(LogBuffer::default()),
            artifacts: Arc::new(Mutex::new(Vec::new())),
            state: Arc::new(Mutex::new(JobState::Queued)),
            error: Arc::new(Mutex::new(None)),
            started: Instant::now(),
            cancel: Arc::new(AtomicBool::new(false)),
        });
        self.inner.jobs.lock().expect("jobs mutex").insert(0, job.clone());
        let heavy = self.inner.heavy.clone();
        let env = self.inner.env.clone();
        let for_task = Arc::clone(&job);
        tokio::spawn(async move {
            run_job(for_task, heavy, env).await;
        });
        job
    }

    /// Asks a job to stop. A running child is killed, not asked politely.
    pub fn cancel(&self, id: &str) -> Result<()> {
        let job = self.get(id)?;
        job.cancel.store(true, Ordering::SeqCst);
        let mut state = job.state.lock().expect("state mutex");
        if *state == JobState::Queued || *state == JobState::Running {
            *state = JobState::Cancelled;
        }
        Ok(())
    }
}

async fn run_job(job: Arc<Job>, heavy: Arc<Semaphore>, env: Env) {
    let permit = if job.kind.is_heavy() { heavy.acquire_owned().await.ok() } else { None };
    {
        let mut st = job.state.lock().expect("state mutex");
        if *st == JobState::Cancelled {
            drop(permit);
            return;
        }
        *st = JobState::Running;
    }

    let log_path = env.cache_root.join("logs").join(format!("{}.log", job.id));
    if let Some(d) = log_path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let log_file = std::fs::OpenOptions::new().create(true).append(true).open(&log_path).ok();
    if log_file.is_some() {
        job.artifacts.lock().expect("artifacts mutex").push(paths_string(&log_path));
    }

    let outcome = execute(&job, &env, &log_file).await;
    drop(permit);
    let mut st = job.state.lock().expect("state mutex");
    if *st == JobState::Cancelled {
        return;
    }
    match outcome {
        Ok(()) => {
            *st = JobState::Succeeded;
            job.log.push("PROGRESS 100".into());
        }
        Err(JobError::Cancelled) => {
            *st = JobState::Cancelled;
        }
        Err(JobError::Failed(m)) => {
            job.log.push(format!("FAILED {m}"));
            *job.error.lock().expect("error mutex") = Some(m);
            *st = JobState::Failed;
        }
    }
}

fn paths_string(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

#[derive(Debug)]
enum JobError {
    Cancelled,
    Failed(String),
}

impl From<String> for JobError {
    fn from(s: String) -> Self {
        Self::Failed(s)
    }
}

async fn execute(
    job: &Arc<Job>,
    env: &Env,
    log_file: &Option<std::fs::File>,
) -> std::result::Result<(), JobError> {
    for step in &job.plan.steps {
        if job.cancel.load(Ordering::SeqCst) {
            return Err(JobError::Cancelled);
        }
        match step {
            Step::Tool { program, args } => run_tool(job, program, args, log_file).await?,
            Step::Live { code } => {
                let target = crate::live::Target::new(&env.game_root);
                let link = crate::live::agent::Link::new(target);
                let mut session = link.session().await.map_err(|e| JobError::Failed(e.to_string()))?;
                session
                    .script(code)
                    .await
                    .map_err(|e| JobError::Failed(e.to_string()))?;
                job.log.push(format!("sent {} bytes of script to the agent link", code.len()));
            }
            Step::Reference { stem } => {
                let (doc, files) = crate::reference::build(&env.cache_root, stem)
                    .map_err(|e| JobError::Failed(e.to_string()))?;
                job.log.push(format!(
                    "reference layer: {} chunks, {} triangles from {} of {} meshes",
                    doc.chunks.len(),
                    doc.triangles,
                    doc.decoded,
                    doc.meshes
                ));
                for skip in doc.skipped.iter().take(20) {
                    job.log.push(format!("skipped {skip}"));
                }
                for f in files {
                    job.artifacts.lock().expect("artifacts mutex").push(paths_string(&f));
                }
            }
            Step::Deploy { files, replace_shipped } => {
                let m = crate::deploy::deploy(env, &job.project, files, *replace_shipped, &job.log)
                    .map_err(|e| JobError::Failed(e.to_string()))?;
                job.log.push(format!("deployed {} files into {}", m.written.len(), m.manifest));
                job.artifacts.lock().expect("artifacts mutex").push(m.manifest);
            }
            Step::Undeploy { manifest } => {
                let n = crate::deploy::undeploy(env, manifest, &job.log)
                    .map_err(|e| JobError::Failed(e.to_string()))?;
                job.log.push(format!("undeploy restored {n} files"));
            }
        }
    }
    Ok(())
}

/// Runs one tool, streaming its output into the log and watching the clock and
/// the cancel flag. The child is killed outright on either.
async fn run_tool(
    job: &Arc<Job>,
    program: &Path,
    args: &[OsString],
    log_file: &Option<std::fs::File>,
) -> std::result::Result<(), JobError> {
    let quoted: Vec<String> = std::iter::once(program.display().to_string())
        .chain(args.iter().map(|a| a.to_string_lossy().into_owned()))
        .collect();
    job.log.push(format!("$ {}", quoted.join(" ")));

    let mut cmd = Command::new(program);
    cmd.args(args);
    cmd.current_dir(&job.plan.cwd);
    for (k, v) in &job.plan.env {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| JobError::Failed(format!("could not start {}: {e}", program.display())))?;
    let mut stdout = child.stdout.take().ok_or_else(|| JobError::Failed("no stdout".into()))?;
    let mut stderr = child.stderr.take().ok_or_else(|| JobError::Failed("no stderr".into()))?;
    job.log.push(format!("cwd {}", paths_string(&job.plan.cwd)));

    let deadline = Instant::now() + job.plan.timeout;
    let mut out_lines = BufReader::new(&mut stdout).lines();
    let mut err_lines = BufReader::new(&mut stderr).lines();
    let mut out_done = false;
    let mut err_done = false;
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            l = out_lines.next_line(), if !out_done => match l {
                Ok(Some(line)) => record(job, log_file, line),
                _ => out_done = true,
            },
            l = err_lines.next_line(), if !err_done => match l {
                Ok(Some(line)) => record(job, log_file, line),
                _ => err_done = true,
            },
            _ = tick.tick() => {
                if job.cancel.load(Ordering::SeqCst) {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    return Err(JobError::Cancelled);
                }
                if Instant::now() >= deadline {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    return Err(JobError::Failed(format!(
                        "{} exceeded {:?} and was killed",
                        program.display(),
                        job.plan.timeout
                    )));
                }
            }
        }
        if out_done && err_done {
            break;
        }
    }
    let status = child
        .wait()
        .await
        .map_err(|e| JobError::Failed(format!("wait failed: {e}")))?;
    if job.cancel.load(Ordering::SeqCst) {
        return Err(JobError::Cancelled);
    }
    if !status.success() {
        return Err(JobError::Failed(format!("{} exited with {status}", program.display())));
    }
    Ok(())
}

fn record(job: &Arc<Job>, log_file: &Option<std::fs::File>, line: String) {
    let l = job.log.push(line.clone());
    if let Some(f) = log_file {
        use std::io::Write;
        let mut f = f;
        let _ = writeln!(f, "[{}] {}", l.seq, line);
    }
}

/// Milliseconds since the epoch, used for the deploy manifest's timestamp.
pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// The argument vector an `import_map` job runs the compiler with: the verb, the
/// map stem, and the project it is imported into. Nothing else can reach the
/// vector, so a job of that kind cannot name a different program or a different
/// pair of arguments.
pub fn import_map_argv(stem: &str, project: &Path, pak: &Path) -> Result<Vec<OsString>> {
    crate::plan::map_stem(stem)?;
    crate::paths::project_name(
        project
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| crate::error::ApiError::bad("the project directory has no name"))?,
    )?;
    Ok(vec![
        OsString::from("import-map"),
        OsString::from(stem),
        OsString::from(project.to_string_lossy().into_owned()),
        OsString::from("--from"),
        OsString::from(pak.to_string_lossy().into_owned()),
        OsString::from("--force"),
    ])
}
