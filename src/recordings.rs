use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    audio::write_float_wav,
    domain::TranscriptionStatus,
    error::{AppError, Result},
};

const MAX_RECORDINGS: usize = 7;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CaptureStatus {
    pub dropped_samples: usize,
    pub backend_failed: bool,
    pub duration_exceeded: bool,
    pub stop_failed: bool,
    pub drain_failed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecordingMetadata {
    pub version: u32,
    pub session_id: String,
    pub stopped_at_ms: u64,
    pub sample_rate: i32,
    pub frames: usize,
    pub model: String,
    pub streaming_profile: String,
    pub speech_gate: bool,
    pub denoise: bool,
    pub itn: bool,
    pub capture: CaptureStatus,
    pub transcription: TranscriptionStatus,
}

pub struct Recording {
    pub metadata: RecordingMetadata,
    pub samples: Vec<f32>,
}

pub struct SaveOutcome {
    pub path: PathBuf,
    pub retention_warning: Option<String>,
}

pub struct RecordingStore {
    root: PathBuf,
    #[cfg(test)]
    fail_after_wav: bool,
    #[cfg(test)]
    fail_staging_dir_sync: bool,
    #[cfg(test)]
    fail_root_dir_sync: bool,
    #[cfg(test)]
    fail_prune: bool,
}

impl RecordingStore {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            #[cfg(test)]
            fail_after_wav: false,
            #[cfg(test)]
            fail_staging_dir_sync: false,
            #[cfg(test)]
            fail_root_dir_sync: false,
            #[cfg(test)]
            fail_prune: false,
        }
    }

    pub fn save(&self, recording: &Recording) -> Result<SaveOutcome> {
        let metadata = &recording.metadata;
        if metadata.version != 1
            || !valid_id(&metadata.session_id)
            || metadata.frames != recording.samples.len()
            || metadata.sample_rate <= 0
        {
            return Err(AppError::Unavailable("invalid recording metadata".into()));
        }
        let payload = serde_json::to_vec_pretty(metadata)?;
        self.prepare_root()?;
        let path = self.root.join(format!(
            "{:020}-{}",
            metadata.stopped_at_ms, metadata.session_id
        ));
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                return Err(AppError::Unavailable(
                    "recording entry already exists".into(),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        // Reject an entry that rotation would immediately delete. A successful
        // save must always return a path that still holds the new recording.
        let owned = self.owned_entries()?;
        if owned.len() >= MAX_RECORDINGS {
            let oldest_retained = &owned[owned.len() - MAX_RECORDINGS];
            if (metadata.stopped_at_ms, metadata.session_id.as_str())
                < (oldest_retained.1, oldest_retained.2.as_str())
            {
                return Err(AppError::Unavailable(
                    "recording is older than the retained entries".into(),
                ));
            }
        }
        let staging = self.root.join(format!(".staging-{}", metadata.session_id));
        DirBuilder::new().mode(0o700).create(&staging)?;
        let staged = (|| -> Result<()> {
            let mut audio = private_file(&staging.join("audio.wav"))?;
            write_float_wav(&mut audio, metadata.sample_rate, &recording.samples)?;
            audio.sync_all()?;
            #[cfg(test)]
            if self.fail_after_wav {
                return Err(AppError::Unavailable("injected staging failure".into()));
            }
            let mut json = private_file(&staging.join("metadata.json"))?;
            json.write_all(&payload)?;
            json.sync_all()?;
            #[cfg(test)]
            if self.fail_staging_dir_sync {
                return Err(AppError::Unavailable(
                    "injected staging directory sync failure".into(),
                ));
            }
            File::open(&staging)?.sync_all()?;
            fs::rename(&staging, &path)?;
            Ok(())
        })();
        if let Err(error) = staged {
            // If cleanup fails, reconcile will retry this uncommitted staging entry.
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
        let mut warnings = Vec::new();
        let root_sync = (|| -> Result<()> {
            #[cfg(test)]
            if self.fail_root_dir_sync {
                return Err(AppError::Unavailable(
                    "injected root directory sync failure".into(),
                ));
            }
            File::open(&self.root)?.sync_all()?;
            Ok(())
        })();
        if let Err(error) = root_sync {
            return Ok(SaveOutcome {
                path,
                retention_warning: Some(format!(
                    "recording saved, but directory sync failed: {error}"
                )),
            });
        }
        if let Err(error) = self.prune() {
            warnings.push(format!("retention prune failed; retry on startup: {error}"));
        }
        Ok(SaveOutcome {
            path,
            retention_warning: (!warnings.is_empty())
                .then(|| format!("recording saved, but {}", warnings.join("; "))),
        })
    }

    pub fn reconcile(&self) -> Result<()> {
        self.prepare_root()?;
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.strip_prefix(".staging-").is_some_and(valid_id)
                && entry.file_type()?.is_dir()
                && expected_files(&entry.path(), false)?
            {
                fs::remove_dir_all(entry.path())?;
            }
        }
        self.prune()
    }

    fn prepare_root(&self) -> Result<()> {
        // The caller owns the state-home path. Only nvstt and its children are
        // store-managed; a symlink in state-home itself is allowed.
        let managed = self
            .root
            .ancestors()
            .find(|path| path.file_name().is_some_and(|name| name == "nvstt"))
            .unwrap_or(&self.root);
        if let Some(parent) = managed
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)?;
        }
        let mut path = managed.to_path_buf();
        ensure_real_directory(&path)?;
        for component in self.root.strip_prefix(managed).unwrap().components() {
            path.push(component);
            ensure_real_directory(&path)?;
        }
        fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700))?;
        Ok(())
    }

    fn prune(&self) -> Result<()> {
        #[cfg(test)]
        if self.fail_prune {
            return Err(AppError::Unavailable(
                "injected retention prune failure".into(),
            ));
        }
        let owned = self.owned_entries()?;
        for (path, _, _) in owned
            .iter()
            .take(owned.len().saturating_sub(MAX_RECORDINGS))
        {
            fs::remove_dir_all(path)?;
        }
        Ok(())
    }

    fn owned_entries(&self) -> Result<Vec<(PathBuf, u64, String)>> {
        let mut owned = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let Some((timestamp, id)) = owned_name(name.to_str().unwrap_or("")) else {
                continue;
            };
            let path = entry.path();
            match expected_files(&path, true) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(AppError::Io(error))
                    if error.kind() == std::io::ErrorKind::PermissionDenied =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            }
            let Ok(contents) = fs::read(path.join("metadata.json")) else {
                continue;
            };
            let Ok(metadata) = serde_json::from_slice::<RecordingMetadata>(&contents) else {
                continue;
            };
            if metadata.version == 1
                && metadata.stopped_at_ms == timestamp
                && metadata.session_id == id
            {
                owned.push((path, timestamp, id.to_owned()));
            }
        }
        owned.sort_by(|a, b| (a.1, &a.2).cmp(&(b.1, &b.2)));
        Ok(owned)
    }
}

fn ensure_real_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
        Ok(_) => Err(AppError::Unavailable(format!(
            "recording store path is not a real directory: {}",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            DirBuilder::new().mode(0o700).create(path)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn valid_id(id: &str) -> bool {
    id.split_once('-').is_some_and(|(timestamp, sequence)| {
        !timestamp.is_empty()
            && !sequence.is_empty()
            && timestamp.bytes().all(|b| b.is_ascii_digit())
            && sequence.bytes().all(|b| b.is_ascii_digit())
    })
}

fn owned_name(name: &str) -> Option<(u64, &str)> {
    let (timestamp, id) = name.split_once('-')?;
    if timestamp.len() != 20 || !valid_id(id) {
        return None;
    }
    Some((timestamp.parse().ok()?, id))
}

fn private_file(path: &Path) -> Result<File> {
    Ok(OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?)
}

// Do not prune a directory that contains unrelated files or symlinks.
fn expected_files(path: &Path, committed: bool) -> Result<bool> {
    let (mut audio, mut metadata) = (false, false);
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            return Ok(false);
        }
        match entry.file_name().to_str() {
            Some("audio.wav") => audio = true,
            Some("metadata.json") => metadata = true,
            _ => return Ok(false),
        }
    }
    Ok(!committed || (audio && metadata))
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt};

    use tempfile::tempdir;

    use super::*;
    use crate::{config::Config, domain::TranscriptionStatus};

    fn fixture(n: u64, samples: &[f32]) -> Recording {
        let config = Config::default();
        Recording {
            metadata: RecordingMetadata {
                version: 1,
                session_id: format!("1700000000000-{n}"),
                stopped_at_ms: 1_700_000_000_000 + n,
                sample_rate: 48_000,
                frames: samples.len(),
                model: config.model,
                streaming_profile: config.streaming_profile,
                speech_gate: config.speech_gate,
                denoise: config.denoise,
                itn: config.itn,
                capture: CaptureStatus {
                    dropped_samples: 0,
                    backend_failed: false,
                    duration_exceeded: false,
                    stop_failed: false,
                    drain_failed: false,
                },
                transcription: TranscriptionStatus::Succeeded,
            },
            samples: samples.to_vec(),
        }
    }

    fn entries(root: &std::path::Path) -> Vec<std::path::PathBuf> {
        fs::read_dir(root)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect()
    }

    #[test]
    fn rotates_eight_entries_and_writes_private_float_wav_and_metadata() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("recordings");
        let store = RecordingStore::new(root.clone());
        for n in 0..8 {
            let outcome = store.save(&fixture(n, &[0.125, -0.25])).unwrap();
            assert!(outcome.retention_warning.is_none());
            let wave = crate::audio::read_wav(&outcome.path.join("audio.wav")).unwrap();
            assert_eq!(wave.sample_rate, 48_000);
            assert_eq!(wave.samples, [0.125, -0.25]);
        }
        let paths = entries(&root);
        assert_eq!(paths.len(), 7);
        assert!(
            !paths
                .iter()
                .any(|p| p.file_name().unwrap().to_string_lossy().ends_with("-0"))
        );
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for path in paths {
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o700
            );
            for name in ["audio.wav", "metadata.json"] {
                assert_eq!(
                    fs::metadata(path.join(name)).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
            let metadata: RecordingMetadata =
                serde_json::from_slice(&fs::read(path.join("metadata.json")).unwrap()).unwrap();
            assert_eq!(metadata.frames, 2);
            assert_eq!(metadata.version, 1);
            let json = fs::read_to_string(path.join("metadata.json")).unwrap();
            assert!(!json.contains("\"transcript\":"));
            assert!(!json.contains("\"samples\":"));
        }
    }

    #[test]
    fn ties_are_pruned_by_session_id() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("recordings");
        let store = RecordingStore::new(root.clone());
        for n in 0..8 {
            let mut recording = fixture(n, &[]);
            recording.metadata.stopped_at_ms = 1_700_000_000_000;
            store.save(&recording).unwrap();
        }
        assert_eq!(entries(&root).len(), 7);
        assert!(
            entries(&root).iter().all(|p| !p
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with("-0"))
        );
    }

    #[test]
    fn startup_reconciles_staging_and_old_entries_but_preserves_unrelated_files() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("recordings");
        let store = RecordingStore::new(root.clone());
        for n in 0..8 {
            store.save(&fixture(n, &[0.125])).unwrap();
        }
        // An older valid entry reappears (for example after an interrupted prune).
        let old = fixture(0, &[0.125]);
        let old_path = root.join(format!(
            "{:020}-{}",
            old.metadata.stopped_at_ms, old.metadata.session_id
        ));
        fs::create_dir(&old_path).unwrap();
        fs::write(
            old_path.join("metadata.json"),
            serde_json::to_vec(&old.metadata).unwrap(),
        )
        .unwrap();
        fs::write(old_path.join("audio.wav"), b"wav").unwrap();
        let staging = root.join(".staging-1700000000000-99");
        fs::create_dir(&staging).unwrap();
        fs::write(staging.join("audio.wav"), b"partial").unwrap();
        fs::write(root.join("notes.txt"), b"keep").unwrap();
        let other_dir = root.join("another-app");
        fs::create_dir(&other_dir).unwrap();
        fs::write(other_dir.join("metadata.json"), b"not ours").unwrap();
        std::os::unix::fs::symlink(&old_path, root.join("linked-entry")).unwrap();
        std::os::unix::fs::symlink(&staging, root.join(".staging-1700000000000-100")).unwrap();

        store.reconcile().unwrap();
        assert!(!staging.exists());
        assert!(!old_path.exists());
        assert_eq!(entries(&root).len(), 11); // seven entries, file, directory, two symlinks
        assert!(root.join("notes.txt").exists());
        assert!(other_dir.exists());
        assert!(
            fs::symlink_metadata(root.join("linked-entry"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            fs::symlink_metadata(root.join(".staging-1700000000000-100"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn a_file_blocking_root_does_not_change_it() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("recordings");
        fs::write(&root, b"keep").unwrap();
        assert!(
            RecordingStore::new(root.clone())
                .save(&fixture(0, &[0.125]))
                .is_err()
        );
        assert_eq!(fs::read(root).unwrap(), b"keep");
    }

    #[test]
    fn symlinked_nvstt_cannot_redirect_save_or_reconciliation() {
        let dir = tempdir().unwrap();
        let state = dir.path().join("state");
        let outside = dir.path().join("outside");
        fs::create_dir(&state).unwrap();
        fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, state.join("nvstt")).unwrap();
        let root = state.join("nvstt/recordings");
        let store = RecordingStore::new(root);
        assert!(store.save(&fixture(0, &[0.125])).is_err());
        assert!(!outside.join("recordings").exists());

        let redirected = outside.join("recordings");
        fs::create_dir(&redirected).unwrap();
        let staging = redirected.join(".staging-1700000000000-99");
        fs::create_dir(&staging).unwrap();
        fs::write(staging.join("audio.wav"), b"keep").unwrap();
        assert!(store.reconcile().is_err());
        assert_eq!(fs::read(staging.join("audio.wav")).unwrap(), b"keep");
    }

    #[test]
    fn symlinked_xdg_state_home_can_still_hold_recordings() {
        let dir = tempdir().unwrap();
        let actual = dir.path().join("actual-state");
        fs::create_dir(&actual).unwrap();
        let state = dir.path().join("state-link");
        std::os::unix::fs::symlink(&actual, &state).unwrap();
        let store = RecordingStore::new(state.join("nvstt/recordings"));
        let saved = store.save(&fixture(0, &[0.125])).unwrap();
        assert!(saved.path.join("audio.wav").is_file());
        store.reconcile().unwrap();
    }

    #[test]
    fn staging_directory_sync_failure_does_not_publish_or_prune() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("recordings");
        let mut store = RecordingStore::new(root.clone());
        for n in 0..7 {
            store.save(&fixture(n, &[0.125])).unwrap();
        }
        store.fail_staging_dir_sync = true;
        assert!(store.save(&fixture(7, &[0.125])).is_err());
        assert_eq!(entries(&root).len(), 7);
        assert!(
            !entries(&root).iter().any(|p| p
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with("-7"))
        );
    }

    #[test]
    fn published_root_sync_failure_reports_saved_path_and_warning() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("recordings");
        let mut store = RecordingStore::new(root.clone());
        store.fail_root_dir_sync = true;
        let outcome = store.save(&fixture(0, &[0.125])).unwrap();
        assert!(outcome.path.join("audio.wav").is_file());
        assert!(
            outcome
                .retention_warning
                .unwrap()
                .contains("directory sync")
        );
    }

    #[test]
    fn failed_root_sync_keeps_all_previous_entries_until_reconcile() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("recordings");
        let healthy = RecordingStore::new(root.clone());
        for n in 0..7 {
            healthy.save(&fixture(n, &[0.125])).unwrap();
        }
        let foreign = root.join("notes.txt");
        fs::write(&foreign, b"keep").unwrap();
        let mut failing = RecordingStore::new(root.clone());
        failing.fail_root_dir_sync = true;
        let saved = failing.save(&fixture(7, &[0.25])).unwrap();
        assert!(saved.path.join("audio.wav").is_file());
        assert!(
            saved
                .retention_warning
                .as_deref()
                .unwrap()
                .contains("directory sync")
        );
        assert_eq!(failing.owned_entries().unwrap().len(), 8);
        assert!(
            entries(&root)
                .iter()
                .any(|p| p.file_name().unwrap().to_string_lossy().ends_with("-0"))
        );
        healthy.reconcile().unwrap();
        assert_eq!(healthy.owned_entries().unwrap().len(), 7);
        assert!(saved.path.exists());
        assert_eq!(fs::read(foreign).unwrap(), b"keep");
    }

    #[test]
    fn failed_staging_write_does_not_publish_or_prune() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("recordings");
        let mut store = RecordingStore::new(root.clone());
        for n in 0..7 {
            store.save(&fixture(n, &[0.125])).unwrap();
        }
        store.fail_after_wav = true;
        assert!(store.save(&fixture(7, &[0.125])).is_err());
        let paths = entries(&root);
        assert_eq!(paths.len(), 7);
        assert!(
            paths
                .iter()
                .any(|p| p.file_name().unwrap().to_string_lossy().ends_with("-0"))
        );
    }

    #[test]
    fn failed_post_commit_prune_reports_saved_path_and_reconcile_retries() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("recordings");
        let mut store = RecordingStore::new(root.clone());
        for n in 0..7 {
            store.save(&fixture(n, &[0.125])).unwrap();
        }
        store.fail_prune = true;
        let outcome = store.save(&fixture(7, &[0.125])).unwrap();
        assert!(outcome.path.join("audio.wav").is_file());
        assert!(outcome.retention_warning.unwrap().contains("retention"));
        assert_eq!(entries(&root).len(), 8);
        RecordingStore::new(root.clone()).reconcile().unwrap();
        assert_eq!(entries(&root).len(), 7);
    }

    #[test]
    fn invalid_metadata_or_samples_cannot_publish_or_prune() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("recordings");
        let store = RecordingStore::new(root.clone());
        for n in 0..7 {
            store.save(&fixture(n, &[])).unwrap();
        }
        for mut invalid in [
            fixture(7, &[1.0]),
            fixture(8, &[1.0]),
            fixture(9, &[1.0]),
            fixture(10, &[1.0]),
        ] {
            match invalid.metadata.stopped_at_ms % 10 {
                7 => invalid.metadata.frames = 0,
                8 => invalid.metadata.sample_rate = 0,
                9 => invalid.metadata.version = 2,
                _ => invalid.metadata.session_id = "../bad".into(),
            }
            assert!(store.save(&invalid).is_err());
        }
        assert_eq!(entries(&root).len(), 7);
    }

    #[test]
    fn a_stale_save_cannot_return_a_path_that_rotation_would_remove() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("recordings");
        let store = RecordingStore::new(root.clone());
        for n in 1..8 {
            store.save(&fixture(n, &[])).unwrap();
        }
        assert!(store.save(&fixture(0, &[])).is_err());
        assert_eq!(entries(&root).len(), 7);
        assert!(
            entries(&root).iter().all(|p| !p
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with("-0"))
        );
    }

    #[test]
    #[ignore]
    fn measure_two_minute_save() {
        let dir = tempdir().unwrap();
        let store = RecordingStore::new(dir.path().join("recordings"));
        let mut failing_store = RecordingStore::new(dir.path().join("failed-recordings"));
        failing_store.fail_after_wav = true;
        let samples = vec![0.0_f32; 48_000 * 120];
        let mut times = Vec::new();
        let mut failure_times = Vec::new();
        for n in 0..20 {
            let recording = fixture(n, &samples);
            let began = std::time::Instant::now();
            store.save(&recording).unwrap();
            times.push(began.elapsed().as_millis());

            let began = std::time::Instant::now();
            let failure = failing_store.save(&recording);
            failure_times.push(began.elapsed().as_millis());
            assert!(matches!(
                failure,
                Err(AppError::Unavailable(ref message)) if message == "injected staging failure"
            ));
        }
        times.sort_unstable();
        failure_times.sort_unstable();
        println!("save: p50={} ms p95={} ms", times[9], times[18]);
        println!(
            "injected pre-commit failure: p50={} ms p95={} ms",
            failure_times[9], failure_times[18]
        );
    }

    #[test]
    fn unreadable_foreign_directory_does_not_block_saving_or_get_pruned() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("recordings");
        let store = RecordingStore::new(root.clone());
        store.save(&fixture(0, &[])).unwrap();
        let foreign = root.join("00000000000000000001-1700000000000-99");
        fs::create_dir(&foreign).unwrap();
        fs::write(foreign.join("notes.txt"), b"keep").unwrap();
        fs::set_permissions(&foreign, fs::Permissions::from_mode(0o000)).unwrap();
        let saved = store.save(&fixture(1, &[0.25])).unwrap();
        store.reconcile().unwrap();
        assert!(saved.path.join("audio.wav").is_file());
        fs::set_permissions(&foreign, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(fs::read(foreign.join("notes.txt")).unwrap(), b"keep");
    }

    #[test]
    fn invalid_owned_entry_is_never_pruned() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("recordings");
        let store = RecordingStore::new(root.clone());
        for n in 0..7 {
            store.save(&fixture(n, &[0.125])).unwrap();
        }
        let old = fixture(20, &[]);
        let invalid = root.join(format!("{:020}-{}", 1, old.metadata.session_id));
        fs::create_dir(&invalid).unwrap();
        fs::write(invalid.join("metadata.json"), b"not json").unwrap();
        fs::write(invalid.join("audio.wav"), b"not wav").unwrap();
        let staged_unrelated = root.join(".staging-1700000000000-20");
        fs::create_dir(&staged_unrelated).unwrap();
        fs::write(staged_unrelated.join("notes.txt"), b"keep").unwrap();
        store.reconcile().unwrap();
        assert!(invalid.exists());
        assert!(staged_unrelated.join("notes.txt").exists());
    }
}
