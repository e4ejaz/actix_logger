use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::env;
use std::error;
use std::sync::Arc;
use tokio::sync::RwLock;

use aws_config::{Region, SdkConfig};
use aws_sdk_cloudwatchlogs::config::{Credentials, SharedCredentialsProvider};
use aws_sdk_cloudwatchlogs::operation::put_log_events::PutLogEventsError;
use aws_sdk_cloudwatchlogs::{types::InputLogEvent, Client as CloudWatchLogsClient};

use chrono::Utc;
use env_logger::Builder;
use log::Level;

/// This static cache keeps track of whether a log group exists, avoiding repeated Describe calls.
static GROUP_EXISTS_CACHE: Lazy<RwLock<HashMap<String, bool>>> = Lazy::new(|| RwLock::new(HashMap::new()));

/// This static cache keeps track of whether a particular log stream exists, avoiding repeated Describe calls.
static STREAM_EXISTS_CACHE: Lazy<RwLock<HashMap<String, bool>>> = Lazy::new(|| RwLock::new(HashMap::new()));

/// A static map of log-stream keys to their latest sequence tokens, allowing for proper CloudWatch log event ordering.
static NEXT_SEQUENCE_TOKENS: Lazy<RwLock<HashMap<String, Option<String>>>> = Lazy::new(|| RwLock::new(HashMap::new()));

/// A globally shared client for CloudWatch Logs. This approach ensures that only one client instance is created
/// and reused throughout the program lifecycle. We retrieve necessary credentials and region info from the environment.
static GLOBAL_CLIENT: Lazy<Arc<CloudWatchLogsClient>> = Lazy::new(|| {
    let region_str = env::var("CLOUDWATCH_AWS_REGION").unwrap_or_else(|_| "us-east-1".to_string());
    let region = Region::new(region_str);

    let access_key = env::var("CLOUDWATCH_AWS_ACCESS_KEY").unwrap_or_else(|_| "MISSING_KEY".to_string());
    let secret_key = env::var("CLOUDWATCH_AWS_SECRET_KEY").unwrap_or_else(|_| "MISSING_SECRET".to_string());

    // Construct AWS credentials provider
    let credentials = Credentials::new(access_key, secret_key, None, None, "default");
    let creds_provider = SharedCredentialsProvider::new(credentials);

    // Build the overall configuration, specifying region and credentials
    let config = SdkConfig::builder().region(region).credentials_provider(creds_provider).build();

    // Wrap the CloudWatchLogsClient in an Arc so it can be cloned and shared safely
    Arc::new(CloudWatchLogsClient::new(&config))
});

/// Custom error type for log-related failures, such as missing environment variables or AWS configuration issues.
#[derive(Debug)]
pub enum Error {
    EnvVarMissing(String),
    AwsConfig,
}

/// Represents different categories of log streams. This enum is used for mapping log levels or custom stream names
/// to appropriate CloudWatch log streams. Each variant holds a string representation used for naming the log streams.
pub enum LogStream {
    ServerResponses,
    ClientResponses,
    RedirectionResponses,
    SuccessfulResponses,
    InformationalResponses,
    UnknownOrUnassigned,
    Custom(String),
}

impl LogStream {
    /// Returns the base string for each log stream variant, which is used to build final CloudWatch stream names.
    fn as_str(&self) -> &str {
        match *self {
            LogStream::ServerResponses => "Server_Responses",
            LogStream::ClientResponses => "Client_Responses",
            LogStream::RedirectionResponses => "Redirection_Responses",
            LogStream::SuccessfulResponses => "Successful_Responses",
            LogStream::InformationalResponses => "Informational_Responses",
            LogStream::UnknownOrUnassigned => "Unknown_Or_Unassigned",
            LogStream::Custom(ref s) => s,
        }
    }

    /// Construct a `LogStream` from a given string. If the string matches any predefined variant,
    /// it returns that variant; otherwise, it returns a `Custom` variant.
    pub fn from_string(stream_name: String) -> Self {
        match stream_name.as_str() {
            "Server_Responses" => LogStream::ServerResponses,
            "Client_Responses" => LogStream::ClientResponses,
            "Redirection_Responses" => LogStream::RedirectionResponses,
            "Successful_Responses" => LogStream::SuccessfulResponses,
            "Informational_Responses" => LogStream::InformationalResponses,
            "Unknown_Or_Unassigned" => LogStream::UnknownOrUnassigned,
            _ => LogStream::Custom(stream_name),
        }
    }

    /// Generates a date-based log stream name for daily partitioning, e.g. `"2025-01-21-Server_Responses"`.
    fn with_date(&self) -> String {
        let current_date = Utc::now().format("%Y-%m-%d").to_string();
        format!("{}-{}", current_date, self.as_str())
    }

    /// Maps a Rust `log::Level` to one of our log stream variants.
    pub fn from_level(level: &Level) -> LogStream {
        match level {
            Level::Error => LogStream::ServerResponses,
            Level::Warn => LogStream::ClientResponses,
            Level::Info => LogStream::InformationalResponses,
            Level::Debug => LogStream::ServerResponses,
            Level::Trace => LogStream::ServerResponses,
        }
    }
}

/// Sends a custom log message to CloudWatch if the `AWS_LOG_GROUP` environment variable is set,
/// otherwise returning an error if it is missing. This function ensures the log group and log stream
/// exist, and then performs the actual PutLogEvents API call with proper sequence token handling.
///
/// # Arguments
/// * `level` - The log level (e.g., `Level::Info`)
/// * `message` - The log message to be recorded
/// * `log_stream` - The stream category or custom name
/// * `file` - The source file where the log occurred
/// * `line` - The line number in the source file
pub async fn custom_cloudwatch_log(level: Level, message: &str, log_stream: LogStream, file: &str, line: u32) -> Result<(), Error> {
    let log_group_name = match env::var("AWS_LOG_GROUP") {
        Ok(name) => name,
        Err(_) => return Err(Error::EnvVarMissing("AWS_LOG_GROUP".to_string())),
    };

    // Build the final stream name (typically daily-based)
    let log_stream_name = log_stream.with_date();

    // Construct the full message including file and line info
    let msg_str = format!("{} - {} (File: {}, Line: {})", level, message, file, line);

    // Get the globally shared client
    let client = GLOBAL_CLIENT.clone();

    // Ensure that the log group and stream exist before logging
    match ensure_log_stream_exists(&client, &log_group_name, &log_stream_name).await {
        Ok(_) => (),
        Err(_) => return Err(Error::AwsConfig),
    }

    // Perform the log event submission with retry on invalid sequence token
    match handle_logging_operation(client, log_group_name, log_stream_name, msg_str).await {
        Ok(_) => Ok(()),
        Err(_) => Err(Error::AwsConfig),
    }
}

/// Internal function to handle constructing the `InputLogEvent` and calling the retry logic.
async fn handle_logging_operation(client: Arc<CloudWatchLogsClient>, group: String, stream: String, message: String) -> Result<(), Box<dyn error::Error + Send + Sync>> {
    // Build a single log event with current timestamp
    let log_event = InputLogEvent::builder().message(message).timestamp(Utc::now().timestamp_millis()).build().expect("Failed to build log event");

    // Attempt to put the log event with proper sequence token handling
    put_log_events_with_retry(&client, &group, &stream, log_event).await?;
    Ok(())
}

/// Puts log events, retrying once if we receive an `InvalidSequenceTokenException`.
/// This handles the "sequence token mismatch" scenario by fetching the latest token and retrying.
///
/// # Arguments
/// * `client` - The CloudWatchLogs client
/// * `group` - The log group name
/// * `stream` - The log stream name
/// * `log_event` - The `InputLogEvent` to be sent
async fn put_log_events_with_retry(client: &CloudWatchLogsClient, group: &str, stream: &str, log_event: InputLogEvent) -> Result<(), Box<dyn error::Error + Send + Sync>> {
    let key = format!("{}::{}", group, stream);

    // Retrieve any stored sequence token for this log stream
    let token_opt = {
        let map = NEXT_SEQUENCE_TOKENS.read().await;
        map.get(&key).cloned().unwrap_or(None)
    };

    // First attempt
    match put_log_events_once(client, group, stream, token_opt, vec![log_event.clone()]).await {
        Ok(new_tok) => {
            update_sequence_token(key, new_tok).await;
            Ok(())
        }
        Err(e) => {
            // If there's an invalid sequence token error, fetch the latest token and retry once
            if let Some(PutLogEventsError::InvalidSequenceTokenException(_)) = e.downcast_ref::<PutLogEventsError>() {
                let fresh = fetch_latest_stream_token(client, group, stream).await;
                match put_log_events_once(client, group, stream, fresh.clone(), vec![log_event]).await {
                    Ok(new_tok2) => {
                        update_sequence_token(format!("{}::{}", group, stream), new_tok2).await;
                        Ok(())
                    }
                    Err(e2) => Err(e2),
                }
            } else {
                Err(e)
            }
        }
    }
}

/// Sends the log events to CloudWatch once using the specified sequence token (if any).
/// If the call succeeds, it returns the new token included in the response.
///
/// # Arguments
/// * `client` - The CloudWatchLogs client
/// * `group` - The log group name
/// * `stream` - The log stream name
/// * `sequence_token` - Current sequence token if available
/// * `events` - Vector of `InputLogEvent`
async fn put_log_events_once(client: &CloudWatchLogsClient, group: &str, stream: &str, sequence_token: Option<String>, events: Vec<InputLogEvent>) -> Result<Option<String>, Box<dyn error::Error + Send + Sync>> {
    let mut req = client.put_log_events().log_group_name(group).log_stream_name(stream);

    if let Some(tok) = sequence_token {
        req = req.sequence_token(tok);
    }

    for ev in events {
        req = req.log_events(ev);
    }

    let resp = req.send().await?;
    let next_tok = resp.next_sequence_token().map(|s| s.to_string());
    Ok(next_tok)
}

/// Fetches the latest sequence token from AWS for the given log stream. This is called after detecting an
/// `InvalidSequenceTokenException` to ensure the next attempt uses the correct token.
///
/// # Arguments
/// * `client` - The CloudWatchLogs client
/// * `group` - The log group name
/// * `stream` - The log stream name
async fn fetch_latest_stream_token(client: &CloudWatchLogsClient, group: &str, stream: &str) -> Option<String> {
    if let Ok(resp) = client.describe_log_streams().log_group_name(group).log_stream_name_prefix(stream).send().await {
        for s in resp.log_streams() {
            if let Some(name) = s.log_stream_name() {
                if name == stream {
                    return s.upload_sequence_token().map(|st| st.to_string());
                }
            }
        }
    }
    None
}

/// Updates the in-memory sequence token map with the new token if it exists.
///
/// # Arguments
/// * `key` - A string key composed of `log_group::log_stream`
/// * `new_tok` - The new sequence token returned from CloudWatch
async fn update_sequence_token(key: String, new_tok: Option<String>) {
    let mut map = NEXT_SEQUENCE_TOKENS.write().await;
    map.insert(key, new_tok);
}

/// Ensures that the specified log stream exists within the given log group. If not, it creates it.
/// Also updates the STREAM_EXISTS_CACHE upon success, avoiding repeated checks.
///
/// # Arguments
/// * `client` - The CloudWatchLogs client
/// * `group` - The log group name
/// * `stream` - The log stream name
async fn ensure_log_stream_exists(client: &CloudWatchLogsClient, group: &str, stream: &str) -> Result<(), Box<dyn error::Error + Send + Sync>> {
    // Make sure the log group exists first
    ensure_log_group_exists(client, group).await?;

    let key = format!("{}::{}", group, stream);

    // Check cache for stream existence
    {
        let read_map = STREAM_EXISTS_CACHE.read().await;
        if let Some(already_exists) = read_map.get(&key) {
            if *already_exists {
                return Ok(());
            }
        }
    }

    // Not in cache; describe to see if it exists
    let resp = client.describe_log_streams().log_group_name(group).log_stream_name_prefix(stream).send().await?;

    let found = resp.log_streams().iter().any(|s| s.log_stream_name().map(|n| n == stream).unwrap_or(false));

    // Create if not found
    if !found {
        client.create_log_stream().log_group_name(group).log_stream_name(stream).send().await?;
    }

    // Update cache
    {
        let mut write_map = STREAM_EXISTS_CACHE.write().await;
        write_map.insert(key, true);
    }

    Ok(())
}

/// Ensures the specified log group exists, creating it if necessary, and populates the GROUP_EXISTS_CACHE.
///
/// # Arguments
/// * `client` - The CloudWatchLogs client
/// * `group` - The log group name
async fn ensure_log_group_exists(client: &CloudWatchLogsClient, group: &str) -> Result<(), Box<dyn error::Error + Send + Sync>> {
    // Check group cache first
    {
        let read_map = GROUP_EXISTS_CACHE.read().await;
        if let Some(already) = read_map.get(group) {
            if *already {
                return Ok(());
            }
        }
    }

    // If not in cache, describe
    let resp = client.describe_log_groups().log_group_name_prefix(group).send().await?;

    let found = resp.log_groups().iter().any(|g| g.log_group_name().map(|n| n == group).unwrap_or(false));

    // If not found, create the group
    if !found {
        client.create_log_group().log_group_name(group).send().await?;
    }

    // Update group cache
    {
        let mut write_map = GROUP_EXISTS_CACHE.write().await;
        write_map.insert(group.to_string(), true);
    }

    Ok(())
}

/// Initializes logs by setting up env_logger and installing a custom panic hook. The panic hook logs
/// panic info to stderr, including file and line number, which aids in debugging.
pub fn initialize_logs() {
    Builder::from_default_env().init();

    std::panic::set_hook(Box::new(|panic_info| {
        let location = panic_info.location().unwrap();
        let panic_message = if let Some(s) = panic_info.payload().downcast_ref::<&str>() {
            format!("Panic in file '{}' at line {}: {}", location.file(), location.line(), s)
        } else if let Some(s) = panic_info.payload().downcast_ref::<String>() {
            format!("Panic in file '{}' at line {}: {}", location.file(), location.line(), s)
        } else {
            format!("Panic occurred in file '{}' at line {}. The panic message is not a string.", location.file(), location.line())
        };

        eprintln!("PANIC => {}", panic_message);
    }));
}
