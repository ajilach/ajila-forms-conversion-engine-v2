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

use agent::OutputTarget;
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
    /// content-package ZIP to pre-load as an editable template.
    #[arg(value_name = "DOCUMENT", required = true)]
    documents: Vec<PathBuf>,

    /// Conversion profile (AEM config + reference library). Defaults to the
    /// session's profile when resuming, or to the only installed profile.
    #[arg(long)]
    profile: Option<String>,

    /// What the run produces: "aem" or "redacto".
    #[arg(long, default_value = "aem", value_parser = parse_target)]
    target: OutputTarget,

    /// Directory the artefacts are written to (created if missing).
    #[arg(long, default_value = ".")]
    out: PathBuf,

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
    /// the desktop app.
    #[arg(long, value_name = "ID")]
    model: Option<String>,

    /// Reviewer → Author-fix rounds to allow before finalizing.
    #[arg(long, value_name = "N")]
    max_review_rounds: Option<usize>,

    /// Extra operator instructions, appended to every role's system prompt.
    #[arg(long, value_name = "TEXT")]
    instructions: Option<String>,

    /// Read the extra operator instructions from a file.
    #[arg(long, value_name = "PATH", conflicts_with = "instructions")]
    instructions_file: Option<PathBuf>,

    /// The AEM Forms image the verifier boots, overriding the desktop app's
    /// setting. See docker/aem/README.md.
    #[arg(long, value_name = "IMAGE")]
    aem_image: Option<String>,

    /// The Docker volume holding the deployed UBS platform, overriding the
    /// desktop app's setting.
    #[arg(long, value_name = "VOLUME")]
    aem_volume: Option<String>,

    /// Refine an earlier run instead of converting afresh: applies this feedback
    /// to the result held in --session. Skips the Analyst.
    #[arg(long, value_name = "TEXT", requires = "session")]
    feedback: Option<String>,

    /// Edit-history session to resume (list them with `blueprint sessions`).
    ///
    /// On its own it carries that session on with nothing to apply: the agent
    /// finishes the tree the earlier run left. Add --feedback to give it
    /// something specific to change. Either way the Analyst is skipped.
    #[arg(long, value_name = "ID")]
    session: Option<String>,

    /// Retries for a failed model turn before the run gives up. The controller's
    /// own automatic retries happen first; this budget is what a person would
    /// otherwise decide with the app's Retry button.
    #[arg(long, default_value = "2", value_name = "N")]
    retries: usize,

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

pub(crate) fn parse_target(value: &str) -> Result<OutputTarget, String> {
    OutputTarget::parse(value).ok_or_else(|| {
        let known: Vec<&str> = OutputTarget::ALL.iter().map(|t| t.as_str()).collect();
        format!(
            "unknown target `{value}` (expected one of: {})",
            known.join(", ")
        )
    })
}

/// Run the conversion and write what it produced.
pub fn run(args: ConvertArgs) -> Result<(), Box<dyn Error>> {
    let files = read_documents(&args.documents)?;
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

    let profile = resolve_profile(&args)?;
    let settings = resolve_settings(&args)?;

    // The run is async; the CLI owns the runtime the app gets from Dioxus.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let abort = AbortFlag::default();
    let plan = TurnPlan::for_settings(&settings);
    // Kept as our own `Arc` (not just `pipeline::SharedObserver::new`'s), so
    // `report_spend`/`transcript` below can read the concrete `ConsoleObserver`
    // back once the run has returned — methods `SharedObserver` does not
    // expose, since it only forwards the `RunObserver` trait itself.
    let observer = std::sync::Arc::new(std::sync::Mutex::new(ConsoleObserver::new(
        plan.context_window,
        args.retries,
    )));
    let obs = pipeline::SharedObserver::from_arc(observer.clone());

    println!("Profile: {}", profile.as_deref().unwrap_or("(none)"));
    println!("Target: {}", args.target.label());
    println!("{}", plan.describe());
    match args.target {
        OutputTarget::Aem => println!(
            "Verification: AEM image {}, data volume {} (checked before the run starts)",
            settings.aem_verify.image, settings.aem_verify.data_volume
        ),
        OutputTarget::Redacto => println!(
            "Verification: Redacto core image {}, rendering image {} (checked before the run starts)",
            settings.redacto_verify.core_image, settings.redacto_verify.rendering_image
        ),
    }

    let opts = runner::RunOptions {
        profile,
        target: args.target,
        settings,
        abort: abort.clone(),
    };

    let completed = runtime.block_on(async {
        // Ctrl-C stops the run at its next checkpoint rather than killing the
        // process: the stage ends cleanly and the edit history keeps what the
        // agent had built, so the session can be resumed. It does NOT finalize —
        // an interrupted run writes no artefacts. A second Ctrl-C is the
        // operating system's business.
        tokio::spawn({
            let abort = abort.clone();
            async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    eprintln!("\nStopping at the next checkpoint…");
                    abort.abort();
                }
            }
        });

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

    // Printed before the outcome is examined: a run that stopped early still
    // recorded its history under this id, and that is what a resume needs.
    println!("\n── Result ──");
    println!("Session: {}", completed.session_id);
    // Reported even when the run stopped early: those turns were still billed.
    {
        let mut console = observer.lock().unwrap_or_else(|p| p.into_inner());
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
    }

    let Some(outcome) = completed.outcome else {
        // Aborted, or the retry budget ran out. The observer said why.
        return Err("The run stopped before producing a result.".into());
    };

    println!("Elapsed: {}s", completed.elapsed_secs);
    if let Some(code) = &outcome.form_code {
        println!("Form code: {code}");
    }
    for warning in &outcome.warnings {
        println!("Warning: {warning}");
    }

    write_artifacts(
        &args,
        &outcome,
        &observer.lock().unwrap_or_else(|p| p.into_inner()),
    )?;

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

/// Write every artefact the run produced for its target, plus the transcript.
fn write_artifacts(
    args: &ConvertArgs,
    outcome: &pipeline::RunOutcome,
    observer: &ConsoleObserver,
) -> Result<(), Box<dyn Error>> {
    std::fs::create_dir_all(&args.out)
        .map_err(|e| format!("Could not create {}: {e}", args.out.display()))?;
    let code = outcome.form_code.as_deref();

    for artifact in Artifact::ALL {
        if !artifact.belongs_to(args.target) {
            continue;
        }
        match artifact.bytes_from(outcome) {
            Some(bytes) => write_file(&args.out, &artifact.filename(code), &bytes)?,
            None => println!("Not produced: {}", artifact.filename(code)),
        }
    }

    write_file(
        &args.out,
        &runner::artifact_filename("agent-log", code, "md"),
        observer.transcript().as_bytes(),
    )?;

    // The document the run authored, which a later run can be resumed from.
    write_file(
        &args.out,
        &runner::artifact_filename("document", code, "json"),
        &serde_json::to_vec_pretty(&outcome.document)?,
    )?;
    Ok(())
}

fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> Result<(), Box<dyn Error>> {
    let path = dir.join(name);
    std::fs::write(&path, bytes).map_err(|e| format!("Could not write {}: {e}", path.display()))?;
    println!("Wrote: {}", path.display());
    Ok(())
}

/// Read the sources, keeping the full file names: the agent tells PDFs from an
/// attached content package by extension, so a stem would hide both.
fn read_documents(paths: &[PathBuf]) -> Result<Sources, Box<dyn Error>> {
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

fn pdfs_only(files: &[(String, Vec<u8>)]) -> Sources {
    files
        .iter()
        .filter(|(name, _)| name.to_ascii_lowercase().ends_with(".pdf"))
        .cloned()
        .collect()
}

/// Which profile the run uses: the flag, else the resumed session's, else the
/// only one installed. Guessing between several would silently convert against
/// the wrong AEM config and reference library.
fn resolve_profile(args: &ConvertArgs) -> Result<Option<String>, Box<dyn Error>> {
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

    if let Some(session) = &args.session
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
fn resolve_settings(args: &ConvertArgs) -> Result<AppSettings, Box<dyn Error>> {
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
    if settings.active_model().is_empty() {
        return Err(format!(
            "No model for the {} provider. Pass --model, \
             or configure one in the desktop app's settings.",
            settings.llm_provider.as_str()
        )
        .into());
    }
    if let Some(rounds) = args.max_review_rounds {
        settings.max_review_rounds = rounds;
    }
    if let Some(text) = &args.instructions {
        settings.agent_instructions = text.clone();
    }
    if let Some(path) = &args.instructions_file {
        settings.agent_instructions = std::fs::read_to_string(path)
            .map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    }

    if let Some(image) = &args.aem_image {
        settings.aem_verify.image = image.clone();
    }
    if let Some(volume) = &args.aem_volume {
        settings.aem_verify.data_volume = volume.clone();
    }

    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_target_is_parsed_case_insensitively_and_rejected_when_unknown() {
        assert_eq!(parse_target("ReDaCtO").unwrap(), OutputTarget::Redacto);
        assert!(parse_target("html").is_err());
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
}
