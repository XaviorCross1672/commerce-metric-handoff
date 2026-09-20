use std::{error::Error, sync::Arc};

use commerce_metric_handoff::{
    commerce_metrics::{record_order, CheckoutError, OrderEvent},
    infrai_metrics::InfraiMetrics,
};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let metrics = Arc::new(InfraiMetrics::from_env()?);
    let listener = TcpListener::bind("127.0.0.1:8080").await?;
    println!("order metrics listening on http://127.0.0.1:8080");
    loop {
        let (stream, _) = listener.accept().await?;
        let metrics = Arc::clone(&metrics);
        tokio::spawn(async move {
            if let Err(error) = serve(stream, metrics).await {
                eprintln!("connection ended: {error}");
            }
        });
    }
}

async fn serve(mut stream: TcpStream, metrics: Arc<InfraiMetrics>) -> Result<(), Box<dyn Error>> {
    let mut request = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
        if complete_request(&request) {
            break;
        }
    }

    let (status, body) = match parse_order(&request) {
        Ok(order) => match record_order(metrics.as_ref(), order).await {
            Ok(update) => (200, serde_json::to_value(update)?),
            Err(CheckoutError::InvalidOrder) => (400, json!({ "error": "invalid order" })),
            Err(CheckoutError::Metrics(error)) => {
                (error_status(&error), json!({ "error": error.to_string() }))
            }
        },
        Err(message) => (400, json!({ "error": message })),
    };
    let payload = serde_json::to_vec(&body)?;
    let reason = if status == 200 {
        "OK"
    } else if status == 429 {
        "Too Many Requests"
    } else {
        "Bad Request"
    };
    let head = format!("HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", payload.len());
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&payload).await?;
    Ok(())
}

fn parse_order(request: &[u8]) -> Result<OrderEvent, &'static str> {
    let boundary = request
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("incomplete request")?;
    let head = std::str::from_utf8(&request[..boundary]).map_err(|_| "invalid request")?;
    if !head.starts_with("POST /orders HTTP/1.1") {
        return Err("use POST /orders");
    }
    serde_json::from_slice(&request[boundary + 4..]).map_err(|_| "invalid order JSON")
}

fn complete_request(request: &[u8]) -> bool {
    let Some(boundary) = request.windows(4).position(|w| w == b"\r\n\r\n") else {
        return false;
    };
    let Ok(head) = std::str::from_utf8(&request[..boundary]) else {
        return true;
    };
    let length = head
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(str::trim)
                .and_then(|v| v.parse::<usize>().ok())
        })
        .unwrap_or(0);
    request.len() >= boundary + 4 + length
}

fn error_status(error: &commerce_metric_handoff::infrai_metrics::MetricsError) -> u16 {
    match error {
        commerce_metric_handoff::infrai_metrics::MetricsError::Rejected { status, .. }
            if *status < 500 =>
        {
            *status
        }
        _ => 502,
    }
}
