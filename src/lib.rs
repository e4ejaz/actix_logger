#[macro_use]
pub mod logs;

#[macro_export]
macro_rules! log {
    ($level:expr, $($arg:tt)+) => {{
        let log_to_cloudwatch = std::env::var("LOG_TO_CLOUDWATCH").unwrap_or_else(|_| "false".to_string()) == "true";

        if log_to_cloudwatch {
            let message_str = format!($($arg)+);
            let log_stream = $crate::logs::LogStream::from_level(&$level);
            tokio::spawn(async move {
                let _ = $crate::logs::log($level, &message_str, log_stream, file!(), line!()).await;
            });
        } else {
            match $level {
                log::Level::Error => println!("ERROR: {}", format!($($arg)+)),
                log::Level::Warn => println!("WARN: {}", format!($($arg)+)),
                log::Level::Info => println!("INFO: {}", format!($($arg)+)),
                log::Level::Debug => println!("DEBUG: {}", format!($($arg)+)),
                log::Level::Trace => println!("TRACE: {}", format!($($arg)+)),
            }
        }
    }};
}

#[macro_export]
macro_rules! log_custom {
    ($level:expr, $log_stream:expr, $($arg:tt)+) => {{
        let log_to_cloudwatch = std::env::var("LOG_TO_CLOUDWATCH").unwrap_or_else(|_| "false".to_string()) == "true";

        if log_to_cloudwatch {
            let message_str = format!($($arg)+);
            let stream = $crate::logs::LogStream::Custom($log_stream.to_string());
            tokio::spawn(async move {
                let _ = $crate::logs::log($level, &message_str, stream, file!(), line!()).await;
            });
        } else {

            match $level {
                log::Level::Error => println!("{} ::ERROR: {}",$log_stream, format!($($arg)+)),
                log::Level::Warn  => println!("{} ::WARN: {}",$log_stream, format!($($arg)+)),
                log::Level::Info  => println!("{} ::INFO: {}",$log_stream, format!($($arg)+)),
                log::Level::Debug => println!("{} ::DEBUG: {}",$log_stream, format!($($arg)+)),
                log::Level::Trace => println!("{} ::TRACE: {}",$log_stream, format!($($arg)+)),
            }
        }
    }};
}

