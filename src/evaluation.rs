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
    config::Config,
    error::{AppError, Result},
    model::ModelStatus,
    paths::AppPaths,
    recognizer::{RecognitionOutcome, StreamingRecognizer, create_recognizer, execution_provider},
};

const FEED_CHUNK_MILLISECONDS: usize = 20;

#[derive(Clone, Debug, Deserialize)]
struct ManifestEntry {
    id: String,
    audio: PathBuf,
    reference: String,
    category: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct EvaluationClipReport {
    pub id: String,
    pub audio: PathBuf,
    pub category: String,
    pub reference: String,
    pub hypothesis: String,
    pub substitutions: usize,
    pub deletions: usize,
    pub insertions: usize,
    pub word_error_rate: f64,
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
    pub word_error_rate: f64,
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

    for entry in entries {
        let waveform = read_wav(&entry.audio)?;
        let hypothesis =
            transcribe_waveform(recognizer.as_mut(), waveform.sample_rate, &waveform.samples)?;
        let normalized_reference = normalized_words(&entry.reference);
        let normalized_hypothesis = normalized_words(&hypothesis.text);
        let edits = word_edits(&normalized_reference, &normalized_hypothesis);
        let is_silent = normalized_reference.is_empty() || is_silence_or_noise(&entry.category);
        let silent_clip_failure = is_silent && !normalized_hypothesis.is_empty();
        let clip_wer = rate(edits.total(), normalized_reference.len());
        reference_words += normalized_reference.len();
        totals.substitutions += edits.substitutions;
        totals.deletions += edits.deletions;
        totals.insertions += edits.insertions;
        silent_clip_failures += usize::from(silent_clip_failure);
        clips.push(EvaluationClipReport {
            id: entry.id,
            audio: entry.audio,
            category: entry.category,
            reference: entry.reference,
            hypothesis: hypothesis.text,
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
    Ok(EvaluationReport {
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
        word_error_rate: rate(total_word_errors, reference_words),
        silent_clip_failures,
        p50_finalization_latency_ms: percentile(&latencies, 50),
        p95_finalization_latency_ms: percentile(&latencies, 95),
    })
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
) -> Result<RecognitionMeasurement> {
    recognizer.start_session()?;
    let chunk_size = ((sample_rate.max(1) as usize * FEED_CHUNK_MILLISECONDS) / 1_000).max(1);
    let mut samples_before = 0usize;
    let mut worker_ready_at = Duration::ZERO;
    for chunk in samples.chunks(chunk_size) {
        let arrival_at = audio_duration(samples_before, sample_rate);
        let processing_started = Instant::now();
        if let Err(error) = recognizer.accept_audio(sample_rate, chunk) {
            let _ = recognizer.cancel_session();
            return Err(error);
        }
        worker_ready_at = worker_ready_at.max(arrival_at) + processing_started.elapsed();
        samples_before += chunk.len();
    }
    let release_at = audio_duration(samples.len(), sample_rate);
    let pre_stop_backlog = worker_ready_at.saturating_sub(release_at);
    let finalization_started = Instant::now();
    let outcome = recognizer.finish_session()?;
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
    let mut matrix = vec![vec![WordEdits::default(); hypothesis.len() + 1]; reference.len() + 1];
    for (index, cell) in matrix.iter_mut().enumerate().skip(1) {
        cell[0] = WordEdits {
            deletions: index,
            ..WordEdits::default()
        };
    }
    for (index, cell) in matrix[0].iter_mut().enumerate().skip(1) {
        *cell = WordEdits {
            insertions: index,
            ..WordEdits::default()
        };
    }

    for reference_index in 1..=reference.len() {
        for hypothesis_index in 1..=hypothesis.len() {
            let substitution = if reference[reference_index - 1] == hypothesis[hypothesis_index - 1]
            {
                matrix[reference_index - 1][hypothesis_index - 1]
            } else {
                matrix[reference_index - 1][hypothesis_index - 1].with_substitution()
            };
            let deletion = matrix[reference_index - 1][hypothesis_index].with_deletion();
            let insertion = matrix[reference_index][hypothesis_index - 1].with_insertion();
            matrix[reference_index][hypothesis_index] =
                choose_best([substitution, deletion, insertion]);
        }
    }
    matrix[reference.len()][hypothesis.len()]
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
    category.contains("silence") || category.contains("noise") || category.contains("no_speech")
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

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
