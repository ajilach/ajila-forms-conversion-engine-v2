//! Starting a run: everything both consumers do around [`pipeline::run`].
//!
//! Building the [`ConversionAgent`] from a file set, opening (or continuing) an
//! edit-history session, resolving the turn provider from the operator settings,
//! and recording the finished envelope back into the history. What is left for a
//! consumer is the [`pipeline::RunObserver`] — how progress is shown and how a
//! retry prompt is answered — and what to do with the artefacts.

use std::time::Instant;

use agent::ConversionAgent;
use agent::browser::BrowserSession;
use blueprint::OutputTarget;
use pipeline::{AbortFlag, RunEvent, RunOutcome, RunSeed, SharedObserver};

use crate::aem_lock::{self, AemLease};
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
    // An attached AEM content-package ZIP is pre-loaded as the agent's editable
    // working tree (the ConversionAgent splits PDFs vs. template internally).
    let has_template = files
        .iter()
        .any(|(_, bytes)| blueprint::detect_aem_zip(bytes));

    // The browser preflight comes first: it is the one step that can refuse
    // the run, and a refused run should leave no session behind.
    let browser = browser_for(opts, obs).await?;

    // Hash on the PDFs when present, otherwise on the template, so the session
    // id is stable for template-only runs.
    let pdfs: Vec<(String, Vec<u8>)> = files
        .iter()
        .filter(|(name, _)| name.to_ascii_lowercase().ends_with(".pdf"))
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
            None => {
                close_browser(browser).await;
                return Err(NO_SESSION.to_string());
            }
        };
    agent::db::insert_edit(&session_id, "Initial (empty)", "[]");

    let mut agent = ConversionAgent::new(
        opts.profile.clone(),
        files,
        opts.settings.aem_connection(),
        session_id.clone(),
        opts.target,
    );
    if let Some(browser) = browser {
        agent = agent.with_browser(browser);
    }

    // An uploaded content package is an AEM artefact; it is not pre-loaded for
    // any other target, so don't tell the Author it was.
    let template_note = if has_template && opts.target == OutputTarget::Aem {
        "\n\nA template AEM tree from an uploaded content package has been pre-loaded as the \
working tree. Inspect it with get_aem_translated_outline and modify it to match the source \
instead of authoring from scratch."
    } else {
        ""
    };

    // Before the first token is spent. A refused run leaves no session behind:
    // an empty session is litter in the history the operator never asked for.
    let lease = match claim_aem_form(&agent, opts) {
        Ok(lease) => lease,
        Err(e) => {
            agent::db::delete_session(&session_id);
            agent.shutdown_browser().await;
            return Err(e);
        }
    };

    Ok(drive(
        agent,
        opts,
        RunSeed::Fresh,
        template_note,
        session_id,
        lease,
        obs,
    )
    .await)
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
/// Always skips the Analyst: the session already holds an authored tree, so
/// there is nothing left to analyse.
pub async fn resume(
    seed: RunSeed,
    pdfs: Vec<(String, Vec<u8>)>,
    opts: &RunOptions,
    session_id: String,
    obs: &SharedObserver,
) -> Result<Completed, String> {
    // Seed the agent from the continuing session so the run applies to the prior
    // result: both the structured content and the AEM tree the last run authored,
    // so the Author refines that tree instead of re-deriving one from the source.
    let prior = agent::session::restore(&session_id, opts.profile.as_deref());

    // A continuation with nothing restored has no brief at all: the seeded tree
    // *is* the instruction, and the Author would be told to finish work it was
    // never handed. Feedback still carries one, so only this case is fatal.
    if prior.is_none() && matches!(seed, RunSeed::Continue) {
        return Err(no_prior_state(&session_id));
    }

    let browser = browser_for(opts, obs).await?;

    let mut agent = ConversionAgent::new(
        opts.profile.clone(),
        pdfs,
        opts.settings.aem_connection(),
        session_id.clone(),
        opts.target,
    );
    if let Some(browser) = browser {
        agent = agent.with_browser(browser);
    }
    if let Some(prior) = prior {
        agent.seed_structured(prior.envelope.content);
        // A no-op for a Redacto run, which has no AEM tree to seed.
        if let Some(tree) = prior.aem_translated {
            agent.seed_aem_translated(tree);
        }
    }

    // A resumed run installs over the same form as the run it continues, so it
    // contends exactly like a fresh one. The session already exists and is the
    // user's own history, so it is left alone.
    let lease = match claim_aem_form(&agent, opts) {
        Ok(lease) => lease,
        Err(e) => {
            agent.shutdown_browser().await;
            return Err(e);
        }
    };

    Ok(drive(agent, opts, seed, "", session_id, lease, obs).await)
}

/// Run the browser preflight when the settings ask for a browser: a started,
/// logged-in session on success, `None` when the browser is off, and the
/// preflight's own error otherwise, which the caller returns before the run
/// spends a token. Only an AEM target has anything to open.
async fn browser_for(
    opts: &RunOptions,
    obs: &SharedObserver,
) -> Result<Option<BrowserSession>, String> {
    if opts.target != OutputTarget::Aem {
        return Ok(None);
    }
    let (Some(cfg), Some(conn)) = (
        opts.settings.browser_config(),
        opts.settings.aem_connection(),
    ) else {
        return Ok(None);
    };
    obs.emit(RunEvent::Thought(
        "Checking the browser for form verification…".into(),
    ));
    let (report, session) = {
        let mut progress = |line: &str| obs.emit(RunEvent::Thought(line.to_string()));
        agent::browser::preflight(&cfg, &conn, &mut progress)
            .await
            .map_err(|e| format!("Browser verification is not possible: {e}"))?
    };
    obs.emit(RunEvent::Thought(format!(
        "Browser verification ready.\n{report}"
    )));
    Ok(Some(session))
}

async fn close_browser(browser: Option<BrowserSession>) {
    if let Some(browser) = browser {
        browser.shutdown().await;
    }
}

/// Drive the controller over `agent` and record what it produced.
/// Claim this run's AEM form so no other run can install over it.
///
/// Only AEM runs against a configured instance can collide; a Redacto run
/// touches no instance and is never blocked. A profile with no AEM config yields
/// no path, and a run that cannot name its target cannot conflict over it.
fn claim_aem_form(agent: &ConversionAgent, opts: &RunOptions) -> Result<Option<AemLease>, String> {
    let Some(connection) = opts.settings.aem_connection() else {
        return Ok(None);
    };
    let Some(path) = agent.planned_jcr_path() else {
        return Ok(None);
    };
    aem_lock::acquire(&connection.host, &path).map(Some)
}

async fn drive(
    agent: ConversionAgent,
    opts: &RunOptions,
    seed: RunSeed,
    template_note: &'static str,
    session_id: String,
    // Held for the whole run and released when it returns, so the next run of
    // the same form starts only once this one has stopped touching it.
    _aem_lease: Option<AemLease>,
    obs: &SharedObserver,
) -> Completed {
    let started_at = Instant::now();

    // An endpoint that cannot produce a model ends the run here, with the
    // message that says what to configure — not on the first turn, after a
    // session has been opened and the browser started.
    let resolved = match TurnPlan::for_settings(&opts.settings).resolve() {
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
        has_aem_connection: opts.settings.aem_connection().is_some(),
        model: resolved.model,
        price: resolved.price,
        max_tokens: resolved.max_tokens,
    };

    let shared_agent: pipeline::SharedAgent = std::sync::Arc::new(tokio::sync::Mutex::new(agent));
    let outcome = pipeline::run(shared_agent, run_config, seed, obs.clone()).await;

    // Record the result in the structured history, so the run can be reopened
    // from the session browser. Without this the session holds nothing but the
    // empty seed and there is nothing to load.
    //
    // A failure here costs the operator the whole result the moment the window
    // closes, so it is reported rather than swallowed: the store already prints
    // the cause, and this is what puts it in front of whoever ran the
    // conversion.
    if let Some(outcome) = &outcome {
        match serde_json::to_string(&outcome.envelope) {
            Ok(json) if agent::db::insert_edit(&session_id, "Agent conversion", &json).is_none() => {
                obs.emit(RunEvent::Warning(format!(
                    "The result could not be recorded in the edit history, so session \
                     {session_id} cannot be reopened. Download the outputs before closing."
                )));
            }
            Err(e) => obs.emit(RunEvent::Warning(format!(
                "The result could not be recorded in the edit history ({e}), so session \
                 {session_id} cannot be reopened. Download the outputs before closing."
            ))),
            _ => {}
        }
    }

    Completed {
        session_id,
        outcome,
        elapsed_secs: started_at.elapsed().as_secs(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipeline::{NullObserver, SharedObserver};

    /// A run that asks for the browser and cannot have it must not start: no
    /// session is opened, no token is spent, and the error says what to fix.
    #[tokio::test]
    async fn a_failed_browser_preflight_refuses_the_run_before_it_starts() {
        let settings = AppSettings {
            aem_host: "http://localhost:4502".into(),
            aem_username: "admin".into(),
            browser_enabled: true,
            browser_npx_path: "/nonexistent/blueprint-test/npx".into(),
            ..AppSettings::default()
        };
        assert!(settings.browser_config().is_some());

        let opts = RunOptions {
            profile: None,
            target: OutputTarget::Aem,
            settings,
            abort: AbortFlag::default(),
        };
        let err = run_fresh(Vec::new(), &opts, "preflight-test", &SharedObserver::new(NullObserver))
            .await
            .err()
            .expect("the run must be refused");
        assert!(
            err.contains("Browser verification is not possible"),
            "{err}"
        );
        assert!(err.contains("/nonexistent/blueprint-test/npx"), "{err}");
        assert!(err.contains("--no-browser"), "{err}");
    }

    /// The resume path runs the same preflight before restoring anything.
    #[tokio::test]
    async fn a_feedback_run_is_refused_the_same_way() {
        let settings = AppSettings {
            aem_host: "http://localhost:4502".into(),
            aem_username: "admin".into(),
            browser_enabled: true,
            browser_npx_path: "/nonexistent/blueprint-test/npx".into(),
            ..AppSettings::default()
        };
        let opts = RunOptions {
            profile: None,
            target: OutputTarget::Aem,
            settings,
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
        assert!(err.contains("/nonexistent/blueprint-test/npx"), "{err}");
        assert!(err.contains("--no-browser"), "{err}");
    }

    /// A continuation is nothing but the tree it was seeded with, so a session
    /// that holds no form has to be refused before a stage is spent on it.
    ///
    /// The browser is off here on purpose: with it on the preflight would refuse
    /// the run first, and this check would never be the reason.
    #[tokio::test]
    async fn a_continuation_with_nothing_to_continue_is_refused() {
        let opts = RunOptions {
            profile: None,
            target: OutputTarget::Aem,
            settings: AppSettings {
                browser_enabled: false,
                ..AppSettings::default()
            },
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

    /// The browser only ever accompanies an AEM upload, and a Redacto run has
    /// nothing to open: neither gets a browser config.
    #[test]
    fn the_browser_needs_an_aem_connection_and_the_switch() {
        let mut settings = AppSettings::default();
        assert!(settings.browser_enabled, "on by default");
        assert!(
            settings.browser_config().is_some(),
            "the default settings carry an AEM host"
        );

        settings.browser_enabled = false;
        assert!(settings.browser_config().is_none());

        settings.browser_enabled = true;
        settings.aem_host = String::new();
        assert!(settings.browser_config().is_none(), "no AEM, no browser");

        settings.aem_host = "http://localhost:4502".into();
        settings.browser_npx_path = "  /opt/homebrew/bin/npx ".into();
        assert_eq!(
            settings.browser_config().unwrap().npx,
            Some(std::path::PathBuf::from("/opt/homebrew/bin/npx"))
        );
    }
}
