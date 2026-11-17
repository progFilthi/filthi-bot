//! Phase 0 feasibility spike: minimal Rust TCP server for the MT5 Bridge EA.
//!
//! Proves on this Mac (MT5 under Wine) that the EA can:
//! 1. open a TCP socket to this server on 127.0.0.1:5555,
//! 2. stream ticks (NDJSON),
//! 3. receive a test market order (with mandatory SL),
//! 4. report the fill back,
//! 5. receive modify + close and ack the close.
//!
//! Exits 0 with ROUND_TRIP_OK on success. Every wire message is appended to
//! the transcript JSONL for the phase report.

mod config;
mod protocol;
mod session;
mod transcript;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::{error, info, warn};

use config::Config;
use session::{Outcome, Shared};
use transcript::Transcript;

#[derive(Parser, Debug)]
#[command(name = "xau-bridge-server", version, about)]
struct Args {
    /// TOML config path.
    #[arg(long, default_value = "configs/phase0.toml")]
    config: String,
    /// Override server bind port.
    #[arg(long)]
    port: Option<u16>,
}

/// Human-readable summary of an outcome.
fn summarize(o: &Outcome) -> String {
    let mut lines = Vec::new();
    if o.ok {
        lines.push("ROUND_TRIP_OK".to_string());
    } else {
        lines.push(format!(
            "ROUND_TRIP_FAIL: {}",
            o.error.as_deref().unwrap_or("unknown")
        ));
    }
    if let Some(h) = &o.hello {
        lines.push(format!(
            "broker: account={} server={} symbol={} digits={} point={} contract={} min_lot={} lot_step={} stops_level={:>3}pts demo={} utc_offset={:>+5}s",
            h.account, h.server, h.symbol, h.digits, h.point, h.contract_size, h.min_lot,
            h.lot_step, h.stops_level_pts, h.demo, h.server_utc_offset_s
        ));
    }
    for (stage, ms) in &o.stages {
        lines.push(format!("  stage {:<22} @ {:>7} ms", stage, ms));
    }
    lines.push(format!(
        "  ticks={} bars={} fills={} rejects={}",
        o.ticks,
        o.bars,
        o.fills.len(),
        o.rejects.len()
    ));
    for f in &o.fills {
        lines.push(format!(
            "  fill order_id={} ticket={} price={:.2} lots={:.2} slippage={}pts",
            f.order_id, f.ticket, f.price, f.lots, f.slippage_pts
        ));
    }
    for (id, rc, why) in &o.rejects {
        lines.push(format!("  reject order_id={} retcode={} ({})", id, rc, why));
    }
    if let Some(a) = &o.account {
        lines.push(format!(
            "  account balance={:.2} equity={:.2} margin={:.2} free={:.2}",
            a.balance, a.equity, a.margin, a.free_margin
        ));
    }
    if let Some(r) = &o.disconnect_reason {
        lines.push(format!("  disconnect: {r}"));
    }
    lines.join("\n")
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .json()
        .with_target(false)
        .with_current_span(false)
        .with_span_list(false)
        .init();
    let mut cfg = Config::load(&args.config)?;
    if let Some(p) = args.port {
        cfg.server.port = p;
    }
    let cfg = Arc::new(cfg);
    let transcript = Arc::new(Transcript::new(&cfg.transcript.path)?);

    let bind = format!("{}:{}", cfg.server.host, cfg.server.port);
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    info!(%bind, "phase0 bridge server listening; waiting for the MT5 EA");

    let shared = Arc::new(Shared::default());
    let stop = Arc::new(AtomicBool::new(false));
    let overall = Duration::from_secs(cfg.timeouts.overall_s);
    let started = std::time::Instant::now();
    let mut last_fatal: Option<String> = None;

    loop {
        if stop.load(Ordering::SeqCst) || started.elapsed() > overall {
            break;
        }
        let accept = tokio::time::timeout(
            overall
                .saturating_sub(started.elapsed())
                .max(Duration::from_millis(1)),
            listener.accept(),
        )
        .await;
        let (stream, _peer) = match accept {
            Ok(Ok(x)) => x,
            Ok(Err(e)) => {
                error!(%e, "accept failed");
                continue;
            }
            Err(_elapsed) => break,
        };
        stream.set_nodelay(true).ok();
        let cfg2 = Arc::clone(&cfg);
        let tr2 = Arc::clone(&transcript);
        let sh2 = Arc::clone(&shared);
        let outcome = session::run(stream, cfg2, tr2, sh2).await;
        transcript.summary(&serde_json::json!({
            "event": "session_end",
            "ok": outcome.ok,
            "fatal": outcome.fatal,
            "error": outcome.error,
            "stages": outcome.stages.iter().map(|(s, ms)| serde_json::json!({"stage": s, "ms": ms})).collect::<Vec<_>>(),
            "ticks": outcome.ticks,
            "fills": outcome.fills.iter().map(|f| serde_json::json!({
                "order_id": f.order_id, "ticket": f.ticket, "price": f.price,
                "lots": f.lots, "slippage_pts": f.slippage_pts
            })).collect::<Vec<_>>(),
            "rejects": outcome.rejects.iter().map(|(id, rc, r)| serde_json::json!({
                "order_id": id, "retcode": rc, "reason": r
            })).collect::<Vec<_>>(),
        }));
        println!("{}", summarize(&outcome));
        if outcome.fatal {
            last_fatal = outcome.error.clone();
            // A fatal test failure is definitive for this spike: stop retrying.
            break;
        }
        if outcome.ok {
            println!("\nRESULT: socket round trip under Wine verified. Phase 0 gate satisfied.");
            return Ok(());
        }
        if shared.test_done.load(Ordering::SeqCst) {
            break;
        }
        warn!("awaiting EA reconnect...");
    }

    let _ = last_fatal;
    eprintln!("\nRESULT: Phase 0 round trip NOT confirmed within the overall timeout.");
    std::process::exit(1);
}
