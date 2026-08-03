use clap::{Parser, Subcommand};
use nvstt::{
    app::run_daemon,
    config::Config,
    error::{AppError, Result},
    evaluation::{EvaluationReport, evaluate_model},
    installer::{DownloadProgress, install_model},
    ipc::{IpcRequest, IpcResponse, send_request},
    model::ModelStatus,
    paths::default_paths,
};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "nvstt", version, about = "Local Linux voice dictation")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start listening or finalize and deliver the current dictation.
    Toggle,
    /// Cancel an active dictation.
    Cancel,
    /// Show daemon state.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Show the newest successful transcripts.
    History {
        #[arg(long, default_value_t = 10)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Run the long-lived daemon.
    Daemon,
    /// Inspect the locally installed recognition model.
    Model {
        #[command(subcommand)]
        command: ModelCommand,
    },
}

#[derive(Debug, Subcommand)]
enum ModelCommand {
    /// Download and install the configured model.
    Install {
        /// Select a supported model without changing the user configuration.
        #[arg(long)]
        model: Option<String>,
        /// Select a model streaming profile without changing the user configuration.
        #[arg(long)]
        streaming_profile: Option<String>,
    },
    /// Show model files and readiness.
    Status {
        #[arg(long)]
        json: bool,
        /// Select a supported model without changing the user configuration.
        #[arg(long)]
        model: Option<String>,
        /// Select a model streaming profile without changing the user configuration.
        #[arg(long)]
        streaming_profile: Option<String>,
    },
    /// Print the expected model directory.
    Path {
        /// Select a supported model without changing the user configuration.
        #[arg(long)]
        model: Option<String>,
        /// Select a model streaming profile without changing the user configuration.
        #[arg(long)]
        streaming_profile: Option<String>,
    },
    /// Evaluate an installed model against a private JSONL WAV manifest.
    Evaluate {
        /// Path to a JSONL manifest. Relative audio paths use its directory.
        #[arg(long)]
        manifest: PathBuf,
        /// Select a supported model without changing the user configuration.
        #[arg(long)]
        model: Option<String>,
        /// Select a model streaming profile without changing the user configuration.
        #[arg(long)]
        streaming_profile: Option<String>,
        /// Override the selected configuration's speech gate for this evaluation only.
        #[arg(long)]
        speech_gate: Option<bool>,
        /// Print the complete machine-readable report.
        #[arg(long)]
        json: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nvstt=info".into()),
        )
        .try_init()
        .ok();

    let cli = Cli::parse();
    let paths = default_paths()?;
    let config = Config::load(&paths.config_path)?;

    match cli.command {
        Command::Daemon => run_daemon(paths, config).await,
        Command::Toggle => run_request(&paths, IpcRequest::Toggle, false).await,
        Command::Cancel => run_request(&paths, IpcRequest::Cancel, false).await,
        Command::Status { json } => run_request(&paths, IpcRequest::Status, json).await,
        Command::History { limit, json } => {
            run_request(&paths, IpcRequest::History { limit }, json).await
        }
        Command::Model { command } => run_model_command(&paths, &config, command),
    }
}

fn run_model_command(
    paths: &nvstt::paths::AppPaths,
    config: &Config,
    command: ModelCommand,
) -> Result<()> {
    match command {
        ModelCommand::Install {
            model,
            streaming_profile,
        } => {
            let selected = config.with_model_overrides(model, streaming_profile)?;
            let report = install_model(&selected, paths, |progress: DownloadProgress| {
                if let Some(total) = progress.total_bytes {
                    eprintln!(
                        "downloading model: {:.1} / {:.1} MiB",
                        progress.downloaded_bytes as f64 / (1024.0 * 1024.0),
                        total as f64 / (1024.0 * 1024.0)
                    );
                } else {
                    eprintln!(
                        "downloading model: {:.1} MiB",
                        progress.downloaded_bytes as f64 / (1024.0 * 1024.0)
                    );
                }
            })?;
            println!("{}", report.message());
            Ok(())
        }
        ModelCommand::Path {
            model,
            streaming_profile,
        } => {
            let selected = config.with_model_overrides(model, streaming_profile)?;
            let status = ModelStatus::inspect(&selected, paths);
            println!("{}", status.path.display());
            Ok(())
        }
        ModelCommand::Status {
            json,
            model,
            streaming_profile,
        } => {
            let selected = config.with_model_overrides(model, streaming_profile)?;
            let status = ModelStatus::inspect(&selected, paths);
            if json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                println!("model: {}", status.model);
                println!("streaming profile: {}", status.streaming_profile);
                println!("artifact: {}", status.artifact);
                println!("path: {}", status.path.display());
                println!("ready: {}", status.ready);
                for file in &status.files {
                    println!(
                        "{}: {} ({})",
                        file.name,
                        if file.present { "present" } else { "missing" },
                        file.path.display()
                    );
                }
                println!("speech gate enabled: {}", status.speech_gate.enabled);
                println!("speech gate path: {}", status.speech_gate.path.display());
                println!("speech gate ready: {}", status.speech_gate.ready);
                println!("message: {}", status.message());
            }
            if status.ready {
                Ok(())
            } else {
                Err(AppError::Unavailable(status.message()))
            }
        }
        ModelCommand::Evaluate {
            manifest,
            model,
            streaming_profile,
            speech_gate,
            json,
        } => {
            let mut selected = config.with_model_overrides(model, streaming_profile)?;
            if let Some(speech_gate) = speech_gate {
                selected.speech_gate = speech_gate;
            }
            let report = evaluate_model(&selected, paths, &manifest)?;
            print_evaluation_report(&report, json)?;
            Ok(())
        }
    }
}

fn print_evaluation_report(report: &EvaluationReport, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }

    println!("model: {}", report.model);
    println!("streaming profile: {}", report.streaming_profile);
    println!("provider: {}", report.execution_provider);
    println!("speech gate: {}", report.speech_gate_enabled);
    println!(
        "word errors: {} (S={} D={} I={}, WER={:.3})",
        report.total_word_errors,
        report.total_substitutions,
        report.total_deletions,
        report.total_insertions,
        report.word_error_rate,
    );
    println!("silent/noise failures: {}", report.silent_clip_failures);
    println!(
        "finalization latency: p50={} ms p95={} ms",
        report.p50_finalization_latency_ms, report.p95_finalization_latency_ms
    );
    for clip in &report.clips {
        println!(
            "{} [{}]: WER={:.3} S={} D={} I={} silent_failure={} backlog={} ms model_flush={} ms finalization={} ms",
            clip.id,
            clip.category,
            clip.word_error_rate,
            clip.substitutions,
            clip.deletions,
            clip.insertions,
            clip.silent_clip_failure,
            clip.pre_stop_backlog_ms,
            clip.model_finalization_ms,
            clip.finalization_latency_ms,
        );
        println!("  reference: {}", clip.reference);
        println!("  hypothesis: {}", clip.hypothesis);
    }
    Ok(())
}

async fn run_request(
    paths: &nvstt::paths::AppPaths,
    request: IpcRequest,
    json: bool,
) -> Result<()> {
    let response = send_request(&paths.socket_path, &request).await?;
    print_response(&response, json)?;
    if response.is_ok() {
        Ok(())
    } else {
        Err(AppError::Ipc(response_message(&response)))
    }
}

fn print_response(response: &IpcResponse, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(response)?);
        return Ok(());
    }

    match response {
        IpcResponse::Command { result } => {
            println!("{}", result.message);
            println!("state: {:?}", result.status.state);
            println!("transcription: {:?}", result.transcription);
            println!("delivery: {:?}", result.delivery);
            if let Some(transcript) = &result.transcript {
                println!("transcript: {transcript}");
            }
        }
        IpcResponse::Status { snapshot } => {
            println!("state: {:?}", snapshot.state);
            println!("model: {}", snapshot.model);
            println!("model ready: {}", snapshot.model_ready);
            if let Some(path) = &snapshot.model_path {
                println!("model path: {path}");
            }
            println!("message: {}", snapshot.message);
            if let Some(session_id) = &snapshot.session_id {
                println!("session: {session_id}");
            }
        }
        IpcResponse::History { records } => {
            if records.is_empty() {
                println!("history is empty");
            } else {
                for record in records {
                    println!(
                        "{} | {} | transcription={:?} delivery={:?} | {}",
                        record.created_at_ms,
                        record.model,
                        record.transcription_status,
                        record.delivery_status,
                        record.transcript.replace('\n', " ")
                    );
                }
            }
        }
        IpcResponse::Error { code, message } => {
            eprintln!("{code}: {message}");
        }
    }
    Ok(())
}

fn response_message(response: &IpcResponse) -> String {
    match response {
        IpcResponse::Command { result } => result.message.clone(),
        IpcResponse::Error { message, .. } => message.clone(),
        IpcResponse::Status { .. } | IpcResponse::History { .. } => "request failed".to_owned(),
    }
}
