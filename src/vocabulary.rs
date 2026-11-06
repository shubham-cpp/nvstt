//! Explicit personal corrections. Readers see a complete, immutable snapshot.
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::Path,
};

use serde::{Deserialize, Serialize};

use crate::{
    dictation_transcript::{Replacements, replacement_key},
    error::{AppError, Result},
};

const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_ENTRIES: usize = 512;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Vocabulary {
    version: u32,
    #[serde(deserialize_with = "deserialize_entries")]
    entries: BTreeMap<String, String>,
}

impl Default for Vocabulary {
    fn default() -> Self {
        Self {
            version: 1,
            entries: BTreeMap::new(),
        }
    }
}

impl Vocabulary {
    pub fn load(path: &Path) -> Result<Self> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(AppError::Config("vocabulary file exceeds 2 MiB".into()));
        }
        let stored: Self = serde_json::from_slice(&bytes).map_err(|error| {
            AppError::Config(format!(
                "cannot read vocabulary {}: {error}",
                path.display()
            ))
        })?;
        if stored.version != 1 {
            return Err(AppError::Config("unsupported vocabulary version".into()));
        }
        if stored.entries.len() > MAX_ENTRIES {
            return Err(AppError::Config("vocabulary exceeds 512 entries".into()));
        }
        let mut vocabulary = Self::default();
        for (pattern, replacement) in stored.entries {
            validate(&pattern, 256)?;
            validate(&replacement, 2048)?;
            if vocabulary
                .entries
                .insert(replacement_key(&pattern), replacement)
                .is_some()
            {
                return Err(AppError::Config(
                    "vocabulary has duplicate matching phrases".into(),
                ));
            }
        }
        Ok(vocabulary)
    }

    pub fn entries(&self) -> &BTreeMap<String, String> {
        &self.entries
    }

    pub fn apply_to(&self, base: &Replacements) -> Replacements {
        base.with_overrides(&Replacements::from_pairs(self.entries.clone()))
    }

    pub fn add(path: &Path, pattern: &str, replacement: &str) -> Result<()> {
        validate(pattern, 256)?;
        validate(replacement, 2048)?;
        Self::update(path, |vocabulary| {
            vocabulary
                .entries
                .insert(replacement_key(pattern), replacement.trim().to_owned());
            if vocabulary.entries.len() > MAX_ENTRIES {
                return Err(AppError::Config("vocabulary exceeds 512 entries".into()));
            }
            Ok(())
        })
    }

    pub fn remove(path: &Path, pattern: &str) -> Result<()> {
        validate(pattern, 256)?;
        Self::update(path, |vocabulary| {
            vocabulary
                .entries
                .remove(&replacement_key(pattern))
                .ok_or_else(|| {
                    AppError::Config("phrase is not in your personal vocabulary".into())
                })?;
            Ok(())
        })
    }

    fn update(path: &Path, change: impl FnOnce(&mut Self) -> Result<()>) -> Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| AppError::Config("vocabulary path has no parent".into()))?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
        // Lock a separate inode: atomic replacement must not replace the lock.
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path.with_extension("lock"))?;
        lock.try_lock().map_err(|error| {
            AppError::Unavailable(format!("vocabulary is busy; retry: {error}"))
        })?;
        let mut vocabulary = Self::load(path)?;
        change(&mut vocabulary)?;
        let mut bytes = serde_json::to_vec_pretty(&vocabulary)?;
        bytes.push(b'\n');
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(AppError::Config("vocabulary file exceeds 2 MiB".into()));
        }
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(&bytes)?;
        file.as_file().sync_all()?;
        file.persist(path)
            .map_err(|error| AppError::from(error.error))?;
        File::open(parent)?.sync_all()?;
        Ok(())
    }
}

fn deserialize_entries<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<BTreeMap<String, String>, D::Error> {
    struct Entries;
    impl<'de> serde::de::Visitor<'de> for Entries {
        type Value = BTreeMap<String, String>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a map of unique correction phrases")
        }

        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> std::result::Result<Self::Value, M::Error> {
            let mut entries = BTreeMap::new();
            while let Some((phrase, replacement)) = map.next_entry::<String, String>()? {
                if entries.insert(phrase, replacement).is_some() {
                    return Err(serde::de::Error::custom("duplicate vocabulary phrase"));
                }
                if entries.len() > MAX_ENTRIES {
                    return Err(serde::de::Error::custom("vocabulary exceeds 512 entries"));
                }
            }
            Ok(entries)
        }
    }
    deserializer.deserialize_map(Entries)
}

fn validate(text: &str, max_bytes: usize) -> Result<()> {
    if text.trim().is_empty() || text.len() > max_bytes || text.chars().any(char::is_control) {
        return Err(AppError::Config(format!(
            "vocabulary text must be nonempty, contain no control characters, and fit in {max_bytes} bytes"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        dictation_transcript::{DictationTranscript, dictation_transcript},
        recognizer::RecognitionOutcome,
    };
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn corrections_override_equivalent_config_rules_without_cascading() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("vocabulary.json");
        Vocabulary::add(&path, "Para  Keet", "Parakeet").unwrap();
        Vocabulary::add(&path, "parakeet", "wrong second pass").unwrap();
        let base = Replacements::from_pairs([("para keet".into(), "old value".into())]);
        let rules = Vocabulary::load(&path).unwrap().apply_to(&base);
        assert_eq!(
            dictation_transcript(
                RecognitionOutcome::Transcript("Use para keet, with twenty one files".into()),
                &rules,
                true
            )
            .unwrap(),
            DictationTranscript::Ready("Use Parakeet, with 21 files".into())
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        Vocabulary::remove(&path, "PARA KEET").unwrap();
        assert!(
            !Vocabulary::load(&path)
                .unwrap()
                .entries()
                .contains_key("para keet")
        );
    }

    #[test]
    fn invalid_edits_and_busy_writers_leave_saved_rules_intact() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("vocabulary.json");
        Vocabulary::add(&path, "nv stt", "nvstt").unwrap();
        let before = fs::read(&path).unwrap();
        for (pattern, replacement) in [("", "value"), ("word", ""), ("word", "line\nbreak")] {
            assert!(Vocabulary::add(&path, pattern, replacement).is_err());
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path.with_extension("lock"))
            .unwrap();
        lock.lock().unwrap();
        assert!(Vocabulary::add(&path, "another", "value").is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn missing_file_is_empty_but_corrupt_or_ambiguous_files_fail() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("vocabulary.json");
        assert!(Vocabulary::load(&path).unwrap().entries().is_empty());
        for content in [
            "not json",
            r#"{"version":2,"entries":{}}"#,
            r#"{"version":1,"entries":{"NV STT":"one","nv stt":"two"}}"#,
            r#"{"version":1,"entries":{"nv stt":"one","nv stt":"two"}}"#,
        ] {
            fs::write(&path, content).unwrap();
            assert!(Vocabulary::load(&path).is_err());
        }
    }

    #[test]
    fn oversized_serialized_edit_preserves_readable_saved_rules() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("vocabulary.json");
        Vocabulary::add(&path, "nv stt", "nvstt").unwrap();
        let before = fs::read(&path).unwrap();
        let result = Vocabulary::update(&path, |vocabulary| {
            vocabulary.entries = (0..MAX_ENTRIES)
                .map(|index| (format!("phrase {index}"), "\\".repeat(2048)))
                .collect();
            Ok(())
        });
        assert!(result.unwrap_err().to_string().contains("exceeds 2 MiB"));
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(
            Vocabulary::load(&path).unwrap().entries()["nv stt"],
            "nvstt"
        );
    }
}
