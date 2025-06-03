use axum::{
    body::Bytes,
    extract::Query,
    http::StatusCode,
    response::Json as ResponseJson,
    routing::post,
    Router,
};
use serde::{Deserialize, Serialize};
use tokio;
use tower::ServiceBuilder;
use tower_http::cors::{Any, CorsLayer};
use chrono::{DateTime, Utc, NaiveDateTime, TimeZone};
use reqwest::Client;

#[derive(Debug, Deserialize)]
struct QueryParams {
    device_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct ApiResponse {
    success: bool,
    message: String,
    processed_count: usize,
}

#[derive(Debug, Clone)]
struct AppState {
    greptime_url: String,
    greptime_db: String,
    http_client: Client,
}

impl AppState {
    fn new(greptime_url: String, greptime_db: String) -> Self {
        Self {
            greptime_url,
            greptime_db,
            http_client: Client::new(),
        }
    }
}

#[derive(Debug)]
struct HeartRateRecord {
    value: f64,
    timestamp: DateTime<Utc>,
}

// Fixed heart rate data parsing function
fn parse_heart_rate_data(text: &str) -> Result<Vec<HeartRateRecord>, Box<dyn std::error::Error>> {
    let lines: Vec<&str> = text.trim().lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())  // Filter empty lines
        .collect();

    let mut records = Vec::new();
    let mut heart_rates = Vec::new();
    let mut timestamps = Vec::new();

    println!("Total non-empty lines: {}", lines.len());

    // Step 1: Collect heart rate values and timestamps separately
    for (i, line) in lines.iter().enumerate() {
        // Try to parse as heart rate value (number)
        if let Ok(heart_rate) = line.parse::<f64>() {
            // Check reasonable heart rate range (30-220 BPM)
            if heart_rate >= 30.0 && heart_rate <= 220.0 {
                heart_rates.push(heart_rate);
                println!("Found heart rate: {} at line {}", heart_rate, i);
                continue;
            }
        }

        // Try to parse as timestamp
        if let Some(timestamp) = parse_chinese_datetime(line) {
            timestamps.push(timestamp);
            println!("Found timestamp: {} at line {}", timestamp, i);
            continue;
        }

        // If neither heart rate nor timestamp, print warning
        println!("Warning: Could not parse line {}: '{}'", i, line);
    }

    println!("Found {} heart rates and {} timestamps", heart_rates.len(), timestamps.len());

    // Step 2: Pair heart rates and timestamps
    let pairs_count = heart_rates.len().min(timestamps.len());

    if pairs_count == 0 {
        return Err("No valid heart rate and timestamp pairs found".into());
    }

    // Based on data format, there might be several pairing methods:
    // 1. Heart rates and timestamps appear alternately in sequence
    // 2. All heart rates first, all timestamps after
    // 3. All timestamps first, all heart rates after

    // First try to pair in sequence
    for i in 0..pairs_count {
        records.push(HeartRateRecord {
            value: heart_rates[i],
            timestamp: timestamps[i],
        });
    }

    // Sort by timestamp to ensure data is in chronological order
    records.sort_by_key(|record| record.timestamp);

    println!("Successfully created {} heart rate records", records.len());

    // Print first few records for debugging
    for (i, record) in records.iter().take(5).enumerate() {
        println!("Record {}: {} BPM at {}", i + 1, record.value, record.timestamp);
    }

    Ok(records)
}

// Parse Chinese datetime format: 2025年6月2日 21:28
fn parse_chinese_datetime(datetime_str: &str) -> Option<DateTime<Utc>> {
    // Use regex to parse Chinese date format
    let re = regex::Regex::new(r"(\d{4})年(\d{1,2})月(\d{1,2})日\s+(\d{1,2}):(\d{2})").ok()?;

    if let Some(caps) = re.captures(datetime_str) {
        let year: i32 = caps[1].parse().ok()?;
        let month: u32 = caps[2].parse().ok()?;
        let day: u32 = caps[3].parse().ok()?;
        let hour: u32 = caps[4].parse().ok()?;
        let minute: u32 = caps[5].parse().ok()?;

        // Assume timezone is UTC+8 (China timezone)
        let naive = NaiveDateTime::new(
            chrono::NaiveDate::from_ymd_opt(year, month, day)?,
            chrono::NaiveTime::from_hms_opt(hour, minute, 0)?
        );

        // Convert to UTC (subtract 8 hours)
        let utc_time = naive - chrono::Duration::hours(8);
        return Some(DateTime::from_naive_utc_and_offset(utc_time, Utc));
    }

    None
}

// Convert to InfluxDB Line Protocol format
fn to_influxdb_line(record: &HeartRateRecord, device_id: &str) -> String {
    let timestamp_ms = record.timestamp.timestamp_millis();

    format!(
        "heart_rate,device_id={} value={} {}",
        device_id.replace(" ", "\\ ").replace(",", "\\,"),
        record.value,
        timestamp_ms
    )
}

// Send data to GreptimeDB
async fn send_to_greptime(
    app_state: &AppState,
    lines: Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let body = lines.join("\n");

    let url = format!(
        "{}/v1/influxdb/api/v2/write?db={}&precision=ms",
        app_state.greptime_url,
        app_state.greptime_db
    );

    println!("Sending to GreptimeDB: {}", url);
    println!("Sending {} lines of data", lines.len());

    let response = app_state
        .http_client
        .post(&url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await?;

    if !response.status().is_success() {
        let error_text = response.text().await.unwrap_or_default();
        return Err(format!("GreptimeDB error: {}", error_text).into());
    }

    println!("Successfully sent to GreptimeDB");
    Ok(())
}

// Main processing function
async fn process_heart_rate_text(
    axum::extract::State(app_state): axum::extract::State<AppState>,
    Query(params): Query<QueryParams>,
    body: Bytes,
) -> Result<ResponseJson<ApiResponse>, (StatusCode, String)> {

    // Convert bytes to string
    let text = match String::from_utf8(body.to_vec()) {
        Ok(text) => text,
        Err(e) => {
            return Err((StatusCode::BAD_REQUEST, format!("Invalid UTF-8: {}", e)));
        }
    };

    let device_id = params.device_id.unwrap_or_else(|| "apple-watch".to_string());

    println!("=== Received Heart Rate Data ===");
    println!("Device ID: {}", device_id);
    println!("Data length: {} characters", text.len());
    println!("First 500 characters of raw data:\n{}",
             if text.len() > 500 { &text[..500] } else { &text });

    // Parse heart rate data
    let records = match parse_heart_rate_data(&text) {
        Ok(records) => records,
        Err(e) => {
            eprintln!("Failed to parse heart rate data: {}", e);
            return Err((StatusCode::BAD_REQUEST, format!("Parse error: {}", e)));
        }
    };

    println!("Parsed {} heart rate records", records.len());

    if records.is_empty() {
        return Ok(ResponseJson(ApiResponse {
            success: false,
            message: "No valid heart rate records found".to_string(),
            processed_count: 0,
        }));
    }

    // Convert to InfluxDB Line Protocol
    let lines: Vec<String> = records
        .iter()
        .map(|record| to_influxdb_line(record, &device_id))
        .collect();

    println!("Generated {} InfluxDB lines", lines.len());

    // Only print first few lines for debugging
    println!("First few InfluxDB lines:");
    for (i, line) in lines.iter().take(3).enumerate() {
        println!("  {}: {}", i + 1, line);
    }
    if lines.len() > 3 {
        println!("  ... and {} more lines", lines.len() - 3);
    }

    // Send to GreptimeDB
    if let Err(e) = send_to_greptime(&app_state, lines).await {
        eprintln!("Failed to send to GreptimeDB: {}", e);
        return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("GreptimeDB error: {}", e)));
    }

    println!("=== Processing Complete ===");

    Ok(ResponseJson(ApiResponse {
        success: true,
        message: format!("Successfully processed {} heart rate records", records.len()),
        processed_count: records.len(),
    }))
}

async fn health_check() -> &'static str {
    "OK"
}

#[tokio::main]
async fn main() {
    // Read configuration from environment variables
    let greptime_url = std::env::var("GREPTIME_URL")
        .unwrap_or_else(|_| "http://127.0.0.1".to_string());
    let greptime_db = std::env::var("GREPTIME_DB")
        .unwrap_or_else(|_| "heartbeat_test".to_string());
    let port = std::env::var("PORT")
        .unwrap_or_else(|_| "3000".to_string())
        .parse::<u16>()
        .unwrap_or(3000);

    let app_state = AppState::new(greptime_url.clone(), greptime_db.clone());

    println!("Starting heart rate proxy server...");
    println!("GreptimeDB URL: {}", greptime_url);
    println!("Database: {}", greptime_db);
    println!("Server port: {}", port);

    let app = Router::new()
        .route("/heart-rate", post(process_heart_rate_text))
        .route("/health", axum::routing::get(health_check))
        .layer(
            ServiceBuilder::new()
                .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any))
        )
        .with_state(app_state);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port))
        .await
        .unwrap();

    println!("Server running on http://0.0.0.0:{}", port);

    axum::serve(listener, app).await.unwrap();
}