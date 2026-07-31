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

Allowed actions are only: search_files, get_system_info, list_large_files, get_network_status, toggle_setting, read_recent_logs. Any request to open or launch an application (including File Explorer), change an unlisted setting, delete files, install software, or perform an unsupported action MUST set clarification_needed to true. Use action "search_files", params {}, risk_tier "low", confidence below 0.9, and ask the user what supported task they want instead.

Parameter rules: get_system_info and get_network_status require params {}. search_files requires query and/or filters. list_large_files requires directory and threshold_mb. toggle_setting requires setting_name (wifi, brightness, dark_mode, or do_not_disturb) and value. read_recent_logs requires service_name and lines.

Use low risk only for read-only actions; medium only for toggle_setting. Params may use only query, filters, directory, threshold_mb, setting_name, value, service_name, lines. filters may use type, date_range, size_min, size_max, path. confidence MUST be a JSON number from 0 to 1, for example 0.95; never use words such as high, medium, or low. If ambiguous or confidence below 0.9, set clarification_needed true and provide a non-empty clarification_question. Never invent an unlisted action."#
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

fn local_toggle_request(request: &str) -> Option<Intent> {
    let request = request.to_lowercase();
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
        _ => Ok(()),
    }
}

fn planned_risk(action: Action) -> RiskTier {
    match action {
        Action::ToggleSetting => RiskTier::Medium,
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
        ToggleSetting::DarkMode { enabled } => {
            format!("Turn dark mode {}.", if *enabled { "on" } else { "off" })
        }
        ToggleSetting::DoNotDisturb { enabled } => format!(
            "Turn Do Not Disturb {}.",
            if *enabled { "on" } else { "off" }
        ),
    }
}

#[tauri::command]
async fn parse_intent(app: AppHandle, request: String) -> Result<Intent, String> {
    let request = request.trim();
    if request.is_empty() {
        return Err("Enter a request first.".to_string());
    }
    if request.len() > 4_000 {
        return Err("Request is too long (maximum 4,000 characters).".to_string());
    }
    if let Some(clarification) = unsupported_app_request(request) {
        append_debug_log(
            &app,
            request,
            "Locally rejected unsupported app-launch request.",
        );
        return Ok(clarification);
    }
    if let Some(intent) = local_toggle_request(request) {
        append_debug_log(
            &app,
            request,
            "Locally parsed whitelisted setting request before Ollama.",
        );
        return Ok(intent);
    }
    let timeout = ollama_timeout();
    let client = Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|_| "Could not initialize the local Ollama client.".to_string())?;
    let model = selected_ollama_model(&client).await;
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
    let response = client.post(OLLAMA_API_URL).json(&body).send().await.map_err(|error| {
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
    DarkMode { enabled: bool },
    DoNotDisturb { enabled: bool },
}

struct PendingAction {
    setting: ToggleSetting,
    audit: AuditContext,
    created_at: Instant,
}

struct PendingConfirmation(Mutex<Option<PendingAction>>);

impl Default for PendingConfirmation {
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
            CREATE INDEX IF NOT EXISTS audit_events_timestamp_idx ON audit_events(timestamp DESC);",
        )
        .map_err(|_| "Could not initialize the local audit database.".to_string())?;
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
    }
}

#[tauri::command]
async fn process_request(
    app: AppHandle,
    pending_confirmation: State<'_, PendingConfirmation>,
    request: String,
) -> Result<ProcessResponse, String> {
    let intent = parse_intent(app.clone(), request).await?;
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
            let setting = prepare_toggle(&intent.params)?;
            let preview = toggle_preview(&setting);
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
                setting,
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
    match execute_toggle(&pending.setting) {
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

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(PendingConfirmation::default())
        .invoke_handler(tauri::generate_handler![
            parse_intent,
            process_request,
            confirm_pending_action,
            cancel_pending_action,
            get_audit_history
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
    fn local_whitelisted_setting_request_bypasses_the_model() {
        let intent = local_toggle_request("turn on the dark mode")
            .expect("dark mode should be parsed locally");
        assert!(matches!(intent.action, Action::ToggleSetting));
        assert!(matches!(intent.risk_tier, RiskTier::Medium));
        assert_eq!(intent.params.setting_name.as_deref(), Some("dark_mode"));
        assert_eq!(intent.params.value, Some(json!(true)));
    }

    #[test]
    fn light_mode_request_maps_to_dark_mode_off() {
        let intent = local_toggle_request("turn on light mode")
            .expect("light mode should be parsed locally");
        assert_eq!(intent.params.setting_name.as_deref(), Some("dark_mode"));
        assert_eq!(intent.params.value, Some(json!(false)));
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
