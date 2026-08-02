//! Explicit model download and installation.
//!
//! Installation is intentionally separate from daemon startup. The command
//! downloads the pinned Parakeet archive into a private staging directory,
//! validates its file set, and activates it only after extraction succeeds.

use std::{
    env,
    ffi::OsStr,
    fs,
    io::{self, Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use bzip2::read::BzDecoder;
use tar::Archive;

use crate::{
    config::Config,
    error::{AppError, Result},
    model::ModelStatus,
    paths::AppPaths,
};

pub const MODEL_ARCHIVE_NAME: &str =
    "sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-streaming-560ms.tar.bz2";
pub const MODEL_DOWNLOAD_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-streaming-560ms.tar.bz2";

const REQUIRED_FILES: [&str; 4] = [
    "encoder.int8.onnx",
    "decoder.int8.onnx",
    "joiner.int8.onnx",
    "tokens.txt",
];
const DOWNLOAD_BUFFER_SIZE: usize = 128 * 1024;
const PROGRESS_STEP: u64 = 10 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct DownloadProgress {
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct InstallReport {
    pub path: PathBuf,
    pub downloaded_bytes: u64,
    pub already_ready: bool,
    pub replaced_existing: bool,
}

impl InstallReport {
    pub fn message(&self) -> String {
        if self.already_ready {
            return format!("model already installed at {}", self.path.display());
        }

        let size_mib = self.downloaded_bytes as f64 / (1024.0 * 1024.0);
        let replacement = if self.replaced_existing {
            "; replaced incomplete installation"
        } else {
            ""
        };
        format!(
            "model installed at {} ({size_mib:.1} MiB downloaded{replacement})",
            self.path.display()
        )
    }
}

/// Download and install the configured model.
pub fn install_model<F>(config: &Config, paths: &AppPaths, mut progress: F) -> Result<InstallReport>
where
    F: FnMut(DownloadProgress),
{
    config.validate()?;
    let status = ModelStatus::inspect(config, paths);
    if status.ready {
        return Ok(InstallReport {
            path: status.path,
            downloaded_bytes: 0,
            already_ready: true,
            replaced_existing: false,
        });
    }

    paths.create_user_dirs()?;
    let target = paths.model_dir.join(config.artifact_name());
    let staging = create_staging_dir(&paths.model_dir, config.artifact_name())?;
    let result = install_into_staging(config.artifact_name(), &staging, &target, &mut progress);
    let cleanup_result = remove_path(&staging);

    match result {
        Ok((downloaded_bytes, replaced_existing)) => {
            cleanup_result?;
            Ok(InstallReport {
                path: target,
                downloaded_bytes,
                already_ready: false,
                replaced_existing,
            })
        }
        Err(error) => {
            let _ = cleanup_result;
            Err(error)
        }
    }
}

fn install_into_staging<F>(
    artifact: &str,
    staging: &Path,
    target: &Path,
    progress: &mut F,
) -> Result<(u64, bool)>
where
    F: FnMut(DownloadProgress),
{
    let archive_path = staging.join(MODEL_ARCHIVE_NAME);
    let extracted_path = staging.join("extracted");
    fs::create_dir(&extracted_path)?;
    fs::set_permissions(&extracted_path, fs::Permissions::from_mode(0o700))?;

    let downloaded_bytes = download_archive(&archive_path, progress)?;
    extract_archive(&archive_path, &extracted_path, artifact)?;
    let model_root = extracted_path.join(artifact);
    validate_model_files(&model_root)?;
    let replaced_existing = activate_model(&model_root, target)?;
    Ok((downloaded_bytes, replaced_existing))
}

fn download_archive<F>(destination: &Path, progress: &mut F) -> Result<u64>
where
    F: FnMut(DownloadProgress),
{
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(30))
        .timeout_read(Duration::from_secs(30))
        .user_agent(concat!("nvstt/", env!("CARGO_PKG_VERSION")))
        .build();
    let response = agent
        .get(MODEL_DOWNLOAD_URL)
        .call()
        .map_err(|error| AppError::Unavailable(format!("model download failed: {error}")))?;
    let total_bytes = response
        .header("content-length")
        .and_then(|value| value.parse::<u64>().ok());

    let file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination)?;
    let mut file = io::BufWriter::new(file);
    let mut reader = response.into_reader();
    let mut buffer = [0_u8; DOWNLOAD_BUFFER_SIZE];
    let mut downloaded_bytes = 0_u64;
    let mut next_progress = PROGRESS_STEP;
    progress(DownloadProgress {
        downloaded_bytes,
        total_bytes,
    });

    loop {
        let count = reader.read(&mut buffer).map_err(|error| {
            AppError::Unavailable(format!("could not read model download: {error}"))
        })?;
        if count == 0 {
            break;
        }
        file.write_all(&buffer[..count])?;
        downloaded_bytes = downloaded_bytes.saturating_add(count as u64);
        if downloaded_bytes >= next_progress {
            progress(DownloadProgress {
                downloaded_bytes,
                total_bytes,
            });
            next_progress = downloaded_bytes.saturating_add(PROGRESS_STEP);
        }
    }
    file.flush()?;
    file.get_ref().sync_all()?;
    progress(DownloadProgress {
        downloaded_bytes,
        total_bytes,
    });

    if let Some(total_bytes) = total_bytes
        && total_bytes != downloaded_bytes
    {
        return Err(AppError::Unavailable(format!(
            "model download was truncated: expected {total_bytes} bytes, received {downloaded_bytes}"
        )));
    }
    Ok(downloaded_bytes)
}

fn extract_archive(archive_path: &Path, destination: &Path, artifact: &str) -> Result<()> {
    let archive_file = fs::File::open(archive_path)?;
    let decoder = BzDecoder::new(archive_file);
    let mut archive = Archive::new(decoder);
    let expected_root = OsStr::new(artifact);
    let mut saw_file = false;

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        validate_archive_path(&path, expected_root)?;
        let entry_type = entry.header().entry_type();
        if !entry_type.is_file() && !entry_type.is_dir() {
            return Err(AppError::Unavailable(format!(
                "model archive contains unsupported entry: {}",
                path.display()
            )));
        }
        saw_file |= entry_type.is_file();
        entry.unpack_in(destination)?;
    }

    if !saw_file {
        return Err(AppError::Unavailable(
            "model archive did not contain any files".to_owned(),
        ));
    }
    Ok(())
}

fn validate_archive_path(path: &Path, expected_root: &OsStr) -> Result<()> {
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(AppError::Unavailable(format!(
            "model archive contains an unsafe path: {}",
            path.display()
        )));
    }

    match path.components().next() {
        Some(Component::Normal(root)) if root == expected_root => Ok(()),
        _ => Err(AppError::Unavailable(format!(
            "model archive entry is outside the expected model directory: {}",
            path.display()
        ))),
    }
}

fn validate_model_files(model_root: &Path) -> Result<()> {
    for file in REQUIRED_FILES {
        let path = model_root.join(file);
        if !path.is_file() {
            return Err(AppError::Unavailable(format!(
                "model archive is missing required file: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn activate_model(source: &Path, target: &Path) -> Result<bool> {
    let parent = target.parent().ok_or_else(|| {
        AppError::Unavailable("model target path has no parent directory".to_owned())
    })?;
    let replaced_existing = fs::symlink_metadata(target).is_ok();
    if !replaced_existing {
        fs::rename(source, target)?;
        return Ok(false);
    }

    let backup = unique_path(parent, ".nvstt-model-backup");
    fs::rename(target, &backup)?;
    match fs::rename(source, target) {
        Ok(()) => {
            let _ = remove_path(&backup);
            Ok(true)
        }
        Err(error) => {
            let _ = fs::rename(&backup, target);
            Err(error.into())
        }
    }
}

fn create_staging_dir(parent: &Path, artifact: &str) -> Result<PathBuf> {
    for _ in 0..100 {
        let candidate = unique_path(parent, &format!(".{artifact}.install"));
        match fs::create_dir(&candidate) {
            Ok(()) => {
                fs::set_permissions(&candidate, fs::Permissions::from_mode(0o700))?;
                return Ok(candidate);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(AppError::Unavailable(
        "could not create a unique model staging directory".to_owned(),
    ))
}

fn unique_path(parent: &Path, prefix: &str) -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    parent.join(format!("{prefix}-{}-{timestamp}", std::process::id()))
}

fn remove_path(path: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use bzip2::{Compression, write::BzEncoder};
    use tar::{Builder, Header};
    use tempfile::tempdir;

    use super::*;

    fn fixture_archive(path: &Path, artifact: &str) {
        let file = File::create(path).expect("archive file");
        let encoder = BzEncoder::new(file, Compression::best());
        let mut builder = Builder::new(encoder);
        builder
            .append_dir(format!("{artifact}/"), ".")
            .expect("model directory");
        for name in REQUIRED_FILES {
            let contents = b"fixture";
            let mut header = Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o600);
            header.set_cksum();
            builder
                .append_data(&mut header, format!("{artifact}/{name}"), &contents[..])
                .expect("model file");
        }
        builder
            .into_inner()
            .expect("finish tar")
            .finish()
            .expect("finish bzip2");
    }

    #[test]
    fn extracts_and_validates_the_expected_file_set() {
        let directory = tempdir().expect("temporary directory");
        let archive = directory.path().join(MODEL_ARCHIVE_NAME);
        let extracted = directory.path().join("extracted");
        fs::create_dir(&extracted).expect("extracted directory");
        fixture_archive(&archive, "artifact");

        extract_archive(&archive, &extracted, "artifact").expect("extract archive");
        validate_model_files(&extracted.join("artifact")).expect("validate model");
    }

    #[test]
    fn rejects_archive_path_escape() {
        let error = validate_archive_path(Path::new("artifact/../escape"), OsStr::new("artifact"))
            .expect_err("parent traversal must be rejected");
        assert!(error.to_string().contains("unsafe path"));
    }
}
