# Running it

## The short version

```sh
./scripts/app.sh start
```

That builds if anything changed, starts both processes, waits for the page, prints
what the board can see, and opens <http://127.0.0.1:8787>.

| Command | What it does |
| --- | --- |
| `./scripts/app.sh start` | build, start both, open the page |
| `./scripts/app.sh start --no-build` | same, but reuse the binaries in `target/release/` |
| `./scripts/app.sh stop` | stop both |
| `./scripts/app.sh status` | are they up, and what the board currently sees |
| `./scripts/app.sh logs` | follow both logs (ctrl-c to leave) |

`start` always stops anything already running first, so it is safe to run twice.
Nothing survives a reboot — see "Surviving restarts" below.

## Two processes, not one

| Process | Binary | Job |
| --- | --- | --- |
| Dashboard | `xau-dashboard` | serves the page and the API on port 8787. Shows the signals, account, positions, history. Does not trade on its own. |
| Runner | `xau-live --period H1 --intraday` | the bot. Checks every closed hourly bar, places orders tagged `D3-H1`, trails the stop, closes at the day's last bar. |

Both need to be running. With the dashboard alone you see the numbers but nothing
trades; with the runner alone it trades but you are blind.

The runner is alive all day, but it only *enters* when the next bar opens between
**07:00 and 18:00 UTC**. Outside that window it logs `signal_outside_window` and
waits. That is the rule working, not a fault.

## Prerequisites (both outside this repo)

1. **MetaTrader 5 is running** with its MCP server on `127.0.0.1:22346`.
2. **`runs/live/mcp_token` holds the current token.** The terminal issues a new
   one every time it restarts. A stale token looks like a broken account:
   `HTTP 401` on everything, including reads. Copy the new token into that file
   (or `export MT5_MCP_TOKEN=...`), then `./scripts/app.sh start` again.

## From inside RustRover

Two ways, both fine:

**Terminal.** Open it with ⌥F12 (or View → Tool Windows → Terminal) and run
`./scripts/app.sh start`. This is the most predictable route, because the script
handles the build, the token check and the restart in one go.

**Run configurations.** The repo ships two in `.run/`, so they appear in the
configuration dropdown at the top right:

1. `1 · Dashboard` — green arrow, wait for `dashboard: http://127.0.0.1:8787`
2. `2 · Live runner (intraday H1)` — green arrow

Run them in that order; each opens its own tab and keeps running while the tab is
alive. Pressing the red stop button kills that process only.

Do not start a second runner. Two runners on the same account would both try to
manage the same position. `./scripts/app.sh status` tells you what is already up.

## Where things are

| Path | What |
| --- | --- |
| `runs/live/dashboard.log` | dashboard output |
| `runs/live/rust_h1_runner.log` | every signal, order and exit, one JSON line each |
| `runs/live/rust_h1_state.json` | runner state: open position, last bar, counters |
| `runs/dashboard/history.db` | SQLite: signals seen, orders sent, equity points |
| `runs/live/mcp_token` | the broker token (gitignored, `chmod 600`) |

## When it looks broken

| Symptom | Cause |
| --- | --- |
| `401` on everything, page shows no account | the token rotated. Refresh `runs/live/mcp_token`, restart. |
| Page loads, "terminal error" | MT5 or its MCP server is not running on `:22346`. |
| `no MCP token` at startup | `runs/live/mcp_token` is missing or empty. |
| `address already in use` | something already holds port 8787. `./scripts/app.sh stop`, or set `XAU_DASH_PORT`. |
| Signals never appear | the terminal is up but the feed is stale — check the "last closed" time on the Signals card. |
| Orders refused with `market is closed` | outside Mon–Fri trading hours. |

## Surviving restarts

Neither process is a service: they die with the terminal, a logout or a reboot.
If you want them to come back on their own, install a macOS LaunchAgent that runs
`scripts/app.sh start --no-build` at login. That has not been set up yet.
