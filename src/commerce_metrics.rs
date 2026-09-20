use std::{future::Future, pin::Pin};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::infrai_metrics::{MetricPoint, MetricsError};

pub type MetricFuture<'a> = Pin<Box<dyn Future<Output = Result<(), MetricsError>> + Send + 'a>>;

pub trait MetricSink: Send + Sync {
    fn report<'a>(&'a self, point: MetricPoint, idempotency_key: &'a str) -> MetricFuture<'a>;
    fn batch<'a>(&'a self, points: Vec<MetricPoint>, idempotency_key: &'a str) -> MetricFuture<'a>;
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrderEvent {
    pub order_id: String,
    pub customer_id: String,
    pub amount_cents: u64,
    pub items: u32,
    pub fulfillment: Fulfillment,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Fulfillment {
    Pending,
    Shipped,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct OrderUpdate {
    pub order_id: String,
    pub customer_id: String,
    pub state: &'static str,
    pub receipt: &'static str,
    pub metrics_written: usize,
}

#[derive(Debug, Error)]
pub enum CheckoutError {
    #[error("order_id, customer_id, amount_cents, and items must be present")]
    InvalidOrder,
    #[error(transparent)]
    Metrics(#[from] MetricsError),
}

pub async fn record_order(
    sink: &impl MetricSink,
    order: OrderEvent,
) -> Result<OrderUpdate, CheckoutError> {
    if order.order_id.trim().is_empty()
        || order.customer_id.trim().is_empty()
        || order.amount_cents == 0
        || order.items == 0
    {
        return Err(CheckoutError::InvalidOrder);
    }

    let tags = [
        ("order_id", order.order_id.as_str()),
        ("customer_id", order.customer_id.as_str()),
    ];
    sink.report(
        MetricPoint::new("counter", "checkout.completed", 1.0, &tags),
        &format!("order:{}:checkout", order.order_id),
    )
    .await?;

    let (state, fulfillment_value) = match order.fulfillment {
        Fulfillment::Pending => ("processing", 0.0),
        Fulfillment::Shipped => ("shipped", 1.0),
    };
    let handoff = vec![
        MetricPoint::new("gauge", "fulfillment.shipped", fulfillment_value, &tags),
        MetricPoint::new("counter", "receipt.issued", 1.0, &tags),
        MetricPoint::new("gauge", "customer_order.items", order.items as f64, &tags),
        MetricPoint::new(
            "gauge",
            "customer_order.amount_cents",
            order.amount_cents as f64,
            &tags,
        ),
    ];
    sink.batch(handoff, &format!("order:{}:handoff", order.order_id))
        .await?;

    Ok(OrderUpdate {
        order_id: order.order_id,
        customer_id: order.customer_id,
        state,
        receipt: "issued",
        metrics_written: 5,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct RecordingSink {
        calls: Mutex<Vec<(String, Vec<MetricPoint>)>>,
    }

    impl MetricSink for RecordingSink {
        fn report<'a>(&'a self, point: MetricPoint, key: &'a str) -> MetricFuture<'a> {
            Box::pin(async move {
                self.calls
                    .lock()
                    .unwrap()
                    .push((key.to_owned(), vec![point]));
                Ok(())
            })
        }

        fn batch<'a>(&'a self, points: Vec<MetricPoint>, key: &'a str) -> MetricFuture<'a> {
            Box::pin(async move {
                self.calls.lock().unwrap().push((key.to_owned(), points));
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn shipped_checkout_hands_customer_update_to_batch() {
        let sink = RecordingSink::default();
        let update = record_order(
            &sink,
            OrderEvent {
                order_id: "ord-42".into(),
                customer_id: "cus-7".into(),
                amount_cents: 12_500,
                items: 3,
                fulfillment: Fulfillment::Shipped,
            },
        )
        .await
        .unwrap();

        assert_eq!(update.state, "shipped");
        assert_eq!(update.receipt, "issued");
        let calls = sink.calls.lock().unwrap();
        assert_eq!(calls[0].0, "order:ord-42:checkout");
        assert_eq!(calls[1].0, "order:ord-42:handoff");
        assert_eq!(calls[1].1.len(), 4);
        assert_eq!(calls[1].1[0].name, "fulfillment.shipped");
        assert_eq!(calls[1].1[0].value, 1.0);
    }
}
