//! Discovery and selection affect this application's streams only.
use std::{ffi::CString, io::BufReader, os::unix::net::UnixStream, time::Duration};

use cpal::traits::{DeviceTrait, HostTrait};

use crate::{
    domain::{AudioInput, InputKind, InputPreference},
    error::{AppError, Result},
};

pub struct InputCatalog {
    pub inputs: Vec<AudioInput>,
    pub default_input: Option<AudioInput>,
}

pub trait InputProvider: Send {
    fn catalog(&mut self) -> Result<InputCatalog>;
    fn resolve(&mut self, preference: &InputPreference) -> Result<AudioInput>;
}

#[derive(Default)]
pub struct CpalInputs;

impl InputProvider for CpalInputs {
    fn catalog(&mut self) -> Result<InputCatalog> {
        let host = capture_host()?;
        let monitors = if host.id() == cpal::HostId::PulseAudio {
            monitor_ids()?
        } else {
            Vec::new()
        };
        let mut inputs = host
            .input_devices()
            .map_err(unavailable)?
            .filter_map(|device| {
                let input = describe(&device, &monitors).ok()?;
                // Null is a generator, not a microphone. Keep other ALSA entries
                // explicit as unknown rather than guessing from translated names.
                (!matches!(input.id.as_str(), "alsa:null")).then_some(input)
            })
            .collect::<Vec<_>>();
        inputs.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        let mut seen = std::collections::HashSet::new();
        inputs.retain(|input| seen.insert(input.id.clone()));
        let default_input = host
            .default_input_device()
            .map(|device| describe(&device, &monitors))
            .transpose()?;
        Ok(InputCatalog {
            inputs,
            default_input,
        })
    }

    fn resolve(&mut self, preference: &InputPreference) -> Result<AudioInput> {
        let device = resolve_device(preference)?;
        let monitors = if device.id().map_err(unavailable)?.host() == cpal::HostId::PulseAudio {
            monitor_ids()?
        } else {
            Vec::new()
        };
        describe(&device, &monitors)
    }
}

/// Choose the audio server deliberately. Do not silently bypass a reachable
/// but broken PulseAudio server and open hardware behind it.
fn capture_host() -> Result<cpal::Host> {
    let host_id = if cpal::available_hosts().contains(&cpal::HostId::PulseAudio) {
        cpal::HostId::PulseAudio
    } else {
        cpal::HostId::Alsa
    };
    cpal::host_from_id(host_id).map_err(unavailable)
}

pub(crate) fn resolve_device(preference: &InputPreference) -> Result<cpal::Device> {
    match preference {
        InputPreference::SystemDefault => capture_host()?
            .default_input_device()
            .ok_or_else(|| AppError::Unavailable("no default input is available".to_owned())),
        InputPreference::Device { id } => {
            let parsed: cpal::DeviceId = id
                .parse()
                .map_err(|_| AppError::Config("invalid audio input ID".to_owned()))?;
            if !matches!(parsed.host(), cpal::HostId::PulseAudio | cpal::HostId::Alsa) {
                return Err(AppError::Config("unsupported audio input host".to_owned()));
            }
            let host = cpal::host_from_id(parsed.host()).map_err(unavailable)?;
            let device = host.device_by_id(&parsed).ok_or_else(|| {
                AppError::Unavailable("selected input is no longer available".to_owned())
            })?;
            if !device.supports_input() || id == "alsa:null" {
                return Err(AppError::Unavailable(
                    "device is not a capture input".to_owned(),
                ));
            }
            Ok(device)
        }
    }
}

fn describe(device: &cpal::Device, monitors: &[String]) -> Result<AudioInput> {
    let id = device.id().map_err(unavailable)?.to_string();
    let kind = if monitors.contains(&id) {
        InputKind::Monitor
    } else if id.starts_with("pulseaudio:") {
        InputKind::Source
    } else {
        InputKind::Unknown
    };
    Ok(AudioInput {
        id,
        name: device.description().map_err(unavailable)?.name().to_owned(),
        kind,
    })
}

fn unavailable(error: impl std::fmt::Display) -> AppError {
    AppError::Unavailable(format!("audio input: {error}"))
}

/// CPAL hides PulseAudio source metadata. Reuse its Rust protocol dependency
/// to identify sink monitors without a name heuristic or native libpulse.
/// Blocking socket deadlines also bound authentication and metadata reads.
fn monitor_ids() -> Result<Vec<String>> {
    use pulseaudio::protocol as p;
    let path = pulseaudio::socket_path_from_env()
        .ok_or_else(|| unavailable("audio server socket is missing"))?;
    let socket = UnixStream::connect(path)?;
    socket.set_read_timeout(Some(Duration::from_secs(2)))?;
    socket.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut reader = BufReader::new(socket);
    let cookie = pulseaudio::cookie_path_from_env()
        .and_then(|path| std::fs::read(path).ok())
        .unwrap_or_default();
    p::write_command_message(
        reader.get_mut(),
        0,
        &p::Command::Auth(p::AuthParams {
            version: p::MAX_VERSION,
            supports_shm: false,
            supports_memfd: false,
            cookie,
        }),
        p::MAX_VERSION,
    )
    .map_err(unavailable)?;
    let (_, auth) =
        p::read_reply_message::<p::AuthReply>(&mut reader, p::MAX_VERSION).map_err(unavailable)?;
    let version = auth.version.min(p::MAX_VERSION);
    let mut props = p::Props::new();
    props.set(
        p::Prop::ApplicationName,
        CString::new("nvstt input catalog").expect("literal"),
    );
    p::write_command_message(
        reader.get_mut(),
        1,
        &p::Command::SetClientName(props),
        version,
    )
    .map_err(unavailable)?;
    p::read_reply_message::<p::SetClientNameReply>(&mut reader, version).map_err(unavailable)?;
    p::write_command_message(reader.get_mut(), 2, &p::Command::GetSourceInfoList, version)
        .map_err(unavailable)?;
    let (_, sources) =
        p::read_reply_message::<p::SourceInfoList>(&mut reader, version).map_err(unavailable)?;
    Ok(sources
        .into_iter()
        .filter(|source| source.monitor_of_sink_index.is_some())
        .map(|source| format!("pulseaudio:{}", source.name.to_string_lossy()))
        .collect())
}

pub(crate) struct DevelopmentInputs;

impl InputProvider for DevelopmentInputs {
    fn catalog(&mut self) -> Result<InputCatalog> {
        Ok(InputCatalog {
            inputs: vec![development_input()],
            default_input: Some(development_input()),
        })
    }

    fn resolve(&mut self, preference: &InputPreference) -> Result<AudioInput> {
        match preference {
            InputPreference::SystemDefault => Ok(development_input()),
            InputPreference::Device { id } if id == "development:input" => Ok(development_input()),
            _ => Err(unavailable("development input unavailable")),
        }
    }
}

fn development_input() -> AudioInput {
    AudioInput {
        id: "development:input".to_owned(),
        name: "Development input".to_owned(),
        kind: InputKind::Source,
    }
}
