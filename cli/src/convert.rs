//! `blueprint convert`: the autonomous conversion the desktop app runs, driven
//! from a terminal.
//!
//! The run itself is [`runner::run_fresh`] / [`runner::resume`] — the same
//! entry points the app calls, over the same `pipeline` controller and the same
//! Anthropic transport. What is different here is only the reporting (a
//! [`crate::console::ConsoleObserver`] instead of a Dioxus signal) and where the
//! artefacts land (an output directory instead of the Downloads folder).

use std::error::Error;
use std::path::{Path, PathBuf};

use clap::Args;
use pipeline::AbortFlag;
use runner::{AppSettings, Artifact, Provider, TurnPlan};

use crate::console::ConsoleObserver;

/// Environment variable consulted for the Anthropic key when `--api-key` is not
/// given.
const API_KEY_ENV: &str = "ANTHROPIC_API_KEY";

/// Environment variable consulted for the OpenAI-compatible key when
/// `--api-key` is not given.
const OPENAI_KEY_ENV: &str = "OPENAI_API_KEY";

/// A source file set: `(file name, bytes)`, the shape every consumer of the
/// conversion agent passes its sources in.
type Sources = Vec<(String, Vec<u8>)>;

#[derive(Args, Debug)]
pub struct ConvertArgs {
    /// Source document(s): the PDF(s) to convert, plus optionally an AEM
    /// content-package ZIP to pre-load as an editable template. A `*` in a
    /// file name selects every matching file, e.g. 'forms/AAAA_019_*.pdf'.
    #[arg(value_name = "DOCUMENT", required = true)]
    documents: Vec<PathBuf>,

    /// Directory the artefacts are written to (created if missing).
    #[arg(long, default_value = ".")]
    out: PathBuf,

    /// Refine an earlier run instead of converting afresh: applies this feedback
    /// to the result held in --session.
    #[arg(long, value_name = "TEXT", requires = "session")]
    feedback: Option<String>,

    /// Edit-history session to resume (list them with `blueprint sessions`).
    ///
    /// On its own it carries that session on with nothing to apply: the agent
    /// finishes the tree the earlier run left. Add --feedback to give it
    /// something specific to change.
    #[arg(long, value_name = "ID")]
    session: Option<String>,

    #[command(flatten)]
    run: RunArgs,
}

/// How a conversion runs: the profile, the model and what the run records.
/// Shared by `convert` and `batch`.
#[derive(Args, Debug)]
pub struct RunArgs {
    /// Conversion profile (AEM config + reference library). Defaults to the
    /// session's profile when resuming, or to the only installed profile.
    #[arg(long)]
    profile: Option<String>,

    /// Which API to talk to: "anthropic" (default) or "openai" for any
    /// OpenAI-compatible endpoint such as OpenRouter. Defaults to the desktop
    /// app's setting.
    #[arg(long, value_name = "NAME", value_parser = parse_provider)]
    provider: Option<Provider>,

    /// API root of the OpenAI-compatible endpoint, e.g.
    /// "https://openrouter.ai/api/v1". Implies --provider openai.
    #[arg(long, value_name = "URL")]
    base_url: Option<String>,

    /// API key for the selected provider. Defaults to $ANTHROPIC_API_KEY
    /// (or $OPENAI_API_KEY with --provider openai), then to the key configured
    /// in the desktop app.
    #[arg(long, value_name = "KEY")]
    api_key: Option<String>,

    /// Model id at the selected provider. Defaults to the model configured in
    /// the desktop app. The Author runs on it, and so do the Reviewer and the
    /// judges unless they are given their own.
    #[arg(long, value_name = "ID")]
    model: Option<String>,

    /// The Reviewer's own model id at the selected provider. Defaults to the
    /// one configured in the desktop app; an empty value means --model.
    #[arg(long, value_name = "ID")]
    reviewer_model: Option<String>,

    /// The judges' own model id at the selected provider (the agents that
    /// check the judged rules, one per rule, after each Author round). Defaults to the one configured in the desktop app; an empty value
    /// means the run's model.
    #[arg(long, value_name = "ID")]
    judge_model: Option<String>,

    /// A model that judges every negative verdict again before it stands,
    /// e.g. a strong model behind a cheap --judge-model: the cheap judges
    /// decide every rule, this one only the few they find broken. Defaults to
    /// the one configured in the desktop app; empty means no second look.
    #[arg(long, value_name = "ID")]
    judge_confirm_model: Option<String>,

    /// How many times the Reviewer may send the form back to the Author
    /// before finalizing.
    #[arg(long, value_name = "N")]
    max_review_rounds: Option<usize>,

    /// What the run may spend, in USD, before it is stopped (0: no limit).
    /// Defaults to the one configured in the desktop app, USD 100 unless
    /// changed.
    #[arg(long, value_name = "USD", value_parser = parse_usd)]
    max_spend: Option<f64>,

    /// The tokens the run may use before it is stopped (0: no limit): fresh
    /// input, cache writes and output, not cache reads. Defaults to the one
    /// configured in the desktop app, 20000000 unless changed.
    #[arg(long, value_name = "N")]
    max_tokens: Option<u64>,

    /// Extra operator instructions, appended to every role's system prompt.
    #[arg(long, value_name = "TEXT")]
    instructions: Option<String>,

    /// Read the extra operator instructions from a file.
    #[arg(long, value_name = "PATH", conflicts_with = "instructions")]
    instructions_file: Option<PathBuf>,

    /// Retries for a failed model turn before the run gives up. The controller's
    /// own automatic retries happen first; this budget is what a person would
    /// otherwise decide with the app's Retry button.
    #[arg(long, default_value = "2", value_name = "N")]
    retries: usize,

    /// Record the run for analysis: a folder with a report of where the time
    /// went, a timeline, per-stage transcripts, the full trace and an
    /// evaluation template. Off unless asked for. The folder goes under
    /// `run-analysis/` in the engine's checkout, as in the app.
    #[arg(long)]
    analysis: bool,

    /// Directory the run's analysis folder is created in instead of
    /// `run-analysis/`. Implies `--analysis`.
    #[arg(long, value_name = "DIR")]
    analysis_dir: Option<PathBuf>,
}

impl RunArgs {
    /// The operator-level retry budget of each run.
    pub(crate) fn retries(&self) -> usize {
        self.retries
    }
}

fn parse_provider(value: &str) -> Result<Provider, String> {
    Provider::parse(value).ok_or_else(|| {
        let known: Vec<&str> = Provider::ALL.iter().map(|p| p.as_str()).collect();
        format!(
            "unknown provider `{value}` (expected one of: {})",
            known.join(", ")
        )
    })
}

/// Run the conversion and write what it produced.
pub fn run(args: ConvertArgs) -> Result<(), Box<dyn Error>> {
    let mut documents = Vec::new();
    for document in &args.documents {
        documents.extend(expand(document)?);
    }
    let files = read_documents(&documents)?;
    let pdfs = pdfs_only(&files);
    // The agent has to have something to work from: sources to convert, or an
    // AEM content package to modify. A feedback run refines a previous result
    // against those same sources, so for it the PDFs are not optional.
    if pdfs.is_empty() {
        if args.feedback.is_some() {
            return Err("Applying feedback needs the run's source PDF(s) as well.".into());
        }
        if agent::conversion::template_of(&files).is_none() {
            return Err(
                "Nothing to convert: pass a source PDF, an AEM content package, or both.".into(),
            );
        }
    }

    let profile = resolve_profile(&args.run, args.session.as_deref())?;
    let settings = resolve_settings(&args.run)?;

    // The run is async; the CLI owns the runtime the app gets from Dioxus.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let abort = AbortFlag::default();
    let plan = TurnPlan::for_settings(&settings);
    // Kept as our own `Arc` (not just `pipeline::SharedObserver::new`'s), so
    // `finish` below can read the concrete `ConsoleObserver` back once the run
    // has returned — methods `SharedObserver` does not expose, since it only
    // forwards the `RunObserver` trait itself.
    let observer = std::sync::Arc::new(std::sync::Mutex::new(ConsoleObserver::new(
        plan.context_window,
        args.run.retries,
    )));
    let obs = pipeline::SharedObserver::from_arc(observer.clone());

    describe_run(profile.as_deref(), &plan, &settings);

    let opts = runner::RunOptions {
        profile,
        settings,
        abort: abort.clone(),
    };

    let completed = runtime.block_on(async {
        stop_on_ctrl_c(abort.clone());

        match args.session.clone() {
            // Carry that session on, applying whatever --feedback gave it. Blank
            // feedback is a continuation rather than an empty instruction, which
            // is what `resuming` decides.
            Some(session) => {
                let seed =
                    pipeline::RunSeed::resuming(args.feedback.as_deref().unwrap_or_default());
                runner::resume(seed, pdfs, &opts, session, &obs).await
            }
            None => {
                let label = files
                    .iter()
                    .map(|(name, _)| name.clone())
                    .collect::<Vec<_>>()
                    .join(", ");
                runner::run_fresh(files, &opts, &label, &obs).await
            }
        }
    })?;

    finish(&completed, &observer, &args.out)?;

    println!(
        "\nRefine it with: blueprint convert {} --session {} --feedback \"…\"",
        args.documents
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(" "),
        completed.session_id
    );
    Ok(())
}

/// What a run uses, printed before it starts.
pub(crate) fn describe_run(profile: Option<&str>, plan: &TurnPlan, settings: &AppSettings) {
    println!("Profile: {}", profile.unwrap_or("(none)"));
    println!("{}{}", plan.describe(), runner::turns::describe_role_models(settings));
    println!(
        "Verification: AEM image {} (pulled from GitHub when missing, checked before the run starts)",
        agent::u2s::AEM_IMAGE
    );
}

/// Ctrl-C stops the runs at their next checkpoint rather than killing the
/// process: each stage ends cleanly and the edit history keeps what the agent
/// had built, so the session can be resumed. It does NOT finalize — an
/// interrupted run writes no artefacts. A second Ctrl-C is the operating
/// system's business.
pub(crate) fn stop_on_ctrl_c(abort: AbortFlag) {
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("\nStopping at the next checkpoint…");
            abort.abort();
        }
    });
}

/// A USD amount from the command line: a finite number of 0 or more.
fn parse_usd(text: &str) -> Result<f64, String> {
    let usd: f64 = text.trim().parse().map_err(|_| format!("{text:?} is not a USD amount"))?;
    if !usd.is_finite() || usd < 0.0 {
        return Err(format!("{text:?} is not a USD amount of 0 or more"));
    }
    Ok(usd)
}

/// Reports a finished run and writes what it produced to `out`: its session
/// (printed first: a run that stopped early still recorded its history under
/// it, and that is what a resume needs), what it cost, and its artefacts. An
/// error when it stopped before producing a result.
pub(crate) fn finish(
    completed: &runner::Completed,
    observer: &std::sync::Mutex<ConsoleObserver>,
    out: &Path,
) -> Result<(), Box<dyn Error>> {
    let mut console = observer.lock().unwrap_or_else(|p| p.into_inner());
    console.say("\n── Result ──");
    console.say(format!("Session: {}", completed.session_id));
    // Reported even when the run stopped early: those turns were still billed.
    console.report_spend();

    // Fold this run's spend into the session's running total, so a form
    // resumed for a later feedback round keeps what its earlier rounds
    // already cost rather than starting the figure over at zero.
    if let Some(run_spend) = console.spend() {
        let mut total: pipeline::Spend = agent::db::session_spend_json(&completed.session_id)
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        total.merge(&run_spend);
        console.report_total_spend(&total);
        if let Ok(json) = serde_json::to_string(&total) {
            agent::db::set_session_spend_json(&completed.session_id, &json);
        }
    }

    if let Some(limit) = &completed.limit_reached {
        return Err(format!("Run stopped: {limit}.").into());
    }
    let Some(outcome) = &completed.outcome else {
        // Aborted, or the retry budget ran out. The observer said why.
        return Err("The run stopped before producing a result.".into());
    };

    console.say(format!("Elapsed: {}s", completed.elapsed_secs));
    if let Some(code) = &outcome.form_code {
        console.say(format!("Form code: {code}"));
    }
    for warning in &outcome.warnings {
        console.say(format!("Warning: {warning}"));
    }

    write_artifacts(out, outcome, &console)
}

/// Write every artefact the run produced, plus the transcript.
fn write_artifacts(
    out: &Path,
    outcome: &pipeline::RunOutcome,
    observer: &ConsoleObserver,
) -> Result<(), Box<dyn Error>> {
    std::fs::create_dir_all(out)
        .map_err(|e| format!("Could not create {}: {e}", out.display()))?;
    let code = outcome.form_code.as_deref();

    for artifact in Artifact::ALL {
        match artifact.bytes_from(outcome) {
            Some(bytes) => write_file(out, &artifact.filename(code), &bytes)?,
            None => observer.say(format!("Not produced: {}", artifact.filename(code))),
        }
    }

    write_file(
        out,
        &runner::artifact_filename("agent-log", code, "md"),
        observer.transcript().as_bytes(),
    )?;

    // The document the run authored, which a later run can be resumed from.
    write_file(
        out,
        &runner::artifact_filename("document", code, "json"),
        &serde_json::to_vec_pretty(&outcome.document)?,
    )?;

    // What the final build looked like on the verifier, a directory per
    // language, numbered in order.
    if let Some(review) = &outcome.review {
        for language in &review.languages {
            let dir = out.join(runner::review_dirname(code)).join(&language.language);
            std::fs::create_dir_all(&dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
            for (side, images) in language.images.sides() {
                for (index, image) in images.iter().enumerate() {
                    write_file(&dir, &runner::review_image_filename(side, index, &image.label), &image.png)?;
                }
            }
        }
    }
    Ok(())
}

fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> Result<(), Box<dyn Error>> {
    let path = dir.join(name);
    std::fs::write(&path, bytes).map_err(|e| format!("Could not write {}: {e}", path.display()))?;
    println!("Wrote: {}", path.display());
    Ok(())
}

/// The files `path` names: itself, or, when its file name holds a `*`, every
/// file in its directory whose name matches (a `*` matching any run of
/// characters), in name order. A pattern that matches nothing is an error, not
/// an empty run.
pub(crate) fn expand(path: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    if dir.to_string_lossy().contains('*') {
        return Err(format!("A `*` only selects files, not directories: {}", path.display()).into());
    }
    let Some(pattern) = path.file_name().and_then(|n| n.to_str()).filter(|n| n.contains('*')) else {
        return Ok(vec![path.to_path_buf()]);
    };
    let mut matches: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("Could not read {}: {e}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|p| p.is_file() && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| matches_pattern(pattern, n)))
        .collect();
    if matches.is_empty() {
        return Err(format!("No file matches {}", path.display()).into());
    }
    matches.sort();
    Ok(matches)
}

/// Whether `name` matches `pattern`, in which `*` stands for any run of
/// characters (none included) and everything else for itself.
fn matches_pattern(pattern: &str, name: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };
    let middle: Vec<&str> = parts.collect();
    let Some((last, middle)) = middle.split_last() else {
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

/// Read the sources, keeping the full file names: the agent tells PDFs from an
/// attached content package by extension, so a stem would hide both.
pub(crate) fn read_documents(paths: &[PathBuf]) -> Result<Sources, Box<dyn Error>> {
    let mut files = Vec::with_capacity(paths.len());
    for path in paths {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("Unreadable file name: {}", path.display()))?
            .to_string();
        let bytes =
            std::fs::read(path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
        files.push((name, bytes));
    }
    Ok(files)
}

pub(crate) fn pdfs_only(files: &[(String, Vec<u8>)]) -> Sources {
    files
        .iter()
        .filter(|(name, _)| agent::conversion::is_source_pdf(name))
        .cloned()
        .collect()
}

/// Which profile the run uses: the flag, else the resumed `session`'s, else the
/// only one installed. Guessing between several would silently convert against
/// the wrong AEM config and reference library.
pub(crate) fn resolve_profile(args: &RunArgs, session: Option<&str>) -> Result<Option<String>, Box<dyn Error>> {
    let available = agent::profiles::list_profiles();

    if let Some(name) = &args.profile {
        if !available.iter().any(|p| p == name) {
            return Err(format!(
                "Unknown profile `{name}` (installed: {})",
                available.join(", ")
            )
            .into());
        }
        return Ok(Some(name.clone()));
    }

    if let Some(session) = session
        && let Some(profile) = agent::db::session_profile(session)
    {
        return Ok(Some(profile));
    }

    match available.as_slice() {
        [only] => Ok(Some(only.clone())),
        [] => Ok(None),
        many => Err(format!(
            "Several profiles are installed ({}) — pick one with --profile.",
            many.join(", ")
        )
        .into()),
    }
}

/// The desktop app's saved settings, with this invocation's overrides applied.
///
/// Sharing the settings store is the point: a key, model and AEM connection
/// configured in the app work here without being repeated on the command line.
pub(crate) fn resolve_settings(args: &RunArgs) -> Result<AppSettings, Box<dyn Error>> {
    let mut settings = AppSettings::load();

    // --base-url is only meaningful for the OpenAI-compatible path, so passing
    // it selects that path: an operator who names an endpoint means to use it.
    settings.llm_provider = match (args.provider, &args.base_url) {
        (Some(p), _) => p,
        (None, Some(_)) => Provider::OpenAi,
        (None, None) => settings.llm_provider,
    };
    if let Some(url) = &args.base_url {
        settings.openai_base_url = url.clone();
    }

    let key_env = match settings.llm_provider {
        Provider::Anthropic => API_KEY_ENV,
        Provider::OpenAi => OPENAI_KEY_ENV,
    };
    let key = args
        .api_key
        .clone()
        .or_else(|| std::env::var(key_env).ok().filter(|k| !k.trim().is_empty()));
    match settings.llm_provider {
        Provider::Anthropic => {
            if let Some(key) = key {
                settings.anthropic_api_key = key;
            }
            if let Some(model) = &args.model {
                settings.anthropic_model = model.clone();
            }
        }
        Provider::OpenAi => {
            if let Some(key) = key {
                settings.openai_api_key = key;
            }
            if let Some(model) = &args.model {
                settings.openai_model = model.clone();
            }
        }
    }

    // One check for both paths, phrased for the console: the app's own message
    // points at a settings screen this process does not have.
    if settings.active_api_key().is_empty() {
        return Err(format!(
            "No API key for the {} provider. Pass --api-key, set {key_env}, \
             or configure one in the desktop app's settings.",
            settings.llm_provider.as_str()
        )
        .into());
    }
    if let Some(model) = &args.reviewer_model {
        settings.reviewer_model = model.trim().to_string();
    }
    if let Some(model) = &args.judge_model {
        settings.judge_model = model.trim().to_string();
    }
    if let Some(model) = &args.judge_confirm_model {
        settings.judge_confirm_model = model.trim().to_string();
    }
    if settings.active_model().is_empty() {
        return Err(format!(
            "No model for the {} provider. Pass --model, \
             or configure one in the desktop app's settings.",
            settings.llm_provider.as_str()
        )
        .into());
    }
    // Recorded only when asked for. Unless named, the analysis folder is the
    // same as the app's: run-analysis/ in the engine's checkout, so every run,
    // app or console, is in one place.
    settings.run_analysis = args.analysis || args.analysis_dir.is_some();
    settings.run_analysis_dir = args
        .analysis_dir
        .as_ref()
        .map(|dir| dir.display().to_string())
        .unwrap_or_default();
    if let Some(rounds) = args.max_review_rounds {
        settings.max_review_rounds = rounds;
    }
    if let Some(usd) = args.max_spend {
        settings.max_spend_usd = usd;
    }
    if let Some(tokens) = args.max_tokens {
        settings.max_run_tokens = tokens;
    }
    if let Some(text) = &args.instructions {
        settings.agent_instructions = text.clone();
    }
    if let Some(path) = &args.instructions_file {
        settings.agent_instructions = std::fs::read_to_string(path)
            .map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    }

    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A spending limit is a USD amount of 0 or more; anything else is refused
    /// where it is typed, not read as some other limit.
    #[test]
    fn a_spending_limit_must_be_a_non_negative_amount() {
        assert_eq!(parse_usd("100"), Ok(100.0));
        assert_eq!(parse_usd("2.5"), Ok(2.5));
        assert_eq!(parse_usd("0"), Ok(0.0));
        assert!(parse_usd("-1").is_err());
        assert!(parse_usd("NaN").is_err());
        assert!(parse_usd("inf").is_err());
        assert!(parse_usd("ten").is_err());
    }

    /// The full file name has to survive: the agent splits sources from an
    /// attached content package on the `.pdf` extension, so a stem would leave
    /// the PDFs looking like neither.
    #[test]
    fn documents_keep_their_extension() {
        let dir = std::env::temp_dir().join("blueprint-cli-convert-test");
        std::fs::create_dir_all(&dir).unwrap();
        let pdf = dir.join("form_DE.pdf");
        std::fs::write(&pdf, b"%PDF-1.4").unwrap();

        let files = read_documents(std::slice::from_ref(&pdf)).unwrap();
        assert_eq!(files[0].0, "form_DE.pdf");
        assert_eq!(pdfs_only(&files).len(), 1);

        let _ = std::fs::remove_file(&pdf);
    }

    #[test]
    fn a_star_matches_any_run_of_characters() {
        assert!(matches_pattern("AAAA_019_*.pdf", "AAAA_019_DE.pdf"));
        assert!(matches_pattern("AAAA_019_*.pdf", "AAAA_019_.pdf"));
        assert!(!matches_pattern("AAAA_019_*.pdf", "AAAA_019_DE.zip"));
        assert!(!matches_pattern("AAAA_019_*.pdf", "AAAB_019_DE.pdf"));
        assert!(matches_pattern("*_019_*", "AAAA_019_EN.pdf"));
        assert!(!matches_pattern("a*a", "a"), "the parts may not overlap");
        assert!(matches_pattern("plain.pdf", "plain.pdf"));
        assert!(!matches_pattern("plain.pdf", "plain.pdfx"));
    }

    /// A pattern names every matching file of its directory, in name order;
    /// one matching nothing is refused rather than converting nothing.
    #[test]
    fn a_pattern_selects_every_matching_file() {
        let dir = std::env::temp_dir().join(format!("blueprint-cli-expand-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["AAAA_019_EN.pdf", "AAAA_019_DE.pdf", "AAAB_019_DE.pdf"] {
            std::fs::write(dir.join(name), b"%PDF-1.4").unwrap();
        }
        let found = expand(&dir.join("AAAA_019_*.pdf")).unwrap();
        let names: Vec<_> = found.iter().map(|p| p.file_name().unwrap().to_str().unwrap()).collect();
        assert_eq!(names, ["AAAA_019_DE.pdf", "AAAA_019_EN.pdf"]);
        assert_eq!(expand(&dir.join("AAAB_019_DE.pdf")).unwrap().len(), 1);
        assert!(expand(&dir.join("ZZZZ_*.pdf")).err().unwrap().to_string().contains("No file matches"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
