//! Starting a run: everything both consumers do around [`pipeline::run`].
//!
//! Building the [`ConversionAgent`] from a file set, opening (or continuing) an
//! edit-history session, resolving the turn provider from the operator settings,
//! and recording the finished envelope back into the history. What is left for a
//! consumer is the [`pipeline::RunObserver`] — how progress is shown and how a
//! retry prompt is answered — and what to do with the artefacts.

use std::time::Instant;

use agent::ConversionAgent;
use agent::u2s::{AemVerifySettings, RedactoVerifySettings};
use agent::OutputTarget;
use pipeline::{AbortFlag, RunEvent, RunOutcome, RunSeed, SharedObserver};

use crate::settings::AppSettings;
use crate::turns::TurnPlan;

/// Reported when the edit-history session cannot be opened. A run without one
/// would build fine and then have nowhere to be reviewed from, so it is a hard
/// stop rather than a warning.
pub const NO_SESSION: &str = "Could not create an edit-history session.";

/// The choices the operator made before starting a run.
pub struct RunOptions {
    pub profile: Option<String>,
    pub target: OutputTarget,
    pub settings: AppSettings,
    /// Set by the caller's stop control to end this run at its next checkpoint.
    pub abort: AbortFlag,
}

/// What a finished — or stopped — run leaves behind.
pub struct Completed {
    /// The edit-history session the run was recorded into. Feed it back to
    /// [`run_feedback`] to refine the result.
    pub session_id: String,
    /// `None` when the run stopped before producing anything: the operator
    /// aborted, or gave up at a retry prompt. The observer already said why.
    pub outcome: Option<RunOutcome>,
    pub elapsed_secs: u64,
}

/// Run the autonomous conversion end to end on a fresh file set.
///
/// The observer is `Send` because a run is driven on a worker thread: several
/// conversions have to make progress at the same time, and the CPU-bound stretches
/// (PDF extraction, package building) would otherwise block every one of them.
///
/// `files` may hold the source PDF(s), an AEM content-package ZIP to use as an
/// editable template, or both.
pub async fn run_fresh(
    files: Vec<(String, Vec<u8>)>,
    opts: &RunOptions,
    session_label: &str,
    obs: &SharedObserver,
) -> Result<Completed, String> {
    // An attached AEM content-package ZIP is decoded into the agent's starting
    // document (the ConversionAgent splits PDFs vs. template internally).
    let has_template = agent::conversion::template_of(&files).is_some();

    // The verification preflight comes first: it is the one step that can
    // refuse the run, and a refused run should leave no session behind.
    let verification = verification_for(opts, obs).await?;

    // Hash on the PDFs when present, otherwise on the template, so the session
    // id is stable for template-only runs.
    let pdfs: Vec<(String, Vec<u8>)> = files
        .iter()
        .filter(|(name, _)| agent::conversion::is_source_pdf(name))
        .cloned()
        .collect();
    let doc_hash = agent::db::document_hash(if pdfs.is_empty() { &files } else { &pdfs });
    agent::db::upsert_document(&doc_hash, session_label);
    // Keep the bytes, not just their hash. Resuming a session replays the
    // sources through the agent, so without them a run recorded here could be
    // reopened and read but never continued — which is most of the point of
    // recording it. Content-addressed, so converting the same document again
    // stores nothing new.
    agent::db::store_sources(&doc_hash, &files);
    let session_id =
        match agent::db::create_session(
            &doc_hash,
            opts.profile.as_deref(),
            opts.target.as_str(),
            session_label,
        ) {
            Some(id) => id,
            None => return Err(NO_SESSION.to_string()),
        };
    let agent = match ConversionAgent::new(opts.profile.clone(), files, session_id.clone(), opts.target)
        .and_then(|agent| verification.attach(agent))
    {
        Ok(agent) => agent,
        Err(e) => {
            agent::db::delete_session(&session_id);
            return Err(e);
        }
    };

    // An uploaded content package is an AEM artefact; it is not pre-loaded for
    // any other target, so don't tell the Author it was.
    let template_note = if has_template && opts.target == OutputTarget::Aem {
        "\n\nThe document's form was decoded from an uploaded content package. Inspect it with \
json_outline and modify it to match the source instead of authoring from scratch. Its `languages` \
also lists the template's own: remove those the source does not have, with their texts."
    } else {
        ""
    };

    Ok(drive(agent, opts, RunSeed::Fresh, template_note, session_id, session_label, obs).await)
}

/// Reported when a continuation names a session that holds no document. There
/// is nothing to carry on from, and the Author would be told to finish a tree it
/// was never given — so it is a hard stop rather than a run that bills a full
/// stage to produce nothing.
pub fn no_prior_state(session_id: &str) -> String {
    format!(
        "Session {session_id} holds no saved form, so there is nothing to continue. \
         Start a fresh conversion from the sources instead."
    )
}

/// Carry an existing session on: restore what the last run built into a fresh
/// agent and drive the controller over it.
///
/// One entry point for both ways in — feedback to apply, or nothing at all — so
/// the restore-plus-preflight-plus-lease sequence exists once. Which of the two
/// it is is the seed's business, and [`RunSeed::resuming`] is what converts the
/// operator's typing into it.
///
/// The session already holds an authored tree, which the Author picks up.
pub async fn resume(
    seed: RunSeed,
    pdfs: Vec<(String, Vec<u8>)>,
    opts: &RunOptions,
    session_id: String,
    obs: &SharedObserver,
) -> Result<Completed, String> {
    // Seed the agent from the continuing session so the run applies to the
    // document the last run authored, and the Author refines it instead of
    // starting over. A session recorded before runs authored one document
    // cannot be resumed, and says so.
    let prior = agent::session::restore(&session_id, opts.target)?;

    // A continuation with nothing restored has no brief at all: the seeded
    // document *is* the instruction, and the Author would be told to finish work
    // it was never handed. Feedback still carries one, so only this case is fatal.
    if matches!(prior, agent::session::Restored::Nothing) && matches!(seed, RunSeed::Continue) {
        return Err(no_prior_state(&session_id));
    }

    let verification = verification_for(opts, obs).await?;

    // Named from the sources before they move into the agent.
    let label = if pdfs.is_empty() {
        "resumed session".to_string()
    } else {
        pdfs.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>().join(", ")
    };
    let mut agent = verification.attach(ConversionAgent::new(
        opts.profile.clone(),
        pdfs,
        session_id.clone(),
        opts.target,
    )?)?;
    if let agent::session::Restored::Document(document) = prior {
        agent.seed_document(document)?;
    }

    Ok(drive(agent, opts, seed, "", session_id, &label, obs).await)
}

/// The verifier a run's target needs, checked and ready to attach.
enum Verification {
    Aem(AemVerifySettings),
    Redacto(RedactoVerifySettings),
}

impl Verification {
    fn attach(self, agent: ConversionAgent) -> Result<ConversionAgent, String> {
        match self {
            Verification::Aem(settings) => agent.with_aem_verify(&settings),
            Verification::Redacto(settings) => agent.with_redacto_verify(&settings),
        }
    }
}

/// Check that the target's rules and verifier can run: the rule sandbox,
/// Docker, the images, the AEM data volume and pdfium. There is no way to run
/// without them, so a target that is not ready refuses the run, before it
/// spends a token.
async fn verification_for(opts: &RunOptions, obs: &SharedObserver) -> Result<Verification, String> {
    obs.emit(RunEvent::Thought("Checking the verification setup…".into()));
    let settings = &opts.settings;
    let report = agent::u2s::readiness(opts.target, &settings.aem_verify, &settings.redacto_verify)
        .await
        .map_err(|e| {
            format!(
                "Verification is not possible, so the run cannot start:\n{e}\n\
                 See docker/aem/README.md for the setup."
            )
        })?;
    obs.emit(RunEvent::Thought(format!("Verification ready. {report}")));
    Ok(match opts.target {
        OutputTarget::Aem => Verification::Aem(settings.aem_verify.clone()),
        OutputTarget::Redacto => Verification::Redacto(settings.redacto_verify.clone()),
    })
}

/// Drive the controller over `agent` and record what it produced.
async fn drive(
    agent: ConversionAgent,
    opts: &RunOptions,
    seed: RunSeed,
    template_note: &'static str,
    session_id: String,
    // What the run converts, for naming its analysis folder.
    label: &str,
    obs: &SharedObserver,
) -> Completed {
    let started_at = Instant::now();

    // An endpoint that cannot produce a model ends the run here, with the
    // message that says what to configure — not on the first turn, after a
    // session has been opened and the browser started.
    let plan = TurnPlan::for_settings(&opts.settings);
    let resolved = match plan.resolve() {
        Ok(resolved) => resolved,
        Err(e) => {
            obs.emit(RunEvent::Warning(e));
            return Completed {
                session_id,
                outcome: None,
                elapsed_secs: started_at.elapsed().as_secs(),
            };
        }
    };

    let run_config = pipeline::RunConfig {
        profile: opts.profile.clone(),
        target: opts.target,
        abort: opts.abort.clone(),
        max_review_rounds: opts.settings.max_review_rounds,
        extra_instructions: crate::settings::extra_instructions_block(
            &opts.settings.agent_instructions,
        ),
        template_note,
        model: resolved.model,
        price: resolved.price,
        max_tokens: resolved.max_tokens,
        context_budget: resolved.context_budget,
        capture_review: true,
        final_rule_check: true,
        finish_nudge: true,
    };

    // A session's review images show its last finished run. This run will
    // change what was built, so the previous run's images go now: a run that
    // ends without capturing any, or never ends, must not leave an older build
    // on show as this one.
    if agent::db::store_review(&session_id, &agent::review::ReviewImages::default()).is_none() {
        obs.emit(RunEvent::Warning(
            "The previous review images could not be cleared, so the review may show an older build.".into(),
        ));
    }

    // Recording starts once the run is certain to start, so a run refused
    // for its settings leaves no empty folder behind.
    let analysis = crate::analysis::root_dir(&opts.settings).and_then(|root| {
        let meta = run_meta(opts, &seed, &session_id, label, plan.describe());
        crate::analysis::RunAnalysis::start(&root, meta, obs)
    });
    let run_obs = match &analysis {
        Some(analysis) => analysis.observer(obs.clone()),
        None => obs.clone(),
    };

    let shared_agent: pipeline::SharedAgent = std::sync::Arc::new(tokio::sync::Mutex::new(agent));
    let outcome = pipeline::run(shared_agent, run_config, seed, run_obs).await;

    if let Some(analysis) = analysis {
        let end = crate::analysis::RunEnd {
            produced: outcome.is_some(),
            form_code: outcome.as_ref().and_then(|o| o.form_code.clone()),
            outputs: outcome.as_ref().map(produced_outputs).unwrap_or_default(),
            warnings: outcome.as_ref().map(|o| o.warnings.clone()).unwrap_or_default(),
        };
        analysis.finish(end, obs);
    }

    // Record the final document in the history, so the run can be reopened
    // from the session browser. The agent records every edit already; this is
    // the result as the run ended, under a label the browser shows.
    //
    // A failure here costs the operator the whole result the moment the window
    // closes, so it is reported rather than swallowed: the store already prints
    // the cause, and this is what puts it in front of whoever ran the
    // conversion.
    if let Some(outcome) = &outcome {
        let json = outcome.document.to_string();
        let recorded = agent::db::insert_edit(
            &agent::session::document_session(&session_id),
            "Agent conversion",
            &json,
        );
        if recorded.is_none() {
            obs.emit(RunEvent::Warning(format!(
                "The result could not be recorded in the edit history, so session \
                 {session_id} cannot be reopened. Download the outputs before closing."
            )));
        }
        if let Some(review) = &outcome.review
            && agent::db::store_review(&session_id, review).is_none()
        {
            obs.emit(RunEvent::Warning(
                "The review images could not be recorded, so the result cannot be reviewed.".into(),
            ));
        }
    }

    Completed {
        session_id,
        outcome,
        elapsed_secs: started_at.elapsed().as_secs(),
    }
}

/// What the analysis folder records about the run at its start.
fn run_meta(
    opts: &RunOptions,
    seed: &RunSeed,
    session_id: &str,
    label: &str,
    model: String,
) -> crate::analysis::RunMeta {
    crate::analysis::RunMeta {
        label: label.to_string(),
        session_id: session_id.to_string(),
        kind: match seed {
            RunSeed::Fresh => "fresh conversion".into(),
            RunSeed::Feedback(text) => format!(
                "feedback round: {}",
                crate::analysis::format::excerpt(&crate::analysis::format::one_line(text), 300)
            ),
            RunSeed::Continue => "continuation".into(),
        },
        started: chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
        profile: opts.profile.clone().unwrap_or_else(|| "(none)".into()),
        target: opts.target.as_str().to_string(),
        model,
        max_review_rounds: opts.settings.max_review_rounds,
        verification: match opts.target {
            OutputTarget::Aem => {
                let v = &opts.settings.aem_verify;
                format!("AEM verifier (image {}, volume {})", v.image, v.data_volume)
            }
            OutputTarget::Redacto => "Redacto verifier".to_string(),
        },
        engine_version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

/// The artefacts a finished run produced, named for the analysis report.
fn produced_outputs(outcome: &pipeline::RunOutcome) -> Vec<String> {
    let mut outputs = Vec::new();
    if outcome.aem_package.is_some() {
        outputs.push("AEM package".to_string());
    }
    if outcome.aem_package_bound.is_some() {
        outputs.push("AEM package with bindRefs".to_string());
    }
    if outcome.xsd_schema.is_some() {
        outputs.push("XSD schema".to_string());
    }
    if outcome.redacto_sql.is_some() {
        outputs.push("Redacto SQL".to_string());
    }
    outputs
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipeline::{NullObserver, SharedObserver};

    /// An AEM run whose verifier is not set up must not start: no session is
    /// opened, no token is spent, and the error says what to fix. Missing
    /// settings are refused before Docker is even asked.
    #[tokio::test]
    async fn an_aem_run_without_its_verifier_refuses_to_start() {
        let opts = RunOptions {
            profile: None,
            target: OutputTarget::Aem,
            settings: AppSettings::default(),
            abort: AbortFlag::default(),
        };
        assert!(opts.settings.aem_verify.image.is_empty(), "no image by default");
        let err = run_fresh(Vec::new(), &opts, "preflight-test", &SharedObserver::new(NullObserver))
            .await
            .err()
            .expect("the run must be refused");
        assert!(err.contains("Verification is not possible"), "{err}");
        assert!(err.contains("the AEM image is not set"), "{err}");
        assert!(err.contains("docker/aem/README.md"), "{err}");
    }

    /// The resume path runs the same preflight before restoring anything, and
    /// a Redacto run is held to its own verifier the same way.
    #[tokio::test]
    async fn a_feedback_run_is_refused_the_same_way() {
        let opts = RunOptions {
            profile: None,
            target: OutputTarget::Redacto,
            settings: AppSettings {
                redacto_verify: agent::u2s::RedactoVerifySettings {
                    core_image: String::new(),
                    ..Default::default()
                },
                ..AppSettings::default()
            },
            abort: AbortFlag::default(),
        };
        let err = resume(
            RunSeed::resuming("make it better"),
            Vec::new(),
            &opts,
            "no-such-session".into(),
            &SharedObserver::new(NullObserver),
        )
        .await
        .err()
        .expect("the run must be refused");
        assert!(err.contains("Verification is not possible"), "{err}");
        assert!(err.contains("U2S_REDACTO_VERIFY_UBS_CORE_IMAGE is not set"), "{err}");
    }

    /// A continuation is nothing but the tree it was seeded with, so a session
    /// that holds no form has to be refused before a stage is spent on it.
    ///
    /// Checked before the verification preflight, so it is the reason even on
    /// a machine where verification is not set up.
    #[tokio::test]
    async fn a_continuation_with_nothing_to_continue_is_refused() {
        let opts = RunOptions {
            profile: None,
            target: OutputTarget::Aem,
            settings: AppSettings::default(),
            abort: AbortFlag::default(),
        };

        let err = resume(
            RunSeed::Continue,
            Vec::new(),
            &opts,
            "no-such-session".into(),
            &SharedObserver::new(NullObserver),
        )
        .await
        .err()
        .expect("a continuation with no prior state must be refused");
        assert_eq!(err, no_prior_state("no-such-session"));
    }
}
