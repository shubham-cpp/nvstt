//! Explicit model download and installation.
//!
//! Installation is intentionally separate from daemon startup. The command
//! downloads the pinned ASR archive and optional speech gate into a private
//! staging directory. It activates both only after validation succeeds.

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
    config::{Config, RequiredModelFileSpec},
    error::{AppError, Result},
    model::ModelStatus,
    paths::AppPaths,
};

const VAD_FILE: &str = "silero_vad.onnx";
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
    let result = install_into_staging(config, &staging, &target, &mut progress);
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
    config: &Config,
    staging: &Path,
    target: &Path,
    progress: &mut F,
) -> Result<(u64, bool)>
where
    F: FnMut(DownloadProgress),
{
    let artifact = config.artifact_name();
    let archive_path = staging.join(archive_name(artifact));
    let extracted_path = staging.join("extracted");
    fs::create_dir(&extracted_path)?;
    fs::set_permissions(&extracted_path, fs::Permissions::from_mode(0o700))?;

    let archive_bytes = download_file(
        &archive_path,
        &config.model_download_url(),
        "model archive",
        progress,
    )?;
    extract_archive(&archive_path, &extracted_path, artifact)?;
    let model_root = extracted_path.join(artifact);
    let vad_bytes = if let Some(vad_url) = config.vad_download_url() {
        download_file(&model_root.join(VAD_FILE), vad_url, "speech gate", progress)?
    } else {
        0
    };
    validate_model_files(
        &model_root,
        config.required_model_files(),
        config.speech_gate,
    )?;
    let replaced_existing = activate_model(&model_root, target)?;
    Ok((archive_bytes.saturating_add(vad_bytes), replaced_existing))
}

fn download_file<F>(
    destination: &Path,
    download_url: &str,
    description: &str,
    progress: &mut F,
) -> Result<u64>
where
    F: FnMut(DownloadProgress),
{
    let agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_body(Some(Duration::from_secs(30)))
        .user_agent(concat!("nvstt/", env!("CARGO_PKG_VERSION")))
        .build()
        .new_agent();
    let mut response = agent.get(download_url).call().map_err(|error| {
        AppError::Unavailable(format!("{description} download failed: {error}"))
    })?;
    let total_bytes = response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());

    let file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination)?;
    let mut file = io::BufWriter::new(file);
    let mut reader = response.body_mut().as_reader();
    let mut buffer = [0_u8; DOWNLOAD_BUFFER_SIZE];
    let mut downloaded_bytes = 0_u64;
    let mut next_progress = PROGRESS_STEP;
    progress(DownloadProgress {
        downloaded_bytes,
        total_bytes,
    });

    loop {
        let count = reader.read(&mut buffer).map_err(|error| {
            AppError::Unavailable(format!("could not read {description} download: {error}"))
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
            "{description} download was truncated: expected {total_bytes} bytes, received {downloaded_bytes}"
        )));
    }
    Ok(downloaded_bytes)
}

fn archive_name(artifact: &str) -> String {
    format!("{artifact}.tar.bz2")
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

fn validate_model_files(
    model_root: &Path,
    required_files: &[RequiredModelFileSpec],
    require_vad: bool,
) -> Result<()> {
    for file in required_files {
        if !file
            .alternatives
            .iter()
            .map(|name| model_root.join(name))
            .any(|path| path.is_file())
        {
            return Err(AppError::Unavailable(format!(
                "model archive is missing required {} file: {}",
                file.name,
                model_root.join(file.alternatives[0]).display()
            )));
        }
    }
    if require_vad {
        let path = model_root.join(VAD_FILE);
        if !path.is_file() || fs::metadata(&path)?.len() == 0 {
            return Err(AppError::Unavailable(format!(
                "model installation is missing required speech gate file: {}",
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
    use std::{fs::File, net::TcpListener, thread};

    use bzip2::{Compression, write::BzEncoder};
    use tar::{Builder, Header};
    use tempfile::tempdir;

    use super::*;
    use crate::config::ONLINE_TRANSDUCER_REQUIRED_FILES;

    fn fixture_archive(path: &Path, artifact: &str) {
        let file = File::create(path).expect("archive file");
        let encoder = BzEncoder::new(file, Compression::best());
        let mut builder = Builder::new(encoder);
        builder
            .append_dir(format!("{artifact}/"), ".")
            .expect("model directory");
        for file in ONLINE_TRANSDUCER_REQUIRED_FILES {
            let name = file.alternatives[0];
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

    fn serve_response(response: &'static [u8]) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener");
        let url = format!(
            "http://{}",
            listener.local_addr().expect("listener address")
        );
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("download request");
            let mut request = [0_u8; 1024];
            assert!(stream.read(&mut request).expect("read request") > 0);
            stream.write_all(response).expect("write response");
        });
        (url, server)
    }

    #[test]
    fn download_streams_file_and_reports_progress() {
        let (url, server) =
            serve_response(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc");
        let directory = tempdir().expect("temporary directory");
        let destination = directory.path().join("model.tar.bz2");
        let mut progress = Vec::new();

        let downloaded = download_file(&destination, &url, "model archive", &mut |event| {
            progress.push((event.downloaded_bytes, event.total_bytes));
        })
        .expect("download");
        server.join().expect("server thread");

        assert_eq!(downloaded, 3);
        assert_eq!(fs::read(destination).expect("downloaded file"), b"abc");
        assert_eq!(progress, vec![(0, Some(3)), (3, Some(3))]);
    }

    #[test]
    fn download_rejects_truncated_response() {
        let (url, server) =
            serve_response(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\nabc");
        let directory = tempdir().expect("temporary directory");
        let destination = directory.path().join("model.tar.bz2");

        let result = download_file(&destination, &url, "model archive", &mut |_| {});
        server.join().expect("server thread");

        assert!(result.is_err(), "short response must not be accepted");
    }

    #[test]
    fn extracts_and_validates_the_expected_file_set() {
        let directory = tempdir().expect("temporary directory");
        let archive = directory.path().join(archive_name("artifact"));
        let extracted = directory.path().join("extracted");
        fs::create_dir(&extracted).expect("extracted directory");
        fixture_archive(&archive, "artifact");

        extract_archive(&archive, &extracted, "artifact").expect("extract archive");
        validate_model_files(
            &extracted.join("artifact"),
            &ONLINE_TRANSDUCER_REQUIRED_FILES,
            false,
        )
        .expect("validate model");
    }

    #[test]
    fn rejects_archive_path_escape() {
        let error = validate_archive_path(Path::new("artifact/../escape"), OsStr::new("artifact"))
            .expect_err("parent traversal must be rejected");
        assert!(error.to_string().contains("unsafe path"));
    }

    #[test]
    fn rejects_an_enabled_speech_gate_without_its_asset() {
        let directory = tempdir().expect("temporary directory");
        let model_root = directory.path().join("model");
        fs::create_dir(&model_root).expect("model directory");
        for file in ONLINE_TRANSDUCER_REQUIRED_FILES {
            fs::write(model_root.join(file.alternatives[0]), b"fixture").expect("model file");
        }

        let error = validate_model_files(&model_root, &ONLINE_TRANSDUCER_REQUIRED_FILES, true)
            .expect_err("VAD must be required");
        assert!(error.to_string().contains("speech gate file"));
    }

    #[test]
    fn activation_replaces_a_target_only_after_a_complete_source_exists() {
        let directory = tempdir().expect("temporary directory");
        let source = directory.path().join("source");
        let target = directory.path().join("target");
        fs::create_dir(&source).expect("source directory");
        fs::write(source.join("marker"), b"new").expect("source marker");
        fs::create_dir(&target).expect("target directory");
        fs::write(target.join("marker"), b"old").expect("target marker");

        assert!(activate_model(&source, &target).expect("activate replacement"));
        assert_eq!(
            fs::read(target.join("marker")).expect("target marker"),
            b"new"
        );
    }
}
