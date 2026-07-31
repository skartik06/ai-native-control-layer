#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use chrono::Utc;
use reqwest::Client;
use rusqlite::{params, Connection};
use serde::{de::Error as DeError, Deserialize, Deserializer, Serialize};
use serde_json::json;
use std::{
    env,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
    time::{Duration, Instant, SystemTime},
};
use sysinfo::{Disks, System};
use tauri::{AppHandle, Manager, State};
use tokio::sync::oneshot;

const OLLAMA_API_URL: &str = "http://127.0.0.1:11434/api/chat";
const OLLAMA_TAGS_URL: &str = "http://127.0.0.1:11434/api/tags";
const DEFAULT_OLLAMA_TIMEOUT_SECONDS: u64 = 180;

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    SearchFiles,
    GetSystemInfo,
    ListLargeFiles,
    GetNetworkStatus,
    ToggleSetting,
    ReadRecentLogs,
    LaunchApp,
    MediaControl,
    SendNotification,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum RiskTier {
    Low,
    Medium,
    High,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FileFilters {
    #[serde(rename = "type")]
    file_type: Option<String>,
    date_range: Option<String>,
    size_min: Option<u64>,
    size_max: Option<u64>,
    path: Option<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct IntentParams {
    query: Option<String>,
    filters: Option<FileFilters>,
    directory: Option<String>,
    threshold_mb: Option<u64>,
    setting_name: Option<String>,
    value: Option<serde_json::Value>,
    service_name: Option<String>,
    lines: Option<u32>,
    app_name: Option<String>,
    media_command: Option<String>,
    notification_body: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    action: Action,
    #[serde(default)]
    params: IntentParams,
    risk_tier: RiskTier,
    #[serde(deserialize_with = "deserialize_confidence")]
    confidence: f32,
    clarification_needed: bool,
    clarification_question: Option<String>,
}

fn deserialize_confidence<'de, D>(deserializer: D) -> Result<f32, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Input {
        Number(f32),
        Label(String),
    }

    match Input::deserialize(deserializer)? {
        Input::Number(value) => Ok(value),
        Input::Label(label) => match label.to_lowercase().as_str() {
            "high" => Ok(0.95),
            "medium" => Ok(0.6),
            "low" => Ok(0.2),
            _ => Err(D::Error::custom("confidence must be a number from 0 to 1")),
        },
    }
}

#[derive(Deserialize)]
struct OllamaResponse {
    message: OllamaMessage,
}
#[derive(Deserialize)]
struct OllamaMessage {
    content: String,
}

#[derive(Deserialize)]
struct OllamaTagsResponse {
    models: Vec<OllamaModel>,
}

#[derive(Deserialize)]
struct OllamaModel {
    name: String,
}

fn system_prompt() -> &'static str {
    r#"You are the intent parser for a safety-first Linux desktop assistant. Return ONLY one JSON object, with no Markdown or explanation. It must have action, params, risk_tier, confidence, clarification_needed, and clarification_question keys.

Allowed actions are only: search_files, get_system_info, list_large_files, get_network_status, toggle_setting, read_recent_logs, media_control, send_notification. Any request to open or launch an application, change an unlisted setting, delete files, install software, or perform an unsupported action MUST set clarification_needed to true. Use action "search_files", params {}, risk_tier "low", confidence below 0.9, and ask the user what supported task they want instead.

Parameter rules: get_system_info and get_network_status require params {}. search_files requires query and/or filters. list_large_files requires directory and threshold_mb. toggle_setting requires setting_name (wifi, brightness, volume, dark_mode, or do_not_disturb) and value. read_recent_logs requires service_name and lines. media_control requires media_command (play, pause, next, or previous). send_notification requires notification_body.

Use low risk only for read-only actions; medium only for toggle_setting, media_control, and send_notification. Params may use only query, filters, directory, threshold_mb, setting_name, value, service_name, lines, media_command, notification_body. filters may use type, date_range, size_min, size_max, path. confidence MUST be a JSON number from 0 to 1, for example 0.95; never use words such as high, medium, or low. If ambiguous or confidence below 0.9, set clarification_needed true and provide a non-empty clarification_question. Never invent an unlisted action."#
}

fn append_debug_log(app: &AppHandle, user_input: &str, model_output: &str) {
    let Some(data_dir) = app.path().app_data_dir().ok() else {
        return;
    };
    if std::fs::create_dir_all(&data_dir).is_err() {
        return;
    }
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(data_dir.join("intent-debug.jsonl"))
    {
        let _ = writeln!(
            file,
            "{}",
            json!({"timestamp": Utc::now().to_rfc3339(), "input": user_input, "model_output": model_output})
        );
    }
}

fn final_model_output(content: &str) -> &str {
    content
        .rsplit_once("</think>")
        .map(|(_, answer)| answer.trim())
        .unwrap_or_else(|| content.trim())
}

fn ollama_timeout() -> Duration {
    let seconds = env::var("OLLAMA_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (30..=600).contains(value))
        .unwrap_or(DEFAULT_OLLAMA_TIMEOUT_SECONDS);
    Duration::from_secs(seconds)
}

fn unsupported_app_request(request: &str) -> Option<Intent> {
    let request = request.to_lowercase();
    let asks_to_launch = ["open", "launch", "start", "run"]
        .iter()
        .any(|verb| request.contains(verb));
    let unsupported_app = ["file manager", "nautilus", "dolphin", "terminal", "browser"]
        .iter()
        .any(|app| request.contains(app));
    if asks_to_launch && unsupported_app {
        Some(Intent {
            action: Action::SearchFiles,
            params: IntentParams::default(),
            risk_tier: RiskTier::Low,
            confidence: 0.0,
            clarification_needed: true,
            clarification_question: Some(
                "Opening applications is not supported in this MVP. Would you like me to search for a file instead?".to_string(),
            ),
        })
    } else {
        None
    }
}

fn local_launch_request(request: &str) -> Option<Intent> {
    let normalized = request.to_lowercase();
    let asks_to_launch = ["open", "opn", "launch", "lauch", "start"]
        .iter()
        .any(|verb| normalized.contains(verb));
    if !asks_to_launch {
        return None;
    }
    let app_name = if ["file manager", "file explorer", "files", "folder"]
        .iter()
        .any(|name| normalized.contains(name))
    {
        "file_manager"
    } else if ["browser", "firefox", "chrome", "browsr", "broser"]
        .iter()
        .any(|name| normalized.contains(name))
    {
        "browser"
    } else if ["terminal", "termnal", "command line"]
        .iter()
        .any(|name| normalized.contains(name))
    {
        "terminal"
    } else {
        return None;
    };
    Some(Intent {
        action: Action::LaunchApp,
        params: IntentParams {
            app_name: Some(app_name.to_string()),
            ..Default::default()
        },
        risk_tier: RiskTier::Medium,
        confidence: 1.0,
        clarification_needed: false,
        clarification_question: None,
    })
}

fn small_talk_request(request: &str) -> Option<Intent> {
    let normalized = request
        .trim()
        .to_lowercase()
        .chars()
        .filter(|character| character.is_alphanumeric() || character.is_whitespace())
        .collect::<String>();
    let is_greeting = [
        "hi",
        "hello",
        "hey",
        "good morning",
        "good afternoon",
        "good evening",
    ]
    .iter()
    .any(|greeting| normalized.trim() == *greeting);
    if is_greeting {
        Some(Intent {
            action: Action::GetSystemInfo,
            params: IntentParams::default(),
            risk_tier: RiskTier::Low,
            confidence: 1.0,
            clarification_needed: true,
            clarification_question: Some(
                "Hi! I can safely check system information, Wi-Fi status, files, large files, or recent service logs. What would you like to do?".to_string(),
            ),
        })
    } else {
        None
    }
}

fn local_large_file_request(request: &str) -> Option<Intent> {
    let normalized = request.to_lowercase();
    if !(normalized.contains("file")
        && (normalized.contains("large") || normalized.contains("bigger")))
    {
        return None;
    }
    let words = normalized.split_whitespace().collect::<Vec<_>>();
    let threshold_mb = words.iter().enumerate().find_map(|(index, word)| {
        word.strip_suffix("mb")
            .and_then(|value| value.parse::<u64>().ok())
            .or_else(|| {
                (words.get(index + 1) == Some(&"mb"))
                    .then(|| word.parse::<u64>().ok())
                    .flatten()
            })
    })?;
    let directory = if normalized.contains("home") {
        "home"
    } else {
        "Documents"
    };
    Some(Intent {
        action: Action::ListLargeFiles,
        params: IntentParams {
            directory: Some(directory.to_string()),
            threshold_mb: Some(threshold_mb),
            ..Default::default()
        },
        risk_tier: RiskTier::Low,
        confidence: 1.0,
        clarification_needed: false,
        clarification_question: None,
    })
}

fn local_toggle_request(request: &str) -> Option<Intent> {
    let request = request.to_lowercase();
    if request.contains("volume") {
        let percent = request.split_whitespace().find_map(|word| {
            word.trim_end_matches('%')
                .parse::<u8>()
                .ok()
                .filter(|value| *value <= 100)
        })?;
        return Some(Intent {
            action: Action::ToggleSetting,
            params: IntentParams {
                setting_name: Some("volume".to_string()),
                value: Some(json!(percent)),
                ..Default::default()
            },
            risk_tier: RiskTier::Medium,
            confidence: 1.0,
            clarification_needed: false,
            clarification_question: None,
        });
    }
    let setting_name = if request.contains("dark mode") || request.contains("light mode") {
        "dark_mode"
    } else if request.contains("do not disturb") || request.contains("dnd") {
        "do_not_disturb"
    } else if request.contains("wi-fi") || request.contains("wifi") {
        "wifi"
    } else {
        return None;
    };
    let enable = ["turn on", "enable", "switch on"]
        .iter()
        .any(|phrase| request.contains(phrase));
    let disable = ["turn off", "disable", "switch off"]
        .iter()
        .any(|phrase| request.contains(phrase));
    let mut value = match (enable, disable) {
        (true, false) => true,
        (false, true) => false,
        _ => return None,
    };
    if request.contains("light mode") {
        value = !value;
    }
    Some(Intent {
        action: Action::ToggleSetting,
        params: IntentParams {
            setting_name: Some(setting_name.to_string()),
            value: Some(json!(value)),
            ..Default::default()
        },
        risk_tier: RiskTier::Medium,
        confidence: 1.0,
        clarification_needed: false,
        clarification_question: None,
    })
}

fn local_media_request(request: &str) -> Option<Intent> {
    let normalized = request.to_lowercase();
    let media_command = if ["pause", "stop music", "stop song"]
        .iter()
        .any(|phrase| normalized.contains(phrase))
    {
        "pause"
    } else if ["next song", "next track", "skip song", "skip track"]
        .iter()
        .any(|phrase| normalized.contains(phrase))
    {
        "next"
    } else if ["previous song", "previous track", "back song", "back track"]
        .iter()
        .any(|phrase| normalized.contains(phrase))
    {
        "previous"
    } else if ["play music", "play song", "resume music", "resume song"]
        .iter()
        .any(|phrase| normalized.contains(phrase))
    {
        "play"
    } else {
        return None;
    };
    Some(Intent {
        action: Action::MediaControl,
        params: IntentParams {
            media_command: Some(media_command.to_string()),
            ..Default::default()
        },
        risk_tier: RiskTier::Medium,
        confidence: 1.0,
        clarification_needed: false,
        clarification_question: None,
    })
}

fn local_notification_request(request: &str) -> Option<Intent> {
    let trimmed = request.trim();
    let lower = trimmed.to_lowercase();
    let body = ["notify me", "remind me"]
        .iter()
        .find_map(|prefix| lower.strip_prefix(prefix).map(str::trim))?;
    if body.is_empty() || body.len() > 280 {
        return None;
    }
    Some(Intent {
        action: Action::SendNotification,
        params: IntentParams {
            notification_body: Some(body.to_string()),
            ..Default::default()
        },
        risk_tier: RiskTier::Medium,
        confidence: 1.0,
        clarification_needed: false,
        clarification_question: None,
    })
}

fn select_available_model(models: &[OllamaModel]) -> Option<String> {
    ["qwen3:4b-instruct", "qwen3:4b", "qwen3:1.7b"]
        .iter()
        .find_map(|preferred| {
            models
                .iter()
                .find(|model| model.name == *preferred)
                .map(|model| model.name.clone())
        })
        .or_else(|| {
            models
                .iter()
                .find(|model| model.name.starts_with("qwen3:"))
                .map(|model| model.name.clone())
        })
        .or_else(|| models.first().map(|model| model.name.clone()))
}

async fn selected_ollama_model(client: &Client) -> String {
    if let Ok(model) = env::var("OLLAMA_MODEL") {
        if !model.trim().is_empty() {
            return model;
        }
    }
    match client.get(OLLAMA_TAGS_URL).send().await {
        Ok(response) if response.status().is_success() => response
            .json::<OllamaTagsResponse>()
            .await
            .ok()
            .and_then(|tags| select_available_model(&tags.models))
            .unwrap_or_else(|| "qwen3:4b-instruct".to_string()),
        _ => "qwen3:4b-instruct".to_string(),
    }
}

fn validate_intent(intent: &Intent) -> Result<(), String> {
    if intent.confidence < 0.9 && !intent.clarification_needed {
        return Err(
            "Ollama returned low confidence without requesting clarification. No action was taken."
                .to_string(),
        );
    }
    if intent.clarification_needed
        && intent
            .clarification_question
            .as_deref()
            .is_none_or(str::is_empty)
    {
        return Err(
            "Ollama requested clarification without a question. No action was taken.".to_string(),
        );
    }

    let params = &intent.params;
    if params.app_name.is_some() && !matches!(intent.action, Action::LaunchApp) {
        return Err(
            "Ollama returned an app parameter for an incompatible action. No action was taken."
                .to_string(),
        );
    }
    if params.media_command.is_some() && !matches!(intent.action, Action::MediaControl) {
        return Err(
            "Ollama returned a media parameter for an incompatible action. No action was taken."
                .to_string(),
        );
    }
    if params.notification_body.is_some() && !matches!(intent.action, Action::SendNotification) {
        return Err("Ollama returned a notification parameter for an incompatible action. No action was taken.".to_string());
    }
    match intent.action {
        Action::SearchFiles
            if (params.query.is_none() && params.filters.is_none())
                || params.directory.is_some()
                || params.threshold_mb.is_some()
                || params.setting_name.is_some()
                || params.value.is_some()
                || params.service_name.is_some()
                || params.lines.is_some() =>
        {
            Err("Ollama returned invalid search-file parameters. No action was taken.".to_string())
        }
        Action::GetSystemInfo | Action::GetNetworkStatus
            if params.query.is_some()
                || params.filters.is_some()
                || params.directory.is_some()
                || params.threshold_mb.is_some()
                || params.setting_name.is_some()
                || params.value.is_some()
                || params.service_name.is_some()
                || params.lines.is_some() =>
        {
            Err("Ollama returned incompatible parameters for a read-only action. No action was taken.".to_string())
        }
        Action::ListLargeFiles
            if params.directory.is_none()
                || params.threshold_mb.is_none()
                || params.query.is_some()
                || params.filters.is_some()
                || params.setting_name.is_some()
                || params.value.is_some()
                || params.service_name.is_some()
                || params.lines.is_some() =>
        {
            Err("Ollama omitted required large-file parameters. No action was taken.".to_string())
        }
        Action::ToggleSetting
            if params.setting_name.is_none()
                || params.value.is_none()
                || params.query.is_some()
                || params.filters.is_some()
                || params.directory.is_some()
                || params.threshold_mb.is_some()
                || params.service_name.is_some()
                || params.lines.is_some() =>
        {
            Err("Ollama omitted required setting parameters. No action was taken.".to_string())
        }
        Action::ReadRecentLogs
            if params.service_name.is_none()
                || params.lines.is_none()
                || params.query.is_some()
                || params.filters.is_some()
                || params.directory.is_some()
                || params.threshold_mb.is_some()
                || params.setting_name.is_some()
                || params.value.is_some() =>
        {
            Err("Ollama omitted required log parameters. No action was taken.".to_string())
        }
        Action::LaunchApp
            if params.app_name.is_none()
                || params.query.is_some()
                || params.filters.is_some()
                || params.directory.is_some()
                || params.threshold_mb.is_some()
                || params.setting_name.is_some()
                || params.value.is_some()
                || params.service_name.is_some()
                || params.lines.is_some() =>
        {
            Err("App launch requires one supported app name. No action was taken.".to_string())
        }
        Action::MediaControl
            if params.media_command.is_none()
                || params.query.is_some()
                || params.filters.is_some()
                || params.directory.is_some()
                || params.threshold_mb.is_some()
                || params.setting_name.is_some()
                || params.value.is_some()
                || params.service_name.is_some()
                || params.lines.is_some()
                || params.app_name.is_some() =>
        {
            Err("Media control requires one supported media command. No action was taken.".to_string())
        }
        Action::SendNotification
            if params.notification_body.as_deref().is_none_or(|body| body.trim().is_empty() || body.len() > 280)
                || params.query.is_some()
                || params.filters.is_some()
                || params.directory.is_some()
                || params.threshold_mb.is_some()
                || params.setting_name.is_some()
                || params.value.is_some()
                || params.service_name.is_some()
                || params.lines.is_some()
                || params.app_name.is_some()
                || params.media_command.is_some() =>
        {
            Err("A notification needs a message of at most 280 characters. No action was taken.".to_string())
        }
        _ => Ok(()),
    }
}

fn planned_risk(action: Action) -> RiskTier {
    match action {
        Action::ToggleSetting
        | Action::LaunchApp
        | Action::MediaControl
        | Action::SendNotification => RiskTier::Medium,
        Action::SearchFiles
        | Action::GetSystemInfo
        | Action::ListLargeFiles
        | Action::GetNetworkStatus
        | Action::ReadRecentLogs => RiskTier::Low,
    }
}

fn plan_intent(intent: &Intent) -> Result<RiskTier, String> {
    validate_intent(intent)?;
    let expected_risk = planned_risk(intent.action);
    if intent.risk_tier != expected_risk {
        return Err("Ollama assigned an incompatible risk tier. No action was taken.".to_string());
    }
    if matches!(intent.action, Action::ToggleSetting) {
        prepare_toggle(&intent.params)?;
    }
    if matches!(intent.action, Action::LaunchApp) {
        prepare_launch(&intent.params)?;
    }
    if matches!(intent.action, Action::MediaControl) {
        prepare_media(&intent.params)?;
    }
    if matches!(intent.action, Action::SendNotification) {
        prepare_notification(&intent.params)?;
    }
    Ok(expected_risk)
}

fn bool_value(value: &serde_json::Value, setting_name: &str) -> Result<bool, String> {
    value.as_bool().ok_or_else(|| {
        format!(
            "{} must be set to true or false. No action was taken.",
            setting_name
        )
    })
}

fn prepare_toggle(params: &IntentParams) -> Result<ToggleSetting, String> {
    let setting_name = params
        .setting_name
        .as_deref()
        .ok_or_else(|| "A setting name is required. No action was taken.".to_string())?
        .trim()
        .to_lowercase();
    let value = params
        .value
        .as_ref()
        .ok_or_else(|| "A setting value is required. No action was taken.".to_string())?;
    match setting_name.as_str() {
        "wifi" => Ok(ToggleSetting::Wifi {
            enabled: bool_value(value, "Wi-Fi")?,
        }),
        "brightness" => {
            let percent = value.as_u64().ok_or_else(|| {
                "Brightness must be a whole number from 0 to 100. No action was taken.".to_string()
            })?;
            let percent = u8::try_from(percent)
                .ok()
                .filter(|value| *value <= 100)
                .ok_or_else(|| {
                    "Brightness must be a whole number from 0 to 100. No action was taken."
                        .to_string()
                })?;
            Ok(ToggleSetting::Brightness { percent })
        }
        "volume" => {
            let percent = value
                .as_u64()
                .and_then(|value| u8::try_from(value).ok())
                .filter(|value| *value <= 100)
                .ok_or_else(|| {
                    "Volume must be a whole number from 0 to 100. No action was taken.".to_string()
                })?;
            Ok(ToggleSetting::Volume { percent })
        }
        "dark_mode" | "dark mode" => Ok(ToggleSetting::DarkMode {
            enabled: bool_value(value, "Dark mode")?,
        }),
        "do_not_disturb" | "do not disturb" => Ok(ToggleSetting::DoNotDisturb {
            enabled: bool_value(value, "Do not disturb")?,
        }),
        _ => {
            Err("That setting is not in the Linux MVP whitelist. No action was taken.".to_string())
        }
    }
}

fn toggle_preview(setting: &ToggleSetting) -> String {
    match setting {
        ToggleSetting::Wifi { enabled } => {
            format!("Turn Wi-Fi {}.", if *enabled { "on" } else { "off" })
        }
        ToggleSetting::Brightness { percent } => format!("Set screen brightness to {}%.", percent),
        ToggleSetting::Volume { percent } => format!("Set system volume to {}%.", percent),
        ToggleSetting::DarkMode { enabled } => {
            format!("Turn dark mode {}.", if *enabled { "on" } else { "off" })
        }
        ToggleSetting::DoNotDisturb { enabled } => format!(
            "Turn Do Not Disturb {}.",
            if *enabled { "on" } else { "off" }
        ),
    }
}

fn prepare_launch(params: &IntentParams) -> Result<LaunchTarget, String> {
    match params.app_name.as_deref().map(str::trim) {
        Some("file_manager") => Ok(LaunchTarget::FileManager),
        Some("browser") => Ok(LaunchTarget::Browser),
        Some("terminal") => Ok(LaunchTarget::Terminal),
        _ => Err("Only the file manager, browser, and terminal can be launched in this MVP. No action was taken.".to_string()),
    }
}

fn launch_preview(target: &LaunchTarget) -> String {
    match target {
        LaunchTarget::FileManager => {
            "Open the default file manager at your home folder.".to_string()
        }
        LaunchTarget::Browser => "Open the default web browser.".to_string(),
        LaunchTarget::Terminal => "Open the default terminal.".to_string(),
    }
}

fn prepare_media(params: &IntentParams) -> Result<MediaCommand, String> {
    match params.media_command.as_deref().map(str::trim) {
        Some("play") => Ok(MediaCommand::Play),
        Some("pause") => Ok(MediaCommand::Pause),
        Some("next") => Ok(MediaCommand::Next),
        Some("previous") => Ok(MediaCommand::Previous),
        _ => Err("Only play, pause, next, and previous are supported media commands. No action was taken.".to_string()),
    }
}

fn media_preview(command: &MediaCommand) -> String {
    match command {
        MediaCommand::Play => "Resume the current media player.".to_string(),
        MediaCommand::Pause => "Pause the current media player.".to_string(),
        MediaCommand::Next => "Skip to the next media track.".to_string(),
        MediaCommand::Previous => "Go to the previous media track.".to_string(),
    }
}

fn prepare_notification(params: &IntentParams) -> Result<String, String> {
    let body = params.notification_body.as_deref().unwrap_or("").trim();
    if body.is_empty() || body.len() > 280 {
        return Err(
            "A notification needs a message of at most 280 characters. No action was taken."
                .to_string(),
        );
    }
    Ok(body.to_string())
}

async fn parse_intent_internal(
    app: AppHandle,
    request: String,
    cancellation: Option<&mut oneshot::Receiver<()>>,
) -> Result<Intent, String> {
    let request = request.trim();
    if request.is_empty() {
        return Err("Enter a request first.".to_string());
    }
    if request.len() > 4_000 {
        return Err("Request is too long (maximum 4,000 characters).".to_string());
    }
    if let Some(intent) = local_launch_request(request) {
        append_debug_log(
            &app,
            request,
            "Locally parsed a whitelisted app launch request.",
        );
        return Ok(intent);
    }
    if let Some(clarification) = unsupported_app_request(request) {
        append_debug_log(
            &app,
            request,
            "Locally rejected unsupported app-launch request.",
        );
        return Ok(clarification);
    }
    if let Some(clarification) = small_talk_request(request) {
        append_debug_log(
            &app,
            request,
            "Locally handled a greeting without calling Ollama.",
        );
        return Ok(clarification);
    }
    if let Some(intent) = local_large_file_request(request) {
        append_debug_log(
            &app,
            request,
            "Locally parsed a large-file request before Ollama.",
        );
        return Ok(intent);
    }
    if let Some(intent) = local_toggle_request(request) {
        append_debug_log(
            &app,
            request,
            "Locally parsed whitelisted setting request before Ollama.",
        );
        return Ok(intent);
    }
    if let Some(intent) = local_media_request(request) {
        append_debug_log(
            &app,
            request,
            "Locally parsed a media request before Ollama.",
        );
        return Ok(intent);
    }
    if let Some(intent) = local_notification_request(request) {
        append_debug_log(
            &app,
            request,
            "Locally parsed a notification request before Ollama.",
        );
        return Ok(intent);
    }
    let timeout = ollama_timeout();
    let client = Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|_| "Could not initialize the local Ollama client.".to_string())?;
    let model = selected_ollama_model(&client).await;
    let memory_context = get_memory(app.clone())
        .unwrap_or_default()
        .into_iter()
        .take(20)
        .map(|entry| format!("- {}: {}", entry.memory_key, entry.value))
        .collect::<Vec<_>>()
        .join("\n");
    let system_message = if memory_context.is_empty() {
        "You are a helpful, privacy-first Linux desktop assistant. Answer conversational questions naturally and concisely. Do not claim that you executed any system action. Tell the user to switch to Control mode for computer actions.".to_string()
    } else {
        format!("You are a helpful, privacy-first Linux desktop assistant. Answer conversational questions naturally and concisely. Do not claim that you executed any system action. Tell the user to switch to Control mode for computer actions. The following are user-approved local preferences; use them only when relevant, never reveal them unless asked, and do not claim to have learned anything else:\n{memory_context}")
    };
    let body = json!({
        "model": model,
        "stream": false,
        "format": "json",
        "think": false,
        "options": {
            "temperature": 0,
            "num_ctx": 2048,
            "num_predict": 256
        },
        "messages": [
            {"role": "system", "content": system_prompt()},
            {"role": "user", "content": request}
        ]
    });
    let request_future = client.post(OLLAMA_API_URL).json(&body).send();
    let send_result = match cancellation {
        Some(receiver) => tokio::select! {
            result = request_future => result,
            _ = receiver => return Err("Request cancelled. No action was taken.".to_string()),
        },
        None => request_future.await,
    };
    let response = send_result.map_err(|error| {
        if error.is_timeout() {
            format!(
                "Ollama took longer than {} seconds. CPU-only virtual machines can be slow; try again or set OLLAMA_TIMEOUT_SECONDS up to 600.",
                timeout.as_secs()
            )
        } else {
            "Could not reach Ollama. Check `systemctl status ollama` and confirm the selected model is installed.".to_string()
        }
    })?;
    let status = response.status();
    let response_text = response
        .text()
        .await
        .map_err(|_| "Could not read the Claude API response.".to_string())?;
    if !status.is_success() {
        append_debug_log(&app, request, &response_text);
        return Err(format!(
            "Ollama returned HTTP {} for model '{}'. Run `ollama list`, then set OLLAMA_MODEL to one of the listed models and restart the app.",
            status, body["model"].as_str().unwrap_or("unknown")
        ));
    }
    let api_response: OllamaResponse = serde_json::from_str(&response_text)
        .map_err(|_| "Ollama returned an unreadable API response.".to_string())?;
    let raw_intent = final_model_output(&api_response.message.content);
    append_debug_log(&app, request, raw_intent);
    let intent: Intent = serde_json::from_str(raw_intent)
        .map_err(|_| "Ollama returned an invalid intent. No action was taken.".to_string())?;
    if !(0.0..=1.0).contains(&intent.confidence) {
        return Err(
            "Ollama returned an invalid confidence score. No action was taken.".to_string(),
        );
    }
    validate_intent(&intent)?;
    Ok(intent)
}

#[tauri::command]
async fn parse_intent(app: AppHandle, request: String) -> Result<Intent, String> {
    parse_intent_internal(app, request, None).await
}

#[tauri::command]
async fn chat_with_assistant(
    app: AppHandle,
    request_cancellation: State<'_, RequestCancellation>,
    message: String,
) -> Result<String, String> {
    let message = message.trim();
    if message.is_empty() || message.len() > 4_000 {
        return Err("Enter a message under 4,000 characters.".to_string());
    }
    if let Some(reply) = local_chat_reply(message) {
        append_debug_log(&app, message, &reply);
        record_chat_entry(&app, "user", message)?;
        record_chat_entry(&app, "assistant", &reply)?;
        return Ok(reply);
    }
    let timeout = ollama_timeout();
    let client = Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|_| "Could not initialize the local assistant model.".to_string())?;
    let model = selected_ollama_model(&client).await;
    let body = json!({
        "model": model,
        "stream": false,
        "think": false,
        "options": {"temperature": 0.7, "num_ctx": 4096, "num_predict": 512},
        "messages": [
            {"role": "system", "content": system_message},
            {"role": "user", "content": message}
        ]
    });
    let (sender, mut receiver) = oneshot::channel();
    {
        let mut current = request_cancellation
            .0
            .lock()
            .map_err(|_| "The request control is unavailable.".to_string())?;
        *current = Some(sender);
    }
    let response_result = tokio::select! {
        result = client.post(OLLAMA_API_URL).json(&body).send() => result,
        _ = &mut receiver => {
            request_cancellation.0.lock().ok().and_then(|mut current| current.take());
            return Err("Chat request cancelled. No system action was taken.".to_string());
        }
    };
    request_cancellation
        .0
        .lock()
        .map_err(|_| "The request control is unavailable.".to_string())?
        .take();
    let response = response_result.map_err(|error| {
            if error.is_timeout() {
                "The local chat model is still thinking. On a VM this can be slow; use a direct Linux GPU install for fast conversational replies.".to_string()
            } else {
                "Could not reach the local chat model. Check that Ollama is running.".to_string()
            }
        })?;
    if !response.status().is_success() {
        return Err("The selected local chat model is unavailable. Run `ollama list` and restart the assistant.".to_string());
    }
    let api_response: OllamaResponse = response
        .json()
        .await
        .map_err(|_| "The local chat model returned an unreadable reply.".to_string())?;
    let reply = final_model_output(&api_response.message.content).to_string();
    append_debug_log(&app, message, &reply);
    record_chat_entry(&app, "user", message)?;
    record_chat_entry(&app, "assistant", &reply)?;
    Ok(reply)
}

fn local_chat_reply(message: &str) -> Option<String> {
    let normalized = message
        .trim()
        .to_lowercase()
        .chars()
        .filter(|character| character.is_alphanumeric() || character.is_whitespace())
        .collect::<String>();
    let text = normalized.trim();
    match text {
        "hi" | "hello" | "hey" | "good morning" | "good afternoon" | "good evening" => Some("Hi! I'm your local Linux assistant. In Control mode I can inspect your system, open supported apps, change approved settings, and control media with confirmation. In Chat mode I can answer general questions using your local model.".to_string()),
        "thanks" | "thank you" | "thx" => Some("You're welcome. Switch to Control mode whenever you want me to do something on your Linux desktop.".to_string()),
        "help" | "what can you do" | "what can u do" | "who are you" => Some("I'm a privacy-first Linux assistant. Try Control mode for: show system information, show Wi-Fi status, find files, open Firefox, turn on dark mode, pause music, or next song. App, setting, and media changes always need your confirmation.".to_string()),
        _ => None,
    }
}

const MAX_RESULTS: usize = 50;
const MAX_WALK_DEPTH: usize = 8;

#[derive(Serialize)]
struct ToolExecution {
    tool: String,
    summary: String,
    data: serde_json::Value,
}

#[derive(Serialize)]
struct ProcessResponse {
    intent: Intent,
    execution: Option<ToolExecution>,
    message: String,
    confirmation: Option<ConfirmationPreview>,
}

#[derive(Serialize)]
struct ConfirmationPreview {
    summary: String,
    expires_in_seconds: u64,
}

#[derive(Clone)]
enum ToggleSetting {
    Wifi { enabled: bool },
    Brightness { percent: u8 },
    Volume { percent: u8 },
    DarkMode { enabled: bool },
    DoNotDisturb { enabled: bool },
}

#[derive(Clone)]
enum LaunchTarget {
    FileManager,
    Browser,
    Terminal,
}

#[derive(Clone)]
enum MediaCommand {
    Play,
    Pause,
    Next,
    Previous,
}

#[derive(Clone)]
enum PendingOperation {
    Toggle(ToggleSetting),
    Launch(LaunchTarget),
    Media(MediaCommand),
    Notification(String),
}

struct PendingAction {
    operation: PendingOperation,
    audit: AuditContext,
    created_at: Instant,
}

struct PendingConfirmation(Mutex<Option<PendingAction>>);

impl Default for PendingConfirmation {
    fn default() -> Self {
        Self(Mutex::new(None))
    }
}

struct RequestCancellation(Mutex<Option<oneshot::Sender<()>>>);

impl Default for RequestCancellation {
    fn default() -> Self {
        Self(Mutex::new(None))
    }
}

const CONFIRMATION_TTL: Duration = Duration::from_secs(60);

#[derive(Clone)]
struct AuditContext {
    action: String,
    risk_tier: String,
    params_json: String,
}

impl From<&Intent> for AuditContext {
    fn from(intent: &Intent) -> Self {
        Self {
            action: serde_json::to_string(&intent.action)
                .unwrap_or_else(|_| "\"unknown\"".to_string())
                .trim_matches('"')
                .to_string(),
            risk_tier: serde_json::to_string(&intent.risk_tier)
                .unwrap_or_else(|_| "\"unknown\"".to_string())
                .trim_matches('"')
                .to_string(),
            params_json: serde_json::to_string(&intent.params).unwrap_or_else(|_| "{}".to_string()),
        }
    }
}

#[derive(Serialize)]
struct AuditEntry {
    id: i64,
    timestamp: String,
    event_type: String,
    action: String,
    risk_tier: String,
    params_json: String,
    outcome: String,
    summary: String,
    result_json: Option<String>,
}

#[derive(Serialize)]
struct RuntimeProfile {
    profile: String,
    total_memory_gb: u64,
    cpu_cores: usize,
    summary: String,
}

#[derive(Serialize)]
struct VoiceStatus {
    text_to_speech_available: bool,
    speech_to_text_available: bool,
    text_to_speech_engine: Option<String>,
    speech_to_text_engine: Option<String>,
    summary: String,
}

#[cfg(target_os = "linux")]
fn executable_available(program: &str) -> bool {
    Command::new("which")
        .arg(program)
        .output()
        .is_ok_and(|output| output.status.success())
}

#[cfg(not(target_os = "linux"))]
fn executable_available(_: &str) -> bool {
    false
}

#[tauri::command]
fn get_voice_status() -> VoiceStatus {
    let text_to_speech_engine = if executable_available("spd-say") {
        Some("spd-say".to_string())
    } else if executable_available("espeak-ng") {
        Some("espeak-ng".to_string())
    } else {
        None
    };
    let speech_to_text_engine = if executable_available("whisper-cli") {
        Some("whisper-cli".to_string())
    } else if executable_available("whisper-cpp") {
        Some("whisper-cpp".to_string())
    } else {
        None
    };
    let summary = match (&text_to_speech_engine, &speech_to_text_engine) {
        (Some(tts), Some(stt)) => format!("Voice output ({tts}) and local speech recognition ({stt}) are available."),
        (Some(tts), None) => format!("Voice output is available through {tts}. Install whisper.cpp to enable local voice input."),
        (None, _) => "Voice output is not installed. Install speech-dispatcher or espeak-ng on Linux.".to_string(),
    };
    VoiceStatus {
        text_to_speech_available: text_to_speech_engine.is_some(),
        speech_to_text_available: speech_to_text_engine.is_some(),
        text_to_speech_engine,
        speech_to_text_engine,
        summary,
    }
}

#[tauri::command]
fn speak_text(text: String) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("There is no response to speak.".to_string());
    }
    let clipped = text.chars().take(1_000).collect::<String>();
    #[cfg(target_os = "linux")]
    {
        let program = if executable_available("spd-say") {
            "spd-say"
        } else if executable_available("espeak-ng") {
            "espeak-ng"
        } else {
            return Err(
                "Voice output is not installed. Install speech-dispatcher or espeak-ng first."
                    .to_string(),
            );
        };
        Command::new(program)
            .arg(&clipped)
            .spawn()
            .map_err(|_| "Could not start the Linux speech engine.".to_string())?;
        return Ok("Speaking the latest assistant response.".to_string());
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = clipped;
        Err("Voice output is available in the Linux desktop build only.".to_string())
    }
}

#[derive(Serialize)]
struct MemoryEntry {
    id: i64,
    created_at: String,
    category: String,
    memory_key: String,
    value: String,
}

#[derive(Serialize)]
struct ChatEntry {
    id: i64,
    created_at: String,
    role: String,
    content: String,
}

fn open_audit_database(app: &AppHandle) -> Result<Connection, String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|_| "Could not locate the local audit data folder.".to_string())?;
    fs::create_dir_all(&data_dir)
        .map_err(|_| "Could not create the local audit data folder.".to_string())?;
    let connection = Connection::open(data_dir.join("audit.sqlite3"))
        .map_err(|_| "Could not open the local audit database.".to_string())?;
    connection
        .busy_timeout(Duration::from_secs(2))
        .map_err(|_| "Could not prepare the local audit database.".to_string())?;
    initialize_audit_database(&connection)?;
    Ok(connection)
}

fn initialize_audit_database(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS audit_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL,
                event_type TEXT NOT NULL,
                action TEXT NOT NULL,
                risk_tier TEXT NOT NULL,
                params_json TEXT NOT NULL,
                outcome TEXT NOT NULL,
                summary TEXT NOT NULL,
                result_json TEXT
            );
            CREATE INDEX IF NOT EXISTS audit_events_timestamp_idx ON audit_events(timestamp DESC);
            CREATE TABLE IF NOT EXISTS assistant_memory (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                created_at TEXT NOT NULL,
                category TEXT NOT NULL,
                memory_key TEXT NOT NULL,
                value TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS assistant_memory_created_at_idx ON assistant_memory(created_at DESC);
            CREATE TABLE IF NOT EXISTS chat_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                created_at TEXT NOT NULL,
                role TEXT NOT NULL CHECK(role IN ('user', 'assistant')),
                content TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS chat_history_created_at_idx ON chat_history(created_at DESC);",
        )
        .map_err(|_| "Could not initialize the local audit database.".to_string())?;
    Ok(())
}

fn record_chat_entry(app: &AppHandle, role: &str, content: &str) -> Result<(), String> {
    let connection = open_audit_database(app)?;
    connection
        .execute(
            "INSERT INTO chat_history (created_at, role, content) VALUES (?1, ?2, ?3)",
            params![Utc::now().to_rfc3339(), role, content],
        )
        .map_err(|_| "Could not save local chat history.".to_string())?;
    Ok(())
}

fn record_audit(
    app: &AppHandle,
    context: &AuditContext,
    event_type: &str,
    outcome: &str,
    summary: &str,
    result: Option<&serde_json::Value>,
) -> Result<(), String> {
    let connection = open_audit_database(app)?;
    let result_json = result
        .map(serde_json::to_string)
        .transpose()
        .map_err(|_| "Could not serialize an audit result.".to_string())?;
    connection
        .execute(
            "INSERT INTO audit_events (timestamp, event_type, action, risk_tier, params_json, outcome, summary, result_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                Utc::now().to_rfc3339(),
                event_type,
                &context.action,
                &context.risk_tier,
                &context.params_json,
                outcome,
                summary,
                result_json,
            ],
        )
        .map_err(|_| "Could not write the local audit event.".to_string())?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn user_root() -> Result<PathBuf, String> {
    let value = env::var("HOME").map_err(|_| "HOME is not configured.".to_string())?;
    fs::canonicalize(value).map_err(|_| "Could not resolve the current user folder.".to_string())
}

#[cfg(not(target_os = "linux"))]
fn user_root() -> Result<PathBuf, String> {
    Err("This MVP supports Linux desktop only.".to_string())
}

fn scoped_directory(path: Option<&str>) -> Result<PathBuf, String> {
    let root = user_root()?;
    let candidate = match path {
        Some(value) if !value.trim().is_empty() => {
            let normalized = value.trim().to_lowercase();
            if matches!(
                normalized.as_str(),
                "home" | "home folder" | "my home" | "my home folder"
            ) {
                return Ok(root);
            }
            let supplied = PathBuf::from(value.trim());
            if supplied.is_absolute() {
                supplied
            } else {
                root.join(supplied)
            }
        }
        _ => root.join("Documents"),
    };
    let resolved = fs::canonicalize(&candidate).map_err(|_| {
        format!(
            "Directory does not exist or is not readable: {}",
            candidate.display()
        )
    })?;
    if !resolved.starts_with(&root) {
        return Err("File tools are limited to folders inside your user profile.".to_string());
    }
    if !resolved.is_dir() {
        return Err("The selected path is not a directory.".to_string());
    }
    Ok(resolved)
}

fn should_skip_directory(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|value| value.to_str()),
        Some(".git" | "node_modules" | "target" | ".cache" | "AppData")
    )
}

fn matches_date_range(modified: SystemTime, date_range: Option<&str>) -> bool {
    let Some(date_range) = date_range else {
        return true;
    };
    let Ok(age) = SystemTime::now().duration_since(modified) else {
        return false;
    };
    match date_range.to_lowercase().as_str() {
        "today" => age <= Duration::from_secs(86_400),
        "last_week" | "week" => age <= Duration::from_secs(7 * 86_400),
        "last_month" | "month" => age <= Duration::from_secs(31 * 86_400),
        _ => true,
    }
}

fn collect_files(
    directory: &Path,
    depth: usize,
    predicate: &impl Fn(&Path, &fs::Metadata) -> bool,
    results: &mut Vec<serde_json::Value>,
) {
    if depth > MAX_WALK_DEPTH || results.len() >= MAX_RESULTS {
        return;
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        if results.len() >= MAX_RESULTS {
            return;
        }
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_dir() {
            if !should_skip_directory(&path) {
                collect_files(&path, depth + 1, predicate, results);
            }
        } else if metadata.is_file() && predicate(&path, &metadata) {
            results.push(json!({
                "path": path.to_string_lossy(),
                "size_bytes": metadata.len(),
                "modified": metadata.modified().ok().and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok()).map(|duration| duration.as_secs())
            }));
        }
    }
}

fn search_files(params: &IntentParams) -> Result<ToolExecution, String> {
    let root = scoped_directory(
        params
            .filters
            .as_ref()
            .and_then(|filters| filters.path.as_deref()),
    )?;
    let query = params.query.as_deref().unwrap_or("").to_lowercase();
    let filters = params.filters.as_ref();
    let mut matches = Vec::new();
    collect_files(
        &root,
        0,
        &|path, metadata| {
            let filename = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("")
                .to_lowercase();
            let extension_matches = filters
                .and_then(|filter| filter.file_type.as_ref())
                .map(|file_type| {
                    path.extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| {
                            extension.eq_ignore_ascii_case(file_type.trim_start_matches('.'))
                        })
                })
                .unwrap_or(true);
            let size_matches = filters
                .and_then(|filter| filter.size_min)
                .is_none_or(|minimum| metadata.len() >= minimum)
                && filters
                    .and_then(|filter| filter.size_max)
                    .is_none_or(|maximum| metadata.len() <= maximum);
            let date_matches = metadata.modified().ok().is_some_and(|modified| {
                matches_date_range(
                    modified,
                    filters.and_then(|filter| filter.date_range.as_deref()),
                )
            });
            (query.is_empty() || filename.contains(&query))
                && extension_matches
                && size_matches
                && date_matches
        },
        &mut matches,
    );
    let count = matches.len();
    Ok(ToolExecution {
        tool: "search_files".to_string(),
        summary: format!("Found {} matching file(s) in {}.", count, root.display()),
        data: json!({"files": matches, "truncated": count == MAX_RESULTS}),
    })
}

fn list_large_files(params: &IntentParams) -> Result<ToolExecution, String> {
    let root = scoped_directory(params.directory.as_deref())?;
    let threshold_mb = params
        .threshold_mb
        .ok_or_else(|| "A threshold_mb value is required.".to_string())?;
    let threshold_bytes = threshold_mb.saturating_mul(1_024 * 1_024);
    let mut matches = Vec::new();
    collect_files(
        &root,
        0,
        &|_, metadata| metadata.len() >= threshold_bytes,
        &mut matches,
    );
    matches.sort_by_key(|value| {
        std::cmp::Reverse(
            value
                .get("size_bytes")
                .and_then(|size| size.as_u64())
                .unwrap_or(0),
        )
    });
    let count = matches.len();
    Ok(ToolExecution {
        tool: "list_large_files".to_string(),
        summary: format!(
            "Found {} file(s) at least {} MB in {}.",
            count,
            threshold_mb,
            root.display()
        ),
        data: json!({"files": matches, "threshold_mb": threshold_mb, "truncated": count == MAX_RESULTS}),
    })
}

fn get_system_info() -> ToolExecution {
    let mut system = System::new_all();
    system.refresh_all();
    let disks = Disks::new_with_refreshed_list();
    let storage: Vec<_> = disks
        .list()
        .iter()
        .map(|disk| {
            json!({
                "name": disk.name().to_string_lossy(),
                "total_bytes": disk.total_space(),
                "available_bytes": disk.available_space()
            })
        })
        .collect();
    let mut apps: Vec<_> = system
        .processes()
        .values()
        .map(|process| process.name().to_string_lossy().to_string())
        .collect();
    apps.sort();
    apps.dedup();
    apps.truncate(50);
    ToolExecution {
        tool: "get_system_info".to_string(),
        summary: "Read system storage, memory, CPU load, and running applications.".to_string(),
        data: json!({
            "storage": storage,
            "ram": {"total_bytes": system.total_memory(), "used_bytes": system.used_memory()},
            "cpu_load_percent": system.global_cpu_usage(),
            "running_apps": apps
        }),
    }
}

#[cfg(target_os = "linux")]
fn split_nmcli_fields(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut escaped = false;
    for character in line.chars() {
        if escaped {
            field.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == ':' {
            fields.push(field);
            field = String::new();
        } else {
            field.push(character);
        }
    }
    if escaped {
        field.push('\\');
    }
    fields.push(field);
    fields
}

#[cfg(target_os = "linux")]
fn parse_network_status(output: &str) -> (bool, Option<String>, Option<u8>) {
    output
        .lines()
        .find_map(|line| {
            let fields = split_nmcli_fields(line);
            (fields.first().map(String::as_str) == Some("yes")).then(|| {
                (
                    true,
                    fields.get(1).filter(|value| !value.is_empty()).cloned(),
                    fields.get(2).and_then(|value| value.parse::<u8>().ok()),
                )
            })
        })
        .unwrap_or((false, None, None))
}

#[cfg(target_os = "linux")]
fn get_network_status() -> Result<ToolExecution, String> {
    let output = Command::new("nmcli")
        .args(["-t", "-f", "ACTIVE,SSID,SIGNAL", "dev", "wifi"])
        .output()
        .map_err(|_| "Could not query NetworkManager. Is nmcli installed?".to_string())?;
    if !output.status.success() {
        return Err("NetworkManager could not report Wi-Fi status.".to_string());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let (connected, ssid, signal_strength) = parse_network_status(&text);
    Ok(ToolExecution {
        tool: "get_network_status".to_string(),
        summary: if connected {
            "Network connection found.".to_string()
        } else {
            "No active Wi-Fi connection found.".to_string()
        },
        data: json!({"connected": connected, "ssid": ssid, "signal_strength": signal_strength}),
    })
}

#[cfg(not(target_os = "linux"))]
fn get_network_status() -> Result<ToolExecution, String> {
    Err("Network diagnostics are available in the Linux desktop MVP only.".to_string())
}

fn is_valid_service_name(service: &str) -> bool {
    service.len() <= 100
        && service.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-' | ' ')
        })
}

#[cfg(target_os = "linux")]
fn read_recent_logs(params: &IntentParams) -> Result<ToolExecution, String> {
    let service = params
        .service_name
        .as_deref()
        .ok_or_else(|| "A service_name is required.".to_string())?;
    if !is_valid_service_name(service) {
        return Err("Service name contains unsupported characters.".to_string());
    }
    let lines = params.lines.unwrap_or(50).clamp(1, 200);
    let output = Command::new("journalctl")
        .args([
            "-u",
            service,
            "-n",
            &lines.to_string(),
            "--no-pager",
            "--output=short-iso",
        ])
        .output()
        .map_err(|_| "Could not run journalctl. Is systemd available?".to_string())?;
    if !output.status.success() {
        return Err(
            "journalctl could not read that service. Check the service name and permissions."
                .to_string(),
        );
    }
    let logs = String::from_utf8_lossy(&output.stdout)
        .chars()
        .take(30_000)
        .collect::<String>();
    Ok(ToolExecution {
        tool: "read_recent_logs".to_string(),
        summary: format!("Read up to {} recent log entries from {}.", lines, service),
        data: json!({"service_name": service, "lines": lines, "logs": logs}),
    })
}

#[cfg(not(target_os = "linux"))]
fn read_recent_logs(_: &IntentParams) -> Result<ToolExecution, String> {
    Err("Log inspection is available in the Linux desktop MVP only.".to_string())
}

#[cfg(target_os = "linux")]
fn run_setting_command(program: &str, args: &[&str]) -> Result<(), String> {
    let output = Command::new(program).args(args).output().map_err(|_| {
        format!(
            "Could not run {}. Check that it is installed and available.",
            program
        )
    })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("{} did not apply the requested setting.", program))
    }
}

#[cfg(target_os = "linux")]
fn execute_toggle(setting: &ToggleSetting) -> Result<ToolExecution, String> {
    match setting {
        ToggleSetting::Wifi { enabled } => {
            run_setting_command(
                "nmcli",
                &["radio", "wifi", if *enabled { "on" } else { "off" }],
            )?;
        }
        ToggleSetting::Brightness { percent } => {
            let value = format!("{}%", percent);
            run_setting_command("brightnessctl", &["set", &value])?;
        }
        ToggleSetting::Volume { percent } => {
            let value = format!("{}%", percent);
            run_setting_command("pactl", &["set-sink-volume", "@DEFAULT_SINK@", &value])?;
        }
        ToggleSetting::DarkMode { enabled } => {
            run_setting_command(
                "gsettings",
                &[
                    "set",
                    "org.gnome.desktop.interface",
                    "color-scheme",
                    if *enabled { "prefer-dark" } else { "default" },
                ],
            )?;
        }
        ToggleSetting::DoNotDisturb { enabled } => {
            run_setting_command(
                "gsettings",
                &[
                    "set",
                    "org.gnome.desktop.notifications",
                    "show-banners",
                    if *enabled { "false" } else { "true" },
                ],
            )?;
        }
    }
    Ok(ToolExecution {
        tool: "toggle_setting".to_string(),
        summary: format!("Applied: {}", toggle_preview(setting)),
        data: json!({"applied": true}),
    })
}

#[cfg(not(target_os = "linux"))]
fn execute_toggle(_: &ToggleSetting) -> Result<ToolExecution, String> {
    Err("Settings changes are available in the Linux desktop MVP only.".to_string())
}

#[cfg(target_os = "linux")]
fn execute_launch(target: &LaunchTarget) -> Result<ToolExecution, String> {
    let mut command = match target {
        LaunchTarget::FileManager => {
            let mut command = Command::new("xdg-open");
            command.arg(user_root()?);
            command
        }
        LaunchTarget::Browser => {
            let mut command = Command::new("xdg-open");
            command.arg("https://example.com");
            command
        }
        LaunchTarget::Terminal => Command::new("x-terminal-emulator"),
    };
    command.spawn().map_err(|_| {
        format!(
            "Could not launch {}. Check that the desktop application is installed.",
            launch_preview(target)
        )
    })?;
    Ok(ToolExecution {
        tool: "launch_app".to_string(),
        summary: format!("Launched: {}", launch_preview(target)),
        data: json!({"launched": true}),
    })
}

#[cfg(not(target_os = "linux"))]
fn execute_launch(_: &LaunchTarget) -> Result<ToolExecution, String> {
    Err("App launching is available in the Linux desktop MVP only.".to_string())
}

#[cfg(target_os = "linux")]
fn execute_media(command: &MediaCommand) -> Result<ToolExecution, String> {
    let action = match command {
        MediaCommand::Play => "play",
        MediaCommand::Pause => "pause",
        MediaCommand::Next => "next",
        MediaCommand::Previous => "previous",
    };
    let output = Command::new("playerctl").arg(action).output().map_err(|_| {
        "Could not run playerctl. Install it with your distro package manager and start a supported media player."
            .to_string()
    })?;
    if !output.status.success() {
        return Err("No compatible media player is currently available for playerctl.".to_string());
    }
    Ok(ToolExecution {
        tool: "media_control".to_string(),
        summary: format!("Applied: {}", media_preview(command)),
        data: json!({"applied": true, "command": action}),
    })
}

#[cfg(not(target_os = "linux"))]
fn execute_media(_: &MediaCommand) -> Result<ToolExecution, String> {
    Err("Media control is available in the Linux desktop MVP only.".to_string())
}

#[cfg(target_os = "linux")]
fn execute_notification(body: &str) -> Result<ToolExecution, String> {
    let output = Command::new("notify-send")
        .args(["Linux Assistant", body])
        .output()
        .map_err(|_| {
            "Could not run notify-send. Install libnotify-bin or your distro equivalent."
                .to_string()
        })?;
    if !output.status.success() {
        return Err("The Linux desktop notification could not be sent.".to_string());
    }
    Ok(ToolExecution {
        tool: "send_notification".to_string(),
        summary: format!("Sent local notification: {body}"),
        data: json!({"sent": true}),
    })
}

#[cfg(not(target_os = "linux"))]
fn execute_notification(_: &str) -> Result<ToolExecution, String> {
    Err("Desktop notifications are available in the Linux desktop MVP only.".to_string())
}

fn execute_low_risk(intent: &Intent) -> Result<ToolExecution, String> {
    match intent.action {
        Action::SearchFiles => search_files(&intent.params),
        Action::GetSystemInfo => Ok(get_system_info()),
        Action::ListLargeFiles => list_large_files(&intent.params),
        Action::GetNetworkStatus => get_network_status(),
        Action::ReadRecentLogs => read_recent_logs(&intent.params),
        Action::ToggleSetting => {
            Err("Settings changes must pass through the confirmation gate.".to_string())
        }
        Action::LaunchApp => {
            Err("App launching must pass through the confirmation gate.".to_string())
        }
        Action::MediaControl => {
            Err("Media changes must pass through the confirmation gate.".to_string())
        }
        Action::SendNotification => {
            Err("Desktop notifications must pass through the confirmation gate.".to_string())
        }
    }
}

#[tauri::command]
async fn process_request(
    app: AppHandle,
    pending_confirmation: State<'_, PendingConfirmation>,
    request_cancellation: State<'_, RequestCancellation>,
    request: String,
) -> Result<ProcessResponse, String> {
    let (sender, mut receiver) = oneshot::channel();
    {
        let mut current = request_cancellation
            .0
            .lock()
            .map_err(|_| "The request control is unavailable.".to_string())?;
        *current = Some(sender);
    }
    let parsed = parse_intent_internal(app.clone(), request, Some(&mut receiver)).await;
    request_cancellation
        .0
        .lock()
        .map_err(|_| "The request control is unavailable.".to_string())?
        .take();
    let intent = parsed?;
    let audit = AuditContext::from(&intent);
    if intent.clarification_needed {
        let message = intent
            .clarification_question
            .clone()
            .unwrap_or_else(|| "Please clarify your request.".to_string());
        record_audit(
            &app,
            &audit,
            "clarification_requested",
            "no_action",
            &message,
            None,
        )?;
        return Ok(ProcessResponse {
            intent,
            execution: None,
            message,
            confirmation: None,
        });
    }
    let planned_risk = match plan_intent(&intent) {
        Ok(risk) => risk,
        Err(error) => {
            record_audit(&app, &audit, "request_rejected", "rejected", &error, None)?;
            return Err(error);
        }
    };
    match planned_risk {
        RiskTier::Low => {
            record_audit(
                &app,
                &audit,
                "tool_started",
                "pending",
                "Executing a read-only tool.",
                None,
            )?;
            match execute_low_risk(&intent) {
                Ok(execution) => {
                    let result = serde_json::to_value(&execution)
                        .map_err(|_| "Could not serialize the tool result.".to_string())?;
                    record_audit(
                        &app,
                        &audit,
                        "tool_completed",
                        "success",
                        &execution.summary,
                        Some(&result),
                    )?;
                    let message = format!("{} No system changes were made.", execution.summary);
                    Ok(ProcessResponse {
                        intent,
                        execution: Some(execution),
                        message,
                        confirmation: None,
                    })
                }
                Err(error) => {
                    record_audit(&app, &audit, "tool_completed", "failed", &error, None)?;
                    Err(error)
                }
            }
        }
        RiskTier::Medium => {
            let (operation, preview) = match intent.action {
                Action::ToggleSetting => {
                    let setting = prepare_toggle(&intent.params)?;
                    (
                        PendingOperation::Toggle(setting.clone()),
                        toggle_preview(&setting),
                    )
                }
                Action::LaunchApp => {
                    let target = prepare_launch(&intent.params)?;
                    (
                        PendingOperation::Launch(target.clone()),
                        launch_preview(&target),
                    )
                }
                Action::MediaControl => {
                    let command = prepare_media(&intent.params)?;
                    (
                        PendingOperation::Media(command.clone()),
                        media_preview(&command),
                    )
                }
                Action::SendNotification => {
                    let body = prepare_notification(&intent.params)?;
                    let preview = format!("Send a desktop notification: {body}");
                    (PendingOperation::Notification(body), preview)
                }
                _ => {
                    return Err(
                        "Unsupported confirmation operation. No action was taken.".to_string()
                    )
                }
            };
            record_audit(
                &app,
                &audit,
                "confirmation_requested",
                "pending_confirmation",
                &preview,
                None,
            )?;
            let mut pending = pending_confirmation.0.lock().map_err(|_| {
                "The confirmation gate is unavailable. No action was taken.".to_string()
            })?;
            *pending = Some(PendingAction {
                operation,
                audit,
                created_at: Instant::now(),
            });
            Ok(ProcessResponse {
                intent,
                execution: None,
                message: "Confirmation required. Nothing has changed yet.".to_string(),
                confirmation: Some(ConfirmationPreview {
                    summary: preview,
                    expires_in_seconds: CONFIRMATION_TTL.as_secs(),
                }),
            })
        }
        RiskTier::High => {
            let message = "High-risk actions are out of scope for this MVP. Nothing was changed.";
            record_audit(&app, &audit, "request_rejected", "rejected", message, None)?;
            Ok(ProcessResponse {
                intent,
                execution: None,
                message: message.to_string(),
                confirmation: None,
            })
        }
    }
}

#[tauri::command]
fn stop_request(request_cancellation: State<'_, RequestCancellation>) -> Result<String, String> {
    let sender = request_cancellation
        .0
        .lock()
        .map_err(|_| "The request control is unavailable.".to_string())?
        .take();
    if let Some(sender) = sender {
        let _ = sender.send(());
        Ok("Stopping the current request. No action was taken.".to_string())
    } else {
        Ok("There is no request currently waiting for Ollama.".to_string())
    }
}

#[tauri::command]
fn confirm_pending_action(
    app: AppHandle,
    pending_confirmation: State<'_, PendingConfirmation>,
) -> Result<ToolExecution, String> {
    let pending = {
        let mut current = pending_confirmation.0.lock().map_err(|_| {
            "The confirmation gate is unavailable. No action was taken.".to_string()
        })?;
        let pending = current
            .take()
            .ok_or_else(|| "There is no pending action to confirm.".to_string())?;
        if pending.created_at.elapsed() > CONFIRMATION_TTL {
            record_audit(
                &app,
                &pending.audit,
                "confirmation_expired",
                "expired",
                "The confirmation expired before execution.",
                None,
            )?;
            return Err(
                "The confirmation expired after 60 seconds. Submit the request again.".to_string(),
            );
        }
        pending
    };
    record_audit(
        &app,
        &pending.audit,
        "confirmation_accepted",
        "pending_execution",
        "User confirmed the previewed setting change.",
        None,
    )?;
    let execution_result = match &pending.operation {
        PendingOperation::Toggle(setting) => execute_toggle(setting),
        PendingOperation::Launch(target) => execute_launch(target),
        PendingOperation::Media(command) => execute_media(command),
        PendingOperation::Notification(body) => execute_notification(body),
    };
    match execution_result {
        Ok(mut execution) => {
            let result = serde_json::to_value(&execution)
                .map_err(|_| "Could not serialize the tool result.".to_string())?;
            if let Err(error) = record_audit(
                &app,
                &pending.audit,
                "tool_completed",
                "success",
                &execution.summary,
                Some(&result),
            ) {
                execution.summary = format!("{} Audit warning: {}", execution.summary, error);
            }
            Ok(execution)
        }
        Err(error) => {
            record_audit(
                &app,
                &pending.audit,
                "tool_completed",
                "failed",
                &error,
                None,
            )?;
            Err(error)
        }
    }
}

#[tauri::command]
fn cancel_pending_action(
    app: AppHandle,
    pending_confirmation: State<'_, PendingConfirmation>,
) -> Result<String, String> {
    let mut current = pending_confirmation
        .0
        .lock()
        .map_err(|_| "The confirmation gate is unavailable.".to_string())?;
    if let Some(pending) = current.take() {
        record_audit(
            &app,
            &pending.audit,
            "confirmation_cancelled",
            "cancelled",
            "User cancelled the previewed setting change.",
            None,
        )?;
        Ok("Pending setting change cancelled. Nothing was changed.".to_string())
    } else {
        Ok("There was no pending setting change.".to_string())
    }
}

#[tauri::command]
fn get_audit_history(app: AppHandle, limit: Option<u32>) -> Result<Vec<AuditEntry>, String> {
    let connection = open_audit_database(&app)?;
    let limit = i64::from(limit.unwrap_or(20).clamp(1, 100));
    let mut statement = connection
        .prepare(
            "SELECT id, timestamp, event_type, action, risk_tier, params_json, outcome, summary, result_json
             FROM audit_events ORDER BY id DESC LIMIT ?1",
        )
        .map_err(|_| "Could not read the local audit history.".to_string())?;
    let events = statement
        .query_map(params![limit], |row| {
            Ok(AuditEntry {
                id: row.get(0)?,
                timestamp: row.get(1)?,
                event_type: row.get(2)?,
                action: row.get(3)?,
                risk_tier: row.get(4)?,
                params_json: row.get(5)?,
                outcome: row.get(6)?,
                summary: row.get(7)?,
                result_json: row.get(8)?,
            })
        })
        .map_err(|_| "Could not read the local audit history.".to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Could not read the local audit history.".to_string())?;
    Ok(events)
}

#[tauri::command]
fn get_runtime_profile() -> RuntimeProfile {
    let mut system = System::new_all();
    system.refresh_memory();
    let total_memory_gb = system.total_memory() / 1024 / 1024 / 1024;
    let cpu_cores = system.cpus().len();
    let profile = if total_memory_gb >= 12 && cpu_cores >= 6 {
        "full"
    } else {
        "lite"
    };
    let summary = if profile == "full" {
        "Full Desktop profile recommended. This system is suitable for richer local assistant features."
    } else {
        "Lite profile recommended. Fast local actions are enabled; larger voice and chat models may need more RAM or a GPU."
    };
    RuntimeProfile {
        profile: profile.to_string(),
        total_memory_gb,
        cpu_cores,
        summary: summary.to_string(),
    }
}

#[tauri::command]
fn remember_preference(
    app: AppHandle,
    category: String,
    memory_key: String,
    value: String,
) -> Result<MemoryEntry, String> {
    let category = category.trim();
    let memory_key = memory_key.trim();
    let value = value.trim();
    if category.is_empty() || memory_key.is_empty() || value.is_empty() || value.len() > 500 {
        return Err("Memory needs a category, key, and a value under 500 characters.".to_string());
    }
    let connection = open_audit_database(&app)?;
    let created_at = Utc::now().to_rfc3339();
    connection
        .execute(
            "INSERT INTO assistant_memory (created_at, category, memory_key, value) VALUES (?1, ?2, ?3, ?4)",
            params![created_at, category, memory_key, value],
        )
        .map_err(|_| "Could not save the local preference.".to_string())?;
    Ok(MemoryEntry {
        id: connection.last_insert_rowid(),
        created_at,
        category: category.to_string(),
        memory_key: memory_key.to_string(),
        value: value.to_string(),
    })
}

#[tauri::command]
fn get_memory(app: AppHandle) -> Result<Vec<MemoryEntry>, String> {
    let connection = open_audit_database(&app)?;
    let mut statement = connection
        .prepare("SELECT id, created_at, category, memory_key, value FROM assistant_memory ORDER BY id DESC LIMIT 100")
        .map_err(|_| "Could not read local memory.".to_string())?;
    statement
        .query_map([], |row| {
            Ok(MemoryEntry {
                id: row.get(0)?,
                created_at: row.get(1)?,
                category: row.get(2)?,
                memory_key: row.get(3)?,
                value: row.get(4)?,
            })
        })
        .map_err(|_| "Could not read local memory.".to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Could not read local memory.".to_string())
}

#[tauri::command]
fn forget_memory(app: AppHandle, id: i64) -> Result<String, String> {
    let connection = open_audit_database(&app)?;
    let removed = connection
        .execute("DELETE FROM assistant_memory WHERE id = ?1", params![id])
        .map_err(|_| "Could not remove the local memory entry.".to_string())?;
    Ok(if removed == 1 {
        "Memory entry removed."
    } else {
        "Memory entry was not found."
    }
    .to_string())
}

#[tauri::command]
fn delete_all_memory(app: AppHandle) -> Result<String, String> {
    let connection = open_audit_database(&app)?;
    connection
        .execute("DELETE FROM assistant_memory", [])
        .map_err(|_| "Could not clear local memory.".to_string())?;
    Ok("All opt-in assistant memory was deleted.".to_string())
}

#[tauri::command]
fn get_chat_history(app: AppHandle, limit: Option<u32>) -> Result<Vec<ChatEntry>, String> {
    let connection = open_audit_database(&app)?;
    let limit = i64::from(limit.unwrap_or(50).clamp(1, 200));
    let mut statement = connection
        .prepare("SELECT id, created_at, role, content FROM chat_history ORDER BY id DESC LIMIT ?1")
        .map_err(|_| "Could not read local chat history.".to_string())?;
    let mut entries = statement
        .query_map(params![limit], |row| {
            Ok(ChatEntry {
                id: row.get(0)?,
                created_at: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
            })
        })
        .map_err(|_| "Could not read local chat history.".to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Could not read local chat history.".to_string())?;
    entries.reverse();
    Ok(entries)
}

#[tauri::command]
fn delete_chat_history(app: AppHandle) -> Result<String, String> {
    let connection = open_audit_database(&app)?;
    connection
        .execute("DELETE FROM chat_history", [])
        .map_err(|_| "Could not clear local chat history.".to_string())?;
    Ok("Local chat history was deleted.".to_string())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(PendingConfirmation::default())
        .manage(RequestCancellation::default())
        .invoke_handler(tauri::generate_handler![
            parse_intent,
            chat_with_assistant,
            process_request,
            stop_request,
            confirm_pending_action,
            cancel_pending_action,
            get_audit_history,
            get_runtime_profile,
            get_voice_status,
            speak_text,
            remember_preference,
            get_memory,
            forget_memory,
            delete_all_memory,
            get_chat_history,
            delete_chat_history
        ])
        .run(tauri::generate_context!())
        .expect("error while running AI Native Control Layer");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent(action: Action, params: IntentParams, risk_tier: RiskTier) -> Intent {
        Intent {
            action,
            params,
            risk_tier,
            confidence: 0.95,
            clarification_needed: false,
            clarification_question: None,
        }
    }

    #[test]
    fn planner_accepts_a_scoped_file_search() {
        let result = plan_intent(&intent(
            Action::SearchFiles,
            IntentParams {
                query: Some("invoice".to_string()),
                ..Default::default()
            },
            RiskTier::Low,
        ));
        assert!(matches!(result, Ok(RiskTier::Low)));
    }

    #[test]
    fn planner_rejects_missing_search_criteria() {
        let result = plan_intent(&intent(
            Action::SearchFiles,
            IntentParams::default(),
            RiskTier::Low,
        ));
        assert!(result.is_err());
    }

    #[test]
    fn planner_does_not_trust_a_model_selected_risk_level() {
        let result = plan_intent(&intent(
            Action::ToggleSetting,
            IntentParams {
                setting_name: Some("wifi".to_string()),
                value: Some(json!(false)),
                ..Default::default()
            },
            RiskTier::Low,
        ));
        assert!(result.is_err());
    }

    #[test]
    fn planner_allows_only_whitelisted_medium_risk_settings() {
        let result = plan_intent(&intent(
            Action::ToggleSetting,
            IntentParams {
                setting_name: Some("dark_mode".to_string()),
                value: Some(json!(true)),
                ..Default::default()
            },
            RiskTier::Medium,
        ));
        assert!(matches!(result, Ok(RiskTier::Medium)));
    }

    #[test]
    fn brightness_requires_a_bounded_integer() {
        let result = prepare_toggle(&IntentParams {
            setting_name: Some("brightness".to_string()),
            value: Some(json!(101)),
            ..Default::default()
        });
        assert!(result.is_err());
    }

    #[test]
    fn preview_shows_the_exact_setting_change() {
        let setting = prepare_toggle(&IntentParams {
            setting_name: Some("wifi".to_string()),
            value: Some(json!(false)),
            ..Default::default()
        })
        .expect("Wi-Fi should be a supported setting");
        assert_eq!(toggle_preview(&setting), "Turn Wi-Fi off.");
    }

    #[test]
    fn app_launch_requests_are_rejected_before_the_model() {
        let result = unsupported_app_request("open the file manager");
        assert!(result.is_some_and(|intent| intent.clarification_needed));
    }

    #[test]
    fn greetings_return_a_helpful_clarification_without_ollama() {
        let intent = small_talk_request("Hi!").expect("greeting should be handled locally");
        assert!(intent.clarification_needed);
        assert!(intent
            .clarification_question
            .is_some_and(|message| message.contains("safely check")));
    }

    #[test]
    fn chat_mode_handles_common_small_talk_without_ollama() {
        assert!(local_chat_reply("hi").is_some());
        assert!(local_chat_reply("thank you").is_some());
        assert!(local_chat_reply("what can you do?").is_some());
        assert!(local_chat_reply("explain the Linux kernel").is_none());
    }

    #[test]
    fn local_whitelisted_setting_request_bypasses_the_model() {
        let intent = local_toggle_request("turn on the dark mode")
            .expect("dark mode should be parsed locally");
        assert!(matches!(intent.action, Action::ToggleSetting));
        assert!(matches!(intent.risk_tier, RiskTier::Medium));
        assert_eq!(intent.params.setting_name.as_deref(), Some("dark_mode"));
        assert_eq!(intent.params.value, Some(json!(true)));
    }

    #[test]
    fn local_media_requests_bypass_the_model_and_require_confirmation() {
        let intent = local_media_request("skip to the next song")
            .expect("next-track request should be parsed locally");
        assert!(matches!(intent.action, Action::MediaControl));
        assert!(matches!(plan_intent(&intent), Ok(RiskTier::Medium)));
        assert_eq!(intent.params.media_command.as_deref(), Some("next"));
    }

    #[test]
    fn local_notification_requests_bypass_the_model_and_require_confirmation() {
        let intent = local_notification_request("remind me to drink water")
            .expect("notification request should be parsed locally");
        assert!(matches!(intent.action, Action::SendNotification));
        assert!(matches!(plan_intent(&intent), Ok(RiskTier::Medium)));
        assert_eq!(
            intent.params.notification_body.as_deref(),
            Some("to drink water")
        );
    }

    #[test]
    fn planner_rejects_unrecognised_media_commands() {
        let result = plan_intent(&intent(
            Action::MediaControl,
            IntentParams {
                media_command: Some("volume_up".to_string()),
                ..Default::default()
            },
            RiskTier::Medium,
        ));
        assert!(result.is_err());
    }

    #[test]
    fn light_mode_request_maps_to_dark_mode_off() {
        let intent = local_toggle_request("turn on light mode")
            .expect("light mode should be parsed locally");
        assert_eq!(intent.params.setting_name.as_deref(), Some("dark_mode"));
        assert_eq!(intent.params.value, Some(json!(false)));
    }

    #[test]
    fn volume_request_is_locally_parsed_and_bounded() {
        let intent = local_toggle_request("set volume to 35%")
            .expect("volume request should be parsed locally");
        assert_eq!(intent.params.setting_name.as_deref(), Some("volume"));
        assert_eq!(intent.params.value, Some(json!(35)));
        assert!(local_toggle_request("set volume to 120%").is_none());
    }

    #[test]
    fn large_file_requests_are_parsed_without_ollama() {
        let intent = local_large_file_request("show files larger than 100 MB in my home folder")
            .expect("large-file request should be parsed locally");
        assert!(matches!(intent.action, Action::ListLargeFiles));
        assert_eq!(intent.params.directory.as_deref(), Some("home"));
        assert_eq!(intent.params.threshold_mb, Some(100));
    }

    #[test]
    fn model_selector_prefers_an_installed_small_qwen_model() {
        let models = vec![OllamaModel {
            name: "qwen3:1.7b".to_string(),
        }];
        assert_eq!(
            select_available_model(&models).as_deref(),
            Some("qwen3:1.7b")
        );
    }

    #[test]
    fn audit_database_records_events() {
        let connection = Connection::open_in_memory().expect("in-memory database should open");
        initialize_audit_database(&connection).expect("audit schema should initialize");
        connection
            .execute(
                "INSERT INTO audit_events (timestamp, event_type, action, risk_tier, params_json, outcome, summary, result_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    "2026-07-31T00:00:00Z",
                    "tool_completed",
                    "get_system_info",
                    "low",
                    "{}",
                    "success",
                    "Read system information.",
                    Option::<String>::None,
                ],
            )
            .expect("audit event should insert");
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM audit_events", [], |row| row.get(0))
            .expect("audit count should be readable");
        assert_eq!(count, 1);
    }

    #[test]
    fn collect_files_applies_the_predicate() {
        let directory = env::temp_dir().join(format!(
            "ai-native-control-test-{}",
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("clock should be valid")
                .as_nanos()
        ));
        fs::create_dir_all(&directory).expect("test directory should be created");
        fs::write(directory.join("keep.txt"), "safe").expect("test file should be written");
        fs::write(directory.join("skip.bin"), "safe").expect("test file should be written");

        let mut results = Vec::new();
        collect_files(
            &directory,
            0,
            &|path, _| path.extension().is_some_and(|extension| extension == "txt"),
            &mut results,
        );

        assert_eq!(results.len(), 1);
        assert!(results[0]["path"]
            .as_str()
            .is_some_and(|path| path.ends_with("keep.txt")));
        fs::remove_dir_all(directory).expect("test directory should be removed");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn home_folder_alias_resolves_to_the_user_root() {
        assert_eq!(
            scoped_directory(Some("my home folder")).unwrap(),
            user_root().unwrap()
        );
    }

    #[test]
    fn log_service_names_are_restricted_to_safe_characters() {
        assert!(is_valid_service_name("NetworkManager.service"));
        assert!(!is_valid_service_name("NetworkManager.service; reboot"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn network_parser_handles_colons_in_an_ssid() {
        let status = parse_network_status("yes:home\\:lab:78");
        assert_eq!(status, (true, Some("home:lab".to_string()), Some(78)));
    }
}
