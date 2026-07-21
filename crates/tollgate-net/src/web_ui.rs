//! Built-in admin web UI dashboard.
//!
//! A lightweight HTML dashboard served at `/admin` from the tollgate binary
//! itself — no external template engine (no askama / maud / tera), no JavaScript
//! framework, no npm. Cross-platform (works on any Linux, not just OpenWrt) and
//! served from the same axum router as the rest of the API.
//!
//! Routes (wired in [`crate::server`]):
//!   GET /admin             — single-page dashboard (HTML + inline CSS + inline JS)
//!   GET /admin/api/status  — JSON `NodeStatus` (same shape as the control socket)
//!   GET /admin/api/config  — JSON `Config` (same shape as control socket `config get`)
//!
//! Auto-refreshes every 5 seconds via AJAX. LAN-only — no authentication.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::json;

use crate::config::Config;
use crate::driver::Driver;

/// Shared state for the admin dashboard handlers. Cheap to clone — `Driver`
/// is `Arc`-backed internally and the `Config` is shared via `Arc`.
#[derive(Clone)]
pub struct WebUiState {
    pub driver: Driver,
    pub config: Arc<Config>,
}

/// Build the `/admin` sub-router. Standalone — owns its state so it merges
/// cleanly into the main router (both end up `Router<()>` after their own
/// `.with_state(...)`).
pub fn router(driver: Driver, config: Arc<Config>) -> Router<()> {
    Router::new()
        .route("/admin", get(dashboard))
        .route("/admin/api/status", get(api_status))
        .route("/admin/api/config", get(api_config))
        .with_state(WebUiState { driver, config })
}

/// `GET /admin` — the single-page dashboard. Pure static HTML (the JS inside
/// does the AJAX work); no template engine, no server-side rendering.
async fn dashboard() -> Response {
    (
        StatusCode::OK,
        [("content-type", "text/html; charset=utf-8")],
        DASHBOARD_HTML,
    )
        .into_response()
}

/// `GET /admin/api/status` — same `NodeStatus` the control socket serves for
/// `status`. Wrapped as raw JSON (no `CLIResponse` envelope) so the dashboard's
/// fetch() can read the fields directly.
async fn api_status(State(state): State<WebUiState>) -> Response {
    let status = state.driver.status().await;
    json_response(serde_json::to_value(&status), "serialize status")
}

/// `GET /admin/api/config` — the loaded `Config` (mirrors control socket
/// `{"command":"config","args":["get"]}`).
async fn api_config(State(state): State<WebUiState>) -> Response {
    let cfg = serde_json::to_value(state.config.as_ref());
    json_response(cfg, "serialize config")
}

/// Serialize `value` to JSON or return a 500 with a helpful error. Keeps the
/// handlers terse and the wire shape uniform.
fn json_response(value: Result<serde_json::Value, serde_json::Error>, label: &str) -> Response {
    match value {
        Ok(v) => (
            StatusCode::OK,
            [("content-type", "application/json")],
            serde_json::to_string(&v).unwrap_or_else(|_| "{}".to_string()),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [("content-type", "application/json")],
            serde_json::to_string(&json!({ "error": format!("{label}: {e}") }))
                .unwrap_or_else(|_| "{}".to_string()),
        )
            .into_response(),
    }
}

// Single-page dashboard. Inline CSS (dark theme, mobile-friendly) and inline
// vanilla JS (no framework, no build step). Polled every 5 seconds.
//
// The version is interpolated at compile time via `env!("CARGO_PKG_VERSION")`
// so the binary reports its own version without a runtime lookup.
const DASHBOARD_HTML: &str = concat!(
    r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>TollGate Node</title>
<style>
  :root {
    --bg: #0f1115;
    --card: #171a21;
    --border: #232833;
    --text: #e6e8ec;
    --muted: #8a94a6;
    --accent: #f5a623;
    --green: #3ecf8e;
    --red: #ff5c5c;
    --yellow: #f5c518;
  }
  * { box-sizing: border-box; }
  html, body {
    margin: 0;
    padding: 0;
    background: var(--bg);
    color: var(--text);
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
    line-height: 1.5;
  }
  header {
    padding: 1.25rem 1rem;
    border-bottom: 1px solid var(--border);
  }
  header h1 {
    margin: 0;
    font-size: 1.25rem;
    font-weight: 600;
  }
  header .version {
    color: var(--muted);
    font-size: 0.85rem;
    margin-left: 0.5rem;
  }
  header .updated {
    color: var(--muted);
    font-size: 0.8rem;
    margin-top: 0.25rem;
  }
  main {
    padding: 1rem;
    display: grid;
    grid-template-columns: 1fr;
    gap: 1rem;
    max-width: 960px;
    margin: 0 auto;
  }
  @media (min-width: 720px) {
    main { grid-template-columns: 1fr 1fr; }
    main .full { grid-column: 1 / -1; }
  }
  section.card {
    background: var(--card);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 1rem;
  }
  section.card h2 {
    margin: 0 0 0.75rem 0;
    font-size: 0.95rem;
    font-weight: 600;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .stat-grid {
    display: grid;
    grid-template-columns: repeat(2, 1fr);
    gap: 0.75rem;
  }
  .stat .label {
    color: var(--muted);
    font-size: 0.75rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .stat .value {
    font-size: 1.4rem;
    font-weight: 600;
    margin-top: 0.15rem;
  }
  .stat .value.green { color: var(--green); }
  .stat .value.red { color: var(--red); }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.85rem;
  }
  th, td {
    text-align: left;
    padding: 0.4rem 0.5rem;
    border-bottom: 1px solid var(--border);
    overflow-wrap: anywhere;
  }
  th {
    color: var(--muted);
    font-weight: 500;
    font-size: 0.75rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  tr:last-child td { border-bottom: none; }
  .pill {
    display: inline-block;
    padding: 0.1rem 0.5rem;
    border-radius: 999px;
    font-size: 0.7rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .pill.active { background: rgba(62, 207, 142, 0.15); color: var(--green); }
  .pill.suspended { background: rgba(245, 197, 24, 0.15); color: var(--yellow); }
  .pill.other { background: rgba(138, 148, 166, 0.15); color: var(--muted); }
  .pill.configured { background: rgba(62, 207, 142, 0.15); color: var(--green); }
  .empty { color: var(--muted); font-style: italic; font-size: 0.85rem; }
  footer {
    padding: 1rem;
    text-align: center;
    color: var(--muted);
    font-size: 0.8rem;
    border-top: 1px solid var(--border);
    margin-top: 1rem;
  }
  footer a { color: var(--accent); text-decoration: none; }
  footer a:hover { text-decoration: underline; }
  .error {
    background: rgba(255, 92, 92, 0.1);
    border: 1px solid var(--red);
    color: var(--red);
    padding: 0.5rem 0.75rem;
    border-radius: 6px;
    font-size: 0.85rem;
    margin-bottom: 0.75rem;
    display: none;
  }
  .mono { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 0.85rem; }
</style>
</head>
<body>
<header>
  <div>
    <h1>TollGate Node<span class="version">v"#,
    env!("CARGO_PKG_VERSION"),
    r#"</span></h1>
    <div class="updated" id="updated">loading…</div>
  </div>
</header>
<main>
  <div class="error" id="error"></div>

  <section class="card full" id="status-card">
    <h2>Status</h2>
    <div class="stat-grid">
      <div class="stat">
        <div class="label">State</div>
        <div class="value green" id="stat-state">—</div>
      </div>
      <div class="stat">
        <div class="label">Uptime</div>
        <div class="value" id="stat-uptime">—</div>
      </div>
      <div class="stat">
        <div class="label">Peers</div>
        <div class="value" id="stat-peers">—</div>
      </div>
      <div class="stat">
        <div class="label">Net Balance (sat)</div>
        <div class="value" id="stat-balance">—</div>
      </div>
      <div class="stat">
        <div class="label">Pubkey</div>
        <div class="value mono" style="font-size: 0.85rem;" id="stat-pubkey">—</div>
      </div>
      <div class="stat">
        <div class="label">Unit</div>
        <div class="value" id="stat-unit">—</div>
      </div>
    </div>
  </section>

  <section class="card" id="peers-card">
    <h2>Peers</h2>
    <div id="peers-body"><p class="empty">loading…</p></div>
  </section>

  <section class="card" id="mints-card">
    <h2>Accepted Mints</h2>
    <div id="mints-body"><p class="empty">loading…</p></div>
  </section>

  <section class="card full" id="pricing-card">
    <h2>Pricing</h2>
    <div id="pricing-body"><p class="empty">loading…</p></div>
  </section>
</main>
<footer>
  TollGate node admin · configuration via CLI socket
  (<code class="mono">tolltop</code>, <code class="mono">tollgate.yaml</code>) ·
  <a href="https://github.com/Amperstrand/tollgate-rs-ai-research-and-experiments" target="_blank" rel="noopener">docs</a>
</footer>

<script>
"use strict";
const REFRESH_MS = 5000;

function showError(msg) {
  const el = document.getElementById("error");
  if (msg) {
    el.textContent = msg;
    el.style.display = "block";
  } else {
    el.style.display = "none";
  }
}

function fmtNet(scaled) {
  // NodeStatus balances are milli-units (scale 1000). Show signed sats.
  if (typeof scaled !== "number" || !isFinite(scaled)) return "—";
  const sats = Math.round(scaled / 1000);
  return (sats >= 0 ? "+" : "") + sats.toLocaleString();
}

function shortHex(hex) {
  if (!hex || hex.length <= 12) return hex || "—";
  return hex.slice(0, 6) + "…" + hex.slice(-4);
}

function statePill(state) {
  const cls = state === "Active" ? "active" : state === "Suspended" ? "suspended" : "other";
  return '<span class="pill ' + cls + '">' + escapeHtml(state || "—") + "</span>";
}

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, function (c) {
    return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c];
  });
}

function renderStatus(status) {
  document.getElementById("stat-state").textContent = "Running";
  document.getElementById("stat-state").className = "value green";

  // Uptime is not currently tracked by the control socket; show max peer
  // metering duration as a proxy, else "—".
  const maxMetered = (status.peers || []).reduce(function (m, p) {
    return Math.max(m, p.metered_secs || 0);
  }, 0);
  document.getElementById("stat-uptime").textContent = maxMetered > 0
    ? maxMetered + "s"
    : "—";

  const peers = status.peers || [];
  document.getElementById("stat-peers").textContent = String(peers.length);

  // Net balance = sum of peer.net_balance() = sum(their_balance - our_balance).
  let net = 0;
  for (const p of peers) net += (p.their_balance || 0) - (p.our_balance || 0);
  const balanceEl = document.getElementById("stat-balance");
  balanceEl.textContent = fmtNet(net);
  balanceEl.className = "value " + (net >= 0 ? "green" : "red");

  document.getElementById("stat-pubkey").textContent = shortHex(status.pubkey);
  document.getElementById("stat-unit").textContent = status.unit || "—";
}

function renderPeers(status) {
  const body = document.getElementById("peers-body");
  const peers = status.peers || [];
  if (peers.length === 0) {
    body.innerHTML = '<p class="empty">No peers connected.</p>';
    return;
  }
  let rows = "";
  for (const p of peers) {
    const net = (p.their_balance || 0) - (p.our_balance || 0);
    rows += "<tr>"
      + '<td class="mono">' + escapeHtml(shortHex(p.pubkey)) + "</td>"
      + "<td>" + escapeHtml(p.ip || "—") + "</td>"
      + "<td>" + statePill(p.state) + "</td>"
      + '<td class="mono">' + fmtNet(net) + "</td>"
      + '<td class="mono">' + escapeHtml(String(p.metered_secs || 0)) + "s</td>"
      + "</tr>";
  }
  body.innerHTML =
    '<table><thead><tr>'
    + "<th>Peer</th><th>IP</th><th>State</th><th>Net</th><th>Metered</th>"
    + "</tr></thead><tbody>" + rows + "</tbody></table>";
}

function renderMints(config) {
  const body = document.getElementById("mints-body");
  const urls = (config && config.mints) || [];
  const accepted = (config && config.v1_compat && config.v1_compat.accepted_mints) || [];
  if (urls.length === 0 && accepted.length === 0) {
    body.innerHTML = '<p class="empty">No mints configured.</p>';
    return;
  }
  let rows = "";
  for (const url of urls) {
    rows += "<tr>"
      + '<td class="mono">' + escapeHtml(url) + "</td>"
      + "<td>—</td><td>—</td>"
      + '<td><span class="pill configured">configured</span></td>'
      + "</tr>";
  }
  for (const m of accepted) {
    rows += "<tr>"
      + '<td class="mono">' + escapeHtml(m.url || "—") + "</td>"
      + '<td class="mono">' + escapeHtml(String(m.price_per_step || 0)) + " " + escapeHtml(m.unit || "sat") + "</td>"
      + '<td class="mono">' + escapeHtml(String(m.min_steps || 0)) + "</td>"
      + '<td><span class="pill configured">configured</span></td>'
      + "</tr>";
  }
  body.innerHTML =
    '<table><thead><tr>'
    + "<th>Mint URL</th><th>Price / Step</th><th>Min Steps</th><th>Status</th>"
    + "</tr></thead><tbody>" + rows + "</tbody></table>";
}

function renderPricing(status, config) {
  const body = document.getElementById("pricing-body");
  const pricing = status && status.pricing;
  const products = (pricing && pricing.products) || [];
  const stepSize = (config && config.v1_compat && config.v1_compat.step_size) || 0;
  const meteringInterval = (config && config.metering_interval_secs) || 0;

  let html = '<table><thead><tr>'
    + "<th>Product</th><th>Mint</th><th>CCY</th><th>Per Sec</th><th>Per Unit</th>"
    + "</tr></thead><tbody>";
  if (products.length === 0) {
    html += '<tr><td colspan="5" class="empty">No products advertised.</td></tr>';
  } else {
    for (const prod of products) {
      const mints = prod.mints || [];
      if (mints.length === 0) {
        html += '<tr><td class="mono">' + escapeHtml(shortHex(prod.product_id))
          + '</td><td colspan="4" class="empty">no mints</td></tr>';
        continue;
      }
      for (const m of mints) {
        html += "<tr>"
          + '<td class="mono">' + escapeHtml(shortHex(prod.product_id)) + "</td>"
          + '<td class="mono">' + escapeHtml(m.mint_url || "—") + "</td>"
          + "<td>" + escapeHtml(m.mint_unit || "—") + "</td>"
          + '<td class="mono">' + escapeHtml(String(m.price_per_second || 0)) + "</td>"
          + '<td class="mono">' + escapeHtml(String(m.price_per_unit || 0)) + "</td>"
          + "</tr>";
      }
    }
  }
  html += "</tbody></table>";
  html += '<div style="margin-top: 0.75rem; color: var(--muted); font-size: 0.8rem;">'
    + "metering_interval_secs=" + meteringInterval
    + (stepSize > 0 ? " · v1 step_size=" + stepSize : "")
    + (pricing ? " · interval " + (pricing.min_interval_ms || 0) + "–" + (pricing.max_interval_ms || 0) + " ms" : "")
    + "</div>";
  body.innerHTML = html;
}

async function fetchJson(url) {
  const resp = await fetch(url, { cache: "no-store" });
  if (!resp.ok) throw new Error(url + " → HTTP " + resp.status);
  return resp.json();
}

function markUpdated() {
  const now = new Date();
  const hh = String(now.getHours()).padStart(2, "0");
  const mm = String(now.getMinutes()).padStart(2, "0");
  const ss = String(now.getSeconds()).padStart(2, "0");
  document.getElementById("updated").textContent = "updated " + hh + ":" + mm + ":" + ss;
}

async function refresh() {
  try {
    const [status, config] = await Promise.all([
      fetchJson("/admin/api/status"),
      fetchJson("/admin/api/config"),
    ]);
    renderStatus(status);
    renderPeers(status);
    renderMints(config);
    renderPricing(status, config);
    markUpdated();
    showError("");
  } catch (e) {
    showError("refresh failed: " + (e && e.message ? e.message : String(e)));
  }
}

(async function init() {
  await refresh();
  setInterval(refresh, REFRESH_MS);
})();
</script>
</body>
</html>"#
);

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use tollgate_core::Price;

    use crate::adapter::IpAdapter;
    use crate::wallet::BootstrapWallet;

    fn test_driver() -> Driver {
        let identity = Arc::new(
            crate::config::Identity::load_or_generate(&Config::default()).unwrap(),
        );
        Driver::new(
            BootstrapWallet::new(vec![]),
            IpAdapter::new(),
            identity,
            Price::default(),
            "bytes",
            Vec::new(),
        )
    }

    #[tokio::test]
    async fn dashboard_returns_html_with_version_and_title() {
        let app: Router<()> = router(test_driver(), Arc::new(Config::default()));

        let resp = app
            .oneshot(Request::builder().uri("/admin").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get("content-type")
                .map(|v| v.to_str().unwrap()),
            Some("text/html; charset=utf-8")
        );

        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let html = std::str::from_utf8(&bytes).unwrap();
        assert!(html.contains("<title>TollGate Node</title>"), "missing title");
        assert!(
            html.contains(&format!("v{}", env!("CARGO_PKG_VERSION"))),
            "version not interpolated"
        );
        assert!(html.contains("/admin/api/status"), "missing status endpoint");
        assert!(html.contains("/admin/api/config"), "missing config endpoint");
    }

    #[tokio::test]
    async fn api_status_returns_node_status_json() {
        let app: Router<()> = router(test_driver(), Arc::new(Config::default()));

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/admin/api/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").map(|v| v.to_str().unwrap()),
            Some("application/json")
        );

        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v["pubkey"].is_string(), "status has pubkey");
        assert!(v["unit"].is_string(), "status has unit");
        assert!(v["peers"].is_array(), "status has peers array");
    }

    #[tokio::test]
    async fn api_config_returns_config_json() {
        let cfg = Config {
            unit: "wh".to_string(),
            mints: vec!["http://mint.example:3338".to_string()],
            ..Config::default()
        };
        let app: Router<()> = router(test_driver(), Arc::new(cfg));

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/admin/api/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["unit"], "wh");
        assert_eq!(v["mints"][0], "http://mint.example:3338");
        assert!(v["listen"].is_string(), "config has listen");
    }

    #[tokio::test]
    async fn unknown_admin_path_returns_404() {
        let app: Router<()> = router(test_driver(), Arc::new(Config::default()));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/admin/api/unknown")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
