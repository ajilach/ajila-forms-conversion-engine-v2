//! The `blueprint` command line: the AI conversion the desktop app runs, headless
//! (Author → rule check → Reviewer, over the `pipeline` controller and the shared
//! `runner` transport, which is why the app and this binary cannot drift apart),
//! the sessions it can resume, and the verifiers it checks its output with.

mod batch;
mod console;
mod convert;

use clap::{Args as ClapArgs, Parser, Subcommand};

/// Blueprint - AI conversion of XFA PDF forms into UBS AEM forms
#[derive(Parser, Debug)]
#[command(name = "blueprint")]
#[command(about = "Convert XFA PDF forms with the AI agent", long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Command,

    /// The container engine the verifiers run on: docker or podman. Defaults
    /// to the desktop app's setting (Docker unless switched).
    #[arg(long, global = true, value_name = "ENGINE", value_parser = agent::container_engine::ContainerEngine::parse)]
    container_engine: Option<agent::container_engine::ContainerEngine>,
}

/// What the command line does.
///
/// The size gap between the variants is clap's business, not a cost: exactly one
/// is built, once, from the process arguments.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand, Debug)]
enum Command {
    /// Convert a form with the AI agent: the desktop app's pipeline, headless.
    Convert(convert::ConvertArgs),

    /// Convert several forms at once, sharing one AEM verifier: each form's
    /// artefacts go to a folder of its own under --out.
    Batch(batch::BatchArgs),

    /// List the conversion sessions a `convert --feedback` run can resume.
    Sessions,

    /// The Docker-hosted AEM Forms instance the Author verifies its output on,
    /// and the Reviewer confirms its decisions on.
    Verify(VerifyArgs),

    /// Render every page of source PDFs as the review shows them (XFA forms
    /// in their initial state), to compare a run's output against.
    RenderSources(RenderSourcesArgs),
}

#[derive(ClapArgs, Debug)]
struct RenderSourcesArgs {
    /// The source PDF(s).
    #[arg(value_name = "PDF", required = true)]
    pdfs: Vec<std::path::PathBuf>,

    /// Directory the pages are written to as `source-NN-<label>.png`
    /// (created if missing).
    #[arg(long, default_value = ".")]
    out: std::path::PathBuf,
}

/// `blueprint render-sources`.
fn render_sources(args: RenderSourcesArgs) -> Result<(), Box<dyn std::error::Error>> {
    let files = args
        .pdfs
        .iter()
        .map(|path| {
            let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
            std::fs::read(path)
                .map(|bytes| (name, bytes))
                .map_err(|e| format!("Could not read {}: {e}", path.display()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let sources = agent::review::render_sources(&files)?;
    std::fs::create_dir_all(&args.out).map_err(|e| format!("Could not create {}: {e}", args.out.display()))?;
    for (index, image) in sources.iter().flat_map(|source| &source.pages).enumerate() {
        let path = args.out.join(runner::review_image_filename("source", index, &image.label));
        std::fs::write(&path, &image.png).map_err(|e| format!("Could not write {}: {e}", path.display()))?;
        println!("Wrote: {}", path.display());
    }
    Ok(())
}

#[derive(ClapArgs, Debug)]
struct VerifyArgs {
    #[command(subcommand)]
    action: VerifyAction,
}

#[derive(Subcommand, Debug)]
enum VerifyAction {
    /// Pull the verifier images that are missing: headless Chromium, and the
    /// AEM image from GitHub with the GitHub CLI's login (`gh auth login -s
    /// read:packages`).
    Prepare,
    /// The preflight a run performs: the settings, Docker, the images (or, for
    /// a missing AEM image, the GitHub CLI login a run pulls it with) and pdfium.
    Check,
}

/// `blueprint verify prepare|check`.
fn verify_command(args: VerifyArgs) -> Result<(), Box<dyn std::error::Error>> {
    let settings = runner::AppSettings::load();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        match args.action {
            VerifyAction::Prepare => {
                let report = agent::u2s::pull_verifier_images(&settings.aem_verify).await?;
                println!("{report}");
            }
            VerifyAction::Check => {
                let report = agent::u2s::readiness(&settings.aem_verify).await?;
                println!("{report}");
                println!("Ready.");
            }
        }
        Ok::<(), Box<dyn std::error::Error>>(())
    })
}

/// Print every recorded conversion session, newest first.
fn list_sessions() {
    let sessions = agent::db::list_all_sessions();
    if sessions.is_empty() {
        println!("No conversion sessions recorded yet.");
        return;
    }
    for s in sessions {
        println!(
            "{}  {}  {:<10}  {:>3} edit(s)  {}",
            agent::db::format_timestamp(&s.created_at),
            s.session_id,
            s.profile.as_deref().unwrap_or("-"),
            s.edit_count,
            s.label,
        );
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // This executable is also the rule worker; see `agent::rules::runner`.
    agent::rules::serve_worker_if_invoked();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let args = Args::parse();
    // Before any runtime starts: Podman means setting DOCKER_HOST, which must
    // happen while this is the only thread.
    let engine = args
        .container_engine
        .unwrap_or_else(|| runner::AppSettings::load().container_engine);
    log::info!("container engine: {}", agent::container_engine::select(engine));

    match args.command {
        Command::Convert(convert_args) => convert::run(convert_args),
        Command::Batch(batch_args) => batch::run(batch_args),
        Command::Sessions => {
            list_sessions();
            Ok(())
        }
        Command::Verify(verify_args) => verify_command(verify_args),
        Command::RenderSources(render_args) => render_sources(render_args),
    }
}
