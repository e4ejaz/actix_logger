use std::env;
use std::error;

use aws_config::{Region, SdkConfig};
use aws_sdk_cloudwatchlogs::{Client as CloudWatchLogsClient, types::InputLogEvent};
use aws_sdk_cloudwatchlogs::config::{Credentials, ProvideCredentials, SharedCredentialsProvider};
use chrono::Utc;
use env_logger::Builder;
use log::{debug, error, info, trace, warn, Level};
use crate::log as log_macro;

#[derive(Debug)]
pub enum Error {
    EnvVarMissing(String),
    AwsConfig,
}

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

    fn with_date(&self) -> String {
        let current_date = Utc::now().format("%Y-%m-%d").to_string();
        format!("{}-{}", current_date, self.as_str())
    }

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

fn make_provider() -> Result<impl ProvideCredentials, Error> {
    let access_key = match env::var("CLOUDWATCH_AWS_ACCESS_KEY") {
        Ok(key) => key,
        Err(_) => return Err(Error::EnvVarMissing("CLOUDWATCH_AWS_ACCESS_KEY".to_string())),
    };

    let secret_key = match env::var("CLOUDWATCH_AWS_SECRET_KEY") {
        Ok(key) => key,
        Err(_) => return Err(Error::EnvVarMissing("CLOUDWATCH_AWS_SECRET_KEY".to_string())),
    };

    Ok(Credentials::new(
        access_key,
        secret_key,
        None,
        None,
        "default",
    ))
}

pub async fn initialize_cloudwatch_client() -> Result<CloudWatchLogsClient, Error> {
    let aws_region = match env::var("CLOUDWATCH_AWS_REGION") {
        Ok(region) => region,
        Err(_) => return Err(Error::EnvVarMissing("CLOUDWATCH_AWS_REGION is missing".to_string())),
    };

    let region = Region::new(aws_region);

    let provider_result = make_provider()?;
    let credentials_provider = SharedCredentialsProvider::new(provider_result);

    let config = SdkConfig::builder()
        .region(region)
        .credentials_provider(credentials_provider)
        .build();

    let client = CloudWatchLogsClient::new(&config);

    Ok(client)
}

async fn handle_logging_operation(client: CloudWatchLogsClient, log_group_name: String, log_stream_name: String, message: String) -> Result<(), Box<dyn error::Error>> {
    let log_event = InputLogEvent::builder()
        .message(message)
        .timestamp(chrono::Utc::now().timestamp_millis())
        .build()
        .expect("Failed to build log event");

    client.put_log_events()
        .log_events(log_event)
        .log_group_name(log_group_name)
        .log_stream_name(log_stream_name)
        .send()
        .await?;

    Ok(())
}

pub async fn log(
    level: Level,
    message: &str,
    log_stream: LogStream,
    file: &str,
    line: u32
) -> Result<(), Error> {
    let log_group_name = match env::var("AWS_LOG_GROUP") {
        Ok(name) => name,
        Err(_) => return Err(Error::EnvVarMissing("AWS_LOG_GROUP".to_string())),
    };

    let log_stream_name = log_stream.with_date();

    let client = match initialize_cloudwatch_client().await {
        Ok(c) => c,
        Err(_) => return Err(Error::AwsConfig),
    };

    let message_str = format!(
        "{} - {} (File: {}, Line: {})",
        level, message, file, line
    );

    let client_clone = client.clone();
    let log_group_name_clone = log_group_name.clone();
    let log_stream_name_clone = log_stream_name.clone();

    tokio::spawn(async move {
        if ensure_log_stream_exists(&client_clone, &log_group_name_clone, &log_stream_name_clone)
            .await
            .is_ok()
        {
            let _ = handle_logging_operation(client_clone, log_group_name_clone, log_stream_name_clone, message_str).await;
        }
    });

    // Custom stream check
    match level {
        Level::Error => {
            if let LogStream::Custom(ref stream_name) = log_stream {
                error!("Stream [{}] - {}", stream_name, message);
            } else {
                error!("{}", message);
            }
        },
        Level::Warn => {
            if let LogStream::Custom(ref stream_name) = log_stream {
                warn!("Stream [{}] - {}", stream_name, message);
            } else {
                warn!("{}", message);
            }
        },
        Level::Info => {
            if let LogStream::Custom(ref stream_name) = log_stream {
                info!("Stream [{}] - {}", stream_name, message);
            } else {
                info!("{}", message);
            }
        },
        Level::Debug => {
            if let LogStream::Custom(ref stream_name) = log_stream {
                debug!("Stream [{}] - {}", stream_name, message);
            } else {
                debug!("{}", message);
            }
        },
        Level::Trace => {
            if let LogStream::Custom(ref stream_name) = log_stream {
                trace!("Stream [{}] - {}", stream_name, message);
            } else {
                trace!("{}", message);
            }
        },
    }

    Ok(())
}


async fn ensure_log_group_exists(client: &CloudWatchLogsClient, log_group_name: &str) -> Result<(), Box<dyn error::Error>> {
    let resp = client
        .describe_log_groups()
        .log_group_name_prefix(log_group_name)
        .send()
        .await?;

    let groups = resp.log_groups();
    if !groups.is_empty() && groups.iter().any(|group| group.log_group_name().unwrap() == log_group_name) {
        Ok(())
    } else {
        let _new_log_group = client.create_log_group().log_group_name(log_group_name).send().await?;
        Ok(())
    }
}

async fn ensure_log_stream_exists(client: &CloudWatchLogsClient, log_group_name: &str, log_stream_name: &str) -> Result<(), Box<dyn error::Error>> {
    ensure_log_group_exists(client, log_group_name).await?;

    let resp = client
        .describe_log_streams()
        .log_group_name(log_group_name)
        .send()
        .await?;

    let streams = resp.log_streams();
    if !streams.is_empty() && streams.iter().any(|stream| stream.log_stream_name().unwrap() == log_stream_name) {
        Ok(())
    } else {
        let _new_stream = client.create_log_stream()
            .log_group_name(log_group_name)
            .log_stream_name(log_stream_name)
            .send()
            .await?;
        Ok(())
    }
}

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

        tokio::spawn(async move {
            log_macro!(Level::Debug, "{}", &panic_message);
        });
    }));
}
