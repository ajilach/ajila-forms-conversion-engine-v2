//! Evaluates generated rule scripts, one at a time, in a process of its own.
//!
//! # Why this is a separate process at all
//!
//! `u2s-rules` runs LLM-written JavaScript on `boa`, and boa offers no heap
//! cap and no interrupt hook. In-process that means two failure modes the
//! host cannot defend against: a script asking for more memory than exists
//! aborts the **whole server** (Rust's allocation-failure path is `abort`,
//! not an unwind, so no `catch_unwind` and no `spawn_blocking` contains it),
//! and a script stuck in a tight non-looping computation never returns, so
//! its thread is never reclaimed and `tokio::time::timeout` cannot cancel
//! it.
//!
//! Out of process both become ordinary, survivable events: the kernel kills
//! this program and the host attributes the death to exactly the rule that
//! was running. PLAN.md's "scripts get a CPU-time and memory budget; an
//! overrunning or throwing script fails **the rule**, not the run" is only
//! true because of this binary.
//!
//! # The caps are self-imposed, deliberately
//!
//! `setrlimit` is called here rather than through `Command::pre_exec` in the
//! parent: `pre_exec` runs between `fork` and `exec` where almost nothing is
//! safe to call and the closure must be `unsafe`, and putting the limits in
//! the parent hides them from anyone reading the program they constrain.

use std::io::Write;

use tokio::io::{AsyncBufReadExt, BufReader};
use u2s_rules::ScriptBudget;
use u2s_rules_host::protocol::{CheckResponse, WorkerRequest, WorkerResponse};

/// Address space this worker may map, in MiB. Not a precise heap accounting
/// -- `RLIMIT_AS` counts mappings, which overshoots real usage -- but the
/// point is a ceiling that exists at all, and a generous one still turns a
/// 68 GB allocation into an immediate failure.
const DEFAULT_MEM_LIMIT_MB: u64 = 256;

/// `libc::setrlimit`'s `resource` parameter is not one type across the unix
/// targets this program runs on: recent `libc` on glibc Linux types it as
/// `u32` (`__rlimit_resource_t`), while macOS keeps `c_int`. An `as` cast at
/// each call site handles both without a second `cfg` arm to keep in sync
/// with `libc`'s own definitions.
#[cfg(target_os = "linux")]
type RlimitResource = u32;
#[cfg(not(target_os = "linux"))]
type RlimitResource = libc::c_int;

/// SAFETY: `setrlimit` with a well-formed `rlimit` is safe to call on a live
/// process and touches no memory this program owns.
#[cfg(unix)]
fn set_limit(resource: RlimitResource, value: u64) -> bool {
    let limit = libc::rlimit {
        rlim_cur: value as libc::rlim_t,
        rlim_max: value as libc::rlim_t,
    };
    unsafe { libc::setrlimit(resource, &limit) == 0 }
}

/// The address-space ceiling, set once.
///
/// Unlike CPU time this is a ceiling on what is mapped *right now*, not a
/// running total, so it means the same thing on the hundredth job as on the
/// first.
///
/// `RLIMIT_AS` is the real memory ceiling on Linux, which is where this runs
/// in production. macOS implements neither it nor `RLIMIT_DATA` and rejects
/// both with EINVAL, so `DATA` is tried as a fallback and the absence is
/// stated once rather than warned about per worker.
///
/// Losing the cap weakens the guarantee without removing it: a script asking
/// for more memory than exists still aborts *this process* and still fails
/// only its own rule, because that property comes from being a separate
/// process, not from the limit. What the limit adds is a ceiling low enough
/// that the child dies before it has taken a meaningful bite out of the
/// machine.
#[cfg(unix)]
fn cap_memory(mem_limit_bytes: u64) {
    if !set_limit(libc::RLIMIT_AS as RlimitResource, mem_limit_bytes)
        && !set_limit(libc::RLIMIT_DATA as RlimitResource, mem_limit_bytes)
    {
        eprintln!(
            "u2s-rules-worker: this platform enforces no address-space limit; \
             a script's memory use is bounded by allocation failure alone"
        );
    }
}

/// CPU already burned by this process, rounded up to whole seconds.
///
/// SAFETY: `getrusage` writes into the `rusage` this function owns and reads
/// nothing else.
#[cfg(unix)]
fn cpu_used_seconds() -> u64 {
    // SAFETY: `rusage` is plain old data; an all-zero value is a valid one,
    // and `getrusage` overwrites it wholesale.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
        // Unreadable usage means the ceiling below cannot be placed
        // relative to it. Zero is the conservative answer: the ceiling
        // lands where it would have on a fresh process, so a job is
        // stopped too early rather than not at all.
        return 0;
    }
    // Summed in `libc`'s own types, whose widths differ by platform, and
    // converted once at the end -- `saturating_add` because two `time_t`s
    // are added and nothing upstream bounds them.
    let secs = usage.ru_utime.tv_sec.saturating_add(usage.ru_stime.tv_sec);
    let usecs = usage
        .ru_utime
        .tv_usec
        .saturating_add(usage.ru_stime.tv_usec);
    // Rounded up, so a partial second already spent is not handed to the
    // next script.
    u64::try_from(secs).unwrap_or(0) + u64::from(usecs > 0)
}

/// Moves the CPU ceiling forward to cover one more job.
///
/// **`RLIMIT_CPU` is a running total for the life of the process, and this
/// process runs many jobs.** Setting it once to a single job's budget
/// therefore does not bound each job at all: it bounds the worker, and the
/// third honest script on a reused worker is killed by the kernel for the
/// CPU the first two spent. That is not a hypothetical -- it is what a
/// batch of ordinary tree-walking scripts did, and the host reported the
/// innocent script as broken.
///
/// So the ceiling is re-placed before every job, at *actual usage so far*
/// plus this job's budget. The hard limit is left exactly as inherited:
/// only the soft ceiling moves, and never above the hard one.
#[cfg(unix)]
fn cap_cpu_for_next_job(cpu_seconds: u64) {
    // SAFETY: `getrlimit` writes into an `rlimit` this function owns.
    let mut current = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(libc::RLIMIT_CPU, &mut current) } != 0 {
        eprintln!(
            "u2s-rules-worker: could not read RLIMIT_CPU: {}",
            std::io::Error::last_os_error()
        );
        return;
    }

    let want = cpu_used_seconds().saturating_add(cpu_seconds);
    let limit = libc::rlimit {
        rlim_cur: (want as libc::rlim_t).min(current.rlim_max),
        rlim_max: current.rlim_max,
    };
    // SAFETY: as `set_limit`.
    if unsafe { libc::setrlimit(libc::RLIMIT_CPU, &limit) } != 0 {
        eprintln!(
            "u2s-rules-worker: could not set RLIMIT_CPU: {}",
            std::io::Error::last_os_error()
        );
    }
}

#[cfg(not(unix))]
fn cap_memory(_mem_limit_bytes: u64) {
    // The host's own per-job deadline still applies, so a worker here is
    // bounded in wall clock but not in memory. Said out loud rather than
    // silently skipped, because it is a real weakening of the guarantee.
    eprintln!(
        "u2s-rules-worker: no rlimit support on this platform; \
         a script's memory use is bounded only by the machine"
    );
}

#[cfg(not(unix))]
fn cap_cpu_for_next_job(_cpu_seconds: u64) {}

fn mem_limit_bytes() -> u64 {
    std::env::var("U2S_RULES_MEM_LIMIT_MB")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|mb| *mb > 0)
        .unwrap_or(DEFAULT_MEM_LIMIT_MB)
        .saturating_mul(1024 * 1024)
}

/// The CPU ceiling, taken from the request's own budget so it tracks
/// whatever the host asked for, with a floor of one second because
/// `RLIMIT_CPU` has one-second granularity and a sub-second value would
/// round to an immediate kill.
///
/// `multiplier` widens this for a job carrying a `fix_js` -- see
/// `CheckRequest::eval_multiplier`'s own docs for why this must derive from
/// the exact same number the host's deadline does.
fn cpu_seconds(budget: &ScriptBudget, multiplier: u32) -> u64 {
    budget
        .wall_clock
        .as_secs()
        .saturating_mul(u64::from(multiplier))
        .max(1)
        .saturating_add(1)
}

/// Runs one job. Every script failure is part of the answer, never a
/// process exit: only a script exceeding its memory or CPU budget ends
/// this process, and the host reads that as a failed answer.
fn answer(request: &WorkerRequest, budget: &ScriptBudget) -> WorkerResponse {
    match request {
        WorkerRequest::Check(request) => {
            WorkerResponse::Check(CheckResponse::from(u2s_rules::classify_check_and_fix(
                &request.script_js,
                request.fix_js.as_deref(),
                &request.output,
                &request.schema,
                &request.facts,
                budget,
            )))
        }
        WorkerRequest::Requires { script_js, .. } => WorkerResponse::Requires {
            result: u2s_rules::read_requires(script_js, budget).map_err(|err| err.to_string()),
        },
        WorkerRequest::Extract {
            script_js, ingest, ..
        } => WorkerResponse::Extract {
            result: u2s_rules::run_extract(script_js, ingest, budget)
                .map_err(|err| err.to_string()),
        },
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut memory_capped = false;

    // Reading line-by-line and answering in order is the entire protocol.
    // One job in flight at a time is not a simplification: it is what lets
    // the host name the exact request that was running when this process
    // died, which is the only way a crash can fail one rule instead of a
    // whole batch.
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<WorkerRequest>(&line) {
            Ok(request) => {
                let budget: ScriptBudget = request.budget();
                if !memory_capped {
                    cap_memory(mem_limit_bytes());
                    memory_capped = true;
                }
                // Every job, not just the first: see `cap_cpu_for_next_job`.
                cap_cpu_for_next_job(cpu_seconds(&budget, request.eval_multiplier()));
                answer(&request, &budget)
            }
            // A request this worker cannot read is the host's bug, not the
            // rule's -- but answering keeps the stream in step, and the
            // host records it against the job it was expecting an answer
            // for (a kind it did not ask for is itself a failure there).
            Err(err) => WorkerResponse::Check(CheckResponse::broken(format!(
                "worker could not read the request: {err}"
            ))),
        };

        let mut encoded = match serde_json::to_vec(&response) {
            Ok(encoded) => encoded,
            Err(err) => serde_json::to_vec(&WorkerResponse::Check(CheckResponse::broken(format!(
                "worker could not encode its answer: {err}"
            ))))
            .expect("a broken response with a plain string reason always serializes"),
        };
        encoded.push(b'\n');

        // Straight to the raw handle and flushed: a response still sitting
        // in a buffer when a later script trips the memory cap would be
        // lost, and the host would attribute the death to the wrong job.
        let mut stdout = std::io::stdout().lock();
        if stdout.write_all(&encoded).is_err() || stdout.flush().is_err() {
            // The host has gone; there is nobody left to answer.
            return;
        }
    }
}
