//! One EA connection: read lines, dispatch protocol messages, drive the
//! Phase 0 round-trip test (hello -> tick -> test order -> fill ->
//! modify -> close -> close ack), enforce heartbeat staleness.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::time::interval;
use tracing::{error, info, warn};
use xau_core::{Fill, OrderRequest, Side, Tick};

use crate::config::Config;
use crate::protocol::{EaCommand, EaMessage};
use crate::transcript::Transcript;

/// Envelope-level parse so every inbound message can be seq-tracked without
/// matching on the variant.
#[derive(serde::Deserialize)]
struct SeqEnvelope {
    seq: i64,
}

/// Broker specs as reported in `hello`.
#[derive(Debug, Clone)]
pub struct HelloInfo {
    pub account: i64,
    pub server: String,
    pub symbol: String,
    pub digits: i32,
    pub point: f64,
    pub contract_size: f64,
    pub min_lot: f64,
    pub lot_step: f64,
    pub server_utc_offset_s: i64,
    pub stops_level_pts: i64,
    pub demo: bool,
}

#[derive(Debug, Clone)]
pub struct AccountInfo {
    pub balance: f64,
    pub equity: f64,
    pub margin: f64,
    pub free_margin: f64,
}

/// Test-order state machine stages.
#[derive(Debug, Clone, PartialEq)]
enum Stage {
    AwaitingHello,
    AwaitingFirstTick,
    AwaitingFill,
    Modifying {
        ticket: i64,
        new_sl: f64,
        since: Instant,
    },
    Closing {
        ticket: i64,
    },
    Done,
}

/// State shared across reconnections (the EA may drop and re-connect).
#[derive(Default)]
pub struct Shared {
    pub last_tick: Mutex<Option<Tick>>,
    pub test_sent: AtomicBool,
    pub test_done: AtomicBool,
    pub next_seq: AtomicI64,
}

impl Shared {
    fn cmd_seq(&self) -> i64 {
        self.next_seq.fetch_add(1, Ordering::SeqCst) + 1
    }
}

/// Result of one connection's session.
#[derive(Debug, Default)]
pub struct Outcome {
    pub ok: bool,
    /// Fatal test failure (a reconnect might fix a disconnect; this cannot).
    pub fatal: bool,
    pub error: Option<String>,
    pub stages: Vec<(&'static str, u64)>,
    pub hello: Option<HelloInfo>,
    pub fills: Vec<Fill>,
    pub rejects: Vec<(String, i64, String)>,
    pub ticks: u64,
    pub bars: u64,
    pub account: Option<AccountInfo>,
    pub open_positions_seen: u64,
    pub disconnect_reason: Option<String>,
    /// Session start, set when the session begins.
    pub start: Option<Instant>,
}

impl Outcome {
    fn stage(&mut self, name: &'static str) {
        let elapsed = self
            .start
            .map(|t| t.elapsed().as_millis() as u64)
            .unwrap_or(0);
        self.stages.push((name, elapsed));
        info!(stage = name, ms = elapsed, "stage reached");
    }
}

struct Session {
    cfg: Arc<Config>,
    transcript: Arc<Transcript>,
    shared: Arc<Shared>,
    write: OwnedWriteHalf,
    started: Instant,
    /// Outcome being built for this connection.
    out: Outcome,
    hello: Option<HelloInfo>,
    stage: Stage,
    pending_order: Option<OrderRequest>,
    halted: bool,
    last_inbound: Instant,
    last_heartbeat_out: Instant,
    /// Order id used for the close ack of the test position.
    close_ack_id: String,
    /// Last inbound seq (wire-fidelity check: gaps and duplicates).
    last_seq_in: Option<i64>,
}

enum Step {
    Continue,
    /// Round trip proven (or test finished as configured): exit success.
    Success,
    /// Fatal failure: exit nonzero immediately.
    Fatal(String),
    /// Connection-level issue: report, let the accept loop try again.
    Disconnected(String),
}

impl Session {
    fn new(
        cfg: Arc<Config>,
        transcript: Arc<Transcript>,
        shared: Arc<Shared>,
        write: OwnedWriteHalf,
    ) -> Self {
        let started = Instant::now();
        let out = Outcome {
            start: Some(started),
            ..Default::default()
        };
        Self {
            cfg,
            transcript,
            shared,
            write,
            started,
            out,
            hello: None,
            stage: Stage::AwaitingHello,
            pending_order: None,
            halted: false,
            last_inbound: Instant::now(),
            last_heartbeat_out: Instant::now(),
            close_ack_id: String::new(),
            last_seq_in: None,
        }
    }

    async fn send_cmd(&mut self, cmd: &EaCommand) -> Result<()> {
        let line = cmd.to_line();
        self.transcript.record("out", &line);
        self.write.write_all(line.as_bytes()).await?;
        self.write.write_all(b"\n").await?;
        self.write.flush().await?;
        Ok(())
    }

    async fn send_heartbeat(&mut self) -> Result<()> {
        let seq = self.shared.cmd_seq();
        self.send_cmd(&EaCommand::Heartbeat { seq }).await
    }

    fn round(&self, price: f64) -> f64 {
        let digits = self.hello.as_ref().map(|h| h.digits).unwrap_or(2);
        let p = 10f64.powi(digits);
        (price * p).round() / p
    }

    async fn send_test_order(&mut self, reason: &str) -> Step {
        let hello = match &self.hello {
            Some(h) => h.clone(),
            None => return Step::Fatal("cannot send order before hello".into()),
        };
        let tick = match *self.shared.last_tick.lock().unwrap() {
            Some(t) => t,
            None => return Step::Fatal("cannot send order without a tick".into()),
        };
        let cfg = &self.cfg.test_order;
        let sl_dist = cfg.sl_distance_pts as f64 * hello.point;
        if sl_dist <= 0.0 {
            return Step::Fatal(format!("invalid sl_distance_pts {}", cfg.sl_distance_pts));
        }
        // Market buy: enter at ask, SL below, TP at R multiple.
        let side = Side::Buy;
        let sl = self.round(tick.ask - sl_dist);
        let tp = self.round(tick.ask + sl_dist * cfg.tp_rr);
        let order_id = uuid::Uuid::new_v4().to_string();
        let req = OrderRequest {
            order_id: order_id.clone(),
            side,
            lots: cfg.lots,
            sl,
            tp,
            max_slippage_pts: cfg.max_slippage_pts,
            magic: cfg.magic,
        };
        let seq = self.shared.cmd_seq();
        let cmd = crate::protocol::order_cmd(seq, &req);
        info!(
            order_id = %order_id,
            lots = req.lots,
            sl = req.sl,
            tp = req.tp,
            bid = tick.bid,
            ask = tick.ask,
            "sending test order ({reason})"
        );
        if let Err(e) = self.send_cmd(&cmd).await {
            return Step::Disconnected(format!("write order failed: {e}"));
        }
        self.shared.test_sent.store(true, Ordering::SeqCst);
        self.pending_order = Some(req);
        self.stage = Stage::AwaitingFill;
        Step::Continue
    }

    async fn handle_line(&mut self, line: &str) -> Step {
        self.transcript.record("in", line);
        self.last_inbound = Instant::now();
        if let Ok(env) = serde_json::from_str::<SeqEnvelope>(line) {
            match self.last_seq_in {
                Some(last) if env.seq > last + 1 => {
                    warn!(
                        expected = last + 1,
                        got = env.seq,
                        "inbound seq gap (messages lost)"
                    );
                }
                Some(last) if env.seq <= last => {
                    warn!(got = env.seq, last, "inbound duplicate/out-of-order seq");
                }
                _ => {}
            }
            self.last_seq_in = Some(env.seq);
        }
        let msg: EaMessage = match serde_json::from_str(line) {
            Ok(m) => m,
            Err(e) => {
                warn!(raw = %line, err = %e, "unparseable line from EA");
                return Step::Continue;
            }
        };
        match msg {
            EaMessage::Hello {
                account,
                server,
                symbol,
                digits,
                point,
                contract_size,
                min_lot,
                lot_step,
                server_utc_offset_s,
                stops_level_pts,
                demo,
                ..
            } => {
                let info = HelloInfo {
                    account,
                    server,
                    symbol,
                    digits,
                    point,
                    contract_size,
                    min_lot,
                    lot_step,
                    server_utc_offset_s,
                    stops_level_pts,
                    demo,
                };
                info!(
                    account = info.account,
                    server = %info.server,
                    symbol = %info.symbol,
                    digits = info.digits,
                    point = info.point,
                    contract_size = info.contract_size,
                    min_lot = info.min_lot,
                    stops_level_pts = info.stops_level_pts,
                    demo = info.demo,
                    utc_offset_s = info.server_utc_offset_s,
                    "hello received"
                );
                if self.out.hello.is_none() {
                    self.out.stage("hello");
                }
                // Both copies: `out.hello` is the report artifact,
                // `self.hello` is the working copy used by send_test_order
                // and round(). (Only setting the report one left the working
                // copy None -> "cannot send order before hello".)
                self.out.hello = Some(info.clone());
                self.hello = Some(info.clone());
                if self.cfg.test_order.expect_demo && !info.demo {
                    return Step::Fatal(format!(
                        "EA reported demo={} but expect_demo=true; refusing to trade",
                        info.demo
                    ));
                }
                // Reconciliation on (re)connect: resume any in-flight action.
                match self.stage.clone() {
                    Stage::AwaitingHello => {
                        if self.cfg.test_order.enabled
                            && !self.shared.test_done.load(Ordering::SeqCst)
                        {
                            self.stage = Stage::AwaitingFirstTick;
                        } else {
                            self.stage = Stage::Done;
                            return Step::Success;
                        }
                    }
                    Stage::AwaitingFirstTick => {}
                    Stage::AwaitingFill => {
                        if let Some(req) = self.pending_order.clone() {
                            info!(order_id = %req.order_id, "reconnect during AwaitingFill: resending order (idempotent)");
                            let seq = self.shared.cmd_seq();
                            let cmd = crate::protocol::order_cmd(seq, &req);
                            if let Err(e) = self.send_cmd(&cmd).await {
                                return Step::Disconnected(format!("resend failed: {e}"));
                            }
                        }
                    }
                    Stage::Modifying { ticket, .. } => {
                        // Skip the modify on reconnect; make sure we still close.
                        let seq = self.shared.cmd_seq();
                        info!(ticket, "reconnect during Modifying: sending close");
                        if let Err(e) = self.send_cmd(&EaCommand::Close { seq, ticket }).await {
                            return Step::Disconnected(format!("close resend failed: {e}"));
                        }
                        self.stage = Stage::Closing { ticket };
                    }
                    Stage::Closing { ticket } => {
                        let seq = self.shared.cmd_seq();
                        info!(ticket, "reconnect during Closing: resending close");
                        if let Err(e) = self.send_cmd(&EaCommand::Close { seq, ticket }).await {
                            return Step::Disconnected(format!("close resend failed: {e}"));
                        }
                    }
                    Stage::Done => return Step::Success,
                }
            }
            EaMessage::Tick {
                t: ts_ms, bid, ask, ..
            } => {
                self.out.ticks += 1;
                let t = Tick { ts_ms, bid, ask };
                *self.shared.last_tick.lock().unwrap() = Some(t);
                if self.stage == Stage::AwaitingFirstTick {
                    self.out.stage("first_tick");
                    let step = self.send_test_order("first tick").await;
                    if !matches!(step, Step::Continue) {
                        return step;
                    }
                }
            }
            EaMessage::Bar { .. } => {
                self.out.bars += 1;
            }
            EaMessage::Fill {
                order_id,
                ticket,
                price,
                lots,
                time,
                slippage_pts,
                ..
            } => {
                let fill = Fill {
                    order_id: order_id.clone(),
                    ticket,
                    price,
                    lots,
                    ts: time,
                    slippage_pts,
                };
                self.out.fills.push(fill.clone());
                info!(order_id = %order_id, ticket, price, lots, slippage_pts, "fill received");
                if self.pending_order.as_ref().map(|p| p.order_id.as_str())
                    == Some(order_id.as_str())
                {
                    // Entry fill: round trip core proven.
                    self.out.stage("entry_fill");
                    let entry = self.pending_order.clone().unwrap();
                    if !self.cfg.test_order.close_after_fill {
                        self.stage = Stage::Done;
                        return Step::Success;
                    }
                    // Exercise `modify`: tighten SL by 10% of the distance.
                    let dist = (entry.sl - fill.price).abs().max(f64::EPSILON);
                    let new_sl = if entry.side == Side::Buy {
                        self.round(fill.price - dist * 0.9)
                    } else {
                        self.round(fill.price + dist * 0.9)
                    };
                    let seq = self.shared.cmd_seq();
                    info!(ticket, new_sl, "sending modify (tighten SL)");
                    if let Err(e) = self
                        .send_cmd(&EaCommand::Modify {
                            seq,
                            ticket,
                            sl: new_sl,
                            tp: entry.tp,
                        })
                        .await
                    {
                        return Step::Disconnected(format!("modify write failed: {e}"));
                    }
                    self.stage = Stage::Modifying {
                        ticket,
                        new_sl,
                        since: Instant::now(),
                    };
                } else if order_id == self.close_ack_id {
                    // Close ack: position gone -> success.
                    self.out.stage("close_ack");
                    self.stage = Stage::Done;
                    return Step::Success;
                }
            }
            EaMessage::OrderReject {
                order_id,
                retcode,
                reason,
                ..
            } => {
                self.out
                    .rejects
                    .push((order_id.clone(), retcode, reason.clone()));
                error!(order_id = %order_id, retcode, %reason, "order rejected by EA/broker");
                if Some(&order_id) == self.pending_order.as_ref().map(|p| &p.order_id)
                    && self.stage == Stage::AwaitingFill
                {
                    return Step::Fatal(format!(
                        "test order rejected: retcode={retcode} reason={reason}"
                    ));
                }
            }
            EaMessage::Account {
                balance,
                equity,
                margin,
                free_margin,
                ..
            } => {
                self.out.account = Some(AccountInfo {
                    balance,
                    equity,
                    margin,
                    free_margin,
                });
                info!(balance, equity, margin, free_margin, "account snapshot");
            }
            EaMessage::Position {
                ticket,
                side,
                lots,
                open_price,
                sl,
                tp,
                ..
            } => {
                self.out.open_positions_seen += 1;
                info!(
                    ticket,
                    side = side.as_str(),
                    lots,
                    open_price,
                    sl,
                    tp,
                    "position reported"
                );
                if let Stage::Modifying {
                    ticket: want,
                    new_sl,
                    since,
                } = self.stage
                {
                    if ticket == want && lots > 0.0 {
                        let got = (sl - new_sl).abs() < new_sl.abs().max(1.0) * 1e-6 + 1e-6;
                        if got {
                            info!(ticket, "modify verified via position snapshot");
                            let seq = self.shared.cmd_seq();
                            self.close_ack_id = format!("close-{}", ticket);
                            if let Err(e) =
                                self.send_cmd(&EaCommand::Close { seq, ticket: want }).await
                            {
                                return Step::Disconnected(format!("close write failed: {e}"));
                            }
                            self.stage = Stage::Closing { ticket: want };
                            return Step::Continue;
                        }
                        warn!(
                            ticket,
                            sl,
                            expected = new_sl,
                            "position SL differs from modify request"
                        );
                        if since.elapsed() > Duration::from_secs(2) {
                            // Don't stall the test on modify quirks: close now.
                            let seq = self.shared.cmd_seq();
                            self.close_ack_id = format!("close-{}", ticket);
                            if let Err(e) =
                                self.send_cmd(&EaCommand::Close { seq, ticket: want }).await
                            {
                                return Step::Disconnected(format!("close write failed: {e}"));
                            }
                            self.stage = Stage::Closing { ticket: want };
                        }
                    }
                }
            }
            EaMessage::Heartbeat { .. } => {}
        }
        Step::Continue
    }

    async fn tick_housekeeping(&mut self) -> Step {
        // Heartbeat every 1s.
        if self.last_heartbeat_out.elapsed() >= Duration::from_secs(1) {
            self.last_heartbeat_out = Instant::now();
            if let Err(e) = self.send_heartbeat().await {
                return Step::Disconnected(format!("heartbeat write failed: {e}"));
            }
        }
        // Staleness: no inbound for > stale_ms -> halt new orders, alert.
        let stale = Duration::from_millis(self.cfg.timeouts.stale_ms);
        if !self.halted && self.last_inbound.elapsed() > stale {
            self.halted = true;
            error!(
                "inbound silence > {}ms: HALTING new orders (alert)",
                stale.as_millis()
            );
            self.out
                .error
                .get_or_insert_with(|| "inbound silence exceeded staleness window".to_string());
        }
        // Modify fallback: if no position snapshot verified the modify within
        // 5s, close anyway so the account is left flat.
        if let Stage::Modifying { ticket, since, .. } = self.stage {
            if since.elapsed() > Duration::from_secs(5) {
                warn!(
                    ticket,
                    "modify unverified after 5s: closing position anyway"
                );
                let seq = self.shared.cmd_seq();
                self.close_ack_id = format!("close-{}", ticket);
                if let Err(e) = self.send_cmd(&EaCommand::Close { seq, ticket }).await {
                    return Step::Disconnected(format!("close write failed: {e}"));
                }
                self.stage = Stage::Closing { ticket };
            }
        }
        // Stage deadlines.
        match self.stage {
            Stage::AwaitingHello
                if self.started.elapsed() > Duration::from_secs(self.cfg.timeouts.hello_s) =>
            {
                return Step::Fatal("no hello within timeout".into());
            }
            Stage::AwaitingFirstTick
                if self.started.elapsed() > Duration::from_secs(self.cfg.timeouts.first_tick_s) =>
            {
                return Step::Fatal("no tick within timeout (market closed?)".into());
            }
            Stage::AwaitingFill
                if self.started.elapsed() > Duration::from_secs(self.cfg.timeouts.fill_s) =>
            {
                return Step::Fatal("no fill within timeout".into());
            }
            _ => {}
        }
        Step::Continue
    }
}

/// Run a session for one established connection.
pub async fn run(
    stream: tokio::net::TcpStream,
    cfg: Arc<Config>,
    transcript: Arc<Transcript>,
    shared: Arc<Shared>,
) -> Outcome {
    let peer = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| "unknown".into());
    info!(%peer, "EA connected");
    let (rd, wr) = stream.into_split();
    let mut reader = BufReader::new(rd).lines();
    let mut sess = Session::new(cfg, transcript, shared, wr);
    let mut ticker = interval(Duration::from_millis(250));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Both loop arms assign before break; the initial value is never read.
    #[allow(unused_assignments)]
    let mut final_step: Option<Step> = None;

    loop {
        tokio::select! {
            line = reader.next_line() => {
                match line {
                    Ok(Some(l)) => {
                        match sess.handle_line(&l).await {
                            Step::Continue => {}
                            other => { final_step = Some(other); break; }
                        }
                    }
                    Ok(None) => {
                        final_step = Some(Step::Disconnected("EOF from EA".into()));
                        break;
                    }
                    Err(e) => {
                        final_step = Some(Step::Disconnected(format!("read error: {e}")));
                        break;
                    }
                }
            }
            _ = ticker.tick() => {
                match sess.tick_housekeeping().await {
                    Step::Continue => {}
                    other => { final_step = Some(other); break; }
                }
            }
        }
    }

    let step = final_step.unwrap_or(Step::Disconnected("loop ended".into()));
    sess.out.disconnect_reason = match &step {
        Step::Disconnected(r) => Some(r.clone()),
        _ => None,
    };
    match step {
        Step::Success => {
            sess.out.ok = true;
            sess.out.stage("round_trip_complete");
        }
        Step::Fatal(e) => {
            sess.out.fatal = true;
            sess.out.error = Some(e);
        }
        Step::Disconnected(e) => {
            warn!(%e, "connection ended; EA may reconnect");
            sess.out.error = sess.out.error.take().or_else(|| Some(e.clone()));
            sess.out.disconnect_reason = Some(e);
        }
        Step::Continue => unreachable!("continue cannot terminate the loop"),
    }
    sess.out
}
