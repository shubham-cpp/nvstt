use clap::{Parser, Subcommand};
use nvstt::{
    app::run_daemon_with_tray,
    config::Config,
    domain::{AttemptOutcome, InputPreference},
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
    /// Manage personal corrections. Changes apply from the next dictation.
    Vocabulary {
        #[command(subcommand)]
        command: VocabularyCommand,
    },
    /// Recover the newest complete saved recording to stdout, without typing.
    Recover {
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        streaming_profile: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Recover a saved WAV to stdout. Does not type or change history.
    Transcribe {
        audio: PathBuf,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        streaming_profile: Option<String>,
        #[arg(long)]
        json: bool,
    },
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
    Daemon {
        /// Disable the tray and keep desktop phase notifications.
        #[arg(long)]
        no_tray: bool,
    },
    /// List or select a runtime microphone.
    Audio {
        #[command(subcommand)]
        command: AudioCommand,
    },
    /// Clear the last attempt error.
    ClearError,
    /// Inspect the locally installed recognition model.
    Model {
        #[command(subcommand)]
        command: ModelCommand,
    },
}

#[derive(Debug, Subcommand)]
enum VocabularyCommand {
    /// Teach an exact phrase correction, such as "para keet" -> "Parakeet".
    Add { heard: String, written: String },
    /// Forget a personal correction.
    Remove { heard: String },
    /// Show personal corrections.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Preview cleanup, number formatting, and corrections without typing.
    Test { text: String },
}

#[derive(Debug, Subcommand)]
enum AudioCommand {
    /// Refresh the input list. Use IDs for selection.
    Inputs {
        #[arg(long)]
        json: bool,
    },
    /// Select an input for the next recording. Selection is not saved.
    Select {
        #[arg(
            long,
            required_unless_present = "system_default",
            conflicts_with = "system_default"
        )]
        id: Option<String>,
        #[arg(long)]
        system_default: bool,
        #[arg(long)]
        json: bool,
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
        /// Override noise suppression for this evaluation.
        #[arg(long)]
        denoise: Option<bool>,
        /// ONNX CPU threads for this evaluation, from 1 to 32.
        #[arg(long)]
        threads: Option<i32>,
        /// Override spoken-number formatting for this evaluation.
        #[arg(long)]
        itn: Option<bool>,
        /// Fail when a phrase, boundary, or silent-input expectation fails.
        #[arg(long)]
        check: bool,
        /// Maximum WER. Requires references for every clip and implies --check.
        #[arg(long)]
        max_word_error_rate: Option<f64>,
        /// Maximum simulated p95 stop-to-result latency; implies --check.
        #[arg(long)]
        max_finalization_ms: Option<u64>,
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
        Command::Vocabulary { command } => run_vocabulary_command(&paths, &config, command),
        Command::Recover {
            model,
            streaming_profile,
            json,
        } => {
            let config = with_vocabulary(
                config.with_model_overrides(model, streaming_profile)?,
                &paths,
            )?;
            let store = nvstt::recordings::RecordingStore::new(paths.state_dir.join("recordings"));
            let (metadata, audio) = store.latest()?;
            let result = nvstt::evaluation::transcribe_file(&config, &paths, &audio)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "recording": metadata, "transcription": result,
                    }))?
                );
            } else {
                eprintln!(
                    "Recovered session {} using {} / {}",
                    metadata.session_id, result.model, result.streaming_profile
                );
                let capture = &metadata.capture;
                if capture.dropped_samples != 0
                    || capture.backend_failed
                    || capture.duration_exceeded
                    || capture.stop_failed
                    || capture.drain_failed
                {
                    eprintln!("Saved audio is incomplete; recovered text may omit words.");
                }
                println!("{}", result.text);
            }
            Ok(())
        }
        Command::Transcribe {
            audio,
            model,
            streaming_profile,
            json,
        } => {
            let config = with_vocabulary(
                config.with_model_overrides(model, streaming_profile)?,
                &paths,
            )?;
            let result = nvstt::evaluation::transcribe_file(&config, &paths, &audio)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("{}", result.text);
            }
            Ok(())
        }
        Command::Daemon { no_tray } => run_daemon_with_tray(paths, config, !no_tray).await,
        Command::Audio { command } => match command {
            AudioCommand::Inputs { json } => {
                run_request(&paths, IpcRequest::AudioInputs, json).await
            }
            AudioCommand::Select {
                id,
                system_default: _,
                json,
            } => {
                let preference = id.map_or(InputPreference::SystemDefault, |id| {
                    InputPreference::Device { id }
                });
                run_request(&paths, IpcRequest::SelectInput { preference }, json).await
            }
        },
        Command::ClearError => run_request(&paths, IpcRequest::ClearError, false).await,
        Command::Toggle => run_request(&paths, IpcRequest::Toggle, false).await,
        Command::Cancel => run_request(&paths, IpcRequest::Cancel, false).await,
        Command::Status { json } => run_request(&paths, IpcRequest::Status, json).await,
        Command::History { limit, json } => {
            run_request(&paths, IpcRequest::History { limit }, json).await
        }
        Command::Model { command } => run_model_command(&paths, &config, command),
    }
}

fn with_vocabulary(mut config: Config, paths: &nvstt::paths::AppPaths) -> Result<Config> {
    config.replacements = nvstt::vocabulary::Vocabulary::load(&paths.vocabulary_path())?
        .apply_to(&config.replacements);
    Ok(config)
}

fn run_vocabulary_command(
    paths: &nvstt::paths::AppPaths,
    config: &Config,
    command: VocabularyCommand,
) -> Result<()> {
    use nvstt::{
        dictation_transcript::{DictationTranscript, dictation_transcript},
        recognizer::RecognitionOutcome,
        vocabulary::Vocabulary,
    };
    let path = paths.vocabulary_path();
    match command {
        VocabularyCommand::Add { heard, written } => {
            Vocabulary::add(&path, &heard, &written)?;
            println!("Saved. The correction applies from your next dictation.");
        }
        VocabularyCommand::Remove { heard } => {
            Vocabulary::remove(&path, &heard)?;
            println!("Removed. The change applies from your next dictation.");
        }
        VocabularyCommand::List { json } => {
            let vocabulary = Vocabulary::load(&path)?;
            if json {
                println!("{}", serde_json::to_string_pretty(vocabulary.entries())?);
            } else if vocabulary.entries().is_empty() {
                println!("No personal corrections.");
            } else {
                for (heard, written) in vocabulary.entries() {
                    println!("{heard} -> {written}");
                }
            }
        }
        VocabularyCommand::Test { text } => {
            let config = with_vocabulary(config.clone(), paths)?;
            let result = dictation_transcript(
                RecognitionOutcome::Transcript(text),
                &config.replacements,
                config.itn,
            )
            .map_err(|_| nvstt::error::AppError::Config("preview text is empty".into()))?;
            match result {
                DictationTranscript::Ready(text) => println!("{text}"),
                DictationTranscript::NoContent => println!(),
            }
        }
    }
    Ok(())
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
            denoise,
            threads,
            itn,
            check,
            max_word_error_rate,
            max_finalization_ms,
            json,
        } => {
            let mut selected = config.with_model_overrides(model, streaming_profile)?;
            if let Some(speech_gate) = speech_gate {
                selected.speech_gate = speech_gate;
            }
            if let Some(denoise) = denoise {
                selected.denoise = denoise;
            }
            if let Some(threads) = threads {
                selected.num_threads = threads;
            }
            if let Some(itn) = itn {
                selected.itn = itn;
            }
            let selected = with_vocabulary(selected, paths)?;
            let report = evaluate_model(&selected, paths, &manifest)?;
            print_evaluation_report(&report, json)?;
            if check || max_word_error_rate.is_some() || max_finalization_ms.is_some() {
                report.check(max_word_error_rate, max_finalization_ms)?;
            }
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
        "word errors: {} (S={} D={} I={}, WER={})",
        report.total_word_errors,
        report.total_substitutions,
        report.total_deletions,
        report.total_insertions,
        report
            .word_error_rate
            .map_or_else(|| "unscored".into(), |v| format!("{v:.3}")),
    );
    println!("silent/noise failures: {}", report.silent_clip_failures);
    println!(
        "finalization latency: p50={} ms p95={} ms",
        report.p50_finalization_latency_ms, report.p95_finalization_latency_ms
    );
    for clip in &report.clips {
        println!(
            "{} [{}]: WER={} S={} D={} I={} silent_failure={} backlog={} ms model_flush={} ms finalization={} ms",
            clip.id,
            clip.category,
            clip.word_error_rate
                .map_or_else(|| "unscored".into(), |v| format!("{v:.3}")),
            clip.substitutions,
            clip.deletions,
            clip.insertions,
            clip.silent_clip_failure,
            clip.pre_stop_backlog_ms,
            clip.model_finalization_ms,
            clip.finalization_latency_ms,
        );
        println!(
            "  reference: {}",
            clip.reference.as_deref().unwrap_or("unverified")
        );
        println!("  raw: {}", clip.raw_hypothesis);
        println!("  expectation failures: {:?}", clip.expectation_failures);
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
            println!("input: {}", snapshot.audio.label());
            println!("input available: {}", snapshot.audio.available);
            if let Some(outcome) = &snapshot.feedback.outcome {
                println!("last result: {}", outcome.label());
                if let AttemptOutcome::Failed { reason, .. } = outcome {
                    println!("failure: {reason}");
                }
            }
            if let Some(warning) = &snapshot.feedback.warning {
                println!("warning: {warning}");
            }
            if let Some(session_id) = &snapshot.session_id {
                println!("session: {session_id}");
            }
        }
        IpcResponse::AudioInputs { audio } => {
            println!("selected: {}", audio.label());
            for input in &audio.inputs {
                println!("{} | {:?} | {}", input.id, input.kind, input.name);
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
        IpcResponse::Status { .. }
        | IpcResponse::History { .. }
        | IpcResponse::AudioInputs { .. } => "request failed".to_owned(),
    }
}
