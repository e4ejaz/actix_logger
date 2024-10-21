Environment Variables

You need to set the following environment variables to use this crate:

```
CLOUDWATCH_AWS_ACCESS_KEY: AWS access key.
CLOUDWATCH_AWS_SECRET_KEY: AWS secret key.
CLOUDWATCH_AWS_REGION: AWS region where you want to upload the logs.
AWS_LOG_GROUP: CloudWatch Logs group name.
LOG_TO_CLOUDWATCH: Set this to true if you want to enable logging to CloudWatch.
```

Logging Macros

You can use the log! macro to generate logs. This macro will automatically check the environment variable and accordingly send logs to CloudWatch or print them to the console.
```
log!(Level::Info, "This is an info message");
log!(Level::Error, "This is an error message");
log!(Level::Debug, "Debugging information");
```
To log in a custom stream (other than info, error and debug) you can use log_custom macro
```
log_custom!(Level::Info,"Custom Stream Name", "This is the message and variable {}",variable);

```