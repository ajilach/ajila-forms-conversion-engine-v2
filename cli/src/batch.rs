//! `blueprint batch`: several forms converted at once, in one process.
//!
//! Each form is its own run of [`runner::run_fresh`], as `convert` makes it.
//! What running them in one process buys is the AEM verifier: the conversions
//! of a process share its one AEM instance (`agent::u2s::VerifierHold`), each
//! checking its form in sessions of its own and taking turns for the calls,
//! while separate processes would have to wait for each other's verifier.

use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use clap::Args;
use pipeline::AbortFlag;
use runner::TurnPlan;

use crate::console::ConsoleObserver;
use crate::convert::{self, RunArgs};

#[derive(Args, Debug)]
pub struct BatchArgs {
    /// The forms to convert, one per argument: a source PDF, or a directory
    /// holding one form's source PDF(s) and, optionally, an AEM content-package
    /// ZIP to pre-load as an editable template.
    #[arg(value_name = "FORM", required = true)]
    forms: Vec<PathBuf>,

    /// How many forms convert at once. They share one AEM instance, which
    /// holds at most 8 open forms, and take turns for its calls.
    #[arg(long, default_value = "3", value_name = "N", value_parser = clap::value_parser!(u8).range(1..=8))]
    jobs: u8,

    /// Directory each form's artefacts go to, in a folder of its own named
    /// after the form (created if missing).
    #[arg(long, default_value = ".")]
    out: PathBuf,

    #[command(flatten)]
    run: RunArgs,
}

/// One form of the batch: what it is called and its source files.
struct Form {
    label: String,
    files: Vec<(String, Vec<u8>)>,
}

/// How one form's run ended.
struct Finished {
    label: String,
    session_id: Option<String>,
    elapsed_secs: u64,
    result: Result<(), String>,
}

/// Convert every form, `--jobs` at a time, and write each one's artefacts to a
/// folder of its own. Fails when any form did not produce a result.
pub fn run(args: BatchArgs) -> Result<(), Box<dyn Error>> {
    let forms = read_forms(&args.forms)?;
    let profile = convert::resolve_profile(&args.run, None)?;
    let settings = convert::resolve_settings(&args.run)?;
    let plan = TurnPlan::for_settings(&settings);
    convert::describe_run(profile.as_deref(), &plan, &settings);
    println!("Forms: {} ({} at a time)", forms.len(), args.jobs);

    // Several runs make progress at once, so they get a thread pool.
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let abort = AbortFlag::default();
    let opts = Arc::new(runner::RunOptions { profile, settings, abort: abort.clone() });
    let retries = args.run.retries();
    let context_window = plan.context_window;
    let out = args.out.clone();

    let finished = runtime.block_on(async move {
        convert::stop_on_ctrl_c(abort);

        // The preflight every run makes, once up front: a setup that cannot
        // verify refuses the whole batch before any form spends a token.
        let preflight = pipeline::SharedObserver::from_arc(Arc::new(Mutex::new(
            ConsoleObserver::new(context_window, 0).labelled("batch"),
        )));
        let verification = runner::verification_for(&opts, &preflight).await?;
        // Keeps the AEM up between one form ending and the next starting,
        // rather than tearing it down and booting it again.
        let hold = agent::u2s::VerifierHold::new(&verification)?;

        let slots = Arc::new(tokio::sync::Semaphore::new(usize::from(args.jobs)));
        let mut runs = tokio::task::JoinSet::new();
        for form in forms {
            let (opts, slots, out) = (Arc::clone(&opts), Arc::clone(&slots), out.join(&form.label));
            runs.spawn(async move {
                let _slot = slots.acquire_owned().await.expect("the slots are never closed");
                convert_one(form, &opts, context_window, retries, &out).await
            });
        }
        let mut finished = Vec::new();
        while let Some(joined) = runs.join_next().await {
            finished.push(joined.map_err(|e| format!("a form's run failed: {e}"))?);
        }

        if let Err(e) = hold.release().await {
            eprintln!("warning: the verification containers could not be removed: {e}");
        }
        Ok::<_, String>(finished)
    })?;

    summarize(finished)
}

/// Runs one form and writes what it produced to `out`.
async fn convert_one(
    form: Form,
    opts: &runner::RunOptions,
    context_window: usize,
    retries: usize,
    out: &Path,
) -> Finished {
    let started = Instant::now();
    let observer = Arc::new(Mutex::new(ConsoleObserver::new(context_window, retries).labelled(&form.label)));
    let obs = pipeline::SharedObserver::from_arc(observer.clone());
    let label = form.files.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>().join(", ");
    let (session_id, result) = match runner::run_fresh(form.files, opts, &label, &obs).await {
        Ok(completed) => {
            let written = convert::finish(&completed, &observer, out).map_err(|e| e.to_string());
            (Some(completed.session_id), written)
        }
        Err(e) => (None, Err(e)),
    };
    Finished { label: form.label, session_id, elapsed_secs: started.elapsed().as_secs(), result }
}

/// Prints how every form ended, and fails when any did not produce a result.
fn summarize(mut finished: Vec<Finished>) -> Result<(), Box<dyn Error>> {
    finished.sort_by(|a, b| a.label.cmp(&b.label));
    println!("\n── Batch ──");
    for form in &finished {
        let session = form.session_id.as_deref().unwrap_or("(none)");
        let outcome = match &form.result {
            Ok(()) => "done".to_string(),
            Err(e) => format!("failed: {e}"),
        };
        println!("{}: {outcome} ({}s, session {session})", form.label, form.elapsed_secs);
    }
    let failed = finished.iter().filter(|f| f.result.is_err()).count();
    if failed > 0 {
        return Err(format!("{failed} of {} forms did not produce a result", finished.len()).into());
    }
    Ok(())
}

/// Reads every form's sources. Two forms with one label would write to one
/// folder, and two with the same sources would convert one form twice on the
/// shared AEM, so both are refused before anything runs.
fn read_forms(paths: &[PathBuf]) -> Result<Vec<Form>, Box<dyn Error>> {
    let mut forms = Vec::with_capacity(paths.len());
    for path in paths {
        forms.push(read_form(path)?);
    }
    let mut labels: HashMap<&str, &Path> = HashMap::new();
    let mut sources: HashMap<String, &Path> = HashMap::new();
    for (form, path) in forms.iter().zip(paths) {
        if let Some(other) = labels.insert(&form.label, path) {
            return Err(format!(
                "{} and {} would both write to the folder {:?}; rename one",
                other.display(),
                path.display(),
                form.label
            )
            .into());
        }
        if let Some(other) = sources.insert(runner::source_hash(&form.files), path) {
            return Err(format!("{} and {} are the same form", other.display(), path.display()).into());
        }
    }
    Ok(forms)
}

/// One form: a single source PDF, or a directory holding its sources (the PDFs
/// and an optional template ZIP, everything else ignored).
fn read_form(path: &Path) -> Result<Form, Box<dyn Error>> {
    let label = path
        .file_stem()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("Unreadable form name: {}", path.display()))?
        .to_string();
    let documents = if path.is_dir() {
        let mut documents: Vec<PathBuf> = std::fs::read_dir(path)
            .map_err(|e| format!("Could not read {}: {e}", path.display()))?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|p| {
                p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                    e.eq_ignore_ascii_case("pdf") || e.eq_ignore_ascii_case("zip")
                })
            })
            .collect();
        documents.sort();
        documents
    } else {
        vec![path.to_path_buf()]
    };
    let files = convert::read_documents(&documents)?;
    if convert::pdfs_only(&files).is_empty() && agent::conversion::template_of(&files).is_none() {
        return Err(format!(
            "Nothing to convert in {}: it needs a source PDF, an AEM content package, or both.",
            path.display()
        )
        .into());
    }
    Ok(Form { label, files })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("blueprint-cli-batch-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A directory is one form of its PDFs (and template), named after the
    /// directory; a PDF on its own is a form named after the file.
    #[test]
    fn a_form_is_a_pdf_or_a_directory_of_its_sources() {
        let dir = scratch("forms");
        let multilingual = dir.join("AAEV_019");
        std::fs::create_dir_all(&multilingual).unwrap();
        std::fs::write(multilingual.join("AAEV_019_DE.pdf"), b"%PDF-1.4 de").unwrap();
        std::fs::write(multilingual.join("AAEV_019_EN.pdf"), b"%PDF-1.4 en").unwrap();
        std::fs::write(multilingual.join("notes.txt"), b"ignored").unwrap();
        let single = dir.join("AAOS_033_IT.pdf");
        std::fs::write(&single, b"%PDF-1.4 it").unwrap();

        let forms = read_forms(&[multilingual, single]).unwrap();
        assert_eq!(forms[0].label, "AAEV_019");
        let names: Vec<&str> = forms[0].files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["AAEV_019_DE.pdf", "AAEV_019_EN.pdf"]);
        assert_eq!(forms[1].label, "AAOS_033_IT");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same sources twice would convert one form twice on the shared AEM,
    /// and an empty directory has nothing to convert: both are refused before
    /// anything runs.
    #[test]
    fn the_same_form_twice_and_an_empty_form_are_refused() {
        let dir = scratch("refused");
        let first = dir.join("first.pdf");
        std::fs::write(&first, b"%PDF-1.4 same").unwrap();
        let copy = dir.join("copy");
        std::fs::create_dir_all(&copy).unwrap();
        std::fs::copy(&first, copy.join("first.pdf")).unwrap();
        let err = read_forms(&[first.clone(), copy]).err().unwrap().to_string();
        assert!(err.contains("are the same form"), "{err}");

        let empty = dir.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let err = read_forms(&[empty]).err().unwrap().to_string();
        assert!(err.contains("Nothing to convert"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The flags of a single run do not apply to a batch.
    #[test]
    fn a_batch_takes_no_session() {
        use clap::Parser;
        #[derive(Parser)]
        struct Cli {
            #[command(flatten)]
            batch: BatchArgs,
        }
        assert!(Cli::try_parse_from(["blueprint", "a.pdf", "--session", "x"]).is_err());
        assert!(Cli::try_parse_from(["blueprint", "a.pdf", "--jobs", "9"]).is_err());
        let cli = Cli::try_parse_from(["blueprint", "a.pdf", "b.pdf", "--jobs", "2"]).unwrap();
        assert_eq!((cli.batch.forms.len(), cli.batch.jobs), (2, 2));
    }
}
