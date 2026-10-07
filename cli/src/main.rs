//! The `blueprint` command line: the AI conversion the desktop app runs, headless
//! (Author → Reviewer, over the `pipeline` controller and the shared
//! `runner` transport, which is why the app and this binary cannot drift apart),
//! the sessions it can resume, and the verifiers it checks its output with.

mod console;
mod convert;

use agent::OutputTarget;
use clap::{Args as ClapArgs, Parser, Subcommand};

/// Blueprint - AI conversion of XFA PDF forms into UBS AEM forms and Redacto documents
#[derive(Parser, Debug)]
#[command(name = "blueprint")]
#[command(about = "Convert XFA PDF forms with the AI agent", long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Command,
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

    /// List the conversion sessions a `convert --feedback` run can resume.
    Sessions,

    /// The Docker-hosted verifiers the Author and Reviewer check their output
    /// with: the AEM Forms instance for an AEM run, Postgres for a Redacto run.
    Verify(VerifyArgs),
}

#[derive(ClapArgs, Debug)]
struct VerifyArgs {
    #[command(subcommand)]
    action: VerifyAction,

    /// Which target's verifier to check: aem or redacto.
    #[arg(long, default_value = "aem", value_parser = convert::parse_target, global = true)]
    target: OutputTarget,
}

#[derive(Subcommand, Debug)]
enum VerifyAction {
    /// Pull the public images the verifiers run (headless Chromium, Postgres).
    /// Needs a network connection once. The AEM image is private: pull it by
    /// hand after `az acr login` (see docker/aem/README.md).
    Prepare,
    /// The preflight a run performs: the settings, Docker, the images, the AEM
    /// data volume and pdfium.
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
                let report =
                    agent::u2s::pull_verifier_images(&settings.aem_verify, &settings.redacto_verify).await?;
                println!("{report}");
            }
            VerifyAction::Check => {
                let report = match args.target {
                    OutputTarget::Aem => agent::u2s::aem_verify_readiness(&settings.aem_verify).await?,
                    OutputTarget::Redacto => {
                        agent::u2s::redacto_verify_readiness(&settings.redacto_verify).await?
                    }
                };
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

    match Args::parse().command {
        Command::Convert(convert_args) => convert::run(convert_args),
        Command::Sessions => {
            list_sessions();
            Ok(())
        }
        Command::Verify(verify_args) => verify_command(verify_args),
    }
}
