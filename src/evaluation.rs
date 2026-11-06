//! Private-corpus evaluation for local ASR model comparisons.
//!
//! The manifest contains paths and text only. Audio stays outside the Git
//! worktree and is resolved relative to the manifest file.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use crate::{
    audio::read_wav,
    audio_pipeline::{AudioPipeline, MODEL_SAMPLE_RATE},
    config::Config,
    dictation_transcript::{DictationTranscript, dictation_transcript},
    error::{AppError, Result},
    model::ModelStatus,
    paths::AppPaths,
    recognizer::{RecognitionOutcome, StreamingRecognizer, create_recognizer, execution_provider},
};

const FEED_CHUNK_MILLISECONDS: usize = 20;

#[derive(Debug, Serialize)]
pub struct FileTranscription {
    pub audio: PathBuf,
    pub raw_text: String,
    pub text: String,
    pub model: String,
    pub streaming_profile: String,
    pub finalization_latency_ms: u64,
}

/// Recover text from a saved WAV without inserting anything into the desktop.
pub fn transcribe_file(
    config: &Config,
    paths: &AppPaths,
    audio: &Path,
) -> Result<FileTranscription> {
    config.validate()?;
    let status = ModelStatus::inspect(config, paths);
    if !status.ready {
        return Err(AppError::Unavailable(status.message()));
    }
    let wave = read_wav(audio)?;
    let mut recognizer = create_recognizer(config, &status.path)?;
    let result = transcribe_waveform(
        recognizer.as_mut(),
        wave.sample_rate,
        &wave.samples,
        config.denoise,
    )?;
    Ok(FileTranscription {
        audio: audio.to_owned(),
        text: processed_text(&result.text, config)?,
        raw_text: result.text,
        model: config.model.clone(),
        streaming_profile: config.streaming_profile.clone(),
        finalization_latency_ms: result.finalization_latency_ms,
    })
}

fn processed_text(raw: &str, config: &Config) -> Result<String> {
    if raw.trim().is_empty() {
        return Ok(String::new());
    }
    match dictation_transcript(
        RecognitionOutcome::Transcript(raw.to_owned()),
        &config.replacements,
        config.itn,
    ) {
        Ok(DictationTranscript::Ready(text)) => Ok(text),
        Ok(DictationTranscript::NoContent) => Ok(String::new()),
        Err(_) => Err(AppError::Unavailable("text processing failed".into())),
    }
}

#[derive(Clone, Debug, Deserialize)]
struct ManifestEntry {
    id: String,
    audio: PathBuf,
    #[serde(default)]
    reference: Option<String>,
    #[serde(default)]
    raw_reference: Option<String>,
    category: String,
    #[serde(default)]
    expected_prefix: Option<String>,
    #[serde(default)]
    expected_suffix: Option<String>,
    #[serde(default)]
    required_phrases: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct EvaluationClipReport {
    pub id: String,
    pub audio: PathBuf,
    pub category: String,
    pub reference: Option<String>,
    pub raw_reference: Option<String>,
    pub hypothesis: String,
    pub raw_hypothesis: String,
    pub raw_word_error_rate: Option<f64>,
    pub expectation_failures: Vec<String>,
    pub expectation_count: usize,
    pub audio_duration_ms: u64,
    pub processing_ms: u64,
    pub substitutions: usize,
    pub deletions: usize,
    pub insertions: usize,
    pub word_error_rate: Option<f64>,
    pub silent_clip_failure: bool,
    /// Time spent flushing the model after the final audio frame.
    pub model_finalization_ms: u64,
    /// Virtual real-time backlog present when the hotkey is released.
    pub pre_stop_backlog_ms: u64,
    /// Backlog plus model finalization. This approximates release-to-result.
    pub finalization_latency_ms: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct EvaluationReport {
    pub schema_version: u32,
    pub config: Config,
    pub scored_clips: usize,
    pub expectation_failures: usize,
    pub total_audio_ms: u64,
    pub total_processing_ms: u64,
    pub real_time_factor: f64,
    pub model: String,
    pub streaming_profile: String,
    pub execution_provider: String,
    pub speech_gate_enabled: bool,
    pub clips: Vec<EvaluationClipReport>,
    pub total_substitutions: usize,
    pub total_deletions: usize,
    pub total_insertions: usize,
    pub total_word_errors: usize,
    pub total_reference_words: usize,
    pub word_error_rate: Option<f64>,
    pub silent_clip_failures: usize,
    pub p50_finalization_latency_ms: u64,
    pub p95_finalization_latency_ms: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct WordEdits {
    substitutions: usize,
    deletions: usize,
    insertions: usize,
}

impl WordEdits {
    fn total(self) -> usize {
        self.substitutions + self.deletions + self.insertions
    }

    fn with_substitution(self) -> Self {
        Self {
            substitutions: self.substitutions + 1,
            ..self
        }
    }

    fn with_deletion(self) -> Self {
        Self {
            deletions: self.deletions + 1,
            ..self
        }
    }

    fn with_insertion(self) -> Self {
        Self {
            insertions: self.insertions + 1,
            ..self
        }
    }
}

/// Evaluate one installed model against a JSONL manifest without changing
/// user configuration or delivering any text to the desktop.
pub fn evaluate_model(
    config: &Config,
    paths: &AppPaths,
    manifest_path: &Path,
) -> Result<EvaluationReport> {
    config.validate()?;
    let model_status = ModelStatus::inspect(config, paths);
    if !model_status.ready {
        return Err(AppError::Unavailable(model_status.message()));
    }
    let entries = read_manifest(manifest_path)?;
    if entries.is_empty() {
        return Err(AppError::Config(
            "evaluation manifest has no clips".to_owned(),
        ));
    }
    let mut recognizer = create_recognizer(config, &model_status.path)?;
    let mut clips = Vec::with_capacity(entries.len());
    let mut totals = WordEdits::default();
    let mut reference_words = 0usize;
    let mut silent_clip_failures = 0usize;
    let mut scored_clips = 0usize;

    for entry in entries {
        let waveform = read_wav(&entry.audio)?;
        let started = Instant::now();
        let hypothesis = transcribe_waveform(
            recognizer.as_mut(),
            waveform.sample_rate,
            &waveform.samples,
            config.denoise,
        )?;
        let text = processed_text(&hypothesis.text, config)?;
        let processing_ms = duration_to_ms(started.elapsed());
        let normalized_hypothesis = normalized_words(&text);
        let mut edits = WordEdits::default();
        let clip_wer = entry.reference.as_ref().map(|reference| {
            let normalized_reference = normalized_words(reference);
            edits = word_edits(&normalized_reference, &normalized_hypothesis);
            reference_words += normalized_reference.len();
            scored_clips += 1;
            rate(edits.total(), normalized_reference.len())
        });
        let raw_word_error_rate = entry.raw_reference.as_ref().map(|reference| {
            let reference = normalized_words(reference);
            rate(
                word_edits(&reference, &normalized_words(&hypothesis.text)).total(),
                reference.len(),
            )
        });
        let is_silent = entry
            .reference
            .as_ref()
            .is_some_and(|r| normalized_words(r).is_empty())
            || is_silence_or_noise(&entry.category);
        let silent_clip_failure = is_silent && !normalized_hypothesis.is_empty();
        totals.substitutions += edits.substitutions;
        totals.deletions += edits.deletions;
        totals.insertions += edits.insertions;
        silent_clip_failures += usize::from(silent_clip_failure);
        let expectation_failures = check_expectations(&entry, &normalized_hypothesis);
        clips.push(EvaluationClipReport {
            id: entry.id,
            audio: entry.audio,
            category: entry.category,
            reference: entry.reference,
            raw_reference: entry.raw_reference,
            hypothesis: text,
            raw_hypothesis: hypothesis.text,
            raw_word_error_rate,
            expectation_failures,
            expectation_count: entry.required_phrases.len()
                + usize::from(entry.expected_prefix.is_some())
                + usize::from(entry.expected_suffix.is_some())
                + usize::from(is_silent),
            audio_duration_ms: duration_to_ms(audio_duration(
                waveform.samples.len(),
                waveform.sample_rate,
            )),
            processing_ms,
            substitutions: edits.substitutions,
            deletions: edits.deletions,
            insertions: edits.insertions,
            word_error_rate: clip_wer,
            silent_clip_failure,
            model_finalization_ms: hypothesis.model_finalization_ms,
            pre_stop_backlog_ms: hypothesis.pre_stop_backlog_ms,
            finalization_latency_ms: hypothesis.finalization_latency_ms,
        });
    }

    let total_word_errors = totals.total();
    let mut latencies = clips
        .iter()
        .map(|clip| clip.finalization_latency_ms)
        .collect::<Vec<_>>();
    latencies.sort_unstable();
    let total_audio_ms = clips.iter().map(|c| c.audio_duration_ms).sum::<u64>();
    let total_processing_ms = clips.iter().map(|c| c.processing_ms).sum::<u64>();
    Ok(EvaluationReport {
        schema_version: 2,
        config: config.clone(),
        scored_clips,
        expectation_failures: clips.iter().map(|c| c.expectation_failures.len()).sum(),
        total_audio_ms,
        total_processing_ms,
        real_time_factor: total_processing_ms as f64 / total_audio_ms.max(1) as f64,
        model: config.model.clone(),
        streaming_profile: config.streaming_profile.clone(),
        execution_provider: execution_provider().to_owned(),
        speech_gate_enabled: config.speech_gate,
        clips,
        total_substitutions: totals.substitutions,
        total_deletions: totals.deletions,
        total_insertions: totals.insertions,
        total_word_errors,
        total_reference_words: reference_words,
        word_error_rate: (reference_words > 0).then(|| rate(total_word_errors, reference_words)),
        silent_clip_failures,
        p50_finalization_latency_ms: percentile(&latencies, 50),
        p95_finalization_latency_ms: percentile(&latencies, 95),
    })
}

fn check_expectations(entry: &ManifestEntry, words: &[String]) -> Vec<String> {
    let mut failures = Vec::new();
    if entry
        .expected_prefix
        .as_ref()
        .is_some_and(|p| !words.starts_with(&normalized_words(p)))
    {
        failures.push("prefix".to_owned());
    }
    if entry
        .expected_suffix
        .as_ref()
        .is_some_and(|p| !words.ends_with(&normalized_words(p)))
    {
        failures.push("suffix".to_owned());
    }
    for (index, phrase) in entry.required_phrases.iter().enumerate() {
        let required = normalized_words(phrase);
        if !words
            .windows(required.len())
            .any(|window| window == required)
        {
            failures.push(format!("required_phrase_{index}"));
        }
    }
    failures
}

impl EvaluationReport {
    pub fn check(&self, max_wer: Option<f64>, max_finalization_ms: Option<u64>) -> Result<()> {
        let mut failures = Vec::new();
        if self
            .clips
            .iter()
            .any(|c| c.reference.is_none() && c.expectation_count == 0)
        {
            failures
                .push("each clip needs a reviewed reference or an explicit expectation".to_owned());
        }
        if self.expectation_failures > 0 {
            failures.push(format!(
                "{} phrase or boundary failures",
                self.expectation_failures
            ));
        }
        if self.silent_clip_failures > 0 {
            failures.push(format!(
                "{} silent-input failures",
                self.silent_clip_failures
            ));
        }
        if let Some(maximum) = max_wer {
            if !maximum.is_finite() || maximum < 0.0 {
                return Err(AppError::Config(
                    "WER limit must be finite and nonnegative".into(),
                ));
            }
            if self.scored_clips != self.clips.len() || self.word_error_rate.is_none() {
                failures.push("WER gate requires a reference for every clip".to_owned());
            } else if self.word_error_rate.is_some_and(|wer| wer > maximum) {
                failures.push("word error rate exceeds limit".to_owned());
            }
        }
        if max_finalization_ms.is_some_and(|limit| self.p95_finalization_latency_ms > limit) {
            failures.push("p95 finalization latency exceeds limit".to_owned());
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(AppError::Unavailable(failures.join("; ")))
        }
    }
}

struct RecognitionMeasurement {
    text: String,
    model_finalization_ms: u64,
    pre_stop_backlog_ms: u64,
    finalization_latency_ms: u64,
}

fn transcribe_waveform(
    recognizer: &mut dyn StreamingRecognizer,
    sample_rate: i32,
    samples: &[f32],
    denoise: bool,
) -> Result<RecognitionMeasurement> {
    recognizer.start_session()?;
    let mut audio_pipeline = AudioPipeline::new(denoise);
    let chunk_size = ((sample_rate.max(1) as usize * FEED_CHUNK_MILLISECONDS) / 1_000).max(1);
    let mut samples_before = 0usize;
    let mut worker_ready_at = Duration::ZERO;
    for chunk in samples.chunks(chunk_size) {
        let arrival_at = audio_duration(samples_before, sample_rate);
        let processing_started = Instant::now();
        if let Err(error) = feed_pipeline_audio(recognizer, &mut audio_pipeline, sample_rate, chunk)
        {
            let _ = recognizer.cancel_session();
            return Err(error);
        }
        worker_ready_at = worker_ready_at.max(arrival_at) + processing_started.elapsed();
        samples_before += chunk.len();
    }
    let release_at = audio_duration(samples.len(), sample_rate);
    let pre_stop_backlog = worker_ready_at.saturating_sub(release_at);
    let finalization_started = Instant::now();
    if let Err(error) = finish_pipeline_audio(recognizer, &mut audio_pipeline) {
        let _ = recognizer.cancel_session();
        return Err(error);
    }
    let outcome = match recognizer.finish_session() {
        Ok(outcome) => outcome,
        Err(error) => {
            let _ = recognizer.cancel_session();
            return Err(error);
        }
    };
    let model_finalization = finalization_started.elapsed();
    let text = match outcome {
        RecognitionOutcome::Transcript(text) => text,
        RecognitionOutcome::NoSpeech => String::new(),
    };
    Ok(RecognitionMeasurement {
        text,
        model_finalization_ms: duration_to_ms(model_finalization),
        pre_stop_backlog_ms: duration_to_ms(pre_stop_backlog),
        finalization_latency_ms: duration_to_ms(pre_stop_backlog + model_finalization),
    })
}

fn feed_pipeline_audio(
    recognizer: &mut dyn StreamingRecognizer,
    audio_pipeline: &mut AudioPipeline,
    sample_rate: i32,
    samples: &[f32],
) -> Result<()> {
    let converted = audio_pipeline.accept_audio(sample_rate, samples)?;
    feed_converted_audio(recognizer, &converted)
}

fn finish_pipeline_audio(
    recognizer: &mut dyn StreamingRecognizer,
    audio_pipeline: &mut AudioPipeline,
) -> Result<()> {
    let converted = audio_pipeline.finish()?;
    feed_converted_audio(recognizer, &converted)
}

fn feed_converted_audio(recognizer: &mut dyn StreamingRecognizer, samples: &[f32]) -> Result<()> {
    if !samples.is_empty() {
        recognizer.accept_audio(MODEL_SAMPLE_RATE, samples)?;
    }
    Ok(())
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

fn read_manifest(path: &Path) -> Result<Vec<ManifestEntry>> {
    let contents = fs::read_to_string(path)?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut entries = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for (index, line) in contents.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let mut entry = serde_json::from_str::<ManifestEntry>(line).map_err(|error| {
            AppError::Config(format!(
                "invalid evaluation manifest line {}: {error}",
                index + 1
            ))
        })?;
        if entry.id.trim().is_empty() {
            return Err(AppError::Config(format!(
                "evaluation manifest line {} has an empty id",
                index + 1
            )));
        }
        if !ids.insert(entry.id.clone()) {
            return Err(AppError::Config(format!(
                "duplicate evaluation clip id: {}",
                entry.id
            )));
        }
        for phrase in entry
            .required_phrases
            .iter()
            .chain(entry.expected_prefix.iter())
            .chain(entry.expected_suffix.iter())
        {
            if normalized_words(phrase).is_empty() {
                return Err(AppError::Config(format!(
                    "empty expectation for clip {}",
                    entry.id
                )));
            }
        }
        if entry.audio.as_os_str().is_empty() {
            return Err(AppError::Config(format!(
                "evaluation manifest line {} has an empty audio path",
                index + 1
            )));
        }
        if entry.audio.is_relative() {
            entry.audio = parent.join(&entry.audio);
        }
        entries.push(entry);
    }
    Ok(entries)
}

/// Convert text to comparable words. Case, punctuation, apostrophes, and
/// repeated whitespace do not affect the score.
pub fn normalized_words(text: &str) -> Vec<String> {
    let mut normalized = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_alphanumeric() {
            normalized.extend(character.to_lowercase());
        } else if matches!(character, '\'' | '’' | '‘') {
            // Do not split contractions merely because an input uses a curly
            // apostrophe while another uses ASCII punctuation.
        } else {
            normalized.push(' ');
        }
    }
    normalized
        .split_whitespace()
        .map(ToOwned::to_owned)
        .collect()
}

fn word_edits(reference: &[String], hypothesis: &[String]) -> WordEdits {
    // Two rows preserve edit counts and tie-breaking with O(hypothesis) memory.
    let mut previous = (0..=hypothesis.len())
        .map(|insertions| WordEdits {
            insertions,
            ..WordEdits::default()
        })
        .collect::<Vec<_>>();
    let mut current = vec![WordEdits::default(); hypothesis.len() + 1];
    for (reference_index, reference_word) in reference.iter().enumerate() {
        current[0] = WordEdits {
            deletions: reference_index + 1,
            ..WordEdits::default()
        };
        for (hypothesis_index, hypothesis_word) in hypothesis.iter().enumerate() {
            let substitution = if reference_word == hypothesis_word {
                previous[hypothesis_index]
            } else {
                previous[hypothesis_index].with_substitution()
            };
            current[hypothesis_index + 1] = choose_best([
                substitution,
                previous[hypothesis_index + 1].with_deletion(),
                current[hypothesis_index].with_insertion(),
            ]);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[hypothesis.len()]
}

fn choose_best(candidates: [WordEdits; 3]) -> WordEdits {
    candidates
        .into_iter()
        .min_by_key(|candidate| {
            (
                candidate.total(),
                candidate.substitutions,
                candidate.deletions,
                candidate.insertions,
            )
        })
        .expect("three edit candidates are always present")
}

fn rate(errors: usize, reference_words: usize) -> f64 {
    errors as f64 / reference_words.max(1) as f64
}

fn percentile(sorted_values: &[u64], percentile: usize) -> u64 {
    if sorted_values.is_empty() {
        return 0;
    }
    let rank = (sorted_values.len() * percentile).div_ceil(100).max(1);
    sorted_values[rank - 1]
}

fn is_silence_or_noise(category: &str) -> bool {
    let category = category.to_ascii_lowercase();
    matches!(category.as_str(), "silence" | "noise" | "no_speech")
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn checks_boundaries_and_phrases_independently() {
        let entry: ManifestEntry = serde_json::from_str(r#"{"id":"test","audio":"x.wav","category":"dictation","expected_prefix":"hello","expected_suffix":"last word","required_phrases":["middle phrase"]}"#).unwrap();
        assert!(
            entry.reference.is_none(),
            "unverified text must not count as a silent reference"
        );
        assert!(
            check_expectations(&entry, &normalized_words("Hello middle phrase last word."))
                .is_empty()
        );
        assert_eq!(
            check_expectations(&entry, &normalized_words("middle phrase last")),
            ["prefix", "suffix"]
        );
        assert!(!is_silence_or_noise("noisy_speech"));
    }

    #[test]
    fn duplicate_ids_and_empty_expectations_are_invalid() {
        let directory = tempdir().unwrap();
        let manifest = directory.path().join("manifest.jsonl");
        let line = r#"{"id":"one","audio":"a.wav","category":"speech"}"#;
        fs::write(&manifest, format!("{line}\n{line}\n")).unwrap();
        assert!(read_manifest(&manifest).is_err());
        fs::write(
            &manifest,
            r#"{"id":"one","audio":"a.wav","category":"speech","required_phrases":["!"]}"#,
        )
        .unwrap();
        assert!(read_manifest(&manifest).is_err());
    }

    #[test]
    fn evaluates_the_same_text_pipeline_as_delivery() {
        let config = Config::default();
        assert_eq!(
            processed_text("Uh five to eight runs", &config).unwrap(),
            "5 to 8 runs"
        );
        assert_eq!(processed_text("", &config).unwrap(), "");
    }

    #[test]
    fn normalizes_case_punctuation_apostrophes_and_whitespace() {
        assert_eq!(
            normalized_words("  DON’T,   stop! It's  fine. "),
            vec!["dont", "stop", "its", "fine"]
        );
    }

    #[test]
    fn reports_substitution_deletion_and_insertion() {
        let reference = normalized_words("one two three");
        let hypothesis = normalized_words("one four extra");
        let edits = word_edits(&reference, &hypothesis);
        assert_eq!(edits.substitutions, 2);
        assert_eq!(edits.deletions, 0);
        assert_eq!(edits.insertions, 0);
    }

    #[test]
    fn scores_empty_reference_as_silent_failure_when_text_is_emitted() {
        let edits = word_edits(&[], &normalized_words("noise words"));
        assert_eq!(edits.insertions, 2);
        assert_eq!(rate(edits.total(), 0), 2.0);
    }

    #[test]
    fn resolves_relative_audio_paths_from_the_manifest_directory() {
        let directory = tempdir().expect("temporary directory");
        let manifest = directory.path().join("manifest.jsonl");
        fs::write(
            &manifest,
            "{\"id\":\"clip\",\"audio\":\"audio/clip.wav\",\"reference\":\"text\",\"category\":\"dictation\"}\n",
        )
        .expect("write manifest");
        let entries = read_manifest(&manifest).expect("read manifest");
        assert_eq!(entries[0].audio, directory.path().join("audio/clip.wav"));
    }
}
