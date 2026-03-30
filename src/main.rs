use aws_config::Region;
use aws_credential_types::Credentials;
use aws_sdk_lambda::{
    config::{BehaviorVersion, Builder},
    primitives::Blob,
    types::{InvocationType, LogType},
    Client,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::{env, process};

fn get_input(name: &str) -> Option<String> {
    let key = format!("INPUT_{}", name.to_uppercase().replace('-', "_"));
    env::var(&key).ok().filter(|v| !v.is_empty())
}

fn require_input(name: &str) -> String {
    get_input(name).unwrap_or_else(|| {
        eprintln!("Error: required input '{}' is missing", name);
        process::exit(1);
    })
}

fn set_output(name: &str, value: &str) {
    // GitHub Actions output via $GITHUB_OUTPUT file
    if let Ok(path) = env::var("GITHUB_OUTPUT") {
        use std::fs::OpenOptions;
        use std::io::Write;
        let mut f = OpenOptions::new().append(true).open(path).unwrap();
        writeln!(f, "{}={}", name, value).unwrap();
    } else {
        // Fallback for older runners
        println!("::set-output name={}::{}", name, value);
    }
}

#[tokio::main]
async fn main() {
    let access_key = require_input("AWS_ACCESS_KEY_ID");
    let secret_key = require_input("AWS_SECRET_ACCESS_KEY");
    let session_token = get_input("AWS_SESSION_TOKEN");
    let region = get_input("REGION").unwrap_or_else(|| "us-east-1".to_string());
    let function_name = require_input("FunctionName");
    let invocation_type = get_input("InvocationType").unwrap_or_else(|| "RequestResponse".to_string());
    let log_type = get_input("LogType").unwrap_or_else(|| "None".to_string());
    let payload = get_input("Payload");
    let qualifier = get_input("Qualifier");
    let client_context = get_input("ClientContext");
    let succeed_on_failure = get_input("SUCCEED_ON_FUNCTION_FAILURE")
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let creds = Credentials::new(
        &access_key,
        &secret_key,
        session_token.clone(),
        None,
        "github-action",
    );

    let config = Builder::new()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new(region))
        .credentials_provider(creds)
        .build();

    let client = Client::from_conf(config);

    let inv_type = match invocation_type.as_str() {
        "Event" => InvocationType::Event,
        "DryRun" => InvocationType::DryRun,
        _ => InvocationType::RequestResponse,
    };

    let log_t = match log_type.as_str() {
        "Tail" => LogType::Tail,
        _ => LogType::None,
    };

    let mut req = client
        .invoke()
        .function_name(&function_name)
        .invocation_type(inv_type)
        .log_type(log_t);

    if let Some(p) = payload {
        req = req.payload(Blob::new(p.into_bytes()));
    }
    if let Some(q) = qualifier {
        req = req.qualifier(q);
    }
    if let Some(ctx) = client_context {
        req = req.client_context(ctx);
    }

    let resp = req.send().await.unwrap_or_else(|e| {
        eprintln!("Error invoking Lambda: {}", e);
        let mut source: Option<&dyn std::error::Error> = std::error::Error::source(&e);
        while let Some(err) = source {
            eprintln!("  Caused by: {}", err);
            source = err.source();
        }
        process::exit(1);
    });

    let status_code = resp.status_code();
    let executed_version = resp.executed_version().unwrap_or("$LATEST").to_string();
    let function_error = resp.function_error().map(|s| s.to_string());
    let log_result = resp.log_result().map(|s| s.to_string());

    let payload_str = resp
        .payload()
        .map(|b| String::from_utf8_lossy(b.as_ref()).to_string())
        .unwrap_or_default();

    let payload_value: Value = serde_json::from_str(&payload_str).unwrap_or(Value::Null);

    let mut response = json!({
        "StatusCode": status_code,
        "ExecutedVersion": executed_version,
        "Payload": payload_value,
    });

    if let Some(err) = &function_error {
        response["FunctionError"] = json!(err);
    }
    if let Some(log) = &log_result {
        response["LogResult"] = json!(STANDARD.encode(log));
    }

    let response_str = response.to_string();
    set_output("response", &response_str);
    println!("{}", response_str);

    if function_error.is_some() && !succeed_on_failure {
        eprintln!(
            "Lambda returned a function error: {}",
            function_error.as_deref().unwrap_or("unknown")
        );
        if !payload_str.is_empty() {
            eprintln!("Error details: {}", payload_str);
        }
        process::exit(1);
    }
}
