use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    env, fs,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    net::{TcpListener, UdpSocket},
    sync::broadcast,
    time,
};
use tower_http::{services::ServeDir, trace::TraceLayer};
use tracing::{error, info, warn};

const DEFAULT_SETTINGS_PATH: &str = "lpudp.toml";
const DEFAULT_HTTP_ADDR: &str = "127.0.0.1:8080";
const DEFAULT_UDP_ADDR: &str = "0.0.0.0:8888";
const DEFAULT_SAMPLE_RATE: f64 = 100.0;
const MAX_PACKET_BYTES: usize = 8192;
const MAX_DISPLAY_POINTS: usize = 1600;

type Shared<T> = Arc<RwLock<T>>;

#[derive(Clone)]
struct AppState {
    runtime: Shared<RuntimeState>,
    settings: Shared<Settings>,
    settings_path: Arc<PathBuf>,
    admin_token: Option<Arc<String>>,
    events: broadcast::Sender<ServerEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
struct Settings {
    station: StationSettings,
    display: DisplaySettings,
    alert: AlertSettings,
    archive: ArchiveSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct StationSettings {
    network: String,
    station: String,
    location: String,
    sample_rate_hz: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct DisplaySettings {
    window_seconds: u32,
    refresh_hz: u32,
    channels: Vec<String>,
    spectrogram: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct AlertSettings {
    enabled: bool,
    channel: String,
    sta_seconds: f64,
    lta_seconds: f64,
    threshold: f64,
    reset: f64,
    minimum_duration_seconds: f64,
    sound_enabled: bool,
    sound_file: String,
    screenshot_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct ArchiveSettings {
    enabled: bool,
    source: String,
    directory: String,
    mini_seed_version: u8,
    encoding: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct SettingsPatch {
    alert: Option<AlertSettings>,
    archive: Option<ArchiveSettings>,
}

impl Default for StationSettings {
    fn default() -> Self {
        Self {
            network: "AM".into(),
            station: "Z0000".into(),
            location: "00".into(),
            sample_rate_hz: DEFAULT_SAMPLE_RATE,
        }
    }
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            window_seconds: 90,
            refresh_hz: 10,
            channels: vec!["EHZ".into(), "EHE".into(), "EHN".into(), "ENZ".into()],
            spectrogram: true,
        }
    }
}

impl Default for AlertSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            channel: "EHZ".into(),
            sta_seconds: 6.0,
            lta_seconds: 30.0,
            threshold: 3.95,
            reset: 0.9,
            minimum_duration_seconds: 0.0,
            sound_enabled: true,
            sound_file: "alert.wav".into(),
            screenshot_enabled: true,
        }
    }
}

impl Default for ArchiveSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            source: "seedlink".into(),
            directory: "archive".into(),
            mini_seed_version: 2,
            encoding: "STEIM2".into(),
        }
    }
}

#[derive(Debug, Default)]
struct RuntimeState {
    started_at_ms: u64,
    packets_received: u64,
    packets_invalid: u64,
    bytes_received: u64,
    last_packet_at_ms: Option<u64>,
    source_ip: Option<String>,
    demo: bool,
    channels: BTreeMap<String, ChannelBuffer>,
    alert: AlertRuntime,
}

#[derive(Debug)]
struct ChannelBuffer {
    sample_rate_hz: f64,
    samples: VecDeque<i32>,
    max_samples: usize,
    last_start_ms: Option<u64>,
    last_sample_count: usize,
    packets: u64,
    gaps: u64,
}

impl ChannelBuffer {
    fn new(sample_rate_hz: f64) -> Self {
        let max_samples = (sample_rate_hz * 600.0).max(1000.0) as usize;
        Self {
            sample_rate_hz,
            samples: VecDeque::with_capacity(max_samples),
            max_samples,
            last_start_ms: None,
            last_sample_count: 0,
            packets: 0,
            gaps: 0,
        }
    }

    fn append(&mut self, start_ms: u64, samples: &[i32]) {
        if let Some(previous_start_ms) = self.last_start_ms {
            let expected_ms = previous_start_ms
                + ((self.last_sample_count.max(1) as f64 / self.sample_rate_hz) * 1000.0) as u64;
            if start_ms > expected_ms + 20 {
                self.gaps = self.gaps.saturating_add(1);
            }
        }
        for sample in samples {
            self.samples.push_back(*sample);
        }
        while self.samples.len() > self.max_samples {
            self.samples.pop_front();
        }
        self.last_start_ms = Some(start_ms);
        self.last_sample_count = samples.len();
        self.packets = self.packets.saturating_add(1);
    }
}

#[derive(Debug, Default)]
struct AlertRuntime {
    active: bool,
    current_ratio: f64,
    peak_ratio: f64,
    last_event_ms: Option<u64>,
    config_key: Option<String>,
    sta_values: VecDeque<f64>,
    lta_values: VecDeque<f64>,
    sta_sum: f64,
    lta_sum: f64,
    above_samples: usize,
}

#[derive(Debug, Clone)]
struct WaveBlock {
    channel: String,
    start_ms: u64,
    sample_rate_hz: f64,
    samples: Vec<i32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
enum ServerEvent {
    Alarm {
        event_time_ms: u64,
        channel: String,
        ratio: f64,
    },
    Reset {
        event_time_ms: u64,
        channel: String,
        peak_ratio: f64,
    },
}

#[derive(Debug, Serialize)]
struct StatusResponse {
    started_at_ms: u64,
    packets_received: u64,
    packets_invalid: u64,
    bytes_received: u64,
    last_packet_at_ms: Option<u64>,
    source_ip: Option<String>,
    demo: bool,
    alert_active: bool,
    current_ratio: f64,
    peak_ratio: f64,
    channels: Vec<ChannelStatus>,
}

#[derive(Debug, Serialize)]
struct ChannelStatus {
    channel: String,
    sample_rate_hz: f64,
    sample_count: usize,
    packet_count: u64,
    gap_count: u64,
}

#[derive(Debug, Serialize, Clone)]
struct ChannelSnapshot {
    channel: String,
    sample_rate_hz: f64,
    start_time_ms: Option<u64>,
    samples: Vec<i32>,
    packet_count: u64,
    gap_count: u64,
}

#[derive(Debug, Serialize)]
struct LiveSnapshot {
    r#type: &'static str,
    server_time_ms: u64,
    display_window_seconds: u32,
    status: StatusResponse,
    channels: Vec<ChannelSnapshot>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn load_settings(path: &Path) -> Settings {
    match fs::read_to_string(path) {
        Ok(text) => match toml::from_str::<Settings>(&text) {
            Ok(settings) => settings,
            Err(error) => {
                warn!(?error, "Could not parse settings file. Using defaults.");
                Settings::default()
            }
        },
        Err(_) => Settings::default(),
    }
}

fn save_settings(path: &Path, settings: &Settings) -> Result<(), String> {
    let text = toml::to_string_pretty(settings).map_err(|error| error.to_string())?;
    let temporary = path.with_extension("toml.tmp");
    fs::write(&temporary, text).map_err(|error| error.to_string())?;
    fs::rename(&temporary, path).map_err(|error| error.to_string())
}

fn parse_datagram(bytes: &[u8], sample_rate_hz: f64) -> Result<WaveBlock, String> {
    let text = std::str::from_utf8(bytes).map_err(|error| format!("invalid UTF-8: {error}"))?;
    let body = text
        .trim()
        .strip_prefix('{')
        .and_then(|value| value.strip_suffix('}'))
        .ok_or_else(|| "packet has no surrounding braces".to_string())?;
    let mut fields = body.split(',');
    let channel = fields
        .next()
        .map(str::trim)
        .map(|value| value.trim_matches(['\'', '"']))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "packet has no channel".to_string())?
        .to_ascii_uppercase();
    if !matches!(
        channel.as_str(),
        "SHZ" | "EHZ" | "EHE" | "EHN" | "ENZ" | "ENE" | "ENN" | "HDF"
    ) {
        return Err(format!("unsupported channel {channel}"));
    }
    let timestamp_seconds = fields
        .next()
        .ok_or_else(|| "packet has no timestamp".to_string())?
        .trim()
        .parse::<f64>()
        .map_err(|error| format!("invalid timestamp: {error}"))?;
    if !timestamp_seconds.is_finite() || timestamp_seconds < 0.0 {
        return Err("timestamp is not a finite positive value".to_string());
    }
    let samples = fields
        .map(|field| {
            field
                .trim()
                .parse::<i32>()
                .map_err(|error| format!("invalid sample: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if samples.is_empty() {
        return Err("packet contains no samples".to_string());
    }
    Ok(WaveBlock {
        channel,
        start_ms: (timestamp_seconds * 1000.0).round() as u64,
        sample_rate_hz,
        samples,
    })
}

fn current_status(runtime: &RuntimeState) -> StatusResponse {
    StatusResponse {
        started_at_ms: runtime.started_at_ms,
        packets_received: runtime.packets_received,
        packets_invalid: runtime.packets_invalid,
        bytes_received: runtime.bytes_received,
        last_packet_at_ms: runtime.last_packet_at_ms,
        source_ip: runtime.source_ip.clone(),
        demo: runtime.demo,
        alert_active: runtime.alert.active,
        current_ratio: runtime.alert.current_ratio,
        peak_ratio: runtime.alert.peak_ratio,
        channels: runtime
            .channels
            .iter()
            .map(|(channel, buffer)| ChannelStatus {
                channel: channel.clone(),
                sample_rate_hz: buffer.sample_rate_hz,
                sample_count: buffer.samples.len(),
                packet_count: buffer.packets,
                gap_count: buffer.gaps,
            })
            .collect(),
    }
}

fn retained_start_time_ms(buffer: &ChannelBuffer) -> Option<u64> {
    let last_start_ms = buffer.last_start_ms?;
    let last_packet_duration_ms =
        (buffer.last_sample_count as f64 / buffer.sample_rate_hz * 1000.0).round() as u64;
    let retained_duration_ms =
        (buffer.samples.len() as f64 / buffer.sample_rate_hz * 1000.0).round() as u64;
    Some(
        last_start_ms
            .saturating_add(last_packet_duration_ms)
            .saturating_sub(retained_duration_ms),
    )
}

fn display_samples(buffer: &ChannelBuffer, display: &DisplaySettings) -> Vec<i32> {
    let requested = (display.window_seconds as f64 * buffer.sample_rate_hz).round() as usize;
    let start = buffer.samples.len().saturating_sub(requested.max(1));
    let values = buffer
        .samples
        .iter()
        .skip(start)
        .copied()
        .collect::<Vec<_>>();
    if values.len() <= MAX_DISPLAY_POINTS {
        return values;
    }

    let bucket_count = MAX_DISPLAY_POINTS.div_ceil(2);
    let mut reduced = Vec::with_capacity(MAX_DISPLAY_POINTS);
    for bucket in 0..bucket_count {
        let bucket_start = bucket * values.len() / bucket_count;
        let bucket_end = ((bucket + 1) * values.len() / bucket_count).max(bucket_start + 1);
        let slice = &values[bucket_start..bucket_end.min(values.len())];
        let (min_index, min_value) = slice
            .iter()
            .enumerate()
            .min_by_key(|(_, value)| **value)
            .expect("display bucket must contain a sample");
        let (max_index, max_value) = slice
            .iter()
            .enumerate()
            .max_by_key(|(_, value)| **value)
            .expect("display bucket must contain a sample");
        if min_index <= max_index {
            reduced.push(*min_value);
            reduced.push(*max_value);
        } else {
            reduced.push(*max_value);
            reduced.push(*min_value);
        }
    }
    reduced.truncate(MAX_DISPLAY_POINTS);
    reduced
}

fn snapshot(runtime: &RuntimeState, settings: &Settings) -> LiveSnapshot {
    LiveSnapshot {
        r#type: "snapshot",
        server_time_ms: now_ms(),
        display_window_seconds: settings.display.window_seconds,
        status: current_status(runtime),
        channels: runtime
            .channels
            .iter()
            .map(|(channel, buffer)| ChannelSnapshot {
                channel: channel.clone(),
                sample_rate_hz: buffer.sample_rate_hz,
                start_time_ms: retained_start_time_ms(buffer),
                samples: display_samples(buffer, &settings.display),
                packet_count: buffer.packets,
                gap_count: buffer.gaps,
            })
            .collect(),
    }
}

fn window_push(values: &mut VecDeque<f64>, sum: &mut f64, capacity: usize, value: f64) {
    values.push_back(value);
    *sum += value;
    while values.len() > capacity.max(1) {
        if let Some(removed) = values.pop_front() {
            *sum -= removed;
        }
    }
}

fn detect_alert(
    runtime: &mut RuntimeState,
    settings: &Settings,
    block: &WaveBlock,
) -> Option<ServerEvent> {
    let config = &settings.alert;
    if !config.enabled || block.channel != config.channel {
        return None;
    }
    let key = format!(
        "{}:{}:{}:{}:{}",
        config.channel, config.sta_seconds, config.lta_seconds, config.threshold, config.reset
    );
    if runtime.alert.config_key.as_deref() != Some(key.as_str()) {
        runtime.alert = AlertRuntime {
            config_key: Some(key),
            ..AlertRuntime::default()
        };
    }
    let sta_capacity = (config.sta_seconds * block.sample_rate_hz).round() as usize;
    let lta_capacity = (config.lta_seconds * block.sample_rate_hz).round() as usize;
    let mut event = None;
    for sample in &block.samples {
        let value = (*sample as f64).abs();
        window_push(
            &mut runtime.alert.sta_values,
            &mut runtime.alert.sta_sum,
            sta_capacity,
            value,
        );
        window_push(
            &mut runtime.alert.lta_values,
            &mut runtime.alert.lta_sum,
            lta_capacity,
            value,
        );
        if runtime.alert.lta_values.len() < lta_capacity.max(1) {
            continue;
        }
        let sta = runtime.alert.sta_sum / runtime.alert.sta_values.len() as f64;
        let lta = runtime.alert.lta_sum / runtime.alert.lta_values.len() as f64;
        let ratio = if lta > f64::EPSILON { sta / lta } else { 0.0 };
        runtime.alert.current_ratio = ratio;
        runtime.alert.peak_ratio = runtime.alert.peak_ratio.max(ratio);
        if !runtime.alert.active && ratio >= config.threshold {
            runtime.alert.above_samples = runtime.alert.above_samples.saturating_add(1);
            let duration_samples =
                (config.minimum_duration_seconds * block.sample_rate_hz).round() as usize;
            if config.minimum_duration_seconds == 0.0
                || runtime.alert.above_samples >= duration_samples.max(1)
            {
                runtime.alert.active = true;
                runtime.alert.last_event_ms = Some(block.start_ms);
                event = Some(ServerEvent::Alarm {
                    event_time_ms: block.start_ms,
                    channel: block.channel.clone(),
                    ratio,
                });
            }
        } else if ratio < config.threshold {
            runtime.alert.above_samples = 0;
        }
        if runtime.alert.active && ratio <= config.reset {
            runtime.alert.active = false;
            event = Some(ServerEvent::Reset {
                event_time_ms: block.start_ms,
                channel: block.channel.clone(),
                peak_ratio: runtime.alert.peak_ratio,
            });
            runtime.alert.peak_ratio = 0.0;
        }
    }
    event
}

fn validate_settings(settings: &Settings) -> Result<(), String> {
    let alert = &settings.alert;
    if !matches!(
        alert.channel.as_str(),
        "SHZ" | "EHZ" | "EHE" | "EHN" | "ENZ" | "ENE" | "ENN" | "HDF"
    ) {
        return Err("Alert channel is not supported.".into());
    }
    if alert.sta_seconds <= 0.0 {
        return Err("STA duration must be greater than zero.".into());
    }
    if alert.lta_seconds <= alert.sta_seconds {
        return Err("LTA duration must be greater than STA duration.".into());
    }
    if alert.threshold <= 0.0 {
        return Err("Alert threshold must be greater than zero.".into());
    }
    if alert.reset < 0.0 || alert.reset >= alert.threshold {
        return Err("Reset ratio must be non-negative and lower than the alert threshold.".into());
    }
    if alert.minimum_duration_seconds < 0.0 {
        return Err("Minimum alert duration cannot be negative.".into());
    }
    if settings.station.sample_rate_hz <= 0.0 {
        return Err("Sample rate must be greater than zero.".into());
    }
    if settings.display.window_seconds == 0 || settings.display.refresh_hz == 0 {
        return Err("Display window and refresh rate must be greater than zero.".into());
    }
    Ok(())
}

async fn accept_block(
    state: &AppState,
    block: WaveBlock,
    source_ip: Option<String>,
    byte_count: usize,
) {
    let settings = state.settings.read().unwrap().clone();
    let event = {
        let mut runtime = state.runtime.write().unwrap();
        runtime.packets_received = runtime.packets_received.saturating_add(1);
        runtime.bytes_received = runtime.bytes_received.saturating_add(byte_count as u64);
        runtime.last_packet_at_ms = Some(now_ms());
        if runtime.source_ip.is_none() {
            runtime.source_ip = source_ip;
        }
        let buffer = runtime
            .channels
            .entry(block.channel.clone())
            .or_insert_with(|| ChannelBuffer::new(block.sample_rate_hz));
        buffer.append(block.start_ms, &block.samples);
        detect_alert(&mut runtime, &settings, &block)
    };
    if let Some(event) = event {
        if let Err(error) = state.events.send(event.clone()) {
            warn!(?error, "No browser clients received the alert event.");
        }
        if let ServerEvent::Alarm { .. } = event {
            if settings.alert.sound_enabled {
                play_alert_sound(&settings.alert.sound_file);
            }
        }
    }
}

fn play_alert_sound(path: &str) {
    if path.is_empty() {
        return;
    }
    match std::process::Command::new("paplay").arg(path).spawn() {
        Ok(_) => info!(path, "Started alert sound."),
        Err(error) => warn!(
            ?error,
            path, "Could not start alert sound. Use a valid WAV file and PipeWire or PulseAudio."
        ),
    }
}

async fn udp_ingest(state: AppState, socket: UdpSocket) {
    let mut bytes = [0_u8; MAX_PACKET_BYTES];
    loop {
        match socket.recv_from(&mut bytes).await {
            Ok((length, address)) => {
                let sample_rate_hz = state.settings.read().unwrap().station.sample_rate_hz;
                match parse_datagram(&bytes[..length], sample_rate_hz) {
                    Ok(block) => {
                        accept_block(&state, block, Some(address.ip().to_string()), length).await
                    }
                    Err(error) => {
                        state.runtime.write().unwrap().packets_invalid += 1;
                        warn!(?error, "Rejected UDP packet.");
                    }
                }
            }
            Err(error) => {
                error!(?error, "UDP receive error.");
                time::sleep(Duration::from_millis(250)).await;
            }
        }
    }
}

async fn demo_ingest(state: AppState) {
    let mut tick = time::interval(Duration::from_millis(250));
    let channels = ["EHZ", "EHE", "EHN", "ENZ"];
    let mut sequence = 0_u64;
    loop {
        tick.tick().await;
        let start_ms = now_ms();
        for (channel_index, channel) in channels.iter().enumerate() {
            let samples = (0..25)
                .map(|index| {
                    let phase = (sequence * 25 + index) as f64 / 100.0;
                    let signal = (phase * (1.0 + channel_index as f64 * 0.17)).sin() * 180.0;
                    let pulse = if sequence % 80 > 56 {
                        (phase * 14.0).sin() * 1800.0
                    } else {
                        0.0
                    };
                    (signal + pulse) as i32
                })
                .collect::<Vec<_>>();
            accept_block(
                &state,
                WaveBlock {
                    channel: (*channel).into(),
                    start_ms,
                    sample_rate_hz: DEFAULT_SAMPLE_RATE,
                    samples,
                },
                Some("demo".into()),
                0,
            )
            .await;
        }
        sequence = sequence.saturating_add(1);
    }
}

async fn status(State(state): State<AppState>) -> Json<StatusResponse> {
    Json(current_status(&state.runtime.read().unwrap()))
}

async fn get_settings(State(state): State<AppState>) -> Json<Settings> {
    Json(state.settings.read().unwrap().clone())
}

fn is_authorized(headers: &HeaderMap, state: &AppState) -> bool {
    match &state.admin_token {
        None => true,
        Some(expected) => headers
            .get("x-admin-token")
            .and_then(|value| value.to_str().ok())
            .map(|value| value == expected.as_str())
            .unwrap_or(false),
    }
}

async fn put_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(patch): Json<SettingsPatch>,
) -> Response {
    if !is_authorized(&headers, &state) {
        return (
            StatusCode::UNAUTHORIZED,
            "An administrator token is required.",
        )
            .into_response();
    }
    let updated = {
        let settings = state.settings.read().unwrap();
        let mut updated = settings.clone();
        if let Some(alert) = patch.alert {
            updated.alert = alert;
        }
        if let Some(archive) = patch.archive {
            updated.archive = archive;
        }
        updated
    };
    if let Err(error) = validate_settings(&updated) {
        return (StatusCode::BAD_REQUEST, error).into_response();
    }
    if let Err(error) = save_settings(&state.settings_path, &updated) {
        error!(?error, "Could not save settings.");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not save settings.",
        )
            .into_response();
    }
    {
        let mut settings = state.settings.write().unwrap();
        *settings = updated.clone();
    }
    Json(updated).into_response()
}

async fn websocket(State(state): State<AppState>, upgrade: WebSocketUpgrade) -> impl IntoResponse {
    upgrade.on_upgrade(move |socket| websocket_session(socket, state))
}

async fn websocket_session(socket: WebSocket, state: AppState) {
    let mut events = state.events.subscribe();
    let refresh_hz = state.settings.read().unwrap().display.refresh_hz.max(1);
    let mut interval = time::interval(Duration::from_millis(1000 / refresh_hz as u64));
    let (mut sender, mut receiver) = socket.split();
    loop {
        tokio::select! {
            _ = interval.tick() => {
                let settings = state.settings.read().unwrap().clone();
                let value = serde_json::to_string(&snapshot(&state.runtime.read().unwrap(), &settings));
                match value {
                    Ok(value) => {
                        if sender.send(Message::Text(value.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        warn!(?error, "Could not encode live snapshot.");
                        break;
                    }
                }
            }
            event = events.recv() => {
                match event {
                    Ok(event) => match serde_json::to_string(&event) {
                        Ok(value) => {
                            if sender.send(Message::Text(value.into())).await.is_err() {
                                break;
                            }
                        }
                        Err(error) => warn!(?error, "Could not encode alert event."),
                    },
                    Err(broadcast::error::RecvError::Lagged(count)) => warn!(count, "Browser client missed alert events."),
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            message = receiver.next() => {
                match message {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(value))) => {
                        if sender.send(Message::Pong(value)).await.is_err() { break; }
                    }
                    Some(Ok(Message::Text(_))) | Some(Ok(Message::Binary(_))) | Some(Ok(Message::Pong(_))) => {}
                    Some(Err(_)) => break,
                }
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let settings_path =
        PathBuf::from(env::var("LPUDP_SETTINGS").unwrap_or_else(|_| DEFAULT_SETTINGS_PATH.into()));
    let settings = load_settings(&settings_path);
    let udp_addr = env::var("LPUDP_UDP_ADDR").unwrap_or_else(|_| DEFAULT_UDP_ADDR.into());
    let http_addr = env::var("LPUDP_HTTP_ADDR").unwrap_or_else(|_| DEFAULT_HTTP_ADDR.into());
    let demo = env::args().any(|argument| argument == "--demo");
    let admin_token = env::var("LPUDP_ADMIN_TOKEN")
        .ok()
        .filter(|value| !value.is_empty())
        .map(Arc::new);
    let (events, _) = broadcast::channel(64);
    let state = AppState {
        runtime: Arc::new(RwLock::new(RuntimeState {
            started_at_ms: now_ms(),
            demo,
            ..RuntimeState::default()
        })),
        settings: Arc::new(RwLock::new(settings)),
        settings_path: Arc::new(settings_path),
        admin_token,
        events,
    };

    if demo {
        info!("Starting demo data source.");
        tokio::spawn(demo_ingest(state.clone()));
    } else {
        let socket = UdpSocket::bind(&udp_addr).await?;
        info!(%udp_addr, "Listening for Raspberry Shake UDP data.");
        tokio::spawn(udp_ingest(state.clone(), socket));
    }

    let app = Router::new()
        .route("/api/v1/status", get(status))
        .route("/api/v1/settings", get(get_settings).put(put_settings))
        .route("/api/v1/live", get(websocket))
        .fallback_service(ServeDir::new("web").append_index_html_on_directories(true))
        .layer(TraceLayer::new_for_http())
        .with_state(state);
    let listener = TcpListener::bind(&http_addr).await?;
    info!(%http_addr, "Serving the browser viewer.");
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_raspberry_shake_datagram() {
        let block = parse_datagram(b"{'EHZ', 1700000000.125, 1, -2, 3}", 100.0)
            .expect("valid Raspberry Shake packet");
        assert_eq!(block.channel, "EHZ");
        assert_eq!(block.start_ms, 1_700_000_000_125);
        assert_eq!(block.samples, vec![1, -2, 3]);
    }

    #[test]
    fn rejects_unknown_or_empty_datagrams() {
        assert!(parse_datagram(b"{'XYZ', 1700000000.0, 1}", 100.0).is_err());
        assert!(parse_datagram(b"{'EHZ', 1700000000.0}", 100.0).is_err());
    }

    #[test]
    fn counts_gaps_from_packet_length() {
        let mut buffer = ChannelBuffer::new(100.0);
        buffer.append(0, &[1; 25]);
        buffer.append(250, &[1; 25]);
        assert_eq!(buffer.gaps, 0);
        buffer.append(600, &[1; 25]);
        assert_eq!(buffer.gaps, 1);
    }

    #[test]
    fn keeps_display_payload_bounded_and_preserves_extrema() {
        let mut buffer = ChannelBuffer::new(100.0);
        let samples = (0..10_000).map(|value| value as i32).collect::<Vec<_>>();
        buffer.append(0, &samples);
        let display = DisplaySettings {
            window_seconds: 100,
            ..DisplaySettings::default()
        };
        let reduced = display_samples(&buffer, &display);
        assert!(reduced.len() <= MAX_DISPLAY_POINTS);
        assert!(reduced.contains(&0));
        assert!(reduced.contains(&9_999));
    }

    #[test]
    fn alert_triggers_after_lta_warmup() {
        let mut settings = Settings::default();
        settings.station.sample_rate_hz = 10.0;
        settings.alert.sta_seconds = 1.0;
        settings.alert.lta_seconds = 2.0;
        settings.alert.threshold = 1.5;
        settings.alert.reset = 1.1;
        let mut runtime = RuntimeState::default();
        let quiet = WaveBlock {
            channel: "EHZ".into(),
            start_ms: 0,
            sample_rate_hz: 10.0,
            samples: vec![1; 20],
        };
        assert!(detect_alert(&mut runtime, &settings, &quiet).is_none());
        let signal = WaveBlock {
            channel: "EHZ".into(),
            start_ms: 2_000,
            sample_rate_hz: 10.0,
            samples: vec![10; 10],
        };
        assert!(matches!(
            detect_alert(&mut runtime, &settings, &signal),
            Some(ServerEvent::Alarm { .. })
        ));
        assert!(runtime.alert.active);
    }
}
