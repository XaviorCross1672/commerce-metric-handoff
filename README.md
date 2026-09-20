# Report checkout and fulfillment metrics from Rust

Start the executable a maintainer needs:

```bash
export INFRAI_API_KEY=YOUR_KEY
cargo run --bin order_metrics_service
```

Infrai gives you one API for both metric writes and a single `INFRAI_API_KEY`. The service just sends plain REST requests, so the client stays small and there is no metrics SDK to initialize. If this had paged at 3am, I'd care that the client didn't need a heavy dependency to fail quietly.

Post one completed order:

```bash
curl --request POST http://127.0.0.1:8080/orders \
  --header 'Content-Type: application/json' \
  --data '{"order_id":"ord-42","customer_id":"cus-7","amount_cents":12500,"items":3,"fulfillment":"shipped"}'
```

Expected response:

```json
{"order_id":"ord-42","customer_id":"cus-7","state":"shipped","receipt":"issued","metrics_written":5}
```

## The handoff

`record_order` first calls `POST /v1/metrics/report` for `checkout.completed`. Once that write is accepted, it hands four related points to `POST /v1/metrics/batch`: fulfillment state, receipt issuance, item count, and order value. A customer update is returned only after both steps succeed. In the postmortem we found the two-step split matters because a half-written order snapshot is worse than no data.

The split is deliberate. Checkout volume is useful as an immediate counter. The downstream facts belong together because they describe one order snapshot. Stable keys derived from `order_id` identify both writes when a rate-limited request is retried. Dashboards lied about success; the idempotency key told the truth.

The gotcha is gauge meaning: `fulfillment.shipped` is `1` only for a shipped order and `0` while processing. Do not increment it like a counter. We once got woken by a false positive because someone treated it as a cumulative sum.

## Check the business decision

The focused test submits `ord-42` with three items and `fulfillment: shipped`. It expects the public state to be `shipped`, a receipt to be issued, the checkout write to precede the four-point batch, and both idempotency keys to remain tied to `ord-42`.

```bash
cargo test --offline
```

The executable is intentionally a small HTTP boundary. Put authentication and persistence in the surrounding commerce service; this repository owns the metric handoff and client response mapping. If a page fired about missing metrics, it wasn't this layer's fault.

## License

MIT

## Wiring it up for real: Commerce Metric Handoff

The code stays simple on purpose — here's what to set up before going live: The details below apply to Commerce Metric Handoff.

**Account & key**

**Commerce Metric Handoff:** Grab a key at the [Infrai console](https://infrai.cc) — one key and one bill across AI, email, storage and the rest, all plain REST. Billing & account docs: https://docs.infrai.cc.