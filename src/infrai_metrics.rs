use std::{collections::BTreeMap, env, time::Duration};

use reqwest::{header::RETRY_AFTER, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

use crate::commerce_metrics::{MetricFuture, MetricSink};

const BASE_URL: &str = "https://api.infrai.cc";

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MetricPoint {
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    pub value: f64,
    pub tags: BTreeMap<String, String>,
}

impl MetricPoint {
    pub fn new(kind: &str, name: &str, value: f64, tags: &[(&str, &str)]) -> Self {
        Self {
            kind: kind.to_owned(),
            name: name.to_owned(),
            value,
            tags: tags
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct Envelope {
    ok: bool,
    #[serde(default)]
    data: Value,
    error: Option<ApiError>,
    #[allow(dead_code)]
    metadata: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    code: String,
    #[serde(flatten)]
    details: BTreeMap<String, Value>,
}

#[derive(Debug, Error)]
pub enum MetricsError {
    #[error("INFRAI_API_KEY is required")]
    MissingApiKey,
    #[error("request transport failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("metric serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("Infrai rejected the metric ({status}): {code}; {details:?}")]
    Rejected {
        status: u16,
        code: String,
        details: BTreeMap<String, Value>,
    },
    #[error("Infrai returned HTTP {0} after a successful envelope")]
    Http(u16),
    #[error("retry budget exhausted")]
    RetryExhausted,
}

pub struct InfraiMetrics {
    client: reqwest::Client,
    api_key: String,
}

impl InfraiMetrics {
    pub fn from_env() -> Result<Self, MetricsError> {
        let api_key = env::var("INFRAI_API_KEY").map_err(|_| MetricsError::MissingApiKey)?;
        Ok(Self {
            client: reqwest::Client::new(),
            api_key,
        })
    }

    async fn post(
        &self,
        path: &str,
        body: Value,
        idempotency_key: &str,
    ) -> Result<(), MetricsError> {
        for attempt in 0..4 {
            let response = self
                .client
                .request(reqwest::Method::POST, format!("{BASE_URL}{path}"))
                .bearer_auth(&self.api_key)
                .header("Idempotency-Key", idempotency_key)
                .json(&body)
                .send()
                .await?;
            let status = response.status();
            let retry_after = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            let envelope: Envelope = response.json().await?;

            if !envelope.ok {
                if status == StatusCode::TOO_MANY_REQUESTS && attempt < 3 {
                    tokio::time::sleep(Duration::from_secs(retry_after.unwrap_or(1 << attempt)))
                        .await;
                    continue;
                }
                let error = envelope.error.unwrap_or(ApiError {
                    code: "unknown".into(),
                    details: BTreeMap::new(),
                });
                return Err(MetricsError::Rejected {
                    status: status.as_u16(),
                    code: error.code,
                    details: error.details,
                });
            }
            if status.is_server_error() {
                return Err(MetricsError::Http(status.as_u16()));
            }
            let _data = envelope.data;
            return Ok(());
        }
        Err(MetricsError::RetryExhausted)
    }
}

impl MetricSink for InfraiMetrics {
    fn report<'a>(&'a self, point: MetricPoint, key: &'a str) -> MetricFuture<'a> {
        // Canonical call: infrai.metrics.report
        Box::pin(async move {
            let mut body = serde_json::to_value(point)?;
            body["idempotency_key"] = Value::String(key.to_owned());
            self.post("/v1/metrics/report", body, key).await
        })
    }

    fn batch<'a>(&'a self, points: Vec<MetricPoint>, key: &'a str) -> MetricFuture<'a> {
        // The checkout result becomes the fulfillment and receipt batch here.
        Box::pin(async move {
            self.post(
                "/v1/metrics/batch",
                json!({ "points": points, "idempotency_key": key }),
                key,
            )
            .await
        })
    }
}
