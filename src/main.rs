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

// 修复后的心率数据解析函数
fn parse_heart_rate_data(text: &str) -> Result<Vec<HeartRateRecord>, Box<dyn std::error::Error>> {
    let lines: Vec<&str> = text.trim().lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())  // 过滤空行
        .collect();

    let mut records = Vec::new();
    let mut heart_rates = Vec::new();
    let mut timestamps = Vec::new();

    println!("Total non-empty lines: {}", lines.len());

    // 第一步：分别收集心率值和时间戳
    for (i, line) in lines.iter().enumerate() {
        // 尝试解析为心率值（数字）
        if let Ok(heart_rate) = line.parse::<f64>() {
            // 检查心率值的合理范围 (30-220 BPM)
            if heart_rate >= 30.0 && heart_rate <= 220.0 {
                heart_rates.push(heart_rate);
                println!("Found heart rate: {} at line {}", heart_rate, i);
                continue;
            }
        }

        // 尝试解析为时间戳
        if let Some(timestamp) = parse_chinese_datetime(line) {
            timestamps.push(timestamp);
            println!("Found timestamp: {} at line {}", timestamp, i);
            continue;
        }

        // 如果既不是心率也不是时间戳，打印警告
        println!("Warning: Could not parse line {}: '{}'", i, line);
    }

    println!("Found {} heart rates and {} timestamps", heart_rates.len(), timestamps.len());

    // 第二步：配对心率和时间戳
    let pairs_count = heart_rates.len().min(timestamps.len());

    if pairs_count == 0 {
        return Err("No valid heart rate and timestamp pairs found".into());
    }

    // 根据数据格式，可能有几种配对方式：
    // 1. 心率和时间戳按顺序交替出现
    // 2. 所有心率在前，所有时间戳在后
    // 3. 所有时间戳在前，所有心率在后

    // 先尝试按顺序配对
    for i in 0..pairs_count {
        records.push(HeartRateRecord {
            value: heart_rates[i],
            timestamp: timestamps[i],
        });
    }

    // 按时间戳排序，确保数据按时间顺序
    records.sort_by_key(|record| record.timestamp);

    println!("Successfully created {} heart rate records", records.len());

    // 打印前几条记录用于调试
    for (i, record) in records.iter().take(5).enumerate() {
        println!("Record {}: {} BPM at {}", i + 1, record.value, record.timestamp);
    }

    Ok(records)
}

// 解析中文日期时间格式：2025年6月2日 21:28
fn parse_chinese_datetime(datetime_str: &str) -> Option<DateTime<Utc>> {
    // 使用正则表达式解析中文日期格式
    let re = regex::Regex::new(r"(\d{4})年(\d{1,2})月(\d{1,2})日\s+(\d{1,2}):(\d{2})").ok()?;

    if let Some(caps) = re.captures(datetime_str) {
        let year: i32 = caps[1].parse().ok()?;
        let month: u32 = caps[2].parse().ok()?;
        let day: u32 = caps[3].parse().ok()?;
        let hour: u32 = caps[4].parse().ok()?;
        let minute: u32 = caps[5].parse().ok()?;

        // 假设时区为 UTC+8 (中国时区)
        let naive = NaiveDateTime::new(
            chrono::NaiveDate::from_ymd_opt(year, month, day)?,
            chrono::NaiveTime::from_hms_opt(hour, minute, 0)?
        );

        // 转换为 UTC (减去8小时)
        let utc_time = naive - chrono::Duration::hours(8);
        return Some(DateTime::from_naive_utc_and_offset(utc_time, Utc));
    }

    None
}

// 转换为 InfluxDB Line Protocol 格式
fn to_influxdb_line(record: &HeartRateRecord, device_id: &str) -> String {
    let timestamp_ms = record.timestamp.timestamp_millis();

    format!(
        "heart_rate,device_id={} value={} {}",
        device_id.replace(" ", "\\ ").replace(",", "\\,"),
        record.value,
        timestamp_ms
    )
}

// 发送数据到 GreptimeDB
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

// 主要的处理函数
async fn process_heart_rate_text(
    axum::extract::State(app_state): axum::extract::State<AppState>,
    Query(params): Query<QueryParams>,
    body: Bytes,
) -> Result<ResponseJson<ApiResponse>, (StatusCode, String)> {

    // 将字节转换为字符串
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

    // 解析心率数据
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

    // 转换为 InfluxDB Line Protocol
    let lines: Vec<String> = records
        .iter()
        .map(|record| to_influxdb_line(record, &device_id))
        .collect();

    println!("Generated {} InfluxDB lines", lines.len());

    // 只打印前几行用于调试
    println!("First few InfluxDB lines:");
    for (i, line) in lines.iter().take(3).enumerate() {
        println!("  {}: {}", i + 1, line);
    }
    if lines.len() > 3 {
        println!("  ... and {} more lines", lines.len() - 3);
    }

    // 发送到 GreptimeDB
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

// 健康检查端点
async fn health_check() -> &'static str {
    "OK"
}

#[tokio::main]
async fn main() {
    // 从环境变量读取配置
    let greptime_url = std::env::var("GREPTIME_URL")
        .unwrap_or_else(|_| "http://192.168.50.137:14000".to_string());
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