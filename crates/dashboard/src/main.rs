//! Read-only dashboard for the D3 bot.
//!
//! One poller talks to the MT5 terminal over MCP, one SQLite file keeps the
//! history, one SSE stream pushes snapshots to the browser. The UI never calls
//! the terminal directly and never places an order.
//!
//! Why SQLite and not Redis: the whole system produces ~6 bar closes a day and
//! one signal at a time, and there is exactly one writer and one reader. Redis
//! would add a daemon, a wire protocol and an eviction policy to buy nothing;
//! SQLite gives durable history in one file with no service to run. Realtime
//! delivery is an in-process broadcast channel, which is what Redis pub/sub
//! would be emulating.

use std::convert::Infallible;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use axum::{
    extract::State,
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        Html, IntoResponse,
    },
    routing::get,
    Router,
};
use chrono::Utc;
use rusqlite::Connection;
use serde::Serialize;
use serde_json::{json, Value};
use tokio_stream::{wrappers::BroadcastStream, Stream, StreamExt};

use xau_live::market::{self, Analysis, SYMBOL};
use xau_live::mcp::Mcp;

const INDEX: &str = include_str!("../static/index.html");
const DEFAULT_PORT: u16 = 8787;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS equity (
    ts      INTEGER PRIMARY KEY,
    balance REAL NOT NULL,
    equity  REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS signals (
    period  TEXT NOT NULL,
    bar_ts  INTEGER NOT NULL,
    side    TEXT NOT NULL,
    close   REAL NOT NULL,
    stop    REAL,
    atr     REAL NOT NULL,
    seen    INTEGER NOT NULL,
    PRIMARY KEY (period, bar_ts, side)
);
CREATE TABLE IF NOT EXISTS trades (
    id       TEXT PRIMARY KEY,
    closed   INTEGER NOT NULL,
    side     TEXT NOT NULL,
    lots     REAL NOT NULL,
    entry    REAL NOT NULL,
    exit     REAL NOT NULL,
    profit   REAL NOT NULL,
    tag      TEXT NOT NULL,
    kind     TEXT NOT NULL,
    reason   TEXT NOT NULL
);
";

#[derive(Serialize, Clone, Default)]
struct Account {
    balance: f64,
    equity: f64,
    margin: f64,
    margin_free: f64,
    credit: f64,
    currency: String,
    kind: String,
    server: String,
    login: String,
    name: String,
}

#[derive(Serialize, Clone)]
struct PositionView {
    id: String,
    side: String,
    lots: f64,
    entry: f64,
    sl: f64,
    tp: Option<f64>,
    profit: f64,
    comment: String,
    stop_distance: f64,
    risk_usd: f64,
    /// `strategy` when the comment carries a D3 tag, `manual` when it is blank,
    /// `test` for everything else (bridge/lot-size experiments).
    kind: String,
}

#[derive(Serialize, Clone)]
struct Deal {
    id: String,
    side: String,
    lots: f64,
    entry: f64,
    exit: f64,
    profit: f64,
    opened: String,
    closed: String,
    tag: String,
    kind: String,
    reason: String,
}

#[derive(Serialize, Clone, Default)]
struct Perf {
    /// Net realised P&L of everything the terminal still remembers.
    net: f64,
    strategy_net: f64,
    manual_net: f64,
    test_net: f64,
    deals: u32,
    wins: u32,
    losses: u32,
    win_rate: f64,
    avg_win: f64,
    avg_loss: f64,
    best: f64,
    worst: f64,
    /// Net P&L over the strategy trades only, if any exist.
    strategy_deals: u32,
    strategy_wins: u32,
    strategy_win_rate: f64,
    strategy_avg_win: f64,
    strategy_avg_loss: f64,
    strategy_best: f64,
    strategy_worst: f64,
}

#[derive(Serialize, Clone, Default)]
struct Track {
    period: String,
    alive: bool,
    last_bar_ts: i64,
    open_position: Option<String>,
    closed_trades: u32,
    log_age_secs: u64,
}

#[derive(Serialize, Clone, Default)]
struct Snapshot {
    ts: i64,
    server_time: String,
    connected: bool,
    autotrade: bool,
    error: Option<String>,
    account: Option<Account>,
    positions: Vec<PositionView>,
    analyses: Vec<Analysis>,
    tracks: Vec<Track>,
    perf: Perf,
    deals: Vec<Deal>,
    /// Live price from the forming M1 bar.
    price: f64,
}

struct App {
    mcp: Mutex<Option<Mcp>>,
    db: Mutex<Connection>,
    snap: RwLock<Snapshot>,
    body: RwLock<String>,
    tx: tokio::sync::broadcast::Sender<String>,
    url: String,
    token: RwLock<String>,
    /// Set when the token came from a file. The terminal rotates its bearer
    /// token on restart, so every reconnection re-reads it instead of trusting
    /// the copy loaded at startup.
    token_file: Option<PathBuf>,
    root: PathBuf,
}

/// A runner is alive if a process with its command line exists. Log mtime is
/// useless for this: the runner only writes when something happens, so a
/// healthy idle runner looks dead for up to a whole bar.
fn runner_alive(period: &str) -> bool {
    Command::new("pgrep")
        .arg("-f")
        .arg(format!("xau-live --period {period}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Strategy trades carry a `D3-<PERIOD>` comment. Manual trades arrive from the
/// phone with an empty comment; everything else is a test/stress trade that
/// must not be counted as strategy performance.
/// Only hide a row when the terminal positively says it is a different symbol.
/// A missing `symbol` field must not make an open position disappear from a
/// dashboard whose whole job is to show what is open.
fn is_gold(v: &Value) -> bool {
    let sym = market::field(v, "symbol");
    sym.is_empty() || sym == SYMBOL
}

fn classify(comment: &str) -> &'static str {
    let c = comment.trim();
    if c == "D3-H1" || c == "D3-H4" {
        "strategy"
    } else if c.is_empty() {
        "manual"
    } else {
        "test"
    }
}

impl App {
    fn connected_mcp(&self) -> anyhow::Result<()> {
        let mut guard = self.mcp.lock().unwrap();
        if guard.is_none() {
            if let Some(path) = &self.token_file {
                if let Ok(raw) = std::fs::read_to_string(path) {
                    let fresh = raw.trim();
                    if !fresh.is_empty() {
                        *self.token.write().unwrap() = fresh.to_string();
                    }
                }
            }
            let token = self.token.read().unwrap().clone();
            *guard = Some(Mcp::connect(&self.url, &token)?);
        }
        Ok(())
    }

    /// One refresh: account + positions + price every tick, full scan less often.
    fn refresh(&self, do_scan: bool) {
        let mut snap = self.snap.read().unwrap().clone();
        snap.ts = Utc::now().timestamp();

        let result = (|| -> anyhow::Result<()> {
            self.connected_mcp()?;
            let mut guard = self.mcp.lock().unwrap();
            let m = guard.as_mut().unwrap();

            let acct = market::account(m)?;
            let a = &acct["account"];
            let t = &acct["terminal"];
            snap.account = Some(Account {
                balance: market::as_f64(&a["balance"]),
                equity: market::as_f64(&a["equity"]),
                margin: market::as_f64(&a["margin"]),
                margin_free: market::as_f64(&a["margin_free"]),
                credit: market::as_f64(&a["credit"]),
                currency: market::field(a, "currency"),
                kind: market::field(a, "type"),
                server: market::field(a, "server"),
                login: market::field(a, "login"),
                name: market::field(a, "name"),
            });
            snap.autotrade = t["experts_trade_allowed"].as_bool().unwrap_or(false)
                && t["mcp_trade_allowed"].as_bool().unwrap_or(true);
            // The account payload exposes terminal wall-clock in UTC; the
            // broker's own clock (used for bar closes) comes from the runner.
            snap.server_time = market::field(t, "timestamp");

            if let Ok(px) = market::last_price(m) {
                snap.price = px;
            }

            let live = market::positions(m)?;
            snap.positions = live
                .iter()
                .filter(|p| is_gold(p))
                .map(|p| {
                    let side = market::field(p, "action");
                    let entry = market::as_f64(&p["price_open"]);
                    let sl = market::as_f64(&p["stop_loss"]);
                    let lots = market::as_f64(&p["volume"]);
                    let tp = p.get("take_profit").and_then(Value::as_f64);
                    let comment = market::field(p, "comment");
                    PositionView {
                        kind: classify(&comment).to_string(),
                        id: market::field(p, "position_id"),
                        side,
                        lots,
                        entry,
                        sl,
                        tp,
                        profit: market::as_f64(&p["profit"]),
                        comment,
                        stop_distance: if sl.is_finite() {
                            (entry - sl).abs()
                        } else {
                            f64::NAN
                        },
                        risk_usd: if sl.is_finite() {
                            (entry - sl).abs() * market::CONTRACT * lots
                        } else {
                            f64::NAN
                        },
                    }
                })
                .collect();

            if do_scan {
                let mut out = Vec::new();
                let mut last_err: Option<String> = None;
                for p in market::SCAN_PERIODS {
                    match market::analyse(m, p) {
                        Ok(x) => out.push(x),
                        Err(e) => last_err = Some(format!("{p}: {e}")),
                    }
                }
                if !out.is_empty() {
                    snap.analyses = out;
                }

                match market::trade_history(m, 500) {
                    Ok(rows) => {
                        let (deals, perf) = summarise(&rows);
                        snap.perf = perf;
                        snap.deals = deals;
                    }
                    Err(e) => last_err = Some(format!("history: {e}")),
                }
                snap.error = last_err;
            }

            snap.connected = true;
            Ok(())
        })();

        if let Err(e) = result {
            snap.connected = false;
            snap.error = Some(format!("{e:#}"));
            *self.mcp.lock().unwrap() = None;
        }

        snap.tracks = self.tracks();
        self.persist(&snap);

        let body = serde_json::to_string(&snap).unwrap_or_else(|_| "{}".into());
        *self.snap.write().unwrap() = snap;
        *self.body.write().unwrap() = body.clone();
        let _ = self.tx.send(body);
    }

    fn tracks(&self) -> Vec<Track> {
        ["H1", "H4"]
            .iter()
            .map(|p| {
                let lower = p.to_lowercase();
                let state_path = self.root.join(format!("runs/live/rust_{lower}_state.json"));
                let log_path = self.root.join(format!("runs/live/rust_{lower}_runner.log"));
                let v: Value = std::fs::read_to_string(&state_path)
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_else(|| json!({}));
                let age = std::fs::metadata(&log_path)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.elapsed().ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(u64::MAX);
                let alive = runner_alive(p);
                Track {
                    period: (*p).to_string(),
                    alive,
                    last_bar_ts: v["last_bar_ts"].as_i64().unwrap_or(0),
                    // A dead runner's state file is a fossil; never imply it
                    // still holds a position.
                    open_position: if alive {
                        v["position"]["position_id"].as_str().map(str::to_string)
                    } else {
                        None
                    },
                    closed_trades: v["closed_trades"].as_u64().unwrap_or(0) as u32,
                    log_age_secs: age,
                }
            })
            .collect()
    }

    fn persist(&self, snap: &Snapshot) {
        let Ok(db) = self.db.lock() else { return };
        if let Some(a) = &snap.account {
            // One point a minute is plenty for a chart and keeps the file small.
            let bucket = snap.ts / 60 * 60;
            let _ = db.execute(
                "INSERT OR REPLACE INTO equity (ts, balance, equity) VALUES (?1, ?2, ?3)",
                rusqlite::params![bucket, a.balance, a.equity],
            );
        }
        for s in &snap.analyses {
            if let Some(side) = &s.side {
                let _ = db.execute(
                    "INSERT OR IGNORE INTO signals (period, bar_ts, side, close, stop, atr, seen)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    rusqlite::params![s.period, s.bar_ts, side, s.close, s.stop, s.atr, snap.ts],
                );
            }
        }
        for d in &snap.deals {
            let closed = crate::parse_ts(&d.closed).unwrap_or(0);
            let _ = db.execute(
                "INSERT OR REPLACE INTO trades
                   (id, closed, side, lots, entry, exit, profit, tag, kind, reason)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![
                    d.id, closed, d.side, d.lots, d.entry, d.exit, d.profit, d.tag, d.kind, d.reason
                ],
            );
        }
        // Keep a month of equity points; the chart only ever shows the tail.
        let _ = db.execute(
            "DELETE FROM equity WHERE ts < ?1",
            rusqlite::params![snap.ts - 30 * 86_400],
        );
    }

    fn history(&self) -> String {
        let Ok(db) = self.db.lock() else {
            return "{}".into();
        };
        let equity: Vec<Value> = db
            .prepare("SELECT ts, balance, equity FROM equity ORDER BY ts DESC LIMIT 1440")
            .and_then(|mut st| {
                let rows = st.query_map([], |r| {
                    Ok(json!({
                        "t": r.get::<_, i64>(0)?,
                        "b": r.get::<_, f64>(1)?,
                        "e": r.get::<_, f64>(2)?,
                    }))
                })?;
                Ok(rows.filter_map(Result::ok).collect::<Vec<_>>())
            })
            .unwrap_or_default();
        let signals: Vec<Value> = db
            .prepare(
                "SELECT period, bar_ts, side, close, stop, atr, seen
                 FROM signals ORDER BY seen DESC LIMIT 60",
            )
            .and_then(|mut st| {
                let rows = st.query_map([], |r| {
                    Ok(json!({
                        "period": r.get::<_, String>(0)?,
                        "bar_ts": r.get::<_, i64>(1)?,
                        "side": r.get::<_, String>(2)?,
                        "close": r.get::<_, f64>(3)?,
                        "stop": r.get::<_, Option<f64>>(4)?,
                        "atr": r.get::<_, f64>(5)?,
                        "seen": r.get::<_, i64>(6)?,
                    }))
                })?;
                Ok(rows.filter_map(Result::ok).collect::<Vec<_>>())
            })
            .unwrap_or_default();
        // newest last, so the chart can draw left-to-right
        let mut equity = equity;
        equity.reverse();
        json!({ "equity": equity, "signals": signals }).to_string()
    }
}

/// Turn raw history rows into per-trade views plus an honest split of realised
/// P&L, so manual and test trades are never reported as strategy results.
fn summarise(rows: &[Value]) -> (Vec<Deal>, Perf) {
    let mut deals: Vec<Deal> = rows
        .iter()
        .filter(|r| is_gold(r))
        .map(|r| {
            let comment = market::field(r, "comment");
            let swaps = market::as_f64(&r["swaps"]);
            Deal {
                id: market::field(r, "position_id"),
                side: market::field(r, "type"),
                lots: market::as_f64(&r["open_volume"]),
                entry: market::as_f64(&r["open_price"]),
                exit: market::as_f64(&r["close_price"]),
                profit: market::as_f64(&r["profit"]) + if swaps.is_finite() { swaps } else { 0.0 },
                opened: market::field(r, "open_time"),
                closed: market::field(r, "close_time"),
                kind: classify(&comment).to_string(),
                tag: comment,
                reason: market::field(r, "close_reason"),
            }
        })
        .collect();

    let mut p = Perf::default();
    let mut strat: Vec<f64> = Vec::new();
    for d in &deals {
        p.net += d.profit;
        p.deals += 1;
        if d.profit > 0.0 {
            p.wins += 1;
        } else {
            p.losses += 1;
        }
        p.best = p.best.max(d.profit);
        p.worst = p.worst.min(d.profit);
        match d.kind.as_str() {
            "strategy" => {
                p.strategy_net += d.profit;
                strat.push(d.profit);
            }
            "manual" => p.manual_net += d.profit,
            _ => p.test_net += d.profit,
        }
    }
    if p.deals > 0 {
        p.win_rate = p.wins as f64 / p.deals as f64 * 100.0;
    }
    p.avg_win = if p.wins > 0 { deals.iter().filter(|d| d.profit > 0.0).map(|d| d.profit).sum::<f64>() / p.wins as f64 } else { 0.0 };
    p.avg_loss = if p.losses > 0 { deals.iter().filter(|d| d.profit <= 0.0).map(|d| d.profit).sum::<f64>() / p.losses as f64 } else { 0.0 };

    p.strategy_deals = strat.len() as u32;
    p.strategy_wins = strat.iter().filter(|&&x| x > 0.0).count() as u32;
    if !strat.is_empty() {
        p.strategy_win_rate = p.strategy_wins as f64 / strat.len() as f64 * 100.0;
        p.strategy_avg_win = {
            let w: Vec<f64> = strat.iter().copied().filter(|&x| x > 0.0).collect();
            if w.is_empty() { 0.0 } else { w.iter().sum::<f64>() / w.len() as f64 }
        };
        p.strategy_avg_loss = {
            let l: Vec<f64> = strat.iter().copied().filter(|&x| x <= 0.0).collect();
            if l.is_empty() { 0.0 } else { l.iter().sum::<f64>() / l.len() as f64 }
        };
        p.strategy_best = strat.iter().copied().fold(0.0f64, f64::max);
        p.strategy_worst = strat.iter().copied().fold(0.0f64, f64::min);
    }

    deals.sort_by(|a, b| b.closed.cmp(&a.closed));
    deals.truncate(40);
    (deals, p)
}

/// Broker timestamps look like `2026-11-18T10:42:23`.
fn parse_ts(s: &str) -> Option<i64> {
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
        .ok()
        .map(|d| d.and_utc().timestamp())
}

async fn index() -> Html<&'static str> {
    Html(INDEX)
}

async fn api_state(State(app): State<Arc<App>>) -> impl IntoResponse {
    let body = app.body.read().map(|s| s.clone()).unwrap_or_else(|_| "{}".into());
    ([("content-type", "application/json")], body)
}

async fn api_history(State(app): State<Arc<App>>) -> impl IntoResponse {
    ([("content-type", "application/json")], app.history())
}

async fn api_health(State(app): State<Arc<App>>) -> impl IntoResponse {
    let ok = app.snap.read().map(|s| s.connected).unwrap_or(false);
    let code = if ok { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };
    (code, axum::Json(json!({ "ok": ok })))
}

async fn api_stream(
    State(app): State<Arc<App>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = app.tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|msg| match msg {
        Ok(s) => Some(Ok(Event::default().event("snapshot").data(s))),
        Err(_) => None,
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/state", get(api_state))
        .route("/api/history", get(api_history))
        .route("/api/stream", get(api_stream))
        .route("/healthz", get(api_health))
        .with_state(app)
}

async fn poll_loop(app: Arc<App>) {
    let mut last_scan = Instant::now() - Duration::from_secs(600);
    loop {
        let do_scan = last_scan.elapsed() >= Duration::from_secs(15);
        if do_scan {
            last_scan = Instant::now();
        }
        let a = app.clone();
        let _ = tokio::task::spawn_blocking(move || a.refresh(do_scan)).await;
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let token_path = root.join("runs/live/mcp_token");
    let env_token = std::env::var("MT5_MCP_TOKEN")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let (token, token_file) = match env_token {
        // An explicit env token is authoritative; never override it from disk.
        Some(t) => (t, None),
        None => {
            let t = std::fs::read_to_string(&token_path)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!("no MCP token: set MT5_MCP_TOKEN or write {}", token_path.display())
                })?;
            (t, Some(token_path.clone()))
        }
    };
    let url = std::env::var("MT5_MCP_URL").unwrap_or_else(|_| "http://127.0.0.1:22346/mcp".into());

    std::fs::create_dir_all(root.join("runs/dashboard"))?;
    let db = Connection::open(root.join("runs/dashboard/history.db"))?;
    db.execute_batch(SCHEMA)?;

    let (tx, _) = tokio::sync::broadcast::channel(64);
    let app = Arc::new(App {
        mcp: Mutex::new(None),
        db: Mutex::new(db),
        snap: RwLock::new(Snapshot::default()),
        body: RwLock::new("{}".into()),
        tx,
        url,
        token: RwLock::new(token),
        token_file,
        root,
    });

    let warm = app.clone();
    let _ = tokio::task::spawn_blocking(move || warm.refresh(true)).await;

    let poller = app.clone();
    tokio::spawn(poll_loop(poller));

    let port: u16 = std::env::var("XAU_DASH_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    println!("dashboard: http://127.0.0.1:{port}");
    axum::serve(listener, router(app)).await?;
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, comment: &str, profit: f64, symbol: &str) -> Value {
        json!({
            "position_id": id,
            "type": "sell",
            "symbol": symbol,
            "open_volume": 0.1,
            "open_price": 3500.0,
            "close_price": 3490.0,
            "profit": profit,
            "open_time": "2026-11-20T10:00:00",
            "close_time": "2026-11-20T11:00:00",
            "close_reason": "Expert",
            "comment": comment,
        })
    }

    #[test]
    fn classify_separates_strategy_from_noise() {
        assert_eq!(classify("D3-H1"), "strategy");
        assert_eq!(classify(" D3-H4 "), "strategy");
        assert_eq!(classify(""), "manual");
        assert_eq!(classify("lot0.1-3"), "test");
        assert_eq!(classify("xau-bridge-0"), "test");
        assert_eq!(classify("D3-smoke"), "test");
    }

    #[test]
    fn summarise_never_reports_manual_pnl_as_strategy() {
        let rows = vec![
            row("1", "D3-H4", 10.10, "XAUUSD"),
            row("2", "", 1836.56, "XAUUSD"),
            row("3", "lot0.1-1", -35.46, "XAUUSD"),
        ];
        let (deals, p) = summarise(&rows);
        assert_eq!(deals.len(), 3);
        assert!((p.strategy_net - 10.10).abs() < 1e-9);
        assert!((p.manual_net - 1836.56).abs() < 1e-9);
        assert!((p.test_net + 35.46).abs() < 1e-9);
        assert!((p.net - 1811.20).abs() < 1e-9);
        assert_eq!(p.strategy_deals, 1);
    }

    #[test]
    fn summarise_ignores_other_symbols_but_keeps_missing_symbol() {
        let mut no_symbol = row("4", "D3-H1", 5.0, "");
        no_symbol.as_object_mut().unwrap().remove("symbol");
        let rows = vec![row("1", "D3-H1", 1.0, "EURUSD"), no_symbol];
        let (deals, p) = summarise(&rows);
        assert_eq!(deals.len(), 1, "a row without a symbol must still be counted");
        assert!((p.strategy_net - 5.0).abs() < 1e-9);
    }

    #[test]
    fn missing_swaps_does_not_corrupt_profit() {
        let rows = vec![row("1", "D3-H1", -96.50, "XAUUSD")];
        let (deals, _) = summarise(&rows);
        assert!((deals[0].profit + 96.50).abs() < 1e-9);
    }

    #[test]
    fn is_gold_tolerates_missing_symbol() {
        assert!(is_gold(&json!({ "symbol": "XAUUSD" })));
        assert!(is_gold(&json!({})));
        assert!(!is_gold(&json!({ "symbol": "EURUSD" })));
    }
}
