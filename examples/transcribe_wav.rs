//! Deterministic local verification for a PCM WAV file.
//!
//! This intentionally stays as an example instead of part of the daemon's
//! public CLI. It verifies the recognizer and model artifact without needing a
//! microphone or a Wayland session:
//!
//! ```text
//! cargo run --example transcribe_wav -- --model-dir /path/to/model sample.wav
//! ```

use std::{
    env,
    path::PathBuf,
    time::{Duration, Instant},
};

use nvstt::audio::{Waveform as Wav, read_wav};
use nvstt::recognizer::StreamingRecognizer;
use nvstt::{
    error::Result,
    recognizer::{ParakeetRecognizer, RecognitionOutcome},
};
use serde::Serialize;

const BENCHMARK_SECONDS: usize = 5;
const BENCHMARK_CHUNK_MS: u64 = 20;
const BENCHMARK_WARMUP_RUNS: usize = 1;
const BENCHMARK_DEFAULT_ITERATIONS: usize = 20;
const FINALIZATION_TARGET: Duration = Duration::from_secs(1);

fn main() -> Result<()> {
    let arguments = parse_args()?;
    let model_dir = arguments.model_dir;
    let wav_path = arguments.wav_path;
    let mut wav = read_wav(&wav_path)?;
    if let Some(max_seconds) = arguments.max_seconds {
        wav.samples.truncate(wav.sample_rate as usize * max_seconds);
    }

    if let Some(iterations) = arguments.benchmark_iterations {
        let benchmark_samples = wav.sample_rate as usize * BENCHMARK_SECONDS;
        if wav.samples.len() < benchmark_samples {
            return Err(nvstt::error::AppError::Config(format!(
                "benchmark requires at least {BENCHMARK_SECONDS} seconds of audio"
            )));
        }
        wav.samples.truncate(benchmark_samples);

        let model_load_started = Instant::now();
        let mut recognizer = ParakeetRecognizer::from_model_dir(model_dir)?;
        let model_load = model_load_started.elapsed();
        return run_benchmark(&mut recognizer, &wav, iterations, model_load);
    }

    let mut recognizer = ParakeetRecognizer::from_model_dir(model_dir)?;
    recognizer.start_session()?;

    for chunk in wav.samples.chunks((wav.sample_rate as usize / 10).max(1)) {
        recognizer.accept_audio(wav.sample_rate, chunk)?;
    }
    match recognizer.finish_session()? {
        RecognitionOutcome::Transcript(transcript) => println!("{transcript}"),
        RecognitionOutcome::NoSpeech => println!(),
    }
    Ok(())
}

struct Arguments {
    model_dir: PathBuf,
    wav_path: PathBuf,
    max_seconds: Option<usize>,
    benchmark_iterations: Option<usize>,
}

fn parse_args() -> Result<Arguments> {
    let mut args = env::args_os().skip(1);
    let mut model_dir = None;
    let mut wav_path = None;
    let mut max_seconds = None;
    let mut benchmark = false;
    let mut benchmark_iterations = None;
    while let Some(arg) = args.next() {
        if arg == "--model-dir" {
            model_dir = args.next().map(PathBuf::from);
        } else if arg == "--seconds" {
            let value = args.next().ok_or_else(|| {
                nvstt::error::AppError::Config("--seconds requires a positive integer".to_owned())
            })?;
            max_seconds = Some(value.to_string_lossy().parse::<usize>().map_err(|_| {
                nvstt::error::AppError::Config("--seconds requires a positive integer".to_owned())
            })?);
            if max_seconds == Some(0) {
                return Err(nvstt::error::AppError::Config(
                    "--seconds requires a positive integer".to_owned(),
                ));
            }
        } else if arg == "--benchmark" {
            benchmark = true;
        } else if arg == "--iterations" {
            let value = args.next().ok_or_else(|| {
                nvstt::error::AppError::Config(
                    "--iterations requires a positive integer".to_owned(),
                )
            })?;
            let iterations = value.to_string_lossy().parse::<usize>().map_err(|_| {
                nvstt::error::AppError::Config(
                    "--iterations requires a positive integer".to_owned(),
                )
            })?;
            if iterations == 0 {
                return Err(nvstt::error::AppError::Config(
                    "--iterations requires a positive integer".to_owned(),
                ));
            }
            benchmark_iterations = Some(iterations);
        } else if wav_path.is_none() {
            wav_path = Some(PathBuf::from(arg));
        } else {
            return Err(nvstt::error::AppError::Config(
                "usage: transcribe_wav --model-dir MODEL_DIR [--seconds N] [--benchmark [--iterations N]] AUDIO.wav".to_owned(),
            ));
        }
    }
    let model_dir = model_dir.ok_or_else(|| {
        nvstt::error::AppError::Config(
            "usage: transcribe_wav --model-dir MODEL_DIR [--seconds N] [--benchmark [--iterations N]] AUDIO.wav".to_owned(),
        )
    })?;
    let wav_path = wav_path.ok_or_else(|| {
        nvstt::error::AppError::Config(
            "usage: transcribe_wav --model-dir MODEL_DIR [--seconds N] [--benchmark [--iterations N]] AUDIO.wav".to_owned(),
        )
    })?;
    if benchmark_iterations.is_some() && !benchmark {
        return Err(nvstt::error::AppError::Config(
            "--iterations requires --benchmark".to_owned(),
        ));
    }
    Ok(Arguments {
        model_dir,
        wav_path,
        max_seconds,
        benchmark_iterations: benchmark
            .then_some(benchmark_iterations.unwrap_or(BENCHMARK_DEFAULT_ITERATIONS)),
    })
}

#[derive(Serialize)]
struct BenchmarkRun {
    stream_processing_ms: u64,
    predicted_pre_stop_backlog_ms: u64,
    model_finalization_ms: u64,
    finalization_latency_ms: u64,
}

#[derive(Serialize)]
struct BenchmarkReport {
    audio_duration_ms: u64,
    chunk_ms: u64,
    warmup_runs: usize,
    measured_runs: usize,
    model_load_ms: u64,
    p50_finalization_ms: u64,
    p95_finalization_ms: u64,
    target_finalization_ms: u64,
    passed: bool,
    runs: Vec<BenchmarkRun>,
}

fn run_benchmark(
    recognizer: &mut ParakeetRecognizer,
    wav: &Wav,
    iterations: usize,
    model_load: Duration,
) -> Result<()> {
    eprintln!("warming the model ({BENCHMARK_WARMUP_RUNS} run)");
    for _ in 0..BENCHMARK_WARMUP_RUNS {
        measure_benchmark_run(recognizer, wav)?;
    }

    let mut runs = Vec::with_capacity(iterations);
    for run in 1..=iterations {
        eprintln!("benchmark run {run}/{iterations}");
        runs.push(measure_benchmark_run(recognizer, wav)?);
    }

    let p50_finalization = percentile_latency(&runs, 50);
    let p95_finalization = percentile_latency(&runs, 95);
    let report = BenchmarkReport {
        audio_duration_ms: duration_to_ms(audio_duration(wav.samples.len(), wav.sample_rate)),
        chunk_ms: BENCHMARK_CHUNK_MS,
        warmup_runs: BENCHMARK_WARMUP_RUNS,
        measured_runs: iterations,
        model_load_ms: duration_to_ms(model_load),
        p50_finalization_ms: duration_to_ms(p50_finalization),
        p95_finalization_ms: duration_to_ms(p95_finalization),
        target_finalization_ms: duration_to_ms(FINALIZATION_TARGET),
        passed: p95_finalization <= FINALIZATION_TARGET,
        runs,
    };
    println!("{}", serde_json::to_string_pretty(&report)?);

    if report.passed {
        Ok(())
    } else {
        Err(nvstt::error::AppError::Unavailable(format!(
            "p95 finalization latency was {} ms; target is {} ms",
            report.p95_finalization_ms, report.target_finalization_ms
        )))
    }
}

fn measure_benchmark_run(recognizer: &mut ParakeetRecognizer, wav: &Wav) -> Result<BenchmarkRun> {
    recognizer.start_session()?;

    let chunk_samples = ((wav.sample_rate as usize * BENCHMARK_CHUNK_MS as usize) / 1_000).max(1);
    let mut samples_before = 0usize;
    let mut stream_processing = Duration::ZERO;
    let mut worker_ready_at = Duration::ZERO;

    for chunk in wav.samples.chunks(chunk_samples) {
        let arrival_at = audio_duration(samples_before, wav.sample_rate);
        let processing_started = Instant::now();
        recognizer.accept_audio(wav.sample_rate, chunk)?;
        let processing_time = processing_started.elapsed();
        stream_processing += processing_time;
        worker_ready_at = worker_ready_at.max(arrival_at) + processing_time;
        samples_before += chunk.len();
    }

    let stop_at = audio_duration(wav.samples.len(), wav.sample_rate);
    let predicted_pre_stop_backlog = worker_ready_at.saturating_sub(stop_at);
    let finalization_started = Instant::now();
    let _ = recognizer.finish_session()?;
    let model_finalization = finalization_started.elapsed();

    Ok(BenchmarkRun {
        stream_processing_ms: duration_to_ms(stream_processing),
        predicted_pre_stop_backlog_ms: duration_to_ms(predicted_pre_stop_backlog),
        model_finalization_ms: duration_to_ms(model_finalization),
        finalization_latency_ms: duration_to_ms(predicted_pre_stop_backlog + model_finalization),
    })
}

fn percentile_latency(runs: &[BenchmarkRun], percentile: u64) -> Duration {
    let mut latencies = runs
        .iter()
        .map(|run| Duration::from_millis(run.finalization_latency_ms))
        .collect::<Vec<_>>();
    latencies.sort_unstable();
    let rank = (latencies.len() as u64 * percentile).div_ceil(100).max(1) as usize;
    latencies[rank - 1]
}

fn audio_duration(samples: usize, sample_rate: i32) -> Duration {
    let nanoseconds = (samples as u128)
        .saturating_mul(1_000_000_000)
        .checked_div(sample_rate.max(1) as u128)
        .unwrap_or_default();
    Duration::from_nanos(nanoseconds.min(u64::MAX as u128) as u64)
}

fn duration_to_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}
