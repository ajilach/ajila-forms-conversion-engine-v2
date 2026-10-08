//! The host side: a pool of worker processes, a deadline per job, and the
//! bookkeeping that turns a dead worker into one broken rule.
//!
//! # Why a pool rather than a process per check
//!
//! Spawning is cheap but not free, and the two real shapes of rule work are
//! both batches: one script over the whole corpus (authoring a rule) and
//! every active script over one document (finishing a conversion). A pool
//! amortizes the spawn across a batch and, more importantly, bounds how many
//! of these processes can exist at once -- which is the thing that made the
//! previous single-threaded design defensible and must not be lost now that
//! the work runs in parallel.
//!
//! # What replaced the old "one blocking worker" rule
//!
//! Script execution used to run a whole batch on one `spawn_blocking`
//! thread, in order, because a hung script would otherwise strand a thread
//! per script. That reasoning was correct for in-process execution and is
//! exactly what isolation dissolves: a hung script now strands a *process*
//! that this module kills on a deadline, so running several at once costs
//! nothing that cannot be reclaimed.

use std::collections::HashMap;
use std::ffi::OsString;
use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use u2s_rules::{CheckedOutcome, ScriptBudget};

use crate::protocol::{CheckRequest, CheckResponse, WorkerRequest, WorkerResponse};

/// The binary this host runs. Located, not configured as a full path, so it
/// is subject to the same containment as every other program u2s spawns.
pub const WORKER_BIN: &str = "u2s-rules-worker";

/// How much longer than the script's own budget the host waits before
/// concluding the worker is not coming back. The gap covers process start,
/// pipe writes and the document parse -- everything that is not the script
/// itself -- so a script inside its budget is never killed for the host's
/// overheads.
const DEADLINE_GRACE: Duration = Duration::from_secs(5);

/// Default pool size. Small on purpose: these are processes evaluating
/// untrusted code, and the useful parallelism is bounded by cores long
/// before it is bounded by anything else.
const DEFAULT_WORKERS: usize = 4;

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("{WORKER_BIN} not found at {path}: {source}")]
    WorkerMissing {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// A worker process and the two pipes that talk to it.
struct Worker {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Worker {
    fn spawn(bin: &Path, args: &[OsString]) -> std::io::Result<Self> {
        let mut child = Command::new(bin)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            // Without this a killed worker becomes a zombie: tokio reaps on
            // drop only when it is allowed to.
            .kill_on_drop(true)
            .spawn()?;
        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }

    /// One request, one response. `Ok(None)` means the worker died without
    /// answering -- the caller turns that into a verdict about the script
    /// that was running, which is the whole reason one job is in flight at
    /// a time.
    async fn exchange(
        &mut self,
        request: &WorkerRequest,
    ) -> std::io::Result<Option<WorkerResponse>> {
        let mut line = serde_json::to_vec(request).map_err(std::io::Error::other)?;
        line.push(b'\n');
        // A broken pipe here is the worker having already died, which is
        // the same observation as an empty read below.
        if self.stdin.write_all(&line).await.is_err() || self.stdin.flush().await.is_err() {
            return Ok(None);
        }

        let mut answer = String::new();
        match self.stdout.read_line(&mut answer).await {
            // EOF: the process is gone.
            Ok(0) => Ok(None),
            Ok(_) => match serde_json::from_str::<WorkerResponse>(answer.trim_end()) {
                Ok(response) => Ok(Some(response)),
                // A line that does not parse is a half-written one, which
                // means the worker died mid-write. Same conclusion as EOF.
                Err(_) => Ok(None),
            },
            Err(_) => Ok(None),
        }
    }

    async fn kill(&mut self) {
        let _ = self.child.kill().await;
    }

    /// How the worker died, in the words the operating system used.
    ///
    /// Worth the extra `wait`: "exited without answering" alone cannot
    /// distinguish a memory abort from a CPU limit from a script that
    /// called `process.exit`, and those want different fixes. `kill` is
    /// called first, so this returns promptly whether or not the child had
    /// already gone.
    async fn cause_of_death(&mut self) -> String {
        match self.child.wait().await {
            Ok(status) => status.to_string(),
            Err(err) => format!("its exit status was unreadable: {err}"),
        }
    }
}

/// Runs rule scripts in worker processes.
///
/// Cloneable and shared: one runner per server, held by both the service
/// layer and the agent's tool dispatch, so the pool bounds every script the
/// process runs rather than one caller's share of them.
#[derive(Clone)]
pub struct RuleRunner {
    inner: Arc<Inner>,
}

struct Inner {
    worker_bin: PathBuf,
    /// Passed to every worker it starts.
    worker_args: Vec<OsString>,
    /// Idle workers, reused across batches. A worker that died is simply not
    /// returned here, so the pool refills itself on the next borrow.
    idle: Mutex<Vec<Worker>>,
    /// Bounds live workers. `Semaphore` rather than a fixed set of tasks
    /// because a batch may be smaller than the pool, and paying to start
    /// workers nobody needs is waste.
    permits: tokio::sync::Semaphore,
}

impl std::fmt::Debug for RuleRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuleRunner")
            .field("worker_bin", &self.inner.worker_bin)
            .field("workers", &self.inner.permits.available_permits())
            .finish_non_exhaustive()
    }
}

impl RuleRunner {
    /// Refuses to build if the worker binary is not there.
    ///
    /// A missing binary is a startup error rather than a fallback to
    /// in-process evaluation, and that is the point: a fallback would
    /// silently restore the abort risk this whole module exists to remove,
    /// and it would do so on exactly the machines where nobody checked.
    pub fn new(worker_bin: PathBuf, workers: usize) -> Result<Self, RunnerError> {
        Self::with_args(worker_bin, Vec::new(), workers)
    }

    /// [`Self::new`] over a program started with `args`: a host executable
    /// that serves as its own worker passes itself and
    /// [`crate::worker::WORKER_ARG`].
    pub fn with_args(
        worker_bin: PathBuf,
        args: Vec<OsString>,
        workers: usize,
    ) -> Result<Self, RunnerError> {
        let resolved = worker_bin
            .canonicalize()
            .map_err(|source| RunnerError::WorkerMissing {
                path: worker_bin.clone(),
                source,
            })?;
        Ok(Self {
            inner: Arc::new(Inner {
                worker_bin: resolved,
                worker_args: args,
                idle: Mutex::new(Vec::new()),
                permits: tokio::sync::Semaphore::new(workers.max(1)),
            }),
        })
    }

    /// `U2S_RULES_WORKERS`, else the smaller of the machine's parallelism
    /// and [`DEFAULT_WORKERS`].
    pub fn workers_from_env() -> usize {
        if let Some(configured) = std::env::var("U2S_RULES_WORKERS")
            .ok()
            .and_then(|raw| raw.parse::<usize>().ok())
            .filter(|n| *n > 0)
        {
            return configured;
        }
        std::thread::available_parallelism()
            .map(|n| n.get().min(DEFAULT_WORKERS))
            .unwrap_or(1)
    }

    /// Evaluates every job, in parallel across the pool, and returns one
    /// outcome per key.
    ///
    /// Total: every key comes back with a verdict. A worker that dies or
    /// overruns yields `Broken` for the job it was running and nothing else
    /// -- the sibling jobs in the batch are unaffected, which is the
    /// invariant `one_broken_script_does_not_abandon_the_rest_of_the_batch`
    /// has always asserted and that this design now upholds against an
    /// *abort*, not merely a throw.
    pub async fn run_batch<K>(&self, jobs: Vec<(K, CheckRequest)>) -> Vec<(K, CheckedOutcome)>
    where
        K: Copy + Eq + Hash + Send + 'static,
    {
        self.run_jobs(
            jobs.into_iter()
                .map(|(key, request)| (key, WorkerRequest::Check(request)))
                .collect(),
        )
        .await
        .into_iter()
        .map(|(key, response)| (key, check_outcome(response)))
        .collect()
    }

    /// Any mix of jobs, in parallel across the pool, one response per key,
    /// each of the kind its request asked for. Total in the same way
    /// [`Self::run_batch`] is.
    pub async fn run_jobs<K>(&self, jobs: Vec<(K, WorkerRequest)>) -> Vec<(K, WorkerResponse)>
    where
        K: Copy + Eq + Hash + Send + 'static,
    {
        if jobs.is_empty() {
            return Vec::new();
        }

        let mut running = Vec::with_capacity(jobs.len());
        for (key, request) in jobs {
            let runner = self.clone();
            running.push(tokio::spawn(
                async move { (key, runner.run_job(request).await) },
            ));
        }

        let mut results = Vec::with_capacity(running.len());
        for handle in running {
            match handle.await {
                Ok((key, outcome)) => results.push((key, outcome)),
                // A panic in the host's own dispatch, not in a script. It
                // cannot be attributed to a key, so it is logged rather
                // than silently dropping a result.
                Err(err) => tracing::error!(error = %err, "a rule check task panicked"),
            }
        }
        results
    }

    /// A script's top-level `requires` declaration, read in a worker --
    /// see `u2s_rules::read_requires`. `Err` names why it could not be
    /// read, including a worker that died.
    pub async fn read_requires(
        &self,
        script_js: &str,
        budget: &ScriptBudget,
    ) -> Result<Vec<String>, String> {
        match self
            .run_job(WorkerRequest::Requires {
                script_js: script_js.to_owned(),
                budget: budget.into(),
            })
            .await
        {
            WorkerResponse::Requires { result } => result,
            other => Err(format!(
                "the sandbox worker answered a different job: {other:?}"
            )),
        }
    }

    /// An `ingest_script` fact's `extract(ingest)`, run in a worker -- see
    /// `u2s_rules::run_extract`.
    pub async fn run_extract(
        &self,
        script_js: &str,
        ingest: &serde_json::Value,
        budget: &ScriptBudget,
    ) -> Result<serde_json::Value, String> {
        match self
            .run_job(WorkerRequest::Extract {
                script_js: script_js.to_owned(),
                ingest: ingest.clone(),
                budget: budget.into(),
            })
            .await
        {
            WorkerResponse::Extract { result } => result,
            other => Err(format!(
                "the sandbox worker answered a different job: {other:?}"
            )),
        }
    }

    /// One job, on a borrowed worker, under a deadline.
    async fn run_job(&self, request: WorkerRequest) -> WorkerResponse {
        let budget: ScriptBudget = request.budget();
        // `eval_multiplier` widens this for a job carrying a `fix_js`: up
        // to three script evaluations (check, fix, re-check) rather than
        // one, so an honest fixed job near its own budget is not killed as
        // if it were a runaway. See `CheckRequest::eval_multiplier`'s own
        // docs for why the worker's CPU ceiling must derive from the same
        // number.
        let deadline = budget.wall_clock * request.eval_multiplier() + DEADLINE_GRACE;

        let _permit = self
            .inner
            .permits
            .acquire()
            .await
            .expect("the semaphore is never closed");

        let mut worker = match self.take_worker().await {
            Ok(worker) => worker,
            Err(err) => {
                return WorkerResponse::failed(
                    &request,
                    format!("no sandbox worker could be started: {err}"),
                );
            }
        };

        match tokio::time::timeout(deadline, worker.exchange(&request)).await {
            Ok(Ok(Some(response))) => {
                // Only a worker that answered is worth keeping: one that
                // died has nothing to return to the pool.
                self.put_worker(worker).await;
                response
            }
            Ok(Ok(None)) => {
                // Died without answering. With one job in flight per worker
                // this is unambiguous: it was *this* script.
                worker.kill().await;
                WorkerResponse::failed(
                    &request,
                    format!(
                        "the sandbox worker died without answering ({}), which is what a script \
                         exceeding its memory or CPU budget looks like",
                        worker.cause_of_death().await
                    ),
                )
            }
            Ok(Err(err)) => {
                worker.kill().await;
                WorkerResponse::failed(
                    &request,
                    format!("the sandbox worker could not be reached: {err}"),
                )
            }
            Err(_) => {
                // The deadline the in-process design could never enforce.
                worker.kill().await;
                WorkerResponse::failed(
                    &request,
                    format!(
                        "the script did not finish within {:?} and was stopped",
                        budget.wall_clock
                    ),
                )
            }
        }
    }

    async fn take_worker(&self) -> std::io::Result<Worker> {
        if let Some(worker) = self.inner.idle.lock().await.pop() {
            return Ok(worker);
        }
        Worker::spawn(&self.inner.worker_bin, &self.inner.worker_args)
    }

    async fn put_worker(&self, worker: Worker) {
        self.inner.idle.lock().await.push(worker);
    }

    /// Convenience for the common single-script case.
    pub async fn run_one_script(
        &self,
        script_js: &str,
        output: &serde_json::Value,
        schema: &serde_json::Value,
        budget: &ScriptBudget,
    ) -> CheckedOutcome {
        check_outcome(
            self.run_job(WorkerRequest::Check(CheckRequest::new(
                script_js.to_owned(),
                None,
                output.clone(),
                schema.clone(),
                serde_json::Map::new(),
                budget,
            )))
            .await,
        )
    }
}

/// A check's outcome from whatever the worker answered. Any other kind of
/// answer to a check is a protocol fault, reported as a broken rule.
fn check_outcome(response: WorkerResponse) -> CheckedOutcome {
    match response {
        WorkerResponse::Check(response) => response.into(),
        other => CheckResponse::broken(format!(
            "the sandbox worker answered a check with a different job: {other:?}"
        ))
        .into(),
    }
}

/// Groups outcomes back into the caller's own order.
pub fn into_map<K: Eq + Hash>(results: Vec<(K, CheckedOutcome)>) -> HashMap<K, CheckedOutcome> {
    results.into_iter().collect()
}

/// Locating the worker from a test binary.
///
/// The walk-up-from-`current_exe` convention every other spawned-binary
/// harness in this workspace uses (`u2s-render-test-harness`,
/// `u2s-server`'s `tests/support`). Lives here so the agent's tests, the
/// server's tests and this crate's own share one copy rather than three.
#[doc(hidden)]
pub mod test_support {
    use super::{RuleRunner, WORKER_BIN};
    use std::path::PathBuf;

    /// `target/<profile>/u2s-rules-worker`.
    pub fn worker_bin() -> PathBuf {
        let mut dir = std::env::current_exe().expect("current exe");
        dir.pop();
        if dir.ends_with("deps") {
            dir.pop();
        }
        let bin = dir.join(WORKER_BIN);
        assert!(
            bin.exists(),
            "{} not built at {} -- run `cargo build -p u2s-rules-host`",
            WORKER_BIN,
            bin.display()
        );
        bin
    }

    /// A runner over that binary, sized exactly as the server sizes its
    /// own.
    ///
    /// Deliberately not "one worker, and ask for more if you care":
    /// `u2s-server` builds one runner for its whole process, so a test
    /// harness that quietly serializes it would leave the entire server
    /// suite exercising a shape production never has. A test that wants a
    /// specific pool size says so with [`runner_with`].
    pub fn runner() -> RuleRunner {
        RuleRunner::new(worker_bin(), RuleRunner::workers_from_env())
            .expect("the worker binary is present")
    }

    /// A runner with a pool size the test chose.
    pub fn runner_with(workers: usize) -> RuleRunner {
        RuleRunner::new(worker_bin(), workers).expect("the worker binary is present")
    }
}
